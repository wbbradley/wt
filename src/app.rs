#[cfg(test)]
use std::cell::Cell;
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use regex::{Regex, RegexBuilder};

use crate::github::{GitHubError, PullRequestMapping};
use crate::model::{
    AuthoredPullRequest, CanonicalPullRequestId, CheckState, GitHubBranchData,
    GitHubRepositoryIdentity, PullRequest, PullRequestDetails, RepositoryConfig,
    SubmittedReviewState, Worktree, WorktreeStatus,
};
use crate::prompt::{
    PromptPullRequest, PromptWorktree, concise_comment_text, format_agent_prompt,
    format_review_request,
};

const LIST_SCROLL_MARGIN: usize = 5;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum BranchId {
    Worktree(PathBuf),
    VirtualPullRequest(CanonicalPullRequestId),
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub enum InlineSection {
    Overview,
    Checks,
    PendingChecks,
    ValidResults,
    Reviewers,
    OpenComments,
    StackedBranches,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum DisclosureKey {
    Repository(PathBuf),
    VirtualRepository(GitHubRepositoryIdentity),
    Backburner(GitHubRepositoryIdentity),
    Branch(BranchId),
    Section(BranchId, InlineSection),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum RowId {
    Repository(PathBuf),
    Worktree(PathBuf),
    VirtualRepository(GitHubRepositoryIdentity),
    Backburner(GitHubRepositoryIdentity),
    VirtualPullRequest(CanonicalPullRequestId),
    Section(BranchId, InlineSection),
    Metadata(BranchId, String),
    Check(CanonicalPullRequestId, String),
    Reviewer(CanonicalPullRequestId, String),
    OpenComment(CanonicalPullRequestId, String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum FocusTarget {
    Repository(PathBuf),
    VirtualRepository(GitHubRepositoryIdentity),
    Backburner(GitHubRepositoryIdentity),
    Branch(BranchId),
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

    pub fn singleton_worktree(&self) -> Option<(usize, &Worktree)> {
        match self.worktrees.as_slice() {
            [worktree] if !worktree.bare => Some((0, worktree)),
            _ => None,
        }
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
        expanded: bool,
        has_children: bool,
        singleton_worktree_index: Option<usize>,
        id: RowId,
    },
    Worktree {
        repository_index: usize,
        worktree_index: usize,
        depth: usize,
        expanded: bool,
        has_children: bool,
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
        depth: usize,
        expanded: bool,
        has_children: bool,
        id: RowId,
    },
    Backburner {
        identity: GitHubRepositoryIdentity,
        depth: usize,
        expanded: bool,
        id: RowId,
    },
    Inline {
        owner: BranchId,
        section: InlineSection,
        depth: usize,
        kind: InlineRowKind,
        text: String,
        url: Option<String>,
        expanded: Option<bool>,
        id: RowId,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InlineRowKind {
    Section,
    Metadata,
    Check,
    Reviewer,
    OpenComment,
}

impl VisibleRow {
    pub fn id(&self) -> &RowId {
        match self {
            Self::Repository { id, .. }
            | Self::Worktree { id, .. }
            | Self::VirtualRepository { id, .. }
            | Self::Backburner { id, .. }
            | Self::VirtualPullRequest { id, .. }
            | Self::Inline { id, .. } => id,
        }
    }

    pub fn owner(&self) -> Option<BranchId> {
        match self {
            Self::Worktree {
                id: RowId::Worktree(path),
                ..
            } => Some(BranchId::Worktree(path.clone())),
            Self::VirtualPullRequest {
                id: RowId::VirtualPullRequest(identity),
                ..
            } => Some(BranchId::VirtualPullRequest(identity.clone())),
            Self::Inline { owner, .. } => Some(owner.clone()),
            Self::Repository { .. }
            | Self::Worktree { .. }
            | Self::VirtualRepository { .. }
            | Self::VirtualPullRequest { .. }
            | Self::Backburner { .. } => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Action {
    CopyAgentPrompt,
    CopyReviewRequest,
    OpenPullRequestWeb,
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
    pub const ALL: [Self; 14] = [
        Self::CopyAgentPrompt,
        Self::CopyReviewRequest,
        Self::OpenPullRequestWeb,
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
            Self::CopyReviewRequest => "copy review request",
            Self::OpenPullRequestWeb => "open pull request in browser",
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

    pub fn shortcut(self) -> Option<&'static str> {
        match self {
            Self::CopyAgentPrompt => Some("c"),
            Self::CopyReviewRequest => Some("p"),
            Self::OpenPullRequestWeb => Some("w"),
            Self::Create => None,
            Self::NewWorktree => Some("n"),
            Self::Move => Some("m"),
            Self::Lock => Some("L"),
            Self::Unlock => Some("U"),
            Self::Remove => Some("d"),
            Self::Repair => Some("R"),
            Self::Prune => Some("P"),
            Self::RegisterRepository => Some("a"),
            Self::EditRepository => Some("e"),
            Self::RemoveRepository => Some("x"),
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
    PersistBackburner,
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
    focus_target: Option<FocusTarget>,
    pub filter: String,
    pub filter_active: bool,
    pub scroll: usize,
    pub viewport_height: usize,
    viewport_initialized: bool,
    disclosure_expanded: HashMap<DisclosureKey, bool>,
    filter_collapsed: HashSet<DisclosureKey>,
    filter_expanded: HashSet<DisclosureKey>,
    pub modal: Option<Modal>,
    pub inline_error: Option<String>,
    pub progress: Option<String>,
    pub statuses: HashMap<PathBuf, StatusState>,
    pub branch_parents: HashMap<PathBuf, PathBuf>,
    pub github: HashMap<PathBuf, GitHubState>,
    pub github_generation: u64,
    pub github_loading: bool,
    pub last_refresh: Option<Instant>,
    github_network_paths: HashSet<PathBuf>,
    github_spinner_frame: usize,
    pub github_hosts: BTreeSet<String>,
    pub authored_pull_requests: AuthoredPullRequestState,
    pub active_pull_requests: HashSet<CanonicalPullRequestId>,
    pub pull_request_details: BTreeMap<CanonicalPullRequestId, PullRequestDetails>,
    pub pull_request_detail_errors: BTreeMap<CanonicalPullRequestId, String>,
    pub backburner: BTreeSet<CanonicalPullRequestId>,
    pub authored_mappings: Vec<PullRequestMapping>,
    pub current_directory: PathBuf,
    current_worktree: Option<PathBuf>,
    pub generation: u64,
    pending_status: usize,
    status_progress_visible: bool,
    refresh_queued: bool,
    #[cfg(test)]
    visible_row_builds: Cell<usize>,
}

impl App {
    pub fn new(repositories: Vec<RepositoryView>, current_directory: PathBuf) -> Self {
        let mut app = Self {
            repositories,
            virtual_repositories: Vec::new(),
            selected: None,
            focus_target: None,
            filter: String::new(),
            filter_active: false,
            scroll: 0,
            viewport_height: 1,
            viewport_initialized: false,
            disclosure_expanded: HashMap::new(),
            filter_collapsed: HashSet::new(),
            filter_expanded: HashSet::new(),
            modal: None,
            inline_error: None,
            progress: None,
            statuses: HashMap::new(),
            branch_parents: HashMap::new(),
            github: HashMap::new(),
            github_generation: 0,
            github_loading: false,
            last_refresh: None,
            github_network_paths: HashSet::new(),
            github_spinner_frame: 0,
            github_hosts: BTreeSet::new(),
            authored_pull_requests: AuthoredPullRequestState::default(),
            active_pull_requests: HashSet::new(),
            pull_request_details: BTreeMap::new(),
            pull_request_detail_errors: BTreeMap::new(),
            backburner: BTreeSet::new(),
            authored_mappings: Vec::new(),
            current_directory,
            current_worktree: None,
            generation: 0,
            pending_status: 0,
            status_progress_visible: false,
            refresh_queued: false,
            #[cfg(test)]
            visible_row_builds: Cell::new(0),
        };
        app.refresh_current_worktree();
        app.select_initial();
        app
    }

    pub fn minutes_since_last_refresh(&self) -> Option<u64> {
        self.last_refresh
            .map(|refreshed| refreshed.elapsed().as_secs() / 60)
    }

    pub fn visible_rows(&self) -> Vec<VisibleRow> {
        #[cfg(test)]
        self.visible_row_builds
            .set(self.visible_row_builds.get().saturating_add(1));
        let rows = self.scoped_logical_rows();
        if self.filter_mode() {
            self.filtered_rows(rows, true)
        } else {
            rows
        }
    }

    #[cfg(test)]
    pub(crate) fn reset_visible_row_builds(&self) {
        self.visible_row_builds.set(0);
    }

    #[cfg(test)]
    pub(crate) fn visible_row_builds(&self) -> usize {
        self.visible_row_builds.get()
    }

    pub(crate) fn is_current_worktree(&self, path: &Path) -> bool {
        self.current_worktree.as_deref() == Some(path)
    }

    fn refresh_current_worktree(&mut self) {
        let candidate = canonical_path_or_owned(&self.current_directory);
        self.current_worktree = self
            .repositories
            .iter()
            .flat_map(|repository| &repository.worktrees)
            .filter(|worktree| !worktree.bare)
            .filter_map(|worktree| {
                let canonical = canonical_path_or_owned(&worktree.path);
                candidate
                    .starts_with(&canonical)
                    .then_some((canonical.components().count(), worktree.path.clone()))
            })
            .max_by_key(|(depth, _)| *depth)
            .map(|(_, path)| path);
    }

    fn logical_rows(&self) -> Vec<VisibleRow> {
        let mut rows = Vec::new();
        let mut repositories = self
            .repositories
            .iter()
            .enumerate()
            .map(|(index, repository)| {
                (
                    repository.config.display_label(),
                    TopLevelRepository::Local(index),
                )
            })
            .chain(
                self.virtual_repositories
                    .iter()
                    .enumerate()
                    .filter(|(_, repository)| repository.mapped_repository.is_none())
                    .map(|(index, repository)| {
                        (
                            repository.identity.full_name(),
                            TopLevelRepository::Virtual(index),
                        )
                    }),
            )
            .collect::<Vec<_>>();
        repositories.sort_by(|(left_label, left), (right_label, right)| {
            alphabetical_cmp(left_label, right_label).then_with(|| left.cmp(right))
        });
        for (_, repository) in repositories {
            match repository {
                TopLevelRepository::Local(repository_index) => {
                    self.append_local_repository(&mut rows, repository_index)
                }
                TopLevelRepository::Virtual(virtual_repository_index) => {
                    self.append_virtual_repository(&mut rows, virtual_repository_index)
                }
            }
        }
        rows
    }

    fn scoped_logical_rows(&self) -> Vec<VisibleRow> {
        self.focus_target
            .as_ref()
            .and_then(|target| self.focused_rows(target))
            .unwrap_or_else(|| self.logical_rows())
    }

    fn focused_rows(&self, target: &FocusTarget) -> Option<Vec<VisibleRow>> {
        let mut rows = Vec::new();
        match target {
            FocusTarget::Repository(path) => {
                let repository_index = self
                    .repositories
                    .iter()
                    .position(|repository| repository.config.path == *path)?;
                self.append_local_repository(&mut rows, repository_index);
            }
            FocusTarget::VirtualRepository(identity) => {
                let virtual_repository_index =
                    self.virtual_repositories.iter().position(|repository| {
                        repository.identity == *identity && repository.mapped_repository.is_none()
                    })?;
                self.append_virtual_repository(&mut rows, virtual_repository_index);
            }
            FocusTarget::Backburner(identity) => {
                let (forest, included, mapped_repository_index) =
                    self.backburner_focus_context(identity)?;
                self.append_backburner(
                    &mut rows,
                    &forest,
                    &included,
                    identity.clone(),
                    0,
                    mapped_repository_index,
                );
            }
            FocusTarget::Branch(branch) => {
                let (forest, index, mapped_repository_index) = self.branch_scope_context(branch)?;
                let included = branch_subtree_indexes(&forest, index);
                self.append_branch(
                    &mut rows,
                    &forest,
                    &included,
                    index,
                    0,
                    mapped_repository_index,
                    None,
                );
            }
        }
        Some(rows)
    }

    fn backburner_focus_context(
        &self,
        identity: &GitHubRepositoryIdentity,
    ) -> Option<(BranchForest, BTreeSet<usize>, Option<usize>)> {
        for (repository_index, repository) in self.repositories.iter().enumerate() {
            let virtual_indexes = self
                .virtual_repositories
                .iter()
                .enumerate()
                .filter(|(_, candidate)| {
                    candidate.mapped_repository.as_ref() == Some(&repository.config.path)
                })
                .map(|(index, _)| index)
                .collect::<Vec<_>>();
            let forest = self.branch_forest(Some(repository_index), &virtual_indexes);
            let included = self.backburner_group_indexes(&forest, identity);
            if !included.is_empty() {
                return Some((forest, included, Some(repository_index)));
            }
        }
        for (virtual_index, repository) in self.virtual_repositories.iter().enumerate() {
            if repository.mapped_repository.is_some() || repository.identity != *identity {
                continue;
            }
            let forest = self.branch_forest(None, &[virtual_index]);
            let included = self.backburner_group_indexes(&forest, identity);
            if !included.is_empty() {
                return Some((forest, included, None));
            }
        }
        None
    }

    fn backburner_group_indexes(
        &self,
        forest: &BranchForest,
        identity: &GitHubRepositoryIdentity,
    ) -> BTreeSet<usize> {
        forest
            .nodes
            .iter()
            .enumerate()
            .filter(|(index, node)| {
                node.backburnered
                    && forest.nodes[backburner_root_index(forest, *index)]
                        .identity
                        .as_ref()
                        .is_some_and(|root| root.repository == *identity)
            })
            .map(|(index, _)| index)
            .collect()
    }

    fn append_local_repository(&self, rows: &mut Vec<VisibleRow>, repository_index: usize) {
        let repository = &self.repositories[repository_index];
        let virtual_repository_indexes = self
            .virtual_repositories
            .iter()
            .enumerate()
            .filter(|(_, candidate)| {
                candidate.mapped_repository.as_deref() == Some(repository.config.path.as_path())
            })
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        let forest = self.branch_forest(Some(repository_index), &virtual_repository_indexes);
        let normal = self.included_branch_nodes(&forest, |node| !node.backburnered);
        let backburner = self.included_branch_nodes(&forest, |node| node.backburnered);
        let candidate_singleton_worktree_index = repository
            .singleton_worktree()
            .map(|(worktree_index, _)| worktree_index);
        let candidate_singleton_node =
            candidate_singleton_worktree_index.and_then(|worktree_index| {
                forest.nodes.iter().position(|node| {
                    node.source
                        == BranchSource::Worktree {
                            repository_index,
                            worktree_index,
                        }
                })
            });
        let singleton_node = candidate_singleton_node.filter(|index| normal.contains(index));
        let singleton_worktree_index = singleton_node.map(|_| {
            candidate_singleton_worktree_index.expect("singleton node has a worktree index")
        });
        let mut children = Vec::new();
        self.append_repository_branch_roots(
            &mut children,
            &forest,
            &normal,
            1,
            Some(repository_index),
            singleton_node,
        );
        let mut backburner_groups = BTreeMap::<GitHubRepositoryIdentity, BTreeSet<usize>>::new();
        for index in &backburner {
            let root = backburner_root_index(&forest, *index);
            let Some(identity) = forest.nodes[root]
                .identity
                .as_ref()
                .map(|identity| identity.repository.clone())
            else {
                continue;
            };
            backburner_groups
                .entry(identity)
                .or_default()
                .insert(*index);
        }
        for (identity, group) in backburner_groups {
            self.append_backburner(
                &mut children,
                &forest,
                &group,
                identity,
                1,
                Some(repository_index),
            );
        }
        let expanded = self.disclosure_expanded(
            &DisclosureKey::Repository(repository.config.path.clone()),
            repository.expanded,
        );
        rows.push(VisibleRow::Repository {
            repository_index,
            expanded,
            has_children: !children.is_empty(),
            singleton_worktree_index,
            id: repository.id(),
        });
        if expanded {
            rows.extend(children);
        }
    }

    fn append_virtual_repository(
        &self,
        rows: &mut Vec<VisibleRow>,
        virtual_repository_index: usize,
    ) {
        let repository = &self.virtual_repositories[virtual_repository_index];
        let forest = self.branch_forest(None, &[virtual_repository_index]);
        let normal = self.included_branch_nodes(&forest, |node| !node.backburnered);
        let backburner = self.included_branch_nodes(&forest, |node| node.backburnered);
        rows.push(VisibleRow::VirtualRepository {
            virtual_repository_index,
            id: repository.id(),
        });
        if self.disclosure_expanded(
            &DisclosureKey::VirtualRepository(repository.identity.clone()),
            repository.expanded,
        ) {
            self.append_branch_roots(rows, &forest, &normal, 1, None);
            if !backburner.is_empty() {
                self.append_backburner(
                    rows,
                    &forest,
                    &backburner,
                    repository.identity.clone(),
                    1,
                    None,
                );
            }
        }
    }

    fn filter_mode(&self) -> bool {
        self.filter_active || !self.filter.is_empty()
    }

    pub fn set_committed_filter(&mut self, filter: impl Into<String>) {
        self.filter = filter.into();
        self.filter_active = false;
        self.filter_collapsed.clear();
        self.filter_expanded.clear();
        self.scroll = 0;
        self.ensure_selection_visible();
    }

    fn filtered_rows(
        &self,
        rows: Vec<VisibleRow>,
        apply_temporary_collapses: bool,
    ) -> Vec<VisibleRow> {
        let Ok(query) = RegexBuilder::new(&self.filter)
            .case_insensitive(true)
            .build()
        else {
            return Vec::new();
        };
        let mut ancestors = Vec::<usize>::new();
        let mut parents = Vec::with_capacity(rows.len());
        let mut matches = Vec::with_capacity(rows.len());
        for (index, row) in rows.iter().enumerate() {
            let depth = self.visible_row_depth(row);
            ancestors.truncate(depth);
            parents.push(ancestors.last().copied());
            matches.push(self.row_matches_filter(row, &query));
            ancestors.push(index);
        }
        let required_expanded = if self.filter.is_empty() {
            HashSet::new()
        } else {
            matches
                .iter()
                .enumerate()
                .filter_map(|(index, matches)| matches.then_some(parents[index]))
                .flatten()
                .flat_map(|mut ancestor| {
                    let mut keys = Vec::new();
                    loop {
                        if let Some(key) = self.disclosure_key_for_row(&rows[ancestor]) {
                            keys.push(key);
                        }
                        let Some(parent) = parents[ancestor] else {
                            break;
                        };
                        ancestor = parent;
                    }
                    keys
                })
                .collect::<HashSet<_>>()
        };
        let mut retained = BTreeSet::new();
        if self.filter_active {
            for index in matches
                .iter()
                .enumerate()
                .filter_map(|(index, matches)| matches.then_some(index))
            {
                let mut ancestor = Some(index);
                while let Some(index) = ancestor {
                    retained.insert(index);
                    ancestor = parents[index];
                }
            }
        } else {
            let matching_scopes = matches
                .iter()
                .enumerate()
                .filter_map(|(index, matches)| {
                    matches
                        .then(|| self.committed_search_branch(&rows, &parents, index))
                        .flatten()
                })
                .collect::<HashSet<_>>();
            for (index, matches) in matches.iter().copied().enumerate() {
                let exact_unscoped = matches
                    && self
                        .committed_search_branch(&rows, &parents, index)
                        .is_none();
                let belongs_to_matching_scope = self
                    .committed_search_branch(&rows, &parents, index)
                    .is_some_and(|scope| matching_scopes.contains(&scope));
                if !exact_unscoped && !belongs_to_matching_scope {
                    continue;
                }
                let mut ancestor = Some(index);
                while let Some(index) = ancestor {
                    retained.insert(index);
                    ancestor = parents[index];
                }
            }
            for scope in matching_scopes {
                let mut ancestor = Some(scope);
                while let Some(index) = ancestor {
                    retained.insert(index);
                    ancestor = parents[index];
                }
            }
        }
        let filtered = rows
            .into_iter()
            .enumerate()
            .filter_map(|(index, row)| retained.contains(&index).then_some(row))
            .collect::<Vec<_>>();
        if !apply_temporary_collapses {
            return filtered;
        }

        let mut visible = Vec::new();
        let mut hidden_below = None;
        for mut row in filtered {
            let depth = self.visible_row_depth(&row);
            if hidden_below.is_some_and(|collapsed_depth| depth > collapsed_depth) {
                continue;
            }
            hidden_below = None;
            if let Some(key) = self.disclosure_key_for_row(&row) {
                let expanded =
                    self.search_disclosure_expanded(&key, required_expanded.contains(&key));
                Self::set_row_expanded(&mut row, expanded);
                if !expanded {
                    hidden_below = Some(depth);
                }
            }
            visible.push(row);
        }
        visible
    }

    fn committed_search_branch(
        &self,
        rows: &[VisibleRow],
        parents: &[Option<usize>],
        index: usize,
    ) -> Option<usize> {
        let mut candidate = Some(index);
        while let Some(index) = candidate {
            if matches!(
                rows[index],
                VisibleRow::Repository {
                    singleton_worktree_index: Some(_),
                    ..
                } | VisibleRow::Worktree { .. }
                    | VisibleRow::VirtualPullRequest { .. }
            ) {
                return Some(index);
            }
            candidate = parents[index];
        }
        None
    }

    fn row_matches_filter(&self, row: &VisibleRow, query: &Regex) -> bool {
        query.is_match(&crate::ui::row_search_text(self, row))
    }

    fn visible_row_depth(&self, row: &VisibleRow) -> usize {
        match row {
            VisibleRow::Repository { .. } => 0,
            VisibleRow::Worktree { depth, .. } => *depth,
            VisibleRow::VirtualRepository {
                virtual_repository_index,
                ..
            } => usize::from(
                self.virtual_repositories[*virtual_repository_index]
                    .mapped_repository
                    .is_some(),
            ),
            VisibleRow::VirtualPullRequest { depth, .. } => *depth,
            VisibleRow::Backburner { depth, .. } => *depth,
            VisibleRow::Inline { depth, .. } => *depth,
        }
    }

    fn disclosure_key_for_row(&self, row: &VisibleRow) -> Option<DisclosureKey> {
        match row {
            VisibleRow::Repository {
                repository_index, ..
            } => Some(DisclosureKey::Repository(
                self.repositories[*repository_index].config.path.clone(),
            )),
            VisibleRow::Worktree {
                id,
                has_children: true,
                ..
            } => {
                let RowId::Worktree(path) = id else {
                    return None;
                };
                Some(DisclosureKey::Branch(BranchId::Worktree(path.clone())))
            }
            VisibleRow::VirtualRepository {
                virtual_repository_index,
                ..
            } => Some(DisclosureKey::VirtualRepository(
                self.virtual_repositories[*virtual_repository_index]
                    .identity
                    .clone(),
            )),
            VisibleRow::VirtualPullRequest {
                id,
                has_children: true,
                ..
            } => {
                let RowId::VirtualPullRequest(identity) = id else {
                    return None;
                };
                Some(DisclosureKey::Branch(BranchId::VirtualPullRequest(
                    identity.clone(),
                )))
            }
            VisibleRow::Backburner { identity, .. } => {
                Some(DisclosureKey::Backburner(identity.clone()))
            }
            VisibleRow::Inline {
                owner,
                section,
                expanded: Some(_),
                ..
            } => Some(DisclosureKey::Section(owner.clone(), *section)),
            VisibleRow::Worktree {
                has_children: false,
                ..
            }
            | VisibleRow::VirtualPullRequest {
                has_children: false,
                ..
            }
            | VisibleRow::Inline { .. } => None,
        }
    }

    fn set_row_expanded(row: &mut VisibleRow, expanded: bool) {
        match row {
            VisibleRow::Repository {
                expanded: current, ..
            }
            | VisibleRow::Worktree {
                expanded: current, ..
            }
            | VisibleRow::VirtualPullRequest {
                expanded: current, ..
            }
            | VisibleRow::Backburner {
                expanded: current, ..
            } => *current = expanded,
            VisibleRow::Inline {
                expanded: current, ..
            } => *current = Some(expanded),
            VisibleRow::VirtualRepository { .. } => {}
        }
    }

    fn branch_forest(
        &self,
        repository_index: Option<usize>,
        virtual_repository_indexes: &[usize],
    ) -> BranchForest {
        let mut nodes = Vec::new();
        let mut local_indexes = HashMap::new();
        let mut represented_pull_requests = BTreeSet::new();
        if let Some(repository_index) = repository_index {
            let repository = &self.repositories[repository_index];
            for (worktree_index, worktree) in repository.worktrees.iter().enumerate() {
                if worktree.bare {
                    continue;
                }
                let pull_request = self
                    .github
                    .get(&worktree.path)
                    .and_then(GitHubState::data)
                    .and_then(|data| data.pull_request.clone());
                let identity = pull_request
                    .as_ref()
                    .and_then(|pull_request| self.pull_request_identity(repository, pull_request));
                if let Some(identity) = &identity {
                    represented_pull_requests.insert(identity.clone());
                }
                local_indexes.insert(worktree.path.clone(), nodes.len());
                nodes.push(BranchNode {
                    id: BranchId::Worktree(worktree.path.clone()),
                    source: BranchSource::Worktree {
                        repository_index,
                        worktree_index,
                    },
                    identity,
                    pull_request,
                    parent: None,
                    children: Vec::new(),
                    backburnered: false,
                });
            }
        }
        for virtual_repository_index in virtual_repository_indexes {
            for (pull_request_index, authored) in self.virtual_repositories
                [*virtual_repository_index]
                .pull_requests
                .iter()
                .enumerate()
            {
                if represented_pull_requests.contains(&authored.identity) {
                    continue;
                }
                nodes.push(BranchNode {
                    id: BranchId::VirtualPullRequest(authored.identity.clone()),
                    source: BranchSource::VirtualPullRequest {
                        virtual_repository_index: *virtual_repository_index,
                        pull_request_index,
                    },
                    identity: Some(authored.identity.clone()),
                    pull_request: Some(authored.pull_request.clone()),
                    parent: None,
                    children: Vec::new(),
                    backburnered: self.backburner.contains(&authored.identity),
                });
            }
        }
        for child in 0..nodes.len() {
            let Some(child_pull_request) = nodes[child].pull_request.as_ref() else {
                continue;
            };
            let candidates = nodes
                .iter()
                .enumerate()
                .filter(|(parent, node)| {
                    *parent != child
                        && node
                            .pull_request
                            .as_ref()
                            .is_some_and(|parent_pull_request| {
                                pull_request_identity_matches(
                                    &parent_pull_request.head,
                                    &child_pull_request.base,
                                )
                            })
                })
                .map(|(index, _)| index)
                .collect::<Vec<_>>();
            if let [parent] = candidates.as_slice() {
                nodes[child].parent = Some(*parent);
            }
        }
        for (index, node) in nodes.iter_mut().enumerate() {
            if node.pull_request.is_some() {
                continue;
            }
            let BranchSource::Worktree {
                repository_index,
                worktree_index,
            } = node.source
            else {
                continue;
            };
            let path = &self.repositories[repository_index].worktrees[worktree_index].path;
            node.parent = self
                .branch_parents
                .get(path)
                .and_then(|parent| local_indexes.get(parent))
                .copied()
                .filter(|parent| *parent != index);
        }
        let mut cyclic = BTreeSet::new();
        for start in 0..nodes.len() {
            let mut path = Vec::new();
            let mut current = Some(start);
            while let Some(index) = current {
                if let Some(cycle_start) = path.iter().position(|candidate| *candidate == index) {
                    cyclic.extend(path[cycle_start..].iter().copied());
                    break;
                }
                path.push(index);
                current = nodes[index].parent;
            }
        }
        for index in cyclic {
            nodes[index].parent = None;
        }
        let mut backburner_lineage = nodes
            .iter()
            .map(|node| {
                node.identity
                    .as_ref()
                    .is_some_and(|identity| self.backburner.contains(identity))
            })
            .collect::<Vec<_>>();
        for _ in 0..nodes.len() {
            let mut changed = false;
            for index in 0..nodes.len() {
                if !backburner_lineage[index]
                    && nodes[index]
                        .parent
                        .is_some_and(|parent| backburner_lineage[parent])
                {
                    backburner_lineage[index] = true;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        for (index, node) in nodes.iter_mut().enumerate() {
            node.backburnered = backburner_lineage[index];
        }
        for child in 0..nodes.len() {
            if let Some(parent) = nodes[child].parent {
                nodes[parent].children.push(child);
            }
        }
        BranchForest { nodes }
    }

    fn included_branch_nodes(
        &self,
        forest: &BranchForest,
        include: impl Fn(&BranchNode) -> bool,
    ) -> BTreeSet<usize> {
        forest
            .nodes
            .iter()
            .enumerate()
            .filter(|(_, node)| include(node))
            .map(|(index, _)| index)
            .collect()
    }

    fn append_branch_roots(
        &self,
        rows: &mut Vec<VisibleRow>,
        forest: &BranchForest,
        included: &BTreeSet<usize>,
        depth: usize,
        mapped_repository_index: Option<usize>,
    ) {
        let roots = self.sorted_branch_indexes(
            forest,
            included.iter().copied().filter(|index| {
                forest.nodes[*index]
                    .parent
                    .is_none_or(|parent| !included.contains(&parent))
            }),
        );
        for index in roots {
            self.append_branch(
                rows,
                forest,
                included,
                index,
                depth,
                mapped_repository_index,
                None,
            );
        }
    }

    fn append_repository_branch_roots(
        &self,
        rows: &mut Vec<VisibleRow>,
        forest: &BranchForest,
        included: &BTreeSet<usize>,
        depth: usize,
        mapped_repository_index: Option<usize>,
        flattened_worktree: Option<usize>,
    ) {
        let roots = self.sorted_branch_indexes(
            forest,
            included.iter().copied().filter(|index| {
                forest.nodes[*index]
                    .parent
                    .is_none_or(|parent| !included.contains(&parent))
            }),
        );
        for index in roots {
            if Some(index) == flattened_worktree {
                self.append_branch_contents(
                    rows,
                    forest,
                    included,
                    index,
                    depth,
                    mapped_repository_index,
                    flattened_worktree,
                );
            } else {
                self.append_branch(
                    rows,
                    forest,
                    included,
                    index,
                    depth,
                    mapped_repository_index,
                    flattened_worktree,
                );
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn append_branch(
        &self,
        rows: &mut Vec<VisibleRow>,
        forest: &BranchForest,
        included: &BTreeSet<usize>,
        index: usize,
        depth: usize,
        mapped_repository_index: Option<usize>,
        flattened_worktree: Option<usize>,
    ) {
        let node = &forest.nodes[index];
        if Some(index) == flattened_worktree {
            self.append_branch_contents(
                rows,
                forest,
                included,
                index,
                depth,
                mapped_repository_index,
                flattened_worktree,
            );
            return;
        }
        let expanded = self.disclosure_expanded(&DisclosureKey::Branch(node.id.clone()), true);
        let mut children = Vec::new();
        self.append_branch_contents(
            &mut children,
            forest,
            included,
            index,
            depth + 1,
            mapped_repository_index,
            flattened_worktree,
        );
        let has_children = !children.is_empty();
        match node.source {
            BranchSource::Worktree {
                repository_index,
                worktree_index,
            } => rows.push(VisibleRow::Worktree {
                repository_index,
                worktree_index,
                depth,
                expanded,
                has_children,
                id: RowId::Worktree(
                    self.repositories[repository_index].worktrees[worktree_index]
                        .path
                        .clone(),
                ),
            }),
            BranchSource::VirtualPullRequest {
                virtual_repository_index,
                pull_request_index,
            } => rows.push(VisibleRow::VirtualPullRequest {
                virtual_repository_index,
                pull_request_index,
                mapped_repository_index,
                depth,
                expanded,
                has_children,
                id: RowId::VirtualPullRequest(
                    self.virtual_repositories[virtual_repository_index].pull_requests
                        [pull_request_index]
                        .identity
                        .clone(),
                ),
            }),
        }
        if expanded {
            rows.extend(children);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn append_branch_contents(
        &self,
        rows: &mut Vec<VisibleRow>,
        forest: &BranchForest,
        included: &BTreeSet<usize>,
        index: usize,
        depth: usize,
        mapped_repository_index: Option<usize>,
        flattened_worktree_index: Option<usize>,
    ) {
        let node = &forest.nodes[index];
        match node.source {
            BranchSource::Worktree {
                repository_index,
                worktree_index,
            } => self.append_local_pull_request_rows(rows, repository_index, worktree_index, depth),
            BranchSource::VirtualPullRequest {
                virtual_repository_index,
                pull_request_index,
            } => self.append_virtual_pull_request_inline_rows(
                rows,
                virtual_repository_index,
                pull_request_index,
                depth,
            ),
        }
        let children = self.sorted_branch_indexes(
            forest,
            node.children
                .iter()
                .copied()
                .filter(|child| included.contains(child)),
        );
        if children.is_empty() {
            return;
        }
        let section = InlineSection::StackedBranches;
        let stack_expanded =
            self.disclosure_expanded(&DisclosureKey::Section(node.id.clone(), section), true);
        let (has_local, has_virtual) = self.descendant_kinds(forest, included, &children);
        let label = match (has_local, has_virtual) {
            (true, false) => "Stacked worktrees",
            (false, true) => "Stacked PRs",
            (true, true) => "Stacked branches",
            (false, false) => return,
        };
        self.push_inline_row(
            rows,
            node.id.clone(),
            section,
            depth,
            InlineRowKind::Section,
            label.to_owned(),
            None,
            Some(stack_expanded),
            RowId::Section(node.id.clone(), section),
        );
        if stack_expanded {
            for child in children {
                self.append_branch(
                    rows,
                    forest,
                    included,
                    child,
                    depth + 1,
                    mapped_repository_index,
                    flattened_worktree_index,
                );
            }
        }
    }

    fn sorted_branch_indexes(
        &self,
        forest: &BranchForest,
        indexes: impl Iterator<Item = usize>,
    ) -> Vec<usize> {
        let mut indexes = indexes.collect::<Vec<_>>();
        indexes.sort_by(|left, right| {
            alphabetical_cmp(
                &self.branch_sort_label(&forest.nodes[*left]),
                &self.branch_sort_label(&forest.nodes[*right]),
            )
            .then_with(|| left.cmp(right))
        });
        indexes
    }

    fn branch_sort_label(&self, node: &BranchNode) -> String {
        match node.source {
            BranchSource::Worktree {
                repository_index,
                worktree_index,
            } => {
                let worktree = &self.repositories[repository_index].worktrees[worktree_index];
                worktree
                    .branch
                    .as_deref()
                    .and_then(|branch| branch.strip_prefix("refs/heads/"))
                    .map(str::to_owned)
                    .or_else(|| {
                        worktree.head.as_ref().map(|head| {
                            let short = head.get(..head.len().min(8)).unwrap_or(head);
                            format!("detached:{short}")
                        })
                    })
                    .unwrap_or_else(|| "unknown".to_owned())
            }
            BranchSource::VirtualPullRequest {
                virtual_repository_index,
                pull_request_index,
            } => self.virtual_repositories[virtual_repository_index].pull_requests
                [pull_request_index]
                .pull_request
                .head
                .branch
                .clone(),
        }
    }

    fn descendant_kinds(
        &self,
        forest: &BranchForest,
        included: &BTreeSet<usize>,
        roots: &[usize],
    ) -> (bool, bool) {
        let mut has_local = false;
        let mut has_virtual = false;
        let mut pending = roots.to_vec();
        while let Some(index) = pending.pop() {
            match forest.nodes[index].source {
                BranchSource::Worktree { .. } => has_local = true,
                BranchSource::VirtualPullRequest { .. } => has_virtual = true,
            }
            pending.extend(
                forest.nodes[index]
                    .children
                    .iter()
                    .copied()
                    .filter(|child| included.contains(child)),
            );
        }
        (has_local, has_virtual)
    }

    fn append_backburner(
        &self,
        rows: &mut Vec<VisibleRow>,
        forest: &BranchForest,
        included: &BTreeSet<usize>,
        identity: GitHubRepositoryIdentity,
        depth: usize,
        mapped_repository_index: Option<usize>,
    ) {
        let expanded =
            self.disclosure_expanded(&DisclosureKey::Backburner(identity.clone()), false);
        rows.push(VisibleRow::Backburner {
            identity: identity.clone(),
            depth,
            expanded,
            id: RowId::Backburner(identity),
        });
        if expanded {
            self.append_branch_roots(rows, forest, included, depth + 1, mapped_repository_index);
        }
    }

    pub fn selected_row(&self) -> Option<VisibleRow> {
        let selected = self.selected.as_ref()?;
        self.visible_rows()
            .into_iter()
            .find(|row| row.id() == selected)
    }

    pub(crate) fn selected_row_in<'a>(&self, rows: &'a [VisibleRow]) -> Option<&'a VisibleRow> {
        let selected = self.selected.as_ref()?;
        rows.iter().find(|row| row.id() == selected)
    }

    pub fn selected_worktree(&self) -> Option<(&RepositoryView, &Worktree, usize)> {
        let selected = self.selected.as_ref()?;
        if let RowId::Repository(path) = selected {
            let repository = self
                .repositories
                .iter()
                .find(|repository| repository.config.path == *path)?;
            let (index, worktree) = repository.singleton_worktree()?;
            return Some((repository, worktree, index));
        }
        let BranchId::Worktree(path) = self.branch_for_row_id(selected)? else {
            return None;
        };
        self.repositories.iter().find_map(|repository| {
            repository
                .worktrees
                .iter()
                .position(|worktree| worktree.path == path)
                .map(|index| (repository, &repository.worktrees[index], index))
        })
    }

    pub fn selected_repository(&self) -> Option<(&RepositoryView, usize)> {
        let selected = self.selected.as_ref()?;
        let index = if let RowId::Repository(path) = selected {
            self.repositories
                .iter()
                .position(|repository| repository.config.path == *path)?
        } else {
            let BranchId::Worktree(path) = self.branch_for_row_id(selected)? else {
                return None;
            };
            self.repositories.iter().position(|repository| {
                repository
                    .worktrees
                    .iter()
                    .any(|worktree| worktree.path == path)
            })?
        };
        Some((&self.repositories[index], index))
    }

    pub fn selected_virtual_pull_request(
        &self,
    ) -> Option<(&VirtualRepositoryView, &AuthoredPullRequest)> {
        let BranchId::VirtualPullRequest(identity) =
            self.branch_for_row_id(self.selected.as_ref()?)?
        else {
            return None;
        };
        self.virtual_repositories.iter().find_map(|repository| {
            repository
                .pull_requests
                .iter()
                .find(|pull_request| pull_request.identity == identity)
                .map(|pull_request| (repository, pull_request))
        })
    }

    fn append_local_pull_request_rows(
        &self,
        rows: &mut Vec<VisibleRow>,
        repository_index: usize,
        worktree_index: usize,
        depth: usize,
    ) {
        let repository = &self.repositories[repository_index];
        let worktree = &repository.worktrees[worktree_index];
        let owner = BranchId::Worktree(worktree.path.clone());
        let Some(github_state) = self.github.get(&worktree.path) else {
            return;
        };
        let Some(data) = github_state.data() else {
            return;
        };
        let Some(pull_request) = data.pull_request.as_ref() else {
            return;
        };
        let Some(identity) = self.pull_request_identity(repository, pull_request) else {
            return;
        };
        let mut context = vec![
            (
                "repository".to_owned(),
                format!("repository: {}", repository.config.display_label()),
            ),
            (
                "local-path".to_owned(),
                format!("local path: {}", worktree.path.display()),
            ),
        ];
        match github_state {
            GitHubState::Loading { .. } => {
                context.push(("github".to_owned(), "GitHub: refreshing".to_owned()))
            }
            GitHubState::Stale { error, .. } => {
                context.push(("github-stale".to_owned(), format!("GitHub stale: {error}")))
            }
            GitHubState::Ready(_) => {}
        }
        context.extend(
            data.warnings.iter().enumerate().map(|(index, warning)| {
                (format!("warning-{index}"), format!("warning: {warning}"))
            }),
        );
        self.append_pull_request_inline_rows(
            rows,
            owner,
            depth,
            &identity,
            pull_request,
            self.pull_request_details.get(&identity),
            context,
        );
    }

    fn append_virtual_pull_request_inline_rows(
        &self,
        rows: &mut Vec<VisibleRow>,
        virtual_repository_index: usize,
        pull_request_index: usize,
        depth: usize,
    ) {
        let repository = &self.virtual_repositories[virtual_repository_index];
        let authored = &repository.pull_requests[pull_request_index];
        let owner = BranchId::VirtualPullRequest(authored.identity.clone());
        let mut context = vec![
            (
                "repository".to_owned(),
                format!("repository: {}", repository.identity.full_name()),
            ),
            (
                "local-repo".to_owned(),
                format!(
                    "local repo: {}",
                    repository
                        .mapped_repository
                        .as_ref()
                        .map(|path| path.display().to_string())
                        .unwrap_or_else(|| "none (virtual)".to_owned())
                ),
            ),
            ("author".to_owned(), format!("author: {}", authored.author)),
        ];
        if self.authored_pull_requests.loading {
            context.push(("github".to_owned(), "GitHub: refreshing".to_owned()));
        } else if let Some(error) = &self.authored_pull_requests.stale_error {
            context.push(("github-stale".to_owned(), format!("GitHub stale: {error}")));
        }
        self.append_pull_request_inline_rows(
            rows,
            owner,
            depth,
            &authored.identity,
            &authored.pull_request,
            self.pull_request_details.get(&authored.identity),
            context,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn append_pull_request_inline_rows(
        &self,
        rows: &mut Vec<VisibleRow>,
        owner: BranchId,
        depth: usize,
        identity: &CanonicalPullRequestId,
        pull_request: &PullRequest,
        details: Option<&PullRequestDetails>,
        mut context: Vec<(String, String)>,
    ) {
        let pr_url = pull_request.url.clone();
        let checks_url = format!("{}/checks", pr_url.trim_end_matches('/'));
        if pull_request.state != crate::model::PullRequestState::Merged {
            context.extend([
                ("url".to_owned(), format!("URL: {}", pull_request.url)),
                (
                    "base".to_owned(),
                    format!(
                        "base: {}:{}",
                        pull_request.base.repository.as_deref().unwrap_or("unknown"),
                        pull_request.base.branch
                    ),
                ),
                (
                    "head".to_owned(),
                    format!(
                        "head: {}:{}",
                        pull_request.head.repository.as_deref().unwrap_or("unknown"),
                        pull_request.head.branch
                    ),
                ),
                (
                    "head-sha".to_owned(),
                    format!(
                        "head SHA: {}",
                        pull_request.head.oid.as_deref().unwrap_or("unknown")
                    ),
                ),
                (
                    "state".to_owned(),
                    format!(
                        "state: {} · updated {}",
                        pull_request.state, pull_request.updated_at
                    ),
                ),
                (
                    "auto-merge".to_owned(),
                    format!(
                        "auto-merge: {}",
                        if pull_request.auto_merge {
                            "enabled"
                        } else {
                            "off"
                        }
                    ),
                ),
            ]);
            if let Some(error) = self.pull_request_detail_errors.get(identity) {
                context.push((
                    "details-stale".to_owned(),
                    format!("details stale: {error}"),
                ));
            }
            if let Some(details) = details {
                context.push((
                    "conflict".to_owned(),
                    format!("conflict: {}", debug_label(details.merge_conflict)),
                ));
                context.extend(details.warnings.iter().enumerate().map(|(index, warning)| {
                    (
                        format!("detail-warning-{index}"),
                        format!("warning: {warning}"),
                    )
                }));
            } else {
                context.push((
                    "attention-details".to_owned(),
                    "attention details: loading or unavailable".to_owned(),
                ));
            }

            let summary = details.map(PullRequestDetails::attention_summary);
            let overview_expanded = self.inline_section_expanded(&owner, InlineSection::Overview);
            self.push_inline_row(
                rows,
                owner.clone(),
                InlineSection::Overview,
                depth,
                InlineRowKind::Section,
                format!(
                    "Overview · {} · auto-merge {} · conflicts {}",
                    pull_request.state,
                    if pull_request.auto_merge {
                        "enabled"
                    } else {
                        "off"
                    },
                    summary
                        .map(|summary| debug_label(summary.merge_conflict))
                        .unwrap_or_else(|| "unknown".to_owned()),
                ),
                Some(pr_url.clone()),
                Some(overview_expanded),
                RowId::Section(owner.clone(), InlineSection::Overview),
            );
            if overview_expanded {
                for (key, text) in context {
                    self.push_inline_row(
                        rows,
                        owner.clone(),
                        InlineSection::Overview,
                        depth + 1,
                        InlineRowKind::Metadata,
                        text,
                        Some(pr_url.clone()),
                        None,
                        RowId::Metadata(owner.clone(), format!("overview-{key}")),
                    );
                }
            }

            let mut checks = details
                .map(|details| details.checks.clone())
                .unwrap_or_default();
            checks.sort_by_key(|check| (check_attention_rank(check.state), check.source_order));
            let has_failures = checks.iter().any(|check| check.state.is_actionable());
            let checks_expanded = self.disclosure_expanded(
                &DisclosureKey::Section(owner.clone(), InlineSection::Checks),
                has_failures,
            );
            let checks_header = details.map_or_else(
                || "Checks · counts:?:?:?".to_owned(),
                |details| {
                    let valid = details
                        .checks
                        .iter()
                        .filter(|check| {
                            matches!(
                                check.state,
                                CheckState::Success | CheckState::Neutral | CheckState::Skipped
                            )
                        })
                        .count();
                    let optional_failing = details
                        .checks
                        .iter()
                        .filter(|check| !check.required && check.state.is_actionable())
                        .count();
                    let failing = details
                        .checks
                        .iter()
                        .filter(|check| check.required && check.state.is_actionable())
                        .count();
                    format!("Checks · counts:{valid}:{optional_failing}:{failing}")
                },
            );
            self.push_inline_row(
                rows,
                owner.clone(),
                InlineSection::Checks,
                depth,
                InlineRowKind::Section,
                checks_header,
                Some(checks_url),
                Some(checks_expanded),
                RowId::Section(owner.clone(), InlineSection::Checks),
            );
            if checks_expanded {
                for check in checks.iter().filter(|check| {
                    matches!(
                        check.state,
                        CheckState::Failure | CheckState::Error | CheckState::Unknown
                    )
                }) {
                    let target_url = check.target_url.clone().unwrap_or_else(|| pr_url.clone());
                    let text = format!(
                        "{} · {} · {}",
                        check.name,
                        debug_label(check.state),
                        if check.required {
                            "required"
                        } else {
                            "optional"
                        }
                    );
                    self.push_inline_row(
                        rows,
                        owner.clone(),
                        InlineSection::Checks,
                        depth + 1,
                        InlineRowKind::Check,
                        text,
                        Some(target_url),
                        None,
                        RowId::Check(identity.clone(), check.name.clone()),
                    );
                }

                for (section, label, states) in [
                    (
                        InlineSection::PendingChecks,
                        "Pending",
                        &[CheckState::Pending, CheckState::Expected][..],
                    ),
                    (
                        InlineSection::ValidResults,
                        "Valid Results",
                        &[
                            CheckState::Success,
                            CheckState::Neutral,
                            CheckState::Skipped,
                        ][..],
                    ),
                ] {
                    let grouped = checks
                        .iter()
                        .filter(|check| states.contains(&check.state))
                        .collect::<Vec<_>>();
                    if grouped.is_empty() {
                        continue;
                    }
                    let group_expanded = self.inline_section_expanded(&owner, section);
                    self.push_inline_row(
                        rows,
                        owner.clone(),
                        section,
                        depth + 1,
                        InlineRowKind::Section,
                        format!("{label} · {}", grouped.len()),
                        Some(pr_url.clone()),
                        Some(group_expanded),
                        RowId::Section(owner.clone(), section),
                    );
                    if group_expanded {
                        for check in grouped {
                            let target_url =
                                check.target_url.clone().unwrap_or_else(|| pr_url.clone());
                            self.push_inline_row(
                                rows,
                                owner.clone(),
                                section,
                                depth + 2,
                                InlineRowKind::Check,
                                format!(
                                    "{} · {} · {}",
                                    check.name,
                                    debug_label(check.state),
                                    if check.required {
                                        "required"
                                    } else {
                                        "optional"
                                    }
                                ),
                                Some(target_url),
                                None,
                                RowId::Check(identity.clone(), check.name.clone()),
                            );
                        }
                    }
                }
            }

            let reviewers = details
                .map(PullRequestDetails::reviewers)
                .unwrap_or_default();
            let reviews_incomplete = details.is_none_or(|details| !details.reviews_complete);
            if !reviewers.is_empty() || reviews_incomplete {
                let reviewers_expanded = self.disclosure_expanded(
                    &DisclosureKey::Section(owner.clone(), InlineSection::Reviewers),
                    false,
                );
                let reviewer_tokens = details
                    .map(PullRequestDetails::reviewer_summary)
                    .unwrap_or_default();
                let reviewer_header = if reviewer_tokens.is_empty() {
                    "Reviewers · ○ unknown".to_owned()
                } else {
                    format!(
                        "Reviewers · {}",
                        reviewer_tokens
                            .iter()
                            .map(|token| token.label())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                };
                self.push_inline_row(
                    rows,
                    owner.clone(),
                    InlineSection::Reviewers,
                    depth,
                    InlineRowKind::Section,
                    reviewer_header,
                    Some(pr_url.clone()),
                    Some(reviewers_expanded),
                    RowId::Section(owner.clone(), InlineSection::Reviewers),
                );
                if reviewers_expanded {
                    for reviewer in reviewers {
                        let state = reviewer.state.unwrap_or(SubmittedReviewState::Unknown);
                        self.push_inline_row(
                            rows,
                            owner.clone(),
                            InlineSection::Reviewers,
                            depth + 1,
                            InlineRowKind::Reviewer,
                            format!(
                                "{} · {} · {}",
                                reviewer.name,
                                debug_label(state),
                                if reviewer.requested {
                                    "requested"
                                } else {
                                    "reviewed"
                                }
                            ),
                            Some(pr_url.clone()),
                            None,
                            RowId::Reviewer(identity.clone(), reviewer.identity.clone()),
                        );
                    }
                }
            }
        }

        let open_comments: Vec<_> = details
            .map(|details| details.unresolved_feedback().cloned().collect())
            .unwrap_or_default();
        if !open_comments.is_empty() {
            let comments_expanded =
                self.inline_section_expanded(&owner, InlineSection::OpenComments);
            self.push_inline_row(
                rows,
                owner.clone(),
                InlineSection::OpenComments,
                depth,
                InlineRowKind::Section,
                format!("Open comments · {} unresolved", open_comments.len()),
                Some(pr_url.clone()),
                Some(comments_expanded),
                RowId::Section(owner.clone(), InlineSection::OpenComments),
            );
            if comments_expanded {
                for feedback in open_comments {
                    let body = concise_comment_text(&feedback.body);
                    let mut text = format!("@{} {body}", feedback.author);
                    if let Some(path) = feedback.path {
                        text.push_str(&format!(" ({path})"));
                    }
                    if feedback.outdated {
                        text.push_str(" [outdated]");
                    }
                    let url = feedback.permalink.unwrap_or_else(|| pr_url.clone());
                    self.push_inline_row(
                        rows,
                        owner.clone(),
                        InlineSection::OpenComments,
                        depth + 1,
                        InlineRowKind::OpenComment,
                        text,
                        Some(url),
                        None,
                        RowId::OpenComment(identity.clone(), feedback.id),
                    );
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn push_inline_row(
        &self,
        rows: &mut Vec<VisibleRow>,
        owner: BranchId,
        section: InlineSection,
        depth: usize,
        kind: InlineRowKind,
        text: String,
        url: Option<String>,
        expanded: Option<bool>,
        id: RowId,
    ) {
        rows.push(VisibleRow::Inline {
            owner,
            section,
            depth,
            kind,
            text,
            url,
            expanded,
            id,
        });
    }

    fn inline_section_expanded(&self, owner: &BranchId, section: InlineSection) -> bool {
        self.disclosure_expanded(
            &DisclosureKey::Section(owner.clone(), section),
            section == InlineSection::OpenComments || section == InlineSection::StackedBranches,
        )
    }

    fn disclosure_expanded(&self, key: &DisclosureKey, default: bool) -> bool {
        if self.filter_mode() {
            true
        } else {
            self.disclosure_expanded
                .get(key)
                .copied()
                .unwrap_or(default)
        }
    }

    fn set_disclosure_expanded(&mut self, key: DisclosureKey, expanded: bool) {
        if self.filter_mode() {
            if expanded {
                self.filter_collapsed.remove(&key);
                self.filter_expanded.insert(key);
            } else {
                self.filter_expanded.remove(&key);
                self.filter_collapsed.insert(key);
            }
        } else {
            self.disclosure_expanded.insert(key, expanded);
        }
    }

    fn displayed_disclosure_expanded(&self, key: &DisclosureKey, default: bool) -> bool {
        if self.filter_mode() {
            self.search_disclosure_expanded(key, self.search_requires_expansion(key))
        } else {
            self.disclosure_expanded
                .get(key)
                .copied()
                .unwrap_or(default)
        }
    }

    fn search_disclosure_expanded(&self, key: &DisclosureKey, required: bool) -> bool {
        if self.filter_collapsed.contains(key) {
            false
        } else if self.filter_expanded.contains(key) || required {
            true
        } else {
            self.saved_disclosure_expanded(key)
        }
    }

    fn saved_disclosure_expanded(&self, key: &DisclosureKey) -> bool {
        self.disclosure_expanded
            .get(key)
            .copied()
            .unwrap_or_else(|| match key {
                DisclosureKey::Repository(path) => self
                    .repositories
                    .iter()
                    .find(|repository| repository.config.path == *path)
                    .is_some_and(|repository| repository.expanded),
                DisclosureKey::VirtualRepository(identity) => self
                    .virtual_repositories
                    .iter()
                    .find(|repository| repository.identity == *identity)
                    .is_some_and(|repository| repository.expanded),
                DisclosureKey::Backburner(_) => false,
                DisclosureKey::Branch(_) => true,
                DisclosureKey::Section(_, section) => matches!(
                    section,
                    InlineSection::OpenComments | InlineSection::StackedBranches
                ),
            })
    }

    fn search_requires_expansion(&self, key: &DisclosureKey) -> bool {
        if self.filter.is_empty() {
            return false;
        }
        let Ok(query) = RegexBuilder::new(&self.filter)
            .case_insensitive(true)
            .build()
        else {
            return false;
        };
        let rows = self.scoped_logical_rows();
        let mut ancestors = Vec::<usize>::new();
        for (index, row) in rows.iter().enumerate() {
            let depth = self.visible_row_depth(row);
            ancestors.truncate(depth);
            if self.row_matches_filter(row, &query)
                && ancestors.iter().any(|ancestor| {
                    self.disclosure_key_for_row(&rows[*ancestor]).as_ref() == Some(key)
                })
            {
                return true;
            }
            ancestors.push(index);
        }
        false
    }

    pub fn virtual_repository_expanded(&self, index: usize) -> bool {
        let repository = &self.virtual_repositories[index];
        self.displayed_disclosure_expanded(
            &DisclosureKey::VirtualRepository(repository.identity.clone()),
            repository.expanded,
        )
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

    pub fn is_focused(&self) -> bool {
        self.focus_target.is_some()
    }

    pub fn focus_label(&self) -> Option<String> {
        match self.focus_target.as_ref()? {
            FocusTarget::Repository(path) => self
                .repositories
                .iter()
                .find(|repository| repository.config.path == *path)
                .map(|repository| repository.config.display_label()),
            FocusTarget::VirtualRepository(identity) => Some(identity.full_name()),
            FocusTarget::Backburner(identity) => {
                Some(format!("{} / Backburner", identity.full_name()))
            }
            FocusTarget::Branch(branch) => {
                let (forest, index, mapped_repository_index) = self.branch_scope_context(branch)?;
                let owner = mapped_repository_index
                    .map(|repository_index| {
                        self.repositories[repository_index].config.display_label()
                    })
                    .or_else(|| {
                        forest.nodes[index]
                            .identity
                            .as_ref()
                            .map(|identity| identity.repository.full_name())
                    })?;
                Some(format!(
                    "{owner}: {}",
                    self.branch_sort_label(&forest.nodes[index])
                ))
            }
        }
    }

    fn toggle_focus(&mut self) {
        if self.focus_target.take().is_some() {
            self.scroll = 0;
            self.ensure_selection_visible();
            return;
        }
        let Some(selected) = self.selected.as_ref() else {
            return;
        };
        self.focus_target = match selected {
            RowId::Repository(path) => Some(FocusTarget::Repository(path.clone())),
            RowId::VirtualRepository(identity) => {
                Some(FocusTarget::VirtualRepository(identity.clone()))
            }
            RowId::Backburner(identity) => Some(FocusTarget::Backburner(identity.clone())),
            _ => self.branch_for_row_id(selected).map(FocusTarget::Branch),
        };
        self.scroll = 0;
        self.ensure_selection_visible();
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
                    self.move_selection((self.viewport_height / 2).max(1) as isize);
                    Intent::None
                }
                KeyCode::Char('u') => {
                    self.move_selection(-((self.viewport_height / 2).max(1) as isize));
                    Intent::None
                }
                _ => Intent::None,
            };
        }
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => self.move_and_continue(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_and_continue(-1),
            KeyCode::Char('g') => {
                self.select_index(0);
                Intent::None
            }
            KeyCode::Char('G') => {
                let length = self.visible_rows().len();
                if length > 0 {
                    self.select_index(length - 1);
                }
                Intent::None
            }
            KeyCode::Char('h') | KeyCode::Left => {
                self.collapse_or_focus_list();
                Intent::None
            }
            KeyCode::Char('l') | KeyCode::Right => {
                self.expand_selected();
                Intent::None
            }
            KeyCode::Char('/') => {
                self.filter_active = true;
                self.filter.clear();
                self.filter_collapsed.clear();
                self.filter_expanded.clear();
                self.scroll = 0;
                self.ensure_selection_visible();
                Intent::None
            }
            KeyCode::Char('n') if !self.filter.is_empty() => {
                self.navigate_search_matches(true);
                Intent::None
            }
            KeyCode::Char('N') if !self.filter.is_empty() => {
                self.navigate_search_matches(false);
                Intent::None
            }
            KeyCode::Char('b') => self.toggle_selected_backburner(),
            KeyCode::Char('F') => {
                self.toggle_focus();
                Intent::None
            }
            KeyCode::Char(']') => {
                self.navigate_attention(true);
                Intent::None
            }
            KeyCode::Char('[') => {
                self.navigate_attention(false);
                Intent::None
            }
            KeyCode::Char('r') => self.request_refresh(),
            KeyCode::Char('?') | KeyCode::Char(' ') => {
                self.modal = Some(Modal::Palette { selected: 0 });
                Intent::None
            }
            KeyCode::Char('q') => Intent::Cancel,
            KeyCode::Char('w') => self.open_selected_url(),
            KeyCode::Char(character) => self.direct_action(character),
            KeyCode::Enter => self.accept_or_toggle(),
            KeyCode::Esc if !self.filter.is_empty() => {
                self.clear_search_preserving_selection();
                Intent::None
            }
            KeyCode::Esc => Intent::Cancel,
            _ => Intent::None,
        }
    }

    fn handle_filter_key(&mut self, key: KeyEvent) -> Intent {
        match key.code {
            KeyCode::Enter => {
                self.filter_active = false;
                if self.filter.is_empty() {
                    self.filter_collapsed.clear();
                    self.filter_expanded.clear();
                }
            }
            KeyCode::Esc => {
                self.clear_search_preserving_selection();
                return Intent::None;
            }
            KeyCode::Backspace => {
                self.filter.pop();
                self.filter_collapsed.clear();
                self.filter_expanded.clear();
            }
            KeyCode::Char(character)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.filter.push(character);
                self.filter_collapsed.clear();
                self.filter_expanded.clear();
            }
            _ => {}
        }
        self.scroll = 0;
        self.ensure_selection_visible();
        Intent::None
    }

    fn navigate_search_matches(&mut self, forward: bool) {
        let Ok(query) = RegexBuilder::new(&self.filter)
            .case_insensitive(true)
            .build()
        else {
            return;
        };
        let rows = self.visible_rows();
        let matches = rows
            .iter()
            .enumerate()
            .filter_map(|(index, row)| self.row_matches_filter(row, &query).then_some(index))
            .collect::<Vec<_>>();
        if matches.is_empty() {
            return;
        }
        let current = self
            .selected
            .as_ref()
            .and_then(|selected| rows.iter().position(|row| row.id() == selected));
        let next = if forward {
            current
                .and_then(|current| matches.iter().copied().find(|index| *index > current))
                .unwrap_or(matches[0])
        } else {
            current
                .and_then(|current| matches.iter().rev().copied().find(|index| *index < current))
                .unwrap_or_else(|| *matches.last().unwrap())
        };
        self.selected = Some(rows[next].id().clone());
        self.ensure_selected_in_view_with_rows(&rows);
    }

    fn clear_search_preserving_selection(&mut self) {
        if let Some(selected) = self.selected.as_ref() {
            let rows = self.scoped_logical_rows();
            if let Some(selected_index) = rows.iter().position(|row| row.id() == selected) {
                let mut ancestors = Vec::new();
                for row in &rows[..=selected_index] {
                    let depth = self.visible_row_depth(row);
                    ancestors.truncate(depth);
                    if row.id() == selected {
                        break;
                    }
                    ancestors.push(row);
                }
                let keys = ancestors
                    .into_iter()
                    .filter_map(|row| self.disclosure_key_for_row(row))
                    .collect::<Vec<_>>();
                for key in keys {
                    self.disclosure_expanded.insert(key, true);
                }
            }
        }
        self.filter_active = false;
        self.filter.clear();
        self.filter_collapsed.clear();
        self.filter_expanded.clear();
        self.scroll = 0;
        self.ensure_selection_visible();
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
        if matches!(action, Action::CopyAgentPrompt | Action::CopyReviewRequest) {
            return ActionAvailability {
                action,
                enabled: true,
                reason: None,
            };
        }
        if action == Action::OpenPullRequestWeb {
            return if self.selected_pull_request_url().is_some() {
                ActionAvailability {
                    action,
                    enabled: true,
                    reason: None,
                }
            } else {
                disabled("selected branch has no associated pull request")
            };
        }
        let selected_is_virtual = match self.selected.as_ref() {
            Some(RowId::VirtualRepository(_) | RowId::VirtualPullRequest(_)) => true,
            Some(selected) => matches!(
                self.branch_for_row_id(selected),
                Some(BranchId::VirtualPullRequest(_))
            ),
            None => false,
        };
        if selected_is_virtual {
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
            Action::CopyReviewRequest => {
                unreachable!("handled before selection validation")
            }
            Action::OpenPullRequestWeb => unreachable!("handled before selection validation"),
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
                            if self.is_current_worktree(&worktree.path) {
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
            self.status_progress_visible = true;
            self.progress = Some("refresh queued".to_owned());
            Intent::RefreshGitHub
        } else {
            Intent::Refresh
        }
    }

    pub fn begin_status_refresh(&mut self, paths: &[PathBuf], show_progress: bool) -> u64 {
        self.generation = self.generation.wrapping_add(1);
        self.pending_status = paths.len();
        self.status_progress_visible = show_progress && !paths.is_empty();
        if show_progress {
            self.progress =
                (!paths.is_empty()).then(|| format!("loading status: 0/{}", paths.len()));
        }
        for path in paths {
            self.statuses
                .entry(path.clone())
                .or_insert(StatusState::Pending);
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
            if self.status_progress_visible {
                self.progress = None;
            }
            self.status_progress_visible = false;
            return std::mem::take(&mut self.refresh_queued);
        }
        if self.status_progress_visible {
            self.progress = Some(format!("loading status: {} remaining", self.pending_status));
        }
        false
    }

    pub fn begin_github_refresh(&mut self, paths: &[PathBuf]) -> u64 {
        self.github_generation = self.github_generation.wrapping_add(1);
        self.github_loading = !paths.is_empty();
        self.github_network_paths = paths.iter().cloned().collect();
        self.github_spinner_frame = 0;
        for path in paths {
            let previous = self.github.get(path).and_then(GitHubState::data).cloned();
            self.github
                .insert(path.clone(), GitHubState::Loading { previous });
        }
        self.github_generation
    }

    pub fn advance_github_spinner(&mut self) {
        self.github_spinner_frame = self.github_spinner_frame.wrapping_add(1);
    }

    pub fn github_spinner_frame(&self) -> usize {
        self.github_spinner_frame
    }

    pub fn github_network_active(&self, path: &Path) -> bool {
        self.github_network_paths.contains(path)
    }

    pub fn has_github_network_activity(&self) -> bool {
        !self.github_network_paths.is_empty()
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
        let detail_paths = paths
            .iter()
            .filter(|path| {
                results
                    .get(*path)
                    .and_then(|result| result.as_ref().ok())
                    .is_some_and(|data| data.pull_request.is_some())
            })
            .cloned()
            .collect();
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
        // Branch lookup is complete, but PR detail hydration is another network
        // request. Keep those worktrees visibly busy until its result arrives.
        self.github_network_paths = detail_paths;
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
        self.github_network_paths.clear();
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
        self.ensure_selection_visible();
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
        self.refresh_current_worktree();
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
        let mappings: HashMap<CanonicalPullRequestId, Option<PathBuf>> = self
            .authored_mappings
            .iter()
            .map(|mapping| (mapping.identity.clone(), mapping.mapped_repository.clone()))
            .collect();
        let mut grouped: BTreeMap<GitHubRepositoryIdentity, VirtualRepositoryView> =
            BTreeMap::new();
        for pull_request in self.authored_pull_requests.visible() {
            let Some(mapped_path) = mappings.get(&pull_request.identity) else {
                continue;
            };
            let mapped_repository = mapped_path
                .as_ref()
                .filter(|path| {
                    self.repositories
                        .iter()
                        .any(|repository| repository.config.path == **path)
                })
                .cloned();
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

        self.reconcile_focus_target();
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
        }
        self.ensure_selected_in_view();
    }

    #[cfg(test)]
    pub fn set_viewport_height(&mut self, height: usize) {
        let rows = self.visible_rows();
        self.set_viewport_height_with_rows(height, &rows);
    }

    pub(crate) fn set_viewport_height_with_rows(&mut self, height: usize, rows: &[VisibleRow]) {
        self.viewport_height = height.max(1);
        if self.viewport_initialized {
            self.ensure_selected_in_view_with_rows(rows);
        } else {
            self.viewport_initialized = true;
            self.ensure_initial_selection_in_view_with_rows(rows);
        }
    }

    fn open_selected_url(&mut self) -> Intent {
        match self.selected_row() {
            Some(VisibleRow::Inline { url: Some(url), .. }) => Intent::OpenUrl(url),
            _ => self.direct_action('w'),
        }
    }

    pub fn selected_pull_request_url(&self) -> Option<String> {
        if let Some((_, authored)) = self.selected_virtual_pull_request() {
            return Some(authored.pull_request.url.clone());
        }
        let (_, worktree, _) = self.selected_worktree()?;
        self.github
            .get(&worktree.path)
            .and_then(GitHubState::data)
            .and_then(|data| data.pull_request.as_ref())
            .map(|pull_request| pull_request.url.clone())
    }

    pub fn agent_prompt(&self) -> Option<String> {
        let all = self.prompt_pull_requests();
        let scoped = match self.selected.as_ref() {
            Some(RowId::Check(identity, name)) => all
                .get(identity)
                .map(|pull_request| PromptPullRequest {
                    identity: identity.clone(),
                    pull_request: pull_request.pull_request.clone(),
                    checks: self
                        .pull_request_details
                        .get(identity)
                        .into_iter()
                        .flat_map(|details| &details.checks)
                        .filter(|check| check.name.eq_ignore_ascii_case(name))
                        .cloned()
                        .collect(),
                    feedback: Vec::new(),
                    worktree: pull_request.worktree.clone(),
                })
                .into_iter()
                .collect(),
            Some(RowId::OpenComment(identity, id)) => all
                .get(identity)
                .map(|pull_request| PromptPullRequest {
                    identity: identity.clone(),
                    pull_request: pull_request.pull_request.clone(),
                    checks: Vec::new(),
                    feedback: self
                        .unresolved_prompt_feedback(identity)
                        .into_iter()
                        .filter(|feedback| feedback.id == *id)
                        .collect(),
                    worktree: pull_request.worktree.clone(),
                })
                .into_iter()
                .collect(),
            Some(RowId::Reviewer(identity, reviewer)) => all
                .get(identity)
                .map(|pull_request| PromptPullRequest {
                    identity: identity.clone(),
                    pull_request: pull_request.pull_request.clone(),
                    checks: Vec::new(),
                    feedback: pull_request
                        .feedback
                        .iter()
                        .filter(|feedback| {
                            feedback.kind == crate::model::FeedbackKind::ReviewSummary
                                && feedback.author.eq_ignore_ascii_case(reviewer)
                        })
                        .cloned()
                        .collect(),
                    worktree: pull_request.worktree.clone(),
                })
                .into_iter()
                .collect(),
            Some(RowId::Section(
                owner,
                section @ (InlineSection::Checks
                | InlineSection::Reviewers
                | InlineSection::OpenComments),
            )) => self
                .pull_request_identity_for_branch(owner)
                .and_then(|identity| {
                    all.get(&identity)
                        .map(|pull_request| (identity, pull_request))
                })
                .map(|(identity, pull_request)| PromptPullRequest {
                    identity: identity.clone(),
                    pull_request: pull_request.pull_request.clone(),
                    checks: if *section == InlineSection::Checks {
                        pull_request.checks.clone()
                    } else {
                        Vec::new()
                    },
                    feedback: if matches!(
                        section,
                        InlineSection::Reviewers | InlineSection::OpenComments
                    ) {
                        if *section == InlineSection::OpenComments {
                            self.unresolved_prompt_feedback(&identity)
                        } else {
                            pull_request
                                .feedback
                                .iter()
                                .filter(|feedback| {
                                    feedback.kind == crate::model::FeedbackKind::ReviewSummary
                                })
                                .cloned()
                                .collect()
                        }
                    } else {
                        Vec::new()
                    },
                    worktree: pull_request.worktree.clone(),
                })
                .into_iter()
                .collect(),
            Some(selected) => self
                .structural_scope_identities(selected, false)
                .into_iter()
                .filter_map(|identity| {
                    let mut pull_request = all.get(&identity).cloned()?;
                    pull_request.feedback = self.unresolved_prompt_feedback(&identity);
                    Some(pull_request)
                })
                .collect(),
            None => Vec::new(),
        };
        format_agent_prompt(&scoped)
    }

    pub fn review_request(&self) -> Option<String> {
        let all = self.prompt_pull_requests();
        let selected = self.selected.as_ref()?;
        let scoped = self
            .structural_scope_identities(selected, true)
            .into_iter()
            .filter_map(|identity| all.get(&identity).cloned())
            .collect::<Vec<_>>();
        format_review_request(&scoped)
    }

    pub fn is_backburnered(&self, identity: &CanonicalPullRequestId) -> bool {
        self.backburner.contains(identity)
    }

    fn selected_pull_request_identity(&self) -> Option<CanonicalPullRequestId> {
        match self.selected.as_ref()? {
            RowId::VirtualPullRequest(identity) => Some(identity.clone()),
            RowId::Repository(path) => {
                let repository = self
                    .repositories
                    .iter()
                    .find(|repository| repository.config.path == *path)?;
                let (_, worktree) = repository.singleton_worktree()?;
                let pull_request = self
                    .github
                    .get(&worktree.path)
                    .and_then(GitHubState::data)
                    .and_then(|data| data.pull_request.as_ref())?;
                self.pull_request_identity(repository, pull_request)
            }
            RowId::Worktree(path) => {
                let repository = self.repositories.iter().find(|repository| {
                    repository
                        .worktrees
                        .iter()
                        .any(|worktree| worktree.path == *path)
                })?;
                let pull_request = self
                    .github
                    .get(path)
                    .and_then(GitHubState::data)
                    .and_then(|data| data.pull_request.as_ref())?;
                self.pull_request_identity(repository, pull_request)
            }
            RowId::Section(owner, _) | RowId::Metadata(owner, _) => {
                self.pull_request_identity_for_branch(owner)
            }
            RowId::Check(identity, _)
            | RowId::Reviewer(identity, _)
            | RowId::OpenComment(identity, _) => Some(identity.clone()),
            RowId::VirtualRepository(_) | RowId::Backburner(_) => None,
        }
    }

    fn pull_request_identity_for_branch(&self, owner: &BranchId) -> Option<CanonicalPullRequestId> {
        match owner {
            BranchId::VirtualPullRequest(identity) => Some(identity.clone()),
            BranchId::Worktree(path) => {
                let repository = self.repositories.iter().find(|repository| {
                    repository
                        .worktrees
                        .iter()
                        .any(|worktree| worktree.path == *path)
                })?;
                let pull_request = self
                    .github
                    .get(path)
                    .and_then(GitHubState::data)
                    .and_then(|data| data.pull_request.as_ref())?;
                self.pull_request_identity(repository, pull_request)
            }
        }
    }

    fn toggle_selected_backburner(&mut self) -> Intent {
        let Some(identity) = self.selected_pull_request_identity() else {
            self.inline_error = Some("select a pull request to toggle Backburner".to_owned());
            return Intent::None;
        };
        let identities = self.pull_request_stack_identities(&identity);
        let backburnering = !self.backburner.contains(&identity);
        for identity in &identities {
            if backburnering {
                self.backburner.insert(identity.clone());
            } else {
                self.backburner.remove(identity);
            }
        }
        if backburnering
            && !self.displayed_disclosure_expanded(
                &DisclosureKey::Backburner(identity.repository.clone()),
                false,
            )
        {
            self.selected = Some(RowId::Backburner(identity.repository.clone()));
        }
        self.ensure_selection_visible();
        Intent::PersistBackburner
    }

    fn navigate_attention(&mut self, forward: bool) {
        let all = self.prompt_pull_requests();
        let mut candidates = Vec::<(CanonicalPullRequestId, RowId)>::new();
        let mut seen = BTreeSet::new();
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
                if !self.backburner.contains(&identity)
                    && self.pull_request_is_actionable(&identity)
                    && seen.insert(identity.clone())
                {
                    candidates.push((identity, RowId::Worktree(worktree.path.clone())));
                }
            }
        }
        for repository in &self.virtual_repositories {
            for row in nested_pull_requests(&repository.pull_requests) {
                let identity = &repository.pull_requests[row.index].identity;
                if all.contains_key(identity)
                    && !self.backburner.contains(identity)
                    && self.pull_request_is_actionable(identity)
                    && seen.insert(identity.clone())
                {
                    candidates.push((
                        identity.clone(),
                        RowId::VirtualPullRequest(identity.clone()),
                    ));
                }
            }
        }
        if candidates.is_empty() {
            return;
        }
        let current = candidates
            .iter()
            .position(|(_, row)| self.selected.as_ref() == Some(row));
        let index = if forward {
            current.map_or(0, |index| (index + 1) % candidates.len())
        } else {
            current.map_or(candidates.len() - 1, |index| {
                (index + candidates.len() - 1) % candidates.len()
            })
        };
        let (_, row) = candidates[index].clone();
        if let Some(branch) = self.branch_for_row_id(&row) {
            self.reveal_branch(&branch);
        }
        self.selected = Some(row);
        self.ensure_selection_visible();
    }

    fn reveal_branch(&mut self, branch: &BranchId) {
        let (repository_index, virtual_repository_indexes, repository_key) = match branch {
            BranchId::Worktree(path) => {
                let Some(repository_index) = self.repositories.iter().position(|repository| {
                    repository
                        .worktrees
                        .iter()
                        .any(|worktree| worktree.path == *path)
                }) else {
                    return;
                };
                let repository_path = self.repositories[repository_index].config.path.clone();
                let virtual_indexes = self
                    .virtual_repositories
                    .iter()
                    .enumerate()
                    .filter(|(_, repository)| {
                        repository.mapped_repository.as_ref() == Some(&repository_path)
                    })
                    .map(|(index, _)| index)
                    .collect();
                (
                    Some(repository_index),
                    virtual_indexes,
                    DisclosureKey::Repository(repository_path),
                )
            }
            BranchId::VirtualPullRequest(identity) => {
                let Some(virtual_index) = self.virtual_repositories.iter().position(|repository| {
                    repository
                        .pull_requests
                        .iter()
                        .any(|pull_request| pull_request.identity == *identity)
                }) else {
                    return;
                };
                if let Some(path) = self.virtual_repositories[virtual_index]
                    .mapped_repository
                    .clone()
                {
                    let repository_index = self
                        .repositories
                        .iter()
                        .position(|repository| repository.config.path == path);
                    let virtual_indexes = self
                        .virtual_repositories
                        .iter()
                        .enumerate()
                        .filter(|(_, repository)| {
                            repository.mapped_repository.as_ref() == Some(&path)
                        })
                        .map(|(index, _)| index)
                        .collect();
                    (
                        repository_index,
                        virtual_indexes,
                        DisclosureKey::Repository(path),
                    )
                } else {
                    (
                        None,
                        vec![virtual_index],
                        DisclosureKey::VirtualRepository(
                            self.virtual_repositories[virtual_index].identity.clone(),
                        ),
                    )
                }
            }
        };
        let forest = self.branch_forest(repository_index, &virtual_repository_indexes);
        let Some(mut index) = forest.nodes.iter().position(|node| node.id == *branch) else {
            return;
        };
        self.set_disclosure_expanded(repository_key, true);
        self.set_disclosure_expanded(DisclosureKey::Branch(branch.clone()), true);
        while let Some(parent) = forest.nodes[index].parent {
            let parent_id = forest.nodes[parent].id.clone();
            self.set_disclosure_expanded(DisclosureKey::Branch(parent_id.clone()), true);
            self.set_disclosure_expanded(
                DisclosureKey::Section(parent_id, InlineSection::StackedBranches),
                true,
            );
            index = parent;
        }
    }

    fn pull_request_is_actionable(&self, identity: &CanonicalPullRequestId) -> bool {
        self.pull_request_details
            .get(identity)
            .is_some_and(|details| details.attention_summary().is_actionable())
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

    fn unresolved_prompt_feedback(
        &self,
        identity: &CanonicalPullRequestId,
    ) -> Vec<crate::model::PullRequestFeedback> {
        self.pull_request_details
            .get(identity)
            .into_iter()
            .flat_map(PullRequestDetails::unresolved_feedback)
            .cloned()
            .collect()
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
                .flat_map(|details| &details.feedback)
                .cloned()
                .collect(),
            worktree: self.prompt_worktree(identity, pull_request),
        }
    }

    fn prompt_worktree(
        &self,
        identity: &CanonicalPullRequestId,
        pull_request: &PullRequest,
    ) -> Option<PromptWorktree> {
        let associated = self.repositories.iter().flat_map(|repository| {
            repository.worktrees.iter().filter_map(|worktree| {
                let candidate = self
                    .github
                    .get(&worktree.path)
                    .and_then(GitHubState::data)
                    .and_then(|data| data.pull_request.as_ref())?;
                (self.pull_request_identity(repository, candidate).as_ref() == Some(identity))
                    .then_some(worktree)
            })
        });

        let matching_repository = |repository: &&RepositoryView| {
            repository
                .config
                .github_remotes
                .values()
                .any(|candidate| candidate == &identity.repository)
        };
        let fallback = self
            .repositories
            .iter()
            .filter(matching_repository)
            .flat_map(|repository| &repository.worktrees)
            .filter(|worktree| !worktree.bare)
            .filter(|worktree| {
                worktree.head.as_deref() == pull_request.head.oid.as_deref()
                    || worktree.branch.as_deref().is_some_and(|branch| {
                        branch.strip_prefix("refs/heads/").unwrap_or(branch)
                            == pull_request.head.branch
                    })
            });

        associated
            .chain(fallback)
            .max_by_key(|worktree| {
                (
                    worktree.head.as_deref() == pull_request.head.oid.as_deref(),
                    worktree.branch.as_deref().is_some_and(|branch| {
                        branch.strip_prefix("refs/heads/").unwrap_or(branch)
                            == pull_request.head.branch
                    }),
                )
            })
            .map(|worktree| PromptWorktree {
                path: worktree.path.clone(),
                branch: worktree.branch.clone(),
                head: worktree.head.clone(),
            })
    }

    fn pull_request_stack_identities(
        &self,
        root: &CanonicalPullRequestId,
    ) -> Vec<CanonicalPullRequestId> {
        let all = self.prompt_pull_requests();
        if !all.contains_key(root) {
            return vec![root.clone()];
        }
        let mut included = BTreeSet::from([root.clone()]);
        let mut ordered = vec![root.clone()];
        loop {
            let mut added = false;
            for (identity, candidate) in &all {
                if identity.repository != root.repository || included.contains(identity) {
                    continue;
                }
                if ordered.iter().any(|parent| {
                    all.get(parent).is_some_and(|parent| {
                        pull_request_identity_matches(
                            &parent.pull_request.head,
                            &candidate.pull_request.base,
                        )
                    })
                }) {
                    included.insert(identity.clone());
                    ordered.push(identity.clone());
                    added = true;
                }
            }
            if !added {
                break;
            }
        }
        ordered
    }

    fn structural_scope_identities(
        &self,
        selected: &RowId,
        non_container_owns_pull_request: bool,
    ) -> Vec<CanonicalPullRequestId> {
        match selected {
            RowId::Worktree(path) => {
                self.branch_scope_identities(&BranchId::Worktree(path.clone()), false)
            }
            RowId::VirtualPullRequest(identity) => {
                self.branch_scope_identities(&BranchId::VirtualPullRequest(identity.clone()), false)
            }
            RowId::Repository(path) => self.repository_scope_identities(path),
            RowId::VirtualRepository(repository) => {
                self.virtual_repository_scope_identities(repository, false)
            }
            RowId::Backburner(repository) => {
                self.virtual_repository_scope_identities(repository, true)
            }
            RowId::Section(owner, InlineSection::StackedBranches) => {
                self.branch_scope_identities(owner, true)
            }
            RowId::Section(owner, _) | RowId::Metadata(owner, _)
                if non_container_owns_pull_request =>
            {
                self.pull_request_identity_for_branch(owner)
                    .into_iter()
                    .collect()
            }
            RowId::Check(identity, _)
            | RowId::Reviewer(identity, _)
            | RowId::OpenComment(identity, _)
                if non_container_owns_pull_request =>
            {
                vec![identity.clone()]
            }
            RowId::Section(_, _)
            | RowId::Metadata(_, _)
            | RowId::Check(_, _)
            | RowId::Reviewer(_, _)
            | RowId::OpenComment(_, _) => Vec::new(),
        }
    }

    fn branch_scope_identities(
        &self,
        branch: &BranchId,
        descendants_only: bool,
    ) -> Vec<CanonicalPullRequestId> {
        let Some((forest, index)) = self.branch_scope_forest(branch) else {
            return Vec::new();
        };
        branch_subtree_identity_order(&forest, index, descendants_only)
    }

    fn branch_scope_forest(&self, branch: &BranchId) -> Option<(BranchForest, usize)> {
        self.branch_scope_context(branch)
            .map(|(forest, index, _)| (forest, index))
    }

    fn branch_scope_context(
        &self,
        branch: &BranchId,
    ) -> Option<(BranchForest, usize, Option<usize>)> {
        let (repository_index, virtual_repository_indexes) = match branch {
            BranchId::Worktree(path) => {
                let repository_index = self.repositories.iter().position(|repository| {
                    repository
                        .worktrees
                        .iter()
                        .any(|worktree| worktree.path == *path)
                })?;
                let repository_path = &self.repositories[repository_index].config.path;
                let virtual_indexes = self
                    .virtual_repositories
                    .iter()
                    .enumerate()
                    .filter(|(_, repository)| {
                        repository.mapped_repository.as_ref() == Some(repository_path)
                    })
                    .map(|(index, _)| index)
                    .collect();
                (Some(repository_index), virtual_indexes)
            }
            BranchId::VirtualPullRequest(identity) => {
                let virtual_index = self.virtual_repositories.iter().position(|repository| {
                    repository
                        .pull_requests
                        .iter()
                        .any(|pull_request| pull_request.identity == *identity)
                })?;
                match self.virtual_repositories[virtual_index]
                    .mapped_repository
                    .as_ref()
                {
                    Some(path) => {
                        let repository_index = self
                            .repositories
                            .iter()
                            .position(|repository| repository.config.path == *path);
                        let virtual_indexes = self
                            .virtual_repositories
                            .iter()
                            .enumerate()
                            .filter(|(_, repository)| {
                                repository.mapped_repository.as_ref() == Some(path)
                            })
                            .map(|(index, _)| index)
                            .collect();
                        (repository_index, virtual_indexes)
                    }
                    None => (None, vec![virtual_index]),
                }
            }
        };
        let forest = self.branch_forest(repository_index, &virtual_repository_indexes);
        let index = forest.nodes.iter().position(|node| node.id == *branch)?;
        Some((forest, index, repository_index))
    }

    fn repository_scope_identities(&self, path: &Path) -> Vec<CanonicalPullRequestId> {
        let Some(repository_index) = self
            .repositories
            .iter()
            .position(|repository| repository.config.path == path)
        else {
            return Vec::new();
        };
        let virtual_indexes = self
            .virtual_repositories
            .iter()
            .enumerate()
            .filter(|(_, repository)| repository.mapped_repository.as_deref() == Some(path))
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        let forest = self.branch_forest(Some(repository_index), &virtual_indexes);
        forest_identity_order(&forest)
            .into_iter()
            .filter(|identity| {
                forest
                    .nodes
                    .iter()
                    .find(|node| node.identity.as_ref() == Some(identity))
                    .is_some_and(|node| !node.backburnered)
            })
            .collect()
    }

    fn virtual_repository_scope_identities(
        &self,
        repository: &GitHubRepositoryIdentity,
        backburner_only: bool,
    ) -> Vec<CanonicalPullRequestId> {
        let Some(virtual_index) = self
            .virtual_repositories
            .iter()
            .position(|candidate| candidate.identity == *repository)
        else {
            return Vec::new();
        };
        let (repository_index, virtual_indexes) = self.virtual_repositories[virtual_index]
            .mapped_repository
            .as_ref()
            .map_or((None, vec![virtual_index]), |path| {
                (
                    self.repositories
                        .iter()
                        .position(|candidate| candidate.config.path == *path),
                    self.virtual_repositories
                        .iter()
                        .enumerate()
                        .filter(|(_, candidate)| candidate.mapped_repository.as_ref() == Some(path))
                        .map(|(index, _)| index)
                        .collect(),
                )
            });
        let forest = self.branch_forest(repository_index, &virtual_indexes);
        forest_identity_order(&forest)
            .into_iter()
            .filter(|identity| {
                let Some(index) = forest
                    .nodes
                    .iter()
                    .position(|node| node.identity.as_ref() == Some(identity))
                else {
                    return false;
                };
                if backburner_only {
                    forest.nodes[index].backburnered
                        && forest.nodes[backburner_root_index(&forest, index)]
                            .identity
                            .as_ref()
                            .is_some_and(|root| root.repository == *repository)
                } else {
                    identity.repository == *repository && !forest.nodes[index].backburnered
                }
            })
            .collect()
    }

    fn direct_action(&mut self, character: char) -> Intent {
        let action = match character {
            'c' => Action::CopyAgentPrompt,
            'p' => Action::CopyReviewRequest,
            'w' => Action::OpenPullRequestWeb,
            'n' => Action::NewWorktree,
            'm' => Action::Move,
            'L' => Action::Lock,
            'U' => Action::Unlock,
            'd' => Action::Remove,
            'R' => Action::Repair,
            'P' => Action::Prune,
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
                if let Some((_, worktree)) =
                    self.repositories[repository_index].singleton_worktree()
                {
                    return if worktree.navigable() && worktree.path.exists() {
                        Intent::Accept(worktree.path.clone())
                    } else {
                        self.inline_error = Some("this row is not a navigable checkout".to_owned());
                        Intent::None
                    };
                }
                let path = self.repositories[repository_index].config.path.clone();
                let expanded = self.displayed_disclosure_expanded(
                    &DisclosureKey::Repository(path.clone()),
                    self.repositories[repository_index].expanded,
                );
                self.set_disclosure_expanded(DisclosureKey::Repository(path), !expanded);
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
                let identity = self.virtual_repositories[virtual_repository_index]
                    .identity
                    .clone();
                let expanded = self.displayed_disclosure_expanded(
                    &DisclosureKey::VirtualRepository(identity.clone()),
                    self.virtual_repositories[virtual_repository_index].expanded,
                );
                self.set_disclosure_expanded(DisclosureKey::VirtualRepository(identity), !expanded);
                Intent::None
            }
            Some(VisibleRow::Backburner { identity, .. }) => {
                let expanded = self.displayed_disclosure_expanded(
                    &DisclosureKey::Backburner(identity.clone()),
                    false,
                );
                self.set_disclosure_expanded(DisclosureKey::Backburner(identity), !expanded);
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
            Some(VisibleRow::Inline { url, .. }) => {
                url.map(Intent::OpenUrl).unwrap_or(Intent::None)
            }
            None => Intent::None,
        }
    }

    fn collapse_or_focus_list(&mut self) {
        if let Some(VisibleRow::Inline {
            owner,
            section,
            expanded,
            ..
        }) = self.selected_row()
        {
            if expanded == Some(false) {
                return;
            }
            self.set_disclosure_expanded(DisclosureKey::Section(owner.clone(), section), false);
            self.selected = Some(RowId::Section(owner, section));
            self.ensure_selected_in_view();
            return;
        }
        if let Some(row) = self.selected_row()
            && let Some(owner) = row.owner()
        {
            self.set_disclosure_expanded(DisclosureKey::Branch(owner), false);
            self.ensure_selected_in_view();
            return;
        }
        if let Some((_, repository_index)) = self.selected_repository() {
            let path = self.repositories[repository_index].config.path.clone();
            self.set_disclosure_expanded(DisclosureKey::Repository(path), false);
            self.selected = Some(self.repositories[repository_index].id());
            self.ensure_selected_in_view();
        } else if let Some(row) = self.selected_row() {
            match row {
                VisibleRow::VirtualPullRequest {
                    mapped_repository_index: Some(repository_index),
                    ..
                } => {
                    let path = self.repositories[repository_index].config.path.clone();
                    self.set_disclosure_expanded(DisclosureKey::Repository(path), false);
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
                    let identity = self.virtual_repositories[virtual_repository_index]
                        .identity
                        .clone();
                    self.set_disclosure_expanded(DisclosureKey::VirtualRepository(identity), false);
                    self.selected = Some(self.virtual_repositories[virtual_repository_index].id());
                }
                VisibleRow::Backburner { identity, .. } => {
                    self.set_disclosure_expanded(DisclosureKey::Backburner(identity), false);
                }
                VisibleRow::Repository { .. }
                | VisibleRow::Worktree { .. }
                | VisibleRow::Inline { .. } => return,
            }
            self.ensure_selected_in_view();
        }
    }

    fn expand_selected(&mut self) {
        match self.selected_row() {
            Some(VisibleRow::Inline {
                owner,
                section,
                kind: InlineRowKind::Section,
                ..
            }) => self.set_disclosure_expanded(DisclosureKey::Section(owner, section), true),
            Some(VisibleRow::Worktree {
                id,
                has_children: true,
                ..
            }) => {
                let RowId::Worktree(path) = id else {
                    return;
                };
                self.set_disclosure_expanded(DisclosureKey::Branch(BranchId::Worktree(path)), true);
            }
            Some(VisibleRow::VirtualPullRequest {
                id,
                has_children: true,
                ..
            }) => {
                let RowId::VirtualPullRequest(identity) = id else {
                    return;
                };
                self.set_disclosure_expanded(
                    DisclosureKey::Branch(BranchId::VirtualPullRequest(identity)),
                    true,
                );
            }
            Some(VisibleRow::Repository {
                repository_index, ..
            }) => {
                let path = self.repositories[repository_index].config.path.clone();
                self.set_disclosure_expanded(DisclosureKey::Repository(path), true);
            }
            Some(VisibleRow::VirtualRepository {
                virtual_repository_index,
                ..
            }) => {
                let identity = self.virtual_repositories[virtual_repository_index]
                    .identity
                    .clone();
                self.set_disclosure_expanded(DisclosureKey::VirtualRepository(identity), true);
            }
            Some(VisibleRow::Backburner { identity, .. }) => {
                self.set_disclosure_expanded(DisclosureKey::Backburner(identity), true);
            }
            Some(VisibleRow::Worktree {
                has_children: false,
                ..
            })
            | Some(VisibleRow::VirtualPullRequest {
                has_children: false,
                ..
            })
            | Some(VisibleRow::Inline { .. })
            | None => return,
        }
        self.ensure_selected_in_view();
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
        self.selected = Some(next_id);
        self.ensure_selected_in_view_with_rows(&rows);
    }

    fn select_index(&mut self, index: usize) {
        let rows = self.visible_rows();
        if let Some(row) = rows.get(index) {
            let id = row.id().clone();
            self.selected = Some(id);
            self.ensure_selected_in_view_with_rows(&rows);
        }
    }

    fn select_initial(&mut self) {
        for repository in &self.repositories {
            for worktree in &repository.worktrees {
                if worktree.navigable() && self.current_worktree.as_ref() == Some(&worktree.path) {
                    self.selected = Some(if repository.singleton_worktree().is_some() {
                        repository.id()
                    } else {
                        RowId::Worktree(worktree.path.clone())
                    });
                    return;
                }
            }
        }
        self.selected = self
            .repositories
            .iter()
            .find_map(|repository| {
                repository
                    .worktrees
                    .iter()
                    .find(|worktree| worktree.navigable())
                    .map(|worktree| {
                        if repository.singleton_worktree().is_some() {
                            repository.id()
                        } else {
                            RowId::Worktree(worktree.path.clone())
                        }
                    })
            })
            .or_else(|| self.visible_rows().first().map(|row| row.id().clone()));
    }

    fn ensure_selection_visible(&mut self) {
        self.reconcile_focus_target();
        let selected = self.selected.clone();
        let owner = self
            .selected
            .as_ref()
            .and_then(|selected| self.branch_for_row_id(selected));
        let rows = self.visible_rows();
        let visible = self
            .selected
            .as_ref()
            .is_some_and(|selected| rows.iter().any(|row| row.id() == selected));
        if !visible {
            self.selected = selected
                .as_ref()
                .and_then(|selected| self.filtered_ancestor_fallback(selected, &rows))
                .or_else(|| {
                    selected.as_ref().and_then(|selected| {
                        self.semantic_fallback_ids(selected)
                            .into_iter()
                            .find(|candidate| rows.iter().any(|row| row.id() == candidate))
                    })
                })
                .or_else(|| {
                    owner.and_then(|owner| {
                        let owner_id = match owner {
                            BranchId::Worktree(path) => RowId::Worktree(path),
                            BranchId::VirtualPullRequest(identity) => {
                                RowId::VirtualPullRequest(identity)
                            }
                        };
                        rows.iter()
                            .find(|row| row.id() == &owner_id)
                            .map(|row| row.id().clone())
                    })
                })
                .or_else(|| {
                    rows.iter()
                        .find(|row| matches!(row, VisibleRow::Worktree { .. }))
                        .map(|row| row.id().clone())
                })
                .or_else(|| rows.first().map(|row| row.id().clone()));
        }
        self.ensure_selected_in_view();
    }

    fn reconcile_focus_target(&mut self) {
        if self
            .focus_target
            .clone()
            .is_some_and(|target| self.focused_rows(&target).is_none())
        {
            self.focus_target = None;
        }
    }

    fn filtered_ancestor_fallback(
        &self,
        selected: &RowId,
        visible: &[VisibleRow],
    ) -> Option<RowId> {
        if !self.filter_mode() {
            return None;
        }
        let expanded = self.filtered_rows(self.scoped_logical_rows(), false);
        let index = expanded.iter().position(|row| row.id() == selected)?;
        let mut child_depth = self.visible_row_depth(&expanded[index]);
        for row in expanded[..index].iter().rev() {
            let depth = self.visible_row_depth(row);
            if depth + 1 != child_depth {
                continue;
            }
            if visible.iter().any(|candidate| candidate.id() == row.id()) {
                return Some(row.id().clone());
            }
            child_depth = depth;
        }
        None
    }

    fn semantic_fallback_ids(&self, selected: &RowId) -> Vec<RowId> {
        let mut candidates = Vec::new();
        match selected {
            RowId::Metadata(owner, _) => {
                candidates.push(RowId::Section(owner.clone(), InlineSection::Overview));
            }
            RowId::Check(identity, name) => {
                if let Some(section) = self
                    .pull_request_details
                    .get(identity)
                    .and_then(|details| {
                        details
                            .checks
                            .iter()
                            .find(|check| check.name.eq_ignore_ascii_case(name))
                    })
                    .and_then(|check| match check.state {
                        CheckState::Pending | CheckState::Expected => {
                            Some(InlineSection::PendingChecks)
                        }
                        CheckState::Success | CheckState::Neutral | CheckState::Skipped => {
                            Some(InlineSection::ValidResults)
                        }
                        CheckState::Failure | CheckState::Error | CheckState::Unknown => None,
                    })
                    && let Some(owner) = self.branch_for_pull_request(identity)
                {
                    candidates.push(RowId::Section(owner, section));
                }
                if let Some(owner) = self.branch_for_pull_request(identity) {
                    candidates.push(RowId::Section(owner, InlineSection::Checks));
                }
            }
            RowId::Reviewer(identity, _) => {
                if let Some(owner) = self.branch_for_pull_request(identity) {
                    candidates.push(RowId::Section(owner, InlineSection::Reviewers));
                }
            }
            RowId::OpenComment(identity, _) => {
                if let Some(owner) = self.branch_for_pull_request(identity) {
                    candidates.push(RowId::Section(owner, InlineSection::OpenComments));
                }
            }
            RowId::Section(owner, section) => {
                if matches!(
                    section,
                    InlineSection::PendingChecks | InlineSection::ValidResults
                ) {
                    candidates.push(RowId::Section(owner.clone(), InlineSection::Checks));
                }
            }
            RowId::Repository(_)
            | RowId::Worktree(_)
            | RowId::VirtualRepository(_)
            | RowId::Backburner(_)
            | RowId::VirtualPullRequest(_) => {}
        }
        if let Some(branch) = self.branch_for_row_id(selected) {
            candidates.push(row_id_for_branch(&branch));
            candidates.extend(self.branch_ancestor_fallback_ids(&branch));
        }
        candidates
    }

    fn branch_ancestor_fallback_ids(&self, branch: &BranchId) -> Vec<RowId> {
        for (repository_index, repository) in self.repositories.iter().enumerate() {
            let virtual_indexes = self
                .virtual_repositories
                .iter()
                .enumerate()
                .filter(|(_, candidate)| {
                    candidate.mapped_repository.as_ref() == Some(&repository.config.path)
                })
                .map(|(index, _)| index)
                .collect::<Vec<_>>();
            let forest = self.branch_forest(Some(repository_index), &virtual_indexes);
            if let Some(index) = forest.nodes.iter().position(|node| node.id == *branch) {
                let mut candidates = branch_parent_row_ids(&forest, index);
                if forest.nodes[index].backburnered
                    && let Some(identity) = &forest.nodes[index].identity
                {
                    candidates.push(RowId::Backburner(identity.repository.clone()));
                }
                candidates.push(repository.id());
                return candidates;
            }
        }
        for (virtual_index, repository) in self.virtual_repositories.iter().enumerate() {
            if repository.mapped_repository.is_some() {
                continue;
            }
            let forest = self.branch_forest(None, &[virtual_index]);
            if let Some(index) = forest.nodes.iter().position(|node| node.id == *branch) {
                let mut candidates = branch_parent_row_ids(&forest, index);
                if forest.nodes[index].backburnered {
                    candidates.push(RowId::Backburner(repository.identity.clone()));
                }
                candidates.push(repository.id());
                return candidates;
            }
        }
        Vec::new()
    }

    fn branch_for_row_id(&self, row: &RowId) -> Option<BranchId> {
        match row {
            RowId::Worktree(path) => Some(BranchId::Worktree(path.clone())),
            RowId::VirtualPullRequest(identity) => {
                Some(BranchId::VirtualPullRequest(identity.clone()))
            }
            RowId::Section(owner, _) | RowId::Metadata(owner, _) => Some(owner.clone()),
            RowId::Check(identity, _)
            | RowId::Reviewer(identity, _)
            | RowId::OpenComment(identity, _) => self.branch_for_pull_request(identity),
            RowId::Repository(_) | RowId::VirtualRepository(_) | RowId::Backburner(_) => None,
        }
    }

    fn branch_for_pull_request(&self, identity: &CanonicalPullRequestId) -> Option<BranchId> {
        for repository in &self.repositories {
            for worktree in &repository.worktrees {
                let matches = self
                    .github
                    .get(&worktree.path)
                    .and_then(GitHubState::data)
                    .and_then(|data| data.pull_request.as_ref())
                    .and_then(|pull_request| self.pull_request_identity(repository, pull_request))
                    .as_ref()
                    == Some(identity);
                if matches {
                    return Some(BranchId::Worktree(worktree.path.clone()));
                }
            }
        }
        self.virtual_repositories
            .iter()
            .flat_map(|repository| &repository.pull_requests)
            .find(|pull_request| pull_request.identity == *identity)
            .map(|_| BranchId::VirtualPullRequest(identity.clone()))
    }

    fn ensure_selected_in_view(&mut self) {
        let rows = self.visible_rows();
        self.ensure_selected_in_view_with_rows(&rows);
    }

    fn ensure_selected_in_view_with_rows(&mut self, rows: &[VisibleRow]) {
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

    fn ensure_initial_selection_in_view_with_rows(&mut self, rows: &[VisibleRow]) {
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum TopLevelRepository {
    Local(usize),
    Virtual(usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BranchSource {
    Worktree {
        repository_index: usize,
        worktree_index: usize,
    },
    VirtualPullRequest {
        virtual_repository_index: usize,
        pull_request_index: usize,
    },
}

#[derive(Clone, Debug)]
struct BranchNode {
    id: BranchId,
    source: BranchSource,
    identity: Option<CanonicalPullRequestId>,
    pull_request: Option<PullRequest>,
    parent: Option<usize>,
    children: Vec<usize>,
    backburnered: bool,
}

#[derive(Clone, Debug, Default)]
struct BranchForest {
    nodes: Vec<BranchNode>,
}

fn alphabetical_cmp(left: &str, right: &str) -> Ordering {
    left.to_lowercase()
        .cmp(&right.to_lowercase())
        .then_with(|| left.cmp(right))
}

fn forest_identity_order(forest: &BranchForest) -> Vec<CanonicalPullRequestId> {
    let mut identities = Vec::new();
    let mut seen = BTreeSet::new();
    for root in (0..forest.nodes.len()).filter(|index| forest.nodes[*index].parent.is_none()) {
        append_branch_identities(forest, root, &mut seen, &mut identities);
    }
    identities
}

fn branch_subtree_identity_order(
    forest: &BranchForest,
    root: usize,
    descendants_only: bool,
) -> Vec<CanonicalPullRequestId> {
    let mut identities = Vec::new();
    let mut seen = BTreeSet::new();
    if descendants_only {
        for child in &forest.nodes[root].children {
            append_branch_identities(forest, *child, &mut seen, &mut identities);
        }
    } else {
        append_branch_identities(forest, root, &mut seen, &mut identities);
    }
    identities
}

fn branch_subtree_indexes(forest: &BranchForest, root: usize) -> BTreeSet<usize> {
    let mut included = BTreeSet::new();
    let mut pending = vec![root];
    while let Some(index) = pending.pop() {
        if included.insert(index) {
            pending.extend(forest.nodes[index].children.iter().copied());
        }
    }
    included
}

fn append_branch_identities(
    forest: &BranchForest,
    index: usize,
    seen: &mut BTreeSet<CanonicalPullRequestId>,
    identities: &mut Vec<CanonicalPullRequestId>,
) {
    if let Some(identity) = &forest.nodes[index].identity
        && seen.insert(identity.clone())
    {
        identities.push(identity.clone());
    }
    for child in &forest.nodes[index].children {
        append_branch_identities(forest, *child, seen, identities);
    }
}

fn row_id_for_branch(branch: &BranchId) -> RowId {
    match branch {
        BranchId::Worktree(path) => RowId::Worktree(path.clone()),
        BranchId::VirtualPullRequest(identity) => RowId::VirtualPullRequest(identity.clone()),
    }
}

fn branch_parent_row_ids(forest: &BranchForest, mut index: usize) -> Vec<RowId> {
    let partition = forest.nodes[index].backburnered;
    let mut candidates = Vec::new();
    while let Some(parent) = forest.nodes[index]
        .parent
        .filter(|parent| forest.nodes[*parent].backburnered == partition)
    {
        let parent_id = forest.nodes[parent].id.clone();
        candidates.push(RowId::Section(
            parent_id.clone(),
            InlineSection::StackedBranches,
        ));
        candidates.push(row_id_for_branch(&parent_id));
        index = parent;
    }
    candidates
}

fn backburner_root_index(forest: &BranchForest, mut index: usize) -> usize {
    while let Some(parent) = forest.nodes[index]
        .parent
        .filter(|parent| forest.nodes[*parent].backburnered)
    {
        index = parent;
    }
    index
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

fn canonical_path_or_owned(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_owned())
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

    #[test]
    fn prompt_worktree_uses_mapped_repository_branch_and_reports_local_head() {
        let authored = authored("team", "project", 42, "2026-01-01T00:00:00Z");
        let identity = authored.identity.clone();
        let mut repository = repository("/repo", true);
        repository.config.github_remotes.insert(
            "origin".to_owned(),
            GitHubRepositoryIdentity::canonical("github.com", "team", "project"),
        );
        repository.worktrees[1].branch = Some("refs/heads/topic-42".to_owned());
        repository.worktrees[1].head = Some("local-head".to_owned());
        let app = App::new(vec![repository], PathBuf::from("/elsewhere"));

        assert_eq!(
            app.prompt_worktree(&identity, &authored.pull_request),
            Some(PromptWorktree {
                path: PathBuf::from("/repo-topic"),
                branch: Some("refs/heads/topic-42".to_owned()),
                head: Some("local-head".to_owned()),
            })
        );
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

    fn filter_test_app() -> (App, CanonicalPullRequestId) {
        let authored = authored("team", "project", 42, "2026-01-01T00:00:00Z");
        let identity = authored.identity.clone();
        let mut app = App::new(Vec::new(), PathBuf::from("/elsewhere"));
        app.virtual_repositories = vec![VirtualRepositoryView {
            identity: identity.repository.clone(),
            mapped_repository: None,
            expanded: true,
            pull_requests: vec![authored],
        }];
        let check =
            |name: &str, state, target_url: Option<&str>, order| crate::model::PullRequestCheck {
                name: name.to_owned(),
                state,
                target_url: target_url.map(str::to_owned),
                required: true,
                source_order: order,
                completed_at: None,
            };
        app.pull_request_details.insert(
            identity.clone(),
            PullRequestDetails {
                checks: vec![
                    check(
                        "failure-needle",
                        crate::model::CheckState::Failure,
                        Some("https://checks.example/failure-hidden"),
                        0,
                    ),
                    check("pending-needle", crate::model::CheckState::Pending, None, 1),
                    check("valid-needle", crate::model::CheckState::Success, None, 2),
                ],
                check_contexts_complete: true,
                review_requests: vec![crate::model::ReviewRequest {
                    id: "request-hidden-id".to_owned(),
                    name: "Alice".to_owned(),
                    kind: crate::model::ReviewerKind::User,
                }],
                reviewer_reviews: vec![crate::model::ReviewerReview {
                    id: "review-hidden-id".to_owned(),
                    database_id: Some(77),
                    reviewer: "Bob".to_owned(),
                    state: crate::model::SubmittedReviewState::Approved,
                    submitted_at: None,
                }],
                reviews_complete: true,
                feedback: vec![
                    crate::model::PullRequestFeedback {
                        id: "summary-hidden-id".to_owned(),
                        database_id: Some(78),
                        thread_id: None,
                        kind: crate::model::FeedbackKind::ReviewSummary,
                        author: "Bob".to_owned(),
                        body: "summary needle body".to_owned(),
                        path: None,
                        permalink: Some("https://reviews.example/summary-hidden".to_owned()),
                        outdated: false,
                    },
                    crate::model::PullRequestFeedback {
                        id: "comment-hidden-id".to_owned(),
                        database_id: Some(88),
                        thread_id: Some("thread-hidden-id".to_owned()),
                        kind: crate::model::FeedbackKind::InlineThread,
                        author: "Carol".to_owned(),
                        body: "comment needle body".to_owned(),
                        path: Some("src/needle.rs".to_owned()),
                        permalink: Some("https://comments.example/comment-hidden".to_owned()),
                        outdated: true,
                    },
                ],
                feedback_complete: true,
                warnings: vec!["detail-warning-hidden".to_owned()],
                ..PullRequestDetails::default()
            },
        );
        app.selected = Some(RowId::VirtualPullRequest(identity.clone()));
        (app, identity)
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
            .map(|(identity, repository_index)| {
                let mapped_repository = repository_index
                    .and_then(|index| app.repositories.get(index))
                    .map(|repository| repository.config.path.clone());
                PullRequestMapping {
                    identity,
                    mapped_repository,
                }
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
        assert_eq!(
            app.visible_rows()
                .iter()
                .filter(|row| matches!(row, VisibleRow::VirtualPullRequest { .. }))
                .count(),
            4
        );
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
                    depth,
                    ..
                } => Some((
                    app.virtual_repositories[0].pull_requests[*pull_request_index]
                        .identity
                        .number,
                    *depth,
                )),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(nested, vec![(10, 1), (11, 3), (12, 5), (13, 1)]);

        app.filter = "stack-grandchild".to_owned();
        app.filter_active = true;
        let filtered = app
            .visible_rows()
            .iter()
            .filter_map(|row| match row {
                VisibleRow::VirtualPullRequest {
                    pull_request_index,
                    depth,
                    ..
                } => Some((
                    app.virtual_repositories[0].pull_requests[*pull_request_index]
                        .identity
                        .number,
                    *depth,
                )),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(filtered, vec![(10, 1), (11, 3), (12, 5)]);
        assert_eq!(
            app.visible_rows()
                .into_iter()
                .map(|row| row.id().clone())
                .collect::<Vec<_>>(),
            vec![
                RowId::Repository(PathBuf::from("/repo")),
                RowId::VirtualPullRequest(parent.identity.clone()),
                RowId::Section(
                    BranchId::VirtualPullRequest(parent.identity.clone()),
                    InlineSection::StackedBranches,
                ),
                RowId::VirtualPullRequest(child.identity.clone()),
                RowId::Section(
                    BranchId::VirtualPullRequest(child.identity.clone()),
                    InlineSection::StackedBranches,
                ),
                RowId::VirtualPullRequest(grandchild.identity.clone()),
                RowId::Section(
                    BranchId::VirtualPullRequest(grandchild.identity.clone()),
                    InlineSection::Overview,
                ),
                RowId::Metadata(
                    BranchId::VirtualPullRequest(grandchild.identity.clone()),
                    "overview-head".to_owned(),
                ),
            ]
        );

        app.filter_active = false;
        let committed_branches = app
            .visible_rows()
            .iter()
            .filter_map(|row| match row {
                VisibleRow::VirtualPullRequest {
                    pull_request_index, ..
                } => Some(
                    app.virtual_repositories[0].pull_requests[*pull_request_index]
                        .identity
                        .number,
                ),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(committed_branches, vec![10, 11, 12]);
        assert!(app.visible_rows().iter().any(|row| {
            row.id()
                == &RowId::Metadata(
                    BranchId::VirtualPullRequest(grandchild.identity.clone()),
                    "overview-url".to_owned(),
                )
        }));

        app.filter.clear();
        let parent_stack = DisclosureKey::Section(
            BranchId::VirtualPullRequest(parent.identity.clone()),
            InlineSection::StackedBranches,
        );
        app.set_disclosure_expanded(parent_stack.clone(), false);
        app.filter = "stack-grandchild".to_owned();
        app.selected = Some(RowId::VirtualPullRequest(grandchild.identity.clone()));
        app.handle_key(key(KeyCode::Esc));
        assert_eq!(
            app.selected,
            Some(RowId::VirtualPullRequest(grandchild.identity.clone()))
        );
        assert!(app.filter.is_empty());
        assert_eq!(app.disclosure_expanded.get(&parent_stack), Some(&true));
    }

    #[test]
    fn tree_sorts_repository_and_branch_labels_alphabetically() {
        let mut zebra = repository("/zebra-path", true);
        zebra.config.label = Some("zebra".to_owned());
        let mut alpha = repository("/alpha-path", true);
        alpha.config.label = Some("Alpha".to_owned());
        let app = App::new(vec![zebra, alpha], PathBuf::from("/elsewhere"));

        let repository_paths = app
            .visible_rows()
            .iter()
            .filter_map(|row| match row {
                VisibleRow::Repository {
                    repository_index, ..
                } => Some(app.repositories[*repository_index].config.path.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            repository_paths,
            vec![PathBuf::from("/alpha-path"), PathBuf::from("/zebra-path")]
        );

        let mut branches = repository("/branches", true);
        branches.worktrees = vec![
            worktree("/branches-zulu", "zulu", false),
            worktree("/branches-alpha", "Alpha", false),
            worktree("/branches-bravo", "bravo", false),
        ];
        let app = App::new(vec![branches], PathBuf::from("/elsewhere"));
        let branch_names = app
            .visible_rows()
            .iter()
            .filter_map(|row| match row {
                VisibleRow::Worktree {
                    repository_index,
                    worktree_index,
                    ..
                } => app.repositories[*repository_index].worktrees[*worktree_index]
                    .branch
                    .as_deref()
                    .and_then(|branch| branch.strip_prefix("refs/heads/")),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(branch_names, vec!["Alpha", "bravo", "zulu"]);
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
                    depth,
                    ..
                } => Some((worktree_index, depth)),
                _ => None,
            })
            .collect::<Vec<_>>();

        assert_eq!(nested, vec![(0, 1), (1, 3), (2, 5)]);
    }

    #[test]
    fn mixed_local_and_virtual_descendants_share_typed_stack_sections_once() {
        let mut local = repository("/repo", true);
        let mut root = authored("team", "project", 9, "2025-12-31");
        root.pull_request.head.repository = Some("team/project".to_owned());
        root.pull_request.head.branch = "stack-root".to_owned();
        let mut parent = authored("team", "project", 10, "2026-01-01");
        parent.pull_request.base = root.pull_request.head.clone();
        parent.pull_request.head.repository = Some("team/project".to_owned());
        parent.pull_request.head.branch = "stack-parent".to_owned();
        let mut child = authored("team", "project", 11, "2026-01-02");
        child.pull_request.base = parent.pull_request.head.clone();
        child.pull_request.head.repository = Some("team/project".to_owned());
        child.pull_request.head.branch = "stack-child".to_owned();
        local
            .config
            .github_remotes
            .insert("origin".to_owned(), parent.identity.repository.clone());
        let topic = local.worktrees[1].path.clone();
        let mut app = App::new(vec![local], PathBuf::from("/elsewhere"));
        app.branch_parents
            .insert(topic.clone(), PathBuf::from("/repo"));
        app.github.insert(
            PathBuf::from("/repo"),
            GitHubState::Ready(GitHubBranchData {
                pull_request: Some(root.pull_request.clone()),
                warnings: Vec::new(),
                rate_limit: None,
            }),
        );
        app.github.insert(
            topic.clone(),
            GitHubState::Ready(GitHubBranchData {
                pull_request: Some(parent.pull_request.clone()),
                warnings: Vec::new(),
                rate_limit: None,
            }),
        );
        app.virtual_repositories = vec![VirtualRepositoryView {
            identity: child.identity.repository.clone(),
            mapped_repository: Some(PathBuf::from("/repo")),
            expanded: true,
            pull_requests: vec![root.clone(), parent.clone(), child.clone()],
        }];

        let rows = app.visible_rows();
        for id in [
            RowId::Worktree(PathBuf::from("/repo")),
            RowId::Worktree(topic.clone()),
            RowId::VirtualPullRequest(child.identity.clone()),
        ] {
            assert_eq!(rows.iter().filter(|row| row.id() == &id).count(), 1);
        }
        assert!(rows.iter().any(|row| {
            matches!(row, VisibleRow::Inline {
                id: RowId::Section(
                    BranchId::Worktree(path),
                    InlineSection::StackedBranches
                ),
                text,
                ..
            } if path == &PathBuf::from("/repo") && text == "Stacked branches")
        }));
        assert!(rows.iter().any(|row| {
            matches!(row, VisibleRow::Inline {
                id: RowId::Section(
                    BranchId::Worktree(path),
                    InlineSection::StackedBranches
                ),
                text,
                ..
            } if path == &PathBuf::from("/repo-topic") && text == "Stacked PRs")
        }));

        app.selected = Some(RowId::Worktree(topic));
        let review_request = app.review_request().unwrap();
        assert_eq!(review_request.lines().count(), 2);
        assert_eq!(
            review_request.matches(&parent.pull_request.url).count(),
            1,
            "the local and authored representations must deduplicate"
        );
        assert_eq!(review_request.matches(&child.pull_request.url).count(), 1);

        app.selected = Some(RowId::Worktree(PathBuf::from("/repo")));
        let root_review_request = app.review_request().unwrap();
        assert_eq!(root_review_request.lines().count(), 3);
        assert_eq!(
            root_review_request.matches(&root.pull_request.url).count(),
            1
        );
        assert_eq!(
            root_review_request
                .matches(&parent.pull_request.url)
                .count(),
            1
        );
        assert_eq!(
            root_review_request.matches(&child.pull_request.url).count(),
            1
        );
        app.selected = Some(RowId::Repository(PathBuf::from("/repo")));
        assert_eq!(
            app.review_request().as_deref(),
            Some(root_review_request.as_str())
        );
    }

    #[test]
    fn github_base_wins_and_remote_cycles_or_ambiguity_become_roots() {
        let mut local = repository("/repo", true);
        let remote_parent = authored("team", "project", 20, "2026-01-01");
        let remote_parent_id = remote_parent.identity.clone();
        let mut local_child = authored("team", "project", 21, "2026-01-02");
        local_child.pull_request.base = remote_parent.pull_request.head.clone();
        local
            .config
            .github_remotes
            .insert("origin".to_owned(), local_child.identity.repository.clone());
        let topic = local.worktrees[1].path.clone();
        let mut app = App::new(vec![local], PathBuf::from("/elsewhere"));
        app.branch_parents
            .insert(topic.clone(), PathBuf::from("/repo"));
        app.github.insert(
            topic.clone(),
            GitHubState::Ready(GitHubBranchData {
                pull_request: Some(local_child.pull_request.clone()),
                warnings: Vec::new(),
                rate_limit: None,
            }),
        );
        app.virtual_repositories = vec![VirtualRepositoryView {
            identity: remote_parent.identity.repository.clone(),
            mapped_repository: Some(PathBuf::from("/repo")),
            expanded: true,
            pull_requests: vec![remote_parent],
        }];
        let forest = app.branch_forest(Some(0), &[0]);
        let topic_index = forest
            .nodes
            .iter()
            .position(|node| node.id == BranchId::Worktree(topic.clone()))
            .unwrap();
        assert_eq!(
            forest.nodes[topic_index]
                .parent
                .map(|parent| &forest.nodes[parent].id),
            Some(&BranchId::VirtualPullRequest(remote_parent_id))
        );

        let mut cycle_a = authored("team", "cycles", 1, "1");
        let mut cycle_b = authored("team", "cycles", 2, "2");
        cycle_a.pull_request.head.branch = "a".to_owned();
        cycle_a.pull_request.base.branch = "b".to_owned();
        cycle_b.pull_request.head.branch = "b".to_owned();
        cycle_b.pull_request.base.branch = "a".to_owned();
        cycle_a.pull_request.head.repository = Some("team/cycles".to_owned());
        cycle_a.pull_request.base.repository = Some("team/cycles".to_owned());
        cycle_b.pull_request.head.repository = Some("team/cycles".to_owned());
        cycle_b.pull_request.base.repository = Some("team/cycles".to_owned());
        let mut ambiguous_parent_a = authored("team", "cycles", 3, "3");
        let mut ambiguous_parent_b = authored("team", "cycles", 4, "4");
        let mut ambiguous_child = authored("team", "cycles", 5, "5");
        for parent in [&mut ambiguous_parent_a, &mut ambiguous_parent_b] {
            parent.pull_request.head.repository = Some("team/cycles".to_owned());
            parent.pull_request.head.branch = "shared".to_owned();
        }
        ambiguous_child.pull_request.base.repository = Some("team/cycles".to_owned());
        ambiguous_child.pull_request.base.branch = "shared".to_owned();
        let identity = cycle_a.identity.repository.clone();
        let mut virtual_app = App::new(Vec::new(), PathBuf::from("/elsewhere"));
        virtual_app.virtual_repositories = vec![VirtualRepositoryView {
            identity,
            mapped_repository: None,
            expanded: true,
            pull_requests: vec![
                cycle_a,
                cycle_b,
                ambiguous_parent_a,
                ambiguous_parent_b,
                ambiguous_child,
            ],
        }];
        let forest = virtual_app.branch_forest(None, &[0]);
        for number in [1, 2, 5] {
            let node = forest
                .nodes
                .iter()
                .find(|node| node.identity.as_ref().is_some_and(|id| id.number == number))
                .unwrap();
            assert_eq!(node.parent, None, "PR #{number} must remain a root");
        }
    }

    #[test]
    fn github_trunk_base_vetoes_incidental_local_commit_ancestry() {
        let mut local = repository("/repo", true);
        let pull_request = authored("team", "project", 21, "2026-01-02");
        local.config.github_remotes.insert(
            "origin".to_owned(),
            pull_request.identity.repository.clone(),
        );
        let topic = local.worktrees[1].path.clone();
        let mut app = App::new(vec![local], PathBuf::from("/elsewhere"));
        app.branch_parents
            .insert(topic.clone(), PathBuf::from("/repo"));
        app.github.insert(
            topic.clone(),
            GitHubState::Ready(GitHubBranchData {
                pull_request: Some(pull_request.pull_request),
                warnings: Vec::new(),
                rate_limit: None,
            }),
        );

        let forest = app.branch_forest(Some(0), &[]);
        let topic = forest
            .nodes
            .iter()
            .find(|node| node.id == BranchId::Worktree(topic.clone()))
            .unwrap();
        assert_eq!(topic.parent, None);
    }

    #[test]
    fn branch_and_stack_disclosures_survive_refresh_independently() {
        let mut app = App::new(vec![repository("/repo", true)], PathBuf::from("/elsewhere"));
        let owner = BranchId::Worktree(PathBuf::from("/repo"));
        app.branch_parents
            .insert(PathBuf::from("/repo-topic"), PathBuf::from("/repo"));
        app.selected = Some(RowId::Worktree(PathBuf::from("/repo")));
        app.handle_key(key(KeyCode::Char('h')));
        assert!(!app.visible_rows().iter().any(|row| {
            matches!(row, VisibleRow::Inline { owner: found, .. } if found == &owner)
        }));

        app.replace_repositories(vec![repository("/repo", true)]);
        assert!(!app.visible_rows().iter().any(|row| {
            matches!(row, VisibleRow::Inline { owner: found, .. } if found == &owner)
        }));
        app.handle_key(key(KeyCode::Char('l')));
        assert!(app.visible_rows().iter().any(|row| {
            row.id() == &RowId::Section(owner.clone(), InlineSection::StackedBranches)
        }));

        app.selected = Some(RowId::Section(
            owner.clone(),
            InlineSection::StackedBranches,
        ));
        app.handle_key(key(KeyCode::Char('h')));
        assert!(
            !app.visible_rows()
                .iter()
                .any(|row| { row.id() == &RowId::Worktree(PathBuf::from("/repo-topic")) })
        );
        app.replace_repositories(vec![repository("/repo", true)]);
        assert!(
            !app.visible_rows()
                .iter()
                .any(|row| { row.id() == &RowId::Worktree(PathBuf::from("/repo-topic")) })
        );
        app.handle_key(key(KeyCode::Char('l')));
        assert!(
            app.visible_rows()
                .iter()
                .any(|row| { row.id() == &RowId::Worktree(PathBuf::from("/repo-topic")) })
        );
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
        app.filter_active = true;
        assert_eq!(
            app.visible_rows()
                .into_iter()
                .map(|row| row.id().clone())
                .collect::<Vec<_>>(),
            vec![RowId::Repository(PathBuf::from("/repo.git"))]
        );
    }

    #[test]
    fn local_only_backburnered_worktree_remains_recoverable_under_bare_repository() {
        let pull_request = authored("team", "project", 42, "2026-01-01");
        let repository_identity = pull_request.identity.repository.clone();
        let mut bare = repository("/repo.git", true);
        bare.config
            .github_remotes
            .insert("origin".to_owned(), repository_identity.clone());
        bare.worktrees = vec![
            worktree("/repo.git", "main", true),
            worktree("/trees/topic", "topic", false),
        ];
        let local_path = bare.worktrees[1].path.clone();
        let mut app = App::new(vec![bare], PathBuf::from("/elsewhere"));
        app.github.insert(
            local_path.clone(),
            GitHubState::Ready(GitHubBranchData {
                pull_request: Some(pull_request.pull_request.clone()),
                warnings: Vec::new(),
                rate_limit: None,
            }),
        );
        app.selected = Some(RowId::Worktree(local_path.clone()));

        assert_eq!(
            app.handle_key(key(KeyCode::Char('b'))),
            Intent::PersistBackburner
        );
        assert!(app.virtual_repositories.is_empty());
        assert_eq!(
            app.selected,
            Some(RowId::Backburner(repository_identity.clone()))
        );
        let collapsed = app.visible_rows();
        assert!(
            collapsed
                .iter()
                .any(|row| { row.id() == &RowId::Backburner(repository_identity.clone()) })
        );
        assert!(
            !collapsed
                .iter()
                .any(|row| row.id() == &RowId::Worktree(local_path.clone()))
        );

        app.set_disclosure_expanded(DisclosureKey::Backburner(repository_identity.clone()), true);
        assert!(
            app.visible_rows()
                .iter()
                .any(|row| row.id() == &RowId::Worktree(local_path.clone()))
        );

        app.selected = Some(RowId::Worktree(local_path.clone()));
        assert_eq!(
            app.handle_key(key(KeyCode::Char('b'))),
            Intent::PersistBackburner
        );
        let restored = app.visible_rows();
        assert!(
            restored
                .iter()
                .any(|row| row.id() == &RowId::Worktree(local_path.clone()))
        );
        assert!(
            !restored
                .iter()
                .any(|row| matches!(row, VisibleRow::Backburner { .. }))
        );
    }

    #[test]
    fn clean_singleton_repository_flattens_to_one_selectable_row() {
        let directory = tempfile::tempdir().unwrap();
        let repository_path = directory.path().join("repo");
        std::fs::create_dir(&repository_path).unwrap();
        let mut singleton = repository(repository_path.to_str().unwrap(), true);
        singleton.worktrees.truncate(1);
        let path = singleton.worktrees[0].path.clone();
        let mut app = App::new(vec![singleton], path.clone());
        app.statuses
            .insert(path.clone(), StatusState::Ready(WorktreeStatus::default()));
        app.ensure_selection_visible();

        assert_eq!(
            app.visible_rows()
                .into_iter()
                .map(|row| row.id().clone())
                .collect::<Vec<_>>(),
            vec![RowId::Repository(path.clone())]
        );
        assert_eq!(app.selected, Some(RowId::Repository(path.clone())));
        assert_eq!(
            app.selected_worktree()
                .map(|(_, worktree, _)| &worktree.path),
            Some(&path)
        );
        assert_eq!(
            app.handle_key(key(KeyCode::Enter)),
            Intent::Accept(path.clone())
        );
        app.handle_key(key(KeyCode::Char('F')));
        assert_eq!(
            app.focus_target,
            Some(FocusTarget::Repository(path.clone()))
        );
        assert_eq!(app.visible_rows().len(), 1);
    }

    #[test]
    fn focus_mode_toggles_repository_and_branch_scopes_without_persistence() {
        let mut app = App::new(
            vec![repository("/alpha", true), repository("/beta", true)],
            PathBuf::from("/elsewhere"),
        );
        assert!(!app.is_focused());

        app.selected = Some(RowId::Repository(PathBuf::from("/alpha")));
        assert_eq!(app.handle_key(key(KeyCode::Char('F'))), Intent::None);
        assert_eq!(app.focus_label().as_deref(), Some("alpha"));
        assert!(app.visible_rows().iter().all(|row| match row {
            VisibleRow::Repository {
                repository_index, ..
            }
            | VisibleRow::Worktree {
                repository_index, ..
            } => *repository_index == 0,
            _ => true,
        }));
        assert!(
            !app.visible_rows()
                .iter()
                .any(|row| row.id() == &RowId::Repository(PathBuf::from("/beta")))
        );

        assert_eq!(app.handle_key(key(KeyCode::Char('F'))), Intent::None);
        assert!(!app.is_focused());
        assert!(
            app.visible_rows()
                .iter()
                .any(|row| row.id() == &RowId::Repository(PathBuf::from("/beta")))
        );

        let branch = RowId::Worktree(PathBuf::from("/alpha-topic"));
        app.selected = Some(branch.clone());
        assert_eq!(app.handle_key(key(KeyCode::Char('F'))), Intent::None);
        assert_eq!(app.focus_label().as_deref(), Some("alpha: topic"));
        assert_eq!(
            app.visible_rows()
                .iter()
                .map(|row| row.id())
                .collect::<Vec<_>>(),
            vec![&branch]
        );
        assert!(matches!(
            app.visible_rows().first(),
            Some(VisibleRow::Worktree { depth: 0, .. })
        ));

        app.handle_key(key(KeyCode::Char('F')));
        app.repositories[0].worktrees[1].branch = None;
        app.repositories[0].worktrees[1].detached = true;
        app.selected = Some(branch);
        app.handle_key(key(KeyCode::Char('F')));
        assert_eq!(
            app.focus_label().as_deref(),
            Some("alpha: detached:12345678")
        );

        let fresh = App::new(
            vec![repository("/fresh", true)],
            PathBuf::from("/elsewhere"),
        );
        assert!(!fresh.is_focused());
    }

    #[test]
    fn focus_mode_uses_owning_branch_for_nested_and_detail_rows() {
        let mut app = App::new(vec![repository("/repo", true)], PathBuf::from("/elsewhere"));
        app.branch_parents
            .insert(PathBuf::from("/repo-topic"), PathBuf::from("/repo"));
        let owner = BranchId::Worktree(PathBuf::from("/repo"));
        app.selected = Some(RowId::Section(
            owner.clone(),
            InlineSection::StackedBranches,
        ));
        app.handle_key(key(KeyCode::Char('F')));
        let rows = app.visible_rows();
        assert!(matches!(
            rows.first(),
            Some(VisibleRow::Worktree { depth: 0, .. })
        ));
        assert!(rows.iter().any(|row| matches!(
            row,
            VisibleRow::Worktree {
                id: RowId::Worktree(path),
                depth: 2,
                ..
            } if path == &PathBuf::from("/repo-topic")
        )));
        assert!(
            !rows
                .iter()
                .any(|row| matches!(row, VisibleRow::Repository { .. }))
        );

        let (mut detail_app, identity) = filter_test_app();
        let detail_owner = BranchId::VirtualPullRequest(identity.clone());
        for section in [
            InlineSection::Overview,
            InlineSection::Checks,
            InlineSection::PendingChecks,
            InlineSection::ValidResults,
            InlineSection::Reviewers,
            InlineSection::OpenComments,
        ] {
            detail_app.set_disclosure_expanded(
                DisclosureKey::Section(detail_owner.clone(), section),
                true,
            );
        }
        let detail_rows = detail_app.logical_rows();
        for kind in [
            InlineRowKind::Section,
            InlineRowKind::Metadata,
            InlineRowKind::Check,
            InlineRowKind::Reviewer,
            InlineRowKind::OpenComment,
        ] {
            let selected = detail_rows
                .iter()
                .find(|row| {
                    matches!(row, VisibleRow::Inline { kind: candidate, .. } if *candidate == kind)
                })
                .expect("detail fixture should contain every inline row kind")
                .id()
                .clone();
            detail_app.selected = Some(selected);
            detail_app.handle_key(key(KeyCode::Char('F')));
            assert_eq!(
                detail_app.focus_target,
                Some(FocusTarget::Branch(detail_owner.clone()))
            );
            detail_app.handle_key(key(KeyCode::Char('F')));
        }
    }

    #[test]
    fn focus_mode_composes_with_search_and_clears_when_target_disappears() {
        let parent = authored("team", "project", 1, "2026-01-01");
        let unrelated = authored("team", "project", 2, "2026-01-02");
        let mut app = App::new(Vec::new(), PathBuf::from("/elsewhere"));
        replace_authored(
            &mut app,
            vec![parent.clone(), unrelated.clone()],
            vec![
                (parent.identity.clone(), None),
                (unrelated.identity.clone(), None),
            ],
        );
        app.selected = Some(RowId::VirtualRepository(parent.identity.repository.clone()));
        app.handle_key(key(KeyCode::Char('F')));
        assert_eq!(app.focus_label().as_deref(), Some("team/project"));
        assert!(matches!(
            app.visible_rows().first(),
            Some(VisibleRow::VirtualRepository { .. })
        ));
        app.handle_key(key(KeyCode::Char('F')));

        app.selected = Some(RowId::VirtualPullRequest(parent.identity.clone()));
        app.handle_key(key(KeyCode::Char('F')));
        assert!(app.is_focused());
        assert_eq!(app.focus_label().as_deref(), Some("team/project: topic-1"));

        app.set_committed_filter("topic-1");
        assert!(app.is_focused());
        assert!(
            app.visible_rows()
                .iter()
                .all(|row| row.id() != &RowId::VirtualPullRequest(unrelated.identity.clone()))
        );
        app.handle_key(key(KeyCode::Esc));
        assert!(app.is_focused());
        assert!(app.filter.is_empty());

        app.filter_active = true;
        app.handle_key(key(KeyCode::Char('F')));
        assert_eq!(app.filter, "F");
        assert!(app.is_focused());
        app.filter_active = false;
        app.filter.clear();

        replace_authored(
            &mut app,
            vec![unrelated.clone()],
            vec![(unrelated.identity.clone(), None)],
        );
        assert!(!app.is_focused());
    }

    #[test]
    fn focus_mode_rebases_backburner_as_a_named_root() {
        let parent = authored("team", "project", 1, "2026-01-01");
        let identity = parent.identity.repository.clone();
        let mut app = App::new(Vec::new(), PathBuf::from("/elsewhere"));
        app.virtual_repositories = vec![VirtualRepositoryView {
            identity: identity.clone(),
            mapped_repository: None,
            expanded: true,
            pull_requests: vec![parent.clone()],
        }];
        app.backburner.insert(parent.identity.clone());
        app.set_disclosure_expanded(DisclosureKey::Backburner(identity.clone()), true);
        app.selected = Some(RowId::Backburner(identity.clone()));

        app.handle_key(key(KeyCode::Char('F')));
        assert_eq!(
            app.focus_label().as_deref(),
            Some("team/project / Backburner")
        );
        assert!(matches!(
            app.visible_rows().first(),
            Some(VisibleRow::Backburner { depth: 0, .. })
        ));
        assert!(
            app.visible_rows()
                .iter()
                .any(|row| { row.id() == &RowId::VirtualPullRequest(parent.identity.clone()) })
        );
    }

    #[test]
    fn dirty_singleton_repository_has_no_worktree_subtree() {
        let mut singleton = repository("/repo", true);
        singleton.worktrees.truncate(1);
        let path = singleton.worktrees[0].path.clone();
        let mut app = App::new(vec![singleton], PathBuf::from("/elsewhere"));
        app.statuses.insert(
            path.clone(),
            StatusState::Ready(WorktreeStatus {
                unstaged: 1,
                ..WorktreeStatus::default()
            }),
        );

        assert_eq!(
            app.visible_rows()
                .into_iter()
                .map(|row| row.id().clone())
                .collect::<Vec<_>>(),
            vec![RowId::Repository(path)]
        );
    }

    #[test]
    fn singleton_repository_promotes_pull_request_sections_and_web_actions() {
        let authored = authored("team", "project", 42, "2026-01-01");
        let mut singleton = repository("/repo", true);
        singleton.worktrees.truncate(1);
        singleton
            .config
            .github_remotes
            .insert("origin".to_owned(), authored.identity.repository.clone());
        let path = singleton.worktrees[0].path.clone();
        let owner = BranchId::Worktree(path.clone());
        let mut app = App::new(vec![singleton], PathBuf::from("/elsewhere"));
        app.statuses
            .insert(path.clone(), StatusState::Ready(WorktreeStatus::default()));
        app.github.insert(
            path,
            GitHubState::Ready(GitHubBranchData {
                pull_request: Some(authored.pull_request),
                warnings: Vec::new(),
                rate_limit: None,
            }),
        );

        let rows = app.visible_rows();
        assert!(
            !rows
                .iter()
                .any(|row| matches!(row, VisibleRow::Worktree { .. }))
        );
        assert!(rows.iter().any(|row| {
            matches!(row, VisibleRow::Inline {
                id: RowId::Section(found, InlineSection::Overview),
                depth: 1,
                ..
            } if found == &owner)
        }));
        app.selected = Some(RowId::Repository(PathBuf::from("/repo")));
        assert_eq!(
            app.handle_key(key(KeyCode::Char('w'))),
            Intent::BeginAction(Action::OpenPullRequestWeb)
        );
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
        assert_eq!(
            app.handle_key(key(KeyCode::Char('w'))),
            Intent::BeginAction(Action::OpenPullRequestWeb)
        );
        for action in Action::ALL {
            assert_eq!(
                app.action_availability(action).enabled,
                matches!(
                    action,
                    Action::CopyAgentPrompt
                        | Action::CopyReviewRequest
                        | Action::OpenPullRequestWeb
                )
            );
        }
        app.handle_key(key(KeyCode::Char('h')));
        assert_eq!(
            app.selected,
            Some(RowId::VirtualPullRequest(selected.identity.clone()))
        );
        assert!(!app.visible_rows().iter().any(|row| {
            matches!(row, VisibleRow::Inline { owner: BranchId::VirtualPullRequest(identity), .. }
                if identity == &selected.identity)
        }));

        app.handle_key(key(KeyCode::Char('l')));
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
        assert!(
            app.selected.as_ref().is_some_and(|selected| {
                app.visible_rows().iter().any(|row| row.id() == selected)
            })
        );
        assert_eq!(
            app.branch_for_row_id(app.selected.as_ref().unwrap()),
            Some(BranchId::VirtualPullRequest(other.identity.clone()))
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
    fn stable_repository_mapping_survives_shifted_app_indexes_and_missing_paths() {
        let mut app = App::new(
            vec![repository("/session", true), repository("/mapped", true)],
            PathBuf::from("/session"),
        );
        app.repositories[0].session_only = true;
        let pull_request = authored("team", "project", 10, "2026-01-01T00:00:00Z");
        let generation = app.authored_pull_requests.begin();
        app.authored_pull_requests.apply_page(
            generation,
            "github.com".to_owned(),
            1,
            vec![pull_request.clone()],
            Vec::new(),
        );
        app.authored_pull_requests
            .finish(generation, true, Vec::new(), None);
        app.authored_mappings = vec![PullRequestMapping {
            identity: pull_request.identity.clone(),
            mapped_repository: Some(PathBuf::from("/mapped")),
        }];

        app.rebuild_virtual_repositories();

        assert_eq!(
            app.virtual_repositories[0].mapped_repository.as_deref(),
            Some(Path::new("/mapped"))
        );
        assert!(app.visible_rows().iter().any(|row| matches!(
            row,
            VisibleRow::VirtualPullRequest {
                mapped_repository_index: Some(1),
                ..
            }
        )));

        app.authored_mappings[0].mapped_repository = Some(PathBuf::from("/gone"));
        app.rebuild_virtual_repositories();

        assert!(app.virtual_repositories[0].mapped_repository.is_none());
        assert!(
            app.visible_rows()
                .iter()
                .any(|row| matches!(row, VisibleRow::VirtualRepository { .. }))
        );
    }

    #[test]
    fn multiple_github_identities_share_one_local_repository_forest() {
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
            0
        );
        assert_eq!(
            rows.iter()
                .filter(|row| matches!(
                    row,
                    VisibleRow::VirtualPullRequest {
                        mapped_repository_index: Some(0),
                        ..
                    }
                ))
                .count(),
            2
        );
    }

    #[test]
    fn backburner_keeps_cross_repository_stack_descendants_under_their_root() {
        let mut app = App::new(vec![repository("/repo", true)], PathBuf::from("/elsewhere"));
        let mut parent = authored("alpha", "project", 10, "2026-01-01T00:00:00Z");
        parent.pull_request.head.branch = "stack-parent".to_owned();
        let mut child = authored("beta", "project", 11, "2026-01-02T00:00:00Z");
        child.pull_request.base = parent.pull_request.head.clone();
        child.pull_request.head.branch = "stack-child".to_owned();
        replace_authored(
            &mut app,
            vec![parent.clone(), child.clone()],
            vec![
                (parent.identity.clone(), Some(0)),
                (child.identity.clone(), Some(0)),
            ],
        );
        app.backburner.insert(parent.identity.clone());
        app.set_disclosure_expanded(
            DisclosureKey::Backburner(parent.identity.repository.clone()),
            true,
        );

        let rows = app.visible_rows();
        assert_eq!(
            rows.iter()
                .filter(|row| matches!(row, VisibleRow::Backburner { .. }))
                .count(),
            1
        );
        let backburner = rows
            .iter()
            .position(|row| row.id() == &RowId::Backburner(parent.identity.repository.clone()))
            .unwrap();
        let parent_row = rows
            .iter()
            .position(|row| row.id() == &RowId::VirtualPullRequest(parent.identity.clone()))
            .unwrap();
        let child_row = rows
            .iter()
            .position(|row| row.id() == &RowId::VirtualPullRequest(child.identity.clone()))
            .unwrap();
        assert!(backburner < parent_row && parent_row < child_row);
        app.selected = Some(RowId::Backburner(parent.identity.repository.clone()));
        let review_request = app.review_request().unwrap();
        assert!(review_request.contains(&parent.pull_request.url));
        assert!(review_request.contains(&child.pull_request.url));
    }

    #[test]
    fn navigation_filter_and_inline_collapse_are_reducer_driven() {
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
        app.handle_key(key(KeyCode::Tab));
        assert_eq!(app.visible_rows().len(), 1);
        app.handle_key(key(KeyCode::Char('l')));
        assert_eq!(app.visible_rows().len(), 3);
        app.handle_key(key(KeyCode::Char('/')));
        app.handle_key(key(KeyCode::Char('t')));
        app.handle_key(key(KeyCode::Char('o')));
        app.handle_key(key(KeyCode::Char('p')));
        app.handle_key(key(KeyCode::Char('i')));
        app.handle_key(key(KeyCode::Char('c')));
        assert!(
            app.visible_rows()
                .iter()
                .any(|row| { row.id() == &RowId::Worktree(PathBuf::from("/repo-topic")) })
        );
        app.handle_key(key(KeyCode::Enter));
        app.selected = Some(RowId::Worktree(PathBuf::from("/repo-topic")));
        app.handle_key(key(KeyCode::Char('h')));
        assert_eq!(
            app.selected,
            Some(RowId::Worktree(PathBuf::from("/repo-topic")))
        );
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
    fn cursor_navigation_builds_visible_rows_once_and_preserves_scroll() {
        let repositories = (0..20)
            .map(|index| repository(&format!("/repo-{index}"), true))
            .collect();
        let mut app = App::new(repositories, PathBuf::from("/elsewhere"));
        app.set_viewport_height(6);

        app.reset_visible_row_builds();
        app.handle_key(key(KeyCode::Char('j')));
        assert_eq!(app.visible_row_builds(), 1);

        for _ in 0..20 {
            app.reset_visible_row_builds();
            app.handle_key(key(KeyCode::Char('j')));
            assert_eq!(app.visible_row_builds(), 1);
        }
        assert!(app.scroll > 0);
        for _ in 0..10 {
            app.reset_visible_row_builds();
            app.handle_key(key(KeyCode::Char('k')));
            assert_eq!(app.visible_row_builds(), 1);
        }
        let rows = app.visible_rows();
        let selected = app.selected.as_ref().unwrap();
        let index = rows.iter().position(|row| row.id() == selected).unwrap();
        assert!(index >= app.scroll);
        assert!(index < app.scroll + app.viewport_height);
    }

    #[test]
    fn repository_refresh_recomputes_cached_current_worktree() {
        let directory = tempfile::tempdir().unwrap();
        let first = directory.path().join("first");
        let second = directory.path().join("second");
        let current = first.join("nested");
        std::fs::create_dir_all(&current).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        let mut initial = repository(first.to_str().unwrap(), true);
        initial.worktrees.truncate(1);
        let mut app = App::new(vec![initial], current);
        assert!(app.is_current_worktree(&first));

        let mut replacement = repository(second.to_str().unwrap(), true);
        replacement.worktrees.truncate(1);
        app.replace_repositories(vec![replacement]);
        assert!(!app.is_current_worktree(&first));
        assert!(!app.is_current_worktree(&second));
    }

    #[test]
    fn initial_selection_does_not_scroll_when_already_visible() {
        let repositories = (0..6)
            .map(|index| repository(&format!("/repo-{index}"), true))
            .collect();
        let mut app = App::new(repositories, PathBuf::from("/repo-1"));

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
    fn half_page_navigation_moves_the_unified_list_selection() {
        let repositories = (0..6)
            .map(|index| repository(&format!("/repo-{index}"), true))
            .collect();
        let mut app = App::new(repositories, PathBuf::from("/elsewhere"));
        app.set_viewport_height(6);
        let selected = app.selected.clone();

        app.handle_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL));
        assert_ne!(app.selected, selected);
        assert!(app.scroll > 0);
        let moved = app.selected.clone();
        app.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_ne!(app.selected, moved);
    }

    #[test]
    fn comment_text_removes_markdown_and_separates_later_sections() {
        assert_eq!(
            concise_comment_text(
                "<!-- metadata -->\n# Summary\n\n**plain** text\n- first item\n  - **second** item"
            ),
            "Summary • plain text • first item • second item"
        );
        assert_eq!(
            concise_comment_text("intro\n## Details\nmore detail"),
            "intro • Details • more detail"
        );
        assert_eq!(
            concise_comment_text("  - first item\n- second item"),
            "first item • second item"
        );
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
                    body: concat!(
                        "<!-- devin-review-comment {\"id\": \"BUG_0001\"} -->\n",
                        "## Summary\n",
                        "**line one**\n",
                        "  - first item\n",
                        "- **second** item\n"
                    )
                    .to_owned(),
                    path: Some("src/lib.rs".to_owned()),
                    permalink: Some("https://comments/7".to_owned()),
                    outdated: false,
                }],
                feedback_complete: true,
                ..PullRequestDetails::default()
            },
        );
        let owner = BranchId::VirtualPullRequest(identity.clone());
        let initial_rows = app.visible_rows();
        assert!(initial_rows.iter().any(|row| {
            matches!(row, VisibleRow::Inline { id: RowId::Section(found_owner, InlineSection::Overview), expanded: Some(false), .. }
                if found_owner == &owner)
        }));
        assert!(initial_rows.iter().any(|row| {
            matches!(row, VisibleRow::Inline { id: RowId::Section(found_owner, InlineSection::Checks), expanded: Some(true), .. }
                if found_owner == &owner)
        }));
        assert!(initial_rows.iter().any(|row| {
            matches!(row, VisibleRow::Inline { id: RowId::Section(found_owner, InlineSection::OpenComments), expanded: Some(true), .. }
                if found_owner == &owner)
        }));
        assert!(initial_rows.iter().any(|row| {
            matches!(row, VisibleRow::Inline { id: RowId::OpenComment(found, _), text, .. }
                if found == &identity
                    && text == "@reviewer Summary • line one • first item • second item (src/lib.rs)")
        }));
        app.selected = Some(RowId::Section(owner.clone(), InlineSection::Overview));
        app.handle_key(key(KeyCode::Char('l')));
        app.authored_pull_requests.loading = true;
        assert!(app.visible_rows().iter().any(
            |row| matches!(row, VisibleRow::Inline { text, .. } if text == "GitHub: refreshing")
        ));
        app.authored_pull_requests.loading = false;
        app.set_viewport_height(4);
        app.handle_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL));
        assert_ne!(
            app.selected,
            Some(RowId::Section(owner.clone(), InlineSection::Overview))
        );
        assert!(
            app.visible_rows()
                .iter()
                .any(|row| matches!(row.id(), RowId::Check(_, name) if name == "failure"))
        );
        app.selected = Some(RowId::Section(owner.clone(), InlineSection::Checks));
        app.handle_key(key(KeyCode::Char('l')));
        let rows = app.visible_rows();
        let check_names: Vec<_> = rows
            .iter()
            .filter_map(|row| match row.id() {
                RowId::Check(_, name) => Some(name.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(check_names, vec!["failure"]);
        assert!(rows.iter().any(|row| {
            matches!(
                row,
                VisibleRow::Inline {
                    id: RowId::Section(_, InlineSection::PendingChecks),
                    expanded: Some(false),
                    ..
                }
            )
        }));
        assert!(rows.iter().any(|row| {
            matches!(
                row,
                VisibleRow::Inline {
                    id: RowId::Section(_, InlineSection::ValidResults),
                    expanded: Some(false),
                    ..
                }
            )
        }));
        assert_eq!(
            app.handle_key(key(KeyCode::Enter)),
            Intent::OpenUrl(format!("{}/checks", authored.pull_request.url))
        );
        assert_eq!(
            app.handle_key(key(KeyCode::Char('w'))),
            Intent::OpenUrl(format!("{}/checks", authored.pull_request.url))
        );

        app.selected = Some(RowId::Check(identity.clone(), "failure".to_owned()));
        app.handle_key(key(KeyCode::Char('h')));
        assert_eq!(
            app.selected,
            Some(RowId::Section(owner.clone(), InlineSection::Checks))
        );
        assert!(
            !app.visible_rows()
                .iter()
                .any(|row| matches!(row.id(), RowId::Check(_, _)))
        );
        app.handle_key(key(KeyCode::Char('l')));
        app.selected = Some(RowId::Check(identity.clone(), "failure".to_owned()));
        assert_eq!(
            app.handle_key(key(KeyCode::Char('w'))),
            Intent::OpenUrl("https://checks/failure".to_owned())
        );
        app.selected = Some(RowId::Section(owner.clone(), InlineSection::ValidResults));
        app.handle_key(key(KeyCode::Char('l')));
        app.selected = Some(RowId::Check(identity.clone(), "success".to_owned()));
        assert_eq!(
            app.handle_key(key(KeyCode::Enter)),
            Intent::OpenUrl(authored.pull_request.url.clone())
        );
        app.selected = Some(RowId::OpenComment(identity.clone(), feedback_id));
        assert_eq!(
            app.handle_key(key(KeyCode::Char('w'))),
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
            app.selected
                .as_ref()
                .is_some_and(|selected| app.visible_rows().iter().any(|row| row.id() == selected))
        );
        assert!(app.visible_rows().iter().any(|row| {
            matches!(row, VisibleRow::Inline { id: RowId::Section(_, InlineSection::Checks), text, .. }
                if text == "Checks · counts:0:0:0")
        }));
    }

    #[test]
    fn merged_pull_requests_omit_active_sections_but_keep_open_comments() {
        let mut merged = authored("team", "project", 42, "2026-01-01");
        merged.pull_request.state = crate::model::PullRequestState::Merged;
        let identity = merged.identity.clone();
        let mut app = App::new(Vec::new(), PathBuf::from("/elsewhere"));
        app.virtual_repositories = vec![VirtualRepositoryView {
            identity: identity.repository.clone(),
            mapped_repository: None,
            expanded: true,
            pull_requests: vec![merged],
        }];
        app.pull_request_details.insert(
            identity.clone(),
            PullRequestDetails {
                checks: vec![crate::model::PullRequestCheck {
                    name: "ci".to_owned(),
                    state: crate::model::CheckState::Success,
                    target_url: None,
                    required: true,
                    source_order: 0,
                    completed_at: None,
                }],
                check_contexts_complete: true,
                reviewer_reviews: vec![crate::model::ReviewerReview {
                    id: "review".to_owned(),
                    database_id: None,
                    reviewer: "reviewer".to_owned(),
                    state: crate::model::SubmittedReviewState::Approved,
                    submitted_at: None,
                }],
                reviews_complete: true,
                feedback: vec![crate::model::PullRequestFeedback {
                    id: "comment".to_owned(),
                    database_id: None,
                    thread_id: Some("thread".to_owned()),
                    kind: crate::model::FeedbackKind::InlineThread,
                    author: "reviewer".to_owned(),
                    body: "  still relevant\n after merge  ".to_owned(),
                    path: None,
                    permalink: None,
                    outdated: false,
                }],
                feedback_complete: true,
                ..PullRequestDetails::default()
            },
        );

        let rows = app.visible_rows();
        for section in [
            InlineSection::Overview,
            InlineSection::Checks,
            InlineSection::Reviewers,
        ] {
            assert!(
                !rows.iter().any(|row| {
                    matches!(row.id(), RowId::Section(_, found) if *found == section)
                })
            );
        }
        assert!(rows.iter().any(|row| {
            matches!(row, VisibleRow::Inline {
                id: RowId::OpenComment(found, _), text, ..
            } if found == &identity && text == "@reviewer still relevant • after merge")
        }));
        assert!(rows.iter().any(|row| {
            matches!(row, VisibleRow::VirtualPullRequest {
                id: RowId::VirtualPullRequest(found), has_children: true, ..
            } if found == &identity)
        }));

        app.pull_request_details
            .get_mut(&identity)
            .unwrap()
            .feedback
            .clear();
        assert!(app.visible_rows().iter().any(|row| {
            matches!(row, VisibleRow::VirtualPullRequest {
                id: RowId::VirtualPullRequest(found), has_children: false, ..
            } if found == &identity)
        }));
        app.selected = Some(RowId::VirtualPullRequest(identity));
        let disclosures = app.disclosure_expanded.clone();
        app.handle_key(key(KeyCode::Char('l')));
        assert_eq!(app.disclosure_expanded, disclosures);
    }

    #[test]
    fn rollup_sections_group_checks_combine_reviewers_and_omit_empty_comments() {
        let authored = authored("team", "project", 42, "2026-01-01");
        let identity = authored.identity.clone();
        let owner = BranchId::VirtualPullRequest(identity.clone());
        let mut app = App::new(Vec::new(), PathBuf::from("/elsewhere"));
        app.virtual_repositories = vec![VirtualRepositoryView {
            identity: identity.repository.clone(),
            mapped_repository: None,
            expanded: true,
            pull_requests: vec![authored],
        }];
        let check = |name: &str, state, order| crate::model::PullRequestCheck {
            name: name.to_owned(),
            state,
            target_url: None,
            required: true,
            source_order: order,
            completed_at: None,
        };
        app.pull_request_details.insert(
            identity.clone(),
            PullRequestDetails {
                checks: vec![
                    check("failure", crate::model::CheckState::Failure, 0),
                    check("pending", crate::model::CheckState::Pending, 1),
                    check("unknown", crate::model::CheckState::Unknown, 2),
                    check("success", crate::model::CheckState::Success, 3),
                ],
                check_contexts_complete: false,
                review_requests: vec![crate::model::ReviewRequest {
                    id: "alice-request".to_owned(),
                    name: "Alice".to_owned(),
                    kind: crate::model::ReviewerKind::User,
                }],
                reviewer_reviews: vec![
                    crate::model::ReviewerReview {
                        id: "bob-review".to_owned(),
                        database_id: None,
                        reviewer: "Bob".to_owned(),
                        state: crate::model::SubmittedReviewState::Approved,
                        submitted_at: None,
                    },
                    crate::model::ReviewerReview {
                        id: "carol-review".to_owned(),
                        database_id: None,
                        reviewer: "Carol".to_owned(),
                        state: crate::model::SubmittedReviewState::ChangesRequested,
                        submitted_at: None,
                    },
                ],
                reviews_complete: true,
                feedback: vec![
                    crate::model::PullRequestFeedback {
                        id: "summary".to_owned(),
                        database_id: None,
                        thread_id: None,
                        kind: crate::model::FeedbackKind::ReviewSummary,
                        author: "Bob".to_owned(),
                        body: "Please address the edge case".to_owned(),
                        path: None,
                        permalink: Some("https://comments/summary".to_owned()),
                        outdated: false,
                    },
                    crate::model::PullRequestFeedback {
                        id: "thread".to_owned(),
                        database_id: Some(9),
                        thread_id: Some("thread-id".to_owned()),
                        kind: crate::model::FeedbackKind::InlineThread,
                        author: "Carol".to_owned(),
                        body: "Old code needs fixing".to_owned(),
                        path: Some("src/lib.rs".to_owned()),
                        permalink: Some("https://comments/thread".to_owned()),
                        outdated: true,
                    },
                ],
                feedback_complete: true,
                ..PullRequestDetails::default()
            },
        );

        let rows = app.visible_rows();
        assert!(rows.iter().any(|row| {
            matches!(row, VisibleRow::Inline {
                id: RowId::Section(found, InlineSection::Checks), text, expanded: Some(true), ..
            } if found == &owner && text == "Checks · counts:1:0:1")
        }));
        assert!(rows.iter().any(|row| {
            matches!(row, VisibleRow::Inline {
                id: RowId::Section(found, InlineSection::Reviewers), text, expanded: Some(false), ..
            } if found == &owner
                && text.contains("req")
                && text.contains("✓ approved")
                && text.contains("✗ changes"))
        }));
        for reviewer in ["alice", "bob", "carol"] {
            assert!(!rows.iter().any(|row| {
                row.id() == &RowId::Reviewer(identity.clone(), reviewer.to_owned())
            }));
        }
        assert!(!rows.iter().any(|row| {
            matches!(row, VisibleRow::Inline { text, .. } if text == "Please address the edge case")
        }));
        app.set_disclosure_expanded(
            DisclosureKey::Section(owner.clone(), InlineSection::Reviewers),
            true,
        );
        let rows = app.visible_rows();
        for reviewer in ["alice", "bob", "carol"] {
            assert!(rows.iter().any(|row| {
                row.id() == &RowId::Reviewer(identity.clone(), reviewer.to_owned())
            }));
        }
        assert!(!rows.iter().any(|row| {
            matches!(row, VisibleRow::Inline { text, .. } if text == "Please address the edge case")
        }));
        assert!(rows.iter().any(|row| {
            matches!(row, VisibleRow::Inline {
                id: RowId::OpenComment(found, id), text, url: Some(url), ..
            } if found == &identity
                && id == "thread"
                && text == "@Carol Old code needs fixing (src/lib.rs) [outdated]"
                && url == "https://comments/thread")
        }));

        app.set_disclosure_expanded(
            DisclosureKey::Section(owner.clone(), InlineSection::Checks),
            true,
        );
        let rows = app.visible_rows();
        for direct in ["failure", "unknown"] {
            assert!(rows.iter().any(|row| {
                matches!(row, VisibleRow::Inline {
                    section: InlineSection::Checks,
                    id: RowId::Check(found, name), ..
                } if found == &identity && name == direct)
            }));
        }
        assert!(rows.iter().any(|row| {
            matches!(
                row,
                VisibleRow::Inline {
                    id: RowId::Section(_, InlineSection::PendingChecks),
                    expanded: Some(false),
                    ..
                }
            )
        }));
        assert!(rows.iter().any(|row| {
            matches!(
                row,
                VisibleRow::Inline {
                    id: RowId::Section(_, InlineSection::ValidResults),
                    expanded: Some(false),
                    ..
                }
            )
        }));
        assert!(!rows.iter().any(|row| {
            matches!(row.id(), RowId::Check(_, name) if name == "pending" || name == "success")
        }));

        app.pull_request_details
            .get_mut(&identity)
            .unwrap()
            .checks
            .iter_mut()
            .find(|check| check.name == "pending")
            .unwrap()
            .state = crate::model::CheckState::Failure;
        assert!(app.visible_rows().iter().any(|row| {
            matches!(row, VisibleRow::Inline {
                section: InlineSection::Checks,
                id: RowId::Check(found, name), ..
            } if found == &identity && name == "pending")
        }));

        app.pull_request_details
            .get_mut(&identity)
            .unwrap()
            .feedback
            .retain(|feedback| feedback.kind == crate::model::FeedbackKind::ReviewSummary);
        assert!(
            !app.visible_rows()
                .iter()
                .any(|row| { matches!(row.id(), RowId::Section(_, InlineSection::OpenComments)) })
        );
    }

    #[test]
    fn unavailable_pull_request_details_keep_unknown_attention_headers_visible() {
        let authored = authored("team", "project", 7, "2026-01-01");
        let identity = authored.identity.clone();
        let mut app = App::new(Vec::new(), PathBuf::from("/elsewhere"));
        app.virtual_repositories = vec![VirtualRepositoryView {
            identity: identity.repository.clone(),
            mapped_repository: None,
            expanded: true,
            pull_requests: vec![authored],
        }];
        let rows = app.visible_rows();
        assert!(rows.iter().any(|row| {
            matches!(row, VisibleRow::Inline { text, .. } if text == "Checks · counts:?:?:?")
        }));
        assert!(rows.iter().any(|row| {
            matches!(row, VisibleRow::Inline { text, .. } if text == "Reviewers · ○ unknown")
        }));
        app.set_disclosure_expanded(
            DisclosureKey::Section(
                BranchId::VirtualPullRequest(identity.clone()),
                InlineSection::Overview,
            ),
            true,
        );
        assert!(app.visible_rows().iter().any(|row| {
            matches!(row, VisibleRow::Inline { text, .. }
                if text == "attention details: loading or unavailable")
        }));
    }

    #[test]
    fn agent_prompt_scopes_are_exact_deterministic_and_collapse_independent() {
        let mut parent = authored("team", "project", 1, "2026-01-01");
        parent.pull_request.head.branch = "stack-parent".to_owned();
        parent.pull_request.title = "feat: parent title".to_owned();
        let mut child = authored("team", "project", 2, "2026-01-02");
        child.pull_request.base = parent.pull_request.head.clone();
        child.pull_request.head.branch = "stack-child".to_owned();
        child.pull_request.title = "fix(scope): child title".to_owned();
        child.pull_request.state = crate::model::PullRequestState::Draft;
        let mut unrelated = authored("team", "project", 3, "2026-01-03");
        unrelated.pull_request.title = "third title".to_owned();
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
                    checks: vec![
                        crate::model::PullRequestCheck {
                            name: format!("check-{}", pull_request.identity.number),
                            state: crate::model::CheckState::Failure,
                            target_url: None,
                            required: true,
                            source_order: 0,
                            completed_at: None,
                        },
                        crate::model::PullRequestCheck {
                            name: format!("valid-{}", pull_request.identity.number),
                            state: crate::model::CheckState::Success,
                            target_url: None,
                            required: true,
                            source_order: 1,
                            completed_at: None,
                        },
                    ],
                    check_contexts_complete: true,
                    feedback: vec![
                        crate::model::PullRequestFeedback {
                            id: format!("feedback-{}", pull_request.identity.number),
                            database_id: None,
                            thread_id: None,
                            kind: crate::model::FeedbackKind::InlineThread,
                            author: "reviewer".to_owned(),
                            body: format!("body {}", pull_request.identity.number),
                            path: None,
                            permalink: None,
                            outdated: false,
                        },
                        crate::model::PullRequestFeedback {
                            id: format!("review-{}", pull_request.identity.number),
                            database_id: None,
                            thread_id: None,
                            kind: crate::model::FeedbackKind::ReviewSummary,
                            author: "reviewer".to_owned(),
                            body: format!("review body {}", pull_request.identity.number),
                            path: None,
                            permalink: None,
                            outdated: false,
                        },
                    ],
                    ..PullRequestDetails::default()
                },
            );
        }
        app.selected = Some(RowId::VirtualPullRequest(parent.identity.clone()));
        assert_eq!(
            app.handle_key(key(KeyCode::Char('c'))),
            Intent::BeginAction(Action::CopyAgentPrompt)
        );
        let expanded = app.agent_prompt().unwrap();
        assert!(expanded.contains("(#1 "));
        assert!(expanded.contains("(#2 "));
        assert!(!expanded.contains("(#3 "));
        assert!(expanded.find("(#1 ").unwrap() < expanded.find("(#2 ").unwrap());
        assert!(expanded.contains("feedback-1"));
        assert!(expanded.contains("feedback-2"));
        assert!(!expanded.contains("review-1"));
        assert!(!expanded.contains("review-2"));

        app.virtual_repositories[0].expanded = false;
        assert_eq!(app.agent_prompt().unwrap(), expanded);

        app.selected = Some(RowId::Check(parent.identity.clone(), "check-1".to_owned()));
        let single = app.agent_prompt().unwrap();
        assert!(single.contains("Checks (all failed):"));
        assert!(single.contains("check-1"));
        assert!(!single.contains("feedback-1"));
        assert!(!single.contains("PR #2"));

        app.selected = Some(RowId::Section(
            BranchId::VirtualPullRequest(parent.identity.clone()),
            InlineSection::OpenComments,
        ));
        let feedback = app.agent_prompt().unwrap();
        assert!(feedback.contains("feedback-1"));
        assert!(!feedback.contains("check-1"));
        assert!(!feedback.contains("(#2 "));
        assert!(!feedback.contains("Checks"));
        assert!(!feedback.contains("failing checks"));

        app.selected = Some(RowId::Reviewer(
            parent.identity.clone(),
            "reviewer".to_owned(),
        ));
        let reviewer = app.agent_prompt().unwrap();
        assert!(reviewer.contains("review-1"));
        assert!(!reviewer.contains("feedback-1"));
        assert!(!reviewer.contains("check-1"));
        app.selected = Some(RowId::Section(
            BranchId::VirtualPullRequest(parent.identity.clone()),
            InlineSection::Reviewers,
        ));
        assert_eq!(app.agent_prompt().as_deref(), Some(reviewer.as_str()));

        app.selected = Some(RowId::Check(parent.identity.clone(), "valid-1".to_owned()));
        let valid = app.agent_prompt().unwrap();
        assert!(valid.contains("Checks:\n  - valid-1 ("));
        assert!(!valid.contains("check-1"));

        app.selected = Some(RowId::Section(
            BranchId::VirtualPullRequest(parent.identity.clone()),
            InlineSection::PendingChecks,
        ));
        assert_eq!(app.agent_prompt(), None);

        app.selected = Some(RowId::Section(
            BranchId::VirtualPullRequest(parent.identity.clone()),
            InlineSection::StackedBranches,
        ));
        let child_prompt = app.agent_prompt().unwrap();
        assert!(!child_prompt.contains("(#1 "));
        assert!(child_prompt.contains("(#2 "));
        assert!(!child_prompt.contains("(#3 "));

        app.selected = Some(RowId::VirtualPullRequest(parent.identity.clone()));
        let review_request = app.review_request().unwrap();
        assert_eq!(
            review_request,
            format!(
                "{} - parent title\n{} - child title - DRAFT",
                parent.pull_request.url, child.pull_request.url
            )
        );
        app.set_disclosure_expanded(
            DisclosureKey::Branch(BranchId::VirtualPullRequest(parent.identity.clone())),
            false,
        );
        app.filter = "no visible match".to_owned();
        assert_eq!(
            app.review_request().as_deref(),
            Some(review_request.as_str())
        );
        app.filter.clear();

        app.selected = Some(RowId::Section(
            BranchId::VirtualPullRequest(parent.identity.clone()),
            InlineSection::StackedBranches,
        ));
        assert_eq!(
            app.review_request().as_deref(),
            Some(format!("{} - child title - DRAFT", child.pull_request.url).as_str())
        );

        app.selected = Some(RowId::Check(parent.identity.clone(), "check-1".to_owned()));
        assert_eq!(
            app.review_request().as_deref(),
            Some(format!("{} - parent title", parent.pull_request.url).as_str())
        );

        app.selected = Some(RowId::VirtualRepository(parent.identity.repository.clone()));
        assert_eq!(
            app.review_request().as_deref(),
            Some(
                format!(
                    "{} - third title\n{} - parent title\n{} - child title - DRAFT",
                    unrelated.pull_request.url, parent.pull_request.url, child.pull_request.url
                )
                .as_str()
            )
        );
    }

    #[test]
    fn backburner_groups_whole_stacks_filters_prompts_and_wraps_attention_navigation() {
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
            pull_requests: vec![child.clone(), unrelated.clone(), parent.clone()],
        }];
        for pull_request in [&parent, &child, &unrelated] {
            app.pull_request_details.insert(
                pull_request.identity.clone(),
                PullRequestDetails {
                    checks: vec![crate::model::PullRequestCheck {
                        name: format!("failure-{}", pull_request.identity.number),
                        state: crate::model::CheckState::Failure,
                        target_url: None,
                        required: true,
                        source_order: 0,
                        completed_at: None,
                    }],
                    check_contexts_complete: true,
                    ..PullRequestDetails::default()
                },
            );
        }
        app.selected = Some(RowId::VirtualPullRequest(parent.identity.clone()));
        assert_eq!(
            app.handle_key(key(KeyCode::Char('b'))),
            Intent::PersistBackburner
        );
        assert_eq!(
            app.backburner,
            BTreeSet::from([parent.identity.clone(), child.identity.clone()])
        );
        assert_eq!(
            app.selected,
            Some(RowId::Backburner(parent.identity.repository.clone()))
        );
        let collapsed = app.visible_rows();
        assert_eq!(
            collapsed
                .iter()
                .filter(|row| matches!(row, VisibleRow::Backburner { .. }))
                .count(),
            1
        );
        assert_eq!(
            collapsed
                .iter()
                .filter(|row| matches!(row, VisibleRow::VirtualPullRequest { .. }))
                .count(),
            1
        );
        let group_prompt = app.agent_prompt().unwrap();
        assert!(group_prompt.contains("(#1 "));
        assert!(group_prompt.contains("(#2 "));
        assert!(!group_prompt.contains("(#3 "));
        let group_review_request = app.review_request().unwrap();
        assert!(group_review_request.contains(&parent.pull_request.url));
        assert!(group_review_request.contains(&child.pull_request.url));
        assert!(!group_review_request.contains(&unrelated.pull_request.url));

        app.filter = "failure-1".to_owned();
        assert!(
            app.visible_rows()
                .iter()
                .any(|row| { row.id() == &RowId::VirtualPullRequest(parent.identity.clone()) })
        );
        assert!(
            app.visible_rows()
                .iter()
                .any(|row| { row.id() == &RowId::Backburner(parent.identity.repository.clone()) })
        );
        app.filter.clear();

        app.selected = Some(RowId::VirtualRepository(parent.identity.repository.clone()));
        let repository_prompt = app.agent_prompt().unwrap();
        assert!(!repository_prompt.contains("(#1 "));
        assert!(!repository_prompt.contains("(#2 "));
        let repository_review_request = app.review_request().unwrap();
        assert!(!repository_review_request.contains(&parent.pull_request.url));
        assert!(!repository_review_request.contains(&child.pull_request.url));
        assert!(repository_review_request.contains(&unrelated.pull_request.url));
        assert!(repository_prompt.contains("(#3 "));

        app.selected = Some(RowId::Backburner(parent.identity.repository.clone()));
        app.handle_key(key(KeyCode::Enter));
        let expanded = app.visible_rows();
        for identity in [&parent.identity, &child.identity, &unrelated.identity] {
            assert_eq!(
                expanded
                    .iter()
                    .filter(|row| row.id() == &RowId::VirtualPullRequest(identity.clone()))
                    .count(),
                1
            );
        }

        app.handle_key(key(KeyCode::Char(']')));
        assert_eq!(
            app.selected,
            Some(RowId::VirtualPullRequest(unrelated.identity.clone()))
        );
        app.handle_key(key(KeyCode::Char(']')));
        assert_eq!(
            app.selected,
            Some(RowId::VirtualPullRequest(unrelated.identity.clone()))
        );
        app.handle_key(key(KeyCode::Char('[')));
        assert_eq!(
            app.selected,
            Some(RowId::VirtualPullRequest(unrelated.identity.clone()))
        );
        app.backburner.insert(unrelated.identity);
        let selected = app.selected.clone();
        app.handle_key(key(KeyCode::Char(']')));
        assert_eq!(app.selected, selected);
        let membership = app.backburner.clone();
        app.rebuild_virtual_repositories();
        assert_eq!(
            app.backburner, membership,
            "incomplete refreshes never prune state"
        );
    }

    #[test]
    fn virtual_descendants_follow_a_backburnered_parent_into_the_backburner_tree() {
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
            pull_requests: vec![child.clone(), unrelated.clone(), parent.clone()],
        }];
        app.backburner.insert(parent.identity.clone());

        let collapsed = app.visible_rows();
        assert!(
            collapsed
                .iter()
                .any(|row| row.id() == &RowId::VirtualPullRequest(unrelated.identity.clone()))
        );
        for identity in [&parent.identity, &child.identity] {
            assert!(
                !collapsed
                    .iter()
                    .any(|row| row.id() == &RowId::VirtualPullRequest(identity.clone()))
            );
        }

        app.set_disclosure_expanded(
            DisclosureKey::Backburner(parent.identity.repository.clone()),
            true,
        );
        let expanded = app.visible_rows();
        let backburner = expanded
            .iter()
            .position(|row| row.id() == &RowId::Backburner(parent.identity.repository.clone()))
            .unwrap();
        let parent_row = expanded
            .iter()
            .position(|row| row.id() == &RowId::VirtualPullRequest(parent.identity.clone()))
            .unwrap();
        let child_row = expanded
            .iter()
            .position(|row| row.id() == &RowId::VirtualPullRequest(child.identity.clone()))
            .unwrap();
        assert!(backburner < parent_row && parent_row < child_row);
    }

    #[test]
    fn backburnering_a_mixed_stack_moves_the_local_base_and_virtual_children_together() {
        let mut parent = authored("team", "project", 10, "2026-01-01");
        parent.pull_request.head.branch = "stack-parent".to_owned();
        let mut child = authored("team", "project", 11, "2026-01-02");
        child.pull_request.base = parent.pull_request.head.clone();
        let repository_identity = parent.identity.repository.clone();
        let mut local = repository("/repo", true);
        local
            .config
            .github_remotes
            .insert("origin".to_owned(), repository_identity.clone());
        let local_path = local.worktrees[1].path.clone();
        let mut app = App::new(vec![local], PathBuf::from("/elsewhere"));
        app.github.insert(
            local_path.clone(),
            GitHubState::Ready(GitHubBranchData {
                pull_request: Some(parent.pull_request.clone()),
                warnings: Vec::new(),
                rate_limit: None,
            }),
        );
        app.virtual_repositories = vec![VirtualRepositoryView {
            identity: repository_identity,
            mapped_repository: Some(PathBuf::from("/repo")),
            expanded: true,
            pull_requests: vec![child.clone()],
        }];
        app.selected = Some(RowId::Worktree(local_path.clone()));

        assert_eq!(
            app.handle_key(key(KeyCode::Char('b'))),
            Intent::PersistBackburner
        );
        assert!(app.backburner.contains(&parent.identity));
        assert!(app.backburner.contains(&child.identity));
        let rows = app.visible_rows();
        assert!(
            !rows
                .iter()
                .any(|row| row.id() == &RowId::Worktree(local_path.clone()))
        );
        assert!(
            rows.iter()
                .any(|row| matches!(row, VisibleRow::Backburner { .. }))
        );
        assert!(
            !rows
                .iter()
                .any(|row| { row.id() == &RowId::VirtualPullRequest(child.identity.clone()) })
        );

        app.set_disclosure_expanded(
            DisclosureKey::Backburner(parent.identity.repository.clone()),
            true,
        );
        let expanded = app.visible_rows();
        let parent_row = expanded
            .iter()
            .position(|row| row.id() == &RowId::Worktree(local_path.clone()))
            .unwrap();
        let stack_header = expanded
            .iter()
            .position(|row| {
                row.id()
                    == &RowId::Section(
                        BranchId::Worktree(local_path.clone()),
                        InlineSection::StackedBranches,
                    )
            })
            .unwrap();
        let child_row = expanded
            .iter()
            .position(|row| row.id() == &RowId::VirtualPullRequest(child.identity.clone()))
            .unwrap();
        assert!(parent_row < stack_header && stack_header < child_row);
    }

    #[test]
    fn enter_in_non_pr_details_never_accepts_the_worktree() {
        let mut app = App::new(vec![repository("/repo", true)], PathBuf::from("/elsewhere"));
        app.selected = Some(RowId::Metadata(
            BranchId::Worktree(PathBuf::from("/repo")),
            "worktree-path".to_owned(),
        ));

        assert_eq!(app.handle_key(key(KeyCode::Enter)), Intent::None);
        assert_eq!(app.handle_key(key(KeyCode::Char('w'))), Intent::None);
    }

    #[test]
    fn web_action_is_enabled_only_for_branches_with_pull_requests() {
        let authored = authored("team", "project", 42, "2026-01-01");
        let mut repository = repository("/repo", true);
        repository
            .config
            .github_remotes
            .insert("origin".to_owned(), authored.identity.repository.clone());
        let pr_path = repository.worktrees[1].path.clone();
        let plain_path = repository.worktrees[0].path.clone();
        let mut app = App::new(vec![repository], PathBuf::from("/elsewhere"));
        app.github.insert(
            pr_path.clone(),
            GitHubState::Ready(GitHubBranchData {
                pull_request: Some(authored.pull_request.clone()),
                warnings: Vec::new(),
                rate_limit: None,
            }),
        );

        app.selected = Some(RowId::Worktree(pr_path));
        assert!(app.action_availability(Action::OpenPullRequestWeb).enabled);
        assert_eq!(
            app.handle_key(key(KeyCode::Char('w'))),
            Intent::BeginAction(Action::OpenPullRequestWeb)
        );
        assert_eq!(
            app.selected_pull_request_url().as_deref(),
            Some(authored.pull_request.url.as_str())
        );

        app.selected = Some(RowId::Worktree(plain_path));
        let availability = app.action_availability(Action::OpenPullRequestWeb);
        assert!(!availability.enabled);
        assert_eq!(
            availability.reason.as_deref(),
            Some("selected branch has no associated pull request")
        );
        assert_eq!(app.handle_key(key(KeyCode::Char('w'))), Intent::None);
    }

    #[test]
    fn refreshes_coalesce_and_stale_generations_are_rejected() {
        let mut app = App::new(vec![repository("/repo", true)], PathBuf::from("/elsewhere"));
        let paths = vec![PathBuf::from("/repo"), PathBuf::from("/repo-topic")];
        let generation = app.begin_status_refresh(&paths, true);
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
    fn quiet_status_refresh_retains_visible_state_and_hides_progress() {
        let mut app = App::new(vec![repository("/repo", true)], PathBuf::from("/elsewhere"));
        let path = PathBuf::from("/repo");
        let status = WorktreeStatus {
            unstaged: 1,
            ..WorktreeStatus::default()
        };
        app.statuses
            .insert(path.clone(), StatusState::Ready(status.clone()));

        let generation = app.begin_status_refresh(std::slice::from_ref(&path), false);

        assert_eq!(app.statuses.get(&path), Some(&StatusState::Ready(status)));
        assert_eq!(app.progress, None);
        assert!(!app.apply_status(StatusUpdate {
            generation,
            path: path.clone(),
            result: Ok(WorktreeStatus::default()),
        }));
        assert_eq!(
            app.statuses.get(&path),
            Some(&StatusState::Ready(WorktreeStatus::default()))
        );
        assert_eq!(app.progress, None);
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
        assert!(!app.github_network_active(&path));

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
    fn github_network_activity_covers_branch_and_pr_detail_requests() {
        let mut app = App::new(vec![repository("/repo", true)], PathBuf::from("/elsewhere"));
        let path = PathBuf::from("/repo-topic");
        let generation = app.begin_github_refresh(std::slice::from_ref(&path));
        assert!(app.github_network_active(&path));

        let data = GitHubBranchData {
            pull_request: Some(authored("team", "project", 42, "now").pull_request),
            warnings: Vec::new(),
            rate_limit: None,
        };
        assert!(app.apply_github_refresh(
            generation,
            std::slice::from_ref(&path),
            HashMap::from([(path.clone(), Ok(data))]),
        ));
        assert!(app.github_network_active(&path));

        assert!(app.apply_pull_request_details(generation, BTreeMap::new()));
        assert!(!app.github_network_active(&path));
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
    fn filter_matches_only_visible_pull_request_presentation() {
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
        let detail_id = CanonicalPullRequestId {
            repository: repository_identity,
            number: 42,
        };
        app.pull_request_details.insert(
            detail_id.clone(),
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
        ] {
            app.filter = filter.to_owned();
            assert!(app.visible_rows().iter().any(
                |row| matches!(row, VisibleRow::Worktree { id: RowId::Worktree(found), .. } if found == &path)
            ));
        }
        app.filter = "123".to_owned();
        assert!(app.visible_rows().is_empty());
        app.filter = "/repo-topic".to_owned();
        assert!(app.visible_rows().iter().any(
            |row| matches!(row, VisibleRow::Worktree { id: RowId::Worktree(found), .. } if found == &path)
        ));
        app.filter = "1234567890".to_owned();
        assert!(app.visible_rows().is_empty());
        app.filter.clear();
        let owner = BranchId::Worktree(path);
        app.selected = Some(RowId::Section(owner.clone(), InlineSection::Overview));
        app.handle_key(key(KeyCode::Char('l')));
        assert!(
            app.visible_rows()
                .iter()
                .any(|row| matches!(row, VisibleRow::Inline { text, .. } if text.contains("local path: /repo-topic")))
        );
    }

    #[test]
    fn filter_matches_visible_repository_state_labels() {
        let directory = tempfile::tempdir().unwrap();
        let invalid_path = directory.path().join("present");
        std::fs::create_dir(&invalid_path).unwrap();
        let stale_path = directory.path().join("gone");

        let mut invalid = repository("/placeholder-invalid", true);
        invalid.config.path = invalid_path.clone();
        invalid.config.label = Some("first".to_owned());
        invalid.stale_error = Some("catalog problem".to_owned());
        invalid.worktrees.clear();

        let mut stale = repository("/placeholder-stale", true);
        stale.config.path = stale_path.clone();
        stale.config.label = Some("second".to_owned());
        stale.stale_error = Some("catalog problem".to_owned());
        stale.worktrees.clear();

        let mut app = App::new(vec![invalid, stale], PathBuf::from("/elsewhere"));
        app.set_committed_filter("INVALID");
        assert_eq!(
            app.visible_rows()
                .iter()
                .map(|row| row.id().clone())
                .collect::<Vec<_>>(),
            vec![RowId::Repository(invalid_path)]
        );

        app.set_committed_filter("stale");
        assert_eq!(
            app.visible_rows()
                .iter()
                .map(|row| row.id().clone())
                .collect::<Vec<_>>(),
            vec![RowId::Repository(stale_path)]
        );
    }

    #[test]
    fn filter_uses_case_insensitive_regular_expressions() {
        let mut first = repository("/first", true);
        first.config.label = Some("Alpha-Service".to_owned());
        let mut second = repository("/second", true);
        second.config.label = Some("beta-worker".to_owned());
        let mut app = App::new(vec![first, second], PathBuf::from("/elsewhere"));

        for (filter, expected) in [
            ("^alpha-[a-z]+", vec![PathBuf::from("/first")]),
            (
                "ALPHA|beta-worker",
                vec![PathBuf::from("/first"), PathBuf::from("/second")],
            ),
            ("alpha-(service|api)", vec![PathBuf::from("/first")]),
        ] {
            app.set_committed_filter(filter);
            assert_eq!(
                app.visible_rows()
                    .into_iter()
                    .filter_map(|row| match row.id() {
                        RowId::Repository(path) => Some(path.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>(),
                expected,
                "unexpected matches for {filter}"
            );
        }

        app.set_committed_filter("[");
        assert!(app.visible_rows().is_empty());
    }

    #[test]
    fn filtered_tree_keeps_only_matches_and_complete_nested_ancestor_paths() {
        let (mut app, identity) = filter_test_app();
        app.filter_active = true;
        let repository = RowId::VirtualRepository(identity.repository.clone());
        let branch = RowId::VirtualPullRequest(identity.clone());
        let owner = BranchId::VirtualPullRequest(identity.clone());

        for (query, expected) in [
            (
                "PENDING-NEEDLE",
                vec![
                    repository.clone(),
                    branch.clone(),
                    RowId::Section(owner.clone(), InlineSection::Checks),
                    RowId::Section(owner.clone(), InlineSection::PendingChecks),
                    RowId::Check(identity.clone(), "pending-needle".to_owned()),
                ],
            ),
            (
                "valid-needle",
                vec![
                    repository.clone(),
                    branch.clone(),
                    RowId::Section(owner.clone(), InlineSection::Checks),
                    RowId::Section(owner.clone(), InlineSection::ValidResults),
                    RowId::Check(identity.clone(), "valid-needle".to_owned()),
                ],
            ),
            (
                "Alice",
                vec![
                    repository.clone(),
                    branch.clone(),
                    RowId::Section(owner.clone(), InlineSection::Reviewers),
                    RowId::Reviewer(identity.clone(), "alice".to_owned()),
                ],
            ),
            (
                "comment needle body",
                vec![
                    repository.clone(),
                    branch.clone(),
                    RowId::Section(owner.clone(), InlineSection::OpenComments),
                    RowId::OpenComment(identity.clone(), "comment-hidden-id".to_owned()),
                ],
            ),
            (
                "detail-warning-hidden",
                vec![
                    repository.clone(),
                    branch.clone(),
                    RowId::Section(owner.clone(), InlineSection::Overview),
                    RowId::Metadata(owner.clone(), "overview-detail-warning-0".to_owned()),
                ],
            ),
            (
                "https://github.com/team/project/pull/42",
                vec![
                    repository.clone(),
                    branch.clone(),
                    RowId::Section(owner.clone(), InlineSection::Overview),
                    RowId::Metadata(owner.clone(), "overview-url".to_owned()),
                ],
            ),
            (
                "failure-needle",
                vec![
                    repository.clone(),
                    branch.clone(),
                    RowId::Section(owner.clone(), InlineSection::Checks),
                    RowId::Check(identity.clone(), "failure-needle".to_owned()),
                ],
            ),
            ("review required", vec![repository.clone(), branch.clone()]),
        ] {
            app.filter = query.to_owned();
            assert_eq!(
                app.visible_rows()
                    .into_iter()
                    .map(|row| row.id().clone())
                    .collect::<Vec<_>>(),
                expected,
                "unexpected filtered path for {query}"
            );
        }

        app.filter = "1 unresolved comment".to_owned();
        assert!(app.visible_rows().is_empty());

        app.filter = "comment needle body".to_owned();
        app.filter_active = false;
        let committed = app
            .visible_rows()
            .into_iter()
            .map(|row| row.id().clone())
            .collect::<Vec<_>>();
        for restored in [
            RowId::Section(owner.clone(), InlineSection::Overview),
            RowId::Section(owner.clone(), InlineSection::Checks),
            RowId::Section(owner.clone(), InlineSection::Reviewers),
            RowId::Section(owner.clone(), InlineSection::OpenComments),
            RowId::OpenComment(identity.clone(), "comment-hidden-id".to_owned()),
        ] {
            assert!(
                committed.contains(&restored),
                "missing committed-search sibling {restored:?}"
            );
        }
        for still_collapsed in [
            RowId::Check(identity.clone(), "failure-needle".to_owned()),
            RowId::Check(identity.clone(), "pending-needle".to_owned()),
            RowId::Reviewer(identity.clone(), "alice".to_owned()),
        ] {
            assert!(!committed.contains(&still_collapsed));
        }
    }

    #[test]
    fn search_does_not_match_required_metadata_hidden_by_the_check_renderer() {
        let (mut app, _) = filter_test_app();
        app.filter = "^required$".to_owned();
        app.filter_active = true;

        assert!(app.visible_rows().is_empty());
    }

    #[test]
    fn filter_folds_are_temporary_and_escape_preserves_the_selected_path() {
        let (mut app, identity) = filter_test_app();
        let owner = BranchId::VirtualPullRequest(identity.clone());
        let checks_key = DisclosureKey::Section(owner.clone(), InlineSection::Checks);
        let reviewers_key = DisclosureKey::Section(owner.clone(), InlineSection::Reviewers);
        let overview_key = DisclosureKey::Section(owner.clone(), InlineSection::Overview);
        app.set_disclosure_expanded(checks_key.clone(), false);
        app.set_disclosure_expanded(reviewers_key.clone(), false);
        app.set_disclosure_expanded(overview_key.clone(), true);
        let saved = app.disclosure_expanded.clone();

        app.filter = "failure-needle".to_owned();
        let check_id = RowId::Check(identity.clone(), "failure-needle".to_owned());
        assert!(app.visible_rows().iter().any(|row| row.id() == &check_id));
        app.selected = Some(check_id.clone());
        app.handle_key(key(KeyCode::Char('h')));
        assert_eq!(
            app.selected,
            Some(RowId::Section(owner.clone(), InlineSection::Checks))
        );
        assert!(app.filter_collapsed.contains(&checks_key));
        assert!(!app.visible_rows().iter().any(|row| row.id() == &check_id));
        assert_eq!(app.disclosure_expanded, saved);

        app.handle_key(key(KeyCode::Char('l')));
        assert!(!app.filter_collapsed.contains(&checks_key));
        assert!(app.visible_rows().iter().any(|row| row.id() == &check_id));

        assert_eq!(app.handle_key(key(KeyCode::Esc)), Intent::None);
        assert!(app.filter.is_empty());
        assert!(app.filter_collapsed.is_empty());
        for (key, expanded) in saved {
            assert_eq!(app.disclosure_expanded.get(&key), Some(&expanded));
        }
        assert_eq!(
            app.disclosure_expanded
                .get(&DisclosureKey::VirtualRepository(
                    identity.repository.clone()
                )),
            Some(&true)
        );
        assert_eq!(
            app.disclosure_expanded
                .get(&DisclosureKey::Branch(owner.clone())),
            Some(&true)
        );
        assert_eq!(
            app.selected,
            Some(RowId::Section(owner.clone(), InlineSection::Checks))
        );
        assert!(!app.visible_rows().iter().any(|row| row.id() == &check_id));
        assert!(app.visible_rows().iter().any(|row| {
            matches!(
                row,
                VisibleRow::Inline {
                    id: RowId::Metadata(_, _),
                    section: InlineSection::Overview,
                    ..
                }
            )
        }));
    }

    #[test]
    fn search_replacement_cancellation_and_refresh_preserve_predictable_state() {
        let (mut app, identity) = filter_test_app();
        app.set_committed_filter("failure-needle");
        assert!(!app.filter_active);
        app.filter_collapsed.insert(DisclosureKey::Section(
            BranchId::VirtualPullRequest(identity.clone()),
            InlineSection::Checks,
        ));

        app.handle_key(key(KeyCode::Char('/')));
        assert!(app.filter_active);
        assert!(app.filter.is_empty());
        assert!(app.filter_collapsed.is_empty());
        for character in "PENDING-(needle|other)".chars() {
            app.handle_key(key(KeyCode::Char(character)));
        }
        app.handle_key(key(KeyCode::Enter));
        assert!(!app.filter_active);
        assert_eq!(app.filter, "PENDING-(needle|other)");

        let details = app.pull_request_details[&identity].clone();
        assert!(app.apply_pull_request_details(
            app.github_generation,
            BTreeMap::from([(identity.clone(), Ok(details))]),
        ));
        assert_eq!(app.filter, "PENDING-(needle|other)");
        assert!(app.visible_rows().iter().any(|row| {
            row.id() == &RowId::Check(identity.clone(), "pending-needle".to_owned())
        }));

        app.handle_key(key(KeyCode::Char('/')));
        app.handle_key(key(KeyCode::Char('x')));
        app.handle_key(key(KeyCode::Esc));
        assert!(!app.filter_active);
        assert!(app.filter.is_empty());
        assert!(app.filter_collapsed.is_empty());
    }

    #[test]
    fn committed_search_navigates_exact_hits_in_both_directions_with_wraparound() {
        let mut app = App::new(
            vec![repository("/first", true), repository("/second", true)],
            PathBuf::from("/elsewhere"),
        );
        app.set_committed_filter("^topic$");
        app.selected = Some(RowId::Repository(PathBuf::from("/first")));
        let first = RowId::Worktree(PathBuf::from("/first-topic"));
        let second = RowId::Worktree(PathBuf::from("/second-topic"));

        app.handle_key(key(KeyCode::Char('n')));
        assert_eq!(app.selected, Some(first.clone()));
        app.handle_key(key(KeyCode::Char('n')));
        assert_eq!(app.selected, Some(second.clone()));
        app.handle_key(key(KeyCode::Char('n')));
        assert_eq!(app.selected, Some(first.clone()));

        app.handle_key(key(KeyCode::Char('N')));
        assert_eq!(app.selected, Some(second.clone()));
        app.handle_key(key(KeyCode::Char('N')));
        assert_eq!(app.selected, Some(first));
    }

    #[test]
    fn left_folds_nearest_leaf_section_right_is_leaf_noop_and_branches_fold_whole_subtrees() {
        let (mut app, identity) = filter_test_app();
        let owner = BranchId::VirtualPullRequest(identity.clone());
        for section in [
            InlineSection::Overview,
            InlineSection::Checks,
            InlineSection::PendingChecks,
            InlineSection::Reviewers,
            InlineSection::OpenComments,
        ] {
            app.set_disclosure_expanded(DisclosureKey::Section(owner.clone(), section), true);
        }

        for (leaf, section) in [
            (
                RowId::Metadata(owner.clone(), "overview-url".to_owned()),
                InlineSection::Overview,
            ),
            (
                RowId::Check(identity.clone(), "pending-needle".to_owned()),
                InlineSection::PendingChecks,
            ),
            (
                RowId::Check(identity.clone(), "valid-needle".to_owned()),
                InlineSection::ValidResults,
            ),
            (
                RowId::Reviewer(identity.clone(), "alice".to_owned()),
                InlineSection::Reviewers,
            ),
            (
                RowId::OpenComment(identity.clone(), "comment-hidden-id".to_owned()),
                InlineSection::OpenComments,
            ),
        ] {
            app.set_disclosure_expanded(DisclosureKey::Section(owner.clone(), section), true);
            app.selected = Some(leaf);
            app.handle_key(key(KeyCode::Char('h')));
            assert_eq!(app.selected, Some(RowId::Section(owner.clone(), section)));
            assert_eq!(
                app.disclosure_expanded
                    .get(&DisclosureKey::Section(owner.clone(), section)),
                Some(&false)
            );
        }

        app.set_disclosure_expanded(
            DisclosureKey::Section(owner.clone(), InlineSection::Checks),
            true,
        );
        app.set_disclosure_expanded(
            DisclosureKey::Section(owner.clone(), InlineSection::PendingChecks),
            true,
        );
        let leaf = RowId::Check(identity.clone(), "pending-needle".to_owned());
        app.selected = Some(leaf.clone());
        let disclosures = app.disclosure_expanded.clone();
        app.handle_key(key(KeyCode::Char('l')));
        assert_eq!(app.selected, Some(leaf));
        assert_eq!(app.disclosure_expanded, disclosures);

        let branch = RowId::VirtualPullRequest(identity.clone());
        app.selected = Some(branch.clone());
        app.handle_key(key(KeyCode::Char('h')));
        assert_eq!(app.selected, Some(branch.clone()));
        assert_eq!(
            app.visible_rows()
                .iter()
                .filter(|row| matches!(row, VisibleRow::Inline { .. }))
                .count(),
            0
        );
        app.handle_key(key(KeyCode::Char('l')));
        assert!(
            app.visible_rows()
                .iter()
                .any(|row| matches!(row, VisibleRow::Inline { .. }))
        );
    }

    #[test]
    fn filtered_check_category_changes_keep_identity_selection_and_new_ancestors() {
        let (mut app, identity) = filter_test_app();
        let owner = BranchId::VirtualPullRequest(identity.clone());
        let check_id = RowId::Check(identity.clone(), "pending-needle".to_owned());
        app.filter = "pending-needle".to_owned();
        app.selected = Some(check_id.clone());
        app.set_viewport_height(2);

        app.pull_request_details
            .get_mut(&identity)
            .unwrap()
            .checks
            .iter_mut()
            .find(|check| check.name == "pending-needle")
            .unwrap()
            .state = CheckState::Success;
        app.ensure_selection_visible();

        assert_eq!(app.selected, Some(check_id.clone()));
        let ids = app
            .visible_rows()
            .into_iter()
            .map(|row| row.id().clone())
            .collect::<Vec<_>>();
        assert!(ids.contains(&RowId::Section(owner.clone(), InlineSection::ValidResults)));
        assert!(!ids.contains(&RowId::Section(owner, InlineSection::PendingChecks)));
        assert!(ids.contains(&check_id));
        assert!(app.scroll < ids.len());
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
    fn copy_and_prune_shortcuts_do_not_bind_advanced_create() {
        assert_eq!(Action::CopyAgentPrompt.shortcut(), Some("c"));
        assert_eq!(Action::CopyReviewRequest.shortcut(), Some("p"));
        assert_eq!(Action::Create.shortcut(), None);
        assert_eq!(Action::Prune.shortcut(), Some("P"));

        let mut app = App::new(vec![repository("/repo", true)], PathBuf::from("/elsewhere"));
        for (key_code, expected) in [
            ('c', Action::CopyAgentPrompt),
            ('p', Action::CopyReviewRequest),
            ('P', Action::Prune),
        ] {
            assert_eq!(
                app.handle_key(key(KeyCode::Char(key_code))),
                Intent::BeginAction(expected)
            );
        }
        assert_eq!(app.handle_key(key(KeyCode::Char('C'))), Intent::None);
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
