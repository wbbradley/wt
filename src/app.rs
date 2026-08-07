use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::github::{GitHubError, PullRequestMapping};
use crate::model::{
    AuthoredPullRequest, CanonicalPullRequestId, GitHubBranchData, GitHubRepositoryIdentity,
    PullRequest, PullRequestDetails, RepositoryConfig, Worktree, WorktreeStatus,
};
use crate::prompt::{PromptPullRequest, format_agent_prompt};

const LIST_SCROLL_MARGIN: usize = 5;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum RowId {
    Repository(PathBuf),
    Worktree(PathBuf),
    VirtualRepository(GitHubRepositoryIdentity),
    VirtualPullRequest(CanonicalPullRequestId),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StatusState {
    Pending,
    Ready(WorktreeStatus),
    Error(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GitHubState {
    Loading {
        previous: Option<GitHubBranchData>,
    },
    Ready(GitHubBranchData),
    Stale {
        previous: Option<GitHubBranchData>,
        error: String,
    },
}

#[derive(Clone, Debug, Default)]
pub struct AuthoredPullRequestState {
    baseline: BTreeMap<CanonicalPullRequestId, AuthoredPullRequest>,
    pending: BTreeMap<CanonicalPullRequestId, AuthoredPullRequest>,
    pub generation: u64,
    pub loading: bool,
    pub warnings: Vec<String>,
    pub stale_error: Option<String>,
    pub current_host: Option<String>,
    pub current_page: usize,
}

impl AuthoredPullRequestState {
    pub fn hydrate(&mut self, pull_requests: Vec<AuthoredPullRequest>) {
        if self.loading {
            return;
        }
        self.baseline = pull_requests
            .into_iter()
            .map(|pull_request| (pull_request.identity.clone(), pull_request))
            .collect();
    }

    pub fn begin(&mut self) -> u64 {
        self.generation = self.generation.wrapping_add(1);
        self.pending.clear();
        self.loading = true;
        self.warnings.clear();
        self.stale_error = None;
        self.current_host = None;
        self.current_page = 0;
        self.generation
    }

    pub fn apply_page(
        &mut self,
        generation: u64,
        host: String,
        page: usize,
        pull_requests: Vec<AuthoredPullRequest>,
        warnings: Vec<String>,
    ) -> bool {
        if generation != self.generation || !self.loading {
            return false;
        }
        for pull_request in pull_requests {
            self.pending
                .insert(pull_request.identity.clone(), pull_request);
        }
        self.warnings.extend(warnings);
        self.current_host = Some(host);
        self.current_page = page;
        true
    }

    pub fn finish(
        &mut self,
        generation: u64,
        complete: bool,
        warnings: Vec<String>,
        error: Option<String>,
    ) -> bool {
        if generation != self.generation || !self.loading {
            return false;
        }
        self.loading = false;
        self.current_host = None;
        self.current_page = 0;
        self.warnings = warnings;
        if complete {
            self.baseline = std::mem::take(&mut self.pending);
            self.stale_error = None;
        } else {
            self.pending.clear();
            self.stale_error = error;
        }
        true
    }

    pub fn visible(&self) -> Vec<AuthoredPullRequest> {
        let mut visible = self.baseline.clone();
        if self.loading {
            visible.extend(self.pending.clone());
        }
        visible.into_values().collect()
    }

    pub fn identities(&self) -> Vec<CanonicalPullRequestId> {
        self.visible()
            .into_iter()
            .map(|pull_request| pull_request.identity)
            .collect()
    }
}

impl GitHubState {
    pub fn data(&self) -> Option<&GitHubBranchData> {
        match self {
            Self::Loading { previous } | Self::Stale { previous, .. } => previous.as_ref(),
            Self::Ready(data) => Some(data),
        }
    }
}

#[derive(Clone, Debug)]
pub struct RepositoryView {
    pub config: RepositoryConfig,
    pub session_only: bool,
    pub stale_error: Option<String>,
    pub expanded: bool,
    pub worktrees: Vec<Worktree>,
}

impl RepositoryView {
    pub fn id(&self) -> RowId {
        RowId::Repository(self.config.path.clone())
    }

    pub fn is_bare(&self) -> bool {
        self.worktrees.iter().any(|worktree| worktree.bare)
    }
}

#[derive(Clone, Debug)]
pub struct VirtualRepositoryView {
    pub identity: GitHubRepositoryIdentity,
    pub mapped_repository: Option<PathBuf>,
    pub expanded: bool,
    pub pull_requests: Vec<AuthoredPullRequest>,
}

impl VirtualRepositoryView {
    pub fn id(&self) -> RowId {
        RowId::VirtualRepository(self.identity.clone())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VisibleRow {
    Repository {
        repository_index: usize,
        id: RowId,
    },
    Worktree {
        repository_index: usize,
        worktree_index: usize,
        stack_depth: usize,
        id: RowId,
    },
    VirtualRepository {
        virtual_repository_index: usize,
        id: RowId,
    },
    VirtualPullRequest {
        virtual_repository_index: usize,
        pull_request_index: usize,
        mapped_repository_index: Option<usize>,
        stack_depth: usize,
        id: RowId,
    },
}

#[derive(Clone, Copy, Debug, Hash, Ord, PartialEq, PartialOrd, Eq)]
pub enum DetailSection {
    Attention,
    Checks,
    Reviews,
    Feedback,
}

#[derive(Clone, Debug, Hash, Ord, PartialEq, PartialOrd, Eq)]
pub enum DetailRowId {
    Summary(CanonicalPullRequestId),
    Section(CanonicalPullRequestId, DetailSection),
    Check(CanonicalPullRequestId, String),
    ReviewRequest(CanonicalPullRequestId, String),
    Review(CanonicalPullRequestId, String),
    Feedback(CanonicalPullRequestId, String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DetailRow {
    pub id: DetailRowId,
    pub lines: Vec<String>,
    pub url: String,
}

impl VisibleRow {
    pub fn id(&self) -> &RowId {
        match self {
            Self::Repository { id, .. }
            | Self::Worktree { id, .. }
            | Self::VirtualRepository { id, .. }
            | Self::VirtualPullRequest { id, .. } => id,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pane {
    List,
    Detail,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Action {
    CopyAgentPrompt,
    Create,
    NewWorktree,
    Move,
    Lock,
    Unlock,
    Remove,
    Repair,
    Prune,
    RegisterRepository,
    EditRepository,
    RemoveRepository,
}

impl Action {
    pub const ALL: [Self; 12] = [
        Self::CopyAgentPrompt,
        Self::Create,
        Self::NewWorktree,
        Self::Move,
        Self::Lock,
        Self::Unlock,
        Self::Remove,
        Self::Repair,
        Self::Prune,
        Self::RegisterRepository,
        Self::EditRepository,
        Self::RemoveRepository,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::CopyAgentPrompt => "copy agent prompt",
            Self::Create => "create worktree",
            Self::NewWorktree => "new tracked worktree",
            Self::Move => "move worktree",
            Self::Lock => "lock worktree",
            Self::Unlock => "unlock worktree",
            Self::Remove => "remove worktree",
            Self::Repair => "repair worktree",
            Self::Prune => "prune stale records",
            Self::RegisterRepository => "register repository",
            Self::EditRepository => "edit repository",
            Self::RemoveRepository => "unregister repository",
        }
    }

    pub fn shortcut(self) -> &'static str {
        match self {
            Self::CopyAgentPrompt => "C",
            Self::Create => "c",
            Self::NewWorktree => "n",
            Self::Move => "m",
            Self::Lock => "L",
            Self::Unlock => "U",
            Self::Remove => "d",
            Self::Repair => "R",
            Self::Prune => "p",
            Self::RegisterRepository => "a",
            Self::EditRepository => "e",
            Self::RemoveRepository => "x",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActionAvailability {
    pub action: Action,
    pub enabled: bool,
    pub reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FormField {
    pub label: String,
    pub value: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Modal {
    Palette {
        selected: usize,
    },
    Form {
        action: Action,
        fields: Vec<FormField>,
        active: usize,
    },
    Confirm {
        action: Action,
        summary: Vec<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Intent {
    None,
    Accept(PathBuf),
    Cancel,
    Refresh,
    RefreshGitHub,
    BeginAction(Action),
    SubmitForm { action: Action, values: Vec<String> },
    ConfirmAction(Action),
    MaterializePullRequest(CanonicalPullRequestId),
    OpenUrl(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatusUpdate {
    pub generation: u64,
    pub path: PathBuf,
    pub result: Result<WorktreeStatus, String>,
}

#[derive(Debug)]
pub struct App {
    pub repositories: Vec<RepositoryView>,
    pub virtual_repositories: Vec<VirtualRepositoryView>,
    pub selected: Option<RowId>,
    pub filter: String,
    pub filter_active: bool,
    pub pane: Pane,
    pub scroll: usize,
    pub viewport_height: usize,
    viewport_initialized: bool,
    pub detail_scroll: usize,
    detail_max_scroll: usize,
    pub detail_selected: Option<DetailRowId>,
    detail_viewport_height: usize,
    pub modal: Option<Modal>,
    pub inline_error: Option<String>,
    pub progress: Option<String>,
    pub statuses: HashMap<PathBuf, StatusState>,
    pub branch_parents: HashMap<PathBuf, PathBuf>,
    pub github: HashMap<PathBuf, GitHubState>,
    pub github_generation: u64,
    pub github_loading: bool,
    pub github_hosts: BTreeSet<String>,
    pub authored_pull_requests: AuthoredPullRequestState,
    pub active_pull_requests: HashSet<CanonicalPullRequestId>,
    pub pull_request_details: BTreeMap<CanonicalPullRequestId, PullRequestDetails>,
    pub pull_request_detail_errors: BTreeMap<CanonicalPullRequestId, String>,
    pub authored_mappings: Vec<PullRequestMapping>,
    pub current_directory: PathBuf,
    pub generation: u64,
    pending_status: usize,
    refresh_queued: bool,
}

impl App {
    pub fn new(repositories: Vec<RepositoryView>, current_directory: PathBuf) -> Self {
        let mut app = Self {
            repositories,
            virtual_repositories: Vec::new(),
            selected: None,
            filter: String::new(),
            filter_active: false,
            pane: Pane::List,
            scroll: 0,
            viewport_height: 1,
            viewport_initialized: false,
            detail_scroll: 0,
            detail_max_scroll: 0,
            detail_selected: None,
            detail_viewport_height: 1,
            modal: None,
            inline_error: None,
            progress: None,
            statuses: HashMap::new(),
            branch_parents: HashMap::new(),
            github: HashMap::new(),
            github_generation: 0,
            github_loading: false,
            github_hosts: BTreeSet::new(),
            authored_pull_requests: AuthoredPullRequestState::default(),
            active_pull_requests: HashSet::new(),
            pull_request_details: BTreeMap::new(),
            pull_request_detail_errors: BTreeMap::new(),
            authored_mappings: Vec::new(),
            current_directory,
            generation: 0,
            pending_status: 0,
            refresh_queued: false,
        };
        app.select_initial();
        app
    }

    pub fn visible_rows(&self) -> Vec<VisibleRow> {
        let filter = self.filter.to_ascii_lowercase();
        let mut rows = Vec::new();
        for (repository_index, repository) in self.repositories.iter().enumerate() {
            let repository_matches = filter.is_empty()
                || repository
                    .config
                    .display_label()
                    .to_ascii_lowercase()
                    .contains(&filter)
                || repository
                    .config
                    .path
                    .to_string_lossy()
                    .to_ascii_lowercase()
                    .contains(&filter)
                || (repository.is_bare() && "bare".contains(&filter));
            let matching_worktrees: Vec<usize> = repository
                .worktrees
                .iter()
                .enumerate()
                .filter(|(_, worktree)| {
                    !worktree.bare
                        && (repository_matches
                            || self.worktree_matches(repository, worktree, &filter))
                })
                .map(|(index, _)| index)
                .collect();
            let mapped_virtual_repositories: Vec<usize> = self
                .virtual_repositories
                .iter()
                .enumerate()
                .filter(|(_, virtual_repository)| {
                    virtual_repository.mapped_repository.as_deref()
                        == Some(repository.config.path.as_path())
                })
                .map(|(index, _)| index)
                .collect();
            let mapped_virtual_matches = mapped_virtual_repositories.iter().any(|index| {
                let virtual_repository = &self.virtual_repositories[*index];
                virtual_repository_matches(virtual_repository, &filter)
                    || virtual_repository.pull_requests.iter().any(|pull_request| {
                        self.virtual_pull_request_matches(pull_request, &filter)
                    })
            });
            if !repository_matches && matching_worktrees.is_empty() && !mapped_virtual_matches {
                continue;
            }
            rows.push(VisibleRow::Repository {
                repository_index,
                id: repository.id(),
            });
            if repository.expanded || !filter.is_empty() {
                let mut included_worktrees: BTreeSet<usize> =
                    matching_worktrees.into_iter().collect();
                if !repository_matches {
                    let indexes = repository
                        .worktrees
                        .iter()
                        .enumerate()
                        .map(|(index, worktree)| (worktree.path.clone(), index))
                        .collect::<HashMap<_, _>>();
                    for mut index in included_worktrees.clone() {
                        let mut visited = BTreeSet::new();
                        while let Some(parent) = self
                            .branch_parents
                            .get(&repository.worktrees[index].path)
                            .and_then(|path| indexes.get(path))
                            .copied()
                        {
                            if !visited.insert(parent) {
                                break;
                            }
                            included_worktrees.insert(parent);
                            index = parent;
                        }
                    }
                }
                for tree_row in nested_worktrees(
                    &repository.worktrees,
                    &self.branch_parents,
                    &included_worktrees,
                ) {
                    let worktree_index = tree_row.index;
                    let worktree = &repository.worktrees[worktree_index];
                    rows.push(VisibleRow::Worktree {
                        repository_index,
                        worktree_index,
                        stack_depth: tree_row.depth,
                        id: RowId::Worktree(worktree.path.clone()),
                    });
                }
                self.append_virtual_rows(
                    &mut rows,
                    &filter,
                    Some(repository.config.path.as_path()),
                    mapped_virtual_repositories.len() > 1,
                    (mapped_virtual_repositories.len() == 1).then_some(repository_index),
                    repository_matches,
                );
            }
        }
        self.append_virtual_rows(&mut rows, &filter, None, true, None, false);
        rows
    }

    fn append_virtual_rows(
        &self,
        rows: &mut Vec<VisibleRow>,
        filter: &str,
        mapped_repository: Option<&Path>,
        show_repository_header: bool,
        mapped_repository_index: Option<usize>,
        parent_matches: bool,
    ) {
        for (virtual_repository_index, repository) in self
            .virtual_repositories
            .iter()
            .enumerate()
            .filter(|(_, repository)| repository.mapped_repository.as_deref() == mapped_repository)
        {
            let repository_matches = parent_matches
                || filter.is_empty()
                || virtual_repository_matches(repository, filter);
            let pull_request_tree = nested_pull_requests(&repository.pull_requests);
            let mut included_pull_requests: BTreeSet<usize> = if repository_matches {
                (0..repository.pull_requests.len()).collect()
            } else {
                repository
                    .pull_requests
                    .iter()
                    .enumerate()
                    .filter(|(_, pull_request)| {
                        self.virtual_pull_request_matches(pull_request, filter)
                    })
                    .map(|(index, _)| index)
                    .collect()
            };
            if !repository_matches {
                let parents: Vec<Option<usize>> = pull_request_tree.iter().fold(
                    vec![None; repository.pull_requests.len()],
                    |mut parents, row| {
                        parents[row.index] = row.parent;
                        parents
                    },
                );
                for mut index in included_pull_requests.clone() {
                    while let Some(parent) = parents[index] {
                        included_pull_requests.insert(parent);
                        index = parent;
                    }
                }
            }
            if included_pull_requests.is_empty() {
                continue;
            }
            if show_repository_header {
                rows.push(VisibleRow::VirtualRepository {
                    virtual_repository_index,
                    id: repository.id(),
                });
            }
            if !show_repository_header || repository.expanded || !filter.is_empty() {
                for tree_row in pull_request_tree
                    .iter()
                    .filter(|row| included_pull_requests.contains(&row.index))
                {
                    let pull_request = &repository.pull_requests[tree_row.index];
                    rows.push(VisibleRow::VirtualPullRequest {
                        virtual_repository_index,
                        pull_request_index: tree_row.index,
                        mapped_repository_index,
                        stack_depth: tree_row.depth,
                        id: RowId::VirtualPullRequest(pull_request.identity.clone()),
                    });
                }
            }
        }
    }

    fn worktree_matches(
        &self,
        repository: &RepositoryView,
        worktree: &Worktree,
        filter: &str,
    ) -> bool {
        if filter.is_empty() {
            return true;
        }
        let local = self
            .statuses
            .get(&worktree.path)
            .and_then(|status| match status {
                StatusState::Ready(status) => Some(status.summary()),
                StatusState::Error(error) => Some(error.clone()),
                StatusState::Pending => None,
            })
            .unwrap_or_default();
        let github = self
            .github
            .get(&worktree.path)
            .map(|state| match state {
                GitHubState::Loading { previous } => previous
                    .as_ref()
                    .map(|data| self.github_search_text(repository, data))
                    .unwrap_or_else(|| "github loading".to_owned()),
                GitHubState::Ready(data) => self.github_search_text(repository, data),
                GitHubState::Stale { previous, error } => {
                    let mut text = previous
                        .as_ref()
                        .map(|data| self.github_search_text(repository, data))
                        .unwrap_or_default();
                    text.push(' ');
                    text.push_str(error);
                    text
                }
            })
            .unwrap_or_default();
        worktree
            .path
            .to_string_lossy()
            .to_ascii_lowercase()
            .contains(filter)
            || worktree
                .branch
                .as_deref()
                .unwrap_or_default()
                .to_ascii_lowercase()
                .contains(filter)
            || worktree
                .head
                .as_deref()
                .unwrap_or_default()
                .to_ascii_lowercase()
                .contains(filter)
            || local.to_ascii_lowercase().contains(filter)
            || github.to_ascii_lowercase().contains(filter)
    }

    pub fn selected_row(&self) -> Option<VisibleRow> {
        let selected = self.selected.as_ref()?;
        self.visible_rows()
            .into_iter()
            .find(|row| row.id() == selected)
    }

    pub fn selected_worktree(&self) -> Option<(&RepositoryView, &Worktree, usize)> {
        match self.selected_row()? {
            VisibleRow::Worktree {
                repository_index,
                worktree_index,
                ..
            } => Some((
                &self.repositories[repository_index],
                &self.repositories[repository_index].worktrees[worktree_index],
                worktree_index,
            )),
            VisibleRow::Repository { .. }
            | VisibleRow::VirtualRepository { .. }
            | VisibleRow::VirtualPullRequest { .. } => None,
        }
    }

    pub fn selected_repository(&self) -> Option<(&RepositoryView, usize)> {
        let row = self.selected_row()?;
        let index = match row {
            VisibleRow::Repository {
                repository_index, ..
            }
            | VisibleRow::Worktree {
                repository_index, ..
            } => repository_index,
            VisibleRow::VirtualRepository { .. } | VisibleRow::VirtualPullRequest { .. } => {
                return None;
            }
        };
        Some((&self.repositories[index], index))
    }

    pub fn selected_virtual_pull_request(
        &self,
    ) -> Option<(&VirtualRepositoryView, &AuthoredPullRequest)> {
        match self.selected_row()? {
            VisibleRow::VirtualPullRequest {
                virtual_repository_index,
                pull_request_index,
                ..
            } => Some((
                &self.virtual_repositories[virtual_repository_index],
                &self.virtual_repositories[virtual_repository_index].pull_requests
                    [pull_request_index],
            )),
            _ => None,
        }
    }

    pub fn detail_rows(&self) -> Vec<DetailRow> {
        let Some((identity, pull_request, details, mut context)) =
            self.selected_pull_request_data()
        else {
            return Vec::new();
        };
        let pr_url = pull_request.url.clone();
        context.extend([
            format!("title: {}", pull_request.title),
            format!("URL: {}", pull_request.url),
            format!(
                "base: {}:{}",
                pull_request.base.repository.as_deref().unwrap_or("unknown"),
                pull_request.base.branch
            ),
            format!(
                "head: {}:{}",
                pull_request.head.repository.as_deref().unwrap_or("unknown"),
                pull_request.head.branch
            ),
            format!(
                "head SHA: {}",
                pull_request.head.oid.as_deref().unwrap_or("unknown")
            ),
            format!(
                "state: {} · updated {}",
                pull_request.state, pull_request.updated_at
            ),
            format!(
                "auto-merge: {}",
                if pull_request.auto_merge {
                    "enabled"
                } else {
                    "off"
                }
            ),
        ]);
        if let Some(error) = self.pull_request_detail_errors.get(&identity) {
            context.push(format!("details stale: {error}"));
        }
        if let Some(details) = &details {
            context.push(format!("conflict: {}", debug_label(details.merge_conflict)));
            context.extend(
                details
                    .warnings
                    .iter()
                    .map(|warning| format!("warning: {warning}")),
            );
        } else {
            context.push("attention details: loading or unavailable".to_owned());
        }
        let mut rows = vec![DetailRow {
            id: DetailRowId::Summary(identity.clone()),
            lines: context,
            url: pr_url.clone(),
        }];

        let summary = details.as_ref().map(PullRequestDetails::attention_summary);
        rows.push(DetailRow {
            id: DetailRowId::Section(identity.clone(), DetailSection::Attention),
            lines: vec![format!(
                "Attention · checks {} · review {} · feedback {} · optional failures {} · conflict {}",
                summary
                    .map(|summary| debug_label(summary.required_checks))
                    .unwrap_or_else(|| "unknown".to_owned()),
                summary
                    .map(|summary| debug_label(summary.review))
                    .unwrap_or_else(|| "unknown".to_owned()),
                summary.map(|summary| summary.unresolved_feedback).unwrap_or(0),
                summary.map(|summary| summary.optional_failures).unwrap_or(0),
                summary
                    .map(|summary| debug_label(summary.merge_conflict))
                    .unwrap_or_else(|| "unknown".to_owned()),
            )],
            url: pr_url.clone(),
        });

        let mut checks = details
            .as_ref()
            .map(|details| details.checks.clone())
            .unwrap_or_default();
        checks.sort_by_key(|check| (check_attention_rank(check.state), check.source_order));
        rows.push(DetailRow {
            id: DetailRowId::Section(identity.clone(), DetailSection::Checks),
            lines: vec![if checks.is_empty() {
                "Checks · none or unavailable".to_owned()
            } else {
                format!("Checks · {}", checks.len())
            }],
            url: pr_url.clone(),
        });
        rows.extend(checks.into_iter().map(|check| {
            let mut lines = vec![format!(
                "{} · {} · {}",
                check.name,
                debug_label(check.state),
                if check.required {
                    "required"
                } else {
                    "optional"
                }
            )];
            if let Some(url) = &check.target_url {
                lines.push(format!("URL: {url}"));
            }
            DetailRow {
                id: DetailRowId::Check(identity.clone(), check.name),
                lines,
                url: check.target_url.unwrap_or_else(|| pr_url.clone()),
            }
        }));

        let review_requests = details
            .as_ref()
            .map(|details| details.review_requests.clone())
            .unwrap_or_default();
        let reviewer_reviews = details
            .as_ref()
            .map(|details| details.reviewer_reviews.clone())
            .unwrap_or_default();
        rows.push(DetailRow {
            id: DetailRowId::Section(identity.clone(), DetailSection::Reviews),
            lines: vec![
                if review_requests.is_empty() && reviewer_reviews.is_empty() {
                    "Reviews · none or unavailable".to_owned()
                } else {
                    format!(
                        "Reviews · {} requested · {} submitted",
                        review_requests.len(),
                        reviewer_reviews.len()
                    )
                },
            ],
            url: pr_url.clone(),
        });
        rows.extend(review_requests.into_iter().map(|request| DetailRow {
            id: DetailRowId::ReviewRequest(identity.clone(), request.id.clone()),
            lines: vec![format!(
                "requested: {} ({}) · github id {}",
                request.name,
                debug_label(request.kind),
                request.id
            )],
            url: pr_url.clone(),
        }));
        rows.extend(reviewer_reviews.into_iter().map(|review| DetailRow {
            id: DetailRowId::Review(identity.clone(), review.id.clone()),
            lines: vec![format!(
                "{} · {} · github id {}{}",
                review.reviewer,
                debug_label(review.state),
                review.id,
                review
                    .database_id
                    .map(|id| format!(" · review id {id}"))
                    .unwrap_or_default()
            )],
            url: pr_url.clone(),
        }));

        let feedback = details
            .as_ref()
            .map(|details| details.feedback.clone())
            .unwrap_or_default();
        rows.push(DetailRow {
            id: DetailRowId::Section(identity.clone(), DetailSection::Feedback),
            lines: vec![if feedback.is_empty() {
                "Feedback · none or unavailable".to_owned()
            } else {
                format!("Feedback · {}", feedback.len())
            }],
            url: pr_url.clone(),
        });
        rows.extend(feedback.into_iter().map(|feedback| {
            let mut metadata = vec![
                debug_label(feedback.kind),
                feedback.author,
                format!("github id {}", feedback.id),
            ];
            if let Some(id) = feedback.database_id {
                metadata.push(format!("id {id}"));
            }
            if let Some(path) = feedback.path {
                metadata.push(path);
            }
            if let Some(thread_id) = feedback.thread_id {
                metadata.push(format!("thread {thread_id}"));
            }
            if feedback.outdated {
                metadata.push("outdated".to_owned());
            }
            let url = feedback.permalink.unwrap_or_else(|| pr_url.clone());
            metadata.push(format!("permalink {url}"));
            DetailRow {
                id: DetailRowId::Feedback(identity.clone(), feedback.id),
                lines: vec![
                    metadata.join(" · "),
                    feedback
                        .body
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" "),
                ],
                url,
            }
        }));
        rows
    }

    fn selected_pull_request_data(
        &self,
    ) -> Option<(
        CanonicalPullRequestId,
        PullRequest,
        Option<PullRequestDetails>,
        Vec<String>,
    )> {
        if let Some((repository, authored)) = self.selected_virtual_pull_request() {
            let mut context = vec![format!("repository: {}", repository.identity.full_name())];
            context.push(format!(
                "local repo: {}",
                repository
                    .mapped_repository
                    .as_ref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| "none (virtual)".to_owned())
            ));
            context.push(format!("author: {}", authored.author));
            if self.authored_pull_requests.loading {
                context.push("GitHub: refreshing".to_owned());
            } else if let Some(error) = &self.authored_pull_requests.stale_error {
                context.push(format!("GitHub stale: {error}"));
            }
            return Some((
                authored.identity.clone(),
                authored.pull_request.clone(),
                self.pull_request_details.get(&authored.identity).cloned(),
                context,
            ));
        }
        let (repository, worktree, _) = self.selected_worktree()?;
        let github_state = self.github.get(&worktree.path)?;
        let data = github_state.data()?;
        let pull_request = data.pull_request.as_ref()?;
        let identity = self.pull_request_identity(repository, pull_request)?;
        let mut context = vec![
            format!("repository: {}", repository.config.display_label()),
            format!("local path: {}", worktree.path.display()),
        ];
        if let Some(status) = self.statuses.get(&worktree.path) {
            context.push(match status {
                StatusState::Pending => "local status: loading".to_owned(),
                StatusState::Ready(status) => format!("local status: {}", status.summary()),
                StatusState::Error(error) => format!("local status error: {error}"),
            });
        }
        match github_state {
            GitHubState::Loading { .. } => context.push("GitHub: refreshing".to_owned()),
            GitHubState::Stale { error, .. } => context.push(format!("GitHub stale: {error}")),
            GitHubState::Ready(_) => {}
        }
        context.extend(
            data.warnings
                .iter()
                .map(|warning| format!("warning: {warning}")),
        );
        Some((
            identity.clone(),
            pull_request.clone(),
            self.pull_request_details.get(&identity).cloned(),
            context,
        ))
    }

    pub fn pull_request_identity(
        &self,
        repository: &RepositoryView,
        pull_request: &PullRequest,
    ) -> Option<CanonicalPullRequestId> {
        let base_repository = pull_request.base.repository.as_deref()?;
        let mut remote_names = Vec::new();
        remote_names.extend(repository.config.github_preferred_remote.iter().cloned());
        remote_names.extend(repository.config.github_remote.iter().cloned());
        remote_names.push("origin".to_owned());
        remote_names.extend(repository.config.github_remotes.keys().cloned());
        remote_names
            .into_iter()
            .filter_map(|name| repository.config.github_remotes.get(&name))
            .find(|identity| identity.full_name().eq_ignore_ascii_case(base_repository))
            .map(|repository| CanonicalPullRequestId {
                repository: repository.clone(),
                number: pull_request.number,
            })
    }

    pub fn pull_request_details_for<'a>(
        &'a self,
        repository: &RepositoryView,
        pull_request: &PullRequest,
    ) -> Option<(&'a CanonicalPullRequestId, &'a PullRequestDetails)> {
        let identity = self.pull_request_identity(repository, pull_request)?;
        self.pull_request_details.get_key_value(&identity)
    }

    fn github_search_text(&self, repository: &RepositoryView, data: &GitHubBranchData) -> String {
        let mut text = github_search_text(data);
        if let Some(pull_request) = &data.pull_request
            && let Some((_, details)) = self.pull_request_details_for(repository, pull_request)
        {
            text.push(' ');
            text.push_str(&pull_request_details_search_text(details));
        }
        text
    }

    fn virtual_pull_request_matches(
        &self,
        pull_request: &AuthoredPullRequest,
        filter: &str,
    ) -> bool {
        virtual_pull_request_matches(pull_request, filter)
            || self
                .pull_request_details
                .get(&pull_request.identity)
                .is_some_and(|details| {
                    pull_request_details_search_text(details)
                        .to_ascii_lowercase()
                        .contains(filter)
                })
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Intent {
        self.inline_error = None;
        if let Some(modal) = self.modal.clone() {
            return self.handle_modal_key(modal, key);
        }
        if self.filter_active {
            return self.handle_filter_key(key);
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return match key.code {
                KeyCode::Char('c') => Intent::Cancel,
                KeyCode::Char('d') => {
                    if self.pane == Pane::Detail {
                        let distance = (self.detail_viewport_height / 2).max(1) as isize;
                        if self.detail_rows().is_empty() {
                            self.scroll_detail(distance);
                        } else {
                            self.move_detail_selection(distance);
                        }
                    } else {
                        self.move_selection((self.viewport_height / 2).max(1) as isize);
                    }
                    Intent::None
                }
                KeyCode::Char('u') => {
                    if self.pane == Pane::Detail {
                        let distance = (self.detail_viewport_height / 2).max(1) as isize;
                        if self.detail_rows().is_empty() {
                            self.scroll_detail(-distance);
                        } else {
                            self.move_detail_selection(-distance);
                        }
                    } else {
                        self.move_selection(-((self.viewport_height / 2).max(1) as isize));
                    }
                    Intent::None
                }
                _ => Intent::None,
            };
        }
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                if self.pane == Pane::Detail {
                    if self.detail_rows().is_empty() {
                        self.scroll_detail(1);
                    } else {
                        self.move_detail_selection(1);
                    }
                    Intent::None
                } else {
                    self.move_and_continue(1)
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if self.pane == Pane::Detail {
                    if self.detail_rows().is_empty() {
                        self.scroll_detail(-1);
                    } else {
                        self.move_detail_selection(-1);
                    }
                    Intent::None
                } else {
                    self.move_and_continue(-1)
                }
            }
            KeyCode::Char('g') => {
                if self.pane == Pane::Detail {
                    if self.detail_rows().is_empty() {
                        self.detail_scroll = 0;
                    } else {
                        self.select_detail_index(0);
                    }
                } else {
                    self.select_index(0);
                }
                Intent::None
            }
            KeyCode::Char('G') => {
                if self.pane == Pane::Detail {
                    let detail_rows = self.detail_rows();
                    if detail_rows.is_empty() {
                        self.detail_scroll = self.detail_max_scroll;
                    } else {
                        self.select_detail_index(detail_rows.len() - 1);
                    }
                } else {
                    let length = self.visible_rows().len();
                    if length > 0 {
                        self.select_index(length - 1);
                    }
                }
                Intent::None
            }
            KeyCode::Char('h') | KeyCode::Left => {
                self.collapse_or_focus_list();
                Intent::None
            }
            KeyCode::Char('l') | KeyCode::Right => {
                self.expand_or_focus_detail();
                Intent::None
            }
            KeyCode::Char('/') => {
                self.filter_active = true;
                Intent::None
            }
            KeyCode::Char('r') => self.request_refresh(),
            KeyCode::Char('?') | KeyCode::Char(' ') => {
                self.modal = Some(Modal::Palette { selected: 0 });
                Intent::None
            }
            KeyCode::Char('q') => Intent::Cancel,
            KeyCode::Char(character) => self.direct_action(character),
            KeyCode::Enter => {
                if self.pane == Pane::Detail {
                    self.open_selected_detail()
                } else {
                    self.accept_or_toggle()
                }
            }
            KeyCode::Esc => Intent::Cancel,
            _ => Intent::None,
        }
    }

    fn handle_filter_key(&mut self, key: KeyEvent) -> Intent {
        match key.code {
            KeyCode::Enter => self.filter_active = false,
            KeyCode::Esc => {
                self.filter_active = false;
                self.filter.clear();
            }
            KeyCode::Backspace => {
                self.filter.pop();
            }
            KeyCode::Char(character)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.filter.push(character);
            }
            _ => {}
        }
        self.ensure_selection_visible();
        Intent::None
    }

    fn handle_modal_key(&mut self, modal: Modal, key: KeyEvent) -> Intent {
        match modal {
            Modal::Palette { mut selected } => match key.code {
                KeyCode::Esc => {
                    self.modal = None;
                    Intent::None
                }
                KeyCode::Char('j') | KeyCode::Down => {
                    selected = (selected + 1).min(Action::ALL.len() - 1);
                    self.modal = Some(Modal::Palette { selected });
                    Intent::None
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    selected = selected.saturating_sub(1);
                    self.modal = Some(Modal::Palette { selected });
                    Intent::None
                }
                KeyCode::Enter => {
                    let availability = self.action_availability(Action::ALL[selected]);
                    if availability.enabled {
                        self.modal = None;
                        Intent::BeginAction(availability.action)
                    } else {
                        self.inline_error = availability.reason;
                        Intent::None
                    }
                }
                _ => Intent::None,
            },
            Modal::Form {
                action,
                mut fields,
                mut active,
            } => match key.code {
                KeyCode::Esc => {
                    self.modal = None;
                    Intent::None
                }
                KeyCode::Tab | KeyCode::Down => {
                    active = (active + 1) % fields.len().max(1);
                    self.modal = Some(Modal::Form {
                        action,
                        fields,
                        active,
                    });
                    Intent::None
                }
                KeyCode::BackTab | KeyCode::Up => {
                    active = active
                        .checked_sub(1)
                        .unwrap_or(fields.len().saturating_sub(1));
                    self.modal = Some(Modal::Form {
                        action,
                        fields,
                        active,
                    });
                    Intent::None
                }
                KeyCode::Backspace => {
                    if let Some(field) = fields.get_mut(active) {
                        field.value.pop();
                    }
                    self.modal = Some(Modal::Form {
                        action,
                        fields,
                        active,
                    });
                    Intent::None
                }
                KeyCode::Char(character)
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    if let Some(field) = fields.get_mut(active) {
                        field.value.push(character);
                    }
                    self.modal = Some(Modal::Form {
                        action,
                        fields,
                        active,
                    });
                    Intent::None
                }
                KeyCode::Enter => {
                    let values = fields.into_iter().map(|field| field.value).collect();
                    Intent::SubmitForm { action, values }
                }
                _ => Intent::None,
            },
            Modal::Confirm { action, .. } => match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                    self.modal = None;
                    Intent::ConfirmAction(action)
                }
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                    self.modal = None;
                    Intent::None
                }
                _ => Intent::None,
            },
        }
    }

    pub fn open_form(&mut self, action: Action, fields: Vec<FormField>) {
        self.modal = Some(Modal::Form {
            action,
            fields,
            active: 0,
        });
    }

    pub fn open_confirmation(&mut self, action: Action, summary: Vec<String>) {
        self.modal = Some(Modal::Confirm { action, summary });
    }

    pub fn action_availability(&self, action: Action) -> ActionAvailability {
        let disabled = |reason: &str| ActionAvailability {
            action,
            enabled: false,
            reason: Some(reason.to_owned()),
        };
        if action == Action::CopyAgentPrompt {
            return ActionAvailability {
                action,
                enabled: true,
                reason: None,
            };
        }
        if matches!(
            self.selected_row(),
            Some(VisibleRow::VirtualRepository { .. } | VisibleRow::VirtualPullRequest { .. })
        ) {
            return disabled("virtual pull requests support only Enter to create a worktree");
        }
        let Some((repository, _)) = self.selected_repository() else {
            return disabled("select a repository or worktree first");
        };
        if repository.stale_error.is_some() {
            return match action {
                Action::EditRepository | Action::RemoveRepository => ActionAvailability {
                    action,
                    enabled: true,
                    reason: None,
                },
                _ => disabled("repository is stale; relink or unregister it first"),
            };
        }
        let enabled = || ActionAvailability {
            action,
            enabled: true,
            reason: None,
        };
        match action {
            Action::RegisterRepository => {
                if repository.session_only {
                    enabled()
                } else {
                    disabled("repository is already registered")
                }
            }
            Action::CopyAgentPrompt => unreachable!("handled before selection validation"),
            Action::EditRepository | Action::RemoveRepository => {
                if repository.session_only {
                    disabled("register this session-only repository first")
                } else {
                    enabled()
                }
            }
            Action::Create | Action::NewWorktree | Action::Prune => enabled(),
            Action::Move | Action::Lock | Action::Unlock | Action::Remove | Action::Repair => {
                let Some((_, worktree, worktree_index)) = self.selected_worktree() else {
                    return disabled("select a linked worktree");
                };
                if worktree.bare {
                    return disabled("the bare anchor is not a checkout");
                }
                if worktree_index == 0
                    && matches!(
                        action,
                        Action::Move | Action::Lock | Action::Unlock | Action::Remove
                    )
                {
                    return disabled("the main worktree cannot use this action");
                }
                match action {
                    Action::Lock if worktree.locked.is_some() => {
                        disabled("worktree is already locked")
                    }
                    Action::Unlock if worktree.locked.is_none() => {
                        disabled("worktree is not locked")
                    }
                    Action::Remove if worktree.locked.is_some() => {
                        disabled("unlock the worktree before removal")
                    }
                    Action::Remove => match self.statuses.get(&worktree.path) {
                        Some(StatusState::Ready(status)) if status.is_dirty() => {
                            disabled("worktree has local changes")
                        }
                        Some(StatusState::Ready(_)) => {
                            if contains_path(&worktree.path, &self.current_directory) {
                                disabled("worktree contains the current directory")
                            } else {
                                enabled()
                            }
                        }
                        Some(StatusState::Error(_)) => disabled("worktree status is unavailable"),
                        _ => disabled("worktree status is still loading"),
                    },
                    _ => enabled(),
                }
            }
        }
    }

    pub fn request_refresh(&mut self) -> Intent {
        if self.pending_status > 0 {
            self.refresh_queued = true;
            self.progress = Some("refresh queued".to_owned());
            Intent::RefreshGitHub
        } else {
            Intent::Refresh
        }
    }

    pub fn begin_status_refresh(&mut self, paths: &[PathBuf]) -> u64 {
        self.generation = self.generation.wrapping_add(1);
        self.pending_status = paths.len();
        self.progress = (!paths.is_empty()).then(|| format!("loading status: 0/{}", paths.len()));
        for path in paths {
            self.statuses.insert(path.clone(), StatusState::Pending);
        }
        self.generation
    }

    pub fn apply_status(&mut self, update: StatusUpdate) -> bool {
        if update.generation != self.generation {
            return false;
        }
        let state = match update.result {
            Ok(status) => StatusState::Ready(status),
            Err(error) => StatusState::Error(error),
        };
        self.statuses.insert(update.path, state);
        self.pending_status = self.pending_status.saturating_sub(1);
        if self.pending_status == 0 {
            self.progress = None;
            return std::mem::take(&mut self.refresh_queued);
        }
        self.progress = Some(format!("loading status: {} remaining", self.pending_status));
        false
    }

    pub fn begin_github_refresh(&mut self, paths: &[PathBuf]) -> u64 {
        self.github_generation = self.github_generation.wrapping_add(1);
        self.github_loading = !paths.is_empty();
        for path in paths {
            let previous = self.github.get(path).and_then(GitHubState::data).cloned();
            self.github
                .insert(path.clone(), GitHubState::Loading { previous });
        }
        self.github_generation
    }

    pub fn apply_github_refresh(
        &mut self,
        generation: u64,
        paths: &[PathBuf],
        mut results: HashMap<PathBuf, Result<GitHubBranchData, GitHubError>>,
    ) -> bool {
        if generation != self.github_generation {
            return false;
        }
        for path in paths {
            let previous = self.github.get(path).and_then(GitHubState::data).cloned();
            let state = match results.remove(path) {
                Some(Ok(data)) => GitHubState::Ready(data),
                Some(Err(error)) => GitHubState::Stale {
                    previous,
                    error: error.to_string(),
                },
                None => GitHubState::Stale {
                    previous,
                    error: "GitHub refresh returned no result".to_owned(),
                },
            };
            self.github.insert(path.clone(), state);
        }
        self.github_loading = false;
        true
    }

    pub fn apply_pull_request_details(
        &mut self,
        generation: u64,
        results: BTreeMap<CanonicalPullRequestId, Result<PullRequestDetails, GitHubError>>,
    ) -> bool {
        if generation != self.github_generation {
            return false;
        }
        for (identity, result) in results {
            match result {
                Ok(details) => {
                    let _ = details.attention_summary();
                    self.pull_request_detail_errors.remove(&identity);
                    self.pull_request_details.insert(identity, details);
                }
                Err(error) => {
                    self.pull_request_detail_errors
                        .insert(identity, error.to_string());
                }
            }
        }
        self.reconcile_detail_selection();
        true
    }

    pub fn replace_repositories(&mut self, repositories: Vec<RepositoryView>) {
        let selected = self.selected.clone();
        let collapsed: HashMap<PathBuf, bool> = self
            .repositories
            .iter()
            .map(|repository| (repository.config.path.clone(), repository.expanded))
            .collect();
        self.repositories = repositories;
        for repository in &mut self.repositories {
            if let Some(expanded) = collapsed.get(&repository.config.path) {
                repository.expanded = *expanded;
            }
        }
        self.selected = selected;
        self.ensure_selection_visible();
    }

    pub fn rebuild_virtual_repositories(&mut self) {
        let previous_rows = self.visible_rows();
        let previous_selected = self.selected.clone();
        let previous_index = previous_selected
            .as_ref()
            .and_then(|selected| previous_rows.iter().position(|row| row.id() == selected));
        let previous_expansion: HashMap<GitHubRepositoryIdentity, bool> = self
            .virtual_repositories
            .iter()
            .map(|repository| (repository.identity.clone(), repository.expanded))
            .collect();
        let mappings: HashMap<CanonicalPullRequestId, Option<usize>> = self
            .authored_mappings
            .iter()
            .map(|mapping| (mapping.identity.clone(), mapping.repository_index))
            .collect();
        let mut grouped: BTreeMap<GitHubRepositoryIdentity, VirtualRepositoryView> =
            BTreeMap::new();
        for pull_request in self.authored_pull_requests.visible() {
            let Some(repository_index) = mappings.get(&pull_request.identity) else {
                continue;
            };
            let mapped_repository = repository_index
                .and_then(|index| self.repositories.get(index))
                .map(|repository| repository.config.path.clone());
            let identity = pull_request.identity.repository.clone();
            grouped
                .entry(identity.clone())
                .or_insert_with(|| VirtualRepositoryView {
                    identity: identity.clone(),
                    mapped_repository,
                    expanded: previous_expansion.get(&identity).copied().unwrap_or(true),
                    pull_requests: Vec::new(),
                })
                .pull_requests
                .push(pull_request);
        }
        let catalog_order: HashMap<PathBuf, usize> = self
            .repositories
            .iter()
            .enumerate()
            .map(|(index, repository)| (repository.config.path.clone(), index))
            .collect();
        self.virtual_repositories = grouped.into_values().collect();
        self.virtual_repositories.sort_by(|left, right| {
            let left_order = left
                .mapped_repository
                .as_ref()
                .and_then(|path| catalog_order.get(path))
                .copied();
            let right_order = right
                .mapped_repository
                .as_ref()
                .and_then(|path| catalog_order.get(path))
                .copied();
            left_order
                .is_none()
                .cmp(&right_order.is_none())
                .then_with(|| left_order.cmp(&right_order))
                .then_with(|| left.identity.cmp(&right.identity))
        });
        for repository in &mut self.virtual_repositories {
            repository.pull_requests.sort_by(|left, right| {
                right
                    .pull_request
                    .updated_at
                    .cmp(&left.pull_request.updated_at)
                    .then_with(|| left.identity.number.cmp(&right.identity.number))
            });
        }

        self.selected = previous_selected;
        let current_rows = self.visible_rows();
        let selected_exists = self
            .selected
            .as_ref()
            .is_some_and(|selected| current_rows.iter().any(|row| row.id() == selected));
        if !selected_exists {
            self.selected = match self.selected.as_ref() {
                Some(RowId::VirtualPullRequest(identity)) => {
                    let repository_id = RowId::VirtualRepository(identity.repository.clone());
                    current_rows
                        .iter()
                        .find(|row| row.id() == &repository_id)
                        .map(|row| row.id().clone())
                }
                _ => None,
            }
            .or_else(|| {
                previous_index
                    .and_then(|index| {
                        current_rows.get(index.min(current_rows.len().saturating_sub(1)))
                    })
                    .map(|row| row.id().clone())
            })
            .or_else(|| current_rows.first().map(|row| row.id().clone()));
            self.detail_scroll = 0;
            self.detail_selected = None;
        }
        self.ensure_selected_in_view();
    }

    pub fn set_viewport_height(&mut self, height: usize) {
        self.viewport_height = height.max(1);
        if self.viewport_initialized {
            self.ensure_selected_in_view();
        } else {
            self.viewport_initialized = true;
            self.ensure_initial_selection_in_view();
        }
    }

    pub fn set_detail_max_scroll(&mut self, max_scroll: usize) {
        self.detail_max_scroll = max_scroll;
        self.detail_scroll = self.detail_scroll.min(max_scroll);
    }

    pub fn set_detail_viewport_height(&mut self, height: usize) {
        self.detail_viewport_height = height.max(1);
        self.reconcile_detail_selection();
    }

    pub fn set_detail_scroll(&mut self, scroll: usize) {
        self.detail_scroll = scroll;
    }

    fn reconcile_detail_selection(&mut self) {
        let rows = self.detail_rows();
        if rows.is_empty() {
            self.detail_selected = None;
            return;
        }
        let previous_index = self
            .detail_selected
            .as_ref()
            .and_then(|selected| rows.iter().position(|row| &row.id == selected));
        if previous_index.is_some() {
            return;
        }
        let fallback = self.detail_selected.as_ref().map_or(0, |selected| {
            rows.iter()
                .position(|row| row.id > *selected)
                .unwrap_or_else(|| rows.len().saturating_sub(1))
        });
        self.detail_selected = rows.get(fallback).map(|row| row.id.clone());
        self.detail_scroll = self.detail_scroll.min(fallback);
    }

    fn move_detail_selection(&mut self, delta: isize) {
        let rows = self.detail_rows();
        if rows.is_empty() {
            return;
        }
        let current = self
            .detail_selected
            .as_ref()
            .and_then(|selected| rows.iter().position(|row| &row.id == selected))
            .unwrap_or(0);
        let next = (current as isize + delta).clamp(0, rows.len() as isize - 1) as usize;
        self.detail_selected = Some(rows[next].id.clone());
    }

    fn select_detail_index(&mut self, index: usize) {
        if let Some(row) = self.detail_rows().get(index) {
            self.detail_selected = Some(row.id.clone());
        }
    }

    fn open_selected_detail(&mut self) -> Intent {
        let rows = self.detail_rows();
        if rows.is_empty() {
            return Intent::None;
        }
        let row = self
            .detail_selected
            .as_ref()
            .and_then(|selected| rows.iter().find(|row| &row.id == selected))
            .or_else(|| rows.first());
        row.map(|row| Intent::OpenUrl(row.url.clone()))
            .unwrap_or(Intent::None)
    }

    pub fn agent_prompt(&self) -> Option<String> {
        let all = self.prompt_pull_requests();
        let selected_detail = (self.pane == Pane::Detail)
            .then_some(self.detail_selected.as_ref())
            .flatten();
        let scoped = match selected_detail {
            Some(DetailRowId::Check(identity, name)) => all
                .get(identity)
                .map(|pull_request| PromptPullRequest {
                    identity: identity.clone(),
                    pull_request: pull_request.pull_request.clone(),
                    checks: pull_request
                        .checks
                        .iter()
                        .filter(|check| check.name == *name && check.state.is_actionable())
                        .cloned()
                        .collect(),
                    feedback: Vec::new(),
                })
                .into_iter()
                .collect(),
            Some(DetailRowId::Feedback(identity, id)) => all
                .get(identity)
                .map(|pull_request| PromptPullRequest {
                    identity: identity.clone(),
                    pull_request: pull_request.pull_request.clone(),
                    checks: Vec::new(),
                    feedback: pull_request
                        .feedback
                        .iter()
                        .filter(|feedback| feedback.id == *id)
                        .cloned()
                        .collect(),
                })
                .into_iter()
                .collect(),
            Some(DetailRowId::Section(identity, DetailSection::Checks)) => all
                .get(identity)
                .map(|pull_request| PromptPullRequest {
                    identity: identity.clone(),
                    pull_request: pull_request.pull_request.clone(),
                    checks: pull_request.checks.clone(),
                    feedback: Vec::new(),
                })
                .into_iter()
                .collect(),
            Some(DetailRowId::Section(identity, DetailSection::Feedback)) => all
                .get(identity)
                .map(|pull_request| PromptPullRequest {
                    identity: identity.clone(),
                    pull_request: pull_request.pull_request.clone(),
                    checks: Vec::new(),
                    feedback: pull_request.feedback.clone(),
                })
                .into_iter()
                .collect(),
            Some(DetailRowId::Summary(identity))
            | Some(DetailRowId::Section(identity, DetailSection::Attention))
            | Some(DetailRowId::Section(identity, DetailSection::Reviews))
            | Some(DetailRowId::ReviewRequest(identity, _))
            | Some(DetailRowId::Review(identity, _)) => self.prompt_stack(&all, identity),
            None => self.prompt_scope_from_tree(&all),
        };
        format_agent_prompt(&scoped)
    }

    fn prompt_pull_requests(&self) -> BTreeMap<CanonicalPullRequestId, PromptPullRequest> {
        let mut pull_requests = BTreeMap::new();
        for repository in &self.virtual_repositories {
            for authored in &repository.pull_requests {
                pull_requests.insert(
                    authored.identity.clone(),
                    self.prompt_pull_request(&authored.identity, &authored.pull_request),
                );
            }
        }
        for repository in &self.repositories {
            for worktree in &repository.worktrees {
                let Some(pull_request) = self
                    .github
                    .get(&worktree.path)
                    .and_then(GitHubState::data)
                    .and_then(|data| data.pull_request.as_ref())
                else {
                    continue;
                };
                let Some(identity) = self.pull_request_identity(repository, pull_request) else {
                    continue;
                };
                pull_requests.insert(
                    identity.clone(),
                    self.prompt_pull_request(&identity, pull_request),
                );
            }
        }
        pull_requests
    }

    fn prompt_pull_request(
        &self,
        identity: &CanonicalPullRequestId,
        pull_request: &PullRequest,
    ) -> PromptPullRequest {
        let details = self.pull_request_details.get(identity);
        PromptPullRequest {
            identity: identity.clone(),
            pull_request: pull_request.clone(),
            checks: details
                .into_iter()
                .flat_map(|details| details.checks.iter())
                .filter(|check| check.state.is_actionable())
                .cloned()
                .collect(),
            feedback: details
                .into_iter()
                .flat_map(|details| details.feedback.iter())
                .cloned()
                .collect(),
        }
    }

    fn prompt_stack(
        &self,
        all: &BTreeMap<CanonicalPullRequestId, PromptPullRequest>,
        root: &CanonicalPullRequestId,
    ) -> Vec<PromptPullRequest> {
        let Some(root_pull_request) = all.get(root) else {
            return Vec::new();
        };
        let mut included = BTreeSet::from([root.clone()]);
        let mut ordered = vec![root_pull_request.clone()];
        loop {
            let mut added = false;
            for (identity, candidate) in all {
                if identity.repository != root.repository || included.contains(identity) {
                    continue;
                }
                if ordered.iter().any(|parent| {
                    pull_request_identity_matches(
                        &parent.pull_request.head,
                        &candidate.pull_request.base,
                    )
                }) {
                    included.insert(identity.clone());
                    ordered.push(candidate.clone());
                    added = true;
                }
            }
            if !added {
                break;
            }
        }
        ordered
    }

    fn prompt_scope_from_tree(
        &self,
        all: &BTreeMap<CanonicalPullRequestId, PromptPullRequest>,
    ) -> Vec<PromptPullRequest> {
        match self.selected.as_ref() {
            Some(RowId::VirtualPullRequest(identity)) => self.prompt_stack(all, identity),
            Some(RowId::Worktree(path)) => self
                .repositories
                .iter()
                .find(|repository| {
                    repository
                        .worktrees
                        .iter()
                        .any(|worktree| worktree.path == *path)
                })
                .and_then(|repository| {
                    let pull_request = self
                        .github
                        .get(path)
                        .and_then(GitHubState::data)
                        .and_then(|data| data.pull_request.as_ref())?;
                    self.pull_request_identity(repository, pull_request)
                })
                .map(|identity| self.prompt_stack(all, &identity))
                .unwrap_or_default(),
            Some(RowId::Repository(path)) => {
                let identities: BTreeSet<_> = self
                    .repositories
                    .iter()
                    .find(|repository| repository.config.path == *path)
                    .into_iter()
                    .flat_map(|repository| repository.config.github_remotes.values())
                    .cloned()
                    .collect();
                all.values()
                    .filter(|pull_request| identities.contains(&pull_request.identity.repository))
                    .cloned()
                    .collect()
            }
            Some(RowId::VirtualRepository(repository)) => all
                .values()
                .filter(|pull_request| pull_request.identity.repository == *repository)
                .cloned()
                .collect(),
            None => Vec::new(),
        }
    }

    fn direct_action(&mut self, character: char) -> Intent {
        let action = match character {
            'C' => Action::CopyAgentPrompt,
            'c' => Action::Create,
            'n' => Action::NewWorktree,
            'm' => Action::Move,
            'L' => Action::Lock,
            'U' => Action::Unlock,
            'd' => Action::Remove,
            'R' => Action::Repair,
            'p' => Action::Prune,
            'a' => Action::RegisterRepository,
            'e' => Action::EditRepository,
            'x' => Action::RemoveRepository,
            _ => return Intent::None,
        };
        let availability = self.action_availability(action);
        if availability.enabled {
            Intent::BeginAction(action)
        } else {
            self.inline_error = availability.reason;
            Intent::None
        }
    }

    fn accept_or_toggle(&mut self) -> Intent {
        match self.selected_row() {
            Some(VisibleRow::Repository {
                repository_index, ..
            }) => {
                self.repositories[repository_index].expanded =
                    !self.repositories[repository_index].expanded;
                Intent::None
            }
            Some(VisibleRow::Worktree {
                repository_index,
                worktree_index,
                ..
            }) => {
                let worktree = &self.repositories[repository_index].worktrees[worktree_index];
                if worktree.navigable() && worktree.path.exists() {
                    Intent::Accept(worktree.path.clone())
                } else {
                    self.inline_error = Some("this row is not a navigable checkout".to_owned());
                    Intent::None
                }
            }
            Some(VisibleRow::VirtualRepository {
                virtual_repository_index,
                ..
            }) => {
                self.virtual_repositories[virtual_repository_index].expanded =
                    !self.virtual_repositories[virtual_repository_index].expanded;
                Intent::None
            }
            Some(VisibleRow::VirtualPullRequest {
                virtual_repository_index,
                pull_request_index,
                ..
            }) => Intent::MaterializePullRequest(
                self.virtual_repositories[virtual_repository_index].pull_requests
                    [pull_request_index]
                    .identity
                    .clone(),
            ),
            None => Intent::None,
        }
    }

    fn collapse_or_focus_list(&mut self) {
        if self.pane == Pane::Detail {
            self.pane = Pane::List;
            return;
        }
        if let Some((_, repository_index)) = self.selected_repository() {
            self.repositories[repository_index].expanded = false;
            self.selected = Some(self.repositories[repository_index].id());
            self.detail_scroll = 0;
            self.detail_selected = None;
            self.ensure_selected_in_view();
        } else if let Some(row) = self.selected_row() {
            match row {
                VisibleRow::VirtualPullRequest {
                    mapped_repository_index: Some(repository_index),
                    ..
                } => {
                    self.repositories[repository_index].expanded = false;
                    self.selected = Some(self.repositories[repository_index].id());
                }
                VisibleRow::VirtualRepository {
                    virtual_repository_index,
                    ..
                }
                | VisibleRow::VirtualPullRequest {
                    virtual_repository_index,
                    ..
                } => {
                    self.virtual_repositories[virtual_repository_index].expanded = false;
                    self.selected = Some(self.virtual_repositories[virtual_repository_index].id());
                }
                VisibleRow::Repository { .. } | VisibleRow::Worktree { .. } => return,
            }
            self.detail_scroll = 0;
            self.detail_selected = None;
            self.ensure_selected_in_view();
        }
    }

    fn expand_or_focus_detail(&mut self) {
        if matches!(self.selected_row(), Some(VisibleRow::Repository { .. })) {
            if let Some((_, index)) = self.selected_repository() {
                self.repositories[index].expanded = true;
            }
        } else if let Some(VisibleRow::VirtualRepository {
            virtual_repository_index,
            ..
        }) = self.selected_row()
        {
            self.virtual_repositories[virtual_repository_index].expanded = true;
        } else {
            self.pane = Pane::Detail;
            self.reconcile_detail_selection();
        }
    }

    fn move_and_continue(&mut self, delta: isize) -> Intent {
        self.move_selection(delta);
        Intent::None
    }

    fn move_selection(&mut self, delta: isize) {
        let rows = self.visible_rows();
        if rows.is_empty() {
            self.selected = None;
            return;
        }
        let current = self
            .selected
            .as_ref()
            .and_then(|selected| rows.iter().position(|row| row.id() == selected))
            .unwrap_or(0);
        let next = (current as isize + delta).clamp(0, rows.len() as isize - 1) as usize;
        let next_id = rows[next].id().clone();
        if self.selected.as_ref() != Some(&next_id) {
            self.detail_scroll = 0;
            self.detail_selected = None;
        }
        self.selected = Some(next_id);
        self.ensure_selected_in_view();
    }

    fn scroll_detail(&mut self, delta: isize) {
        self.detail_scroll = (self.detail_scroll as isize + delta)
            .clamp(0, self.detail_max_scroll as isize) as usize;
    }

    fn select_index(&mut self, index: usize) {
        if let Some(row) = self.visible_rows().get(index) {
            let id = row.id().clone();
            if self.selected.as_ref() != Some(&id) {
                self.detail_scroll = 0;
                self.detail_selected = None;
            }
            self.selected = Some(id);
            self.ensure_selected_in_view();
        }
    }

    fn select_initial(&mut self) {
        for repository in &self.repositories {
            for worktree in &repository.worktrees {
                if worktree.navigable() && contains_path(&worktree.path, &self.current_directory) {
                    self.selected = Some(RowId::Worktree(worktree.path.clone()));
                    return;
                }
            }
        }
        self.selected = self
            .repositories
            .iter()
            .flat_map(|repository| repository.worktrees.iter())
            .find(|worktree| worktree.navigable())
            .map(|worktree| RowId::Worktree(worktree.path.clone()))
            .or_else(|| self.visible_rows().first().map(|row| row.id().clone()));
    }

    fn ensure_selection_visible(&mut self) {
        let previous = self.selected.clone();
        let rows = self.visible_rows();
        let visible = self
            .selected
            .as_ref()
            .is_some_and(|selected| rows.iter().any(|row| row.id() == selected));
        if !visible {
            self.selected = rows
                .iter()
                .find(|row| matches!(row, VisibleRow::Worktree { .. }))
                .or_else(|| rows.first())
                .map(|row| row.id().clone());
        }
        if self.selected != previous {
            self.detail_scroll = 0;
            self.detail_selected = None;
        }
        self.ensure_selected_in_view();
    }

    fn ensure_selected_in_view(&mut self) {
        let rows = self.visible_rows();
        let Some(index) = self
            .selected
            .as_ref()
            .and_then(|selected| rows.iter().position(|row| row.id() == selected))
        else {
            self.scroll = 0;
            return;
        };
        if !self.viewport_initialized {
            return;
        }
        let margin = LIST_SCROLL_MARGIN.min(self.viewport_height.saturating_sub(1) / 2);
        if index < self.scroll.saturating_add(margin) {
            self.scroll = index.saturating_sub(margin);
        } else if index >= self.scroll + self.viewport_height.saturating_sub(margin) {
            self.scroll = index + margin + 1 - self.viewport_height;
        }
        self.clamp_list_scroll(rows.len());
    }

    fn ensure_initial_selection_in_view(&mut self) {
        let rows = self.visible_rows();
        let Some(index) = self
            .selected
            .as_ref()
            .and_then(|selected| rows.iter().position(|row| row.id() == selected))
        else {
            self.scroll = 0;
            return;
        };
        self.scroll = 0;
        if index >= self.viewport_height {
            let margin = LIST_SCROLL_MARGIN.min(self.viewport_height.saturating_sub(1) / 2);
            self.scroll = index + margin + 1 - self.viewport_height;
        }
        self.clamp_list_scroll(rows.len());
    }

    fn clamp_list_scroll(&mut self, row_count: usize) {
        self.scroll = self
            .scroll
            .min(row_count.saturating_sub(self.viewport_height));
    }
}

fn virtual_repository_matches(repository: &VirtualRepositoryView, filter: &str) -> bool {
    repository
        .identity
        .full_name()
        .to_ascii_lowercase()
        .contains(filter)
        || repository
            .identity
            .host
            .to_ascii_lowercase()
            .contains(filter)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct NestedWorktree {
    index: usize,
    depth: usize,
}

fn nested_worktrees(
    worktrees: &[Worktree],
    branch_parents: &HashMap<PathBuf, PathBuf>,
    included: &BTreeSet<usize>,
) -> Vec<NestedWorktree> {
    let indexes = worktrees
        .iter()
        .enumerate()
        .map(|(index, worktree)| (worktree.path.clone(), index))
        .collect::<HashMap<_, _>>();
    let mut parents = vec![None; worktrees.len()];
    for index in included {
        parents[*index] = branch_parents
            .get(&worktrees[*index].path)
            .and_then(|path| indexes.get(path))
            .copied()
            .filter(|parent| included.contains(parent) && parent != index);
    }
    let mut cyclic = BTreeSet::new();
    for start in included {
        let mut path = Vec::new();
        let mut current = Some(*start);
        while let Some(index) = current {
            if let Some(cycle_start) = path.iter().position(|candidate| *candidate == index) {
                cyclic.extend(path[cycle_start..].iter().copied());
                break;
            }
            path.push(index);
            current = parents[index];
        }
    }
    for index in cyclic {
        parents[index] = None;
    }
    let mut children = vec![Vec::new(); worktrees.len()];
    for index in included {
        if let Some(parent) = parents[*index] {
            children[parent].push(*index);
        }
    }
    let mut result = Vec::with_capacity(included.len());
    let mut visited = vec![false; worktrees.len()];
    for root in included
        .iter()
        .copied()
        .filter(|index| parents[*index].is_none())
    {
        append_worktree_subtree(root, 0, &children, &mut visited, &mut result);
    }
    result
}

fn append_worktree_subtree(
    index: usize,
    depth: usize,
    children: &[Vec<usize>],
    visited: &mut [bool],
    result: &mut Vec<NestedWorktree>,
) {
    if std::mem::replace(&mut visited[index], true) {
        return;
    }
    result.push(NestedWorktree { index, depth });
    for child in &children[index] {
        append_worktree_subtree(*child, depth + 1, children, visited, result);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct NestedPullRequest {
    index: usize,
    depth: usize,
    parent: Option<usize>,
}

fn nested_pull_requests(pull_requests: &[AuthoredPullRequest]) -> Vec<NestedPullRequest> {
    let mut parents = vec![None; pull_requests.len()];
    for (child_index, child) in pull_requests.iter().enumerate() {
        let candidates: Vec<usize> = pull_requests
            .iter()
            .enumerate()
            .filter(|(parent_index, parent)| {
                *parent_index != child_index
                    && pull_request_identity_matches(
                        &parent.pull_request.head,
                        &child.pull_request.base,
                    )
            })
            .map(|(index, _)| index)
            .collect();
        if let [parent] = candidates.as_slice() {
            parents[child_index] = Some(*parent);
        }
    }

    let mut cyclic = BTreeSet::new();
    for start in 0..parents.len() {
        let mut path = Vec::new();
        let mut current = Some(start);
        while let Some(index) = current {
            if let Some(cycle_start) = path.iter().position(|candidate| *candidate == index) {
                cyclic.extend(path[cycle_start..].iter().copied());
                break;
            }
            path.push(index);
            current = parents[index];
        }
    }
    for index in cyclic {
        parents[index] = None;
    }

    let mut children = vec![Vec::new(); pull_requests.len()];
    for (child, parent) in parents.iter().copied().enumerate() {
        if let Some(parent) = parent {
            children[parent].push(child);
        }
    }
    let mut result = Vec::with_capacity(pull_requests.len());
    let mut visited = vec![false; pull_requests.len()];
    for root in (0..pull_requests.len()).filter(|index| parents[*index].is_none()) {
        append_pull_request_subtree(root, 0, &parents, &children, &mut visited, &mut result);
    }
    result
}

fn append_pull_request_subtree(
    index: usize,
    depth: usize,
    parents: &[Option<usize>],
    children: &[Vec<usize>],
    visited: &mut [bool],
    result: &mut Vec<NestedPullRequest>,
) {
    if std::mem::replace(&mut visited[index], true) {
        return;
    }
    result.push(NestedPullRequest {
        index,
        depth,
        parent: parents[index],
    });
    for child in &children[index] {
        append_pull_request_subtree(*child, depth + 1, parents, children, visited, result);
    }
}

fn pull_request_identity_matches(
    head: &crate::model::PullRequestIdentity,
    base: &crate::model::PullRequestIdentity,
) -> bool {
    head.branch == base.branch
        && head
            .repository
            .as_deref()
            .zip(base.repository.as_deref())
            .is_some_and(|(head, base)| head.eq_ignore_ascii_case(base))
}

fn github_search_text(data: &GitHubBranchData) -> String {
    let mut parts = data.warnings.clone();
    if let Some(pull_request) = &data.pull_request {
        parts.extend([
            format!("#{}", pull_request.number),
            pull_request.title.clone(),
            pull_request.url.clone(),
            pull_request.state.to_string(),
            pull_request.base.branch.clone(),
            pull_request.head.branch.clone(),
            pull_request.review_decision.clone().unwrap_or_default(),
            pull_request.checks.to_string(),
            if pull_request.auto_merge {
                "auto-merge".to_owned()
            } else {
                String::new()
            },
        ]);
    }
    parts.join(" ")
}

fn virtual_pull_request_matches(pull_request: &AuthoredPullRequest, filter: &str) -> bool {
    let pull_request_data = &pull_request.pull_request;
    pull_request
        .identity
        .repository
        .full_name()
        .contains(filter)
        || pull_request_data
            .head
            .branch
            .to_ascii_lowercase()
            .contains(filter)
        || pull_request_data
            .title
            .to_ascii_lowercase()
            .contains(filter)
        || pull_request.identity.number.to_string().contains(filter)
        || format!("#{}", pull_request.identity.number).contains(filter)
        || pull_request.author.to_ascii_lowercase().contains(filter)
        || (pull_request_data.auto_merge && "auto-merge".contains(filter))
}

fn pull_request_details_search_text(details: &PullRequestDetails) -> String {
    let mut parts = details.warnings.clone();
    parts.extend(details.checks.iter().flat_map(|check| {
        [
            check.name.clone(),
            format!("{:?}", check.state),
            check.target_url.clone().unwrap_or_default(),
            if check.required {
                "required".to_owned()
            } else {
                "optional".to_owned()
            },
        ]
    }));
    parts.extend(details.review_requests.iter().flat_map(|request| {
        [
            request.id.clone(),
            request.name.clone(),
            format!("{:?}", request.kind),
        ]
    }));
    parts.extend(details.reviewer_reviews.iter().flat_map(|review| {
        [
            review.id.clone(),
            review
                .database_id
                .map(|id| id.to_string())
                .unwrap_or_default(),
            review.reviewer.clone(),
            format!("{:?}", review.state),
        ]
    }));
    parts.extend(details.feedback.iter().flat_map(|feedback| {
        [
            feedback.id.clone(),
            feedback
                .database_id
                .map(|id| id.to_string())
                .unwrap_or_default(),
            feedback.author.clone(),
            feedback.body.clone(),
            feedback.path.clone().unwrap_or_default(),
            feedback.permalink.clone().unwrap_or_default(),
            format!("{:?}", feedback.kind),
            if feedback.outdated {
                "outdated".to_owned()
            } else {
                String::new()
            },
        ]
    }));
    parts.push(format!("{:?}", details.merge_conflict));
    parts.join(" ")
}

fn check_attention_rank(state: crate::model::CheckState) -> u8 {
    match state {
        crate::model::CheckState::Failure | crate::model::CheckState::Error => 0,
        crate::model::CheckState::Pending | crate::model::CheckState::Expected => 1,
        crate::model::CheckState::Unknown => 2,
        crate::model::CheckState::Success
        | crate::model::CheckState::Neutral
        | crate::model::CheckState::Skipped => 3,
    }
}

fn debug_label(value: impl std::fmt::Debug) -> String {
    let raw = format!("{value:?}");
    let mut label = String::new();
    for (index, character) in raw.chars().enumerate() {
        if index > 0 && character.is_ascii_uppercase() {
            label.push(' ');
        }
        label.push(character.to_ascii_lowercase());
    }
    label
}

fn contains_path(worktree: &Path, candidate: &Path) -> bool {
    let worktree = std::fs::canonicalize(worktree).unwrap_or_else(|_| worktree.to_owned());
    let candidate = std::fs::canonicalize(candidate).unwrap_or_else(|_| candidate.to_owned());
    candidate.starts_with(worktree)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn repository(path: &str, expanded: bool) -> RepositoryView {
        RepositoryView {
            config: RepositoryConfig {
                path: PathBuf::from(path),
                label: Some(path.trim_start_matches('/').to_owned()),
                worktree_root: None,
                github_remote: None,
                github_remotes: Default::default(),
                github_preferred_remote: None,
            },
            session_only: false,
            stale_error: None,
            expanded,
            worktrees: vec![
                worktree(path, "main", false),
                worktree(&format!("{path}-topic"), "topic", false),
            ],
        }
    }

    fn worktree(path: &str, branch: &str, bare: bool) -> Worktree {
        Worktree {
            path: PathBuf::from(path),
            head: Some("1234567890".to_owned()),
            branch: Some(format!("refs/heads/{branch}")),
            detached: false,
            bare,
            locked: None,
            prunable: None,
        }
    }

    fn authored(
        owner: &str,
        repository: &str,
        number: u64,
        updated_at: &str,
    ) -> AuthoredPullRequest {
        let repository_identity =
            GitHubRepositoryIdentity::canonical("github.com", owner, repository);
        AuthoredPullRequest {
            identity: CanonicalPullRequestId {
                repository: repository_identity,
                number,
            },
            author: "viewer".to_owned(),
            pull_request: crate::model::PullRequest {
                number,
                title: format!("feature {number}"),
                url: format!("https://github.com/{owner}/{repository}/pull/{number}"),
                state: crate::model::PullRequestState::Open,
                updated_at: updated_at.to_owned(),
                review_decision: Some("APPROVED".to_owned()),
                auto_merge: false,
                base: crate::model::PullRequestIdentity {
                    repository: Some(format!("{owner}/{repository}")),
                    branch: "main".to_owned(),
                    oid: Some("base".to_owned()),
                },
                head: crate::model::PullRequestIdentity {
                    repository: Some("viewer/fork".to_owned()),
                    branch: format!("topic-{number}"),
                    oid: Some(format!("head-{number}")),
                },
                checks: crate::model::CheckRollup::Success,
            },
        }
    }

    fn replace_authored(
        app: &mut App,
        pull_requests: Vec<AuthoredPullRequest>,
        mappings: Vec<(CanonicalPullRequestId, Option<usize>)>,
    ) {
        let generation = app.authored_pull_requests.begin();
        app.authored_pull_requests.apply_page(
            generation,
            "github.com".to_owned(),
            1,
            pull_requests,
            Vec::new(),
        );
        app.authored_pull_requests
            .finish(generation, true, Vec::new(), None);
        app.authored_mappings = mappings
            .into_iter()
            .map(|(identity, repository_index)| PullRequestMapping {
                identity,
                repository_index,
            })
            .collect();
        app.rebuild_virtual_repositories();
    }

    #[test]
    fn virtual_repositories_follow_catalog_order_then_unmapped_and_sort_prs_newest() {
        let mut app = App::new(
            vec![repository("/first", true), repository("/second", true)],
            PathBuf::from("/elsewhere"),
        );
        let older = authored("team", "mapped", 1, "2026-01-01T00:00:00Z");
        let newer = authored("team", "mapped", 2, "2026-02-01T00:00:00Z");
        let first = authored("alpha", "first", 3, "2026-01-01T00:00:00Z");
        let unmapped = authored("aardvark", "unmapped", 4, "2026-01-01T00:00:00Z");
        replace_authored(
            &mut app,
            vec![
                older.clone(),
                unmapped.clone(),
                newer.clone(),
                first.clone(),
            ],
            vec![
                (older.identity.clone(), Some(1)),
                (newer.identity.clone(), Some(1)),
                (first.identity.clone(), Some(0)),
                (unmapped.identity.clone(), None),
            ],
        );
        assert_eq!(
            app.virtual_repositories
                .iter()
                .map(|repository| repository.identity.full_name())
                .collect::<Vec<_>>(),
            vec!["alpha/first", "team/mapped", "aardvark/unmapped"]
        );
        assert_eq!(
            app.virtual_repositories[1]
                .pull_requests
                .iter()
                .map(|pull_request| pull_request.identity.number)
                .collect::<Vec<_>>(),
            vec![2, 1]
        );
        assert!(app.virtual_repositories[2].mapped_repository.is_none());

        app.filter = "topic-2".to_owned();
        let rows = app.visible_rows();
        assert!(
            rows.iter()
                .any(|row| { row.id() == &RowId::VirtualPullRequest(newer.identity.clone()) })
        );
        app.filter = "#4".to_owned();
        assert!(
            app.visible_rows()
                .iter()
                .any(|row| { row.id() == &RowId::VirtualPullRequest(unmapped.identity.clone()) })
        );
        app.filter = "viewer".to_owned();
        assert_eq!(app.visible_rows().len(), 7);
    }

    #[test]
    fn stacked_pull_requests_render_parent_first_with_depth_and_filter_ancestors() {
        let mut app = App::new(vec![repository("/repo", true)], PathBuf::from("/elsewhere"));
        let mut parent = authored("team", "project", 10, "2026-01-01T00:00:00Z");
        parent.pull_request.head.repository = Some("team/project".to_owned());
        parent.pull_request.head.branch = "stack-parent".to_owned();
        let mut child = authored("team", "project", 11, "2026-02-01T00:00:00Z");
        child.pull_request.base.branch = "stack-parent".to_owned();
        child.pull_request.head.repository = Some("team/project".to_owned());
        child.pull_request.head.branch = "stack-child".to_owned();
        let mut grandchild = authored("team", "project", 12, "2026-03-01T00:00:00Z");
        grandchild.pull_request.base.branch = "stack-child".to_owned();
        grandchild.pull_request.head.repository = Some("team/project".to_owned());
        grandchild.pull_request.head.branch = "stack-grandchild".to_owned();
        let independent = authored("team", "project", 13, "2026-04-01T00:00:00Z");
        let pull_requests = vec![
            parent.clone(),
            child.clone(),
            grandchild.clone(),
            independent.clone(),
        ];
        replace_authored(
            &mut app,
            pull_requests.clone(),
            pull_requests
                .iter()
                .map(|pull_request| (pull_request.identity.clone(), Some(0)))
                .collect(),
        );

        let rows = app.visible_rows();
        let nested = rows
            .iter()
            .filter_map(|row| match row {
                VisibleRow::VirtualPullRequest {
                    pull_request_index,
                    stack_depth,
                    ..
                } => Some((
                    app.virtual_repositories[0].pull_requests[*pull_request_index]
                        .identity
                        .number,
                    *stack_depth,
                )),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(nested, vec![(13, 0), (10, 0), (11, 1), (12, 2)]);

        app.filter = "stack-grandchild".to_owned();
        let filtered = app
            .visible_rows()
            .iter()
            .filter_map(|row| match row {
                VisibleRow::VirtualPullRequest {
                    pull_request_index,
                    stack_depth,
                    ..
                } => Some((
                    app.virtual_repositories[0].pull_requests[*pull_request_index]
                        .identity
                        .number,
                    *stack_depth,
                )),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(filtered, vec![(10, 0), (11, 1), (12, 2)]);
    }

    #[test]
    fn local_branch_ancestry_nests_each_worktree_once() {
        let mut repository = repository("/repo", true);
        repository
            .worktrees
            .push(worktree("/repo-child", "child", false));
        let mut app = App::new(vec![repository], PathBuf::from("/elsewhere"));
        app.branch_parents
            .insert(PathBuf::from("/repo-topic"), PathBuf::from("/repo"));
        app.branch_parents
            .insert(PathBuf::from("/repo-child"), PathBuf::from("/repo-topic"));

        let nested = app
            .visible_rows()
            .into_iter()
            .filter_map(|row| match row {
                VisibleRow::Worktree {
                    worktree_index,
                    stack_depth,
                    ..
                } => Some((worktree_index, stack_depth)),
                _ => None,
            })
            .collect::<Vec<_>>();

        assert_eq!(nested, vec![(0, 0), (1, 1), (2, 2)]);
    }

    #[test]
    fn bare_anchor_is_represented_by_repository_header_not_worktree_row() {
        let mut bare = repository("/repo.git", true);
        bare.worktrees = vec![
            worktree("/repo.git", "main", true),
            worktree("/trees/topic", "topic", false),
        ];
        let mut app = App::new(vec![bare], PathBuf::from("/elsewhere"));

        assert_eq!(
            app.visible_rows()
                .into_iter()
                .map(|row| row.id().clone())
                .collect::<Vec<_>>(),
            vec![
                RowId::Repository(PathBuf::from("/repo.git")),
                RowId::Worktree(PathBuf::from("/trees/topic")),
            ]
        );

        app.filter = "bare".to_owned();
        assert_eq!(app.visible_rows().len(), 2);
    }

    #[test]
    fn virtual_selection_expansion_and_enter_survive_updates_with_safe_fallback() {
        let mut app = App::new(vec![repository("/repo", true)], PathBuf::from("/elsewhere"));
        let selected = authored("team", "project", 10, "2026-01-01T00:00:00Z");
        let other = authored("team", "project", 11, "2026-02-01T00:00:00Z");
        replace_authored(
            &mut app,
            vec![selected.clone(), other.clone()],
            vec![
                (selected.identity.clone(), Some(0)),
                (other.identity.clone(), Some(0)),
            ],
        );
        app.selected = Some(RowId::VirtualPullRequest(selected.identity.clone()));
        assert_eq!(
            app.handle_key(key(KeyCode::Enter)),
            Intent::MaterializePullRequest(selected.identity.clone())
        );
        for action in Action::ALL {
            assert_eq!(
                app.action_availability(action).enabled,
                action == Action::CopyAgentPrompt
            );
        }
        app.handle_key(key(KeyCode::Char('h')));
        assert_eq!(
            app.selected,
            Some(RowId::Repository(PathBuf::from("/repo")))
        );
        assert!(!app.repositories[0].expanded);

        app.repositories[0].expanded = true;
        app.virtual_repositories[0].expanded = true;
        app.selected = Some(RowId::VirtualPullRequest(selected.identity.clone()));
        replace_authored(
            &mut app,
            vec![other.clone(), selected.clone()],
            vec![
                (selected.identity.clone(), Some(0)),
                (other.identity.clone(), Some(0)),
            ],
        );
        assert_eq!(
            app.selected,
            Some(RowId::VirtualPullRequest(selected.identity.clone()))
        );

        replace_authored(
            &mut app,
            vec![other.clone()],
            vec![(other.identity.clone(), Some(0))],
        );
        assert_eq!(
            app.selected,
            Some(RowId::VirtualPullRequest(other.identity.clone()))
        );
    }

    #[test]
    fn singly_mapped_virtual_repository_folds_into_local_repository() {
        let mut app = App::new(vec![repository("/repo", true)], PathBuf::from("/elsewhere"));
        let pull_request = authored("team", "project", 10, "2026-01-01T00:00:00Z");
        replace_authored(
            &mut app,
            vec![pull_request.clone()],
            vec![(pull_request.identity.clone(), Some(0))],
        );

        let rows = app.visible_rows();
        assert!(
            !rows
                .iter()
                .any(|row| matches!(row, VisibleRow::VirtualRepository { .. }))
        );
        assert!(rows.iter().any(|row| matches!(
            row,
            VisibleRow::VirtualPullRequest {
                mapped_repository_index: Some(0),
                ..
            }
        )));

        app.repositories[0].expanded = false;
        app.filter = "auto-merge".to_owned();
        app.virtual_repositories[0].pull_requests[0]
            .pull_request
            .auto_merge = true;
        assert!(
            app.visible_rows().iter().any(|row| {
                row.id() == &RowId::VirtualPullRequest(pull_request.identity.clone())
            })
        );
    }

    #[test]
    fn multiple_github_identities_mapped_to_one_local_repository_keep_headers() {
        let mut app = App::new(vec![repository("/repo", true)], PathBuf::from("/elsewhere"));
        let first = authored("alpha", "project", 10, "2026-01-01T00:00:00Z");
        let second = authored("beta", "project", 11, "2026-01-01T00:00:00Z");
        replace_authored(
            &mut app,
            vec![first.clone(), second.clone()],
            vec![
                (first.identity.clone(), Some(0)),
                (second.identity.clone(), Some(0)),
            ],
        );

        let rows = app.visible_rows();
        assert_eq!(
            rows.iter()
                .filter(|row| matches!(row, VisibleRow::VirtualRepository { .. }))
                .count(),
            2
        );
        assert!(rows.iter().all(|row| !matches!(
            row,
            VisibleRow::VirtualPullRequest {
                mapped_repository_index: Some(_),
                ..
            }
        )));
    }

    #[test]
    fn navigation_filter_collapse_and_panes_are_reducer_driven() {
        let mut app = App::new(vec![repository("/repo", true)], PathBuf::from("/elsewhere"));
        assert_eq!(app.selected, Some(RowId::Worktree(PathBuf::from("/repo"))));
        app.handle_key(key(KeyCode::Char('j')));
        assert_eq!(
            app.selected,
            Some(RowId::Worktree(PathBuf::from("/repo-topic")))
        );
        app.handle_key(key(KeyCode::Char('g')));
        assert!(matches!(app.selected, Some(RowId::Repository(_))));
        app.handle_key(key(KeyCode::Char('h')));
        assert_eq!(app.visible_rows().len(), 1);
        app.handle_key(key(KeyCode::Char('l')));
        assert_eq!(app.visible_rows().len(), 3);
        app.handle_key(key(KeyCode::Char('/')));
        app.handle_key(key(KeyCode::Char('t')));
        app.handle_key(key(KeyCode::Char('o')));
        app.handle_key(key(KeyCode::Char('p')));
        app.handle_key(key(KeyCode::Char('i')));
        app.handle_key(key(KeyCode::Char('c')));
        assert_eq!(app.visible_rows().len(), 2);
        app.handle_key(key(KeyCode::Enter));
        app.handle_key(key(KeyCode::Char('j')));
        app.handle_key(key(KeyCode::Char('l')));
        assert_eq!(app.pane, Pane::Detail);
        app.handle_key(key(KeyCode::Char('h')));
        assert_eq!(app.pane, Pane::List);
    }

    #[test]
    fn paging_jumps_and_resize_keep_selection_visible() {
        let mut repositories = Vec::new();
        for index in 0..6 {
            repositories.push(repository(&format!("/repo-{index}"), true));
        }
        let mut app = App::new(repositories, PathBuf::from("/elsewhere"));
        app.set_viewport_height(3);
        app.handle_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL));
        app.handle_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL));
        assert!(app.scroll > 0);
        app.handle_key(key(KeyCode::Char('G')));
        assert_eq!(
            app.selected,
            Some(RowId::Worktree(PathBuf::from("/repo-5-topic")))
        );
        app.handle_key(key(KeyCode::Char('g')));
        assert!(matches!(app.selected, Some(RowId::Repository(_))));
    }

    #[test]
    fn initial_selection_does_not_scroll_when_already_visible() {
        let repositories = (0..6)
            .map(|index| repository(&format!("/repo-{index}"), true))
            .collect();
        let mut app = App::new(repositories, PathBuf::from("/repo-3"));

        app.set_viewport_height(12);

        assert_eq!(app.scroll, 0);
        app.select_index(11);
        assert_eq!(app.scroll, 5, "navigation keeps a five-row bottom margin");
        app.select_index(6);
        assert_eq!(app.scroll, 1, "navigation keeps a five-row top margin");
    }

    #[test]
    fn initial_selection_scrolls_down_only_when_below_viewport() {
        let repositories = (0..6)
            .map(|index| repository(&format!("/repo-{index}"), true))
            .collect();
        let mut app = App::new(repositories, PathBuf::from("/repo-4"));

        app.set_viewport_height(12);

        assert_eq!(app.scroll, 6);
    }

    #[test]
    fn detail_pane_scrolls_without_moving_the_list_selection() {
        let mut app = App::new(vec![repository("/repo", true)], PathBuf::from("/elsewhere"));
        let selected = app.selected.clone();
        app.set_detail_max_scroll(2);
        app.handle_key(key(KeyCode::Char('l')));

        app.handle_key(key(KeyCode::Char('j')));
        assert_eq!(app.detail_scroll, 1);
        assert_eq!(app.selected, selected);
        app.handle_key(key(KeyCode::Char('j')));
        app.handle_key(key(KeyCode::Char('j')));
        assert_eq!(app.detail_scroll, 2);
        app.handle_key(key(KeyCode::Char('k')));
        assert_eq!(app.detail_scroll, 1);

        app.handle_key(key(KeyCode::Char('h')));
        app.handle_key(key(KeyCode::Char('j')));
        assert_ne!(app.selected, selected);
        assert_eq!(app.detail_scroll, 0);
    }

    #[test]
    fn selectable_pr_details_navigate_sort_route_urls_and_reconcile() {
        let authored = authored("team", "project", 42, "2026-01-01");
        let identity = authored.identity.clone();
        let mut app = App::new(Vec::new(), PathBuf::from("/elsewhere"));
        app.virtual_repositories = vec![VirtualRepositoryView {
            identity: identity.repository.clone(),
            mapped_repository: None,
            expanded: true,
            pull_requests: vec![authored.clone()],
        }];
        app.selected = Some(RowId::VirtualPullRequest(identity.clone()));
        let check = |name: &str, state, order, url: Option<&str>| crate::model::PullRequestCheck {
            name: name.to_owned(),
            state,
            target_url: url.map(str::to_owned),
            required: true,
            source_order: order,
            completed_at: None,
        };
        let feedback_id = "feedback".to_owned();
        app.pull_request_details.insert(
            identity.clone(),
            PullRequestDetails {
                checks: vec![
                    check("success", crate::model::CheckState::Success, 0, None),
                    check(
                        "failure",
                        crate::model::CheckState::Failure,
                        1,
                        Some("https://checks/failure"),
                    ),
                    check("pending", crate::model::CheckState::Pending, 2, None),
                ],
                check_contexts_complete: true,
                reviews_complete: true,
                feedback: vec![crate::model::PullRequestFeedback {
                    id: feedback_id.clone(),
                    database_id: Some(7),
                    thread_id: Some("thread".to_owned()),
                    kind: crate::model::FeedbackKind::InlineThread,
                    author: "reviewer".to_owned(),
                    body: "line one\n line two".to_owned(),
                    path: Some("src/lib.rs".to_owned()),
                    permalink: Some("https://comments/7".to_owned()),
                    outdated: false,
                }],
                feedback_complete: true,
                ..PullRequestDetails::default()
            },
        );

        app.handle_key(key(KeyCode::Char('l')));
        assert_eq!(app.pane, Pane::Detail);
        assert!(matches!(app.detail_selected, Some(DetailRowId::Summary(_))));
        app.authored_pull_requests.loading = true;
        assert!(
            app.detail_rows()[0]
                .lines
                .iter()
                .any(|line| line == "GitHub: refreshing")
        );
        app.authored_pull_requests.loading = false;
        app.set_detail_viewport_height(4);
        app.handle_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL));
        assert!(!matches!(
            app.detail_selected,
            Some(DetailRowId::Summary(_))
        ));
        app.handle_key(key(KeyCode::Char('g')));
        let rows = app.detail_rows();
        let check_names: Vec<_> = rows
            .iter()
            .filter_map(|row| match &row.id {
                DetailRowId::Check(_, name) => Some(name.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(check_names, vec!["failure", "pending", "success"]);

        app.detail_selected = Some(DetailRowId::Check(identity.clone(), "failure".to_owned()));
        assert_eq!(
            app.handle_key(key(KeyCode::Enter)),
            Intent::OpenUrl("https://checks/failure".to_owned())
        );
        app.detail_selected = Some(DetailRowId::Check(identity.clone(), "success".to_owned()));
        assert_eq!(
            app.handle_key(key(KeyCode::Enter)),
            Intent::OpenUrl(authored.pull_request.url.clone())
        );
        app.handle_key(key(KeyCode::Char('G')));
        assert_eq!(
            app.handle_key(key(KeyCode::Enter)),
            Intent::OpenUrl("https://comments/7".to_owned())
        );

        let generation = app.github_generation;
        app.apply_pull_request_details(
            generation,
            BTreeMap::from([(
                identity.clone(),
                Ok(PullRequestDetails {
                    check_contexts_complete: true,
                    reviews_complete: true,
                    feedback_complete: true,
                    ..PullRequestDetails::default()
                }),
            )]),
        );
        assert!(
            app.detail_selected
                .as_ref()
                .is_some_and(|selected| app.detail_rows().iter().any(|row| &row.id == selected))
        );
        assert!(app.detail_rows().iter().any(|row| {
            matches!(row.id, DetailRowId::Section(_, DetailSection::Checks))
                && row.lines[0].contains("none or unavailable")
        }));
        app.handle_key(key(KeyCode::Char('h')));
        assert_eq!(app.pane, Pane::List);
    }

    #[test]
    fn agent_prompt_scopes_are_exact_deterministic_and_collapse_independent() {
        let mut parent = authored("team", "project", 1, "2026-01-01");
        parent.pull_request.head.branch = "stack-parent".to_owned();
        let mut child = authored("team", "project", 2, "2026-01-02");
        child.pull_request.base = parent.pull_request.head.clone();
        child.pull_request.head.branch = "stack-child".to_owned();
        let unrelated = authored("team", "project", 3, "2026-01-03");
        let mut app = App::new(Vec::new(), PathBuf::from("/elsewhere"));
        app.virtual_repositories = vec![VirtualRepositoryView {
            identity: parent.identity.repository.clone(),
            mapped_repository: None,
            expanded: true,
            pull_requests: vec![unrelated.clone(), child.clone(), parent.clone()],
        }];
        for pull_request in [&parent, &child, &unrelated] {
            app.pull_request_details.insert(
                pull_request.identity.clone(),
                PullRequestDetails {
                    checks: vec![crate::model::PullRequestCheck {
                        name: format!("check-{}", pull_request.identity.number),
                        state: crate::model::CheckState::Failure,
                        target_url: None,
                        required: true,
                        source_order: 0,
                        completed_at: None,
                    }],
                    check_contexts_complete: true,
                    feedback: vec![crate::model::PullRequestFeedback {
                        id: format!("feedback-{}", pull_request.identity.number),
                        database_id: None,
                        thread_id: None,
                        kind: crate::model::FeedbackKind::ReviewSummary,
                        author: "reviewer".to_owned(),
                        body: format!("body {}", pull_request.identity.number),
                        path: None,
                        permalink: None,
                        outdated: false,
                    }],
                    ..PullRequestDetails::default()
                },
            );
        }
        app.selected = Some(RowId::VirtualPullRequest(parent.identity.clone()));
        assert_eq!(
            app.handle_key(key(KeyCode::Char('C'))),
            Intent::BeginAction(Action::CopyAgentPrompt)
        );
        let expanded = app.agent_prompt().unwrap();
        assert!(expanded.contains("PR #1"));
        assert!(expanded.contains("PR #2"));
        assert!(!expanded.contains("PR #3"));
        assert!(expanded.find("PR #1").unwrap() < expanded.find("PR #2").unwrap());

        app.virtual_repositories[0].expanded = false;
        assert_eq!(app.agent_prompt().unwrap(), expanded);

        app.pane = Pane::Detail;
        app.detail_selected = Some(DetailRowId::Check(
            parent.identity.clone(),
            "check-1".to_owned(),
        ));
        let single = app.agent_prompt().unwrap();
        assert!(single.contains("check-1"));
        assert!(!single.contains("feedback-1"));
        assert!(!single.contains("PR #2"));

        app.detail_selected = Some(DetailRowId::Section(
            parent.identity.clone(),
            DetailSection::Feedback,
        ));
        let feedback = app.agent_prompt().unwrap();
        assert!(feedback.contains("feedback-1"));
        assert!(!feedback.contains("check-1"));
        assert!(!feedback.contains("PR #2"));
    }

    #[test]
    fn enter_in_non_pr_details_never_accepts_the_worktree() {
        let mut app = App::new(vec![repository("/repo", true)], PathBuf::from("/elsewhere"));
        app.selected = Some(RowId::Worktree(PathBuf::from("/repo")));
        app.pane = Pane::Detail;

        assert_eq!(app.handle_key(key(KeyCode::Enter)), Intent::None);
    }

    #[test]
    fn refreshes_coalesce_and_stale_generations_are_rejected() {
        let mut app = App::new(vec![repository("/repo", true)], PathBuf::from("/elsewhere"));
        let paths = vec![PathBuf::from("/repo"), PathBuf::from("/repo-topic")];
        let generation = app.begin_status_refresh(&paths);
        assert_eq!(app.request_refresh(), Intent::RefreshGitHub);
        assert!(!app.apply_status(StatusUpdate {
            generation: generation.wrapping_sub(1),
            path: paths[0].clone(),
            result: Ok(WorktreeStatus::default()),
        }));
        assert!(!app.apply_status(StatusUpdate {
            generation,
            path: paths[0].clone(),
            result: Ok(WorktreeStatus::default()),
        }));
        assert!(app.apply_status(StatusUpdate {
            generation,
            path: paths[1].clone(),
            result: Ok(WorktreeStatus::default()),
        }));
    }

    #[test]
    fn github_refresh_retains_stale_data_and_rejects_old_generations() {
        let mut app = App::new(vec![repository("/repo", true)], PathBuf::from("/elsewhere"));
        let path = PathBuf::from("/repo-topic");
        let data = GitHubBranchData {
            pull_request: None,
            warnings: vec!["first warning".to_owned()],
            rate_limit: None,
        };
        let first = app.begin_github_refresh(std::slice::from_ref(&path));
        let mut results = HashMap::new();
        results.insert(path.clone(), Ok(data.clone()));
        assert!(app.apply_github_refresh(first, std::slice::from_ref(&path), results));

        let second = app.begin_github_refresh(std::slice::from_ref(&path));
        assert!(!app.apply_github_refresh(first, std::slice::from_ref(&path), HashMap::new(),));
        let mut failed = HashMap::new();
        failed.insert(path.clone(), Err(GitHubError::Unauthorized));
        assert!(app.apply_github_refresh(second, std::slice::from_ref(&path), failed));
        assert!(matches!(
            app.github.get(&path),
            Some(GitHubState::Stale {
                previous: Some(previous),
                error,
            }) if previous == &data && error.contains("authentication")
        ));
    }

    #[test]
    fn canonical_detail_refresh_retains_data_on_error_and_rejects_old_generation() {
        let mut app = App::new(Vec::new(), PathBuf::from("/elsewhere"));
        let identity = CanonicalPullRequestId {
            repository: GitHubRepositoryIdentity::canonical("github.com", "team", "project"),
            number: 42,
        };
        let first = app.begin_github_refresh(&[]);
        let details = PullRequestDetails {
            check_contexts_complete: true,
            ..PullRequestDetails::default()
        };
        assert!(app.apply_pull_request_details(
            first,
            BTreeMap::from([(identity.clone(), Ok(details.clone()))]),
        ));
        let second = app.begin_github_refresh(&[]);
        assert!(!app.apply_pull_request_details(
            first,
            BTreeMap::from([(identity.clone(), Ok(PullRequestDetails::default()))]),
        ));
        assert!(app.apply_pull_request_details(
            second,
            BTreeMap::from([(identity.clone(), Err(GitHubError::Unauthorized))]),
        ));

        assert_eq!(app.pull_request_details[&identity], details);
        assert!(app.pull_request_detail_errors[&identity].contains("authentication"));
    }

    #[test]
    fn filter_matches_pull_request_enrichment() {
        let mut app = App::new(vec![repository("/repo", true)], PathBuf::from("/elsewhere"));
        let path = PathBuf::from("/repo-topic");
        let repository_identity = GitHubRepositoryIdentity::canonical("github.com", "team", "repo");
        app.repositories[0]
            .config
            .github_remotes
            .insert("origin".to_owned(), repository_identity.clone());
        app.github.insert(
            path.clone(),
            GitHubState::Ready(GitHubBranchData {
                pull_request: Some(crate::model::PullRequest {
                    number: 42,
                    title: "Improve frobnicator".to_owned(),
                    url: "https://example.test/pull/42".to_owned(),
                    state: crate::model::PullRequestState::Open,
                    updated_at: "2026-07-30T00:00:00Z".to_owned(),
                    review_decision: Some("APPROVED".to_owned()),
                    auto_merge: true,
                    base: crate::model::PullRequestIdentity {
                        repository: Some("team/repo".to_owned()),
                        branch: "main".to_owned(),
                        oid: None,
                    },
                    head: crate::model::PullRequestIdentity {
                        repository: Some("fork/repo".to_owned()),
                        branch: "topic".to_owned(),
                        oid: None,
                    },
                    checks: crate::model::CheckRollup::Success,
                }),
                warnings: Vec::new(),
                rate_limit: None,
            }),
        );
        app.pull_request_details.insert(
            CanonicalPullRequestId {
                repository: repository_identity,
                number: 42,
            },
            PullRequestDetails {
                checks: vec![crate::model::PullRequestCheck {
                    name: "deep-lint".to_owned(),
                    state: crate::model::CheckState::Failure,
                    target_url: Some("https://checks.example/hidden".to_owned()),
                    required: false,
                    source_order: 0,
                    completed_at: None,
                }],
                check_contexts_complete: true,
                review_requests: vec![crate::model::ReviewRequest {
                    id: "review-request".to_owned(),
                    name: "platform-team".to_owned(),
                    kind: crate::model::ReviewerKind::Team,
                }],
                reviews_complete: true,
                feedback: vec![crate::model::PullRequestFeedback {
                    id: "comment-id".to_owned(),
                    database_id: Some(123),
                    thread_id: Some("thread-id".to_owned()),
                    kind: crate::model::FeedbackKind::InlineThread,
                    author: "reviewer".to_owned(),
                    body: "hidden race condition".to_owned(),
                    path: Some("src/hidden.rs".to_owned()),
                    permalink: None,
                    outdated: false,
                }],
                feedback_complete: true,
                ..PullRequestDetails::default()
            },
        );
        for filter in [
            "frobnicator",
            "deep-lint",
            "platform-team",
            "race condition",
            "src/hidden.rs",
            "123",
        ] {
            app.filter = filter.to_owned();
            assert!(app.visible_rows().iter().any(
                |row| matches!(row, VisibleRow::Worktree { id: RowId::Worktree(found), .. } if found == &path)
            ));
        }
        app.filter.clear();
        app.selected = Some(RowId::Worktree(path));
        assert!(
            app.detail_rows()[0]
                .lines
                .iter()
                .any(|line| line.contains("local path: /repo-topic"))
        );
    }

    #[test]
    fn selection_identity_survives_reordered_refresh() {
        let mut app = App::new(
            vec![repository("/one", true), repository("/two", true)],
            PathBuf::from("/elsewhere"),
        );
        app.selected = Some(RowId::Worktree(PathBuf::from("/two-topic")));
        app.replace_repositories(vec![repository("/two", true), repository("/one", true)]);
        assert_eq!(
            app.selected,
            Some(RowId::Worktree(PathBuf::from("/two-topic")))
        );
    }

    #[test]
    fn action_availability_handles_headers_bare_stale_dirty_and_session_only() {
        let mut session = repository("/session", true);
        session.session_only = true;
        let mut stale = repository("/stale", true);
        stale.stale_error = Some("missing".to_owned());
        stale.worktrees.clear();
        let mut bare = repository("/bare", true);
        bare.worktrees = vec![worktree("/bare", "main", true)];
        let mut app = App::new(vec![session, stale, bare], PathBuf::from("/elsewhere"));
        app.selected = Some(RowId::Repository(PathBuf::from("/session")));
        assert!(app.action_availability(Action::RegisterRepository).enabled);
        app.selected = Some(RowId::Repository(PathBuf::from("/stale")));
        assert!(!app.action_availability(Action::Create).enabled);
        assert!(app.action_availability(Action::EditRepository).enabled);
        app.selected = Some(RowId::Repository(PathBuf::from("/bare")));
        assert!(app.action_availability(Action::Create).enabled);
        assert!(!app.action_availability(Action::Remove).enabled);
        app.selected = Some(RowId::Worktree(PathBuf::from("/session-topic")));
        app.statuses.insert(
            PathBuf::from("/session-topic"),
            StatusState::Ready(WorktreeStatus {
                untracked: 1,
                ..WorktreeStatus::default()
            }),
        );
        assert!(!app.action_availability(Action::Remove).enabled);
    }

    #[test]
    fn palette_forms_and_confirmations_have_testable_transitions() {
        let mut app = App::new(vec![repository("/repo", true)], PathBuf::from("/elsewhere"));
        app.handle_key(key(KeyCode::Char('?')));
        assert!(matches!(app.modal, Some(Modal::Palette { .. })));
        app.modal = None;
        app.open_form(
            Action::Create,
            vec![FormField {
                label: "branch".to_owned(),
                value: String::new(),
            }],
        );
        app.handle_key(key(KeyCode::Char('x')));
        assert_eq!(
            app.handle_key(key(KeyCode::Enter)),
            Intent::SubmitForm {
                action: Action::Create,
                values: vec!["x".to_owned()]
            }
        );
        app.open_confirmation(Action::Create, vec!["create x".to_owned()]);
        assert_eq!(
            app.handle_key(key(KeyCode::Char('y'))),
            Intent::ConfirmAction(Action::Create)
        );
    }

    #[test]
    fn containing_worktree_is_selected_and_ctrl_c_cancels() {
        let app = App::new(
            vec![repository("/unrelated", true), repository("/project", true)],
            PathBuf::from("/project/subdirectory"),
        );
        assert_eq!(
            app.selected,
            Some(RowId::Worktree(PathBuf::from("/project")))
        );
        let mut app = app;
        assert_eq!(
            app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Intent::Cancel
        );
    }

    #[test]
    fn authored_snapshot_overlays_pages_then_commits_or_reverts_atomically() {
        let authored = |number: u64| {
            let repository =
                crate::model::GitHubRepositoryIdentity::canonical("github.com", "base", "project");
            AuthoredPullRequest {
                identity: CanonicalPullRequestId { repository, number },
                author: "viewer".to_owned(),
                pull_request: crate::model::PullRequest {
                    number,
                    title: format!("change {number}"),
                    url: format!("https://example/pull/{number}"),
                    state: crate::model::PullRequestState::Open,
                    updated_at: "2026-01-01T00:00:00Z".to_owned(),
                    review_decision: None,
                    auto_merge: false,
                    base: crate::model::PullRequestIdentity {
                        repository: Some("base/project".to_owned()),
                        branch: "main".to_owned(),
                        oid: None,
                    },
                    head: crate::model::PullRequestIdentity {
                        repository: Some("fork/project".to_owned()),
                        branch: "topic".to_owned(),
                        oid: Some("head".to_owned()),
                    },
                    checks: crate::model::CheckRollup::Pending,
                },
            }
        };
        let mut state = AuthoredPullRequestState::default();
        let first = state.begin();
        assert!(state.apply_page(
            first,
            "github.com".to_owned(),
            1,
            vec![authored(1)],
            Vec::new()
        ));
        assert!(state.finish(first, true, Vec::new(), None));
        assert_eq!(state.identities()[0].number, 1);

        let failed = state.begin();
        assert!(state.apply_page(
            failed,
            "github.com".to_owned(),
            1,
            vec![authored(2)],
            vec!["page warning".to_owned()]
        ));
        assert_eq!(
            state
                .identities()
                .into_iter()
                .map(|identity| identity.number)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert!(!state.apply_page(
            first,
            "github.com".to_owned(),
            2,
            vec![authored(3)],
            Vec::new()
        ));
        assert!(state.finish(
            failed,
            false,
            Vec::new(),
            Some("later page failed".to_owned())
        ));
        assert_eq!(state.identities()[0].number, 1);
        assert_eq!(state.stale_error.as_deref(), Some("later page failed"));

        let replacement = state.begin();
        state.apply_page(
            replacement,
            "github.com".to_owned(),
            1,
            vec![authored(2)],
            Vec::new(),
        );
        state.finish(replacement, true, Vec::new(), None);
        assert_eq!(state.identities()[0].number, 2);
    }
}
