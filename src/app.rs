use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::github::GitHubError;
use crate::model::{GitHubBranchData, RepositoryConfig, Worktree, WorktreeStatus};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum RowId {
    Repository(PathBuf),
    Worktree(PathBuf),
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
        id: RowId,
    },
}

impl VisibleRow {
    pub fn id(&self) -> &RowId {
        match self {
            Self::Repository { id, .. } | Self::Worktree { id, .. } => id,
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
    Create,
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
    pub const ALL: [Self; 10] = [
        Self::Create,
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
            Self::Create => "create worktree",
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
            Self::Create => "c",
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
    pub selected: Option<RowId>,
    pub filter: String,
    pub filter_active: bool,
    pub pane: Pane,
    pub scroll: usize,
    pub viewport_height: usize,
    pub modal: Option<Modal>,
    pub inline_error: Option<String>,
    pub progress: Option<String>,
    pub statuses: HashMap<PathBuf, StatusState>,
    pub github: HashMap<PathBuf, GitHubState>,
    pub github_generation: u64,
    pub github_loading: bool,
    pub current_directory: PathBuf,
    pub generation: u64,
    pending_status: usize,
    refresh_queued: bool,
}

impl App {
    pub fn new(repositories: Vec<RepositoryView>, current_directory: PathBuf) -> Self {
        let mut app = Self {
            repositories,
            selected: None,
            filter: String::new(),
            filter_active: false,
            pane: Pane::List,
            scroll: 0,
            viewport_height: 1,
            modal: None,
            inline_error: None,
            progress: None,
            statuses: HashMap::new(),
            github: HashMap::new(),
            github_generation: 0,
            github_loading: false,
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
                    .contains(&filter);
            let matching_worktrees: Vec<usize> = repository
                .worktrees
                .iter()
                .enumerate()
                .filter(|(_, worktree)| {
                    repository_matches || self.worktree_matches(worktree, &filter)
                })
                .map(|(index, _)| index)
                .collect();
            if !repository_matches && matching_worktrees.is_empty() {
                continue;
            }
            rows.push(VisibleRow::Repository {
                repository_index,
                id: repository.id(),
            });
            if repository.expanded || !filter.is_empty() {
                for worktree_index in matching_worktrees {
                    let worktree = &repository.worktrees[worktree_index];
                    rows.push(VisibleRow::Worktree {
                        repository_index,
                        worktree_index,
                        id: RowId::Worktree(worktree.path.clone()),
                    });
                }
            }
        }
        rows
    }

    fn worktree_matches(&self, worktree: &Worktree, filter: &str) -> bool {
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
                    .map(github_search_text)
                    .unwrap_or_else(|| "github loading".to_owned()),
                GitHubState::Ready(data) => github_search_text(data),
                GitHubState::Stale { previous, error } => {
                    let mut text = previous
                        .as_ref()
                        .map(github_search_text)
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
            VisibleRow::Repository { .. } => None,
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
        };
        Some((&self.repositories[index], index))
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
            KeyCode::Char(character) => self.direct_action(character),
            KeyCode::Enter => self.accept_or_toggle(),
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
            Action::EditRepository | Action::RemoveRepository => {
                if repository.session_only {
                    disabled("register this session-only repository first")
                } else {
                    enabled()
                }
            }
            Action::Create | Action::Prune => enabled(),
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

    pub fn set_viewport_height(&mut self, height: usize) {
        self.viewport_height = height.max(1);
        self.ensure_selected_in_view();
    }

    fn direct_action(&mut self, character: char) -> Intent {
        let action = match character {
            'c' => Action::Create,
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
            self.ensure_selected_in_view();
        }
    }

    fn expand_or_focus_detail(&mut self) {
        if matches!(self.selected_row(), Some(VisibleRow::Repository { .. })) {
            if let Some((_, index)) = self.selected_repository() {
                self.repositories[index].expanded = true;
            }
        } else {
            self.pane = Pane::Detail;
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
        self.selected = Some(rows[next].id().clone());
        self.ensure_selected_in_view();
    }

    fn select_index(&mut self, index: usize) {
        if let Some(row) = self.visible_rows().get(index) {
            self.selected = Some(row.id().clone());
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
        if index < self.scroll {
            self.scroll = index;
        } else if index >= self.scroll + self.viewport_height {
            self.scroll = index + 1 - self.viewport_height;
        }
    }
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
        ]);
    }
    parts.join(" ")
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
    fn filter_matches_pull_request_enrichment() {
        let mut app = App::new(vec![repository("/repo", true)], PathBuf::from("/elsewhere"));
        let path = PathBuf::from("/repo-topic");
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
        app.filter = "frobnicator".to_owned();
        assert!(app.visible_rows().iter().any(
            |row| matches!(row, VisibleRow::Worktree { id: RowId::Worktree(found), .. } if found == &path)
        ));
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
        app.selected = Some(RowId::Worktree(PathBuf::from("/bare")));
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
}
