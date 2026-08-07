use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};

use crate::app::{App, GitHubState, Modal, Pane, StatusState, VisibleRow};
use crate::model::CheckRollup;

pub fn render(frame: &mut Frame<'_>, app: &mut App) {
    let area = frame.area();
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(3),
            Constraint::Length(2),
        ])
        .split(area);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" wt ", Style::default().fg(Color::Black).bg(Color::Cyan)),
            Span::raw(" global worktrees"),
            Span::styled(header_progress(app), Style::default().fg(Color::Yellow)),
        ])),
        vertical[0],
    );

    let body = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(63), Constraint::Percentage(37)])
        .split(vertical[1]);
    render_list(frame, app, body[0]);
    render_detail(frame, app, body[1]);
    render_footer(frame, app, vertical[2]);
    if let Some(modal) = &app.modal {
        render_modal(frame, app, modal, area);
    }
}

fn render_list(frame: &mut Frame<'_>, app: &mut App, area: Rect) {
    let rows = app.visible_rows();
    app.set_viewport_height(area.height.saturating_sub(2) as usize);
    if rows.is_empty() {
        let message = if !app.filter.is_empty() {
            "No repositories or worktrees match the filter."
        } else if app.repositories.is_empty() {
            "No repositories or authored pull requests are available. Run `wt repo add`, or authenticate GitHub to discover authored PRs."
        } else {
            "Catalog entries are unavailable. Select an invalid or stale repository to relink or unregister it."
        };
        frame.render_widget(
            Paragraph::new(message)
                .wrap(Wrap { trim: true })
                .block(list_block(app)),
            area,
        );
        return;
    }
    let items: Vec<ListItem<'_>> = rows
        .iter()
        .map(|row| match row {
            VisibleRow::Repository {
                repository_index, ..
            } => {
                let repository = &app.repositories[*repository_index];
                let arrow = if repository.expanded { "▾" } else { "▸" };
                let state = [
                    repository.is_bare().then_some("bare"),
                    repository.session_only.then_some("session-only"),
                    repository.stale_error.is_some().then_some(
                        if repository.config.path.exists() {
                            "invalid"
                        } else {
                            "stale"
                        },
                    ),
                ]
                .into_iter()
                .flatten()
                .map(|state| format!(" [{state}]"))
                .collect::<String>();
                ListItem::new(Line::from(vec![
                    Span::styled(
                        format!("{arrow} {}", repository.config.display_label()),
                        Style::default().add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(state, Style::default().fg(Color::Yellow)),
                    Span::styled(
                        format!("  {}", display_path(&repository.config.path)),
                        Style::default().fg(Color::DarkGray),
                    ),
                ]))
            }
            VisibleRow::Worktree {
                repository_index,
                worktree_index,
                ..
            } => {
                let worktree = &app.repositories[*repository_index].worktrees[*worktree_index];
                let current = path_contains(&worktree.path, &app.current_directory);
                let identity = worktree
                    .branch
                    .as_deref()
                    .and_then(|branch| branch.strip_prefix("refs/heads/"))
                    .map(str::to_owned)
                    .or_else(|| {
                        worktree
                            .head
                            .as_ref()
                            .map(|head| format!("detached:{}", short(head)))
                    })
                    .unwrap_or_else(|| if worktree.bare { "bare" } else { "unknown" }.to_owned());
                let flags = [
                    worktree.locked.as_ref().map(|_| "locked"),
                    worktree.prunable.as_ref().map(|_| "prunable"),
                    worktree.bare.then_some("anchor"),
                ]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(",");
                let (local, local_color) = match app.statuses.get(&worktree.path) {
                    Some(StatusState::Pending) => ("[…]".to_owned(), Color::DarkGray),
                    Some(StatusState::Ready(status)) => (
                        status.compact(),
                        if status.is_dirty() {
                            Color::Red
                        } else {
                            Color::Green
                        },
                    ),
                    Some(StatusState::Error(_)) => ("[!]".to_owned(), Color::Yellow),
                    None => (String::new(), Color::DarkGray),
                };
                let github = github_summary(app.github.get(&worktree.path));
                let github_color = github_summary_color(app.github.get(&worktree.path));
                ListItem::new(Line::from(vec![
                    Span::styled(
                        if current { "  ● " } else { "    " },
                        Style::default().fg(Color::Green),
                    ),
                    Span::raw(identity),
                    Span::styled(
                        worktree
                            .head
                            .as_deref()
                            .map(|head| format!(" {}", short(head)))
                            .unwrap_or_default(),
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::styled(
                        if flags.is_empty() {
                            String::new()
                        } else {
                            format!(" [{flags}]")
                        },
                        Style::default().fg(Color::Yellow),
                    ),
                    Span::styled(
                        format!(" {}", display_path(&worktree.path)),
                        Style::default().fg(Color::Blue),
                    ),
                    Span::styled(
                        if local.is_empty() {
                            String::new()
                        } else {
                            format!(" {local}")
                        },
                        Style::default().fg(local_color),
                    ),
                    Span::styled(
                        if github.is_empty() {
                            String::new()
                        } else {
                            format!(" {github}")
                        },
                        Style::default().fg(github_color),
                    ),
                ]))
            }
            VisibleRow::VirtualRepository {
                virtual_repository_index,
                ..
            } => {
                let repository = &app.virtual_repositories[*virtual_repository_index];
                let arrow = if repository.expanded { "▾" } else { "▸" };
                ListItem::new(Line::from(vec![
                    Span::styled(
                        format!("  {arrow} {}", repository.identity.full_name()),
                        Style::default().add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        if repository.mapped_repository.is_none() {
                            " [no local repo]"
                        } else {
                            ""
                        },
                        Style::default().fg(Color::DarkGray),
                    ),
                ]))
            }
            VisibleRow::VirtualPullRequest {
                virtual_repository_index,
                pull_request_index,
                mapped_repository_index,
                stack_depth,
                ..
            } => {
                let pull_request = &app.virtual_repositories[*virtual_repository_index]
                    .pull_requests[*pull_request_index];
                ListItem::new(Line::from(vec![
                    Span::raw(" ".repeat(
                        if mapped_repository_index.is_some() {
                            4
                        } else {
                            6
                        } + stack_depth * 2,
                    )),
                    Span::styled(
                        &pull_request.pull_request.head.branch,
                        Style::default().fg(Color::LightMagenta),
                    ),
                    Span::raw(format!(
                        " #{} — {} ",
                        pull_request.identity.number, pull_request.pull_request.title
                    )),
                    Span::styled("[virtual]", Style::default().fg(Color::Magenta)),
                    Span::styled(
                        format!(" [{}]", pull_request.pull_request.checks),
                        Style::default().fg(check_color(pull_request.pull_request.checks)),
                    ),
                    Span::styled(
                        if pull_request.pull_request.auto_merge {
                            " [auto-merge]"
                        } else {
                            ""
                        },
                        Style::default().fg(Color::LightBlue),
                    ),
                ]))
            }
        })
        .collect();
    let selected = app
        .selected
        .as_ref()
        .and_then(|selected| rows.iter().position(|row| row.id() == selected));
    let mut state = ListState::default()
        .with_selected(selected)
        .with_offset(app.scroll);
    let list = List::new(items)
        .block(list_block(app))
        .highlight_style(
            Style::default()
                .bg(Color::Rgb(45, 55, 72))
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");
    frame.render_stateful_widget(list, area, &mut state);
}

fn list_block(app: &App) -> Block<'static> {
    Block::default()
        .title(" Repositories / Worktrees / Authored PRs ")
        .borders(Borders::ALL)
        .border_style(if app.pane == Pane::List {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default()
        })
}

fn render_detail(frame: &mut Frame<'_>, app: &mut App, area: Rect) {
    let mut lines = Vec::new();
    if let Some((repository, authored)) = app.selected_virtual_pull_request() {
        let pull_request = &authored.pull_request;
        lines.push(field("repository", repository.identity.full_name()));
        lines.push(field(
            "local repo",
            repository
                .mapped_repository
                .as_ref()
                .map(|path| display_path(path))
                .unwrap_or_else(|| "[no local repo]".to_owned()),
        ));
        lines.push(field(
            "pull request",
            format!("#{}", authored.identity.number),
        ));
        lines.push(field("title", pull_request.title.clone()));
        lines.push(field("author", authored.author.clone()));
        lines.push(field(
            "base",
            format!(
                "{}:{}",
                pull_request.base.repository.as_deref().unwrap_or("unknown"),
                pull_request.base.branch
            ),
        ));
        lines.push(field(
            "head",
            format!(
                "{}:{}",
                pull_request.head.repository.as_deref().unwrap_or("unknown"),
                pull_request.head.branch
            ),
        ));
        lines.push(field(
            "head SHA",
            pull_request
                .head
                .oid
                .clone()
                .unwrap_or_else(|| "-".to_owned()),
        ));
        lines.push(field("state", pull_request.state.to_string()));
        lines.push(styled_field(
            "checks",
            pull_request.checks.to_string(),
            check_color(pull_request.checks),
        ));
        lines.push(field(
            "auto-merge",
            if pull_request.auto_merge {
                "enabled"
            } else {
                "not enabled"
            }
            .to_owned(),
        ));
        lines.push(field(
            "review",
            pull_request
                .review_decision
                .clone()
                .unwrap_or_else(|| "-".to_owned()),
        ));
        lines.push(field("URL", pull_request.url.clone()));
        lines.push(Line::styled(
            "Enter to create worktree",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ));
    } else if let Some((repository, worktree, _)) = app.selected_worktree() {
        lines.push(Line::from(vec![
            Span::styled("repository  ", Style::default().fg(Color::DarkGray)),
            Span::raw(repository.config.display_label()),
        ]));
        lines.push(field(
            "anchor",
            repository.config.path.display().to_string(),
        ));
        lines.push(field("path", worktree.path.display().to_string()));
        lines.push(field(
            "branch",
            worktree
                .branch
                .clone()
                .unwrap_or_else(|| "detached".to_owned()),
        ));
        lines.push(field(
            "HEAD",
            worktree.head.clone().unwrap_or_else(|| "-".to_owned()),
        ));
        lines.push(field(
            "locked",
            worktree.locked.clone().unwrap_or_else(|| "no".to_owned()),
        ));
        lines.push(field(
            "prunable",
            worktree.prunable.clone().unwrap_or_else(|| "no".to_owned()),
        ));
        match app.statuses.get(&worktree.path) {
            Some(StatusState::Ready(status)) => {
                lines.push(field(
                    "upstream",
                    status.upstream.clone().unwrap_or_else(|| "-".to_owned()),
                ));
                lines.push(field("local", status.summary()));
            }
            Some(StatusState::Pending) => lines.push(Line::from("local       loading…")),
            Some(StatusState::Error(error)) => lines.push(Line::styled(
                format!("status error {error}"),
                Style::default().fg(Color::Red),
            )),
            None => {}
        }
        match app.github.get(&worktree.path) {
            Some(GitHubState::Loading { previous }) => {
                lines.push(field("GitHub", "loading…".to_owned()));
                if let Some(data) = previous {
                    append_github_details(&mut lines, data);
                }
            }
            Some(GitHubState::Ready(data)) => append_github_details(&mut lines, data),
            Some(GitHubState::Stale { previous, error }) => {
                lines.push(Line::styled(
                    format!("GitHub stale: {error}"),
                    Style::default().fg(Color::Yellow),
                ));
                if let Some(data) = previous {
                    append_github_details(&mut lines, data);
                }
            }
            None => {}
        }
    } else if let Some((repository, _)) = app.selected_repository() {
        lines.push(field("label", repository.config.display_label()));
        lines.push(field(
            "anchor",
            repository.config.path.display().to_string(),
        ));
        lines.push(field(
            "catalog",
            if repository.session_only {
                "session-only; press a to register".to_owned()
            } else {
                "registered".to_owned()
            },
        ));
        if let Some(error) = &repository.stale_error {
            lines.push(Line::styled(
                format!(
                    "{}       {error}",
                    if repository.config.path.exists() {
                        "invalid"
                    } else {
                        "stale"
                    }
                ),
                Style::default().fg(Color::Red),
            ));
        }
    } else if let Some(VisibleRow::VirtualRepository {
        virtual_repository_index,
        ..
    }) = app.selected_row()
    {
        let repository = &app.virtual_repositories[virtual_repository_index];
        lines.push(field("repository", repository.identity.full_name()));
        lines.push(field("host", repository.identity.host.clone()));
        lines.push(field(
            "local repo",
            repository
                .mapped_repository
                .as_ref()
                .map(|path| display_path(path))
                .unwrap_or_else(|| "[no local repo]".to_owned()),
        ));
        lines.push(field(
            "authored PRs",
            repository.pull_requests.len().to_string(),
        ));
    } else {
        lines.push(Line::from("Select a repository or worktree."));
    }
    let block = Block::default()
        .title(" Details ")
        .borders(Borders::ALL)
        .border_style(if app.pane == Pane::Detail {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default()
        });
    let inner = block.inner(area);
    let line_count = wrapped_line_count(&lines, inner.width);
    let paragraph = Paragraph::new(Text::from(lines))
        .wrap(Wrap { trim: false })
        .block(block);
    app.set_detail_max_scroll(line_count.saturating_sub(inner.height as usize));
    let paragraph = paragraph.scroll((app.detail_scroll.min(u16::MAX as usize) as u16, 0));
    frame.render_widget(paragraph, area);
}

fn wrapped_line_count(lines: &[Line<'_>], width: u16) -> usize {
    let width = width.max(1) as usize;
    lines
        .iter()
        .map(|line| line.width().max(1).div_ceil(width))
        .sum()
}

fn render_footer(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let top = if app.filter_active {
        format!("/{}█", app.filter)
    } else if !app.filter.is_empty() {
        format!("filter: {}", app.filter)
    } else {
        "j/k move  h/l panes  / filter  r refresh  ? actions  Enter select/create  q/Esc cancel"
            .to_owned()
    };
    let bottom = app
        .inline_error
        .as_ref()
        .map(|error| format!("error: {error}"))
        .unwrap_or_else(|| {
            "c create  m move  L/U lock  d remove  R repair  p prune  a/e/x catalog".to_owned()
        });
    frame.render_widget(
        Paragraph::new(vec![
            Line::raw(top),
            Line::styled(
                bottom,
                Style::default().fg(if app.inline_error.is_some() {
                    Color::Red
                } else {
                    Color::DarkGray
                }),
            ),
        ]),
        area,
    );
}

fn render_modal(frame: &mut Frame<'_>, app: &App, modal: &Modal, area: Rect) {
    let popup = centered_rect(72, 70, area);
    frame.render_widget(Clear, popup);
    match modal {
        Modal::Palette { selected } => {
            let items: Vec<ListItem<'_>> = crate::app::Action::ALL
                .iter()
                .map(|action| {
                    let availability = app.action_availability(*action);
                    let suffix = availability
                        .reason
                        .map(|reason| format!(" — {reason}"))
                        .unwrap_or_default();
                    ListItem::new(format!(
                        "[{}] {}{}",
                        action.shortcut(),
                        action.label(),
                        suffix
                    ))
                    .style(if availability.enabled {
                        Style::default()
                    } else {
                        Style::default().fg(Color::DarkGray)
                    })
                })
                .collect();
            let mut state = ListState::default().with_selected(Some(*selected));
            frame.render_stateful_widget(
                List::new(items)
                    .block(Block::default().title(" Actions ").borders(Borders::ALL))
                    .highlight_style(Style::default().bg(Color::Blue)),
                popup,
                &mut state,
            );
        }
        Modal::Form {
            action,
            fields,
            active,
        } => {
            let mut lines = Vec::new();
            if *action == crate::app::Action::Create
                && let Some((repository, _)) = app.selected_repository()
            {
                lines.push(Line::from(vec![
                    Span::styled(
                        "Repository: ",
                        Style::default().add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(repository.config.display_label()),
                ]));
                lines.push(Line::styled(
                    format!("Path: {}", display_path(&repository.config.path)),
                    Style::default().fg(Color::DarkGray),
                ));
                lines.push(Line::raw(""));
            }
            lines.extend(fields.iter().enumerate().map(|(index, field)| {
                Line::styled(
                    format!(
                        "{}: {}{}",
                        field.label,
                        field.value,
                        if index == *active { "█" } else { "" }
                    ),
                    if index == *active {
                        Style::default().fg(Color::Cyan)
                    } else {
                        Style::default()
                    },
                )
            }));
            // A rejected submission leaves the form open, so the reason belongs
            // next to the fields instead of only on the footer line, where it
            // reads as the submission having done nothing at all.
            if let Some(error) = &app.inline_error {
                lines.push(Line::raw(""));
                lines.push(Line::styled(
                    format!("error: {error}"),
                    Style::default().fg(Color::Red),
                ));
            }
            frame.render_widget(
                Paragraph::new(lines).wrap(Wrap { trim: false }).block(
                    Block::default()
                        .title(format!(" {} · Enter submit · Esc cancel ", action.label()))
                        .borders(Borders::ALL),
                ),
                popup,
            );
        }
        Modal::Confirm { action, summary } => {
            let mut lines: Vec<Line<'_>> = summary
                .iter()
                .map(|line| Line::raw(line.as_str()))
                .collect();
            lines.push(Line::raw(""));
            lines.push(Line::styled(
                "Enter/y confirms · n/Esc cancels",
                Style::default().fg(Color::Yellow),
            ));
            frame.render_widget(
                Paragraph::new(lines).wrap(Wrap { trim: false }).block(
                    Block::default()
                        .title(format!(" Confirm {} ", action.label()))
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(Color::Yellow)),
                ),
                popup,
            );
        }
    }
}

fn field(label: &str, value: String) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label:<11}"), Style::default().fg(Color::DarkGray)),
        Span::raw(value),
    ])
}

fn styled_field(label: &str, value: String, color: Color) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label:<11}"), Style::default().fg(Color::DarkGray)),
        Span::styled(value, Style::default().fg(color)),
    ])
}

fn header_progress(app: &App) -> String {
    let mut progress = app.progress.clone().into_iter().collect::<Vec<_>>();
    if app.github_loading {
        progress.push("loading GitHub PRs".to_owned());
    }
    if app.authored_pull_requests.loading {
        progress.push(
            app.authored_pull_requests
                .current_host
                .as_ref()
                .map(|host| {
                    format!(
                        "loading authored PRs: {host} page {}",
                        app.authored_pull_requests.current_page
                    )
                })
                .unwrap_or_else(|| "loading authored PRs".to_owned()),
        );
    } else if app.authored_pull_requests.stale_error.is_some() {
        progress.push("authored PRs stale".to_owned());
    }
    if progress.is_empty() {
        String::new()
    } else {
        format!("  ·  {}", progress.join(" · "))
    }
}

fn github_summary(state: Option<&GitHubState>) -> String {
    match state {
        Some(GitHubState::Loading { previous }) => previous
            .as_ref()
            .map(|data| format!("{} (refreshing)", pull_request_summary(data)))
            .unwrap_or_else(|| "PR …".to_owned()),
        Some(GitHubState::Ready(data)) => pull_request_summary(data),
        Some(GitHubState::Stale { previous, .. }) => previous
            .as_ref()
            .map(|data| format!("{} (stale)", pull_request_summary(data)))
            .unwrap_or_else(|| "GitHub error".to_owned()),
        None => String::new(),
    }
}

fn github_summary_color(state: Option<&GitHubState>) -> Color {
    let data = match state {
        Some(GitHubState::Loading { previous }) => previous.as_ref(),
        Some(GitHubState::Ready(data)) => Some(data),
        Some(GitHubState::Stale { previous, .. }) => previous.as_ref(),
        None => None,
    };
    data.and_then(|data| data.pull_request.as_ref())
        .map(|pull_request| check_color(pull_request.checks))
        .unwrap_or(Color::Cyan)
}

fn check_color(checks: CheckRollup) -> Color {
    match checks {
        CheckRollup::Success => Color::Green,
        CheckRollup::Failure | CheckRollup::Error => Color::Red,
        CheckRollup::Pending | CheckRollup::Expected => Color::Yellow,
        CheckRollup::Unknown => Color::DarkGray,
    }
}

fn pull_request_summary(data: &crate::model::GitHubBranchData) -> String {
    data.pull_request
        .as_ref()
        .map(|pull_request| {
            format!(
                "PR #{} {} · {}{} · {}",
                pull_request.number,
                pull_request.state,
                pull_request.checks,
                if pull_request.auto_merge {
                    " · auto-merge"
                } else {
                    ""
                },
                pull_request.title
            )
        })
        .unwrap_or_else(|| "no PR".to_owned())
}

fn append_github_details(lines: &mut Vec<Line<'static>>, data: &crate::model::GitHubBranchData) {
    if let Some(pull_request) = &data.pull_request {
        lines.push(field(
            "PR",
            format!("#{} {}", pull_request.number, pull_request.state),
        ));
        lines.push(field("title", pull_request.title.clone()));
        lines.push(field("URL", pull_request.url.clone()));
        lines.push(field(
            "base",
            format!(
                "{}:{}",
                pull_request.base.repository.as_deref().unwrap_or("?"),
                pull_request.base.branch
            ),
        ));
        lines.push(field(
            "head",
            format!(
                "{}:{}",
                pull_request.head.repository.as_deref().unwrap_or("?"),
                pull_request.head.branch
            ),
        ));
        lines.push(field(
            "review",
            pull_request
                .review_decision
                .clone()
                .unwrap_or_else(|| "-".to_owned()),
        ));
        lines.push(styled_field(
            "checks",
            pull_request.checks.to_string(),
            check_color(pull_request.checks),
        ));
        lines.push(field(
            "auto-merge",
            if pull_request.auto_merge {
                "enabled"
            } else {
                "not enabled"
            }
            .to_owned(),
        ));
        lines.push(field("PR updated", pull_request.updated_at.clone()));
    } else {
        lines.push(field("GitHub", "no associated PR".to_owned()));
    }
    if let Some(rate) = &data.rate_limit {
        lines.push(field(
            "rate limit",
            format!("{} remaining · reset {}", rate.remaining, rate.reset_at),
        ));
    }
    for warning in &data.warnings {
        lines.push(Line::styled(
            format!("warning: {warning}"),
            Style::default().fg(Color::Yellow),
        ));
    }
}

fn short(head: &str) -> &str {
    head.get(..head.len().min(8)).unwrap_or(head)
}

fn display_path(path: &std::path::Path) -> String {
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    shorten_home(path, home.as_deref())
}

fn shorten_home(path: &std::path::Path, home: Option<&std::path::Path>) -> String {
    let Some(home) = home.filter(|home| home.parent().is_some()) else {
        return path.display().to_string();
    };
    match path.strip_prefix(home) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".to_owned(),
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
}

fn path_contains(worktree: &std::path::Path, candidate: &std::path::Path) -> bool {
    let worktree = std::fs::canonicalize(worktree).unwrap_or_else(|_| worktree.to_owned());
    let candidate = std::fs::canonicalize(candidate).unwrap_or_else(|_| candidate.to_owned());
    candidate.starts_with(worktree)
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1])[1]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{GitHubState, RepositoryView, RowId, VirtualRepositoryView};
    use crate::model::{
        AuthoredPullRequest, CanonicalPullRequestId, CheckRollup, GitHubBranchData,
        GitHubRepositoryIdentity, PullRequest, PullRequestIdentity, PullRequestState, RateLimit,
        RepositoryConfig, Worktree,
    };
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use std::path::PathBuf;

    #[test]
    fn renders_grouped_rows_details_and_resizes() {
        let repository = RepositoryView {
            config: RepositoryConfig {
                path: PathBuf::from("/repo"),
                label: Some("project".to_owned()),
                worktree_root: None,
                github_remote: None,
                github_remotes: Default::default(),
                github_preferred_remote: None,
            },
            session_only: true,
            stale_error: None,
            expanded: true,
            worktrees: vec![Worktree {
                path: PathBuf::from("/repo"),
                head: Some("1234567890".to_owned()),
                branch: Some("refs/heads/main".to_owned()),
                detached: false,
                bare: false,
                locked: None,
                prunable: None,
            }],
        };
        let mut app = App::new(vec![repository], PathBuf::from("/elsewhere"));
        app.selected = Some(RowId::Worktree(PathBuf::from("/repo")));
        let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        assert_eq!(app.viewport_height, 11);
        let content = buffer_text(terminal.backend().buffer());
        assert!(content.contains("project [session-only]"));
        assert!(content.contains("branch"));
        terminal.resize(Rect::new(0, 0, 42, 12)).unwrap();
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
    }

    #[test]
    fn renders_bare_state_on_repository_without_anchor_child() {
        let path = PathBuf::from("/repo.git");
        let repository = RepositoryView {
            config: RepositoryConfig {
                path: path.clone(),
                label: Some("project".to_owned()),
                worktree_root: None,
                github_remote: None,
                github_remotes: Default::default(),
                github_preferred_remote: None,
            },
            session_only: false,
            stale_error: None,
            expanded: true,
            worktrees: vec![
                Worktree {
                    path: path.clone(),
                    head: None,
                    branch: None,
                    detached: false,
                    bare: true,
                    locked: None,
                    prunable: None,
                },
                Worktree {
                    path: PathBuf::from("/trees/topic"),
                    head: Some("1234567890".to_owned()),
                    branch: Some("refs/heads/topic".to_owned()),
                    detached: false,
                    bare: false,
                    locked: None,
                    prunable: None,
                },
            ],
        };
        let mut app = App::new(vec![repository], PathBuf::from("/elsewhere"));
        let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();

        terminal.draw(|frame| render(frame, &mut app)).unwrap();

        let content = buffer_text(terminal.backend().buffer());
        assert!(content.contains("project [bare]"));
        assert!(content.contains("topic"));
        assert!(!content.contains("[anchor]"));
    }

    #[test]
    fn renders_virtual_rows_no_local_marker_and_pull_request_details() {
        let identity = GitHubRepositoryIdentity::canonical("github.com", "base", "project");
        let pull_request_id = CanonicalPullRequestId {
            repository: identity.clone(),
            number: 42,
        };
        let authored = AuthoredPullRequest {
            identity: pull_request_id.clone(),
            author: "viewer".to_owned(),
            pull_request: PullRequest {
                number: 42,
                title: "virtual feature".to_owned(),
                url: "https://github.com/base/project/pull/42".to_owned(),
                state: PullRequestState::Draft,
                updated_at: "2026-01-01T00:00:00Z".to_owned(),
                review_decision: Some("CHANGES_REQUESTED".to_owned()),
                auto_merge: true,
                base: PullRequestIdentity {
                    repository: Some("base/project".to_owned()),
                    branch: "main".to_owned(),
                    oid: Some("base-sha".to_owned()),
                },
                head: PullRequestIdentity {
                    repository: Some("viewer/fork".to_owned()),
                    branch: "topic".to_owned(),
                    oid: Some("head-sha".to_owned()),
                },
                checks: CheckRollup::Failure,
            },
        };
        let mut app = App::new(Vec::new(), PathBuf::from("/outside"));
        app.virtual_repositories = vec![VirtualRepositoryView {
            identity,
            mapped_repository: None,
            expanded: true,
            pull_requests: vec![authored],
        }];
        app.selected = Some(RowId::VirtualPullRequest(pull_request_id));
        let mut terminal = Terminal::new(TestBackend::new(120, 50)).unwrap();
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        let buffer = terminal.backend().buffer();
        let content = buffer_text(buffer);
        assert!(content.contains("base/project [no local repo]"));
        assert!(content.contains("topic #42 — virtual feature [virtual] [failure]"));
        assert_eq!(
            buffer
                .content()
                .iter()
                .filter(|cell| cell.fg == Color::LightMagenta)
                .map(|cell| cell.symbol())
                .collect::<String>(),
            "topic"
        );
        assert!(content.contains("[auto-merge]"));
        assert!(content.contains("viewer/fork:topic"));
        assert!(content.contains("head-sha"));
        assert!(content.contains("CHANGES_REQUESTED"));
        assert!(content.contains("auto-merge enabled"));
        assert!(content.contains("Enter to create worktree"));
    }

    #[test]
    fn check_states_use_semantic_colors() {
        assert_eq!(check_color(CheckRollup::Success), Color::Green);
        assert_eq!(check_color(CheckRollup::Failure), Color::Red);
        assert_eq!(check_color(CheckRollup::Error), Color::Red);
        assert_eq!(check_color(CheckRollup::Pending), Color::Yellow);
    }

    #[test]
    fn renders_empty_stale_and_action_palette_states() {
        let mut empty = App::new(Vec::new(), PathBuf::from("/outside"));
        let mut terminal = Terminal::new(TestBackend::new(90, 20)).unwrap();
        terminal.draw(|frame| render(frame, &mut empty)).unwrap();
        assert!(buffer_text(terminal.backend().buffer()).contains("No repositories or authored"));

        empty.modal = Some(Modal::Palette { selected: 0 });
        terminal.draw(|frame| render(frame, &mut empty)).unwrap();
        assert!(buffer_text(terminal.backend().buffer()).contains("Actions"));

        let stale = RepositoryView {
            config: RepositoryConfig {
                path: PathBuf::from("/missing"),
                label: Some("lost".to_owned()),
                worktree_root: None,
                github_remote: None,
                github_remotes: Default::default(),
                github_preferred_remote: None,
            },
            session_only: false,
            stale_error: Some("not found".to_owned()),
            expanded: true,
            worktrees: Vec::new(),
        };
        let mut stale_app = App::new(vec![stale], PathBuf::from("/outside"));
        terminal
            .draw(|frame| render(frame, &mut stale_app))
            .unwrap();
        let content = buffer_text(terminal.backend().buffer());
        assert!(content.contains("lost [stale]"));
        assert!(content.contains("not found"));

        let directory = tempfile::tempdir().unwrap();
        let invalid_path = directory.path().join("invalid");
        std::fs::create_dir(&invalid_path).unwrap();
        stale_app.repositories[0].config.path = invalid_path;
        stale_app.repositories[0].stale_error =
            Some("exists but is not a usable Git repository".to_owned());
        stale_app.selected = Some(stale_app.repositories[0].id());
        terminal
            .draw(|frame| render(frame, &mut stale_app))
            .unwrap();
        let content = buffer_text(terminal.backend().buffer());
        assert!(content.contains("lost [invalid]"));
        assert!(content.contains("invalid       exists but is not a usable Git repository"));
    }

    #[test]
    fn renders_progressive_and_stale_github_states() {
        let path = PathBuf::from("/repo");
        let repository = RepositoryView {
            config: RepositoryConfig {
                path: path.clone(),
                label: Some("project".to_owned()),
                worktree_root: None,
                github_remote: None,
                github_remotes: Default::default(),
                github_preferred_remote: None,
            },
            session_only: false,
            stale_error: None,
            expanded: true,
            worktrees: vec![Worktree {
                path: path.clone(),
                head: Some("1234567890".to_owned()),
                branch: Some("refs/heads/main".to_owned()),
                detached: false,
                bare: false,
                locked: None,
                prunable: None,
            }],
        };
        let mut app = App::new(vec![repository], PathBuf::from("/elsewhere"));
        app.selected = Some(RowId::Worktree(path.clone()));
        app.github_loading = true;
        app.github
            .insert(path.clone(), GitHubState::Loading { previous: None });
        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        let loading = buffer_text(terminal.backend().buffer());
        assert!(loading.contains("loading GitHub PRs"));
        assert!(loading.contains("PR …"));

        let previous = GitHubBranchData {
            pull_request: None,
            warnings: vec!["partial response".to_owned()],
            rate_limit: Some(RateLimit {
                remaining: 12,
                reset_at: "later".to_owned(),
            }),
        };
        app.github_loading = false;
        app.github.insert(
            path,
            GitHubState::Stale {
                previous: Some(previous),
                error: "network unavailable".to_owned(),
            },
        );
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        let stale = buffer_text(terminal.backend().buffer());
        assert!(stale.contains("GitHub stale: network unavailable"));
        app.pane = Pane::Detail;
        for _ in 0..20 {
            app.handle_key(crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char('j'),
                crossterm::event::KeyModifiers::NONE,
            ));
        }
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        let scrolled = buffer_text(terminal.backend().buffer());
        assert!(scrolled.contains("12 remaining"));
        assert!(scrolled.contains("warning: partial response"));
    }

    #[test]
    fn open_form_shows_the_rejection_reason_beside_its_fields() {
        let mut app = App::new(Vec::new(), PathBuf::from("/outside"));
        app.open_form(
            crate::app::Action::Create,
            vec![crate::app::FormField {
                label: "destination (blank = suggested)".to_owned(),
                value: String::new(),
            }],
        );
        app.inline_error = Some("destination parent does not exist: /trees".to_owned());
        let mut terminal = Terminal::new(TestBackend::new(90, 24)).unwrap();
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        let content = buffer_text(terminal.backend().buffer());
        assert!(content.contains("error: destination parent does not exist: /trees"));
    }

    #[test]
    fn create_form_identifies_the_selected_repository() {
        let repository = RepositoryView {
            config: RepositoryConfig {
                path: PathBuf::from("/src/project"),
                label: Some("project".to_owned()),
                worktree_root: None,
                github_remote: None,
                github_remotes: Default::default(),
                github_preferred_remote: None,
            },
            session_only: false,
            stale_error: None,
            expanded: true,
            worktrees: Vec::new(),
        };
        let mut app = App::new(vec![repository], PathBuf::from("/outside"));
        app.open_form(
            crate::app::Action::Create,
            vec![crate::app::FormField {
                label: "mode (existing/new/detached)".to_owned(),
                value: "new".to_owned(),
            }],
        );
        let mut terminal = Terminal::new(TestBackend::new(90, 24)).unwrap();
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        let content = buffer_text(terminal.backend().buffer());
        assert!(content.contains("Repository: project"));
        assert!(content.contains("Path: /src/project"));
    }

    #[test]
    fn shortens_home_prefix_only_on_component_boundaries() {
        let home = PathBuf::from("/Users/dev");
        assert_eq!(shorten_home(&PathBuf::from("/Users/dev"), Some(&home)), "~");
        assert_eq!(
            shorten_home(&PathBuf::from("/Users/dev/src/wt"), Some(&home)),
            "~/src/wt"
        );
        assert_eq!(
            shorten_home(&PathBuf::from("/Users/developer/src"), Some(&home)),
            "/Users/developer/src"
        );
        assert_eq!(
            shorten_home(&PathBuf::from("/opt/tools"), Some(&home)),
            "/opt/tools"
        );
        assert_eq!(shorten_home(&PathBuf::from("/opt"), None), "/opt");
        assert_eq!(
            shorten_home(&PathBuf::from("/opt"), Some(&PathBuf::from("/"))),
            "/opt"
        );
        assert_eq!(
            shorten_home(&PathBuf::from("/opt"), Some(&PathBuf::from(""))),
            "/opt"
        );
    }

    fn buffer_text(buffer: &ratatui::buffer::Buffer) -> String {
        buffer
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<Vec<_>>()
            .join("")
    }
}
