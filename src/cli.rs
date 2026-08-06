use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use clap::{ArgGroup, Args, Parser, Subcommand, ValueEnum};
use thiserror::Error;

use crate::config;
use crate::git::{self, GitRunner, SystemGit};
use crate::model::{Catalog, RepositoryConfig};
use crate::operations::{self, CreateMode};
use crate::tui;

#[derive(Debug, Parser)]
#[command(
    name = "wt",
    version,
    about = "Global Git worktree manager",
    args_conflicts_with_subcommands = true
)]
pub struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
    /// Navigate directly with <repository-label>:<branch-or-worktree>.
    selector: Option<String>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Print shell integration code for evaluation.
    ShellInit {
        #[arg(value_enum)]
        shell: SupportedShell,
    },
    /// Inspect or update global configuration.
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    /// Manage the persistent repository catalog.
    Repo {
        #[command(subcommand)]
        command: RepoCommand,
    },
    /// Inspect and safely manage worktrees.
    Worktree {
        #[command(subcommand)]
        command: WorktreeCommand,
    },
    #[command(name = "__complete", hide = true)]
    Complete(CompletionArgs),
}

#[derive(Debug, Subcommand)]
enum ConfigCommand {
    /// Show configured expressions and their resolved runtime values.
    Show,
    /// Update one global setting.
    Set {
        #[command(subcommand)]
        setting: ConfigSetting,
    },
}

#[derive(Debug, Subcommand)]
enum ConfigSetting {
    /// Set the root used to bootstrap repositories for virtual pull requests.
    RepositoryRoot { expression: String },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum SupportedShell {
    Bash,
}

const BASH_SHELL_INIT: &str = include_str!("../shell/wt.bash");

#[derive(Debug, Args)]
struct CompletionArgs {
    #[arg(allow_hyphen_values = true)]
    words: Vec<String>,
}

#[derive(Debug, Subcommand)]
enum WorktreeCommand {
    /// List worktrees in one repository, or in the full catalog.
    List { repository: Option<String> },
    /// Show full local details for a worktree.
    Inspect(WorktreeSelectorArgs),
    /// Create a worktree in one of three explicit modes.
    Create(CreateArgs),
    /// Move a linked worktree.
    Move(MoveArgs),
    /// Lock a worktree, optionally with a reason.
    Lock(LockArgs),
    /// Unlock a worktree.
    Unlock(MutationSelectorArgs),
    /// Repair a worktree's administrative link.
    Repair(RepairArgs),
    /// Safely remove a clean, unlocked linked worktree.
    Remove(MutationSelectorArgs),
    /// Explicitly force-remove a dirty or locked linked worktree.
    ForceRemove(ForceRemoveArgs),
    /// Show exactly what Git would prune.
    PrunePreview { repository: String },
    /// Preview, confirm, and prune stale administrative records.
    Prune(PruneArgs),
}

#[derive(Debug, Args)]
struct WorktreeSelectorArgs {
    repository: String,
    worktree: String,
}

#[derive(Debug, Args)]
struct MutationSelectorArgs {
    repository: String,
    worktree: String,
    #[arg(long)]
    yes: bool,
}

#[derive(Debug, Args)]
#[command(group(
    ArgGroup::new("mode")
        .required(true)
        .multiple(false)
        .args(["branch", "new_branch", "detach"])
))]
struct CreateArgs {
    repository: String,
    /// Destination path; omit it to use the repository's suggested location.
    destination: Option<PathBuf>,
    /// Check out an existing unattached local branch.
    #[arg(long)]
    branch: Option<String>,
    /// Create and check out a new local branch.
    #[arg(long)]
    new_branch: Option<String>,
    /// Start point for --new-branch.
    #[arg(long, default_value = "HEAD")]
    start_point: String,
    /// Create a detached worktree at this commit-ish.
    #[arg(long)]
    detach: Option<String>,
    #[arg(long)]
    create_parents: bool,
    #[arg(long)]
    yes: bool,
}

#[derive(Debug, Args)]
struct MoveArgs {
    repository: String,
    worktree: String,
    destination: PathBuf,
    #[arg(long)]
    create_parents: bool,
    #[arg(long)]
    yes: bool,
}

#[derive(Debug, Args)]
struct LockArgs {
    repository: String,
    worktree: String,
    #[arg(long)]
    reason: Option<String>,
    #[arg(long)]
    yes: bool,
}

#[derive(Debug, Args)]
struct RepairArgs {
    repository: String,
    path: PathBuf,
    #[arg(long)]
    yes: bool,
}

#[derive(Debug, Args)]
struct ForceRemoveArgs {
    repository: String,
    worktree: String,
    /// Must exactly match the branch name or full worktree path.
    #[arg(long)]
    confirm: String,
}

#[derive(Debug, Args)]
struct PruneArgs {
    repository: String,
    #[arg(long)]
    yes: bool,
}

#[derive(Debug, Subcommand)]
enum RepoCommand {
    /// Register a normal, linked-worktree, or bare repository.
    Add(AddArgs),
    /// List catalog entries, including missing repositories.
    List,
    /// Edit catalog metadata or relink a moved repository.
    Edit(EditArgs),
    /// Unregister a repository without deleting anything.
    Remove(SelectorArgs),
}

#[derive(Debug, Args)]
struct AddArgs {
    #[arg(default_value = ".")]
    path: PathBuf,
    #[arg(long)]
    label: Option<String>,
    #[arg(long)]
    worktree_root: Option<PathBuf>,
    #[arg(long)]
    github_remote: Option<String>,
}

#[derive(Debug, Args)]
struct EditArgs {
    selector: String,
    /// New repository location, used to relink a stale entry.
    #[arg(long)]
    path: Option<PathBuf>,
    #[arg(long, conflicts_with = "clear_label")]
    label: Option<String>,
    /// Remove the explicit label and derive it from the anchor directory.
    #[arg(long)]
    clear_label: bool,
    #[arg(long, conflicts_with = "clear_worktree_root")]
    worktree_root: Option<PathBuf>,
    #[arg(long)]
    clear_worktree_root: bool,
    #[arg(long, conflicts_with = "clear_github_remote")]
    github_remote: Option<String>,
    #[arg(long)]
    clear_github_remote: bool,
}

#[derive(Debug, Args)]
struct SelectorArgs {
    selector: String,
}

#[derive(Debug, Error)]
pub enum CliError {
    #[error(transparent)]
    Config(#[from] config::ConfigError),
    #[error(transparent)]
    Git(#[from] git::GitError),
    #[error("cannot resolve current directory: {0}")]
    CurrentDirectory(std::io::Error),
    #[error("repository selector {0:?} did not match any catalog entry")]
    SelectorNotFound(String),
    #[error("repository selector {0:?} is ambiguous")]
    AmbiguousSelector(String),
    #[error("navigation selector {0:?} must use <repository-label>:<branch-or-worktree>")]
    InvalidNavigationSelector(String),
    #[error("worktree selector {selector:?} did not match in repository {repository:?}")]
    NavigationTargetNotFound {
        repository: String,
        selector: String,
    },
    #[error("repository is already registered as {0:?}")]
    DuplicateRepository(String),
    #[error("{0} cannot be empty")]
    EmptyValue(&'static str),
    #[error(transparent)]
    Operation(#[from] operations::OperationError),
    #[error("failed to read confirmation: {0}")]
    ConfirmationIo(io::Error),
    #[error("operation cancelled")]
    Cancelled,
    #[error("failed to write shell initialization: {0}")]
    ShellInitIo(io::Error),
    #[error(transparent)]
    Tui(#[from] tui::TuiError),
}

pub fn run(cli: Cli) -> Result<Option<PathBuf>, CliError> {
    if let Some(selector) = cli.selector.as_deref() {
        let path = config::catalog_path()?;
        let catalog = config::load(&path)?;
        return match resolve_navigation(&SystemGit, &catalog, selector)? {
            NavigationResolution::Exact(path) => Ok(Some(path)),
            NavigationResolution::Ambiguous { filter } => Ok(tui::run_with_filter(&filter)?),
        };
    }
    if cli.command.is_none() {
        return Ok(tui::run()?);
    }
    if let Some(Command::ShellInit { shell }) = cli.command.as_ref() {
        let script = match shell {
            SupportedShell::Bash => BASH_SHELL_INIT,
        };
        io::stdout()
            .lock()
            .write_all(script.as_bytes())
            .map_err(CliError::ShellInitIo)?;
        return Ok(None);
    }
    let path = config::catalog_path()?;
    run_with(&SystemGit, &path, cli)?;
    Ok(None)
}

fn run_with(runner: &dyn GitRunner, catalog_path: &Path, cli: Cli) -> Result<(), CliError> {
    match cli.command.expect("checked by caller") {
        Command::ShellInit { .. } => unreachable!("handled before catalog loading"),
        Command::Config { command } => match command {
            ConfigCommand::Show => show_config(&config::load(catalog_path)?),
            ConfigCommand::Set { setting } => {
                let _lock = config::acquire_catalog_lock(catalog_path)?;
                let mut catalog = config::load(catalog_path)?;
                match setting {
                    ConfigSetting::RepositoryRoot { expression } => {
                        let resolved = config::resolve_repository_root(&expression)?;
                        catalog.repository_root = Some(expression.clone());
                        config::save(catalog_path, &catalog)?;
                        println!(
                            "repository-root\tconfigured={expression}\tresolved={}",
                            resolved.display()
                        );
                    }
                }
                Ok(())
            }
        },
        Command::Repo { command } => match command {
            RepoCommand::List => list(runner, &config::load(catalog_path)?),
            RepoCommand::Add(arguments) => {
                let _lock = config::acquire_catalog_lock(catalog_path)?;
                let mut catalog = config::load(catalog_path)?;
                add(runner, catalog_path, &mut catalog, arguments)
            }
            RepoCommand::Edit(arguments) => {
                let _lock = config::acquire_catalog_lock(catalog_path)?;
                let mut catalog = config::load(catalog_path)?;
                edit(runner, catalog_path, &mut catalog, arguments)
            }
            RepoCommand::Remove(arguments) => {
                let _lock = config::acquire_catalog_lock(catalog_path)?;
                let mut catalog = config::load(catalog_path)?;
                remove(catalog_path, &mut catalog, &arguments.selector)
            }
        },
        Command::Worktree { command } => worktree(runner, &config::load(catalog_path)?, command),
        Command::Complete(arguments) => {
            complete(runner, &config::load(catalog_path)?, &arguments.words)
        }
    }
}

fn show_config(catalog: &Catalog) -> Result<(), CliError> {
    let resolved = config::repository_root(catalog)?;
    println!(
        "repository-root\tconfigured={}\tresolved={}",
        catalog.repository_root_expression(),
        resolved.display()
    );
    for host in catalog.effective_github_hosts(std::iter::empty()) {
        println!("github-host\t{host}");
    }
    Ok(())
}

enum NavigationResolution {
    Exact(PathBuf),
    Ambiguous { filter: String },
}

fn resolve_navigation(
    runner: &dyn GitRunner,
    catalog: &Catalog,
    selector: &str,
) -> Result<NavigationResolution, CliError> {
    let (repository_selector, worktree_selector) = selector
        .split_once(':')
        .filter(|(repository, worktree)| !repository.is_empty() && !worktree.is_empty())
        .ok_or_else(|| CliError::InvalidNavigationSelector(selector.to_owned()))?;
    let repository = repository(catalog, repository_selector)?;
    let worktrees = git::discover_worktrees(runner, &repository.path)?;
    let matches: Vec<_> = worktrees
        .iter()
        .filter(|worktree| worktree.navigable() && worktree_matches(worktree, worktree_selector))
        .collect();
    match matches.as_slice() {
        [] => Err(CliError::NavigationTargetNotFound {
            repository: repository_selector.to_owned(),
            selector: worktree_selector.to_owned(),
        }),
        [worktree] => Ok(NavigationResolution::Exact(
            fs::canonicalize(&worktree.path).unwrap_or_else(|_| worktree.path.clone()),
        )),
        _ => Ok(NavigationResolution::Ambiguous {
            filter: worktree_selector.to_owned(),
        }),
    }
}

fn worktree_matches(worktree: &crate::model::Worktree, selector: &str) -> bool {
    let selector_path = Path::new(selector);
    worktree.path == selector_path
        || (selector_path.exists()
            && fs::canonicalize(selector_path).is_ok_and(|path| {
                fs::canonicalize(&worktree.path).is_ok_and(|other| other == path)
            }))
        || worktree
            .path
            .file_name()
            .is_some_and(|name| name == selector)
        || worktree.branch.as_deref() == Some(selector)
        || worktree
            .branch
            .as_deref()
            .and_then(|branch| branch.strip_prefix("refs/heads/"))
            == Some(selector)
}

fn complete(runner: &dyn GitRunner, catalog: &Catalog, words: &[String]) -> Result<(), CliError> {
    for candidate in completion_candidates(runner, catalog, words) {
        println!("{candidate}");
    }
    Ok(())
}

fn completion_candidates(
    runner: &dyn GitRunner,
    catalog: &Catalog,
    words: &[String],
) -> Vec<String> {
    let current = words.last().map(String::as_str).unwrap_or("");
    let mut candidates = BTreeSet::new();
    candidates.extend(["--help".to_owned(), "-h".to_owned()]);
    let first = words.first().map(String::as_str).unwrap_or("");
    match first {
        "config" => complete_config(words, &mut candidates),
        "repo" => complete_repo(catalog, words, &mut candidates),
        "shell-init" => {
            candidates.insert("bash".to_owned());
        }
        "worktree" => complete_worktree(runner, catalog, words, &mut candidates),
        _ => {
            candidates.extend(["--version".to_owned(), "-V".to_owned()]);
            candidates.extend(
                ["config", "repo", "shell-init", "worktree"]
                    .into_iter()
                    .map(str::to_owned),
            );
            for repository in &catalog.repositories {
                if let Ok(worktrees) = git::discover_worktrees(runner, &repository.path) {
                    for worktree in worktrees.iter().filter(|worktree| worktree.navigable()) {
                        for identity in worktree_identities(worktree) {
                            candidates.insert(format!("{}:{identity}", repository.display_label()));
                        }
                    }
                }
            }
        }
    }
    candidates
        .into_iter()
        .filter(|candidate| candidate.starts_with(current))
        .collect()
}

fn complete_config(words: &[String], candidates: &mut BTreeSet<String>) {
    if words.len() <= 2 {
        candidates.extend(["set", "show"].map(str::to_owned));
    }
    if words.get(1).map(String::as_str) == Some("set") && words.len() <= 3 {
        candidates.insert("repository-root".to_owned());
    }
    if words.get(2).map(String::as_str) == Some("repository-root") {
        candidates.extend(file_candidates(
            words.last().map(String::as_str).unwrap_or(""),
        ));
    }
}

fn complete_repo(catalog: &Catalog, words: &[String], candidates: &mut BTreeSet<String>) {
    if words.len() <= 2 {
        candidates.extend(["add", "edit", "list", "remove"].map(str::to_owned));
    }
    let subcommand = words.get(1).map(String::as_str);
    if matches!(subcommand, Some("edit" | "remove")) && words.len() == 3 {
        candidates.extend(repository_identities(catalog));
    }
    if subcommand == Some("add") {
        candidates.extend(file_candidates(
            words.last().map(String::as_str).unwrap_or(""),
        ));
        candidates.extend(["--label", "--worktree-root", "--github-remote"].map(str::to_owned));
    }
    if subcommand == Some("edit") {
        candidates.extend(
            [
                "--path",
                "--label",
                "--clear-label",
                "--worktree-root",
                "--clear-worktree-root",
                "--github-remote",
                "--clear-github-remote",
            ]
            .map(str::to_owned),
        );
    }
}

fn complete_worktree(
    runner: &dyn GitRunner,
    catalog: &Catalog,
    words: &[String],
    candidates: &mut BTreeSet<String>,
) {
    if words.len() <= 2 {
        candidates.extend(
            [
                "create",
                "force-remove",
                "inspect",
                "list",
                "lock",
                "move",
                "prune",
                "prune-preview",
                "remove",
                "repair",
                "unlock",
            ]
            .map(str::to_owned),
        );
        return;
    }
    let subcommand = words[1].as_str();
    if words.len() == 3 {
        candidates.extend(repository_identities(catalog));
    } else if let Some(repository_selector) = words.get(2)
        && let Ok(repository) = repository(catalog, repository_selector)
        && matches!(
            subcommand,
            "inspect" | "move" | "lock" | "unlock" | "remove" | "force-remove"
        )
        && words.len() == 4
        && let Ok(worktrees) = git::discover_worktrees(runner, &repository.path)
    {
        for worktree in &worktrees {
            candidates.extend(worktree_identities(worktree));
        }
    }
    if matches!(subcommand, "create" | "move" | "repair") {
        candidates.extend(file_candidates(
            words.last().map(String::as_str).unwrap_or(""),
        ));
    }
    let flags: &[&str] = match subcommand {
        "create" => &[
            "--branch",
            "--new-branch",
            "--start-point",
            "--detach",
            "--create-parents",
            "--yes",
        ],
        "move" => &["--create-parents", "--yes"],
        "lock" => &["--reason", "--yes"],
        "unlock" | "remove" | "repair" | "prune" => &["--yes"],
        "force-remove" => &["--confirm"],
        _ => &[],
    };
    candidates.extend(flags.iter().map(|flag| (*flag).to_owned()));
}

fn repository_identities(catalog: &Catalog) -> impl Iterator<Item = String> + '_ {
    catalog.repositories.iter().flat_map(|repository| {
        [
            repository.display_label(),
            repository.path.to_string_lossy().into_owned(),
        ]
    })
}

fn worktree_identities(worktree: &crate::model::Worktree) -> BTreeSet<String> {
    let mut identities = BTreeSet::from([worktree.path.to_string_lossy().into_owned()]);
    if let Some(name) = worktree.path.file_name() {
        identities.insert(name.to_string_lossy().into_owned());
    }
    if let Some(branch) = worktree.branch.as_deref() {
        identities.insert(branch.to_owned());
        if let Some(short) = branch.strip_prefix("refs/heads/") {
            identities.insert(short.to_owned());
        }
    }
    identities
}

fn file_candidates(prefix: &str) -> Vec<String> {
    let path = Path::new(prefix);
    let (directory, name_prefix) = if prefix.ends_with(std::path::MAIN_SEPARATOR) {
        (path, "")
    } else {
        (
            path.parent().unwrap_or_else(|| Path::new(".")),
            path.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or(""),
        )
    };
    let Ok(entries) = fs::read_dir(directory) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().starts_with(name_prefix))
        .map(|entry| {
            let candidate = if directory == Path::new(".") {
                entry.path().file_name().unwrap().into()
            } else {
                directory.join(entry.file_name())
            };
            let mut candidate = candidate.to_string_lossy().into_owned();
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                candidate.push(std::path::MAIN_SEPARATOR);
            }
            candidate
        })
        .collect()
}

fn worktree(
    runner: &dyn GitRunner,
    catalog: &Catalog,
    command: WorktreeCommand,
) -> Result<(), CliError> {
    match command {
        WorktreeCommand::List { repository } => {
            worktree_list(runner, catalog, repository.as_deref())
        }
        WorktreeCommand::Inspect(arguments) => {
            let repository = repository(catalog, &arguments.repository)?;
            print_details(&operations::inspect(
                runner,
                repository,
                &arguments.worktree,
            )?);
            Ok(())
        }
        WorktreeCommand::Create(arguments) => create_worktree(runner, catalog, arguments),
        WorktreeCommand::Move(arguments) => move_worktree(runner, catalog, arguments),
        WorktreeCommand::Lock(arguments) => lock_worktree(runner, catalog, arguments),
        WorktreeCommand::Unlock(arguments) => {
            let repository = repository(catalog, &arguments.repository)?;
            let details = operations::inspect(runner, repository, &arguments.worktree)?;
            print_mutation("unlock", repository, &details.worktree);
            confirm(arguments.yes, "Unlock this worktree?")?;
            operations::unlock(runner, repository, &arguments.worktree)?;
            println!("unlocked\t{}", details.worktree.path.display());
            Ok(())
        }
        WorktreeCommand::Repair(arguments) => {
            let repository = repository(catalog, &arguments.repository)?;
            let path = absolute_path(arguments.path)?;
            eprintln!(
                "repair\trepository={}\tpath={}",
                repository.display_label(),
                path.display()
            );
            confirm(arguments.yes, "Repair this administrative link?")?;
            operations::repair(runner, repository, &path)?;
            println!("repaired\t{}", path.display());
            Ok(())
        }
        WorktreeCommand::Remove(arguments) => remove_worktree(runner, catalog, arguments),
        WorktreeCommand::ForceRemove(arguments) => {
            force_remove_worktree(runner, catalog, arguments)
        }
        WorktreeCommand::PrunePreview {
            repository: selector,
        } => {
            let repository = repository(catalog, &selector)?;
            print!("{}", operations::preview_prune(runner, repository)?);
            Ok(())
        }
        WorktreeCommand::Prune(arguments) => prune_worktrees(runner, catalog, arguments),
    }
}

fn worktree_list(
    runner: &dyn GitRunner,
    catalog: &Catalog,
    selector: Option<&str>,
) -> Result<(), CliError> {
    let repositories: Vec<&RepositoryConfig> = match selector {
        Some(selector) => vec![repository(catalog, selector)?],
        None => catalog.repositories.iter().collect(),
    };
    for repository in repositories {
        match operations::list(runner, repository) {
            Ok(worktrees) => {
                for worktree in worktrees {
                    let identity = worktree_identity(&worktree);
                    println!(
                        "{}\t{}\t{}\t{}{}",
                        repository.display_label(),
                        worktree.path.display(),
                        identity,
                        if worktree.bare { "bare" } else { "worktree" },
                        worktree
                            .locked
                            .as_ref()
                            .map(|reason| format!(" locked={reason}"))
                            .unwrap_or_default()
                    );
                }
            }
            Err(error) => println!(
                "{}\t{}\terror: {}",
                repository.display_label(),
                repository.path.display(),
                error
            ),
        }
    }
    Ok(())
}

fn create_worktree(
    runner: &dyn GitRunner,
    catalog: &Catalog,
    arguments: CreateArgs,
) -> Result<(), CliError> {
    let repository = repository(catalog, &arguments.repository)?;
    let mode = if let Some(branch) = arguments.branch {
        CreateMode::ExistingBranch(branch)
    } else if let Some(branch) = arguments.new_branch {
        CreateMode::NewBranch {
            branch,
            start_point: arguments.start_point,
        }
    } else {
        CreateMode::Detached(arguments.detach.expect("clap requires a creation mode"))
    };
    let destination = arguments
        .destination
        .map(absolute_path)
        .transpose()?
        .unwrap_or_else(|| operations::suggested_destination(repository, &mode));
    eprintln!(
        "create\trepository={}\tanchor={}\tdestination={}\tmode={mode:?}\tcreate_parents={}",
        repository.display_label(),
        repository.path.display(),
        destination.display(),
        arguments.create_parents
    );
    operations::validate_create(
        runner,
        repository,
        &destination,
        &mode,
        arguments.create_parents,
    )?;
    confirm(arguments.yes, "Create this worktree?")?;
    operations::create(
        runner,
        repository,
        &destination,
        &mode,
        arguments.create_parents,
    )?;
    println!("created\t{}", destination.display());
    Ok(())
}

fn move_worktree(
    runner: &dyn GitRunner,
    catalog: &Catalog,
    arguments: MoveArgs,
) -> Result<(), CliError> {
    let repository = repository(catalog, &arguments.repository)?;
    let details = operations::inspect(runner, repository, &arguments.worktree)?;
    let destination = absolute_path(arguments.destination)?;
    eprintln!(
        "move\trepository={}\tfrom={}\tto={}",
        repository.display_label(),
        details.worktree.path.display(),
        destination.display()
    );
    operations::validate_move(
        runner,
        repository,
        &arguments.worktree,
        &destination,
        arguments.create_parents,
    )?;
    confirm(arguments.yes, "Move this worktree?")?;
    operations::move_worktree(
        runner,
        repository,
        &arguments.worktree,
        &destination,
        arguments.create_parents,
    )?;
    println!("moved\t{}", destination.display());
    Ok(())
}

fn lock_worktree(
    runner: &dyn GitRunner,
    catalog: &Catalog,
    arguments: LockArgs,
) -> Result<(), CliError> {
    let repository = repository(catalog, &arguments.repository)?;
    let details = operations::inspect(runner, repository, &arguments.worktree)?;
    print_mutation("lock", repository, &details.worktree);
    if let Some(reason) = &arguments.reason {
        eprintln!("reason={reason}");
    }
    confirm(arguments.yes, "Lock this worktree?")?;
    operations::lock(
        runner,
        repository,
        &arguments.worktree,
        arguments.reason.as_deref(),
    )?;
    println!("locked\t{}", details.worktree.path.display());
    Ok(())
}

fn remove_worktree(
    runner: &dyn GitRunner,
    catalog: &Catalog,
    arguments: MutationSelectorArgs,
) -> Result<(), CliError> {
    let repository = repository(catalog, &arguments.repository)?;
    let current = env::current_dir().map_err(CliError::CurrentDirectory)?;
    let details =
        operations::removal_preview(runner, repository, &arguments.worktree, &current, false)?;
    print_removal(repository, &details);
    confirm(arguments.yes, "Remove this worktree?")?;
    operations::remove(runner, repository, &arguments.worktree, &current)?;
    println!("removed\t{}", details.worktree.path.display());
    Ok(())
}

fn force_remove_worktree(
    runner: &dyn GitRunner,
    catalog: &Catalog,
    arguments: ForceRemoveArgs,
) -> Result<(), CliError> {
    let repository = repository(catalog, &arguments.repository)?;
    let current = env::current_dir().map_err(CliError::CurrentDirectory)?;
    let details =
        operations::removal_preview(runner, repository, &arguments.worktree, &current, true)?;
    print_removal(repository, &details);
    if let Some(status) = &details.status {
        eprintln!("force removal local status: {}", status.summary());
    }
    operations::force_remove(
        runner,
        repository,
        &arguments.worktree,
        &current,
        &arguments.confirm,
    )?;
    println!("force-removed\t{}", details.worktree.path.display());
    Ok(())
}

fn prune_worktrees(
    runner: &dyn GitRunner,
    catalog: &Catalog,
    arguments: PruneArgs,
) -> Result<(), CliError> {
    let repository = repository(catalog, &arguments.repository)?;
    let preview = operations::preview_prune(runner, repository)?;
    eprintln!("prune preview for {}:", repository.display_label());
    eprint!("{preview}");
    confirm(
        arguments.yes,
        "Prune exactly these currently stale records?",
    )?;
    let output = operations::prune(runner, repository)?;
    print!("{output}");
    Ok(())
}

fn print_details(details: &operations::WorktreeDetails) {
    println!("repository\t{}", details.repository.display_label());
    println!("anchor\t{}", details.repository.path.display());
    println!("path\t{}", details.worktree.path.display());
    println!("identity\t{}", worktree_identity(&details.worktree));
    println!("head\t{}", details.worktree.head.as_deref().unwrap_or("-"));
    println!("bare\t{}", details.worktree.bare);
    println!("detached\t{}", details.worktree.detached);
    println!(
        "locked\t{}",
        details.worktree.locked.as_deref().unwrap_or("-")
    );
    println!(
        "prunable\t{}",
        details.worktree.prunable.as_deref().unwrap_or("-")
    );
    if let Some(status) = &details.status {
        println!("upstream\t{}", status.upstream.as_deref().unwrap_or("-"));
        println!("status\t{}", status.summary());
    } else if let Some(error) = &details.status_error {
        println!("status-error\t{error}");
    }
}

fn print_mutation(action: &str, repository: &RepositoryConfig, worktree: &crate::model::Worktree) {
    eprintln!(
        "{action}\trepository={}\tbranch={}\tpath={}",
        repository.display_label(),
        worktree_identity(worktree),
        worktree.path.display()
    );
}

fn print_removal(repository: &RepositoryConfig, details: &operations::WorktreeDetails) {
    print_mutation("remove", repository, &details.worktree);
    if let Some(status) = &details.status {
        eprintln!("local status: {}", status.summary());
    }
}

fn worktree_identity(worktree: &crate::model::Worktree) -> String {
    worktree
        .branch
        .as_deref()
        .and_then(|branch| branch.strip_prefix("refs/heads/"))
        .map(str::to_owned)
        .or_else(|| {
            worktree
                .head
                .as_ref()
                .map(|head| format!("detached:{head}"))
        })
        .unwrap_or_else(|| if worktree.bare { "bare" } else { "unknown" }.to_owned())
}

fn confirm(yes: bool, prompt: &str) -> Result<(), CliError> {
    if yes {
        return Ok(());
    }
    eprint!("{prompt} [y/N] ");
    io::stderr().flush().map_err(CliError::ConfirmationIo)?;
    let mut answer = String::new();
    io::stdin()
        .read_line(&mut answer)
        .map_err(CliError::ConfirmationIo)?;
    if matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
        Ok(())
    } else {
        Err(CliError::Cancelled)
    }
}

fn add(
    runner: &dyn GitRunner,
    catalog_path: &Path,
    catalog: &mut Catalog,
    arguments: AddArgs,
) -> Result<(), CliError> {
    validate_optional_text(arguments.label.as_deref(), "label")?;
    validate_optional_text(arguments.github_remote.as_deref(), "GitHub remote")?;
    let identity = git::resolve_repository(runner, &arguments.path)?;
    if let Some(existing) = find_by_common_dir(runner, catalog, &identity.common_git_dir, None) {
        return Err(CliError::DuplicateRepository(existing.display_label()));
    }
    let repository = RepositoryConfig {
        path: identity.anchor,
        label: arguments.label,
        worktree_root: arguments.worktree_root.map(absolute_path).transpose()?,
        github_remote: arguments.github_remote,
        github_remotes: Default::default(),
        github_preferred_remote: None,
    };
    catalog.repositories.push(repository);
    config::save(catalog_path, catalog)?;
    let repository = catalog.repositories.last().expect("just inserted");
    println!(
        "registered\t{}\t{}",
        repository.display_label(),
        repository.path.display()
    );
    Ok(())
}

fn list(runner: &dyn GitRunner, catalog: &Catalog) -> Result<(), CliError> {
    for discovery in git::discover_catalog(runner, catalog) {
        match discovery.result {
            Ok(worktrees) => {
                let kind = if worktrees
                    .first()
                    .is_some_and(|worktree| !worktree.navigable())
                {
                    "bare"
                } else {
                    "normal"
                };
                println!(
                    "{}\t{}\t{}",
                    discovery.repository.display_label(),
                    discovery.repository.path.display(),
                    kind
                );
            }
            Err(error) => println!(
                "{}\t{}\tstale: {}",
                discovery.repository.display_label(),
                discovery.repository.path.display(),
                error
            ),
        }
    }
    Ok(())
}

fn edit(
    runner: &dyn GitRunner,
    catalog_path: &Path,
    catalog: &mut Catalog,
    arguments: EditArgs,
) -> Result<(), CliError> {
    validate_optional_text(arguments.label.as_deref(), "label")?;
    validate_optional_text(arguments.github_remote.as_deref(), "GitHub remote")?;
    let index = select(catalog, &arguments.selector)?;

    if let Some(path) = arguments.path {
        let identity = git::resolve_repository(runner, &path)?;
        if let Some(existing) =
            find_by_common_dir(runner, catalog, &identity.common_git_dir, Some(index))
        {
            return Err(CliError::DuplicateRepository(existing.display_label()));
        }
        catalog.repositories[index].path = identity.anchor;
    }
    if let Some(label) = arguments.label {
        catalog.repositories[index].label = Some(label);
    } else if arguments.clear_label {
        catalog.repositories[index].label = None;
    }
    if let Some(root) = arguments.worktree_root {
        catalog.repositories[index].worktree_root = Some(absolute_path(root)?);
    } else if arguments.clear_worktree_root {
        catalog.repositories[index].worktree_root = None;
    }
    if let Some(remote) = arguments.github_remote {
        catalog.repositories[index].github_remote = Some(remote);
    } else if arguments.clear_github_remote {
        catalog.repositories[index].github_remote = None;
    }
    config::save(catalog_path, catalog)?;
    println!(
        "updated\t{}\t{}",
        catalog.repositories[index].display_label(),
        catalog.repositories[index].path.display()
    );
    Ok(())
}

fn remove(catalog_path: &Path, catalog: &mut Catalog, selector: &str) -> Result<(), CliError> {
    let index = select(catalog, selector)?;
    let removed = catalog.repositories.remove(index);
    config::save(catalog_path, catalog)?;
    println!(
        "unregistered\t{}\t{}",
        removed.display_label(),
        removed.path.display()
    );
    Ok(())
}

fn find_by_common_dir<'a>(
    runner: &dyn GitRunner,
    catalog: &'a Catalog,
    common_git_dir: &Path,
    excluded_index: Option<usize>,
) -> Option<&'a RepositoryConfig> {
    catalog
        .repositories
        .iter()
        .enumerate()
        .filter(|(index, _)| Some(*index) != excluded_index)
        .find_map(|(_, repository)| {
            let existing = git::canonical_common_dir(runner, repository).ok()?;
            (existing == common_git_dir).then_some(repository)
        })
}

fn select(catalog: &Catalog, selector: &str) -> Result<usize, CliError> {
    let matches: Vec<usize> = catalog
        .repositories
        .iter()
        .enumerate()
        .filter(|(_, repository)| {
            repository.display_label() == selector || repository.path == Path::new(selector)
        })
        .map(|(index, _)| index)
        .collect();
    match matches.as_slice() {
        [] => Err(CliError::SelectorNotFound(selector.to_owned())),
        [index] => Ok(*index),
        _ => Err(CliError::AmbiguousSelector(selector.to_owned())),
    }
}

fn repository<'a>(catalog: &'a Catalog, selector: &str) -> Result<&'a RepositoryConfig, CliError> {
    let index = select(catalog, selector)?;
    Ok(&catalog.repositories[index])
}

fn validate_optional_text(value: Option<&str>, field: &'static str) -> Result<(), CliError> {
    if value.is_some_and(|text| text.trim().is_empty()) {
        return Err(CliError::EmptyValue(field));
    }
    Ok(())
}

fn absolute_path(path: PathBuf) -> Result<PathBuf, CliError> {
    if path.is_absolute() {
        return Ok(path);
    }
    let current = env::current_dir().map_err(CliError::CurrentDirectory)?;
    Ok(current.join(path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::{CommandOutput, GitError};
    use std::ffi::OsString;

    #[test]
    fn selector_rejects_duplicate_labels() {
        let catalog = Catalog {
            repositories: vec![repository("/one", "same"), repository("/two", "same")],
            ..Catalog::default()
        };
        assert!(matches!(
            select(&catalog, "same"),
            Err(CliError::AmbiguousSelector(_))
        ));
        assert_eq!(select(&catalog, "/two").unwrap(), 1);
    }

    #[test]
    fn ambiguous_qualified_selector_returns_a_tui_filter() {
        struct WorktreeList;
        impl GitRunner for WorktreeList {
            fn run(
                &self,
                _directory: &Path,
                _arguments: &[OsString],
            ) -> Result<CommandOutput, GitError> {
                Ok(CommandOutput {
                    stdout: b"worktree /trees/topic\0HEAD a\0branch refs/heads/main\0\0worktree /trees/other\0HEAD b\0branch refs/heads/topic\0\0".to_vec(),
                    stderr: Vec::new(),
                    success: true,
                })
            }
        }
        let catalog = Catalog {
            repositories: vec![repository("/repo", "project")],
            ..Catalog::default()
        };
        assert!(matches!(
            resolve_navigation(&WorktreeList, &catalog, "project:topic").unwrap(),
            NavigationResolution::Ambiguous { filter } if filter == "topic"
        ));
    }

    #[test]
    fn pull_request_like_direct_selector_never_materializes() {
        struct NoWorktrees;
        impl GitRunner for NoWorktrees {
            fn run(
                &self,
                _directory: &Path,
                _arguments: &[OsString],
            ) -> Result<CommandOutput, GitError> {
                Ok(CommandOutput {
                    stdout: Vec::new(),
                    stderr: Vec::new(),
                    success: true,
                })
            }
        }
        let catalog = Catalog {
            repositories: vec![repository("/repo", "owner/repo")],
            ..Catalog::default()
        };
        assert!(matches!(
            resolve_navigation(&NoWorktrees, &catalog, "owner/repo:#123"),
            Err(CliError::NavigationTargetNotFound { .. })
        ));
    }

    fn repository(path: &str, label: &str) -> RepositoryConfig {
        RepositoryConfig {
            path: PathBuf::from(path),
            label: Some(label.to_owned()),
            worktree_root: None,
            github_remote: None,
            github_remotes: Default::default(),
            github_preferred_remote: None,
        }
    }
}
