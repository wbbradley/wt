use std::collections::VecDeque;
use std::env;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyEventKind};
use thiserror::Error;

use crate::app::{Action, App, FormField, Intent, RepositoryView};
use crate::background::{BackgroundJob, JobError, JobMessage, StatusPool, StatusTask};
use crate::bootstrap;
use crate::config;
use crate::git::{self, SystemGit};
use crate::github::{
    AuthoredHost, AuthoredRefreshEvent, GitHubRefresh, GitHubService, RepositoryGitHubInput,
    SystemCredentials,
};
use crate::model::{Catalog, RepositoryConfig, Worktree};
use crate::operations::{self, CreateMode};
use crate::terminal::{InteractiveTerminal, PanicHookGuard};
use crate::ui;

const EVENT_POLL_INTERVAL: Duration = Duration::from_millis(40);
const MIN_GITHUB_REFRESH_INTERVAL: Duration = Duration::from_secs(30);

enum GitHubMessage {
    Branches {
        generation: u64,
        paths: Vec<PathBuf>,
        refresh: GitHubRefresh,
        cache_updates: Vec<RepositoryConfig>,
        warnings: Vec<String>,
    },
    Authored {
        generation: u64,
        event: AuthoredRefreshEvent,
    },
}

#[derive(Debug, Error)]
pub enum TuiError {
    #[error(transparent)]
    Config(#[from] config::ConfigError),
    #[error(transparent)]
    Git(#[from] git::GitError),
    #[error(transparent)]
    Operation(#[from] operations::OperationError),
    #[error(transparent)]
    GitHub(#[from] crate::github::GitHubError),
    #[error(transparent)]
    Bootstrap(#[from] bootstrap::BootstrapError),
    #[error(transparent)]
    Materialize(#[from] crate::materialize::MaterializeError),
    #[error("terminal error: {0}")]
    Terminal(#[from] io::Error),
    #[error("cannot determine the current directory: {0}")]
    CurrentDirectory(io::Error),
    #[error("invalid {field}: {message}")]
    InvalidForm {
        field: &'static str,
        message: String,
    },
    #[error("the selected repository is no longer available")]
    RepositoryGone,
    #[error("the selected worktree is no longer available")]
    WorktreeGone,
}

#[derive(Clone, Debug)]
enum PendingAction {
    Create {
        repository: PathBuf,
        destination: PathBuf,
        mode: CreateMode,
        create_parents: bool,
    },
    Move {
        repository: PathBuf,
        worktree: PathBuf,
        destination: PathBuf,
        create_parents: bool,
    },
    Lock {
        repository: PathBuf,
        worktree: PathBuf,
        reason: Option<String>,
    },
    Unlock {
        repository: PathBuf,
        worktree: PathBuf,
    },
    Remove {
        repository: PathBuf,
        worktree: PathBuf,
    },
    Repair {
        repository: PathBuf,
        path: PathBuf,
    },
    Prune {
        repository: PathBuf,
        preview: String,
    },
    RegisterRepository {
        repository: RepositoryConfig,
    },
    EditRepository {
        repository: PathBuf,
        new_repository: PathBuf,
        label: Option<String>,
        worktree_root: Option<PathBuf>,
        github_remote: Option<String>,
    },
    RemoveRepository {
        repository: PathBuf,
    },
}

impl PendingAction {
    fn action(&self) -> Action {
        match self {
            Self::Create { .. } => Action::Create,
            Self::Move { .. } => Action::Move,
            Self::Lock { .. } => Action::Lock,
            Self::Unlock { .. } => Action::Unlock,
            Self::Remove { .. } => Action::Remove,
            Self::Repair { .. } => Action::Repair,
            Self::Prune { .. } => Action::Prune,
            Self::RegisterRepository { .. } => Action::RegisterRepository,
            Self::EditRepository { .. } => Action::EditRepository,
            Self::RemoveRepository { .. } => Action::RemoveRepository,
        }
    }
}

pub fn run() -> Result<Option<PathBuf>, TuiError> {
    run_with_filter("")
}

pub fn run_with_filter(initial_filter: &str) -> Result<Option<PathBuf>, TuiError> {
    let catalog_path = config::catalog_path()?;
    let catalog = config::load(&catalog_path)?;
    let current_directory = env::current_dir().map_err(TuiError::CurrentDirectory)?;
    let repositories = load_repository_views(&catalog, &current_directory);
    let mut app = App::new(repositories, current_directory);
    app.filter = initial_filter.to_owned();
    app.filter_active = !initial_filter.is_empty();
    let mut controller = Controller::new(catalog_path, catalog, app);
    let _panic_hook = PanicHookGuard::install();
    let mut terminal = InteractiveTerminal::open()?;

    // The first frame contains only catalog and worktree-list data. Slow status
    // work starts only after that frame is visible.
    terminal
        .terminal_mut()
        .draw(|frame| ui::render(frame, &mut controller.app))?;
    controller.start_status_refresh();
    controller.request_github_refresh();
    terminal
        .terminal_mut()
        .draw(|frame| ui::render(frame, &mut controller.app))?;

    loop {
        if controller.pump_background_results() {
            terminal
                .terminal_mut()
                .draw(|frame| ui::render(frame, &mut controller.app))?;
        }
        if let Some(selection) = controller.completed_materialization.take() {
            terminal.restore()?;
            return Ok(Some(selection));
        }
        if !event::poll(EVENT_POLL_INTERVAL)? {
            continue;
        }
        match event::read()? {
            Event::Key(key) if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) => {
                let intent = controller.app.handle_key(key);
                if matches!(intent, Intent::ConfirmAction(_)) {
                    controller.app.progress = Some("performing operation…".to_owned());
                    terminal
                        .terminal_mut()
                        .draw(|frame| ui::render(frame, &mut controller.app))?;
                }
                match controller.handle_intent(intent) {
                    Ok(ControlFlow::Continue) => {
                        terminal
                            .terminal_mut()
                            .draw(|frame| ui::render(frame, &mut controller.app))?;
                    }
                    Ok(ControlFlow::Exit(selection)) => {
                        terminal.restore()?;
                        return Ok(selection);
                    }
                    Err(error) => {
                        controller.app.progress = None;
                        let mut message = error.to_string();
                        if let Err(refresh_error) = controller.refresh_local() {
                            message.push_str(&format!("; refresh also failed: {refresh_error}"));
                        }
                        controller.app.inline_error = Some(message);
                        terminal
                            .terminal_mut()
                            .draw(|frame| ui::render(frame, &mut controller.app))?;
                    }
                }
            }
            Event::Resize(_, _) => {
                terminal
                    .terminal_mut()
                    .draw(|frame| ui::render(frame, &mut controller.app))?;
            }
            _ => {}
        }
    }
}

enum ControlFlow {
    Continue,
    Exit(Option<PathBuf>),
}

struct MaterializationOutcome {
    path: PathBuf,
    refreshed: crate::model::AuthoredPullRequest,
}

struct Controller {
    catalog_path: PathBuf,
    catalog: Catalog,
    app: App,
    status_pool: StatusPool,
    status_backlog: VecDeque<StatusTask>,
    github_service: GitHubService,
    github_sender: Sender<GitHubMessage>,
    github_receiver: Receiver<GitHubMessage>,
    github_in_flight: bool,
    github_refresh_queued: bool,
    github_refresh_interval: Duration,
    next_github_refresh: Instant,
    discover_authored_pull_requests: bool,
    pending_action: Option<PendingAction>,
    materialization_job: Option<BackgroundJob<MaterializationOutcome>>,
    materialization_progress: Option<String>,
    completed_materialization: Option<PathBuf>,
}

impl Controller {
    fn new(catalog_path: PathBuf, catalog: Catalog, app: App) -> Self {
        let workers = std::thread::available_parallelism()
            .map(|parallelism| parallelism.get().min(4))
            .unwrap_or(2);
        let github_refresh_interval = github_refresh_interval(&catalog);
        let (github_sender, github_receiver) = mpsc::channel();
        Self {
            catalog_path,
            catalog,
            app,
            status_pool: StatusPool::with_git(workers),
            status_backlog: VecDeque::new(),
            github_service: GitHubService::new(),
            github_sender,
            github_receiver,
            github_in_flight: false,
            github_refresh_queued: false,
            github_refresh_interval,
            next_github_refresh: Instant::now(),
            discover_authored_pull_requests: true,
            pending_action: None,
            materialization_job: None,
            materialization_progress: None,
            completed_materialization: None,
        }
    }

    fn handle_intent(&mut self, intent: Intent) -> Result<ControlFlow, TuiError> {
        if self.materialization_job.is_some() {
            return match intent {
                Intent::None => Ok(ControlFlow::Continue),
                Intent::Cancel => {
                    self.cancel_materialization();
                    Ok(ControlFlow::Continue)
                }
                _ => {
                    self.app.inline_error = Some(
                        "pull request materialization is in progress; press Ctrl-C to cancel"
                            .to_owned(),
                    );
                    Ok(ControlFlow::Continue)
                }
            };
        }
        match intent {
            Intent::None => Ok(ControlFlow::Continue),
            Intent::Accept(path) => {
                let absolute = std::fs::canonicalize(&path).unwrap_or(path);
                Ok(ControlFlow::Exit(Some(absolute)))
            }
            Intent::Cancel => Ok(ControlFlow::Exit(None)),
            Intent::Refresh => {
                self.refresh_local()?;
                Ok(ControlFlow::Continue)
            }
            Intent::RefreshGitHub => {
                self.request_github_refresh();
                Ok(ControlFlow::Continue)
            }
            Intent::BeginAction(action) => {
                self.begin_action(action)?;
                Ok(ControlFlow::Continue)
            }
            Intent::SubmitForm { action, values } => {
                self.submit_form(action, values)?;
                Ok(ControlFlow::Continue)
            }
            Intent::ConfirmAction(action) => {
                let pending = self.pending_action.take().ok_or(TuiError::InvalidForm {
                    field: "confirmation",
                    message: "no operation is awaiting confirmation".to_owned(),
                })?;
                if pending.action() != action {
                    return Err(TuiError::InvalidForm {
                        field: "confirmation",
                        message: "the selected operation changed".to_owned(),
                    });
                }
                self.execute(pending)?;
                self.app.progress = None;
                self.refresh_local()?;
                Ok(ControlFlow::Continue)
            }
            Intent::MaterializePullRequest(identity) => {
                self.start_pull_request_materialization(identity)?;
                Ok(ControlFlow::Continue)
            }
        }
    }

    fn begin_action(&mut self, action: Action) -> Result<(), TuiError> {
        let (repository, _) = self
            .app
            .selected_repository()
            .ok_or(TuiError::RepositoryGone)?;
        let repository_path = repository.config.path.clone();
        match action {
            Action::Create => self.app.open_form(
                action,
                vec![
                    field("mode (existing/new/detached)", "new"),
                    field("branch or commit-ish", ""),
                    field("start point (new only)", "HEAD"),
                    field("destination (blank = suggested)", ""),
                    field("create missing parents (yes/no)", "no"),
                ],
            ),
            Action::Move => {
                self.require_selected_worktree()?;
                self.app.open_form(
                    action,
                    vec![
                        field("destination", ""),
                        field("create missing parents (yes/no)", "no"),
                    ],
                );
            }
            Action::Lock => {
                self.require_selected_worktree()?;
                self.app
                    .open_form(action, vec![field("reason (optional)", "")]);
            }
            Action::Repair => {
                let (_, worktree, _) = self.require_selected_worktree()?;
                self.app.open_form(
                    action,
                    vec![field("worktree path", &worktree.path.to_string_lossy())],
                );
            }
            Action::EditRepository => self.app.open_form(
                action,
                vec![
                    field(
                        "repository path (relink)",
                        &repository.config.path.to_string_lossy(),
                    ),
                    field(
                        "label (blank = derived)",
                        repository.config.label.as_deref().unwrap_or(""),
                    ),
                    field(
                        "worktree root (blank = sibling)",
                        &repository
                            .config
                            .worktree_root
                            .as_deref()
                            .map(Path::to_string_lossy)
                            .unwrap_or_default(),
                    ),
                    field(
                        "GitHub remote (blank = origin)",
                        repository.config.github_remote.as_deref().unwrap_or(""),
                    ),
                ],
            ),
            Action::Unlock => {
                let (_, worktree, _) = self.require_selected_worktree()?;
                let pending = PendingAction::Unlock {
                    repository: repository_path,
                    worktree: worktree.path.clone(),
                };
                self.confirm(pending, vec![format!("unlock {}", worktree.path.display())]);
            }
            Action::Remove => {
                let (_, worktree, _) = self.require_selected_worktree()?;
                let details = operations::removal_preview(
                    &SystemGit,
                    &repository.config,
                    &worktree.path.to_string_lossy(),
                    &self.app.current_directory,
                    false,
                )?;
                let mut summary = vec![
                    format!("repository: {}", repository.config.display_label()),
                    format!("branch: {}", identity(&details.worktree)),
                    format!("path: {}", details.worktree.path.display()),
                ];
                if let Some(status) = details.status {
                    summary.push(format!("local status: {}", status.summary()));
                }
                let pending = PendingAction::Remove {
                    repository: repository_path,
                    worktree: worktree.path.clone(),
                };
                self.confirm(pending, summary);
            }
            Action::Prune => {
                let preview = operations::preview_prune(&SystemGit, &repository.config)?;
                let summary = if preview.is_empty() {
                    vec!["Git reports no stale worktree records.".to_owned()]
                } else {
                    preview.lines().map(str::to_owned).collect()
                };
                let pending = PendingAction::Prune {
                    repository: repository_path,
                    preview,
                };
                self.confirm(pending, summary);
            }
            Action::RegisterRepository => {
                let pending = PendingAction::RegisterRepository {
                    repository: repository.config.clone(),
                };
                self.confirm(
                    pending,
                    vec![format!("register {}", repository.config.path.display())],
                );
            }
            Action::RemoveRepository => {
                let pending = PendingAction::RemoveRepository {
                    repository: repository_path,
                };
                self.confirm(
                    pending,
                    vec![
                        "Only the catalog entry will be removed.".to_owned(),
                        "No repository or worktree will be deleted.".to_owned(),
                    ],
                );
            }
        }
        Ok(())
    }

    fn submit_form(&mut self, action: Action, values: Vec<String>) -> Result<(), TuiError> {
        let (repository_view, _) = self
            .app
            .selected_repository()
            .ok_or(TuiError::RepositoryGone)?;
        let repository = repository_view.config.clone();
        match action {
            Action::Create => {
                require_len(&values, 5, "create")?;
                let reference = nonempty(&values[1], "branch or commit-ish")?;
                let mode = match values[0].trim().to_ascii_lowercase().as_str() {
                    "existing" => CreateMode::ExistingBranch(reference.to_owned()),
                    "new" => CreateMode::NewBranch {
                        branch: reference.to_owned(),
                        start_point: if values[2].trim().is_empty() {
                            "HEAD".to_owned()
                        } else {
                            values[2].trim().to_owned()
                        },
                    },
                    "detached" => CreateMode::Detached(reference.to_owned()),
                    _ => return invalid("mode", "use existing, new, or detached"),
                };
                let destination = if values[3].trim().is_empty() {
                    operations::suggested_destination(&repository, &mode)
                } else {
                    absolute_path(&self.app.current_directory, values[3].trim())
                };
                let create_parents = parse_yes_no(&values[4], "create missing parents")?;
                operations::validate_create(
                    &SystemGit,
                    &repository,
                    &destination,
                    &mode,
                    create_parents,
                )?;
                let pending = PendingAction::Create {
                    repository: repository.path.clone(),
                    destination: destination.clone(),
                    mode: mode.clone(),
                    create_parents,
                };
                self.confirm(
                    pending,
                    vec![
                        format!("repository: {}", repository.display_label()),
                        format!("destination: {}", destination.display()),
                        format!("mode: {mode:?}"),
                        format!("create parents: {create_parents}"),
                    ],
                );
            }
            Action::Move => {
                require_len(&values, 2, "move")?;
                let (_, worktree, _) = self.require_selected_worktree()?;
                let worktree_path = worktree.path.clone();
                let destination = absolute_path(
                    &self.app.current_directory,
                    nonempty(&values[0], "destination")?,
                );
                let create_parents = parse_yes_no(&values[1], "create missing parents")?;
                operations::validate_move(
                    &SystemGit,
                    &repository,
                    &worktree_path.to_string_lossy(),
                    &destination,
                    create_parents,
                )?;
                let pending = PendingAction::Move {
                    repository: repository.path.clone(),
                    worktree: worktree_path.clone(),
                    destination: destination.clone(),
                    create_parents,
                };
                self.confirm(
                    pending,
                    vec![
                        format!("from: {}", worktree_path.display()),
                        format!("to: {}", destination.display()),
                    ],
                );
            }
            Action::Lock => {
                require_len(&values, 1, "lock")?;
                let (_, worktree, _) = self.require_selected_worktree()?;
                let reason = optional_text(&values[0]);
                let pending = PendingAction::Lock {
                    repository: repository.path,
                    worktree: worktree.path.clone(),
                    reason: reason.clone(),
                };
                self.confirm(
                    pending,
                    vec![
                        format!("path: {}", worktree.path.display()),
                        format!("reason: {}", reason.as_deref().unwrap_or("none")),
                    ],
                );
            }
            Action::Repair => {
                require_len(&values, 1, "repair")?;
                let path = absolute_path(
                    &self.app.current_directory,
                    nonempty(&values[0], "worktree path")?,
                );
                let pending = PendingAction::Repair {
                    repository: repository.path,
                    path: path.clone(),
                };
                self.confirm(pending, vec![format!("repair: {}", path.display())]);
            }
            Action::EditRepository => {
                require_len(&values, 4, "repository metadata")?;
                let path = absolute_path(
                    &self.app.current_directory,
                    nonempty(&values[0], "repository path")?,
                );
                let identity = git::resolve_repository(&SystemGit, &path)?;
                for other in self
                    .catalog
                    .repositories
                    .iter()
                    .filter(|other| other.path != repository.path)
                {
                    if git::resolve_repository(&SystemGit, &other.path)
                        .is_ok_and(|other| other.common_git_dir == identity.common_git_dir)
                    {
                        return invalid("repository path", "that repository is already registered");
                    }
                }
                let worktree_root = optional_text(&values[2])
                    .map(|path| absolute_path(&self.app.current_directory, &path));
                let pending = PendingAction::EditRepository {
                    repository: repository.path.clone(),
                    new_repository: identity.anchor.clone(),
                    label: optional_text(&values[1]),
                    worktree_root,
                    github_remote: optional_text(&values[3]),
                };
                self.confirm(
                    pending,
                    vec![
                        format!("from: {}", repository.path.display()),
                        format!("to: {}", identity.anchor.display()),
                        "update repository metadata".to_owned(),
                    ],
                );
            }
            _ => return invalid("action", "this action does not use a form"),
        }
        Ok(())
    }

    fn confirm(&mut self, pending: PendingAction, summary: Vec<String>) {
        let action = pending.action();
        self.pending_action = Some(pending);
        self.app.open_confirmation(action, summary);
    }

    fn execute(&mut self, pending: PendingAction) -> Result<(), TuiError> {
        match pending {
            PendingAction::Create {
                repository,
                destination,
                mode,
                create_parents,
            } => {
                let repository = self.repository(&repository)?.clone();
                operations::create(&SystemGit, &repository, &destination, &mode, create_parents)?;
            }
            PendingAction::Move {
                repository,
                worktree,
                destination,
                create_parents,
            } => {
                let repository = self.repository(&repository)?.clone();
                operations::move_worktree(
                    &SystemGit,
                    &repository,
                    &worktree.to_string_lossy(),
                    &destination,
                    create_parents,
                )?;
            }
            PendingAction::Lock {
                repository,
                worktree,
                reason,
            } => {
                let repository = self.repository(&repository)?.clone();
                operations::lock(
                    &SystemGit,
                    &repository,
                    &worktree.to_string_lossy(),
                    reason.as_deref(),
                )?;
            }
            PendingAction::Unlock {
                repository,
                worktree,
            } => {
                let repository = self.repository(&repository)?.clone();
                operations::unlock(&SystemGit, &repository, &worktree.to_string_lossy())?;
            }
            PendingAction::Remove {
                repository,
                worktree,
            } => {
                let repository = self.repository(&repository)?.clone();
                operations::remove(
                    &SystemGit,
                    &repository,
                    &worktree.to_string_lossy(),
                    &self.app.current_directory,
                )?;
            }
            PendingAction::Repair { repository, path } => {
                let repository = self.repository(&repository)?.clone();
                operations::repair(&SystemGit, &repository, &path)?;
            }
            PendingAction::Prune {
                repository,
                preview,
            } => {
                let repository = self.repository(&repository)?.clone();
                let current = operations::preview_prune(&SystemGit, &repository)?;
                if current != preview {
                    return invalid(
                        "prune preview",
                        "stale records changed; refresh and review the new preview",
                    );
                }
                operations::prune(&SystemGit, &repository)?;
            }
            PendingAction::RegisterRepository { repository } => {
                let _lock = config::acquire_catalog_lock(&self.catalog_path)?;
                let mut catalog = config::load(&self.catalog_path)?;
                if !catalog
                    .repositories
                    .iter()
                    .any(|existing| existing.path == repository.path)
                {
                    catalog.repositories.push(repository);
                    config::save(&self.catalog_path, &catalog)?;
                }
                self.catalog = catalog;
            }
            PendingAction::EditRepository {
                repository,
                new_repository,
                label,
                worktree_root,
                github_remote,
            } => {
                let _lock = config::acquire_catalog_lock(&self.catalog_path)?;
                let mut catalog = config::load(&self.catalog_path)?;
                let entry = catalog
                    .repositories
                    .iter_mut()
                    .find(|entry| entry.path == repository)
                    .ok_or(TuiError::RepositoryGone)?;
                entry.path = new_repository;
                entry.label = label;
                entry.worktree_root = worktree_root;
                entry.github_remote = github_remote;
                config::save(&self.catalog_path, &catalog)?;
                self.catalog = catalog;
            }
            PendingAction::RemoveRepository { repository } => {
                let _lock = config::acquire_catalog_lock(&self.catalog_path)?;
                let mut catalog = config::load(&self.catalog_path)?;
                let before = catalog.repositories.len();
                catalog
                    .repositories
                    .retain(|entry| entry.path != repository);
                if catalog.repositories.len() == before {
                    return Err(TuiError::RepositoryGone);
                }
                config::save(&self.catalog_path, &catalog)?;
                self.catalog = catalog;
            }
        }
        Ok(())
    }

    fn repository(&self, path: &Path) -> Result<&RepositoryConfig, TuiError> {
        self.app
            .repositories
            .iter()
            .find(|repository| repository.config.path == path)
            .map(|repository| &repository.config)
            .ok_or(TuiError::RepositoryGone)
    }

    fn require_selected_worktree(&self) -> Result<(&RepositoryView, &Worktree, usize), TuiError> {
        self.app.selected_worktree().ok_or(TuiError::WorktreeGone)
    }

    fn refresh_local(&mut self) -> Result<(), TuiError> {
        self.catalog = config::load(&self.catalog_path)?;
        self.github_refresh_interval = github_refresh_interval(&self.catalog);
        let repositories = load_repository_views(&self.catalog, &self.app.current_directory);
        self.app.replace_repositories(repositories);
        self.start_status_refresh();
        self.request_github_refresh();
        Ok(())
    }

    fn start_status_refresh(&mut self) {
        let paths: Vec<PathBuf> = self
            .app
            .repositories
            .iter()
            .flat_map(|repository| repository.worktrees.iter())
            .filter(|worktree| worktree.navigable() && worktree.path.exists())
            .map(|worktree| worktree.path.clone())
            .collect();
        let generation = self.app.begin_status_refresh(&paths);
        self.status_backlog = paths
            .into_iter()
            .map(|path| StatusTask { generation, path })
            .collect();
        self.submit_status_backlog();
    }

    fn submit_status_backlog(&mut self) {
        while let Some(task) = self.status_backlog.pop_front() {
            if let Err(task) = self.status_pool.try_submit(task) {
                self.status_backlog.push_front(task);
                break;
            }
        }
    }

    fn request_github_refresh(&mut self) {
        if self.github_in_flight {
            self.github_refresh_queued = true;
            return;
        }

        let inputs: Vec<RepositoryGitHubInput> = self
            .app
            .repositories
            .iter()
            .filter(|repository| repository.stale_error.is_none())
            .map(|repository| RepositoryGitHubInput {
                repository: repository.config.clone(),
                worktrees: repository.worktrees.clone(),
            })
            .collect();
        let paths: Vec<PathBuf> = inputs
            .iter()
            .flat_map(|input| input.worktrees.iter())
            .filter(|worktree| {
                !worktree.bare
                    && worktree
                        .branch
                        .as_deref()
                        .is_some_and(|branch| branch.starts_with("refs/heads/"))
            })
            .map(|worktree| worktree.path.clone())
            .collect();
        let generation = self.app.begin_github_refresh(&paths);
        let authored_generation = self.app.authored_pull_requests.begin();
        self.next_github_refresh = Instant::now() + self.github_refresh_interval;

        self.github_in_flight = true;
        let service = self.github_service.clone();
        let sender = self.github_sender.clone();
        let catalog_path = self.catalog_path.clone();
        let fallback_catalog = self.catalog.clone();
        let discover_authored_pull_requests = self.discover_authored_pull_requests;
        std::thread::spawn(move || {
            let mut inputs = inputs;
            let mut cache_updates = Vec::new();
            let mut warnings = Vec::new();
            let catalog = match config::acquire_catalog_lock(&catalog_path).and_then(|_lock| {
                let mut catalog = config::load(&catalog_path)?;
                let refresh =
                    crate::github::refresh_catalog_remote_identities(&SystemGit, &mut catalog);
                if refresh.changed {
                    config::save(&catalog_path, &catalog)?;
                }
                Ok((catalog, refresh.warnings))
            }) {
                Ok((catalog, refresh_warnings)) => {
                    warnings.extend(refresh_warnings);
                    for input in &mut inputs {
                        if let Some(repository) = catalog
                            .repositories
                            .iter()
                            .find(|repository| repository.path == input.repository.path)
                        {
                            input.repository = repository.clone();
                        } else if let Ok(refresh) =
                            crate::github::refresh_repository_remote_identities(
                                &SystemGit,
                                &mut input.repository,
                            )
                        {
                            warnings.extend(refresh.warnings);
                        }
                    }
                    cache_updates = catalog.repositories.clone();
                    catalog
                }
                Err(error) => {
                    warnings.push(format!(
                        "unable to persist GitHub remote identities: {error}"
                    ));
                    fallback_catalog
                }
            };
            let refresh = service.fetch_catalog(&inputs);
            let _ = sender.send(GitHubMessage::Branches {
                generation,
                paths,
                refresh,
                cache_updates,
                warnings,
            });
            let fallback_anchor = catalog_path
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .to_owned();
            let hosts: Vec<AuthoredHost> = crate::github::inferred_github_hosts(&catalog)
                .into_iter()
                .map(|host| {
                    let anchor = catalog
                        .repositories
                        .iter()
                        .find(|repository| {
                            repository
                                .github_remotes
                                .values()
                                .any(|identity| identity.host == host)
                        })
                        .map(|repository| repository.path.clone())
                        .unwrap_or_else(|| fallback_anchor.clone());
                    AuthoredHost::inferred(&host, anchor)
                })
                .collect();
            if discover_authored_pull_requests {
                service.fetch_authored_with(&SystemCredentials, &hosts, |event| {
                    let _ = sender.send(GitHubMessage::Authored {
                        generation: authored_generation,
                        event,
                    });
                });
            } else {
                let _ = sender.send(GitHubMessage::Authored {
                    generation: authored_generation,
                    event: AuthoredRefreshEvent::Finished {
                        complete: true,
                        warnings: Vec::new(),
                        error: None,
                    },
                });
            }
        });
    }

    fn pump_background_results(&mut self) -> bool {
        self.submit_status_backlog();
        let mut refresh = false;
        let mut changed = false;
        while let Some(result) = self.status_pool.try_recv() {
            changed = true;
            refresh |= self.app.apply_status(result);
        }
        if refresh && let Err(error) = self.refresh_local() {
            self.app.inline_error = Some(error.to_string());
            changed = true;
        }
        while let Ok(message) = self.github_receiver.try_recv() {
            match message {
                GitHubMessage::Branches {
                    generation,
                    paths,
                    refresh,
                    cache_updates,
                    warnings,
                } => {
                    for update in cache_updates {
                        if let Some(repository) = self
                            .catalog
                            .repositories
                            .iter_mut()
                            .find(|repository| repository.path == update.path)
                        {
                            repository.github_remotes = update.github_remotes.clone();
                            repository.github_preferred_remote =
                                update.github_preferred_remote.clone();
                        }
                        if let Some(repository) = self
                            .app
                            .repositories
                            .iter_mut()
                            .find(|repository| repository.config.path == update.path)
                        {
                            repository.config.github_remotes = update.github_remotes;
                            repository.config.github_preferred_remote =
                                update.github_preferred_remote;
                        }
                    }
                    if !warnings.is_empty() {
                        self.app.inline_error =
                            Some(format!("GitHub remote warning: {}", warnings.join("; ")));
                        changed = true;
                    }
                    self.app.github_hosts = crate::github::inferred_github_hosts(&self.catalog);
                    self.app.active_pull_requests = refresh.active_pull_requests;
                    self.app.authored_mappings = crate::github::map_pull_request_identities(
                        &self.catalog,
                        self.app.authored_pull_requests.identities(),
                        &self.app.active_pull_requests,
                        |repository| git::resolve_repository(&SystemGit, &repository.path).is_ok(),
                    );
                    changed |= self
                        .app
                        .apply_github_refresh(generation, &paths, refresh.branches);
                }
                GitHubMessage::Authored { generation, event } => match event {
                    AuthoredRefreshEvent::Page {
                        host,
                        page,
                        pull_requests,
                        warnings,
                    } => {
                        changed |= self.app.authored_pull_requests.apply_page(
                            generation,
                            host,
                            page,
                            pull_requests,
                            warnings,
                        );
                        self.refresh_authored_mappings();
                    }
                    AuthoredRefreshEvent::Finished {
                        complete,
                        warnings,
                        error,
                    } => {
                        changed |= self
                            .app
                            .authored_pull_requests
                            .finish(generation, complete, warnings, error);
                        self.refresh_authored_mappings();
                        self.github_in_flight = false;
                        self.app.progress = None;
                        if std::mem::take(&mut self.github_refresh_queued) {
                            self.request_github_refresh();
                        }
                    }
                },
            }
        }
        changed |= self.pump_materialization();
        if !self.github_in_flight && Instant::now() >= self.next_github_refresh {
            self.request_github_refresh();
            changed |= self.app.github_loading;
        }
        changed
    }

    fn pump_materialization(&mut self) -> bool {
        let mut messages = Vec::new();
        if let Some(job) = self.materialization_job.as_ref() {
            while let Some(message) = job.try_recv() {
                messages.push(message);
            }
        }
        if messages.is_empty() {
            if self.materialization_job.is_some() {
                self.app.progress = self.materialization_progress.clone();
            }
            return false;
        }
        let mut changed = false;
        for message in messages {
            match message {
                JobMessage::Progress(progress) => {
                    self.materialization_progress = Some(progress);
                    changed = true;
                }
                JobMessage::Finished(result) => {
                    if let Some(mut job) = self.materialization_job.take() {
                        job.join();
                    }
                    self.materialization_progress = None;
                    self.app.progress = None;
                    match result {
                        Ok(outcome) => {
                            self.app.authored_pull_requests.update(outcome.refreshed);
                            self.app.rebuild_virtual_repositories();
                            match self.refresh_local() {
                                Ok(()) => self.completed_materialization = Some(outcome.path),
                                Err(error) => self.app.inline_error = Some(error.to_string()),
                            }
                        }
                        Err(JobError::Cancelled) => {
                            self.app.inline_error = Some(
                                "pull request materialization cancelled; completed safe stages were retained"
                                    .to_owned(),
                            );
                            if let Err(error) = self.refresh_local() {
                                self.app.inline_error = Some(format!(
                                    "materialization cancelled; refresh failed: {error}"
                                ));
                            }
                        }
                        Err(JobError::Failed(error)) => {
                            self.app.inline_error = Some(error);
                            if let Err(refresh_error) = self.refresh_local() {
                                let message = self.app.inline_error.take().unwrap_or_default();
                                self.app.inline_error = Some(format!(
                                    "{message}; refresh also failed: {refresh_error}"
                                ));
                            }
                        }
                    }
                    changed = true;
                }
            }
        }
        if self.materialization_job.is_some() {
            self.app.progress = self.materialization_progress.clone();
        }
        changed
    }

    fn refresh_authored_mappings(&mut self) {
        self.app.authored_mappings = crate::github::map_pull_request_identities(
            &self.catalog,
            self.app.authored_pull_requests.identities(),
            &self.app.active_pull_requests,
            |repository| git::resolve_repository(&SystemGit, &repository.path).is_ok(),
        );
        self.app.rebuild_virtual_repositories();
    }

    fn start_pull_request_materialization(
        &mut self,
        identity: crate::model::CanonicalPullRequestId,
    ) -> Result<(), TuiError> {
        let mapping = self
            .app
            .authored_mappings
            .iter()
            .find(|mapping| mapping.identity == identity)
            .and_then(|mapping| mapping.repository_index);
        let mapped_path = mapping
            .and_then(|index| self.catalog.repositories.get(index))
            .map(|repository| repository.path.clone());
        let credential_anchor = mapping
            .and_then(|index| self.catalog.repositories.get(index))
            .map(|repository| repository.path.clone())
            .unwrap_or_else(|| {
                self.catalog_path
                    .parent()
                    .unwrap_or_else(|| Path::new("."))
                    .to_owned()
            });
        let catalog_path = self.catalog_path.clone();
        let service = self.github_service.clone();
        let job = BackgroundJob::spawn("wt-pr-materialization", move |context| {
            context.progress("refreshing selected pull request");
            let host = AuthoredHost::inferred(&identity.repository.host, credential_anchor.clone());
            let refreshed = service
                .fetch_pull_request_with(&SystemCredentials, &host, &identity)
                .map_err(|error| error.to_string())?;
            if context.is_cancelled() {
                return Err("materialization cancelled".to_owned());
            }
            let token = crate::github::resolve_token(
                &SystemCredentials,
                &identity.repository.host,
                &credential_anchor,
            )
            .map_err(|error| error.to_string())?;
            context.progress("acquiring catalog lock");
            let _lock = config::acquire_catalog_lock_with(
                &catalog_path,
                || context.is_cancelled(),
                || context.progress("waiting for catalog lock"),
            )
            .map_err(|error| error.to_string())?;
            let mut catalog = config::load(&catalog_path).map_err(|error| error.to_string())?;
            let fresh_mapping = mapped_path.as_ref().and_then(|path| {
                catalog
                    .repositories
                    .iter()
                    .position(|repository| &repository.path == path)
            });
            let repository_root =
                config::repository_root(&catalog).map_err(|error| error.to_string())?;
            let runner = context.git_runner();
            let result = bootstrap::bootstrap_repository(
                &runner,
                &runner,
                &mut catalog,
                &repository_root,
                &identity.repository,
                Some(&token),
                fresh_mapping,
            )
            .map_err(|error| error.to_string())?;
            config::save(&catalog_path, &catalog).map_err(|error| error.to_string())?;
            if context.is_cancelled() {
                return Err("materialization cancelled".to_owned());
            }
            let materialized = crate::materialize::materialize_pull_request(
                &runner,
                &runner,
                &result.repository,
                &repository_root,
                &refreshed,
                Some(&token),
            )
            .map_err(|error| error.to_string())?;
            Ok(MaterializationOutcome {
                path: materialized.path,
                refreshed,
            })
        })?;
        self.materialization_progress = Some("refreshing selected pull request".to_owned());
        self.app.progress = self.materialization_progress.clone();
        self.materialization_job = Some(job);
        Ok(())
    }

    fn cancel_materialization(&mut self) {
        if let Some(job) = self.materialization_job.as_ref() {
            job.cancel();
            self.materialization_progress =
                Some("cancelling pull request materialization".to_owned());
            self.app.progress = self.materialization_progress.clone();
        }
    }
}

fn github_refresh_interval(catalog: &Catalog) -> Duration {
    Duration::from_secs(catalog.github_refresh_interval_secs).max(MIN_GITHUB_REFRESH_INTERVAL)
}

fn load_repository_views(catalog: &Catalog, current_directory: &Path) -> Vec<RepositoryView> {
    let mut views: Vec<RepositoryView> = catalog
        .repositories
        .iter()
        .cloned()
        .map(
            |repository| match git::discover_worktrees(&SystemGit, &repository.path) {
                Ok(worktrees) => RepositoryView {
                    config: repository,
                    session_only: false,
                    stale_error: None,
                    expanded: true,
                    worktrees,
                },
                Err(error) => RepositoryView {
                    config: repository,
                    session_only: false,
                    stale_error: Some(error.to_string()),
                    expanded: true,
                    worktrees: Vec::new(),
                },
            },
        )
        .collect();

    if let Ok(identity) = git::resolve_repository(&SystemGit, current_directory) {
        let registered = catalog.repositories.iter().any(|repository| {
            git::resolve_repository(&SystemGit, &repository.path)
                .is_ok_and(|existing| existing.common_git_dir == identity.common_git_dir)
        });
        if !registered {
            let config = RepositoryConfig {
                path: identity.anchor.clone(),
                label: None,
                worktree_root: None,
                github_remote: None,
                github_remotes: Default::default(),
                github_preferred_remote: None,
            };
            match git::discover_worktrees(&SystemGit, &identity.anchor) {
                Ok(worktrees) => views.push(RepositoryView {
                    config,
                    session_only: true,
                    stale_error: None,
                    expanded: true,
                    worktrees,
                }),
                Err(error) => views.push(RepositoryView {
                    config,
                    session_only: true,
                    stale_error: Some(error.to_string()),
                    expanded: true,
                    worktrees: Vec::new(),
                }),
            }
        }
    }
    views
}

fn field(label: &str, value: &str) -> FormField {
    FormField {
        label: label.to_owned(),
        value: value.to_owned(),
    }
}

fn identity(worktree: &Worktree) -> String {
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
        .unwrap_or_else(|| "unknown".to_owned())
}

fn optional_text(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

fn nonempty<'a>(value: &'a str, field: &'static str) -> Result<&'a str, TuiError> {
    let value = value.trim();
    if value.is_empty() {
        invalid(field, "value cannot be empty")
    } else {
        Ok(value)
    }
}

fn require_len(values: &[String], expected: usize, field: &'static str) -> Result<(), TuiError> {
    if values.len() == expected {
        Ok(())
    } else {
        invalid(field, "form fields changed unexpectedly")
    }
}

fn parse_yes_no(value: &str, field: &'static str) -> Result<bool, TuiError> {
    match value.trim().to_ascii_lowercase().as_str() {
        "y" | "yes" | "true" => Ok(true),
        "n" | "no" | "false" | "" => Ok(false),
        _ => invalid(field, "use yes or no"),
    }
}

fn invalid<T>(field: &'static str, message: impl Into<String>) -> Result<T, TuiError> {
    Err(TuiError::InvalidForm {
        field,
        message: message.into(),
    })
}

fn absolute_path(current_directory: &Path, value: &str) -> PathBuf {
    let path = PathBuf::from(value);
    if path.is_absolute() {
        path
    } else {
        current_directory.join(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Modal;
    use std::process::Command;

    #[test]
    fn empty_catalog_inside_repository_creates_session_only_onboarding() {
        let directory = tempfile::tempdir().unwrap();
        let repository = directory.path().join("project");
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(directory.path())
                .args(["init", repository.to_str().unwrap()])
                .status()
                .unwrap()
                .success()
        );
        let views = load_repository_views(&Catalog::default(), &repository);
        assert_eq!(views.len(), 1);
        assert!(views[0].session_only);
        assert_eq!(
            views[0].config.path,
            std::fs::canonicalize(repository).unwrap()
        );
    }

    #[test]
    fn empty_catalog_outside_repository_remains_empty() {
        let directory = tempfile::tempdir().unwrap();
        assert!(load_repository_views(&Catalog::default(), directory.path()).is_empty());
    }

    #[test]
    fn form_parsers_reject_ambiguous_values() {
        assert!(parse_yes_no("maybe", "choice").is_err());
        assert!(nonempty("  ", "branch").is_err());
        assert_eq!(
            absolute_path(Path::new("/base"), "relative"),
            PathBuf::from("/base/relative")
        );
    }

    #[test]
    fn github_refresh_interval_has_a_thirty_second_floor() {
        let mut catalog = Catalog {
            github_refresh_interval_secs: 1,
            ..Catalog::default()
        };
        assert_eq!(github_refresh_interval(&catalog), Duration::from_secs(30));
        catalog.github_refresh_interval_secs = 450;
        assert_eq!(github_refresh_interval(&catalog), Duration::from_secs(450));
    }

    #[test]
    fn overlapping_github_refreshes_coalesce() {
        let directory = tempfile::tempdir().unwrap();
        let catalog = Catalog::default();
        let app = App::new(Vec::new(), directory.path().to_owned());
        let mut controller = Controller::new(directory.path().join("wt.json"), catalog, app);
        controller.github_in_flight = true;
        controller.request_github_refresh();
        controller.request_github_refresh();
        assert!(controller.github_refresh_queued);
    }

    #[test]
    fn ctrl_c_cancels_materialization_without_producing_a_selection() {
        let directory = tempfile::tempdir().unwrap();
        let mut controller = Controller::new(
            directory.path().join("wt.json"),
            Catalog::default(),
            App::new(Vec::new(), directory.path().to_owned()),
        );
        controller.discover_authored_pull_requests = false;
        controller.materialization_job = Some(
            BackgroundJob::spawn("controller-cancel-test", |context| {
                while !context.is_cancelled() {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err("cancelled".to_owned())
            })
            .unwrap(),
        );
        controller.materialization_progress = Some("creating linked worktree".to_owned());
        assert!(matches!(
            controller.handle_intent(Intent::Cancel).unwrap(),
            ControlFlow::Continue
        ));
        let deadline = Instant::now() + Duration::from_secs(2);
        while controller.materialization_job.is_some() {
            assert!(
                Instant::now() < deadline,
                "controller did not observe cancellation"
            );
            controller.pump_materialization();
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(controller.completed_materialization.is_none());
        assert!(
            controller
                .app
                .inline_error
                .as_deref()
                .is_some_and(|message| message.contains("cancelled"))
        );
    }

    #[test]
    fn successful_background_materialization_returns_the_exact_path() {
        use crate::model::{
            AuthoredPullRequest, CanonicalPullRequestId, CheckRollup, GitHubRepositoryIdentity,
            PullRequest, PullRequestIdentity, PullRequestState,
        };

        let directory = tempfile::tempdir().unwrap();
        let path = std::fs::canonicalize(directory.path()).unwrap();
        let refreshed = AuthoredPullRequest {
            identity: CanonicalPullRequestId {
                repository: GitHubRepositoryIdentity::canonical("github.com", "team", "project"),
                number: 42,
            },
            author: "viewer".to_owned(),
            pull_request: PullRequest {
                number: 42,
                title: "Test".to_owned(),
                url: "https://github.com/team/project/pull/42".to_owned(),
                state: PullRequestState::Open,
                updated_at: "2026-08-06T00:00:00Z".to_owned(),
                review_decision: None,
                auto_merge: false,
                base: PullRequestIdentity {
                    repository: Some("team/project".to_owned()),
                    branch: "main".to_owned(),
                    oid: None,
                },
                head: PullRequestIdentity {
                    repository: Some("team/project".to_owned()),
                    branch: "topic".to_owned(),
                    oid: Some("abc".to_owned()),
                },
                checks: CheckRollup::Success,
            },
        };
        let mut controller = Controller::new(
            directory.path().join("wt.json"),
            Catalog::default(),
            App::new(Vec::new(), directory.path().to_owned()),
        );
        controller.discover_authored_pull_requests = false;
        let expected = path.clone();
        controller.materialization_job = Some(
            BackgroundJob::spawn("controller-success-test", move |_context| {
                Ok(MaterializationOutcome {
                    path: expected,
                    refreshed,
                })
            })
            .unwrap(),
        );
        let deadline = Instant::now() + Duration::from_secs(2);
        while controller.completed_materialization.is_none() {
            assert!(
                Instant::now() < deadline,
                "controller did not observe success"
            );
            controller.pump_materialization();
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(controller.completed_materialization, Some(path));
    }

    #[test]
    fn background_github_refresh_persists_remote_identity_cache() {
        let directory = tempfile::tempdir().unwrap();
        let repository = directory.path().join("project.git");
        run_git_command(
            directory.path(),
            &["init", "--bare", repository.to_str().unwrap()],
        );
        run_git_command(
            &repository,
            &[
                "remote",
                "add",
                "upstream",
                "git@github.com:Team/Project.git",
            ],
        );
        let catalog_path = directory.path().join("config/wt.json");
        let catalog = Catalog {
            repositories: vec![RepositoryConfig {
                path: std::fs::canonicalize(&repository).unwrap(),
                label: Some("project".to_owned()),
                worktree_root: None,
                github_remote: Some("upstream".to_owned()),
                github_remotes: Default::default(),
                github_preferred_remote: None,
            }],
            ..Catalog::default()
        };
        config::save(&catalog_path, &catalog).unwrap();
        let views = load_repository_views(&catalog, directory.path());
        let app = App::new(views, directory.path().to_owned());
        let mut controller = Controller::new(catalog_path.clone(), catalog, app);
        controller.discover_authored_pull_requests = false;
        controller.request_github_refresh();
        let deadline = Instant::now() + Duration::from_secs(2);
        while controller.github_in_flight && Instant::now() < deadline {
            controller.pump_background_results();
            std::thread::yield_now();
        }
        assert!(!controller.github_in_flight);
        let stored = config::load(&catalog_path).unwrap();
        assert_eq!(
            stored.repositories[0].github_remotes["upstream"],
            crate::model::GitHubRepositoryIdentity::canonical("github.com", "team", "project")
        );
        assert_eq!(
            stored.repositories[0].github_preferred_remote.as_deref(),
            Some("upstream")
        );
    }

    #[test]
    fn controller_registers_edits_and_unregisters_session_repository() {
        let directory = tempfile::tempdir().unwrap();
        let repository = directory.path().join("project");
        run_git_command(directory.path(), &["init", repository.to_str().unwrap()]);
        let catalog_path = directory.path().join("config/wt.json");
        let catalog = Catalog::default();
        let views = load_repository_views(&catalog, &repository);
        let app = App::new(views.clone(), repository.clone());
        let mut controller = Controller::new(catalog_path.clone(), catalog, app);
        let config = views[0].config.clone();

        controller
            .execute(PendingAction::RegisterRepository {
                repository: config.clone(),
            })
            .unwrap();
        assert_eq!(config::load(&catalog_path).unwrap().repositories.len(), 1);
        controller
            .execute(PendingAction::EditRepository {
                repository: config.path.clone(),
                new_repository: config.path.clone(),
                label: Some("renamed".to_owned()),
                worktree_root: Some(directory.path().join("trees")),
                github_remote: Some("upstream".to_owned()),
            })
            .unwrap();
        assert_eq!(
            config::load(&catalog_path).unwrap().repositories[0]
                .label
                .as_deref(),
            Some("renamed")
        );
        controller
            .execute(PendingAction::RemoveRepository {
                repository: config.path,
            })
            .unwrap();
        assert!(config::load(&catalog_path).unwrap().repositories.is_empty());
    }

    #[test]
    fn create_form_validates_then_opens_exact_confirmation() {
        let directory = tempfile::tempdir().unwrap();
        let repository = directory.path().join("project");
        run_git_command(
            directory.path(),
            &["init", "-b", "main", repository.to_str().unwrap()],
        );
        run_git_command(&repository, &["config", "user.email", "test@example.com"]);
        run_git_command(&repository, &["config", "user.name", "Test User"]);
        run_git_command(&repository, &["commit", "--allow-empty", "-m", "initial"]);
        let identity = git::resolve_repository(&SystemGit, &repository).unwrap();
        let repository_config = RepositoryConfig {
            path: identity.anchor,
            label: Some("project".to_owned()),
            worktree_root: None,
            github_remote: None,
            github_remotes: Default::default(),
            github_preferred_remote: None,
        };
        let catalog = Catalog {
            repositories: vec![repository_config],
            ..Catalog::default()
        };
        let views = load_repository_views(&catalog, &repository);
        let app = App::new(views, repository);
        let mut controller = Controller::new(directory.path().join("wt.json"), catalog, app);
        let destination = directory.path().join("topic");
        controller
            .submit_form(
                Action::Create,
                vec![
                    "new".to_owned(),
                    "topic".to_owned(),
                    "HEAD".to_owned(),
                    destination.to_string_lossy().into_owned(),
                    "no".to_owned(),
                ],
            )
            .unwrap();
        assert!(matches!(
            controller.pending_action,
            Some(PendingAction::Create { .. })
        ));
        assert!(matches!(controller.app.modal, Some(Modal::Confirm { .. })));
    }

    #[test]
    fn stale_repository_edit_form_relinks_the_catalog_entry() {
        let directory = tempfile::tempdir().unwrap();
        let relocated = directory.path().join("relocated");
        run_git_command(directory.path(), &["init", relocated.to_str().unwrap()]);
        let catalog_path = directory.path().join("wt.json");
        let catalog = Catalog {
            repositories: vec![RepositoryConfig {
                path: directory.path().join("missing"),
                label: Some("stale".to_owned()),
                worktree_root: None,
                github_remote: None,
                github_remotes: Default::default(),
                github_preferred_remote: None,
            }],
            ..Catalog::default()
        };
        config::save(&catalog_path, &catalog).unwrap();
        let views = load_repository_views(&catalog, directory.path());
        let app = App::new(views, directory.path().to_owned());
        let mut controller = Controller::new(catalog_path.clone(), catalog, app);
        controller
            .submit_form(
                Action::EditRepository,
                vec![
                    relocated.to_string_lossy().into_owned(),
                    "relinked".to_owned(),
                    String::new(),
                    "origin".to_owned(),
                ],
            )
            .unwrap();
        let pending = controller.pending_action.take().unwrap();
        controller.execute(pending).unwrap();
        let stored = config::load(&catalog_path).unwrap();
        assert_eq!(stored.repositories[0].label.as_deref(), Some("relinked"));
        assert_eq!(
            stored.repositories[0].path,
            std::fs::canonicalize(relocated).unwrap()
        );
    }

    fn run_git_command(directory: &Path, arguments: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(directory)
            .args(arguments)
            .status()
            .unwrap();
        assert!(status.success(), "git {arguments:?} failed");
    }
}
