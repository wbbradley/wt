use std::env;
use std::path::{Path, PathBuf};

use clap::{Args, Parser, Subcommand};
use thiserror::Error;

use crate::config;
use crate::git::{self, GitRunner, SystemGit};
use crate::model::{Catalog, RepositoryConfig};

#[derive(Debug, Parser)]
#[command(name = "wt", version, about = "Global Git worktree manager")]
pub struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Manage the persistent repository catalog.
    Repo {
        #[command(subcommand)]
        command: RepoCommand,
    },
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
    #[error("repository is already registered as {0:?}")]
    DuplicateRepository(String),
    #[error("{0} cannot be empty")]
    EmptyValue(&'static str),
}

pub fn run(cli: Cli) -> Result<(), CliError> {
    let path = config::catalog_path()?;
    run_with(&SystemGit, &path, cli)
}

fn run_with(runner: &dyn GitRunner, catalog_path: &Path, cli: Cli) -> Result<(), CliError> {
    let mut catalog = config::load(catalog_path)?;
    match cli.command {
        Command::Repo { command } => match command {
            RepoCommand::Add(arguments) => add(runner, catalog_path, &mut catalog, arguments),
            RepoCommand::List => list(runner, &catalog),
            RepoCommand::Edit(arguments) => edit(runner, catalog_path, &mut catalog, arguments),
            RepoCommand::Remove(arguments) => {
                remove(catalog_path, &mut catalog, &arguments.selector)
            }
        },
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

    fn repository(path: &str, label: &str) -> RepositoryConfig {
        RepositoryConfig {
            path: PathBuf::from(path),
            label: Some(label.to_owned()),
            worktree_root: None,
            github_remote: None,
        }
    }
}
