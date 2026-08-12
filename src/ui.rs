use std::hash::{Hash, Hasher};

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Clear, HighlightSpacing, List, ListItem, ListState, Paragraph, Wrap,
};

use crate::app::{App, GitHubState, InlineRowKind, InlineSection, Modal, StatusState, VisibleRow};
#[cfg(test)]
use crate::model::CheckRollup;
use crate::model::{
    MergeConflictState, PullRequestDetails, PullRequestState, RequiredCheckReadiness,
    ReviewReadiness,
};

const ACCENT: Color = Color::Cyan;
const BRANCH: Color = Color::LightBlue;
const REMOTE: Color = Color::LightMagenta;
const LINK: Color = Color::LightBlue;
const SUCCESS: Color = Color::Green;
const WARNING: Color = Color::Yellow;
const DANGER: Color = Color::Red;
const PR_NUMBER: Color = Color::Rgb(255, 165, 0);
const COMMENTS: Color = Color::Rgb(255, 140, 0);
const MUTED: Color = Color::DarkGray;
const SELECTION: Color = Color::Rgb(45, 55, 72);
const GITHUB_SPINNER_FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

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
            Span::styled(" wt ", Style::default().fg(Color::Black).bg(ACCENT)),
            Span::styled(
                " global worktrees",
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(header_progress(app), Style::default().fg(WARNING)),
        ])),
        vertical[0],
    );

    render_list(frame, app, vertical[1]);
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
    let tree_prefixes = tree_prefixes(app, &rows);
    let items: Vec<ListItem<'_>> = rows
        .iter()
        .zip(tree_prefixes)
        .map(|(row, tree_prefix)| match row {
            VisibleRow::Repository {
                repository_index, ..
            } => {
                let repository = &app.repositories[*repository_index];
                let arrow = if app.repository_expanded(*repository_index) {
                    "▾"
                } else {
                    "▸"
                };
                let states = [
                    repository.is_bare().then_some(("bare", BRANCH)),
                    repository.session_only.then_some(("session-only", WARNING)),
                    repository.stale_error.is_some().then_some((
                        if repository.config.path.exists() {
                            "invalid"
                        } else {
                            "stale"
                        },
                        DANGER,
                    )),
                ]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>();
                let state_width = states
                    .iter()
                    .map(|(state, _)| display_width(state) + 3)
                    .sum::<usize>();
                let available = area.width.saturating_sub(7) as usize;
                let label = truncate_label(
                    &repository.config.display_label(),
                    available.saturating_sub(state_width),
                );
                let mut spans = vec![Span::styled(
                    format!("{arrow} {label}"),
                    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
                )];
                spans.extend(states.into_iter().map(|(state, color)| {
                    Span::styled(format!(" [{state}]"), Style::default().fg(color))
                }));
                if let Some(error) = &repository.stale_error {
                    spans.push(Span::styled(
                        format!(" · {error}"),
                        Style::default().fg(DANGER),
                    ));
                }
                ListItem::new(Line::from(spans))
            }
            VisibleRow::Worktree {
                repository_index,
                worktree_index,
                expanded,
                ..
            } => {
                let repository = &app.repositories[*repository_index];
                let worktree = &repository.worktrees[*worktree_index];
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
                let github_state = app.github.get(&worktree.path);
                let pull_request = github_state
                    .and_then(GitHubState::data)
                    .and_then(|data| data.pull_request.as_ref());
                let details = pull_request.and_then(|pull_request| {
                    app.pull_request_details_for(repository, pull_request)
                        .map(|(_, details)| details)
                });
                let backburnered = pull_request
                    .and_then(|pull_request| app.pull_request_identity(repository, pull_request))
                    .is_some_and(|identity| app.is_backburnered(&identity));
                let mut suffix = pull_request
                    .map(|pull_request| {
                        pull_request_tree_spans(pull_request, details, false, backburnered)
                    })
                    .unwrap_or_default();
                suffix.extend(github_freshness_spans(
                    app.github_network_active(&worktree.path),
                    app.github_spinner_frame(),
                ));
                suffix.extend(local_state_spans(
                    app.statuses.get(&worktree.path),
                    worktree,
                ));
                let prefix_width = display_width(&tree_prefix) + 4;
                let line_width = area.width.saturating_sub(4) as usize;
                let label_width = line_width.saturating_sub(prefix_width).max(4);
                let mut spans = vec![
                    Span::styled(tree_prefix, Style::default().fg(MUTED)),
                    Span::styled(
                        if *expanded { "▾ " } else { "▸ " },
                        Style::default().fg(MUTED),
                    ),
                    Span::styled(
                        if current { "● " } else { "  " },
                        Style::default().fg(SUCCESS),
                    ),
                    Span::styled(
                        truncate_label(&identity, label_width),
                        Style::default().fg(BRANCH),
                    ),
                ];
                spans.extend(suffix);
                if backburnered {
                    for span in &mut spans {
                        span.style = span.style.add_modifier(Modifier::DIM);
                    }
                }
                single_line_tree_item(spans, line_width)
            }
            VisibleRow::VirtualRepository {
                virtual_repository_index,
                ..
            } => {
                let repository = &app.virtual_repositories[*virtual_repository_index];
                let arrow = if app.virtual_repository_expanded(*virtual_repository_index) {
                    "▾"
                } else {
                    "▸"
                };
                let marker = if repository.mapped_repository.is_none() {
                    " [no local repo]"
                } else {
                    ""
                };
                let label = truncate_label(
                    &repository.identity.full_name(),
                    (area.width.saturating_sub(7) as usize)
                        .saturating_sub(display_width(&tree_prefix) + display_width(marker)),
                );
                ListItem::new(Line::from(vec![
                    Span::styled(tree_prefix, Style::default().fg(MUTED)),
                    Span::styled(
                        format!("{arrow} {label}"),
                        Style::default().fg(REMOTE).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(marker, Style::default().fg(WARNING)),
                ]))
            }
            VisibleRow::VirtualPullRequest {
                virtual_repository_index,
                pull_request_index,
                expanded,
                ..
            } => {
                let pull_request = &app.virtual_repositories[*virtual_repository_index]
                    .pull_requests[*pull_request_index];
                let details = app.pull_request_details.get(&pull_request.identity);
                let backburnered = app.is_backburnered(&pull_request.identity);
                let suffix = pull_request_tree_spans(
                    &pull_request.pull_request,
                    details,
                    true,
                    backburnered,
                );
                let line_width = area.width.saturating_sub(4) as usize;
                let prefix_width = display_width(&tree_prefix) + 4;
                let label_width = line_width.saturating_sub(prefix_width).max(4);
                let mut spans = vec![
                    Span::styled(tree_prefix, Style::default().fg(MUTED)),
                    Span::styled(
                        if *expanded { "▾ " } else { "▸ " },
                        Style::default().fg(MUTED),
                    ),
                    Span::raw("  "),
                    Span::styled(
                        truncate_label(&pull_request.pull_request.head.branch, label_width),
                        Style::default().fg(REMOTE),
                    ),
                ];
                spans.extend(suffix);
                if backburnered {
                    for span in &mut spans {
                        span.style = span.style.add_modifier(Modifier::DIM);
                    }
                }
                single_line_tree_item(spans, line_width)
            }
            VisibleRow::Backburner { expanded, .. } => ListItem::new(Line::from(vec![
                Span::styled(tree_prefix, Style::default().fg(MUTED)),
                Span::styled(
                    format!("{} Backburner", if *expanded { "▾" } else { "▸" }),
                    Style::default().fg(MUTED).add_modifier(Modifier::BOLD),
                ),
            ])),
            VisibleRow::Inline {
                kind,
                section,
                text,
                expanded,
                ..
            } => {
                let line_width = area.width.saturating_sub(4) as usize;
                single_line_tree_item(
                    inline_row_spans(*kind, *section, text, *expanded, tree_prefix, line_width),
                    line_width,
                )
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
        .highlight_style(Style::default().bg(SELECTION).add_modifier(Modifier::BOLD))
        .highlight_symbol("▶ ")
        .highlight_spacing(HighlightSpacing::Always);
    frame.render_stateful_widget(list, area, &mut state);
    app.scroll = state.offset();
}

fn tree_prefixes(app: &App, rows: &[VisibleRow]) -> Vec<String> {
    let depths = rows
        .iter()
        .map(|row| visible_row_depth(app, row))
        .collect::<Vec<_>>();
    tree_prefixes_from_depths(&depths)
}

fn tree_prefixes_from_depths(depths: &[usize]) -> Vec<String> {
    depths
        .iter()
        .enumerate()
        .map(|(index, depth)| {
            if *depth == 0 {
                return String::new();
            }
            let mut prefix = String::new();
            for ancestor_depth in 1..*depth {
                let ancestor = (0..index)
                    .rev()
                    .find(|candidate| depths[*candidate] == ancestor_depth)
                    .expect("visible tree depth must have an ancestor");
                prefix.push_str(if has_later_sibling(depths, ancestor, ancestor_depth) {
                    "│  "
                } else {
                    "   "
                });
            }
            prefix.push_str(if has_later_sibling(depths, index, *depth) {
                "├─ "
            } else {
                "└─ "
            });
            prefix
        })
        .collect()
}

fn has_later_sibling(depths: &[usize], index: usize, depth: usize) -> bool {
    depths[index + 1..]
        .iter()
        .take_while(|candidate| **candidate >= depth)
        .any(|candidate| *candidate == depth)
}

fn visible_row_depth(app: &App, row: &VisibleRow) -> usize {
    match row {
        VisibleRow::Repository { .. } => 0,
        VisibleRow::Worktree { stack_depth, .. } => 1 + stack_depth,
        VisibleRow::VirtualRepository {
            virtual_repository_index,
            ..
        } => usize::from(
            app.virtual_repositories[*virtual_repository_index]
                .mapped_repository
                .is_some(),
        ),
        VisibleRow::VirtualPullRequest { stack_depth, .. } => 1 + *stack_depth,
        VisibleRow::Backburner { .. } => 1,
        VisibleRow::Inline { depth, .. } => *depth,
    }
}

fn list_block(app: &App) -> Block<'static> {
    let _ = app;
    Block::default()
        .title(" Repositories / Worktrees / Authored PRs · h/l collapse/expand · Enter/w opens ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(ACCENT))
}

fn inline_row_spans(
    kind: InlineRowKind,
    section: InlineSection,
    text: &str,
    expanded: Option<bool>,
    tree_prefix: String,
    line_width: usize,
) -> Vec<Span<'static>> {
    let mut spans = vec![Span::styled(tree_prefix, Style::default().fg(MUTED))];
    if let Some(expanded) = expanded {
        spans.push(Span::styled(
            if expanded { "▾ " } else { "▸ " },
            Style::default().fg(MUTED),
        ));
    }
    if kind == InlineRowKind::Section {
        let (label, summary) = text.split_once(" · ").unwrap_or((text, ""));
        spans.push(Span::styled(
            label.to_owned(),
            Style::default()
                .fg(if section == InlineSection::OpenComments {
                    COMMENTS
                } else {
                    MUTED
                })
                .add_modifier(Modifier::BOLD),
        ));
        match section {
            InlineSection::Checks => append_checks_header_spans(&mut spans, summary),
            InlineSection::Reviewers => append_reviewer_header_spans(&mut spans, summary),
            InlineSection::OpenComments if !summary.is_empty() => {
                spans.push(Span::raw("  "));
                spans.push(Span::styled(
                    summary.to_owned(),
                    Style::default().fg(DANGER),
                ));
            }
            _ if !summary.is_empty() => {
                spans.push(Span::styled(" · ", Style::default().fg(MUTED)));
                spans.push(Span::styled(
                    summary.to_owned(),
                    status_text_style(&summary.to_ascii_lowercase()),
                ));
            }
            _ => {}
        }
        return spans;
    }
    match kind {
        InlineRowKind::Metadata => {
            spans.extend(url_spans(
                text,
                semantic_text_style(&text.to_ascii_lowercase()),
            ));
        }
        InlineRowKind::Check => append_check_spans(&mut spans, text),
        InlineRowKind::Reviewer => append_reviewer_spans(&mut spans, text),
        InlineRowKind::ReviewSummary => spans.push(Span::raw(text.to_owned())),
        InlineRowKind::OpenComment => {
            append_open_comment_spans(&mut spans, text);
            return truncate_spans(spans, line_width);
        }
        InlineRowKind::Section => unreachable!("handled above"),
    }
    spans
}

fn append_checks_header_spans(spans: &mut Vec<Span<'static>>, summary: &str) {
    let mut parts = summary.split(" · ");
    let state = parts.next().unwrap_or("unknown");
    let ratio = parts.next().unwrap_or("unknown");
    let (glyph, style) = readiness_glyph_style(state);
    spans.push(Span::raw("  "));
    spans.push(Span::styled(glyph, style));
    spans.push(Span::raw(" "));
    spans.push(Span::styled(ratio.to_owned(), Style::default().fg(MUTED)));
    for extra in parts {
        spans.push(Span::styled(" · ", Style::default().fg(MUTED)));
        spans.push(Span::styled(extra.to_owned(), status_text_style(extra)));
    }
}

fn append_reviewer_header_spans(spans: &mut Vec<Span<'static>>, summary: &str) {
    if summary.is_empty() {
        return;
    }
    let bracket = Style::default().fg(MUTED);
    spans.push(Span::styled("  [", bracket));
    for (index, token) in summary.split(", ").enumerate() {
        if index > 0 {
            spans.push(Span::styled(", ", bracket));
        }
        spans.push(Span::styled(
            token.to_owned(),
            reviewer_summary_token_style(token),
        ));
    }
    spans.push(Span::styled("]", bracket));
}

fn readiness_glyph_style(state: &str) -> (&'static str, Style) {
    match state {
        "success" | "approved" => ("✓", Style::default().fg(SUCCESS)),
        "failure" | "error" | "changes requested" => (
            "✗",
            Style::default().fg(DANGER).add_modifier(Modifier::BOLD),
        ),
        "pending" | "expected" | "commented" => ("◉", Style::default().fg(WARNING)),
        "skipped" | "dismissed" => (
            "⊘",
            Style::default()
                .fg(MUTED)
                .add_modifier(Modifier::CROSSED_OUT),
        ),
        _ => ("○", Style::default().fg(MUTED)),
    }
}

fn reviewer_summary_token_style(token: &str) -> Style {
    if token == "req" {
        Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
    } else if token.contains("approved") {
        Style::default().fg(SUCCESS)
    } else if token.contains("changes") {
        Style::default().fg(DANGER).add_modifier(Modifier::BOLD)
    } else if token.contains("commented") {
        Style::default().fg(WARNING)
    } else {
        Style::default().fg(MUTED)
    }
}

fn append_check_spans(spans: &mut Vec<Span<'static>>, text: &str) {
    let mut parts = text.split(" · ");
    let name = parts.next().unwrap_or(text);
    let state = parts.next().unwrap_or("unknown");
    let required = parts.next() != Some("optional");
    let (glyph, glyph_style) = readiness_glyph_style(state);
    spans.push(Span::styled(glyph, glyph_style));
    spans.push(Span::raw(" "));
    spans.push(Span::styled(
        name.to_owned(),
        if required {
            Style::default()
        } else {
            Style::default().fg(MUTED)
        },
    ));
    if !required {
        spans.push(Span::styled(" (not required)", Style::default().fg(MUTED)));
    }
}

fn append_reviewer_spans(spans: &mut Vec<Span<'static>>, text: &str) {
    let mut parts = text.split(" · ");
    let name = parts.next().unwrap_or(text);
    let state = parts.next().unwrap_or("unknown");
    let requested = parts.next() == Some("requested");
    let (glyph, glyph_style) = readiness_glyph_style(state);
    spans.push(Span::styled(glyph, glyph_style));
    spans.push(Span::raw(" "));
    spans.push(Span::styled(
        name.to_owned(),
        Style::default().fg(color_for_login(name)),
    ));
    if requested {
        spans.push(Span::styled(
            "  [req]",
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ));
    } else {
        spans.push(Span::styled("  (reviewed)", Style::default().fg(MUTED)));
    }
}

fn append_open_comment_spans(spans: &mut Vec<Span<'static>>, text: &str) {
    let (author, remainder) = text.split_once(' ').unwrap_or((text, ""));
    spans.push(Span::styled(
        format!("{author} "),
        Style::default().fg(color_for_login(author)),
    ));
    let (without_outdated, outdated) = text_suffix(remainder, " [outdated]");
    let (body, path) = without_outdated
        .rfind(" (")
        .filter(|_| without_outdated.ends_with(')'))
        .map_or((without_outdated, None), |index| {
            (&without_outdated[..index], Some(&without_outdated[index..]))
        });
    spans.push(Span::raw(body.to_owned()));
    if let Some(path) = path {
        spans.push(Span::styled(path.to_owned(), Style::default().fg(MUTED)));
    }
    if outdated {
        spans.push(Span::styled(" [outdated]", Style::default().fg(MUTED)));
    }
}

fn text_suffix<'a>(text: &'a str, suffix: &str) -> (&'a str, bool) {
    text.strip_suffix(suffix)
        .map_or((text, false), |text| (text, true))
}

fn truncate_spans(spans: Vec<Span<'static>>, width: usize) -> Vec<Span<'static>> {
    if Line::from(spans.clone()).width() <= width {
        return spans;
    }
    if width == 0 {
        return Vec::new();
    }
    let budget = width - 1;
    let mut used = 0;
    let mut visible = Vec::new();
    'spans: for span in spans {
        let mut text = String::new();
        for character in span.content.chars() {
            let character_width = Span::raw(character.to_string()).width();
            if used + character_width > budget {
                if !text.is_empty() {
                    visible.push(Span::styled(text, span.style));
                }
                break 'spans;
            }
            text.push(character);
            used += character_width;
        }
        if !text.is_empty() {
            visible.push(Span::styled(text, span.style));
        }
    }
    visible.push(Span::raw("…"));
    visible
}

fn color_for_login(login: &str) -> Color {
    let hue = (raw_login_hue(login) + login_hue_offset()).rem_euclid(360.0);
    let (red, green, blue) = hsl_to_rgb(hue, 0.80, 0.60);
    Color::Rgb(red, green, blue)
}

fn raw_login_hue(login: &str) -> f32 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    hasher.write_u32(100);
    login
        .trim_start_matches('@')
        .to_ascii_lowercase()
        .hash(&mut hasher);
    (hasher.finish() % 360) as f32
}

fn login_hue_offset() -> f32 {
    const ANCHOR_LOGIN: &str = "wbbradley";
    const ANCHOR_HUE: f32 = 27.0;
    (ANCHOR_HUE - raw_login_hue(ANCHOR_LOGIN)).rem_euclid(360.0)
}

fn hsl_to_rgb(hue: f32, saturation: f32, lightness: f32) -> (u8, u8, u8) {
    let chroma = (1.0 - (2.0 * lightness - 1.0).abs()) * saturation;
    let hue_sector = hue / 60.0;
    let secondary = chroma * (1.0 - ((hue_sector % 2.0) - 1.0).abs());
    let (red, green, blue) = if hue_sector < 1.0 {
        (chroma, secondary, 0.0)
    } else if hue_sector < 2.0 {
        (secondary, chroma, 0.0)
    } else if hue_sector < 3.0 {
        (0.0, chroma, secondary)
    } else if hue_sector < 4.0 {
        (0.0, secondary, chroma)
    } else if hue_sector < 5.0 {
        (secondary, 0.0, chroma)
    } else {
        (chroma, 0.0, secondary)
    };
    let match_value = lightness - chroma / 2.0;
    let to_byte = |value: f32| ((value + match_value) * 255.0).round().clamp(0.0, 255.0) as u8;
    (to_byte(red), to_byte(green), to_byte(blue))
}

fn pull_request_tree_spans(
    pull_request: &crate::model::PullRequest,
    details: Option<&PullRequestDetails>,
    virtual_row: bool,
    backburnered: bool,
) -> Vec<Span<'static>> {
    let summary = details.map(PullRequestDetails::attention_summary);
    let mut spans = vec![Span::styled(
        format!(" · PR #{}", pull_request.number),
        Style::default().fg(PR_NUMBER).add_modifier(Modifier::BOLD),
    )];
    spans.push(tree_label(&pull_request.title, Color::White));
    match pull_request.state {
        PullRequestState::Draft => spans.push(tree_label("draft", Color::LightBlue)),
        PullRequestState::Merged => spans.push(tree_label("merged", Color::Green)),
        PullRequestState::Closed => spans.push(tree_label("closed", Color::DarkGray)),
        PullRequestState::Open => {}
    }
    if pull_request.auto_merge {
        spans.push(tree_label("[auto-merge]", SUCCESS));
    }
    let required_checks = summary
        .map(|summary| summary.required_checks)
        .unwrap_or(RequiredCheckReadiness::Unknown);
    if required_checks == RequiredCheckReadiness::Failure {
        spans.push(tree_label("checks failing", DANGER));
    }
    let review = summary
        .map(|summary| summary.review)
        .unwrap_or(ReviewReadiness::Unknown);
    match review {
        ReviewReadiness::ChangesRequested => {
            spans.push(tree_label("changes requested", DANGER));
        }
        ReviewReadiness::Waiting => spans.push(tree_label("review required", WARNING)),
        ReviewReadiness::Approved | ReviewReadiness::Unknown => {}
    }
    match summary.map(|summary| summary.merge_conflict) {
        Some(MergeConflictState::Conflicting) => {
            spans.push(tree_label("conflicts present", Color::Red));
        }
        Some(MergeConflictState::Unknown | MergeConflictState::Clean) | None => {}
    }
    if virtual_row {
        spans.push(tree_label("virtual-only", Color::Magenta));
    }
    if backburnered {
        spans.push(tree_label("backburner", Color::DarkGray));
    }
    spans
}

fn github_freshness_spans(network_active: bool, spinner_frame: usize) -> Vec<Span<'static>> {
    if network_active {
        vec![tree_label(
            GITHUB_SPINNER_FRAMES[spinner_frame % GITHUB_SPINNER_FRAMES.len()],
            Color::Yellow,
        )]
    } else {
        Vec::new()
    }
}

fn local_state_spans(
    state: Option<&StatusState>,
    worktree: &crate::model::Worktree,
) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    match state {
        Some(StatusState::Pending) => {
            spans.push(tree_label("local status loading", Color::DarkGray))
        }
        Some(StatusState::Ready(status)) if status.is_dirty() => {
            spans.push(Span::raw(" · ["));
            let mut needs_separator = false;
            for (count, marker, color) in [
                (status.staged, "+", SUCCESS),
                (status.unstaged, "~", WARNING),
                (status.untracked, "?", MUTED),
            ] {
                if count == 0 {
                    continue;
                }
                if needs_separator {
                    spans.push(Span::raw(" "));
                }
                spans.push(Span::styled(
                    format!("{marker}{count}"),
                    Style::default().fg(color),
                ));
                needs_separator = true;
            }
            spans.push(Span::raw("]"));
        }
        Some(StatusState::Error(_)) => {
            spans.push(tree_label("local status unavailable", Color::Yellow));
        }
        Some(StatusState::Ready(_)) | None => {}
    }
    if worktree.locked.is_some() {
        spans.push(tree_label("locked", Color::Yellow));
    }
    if worktree.prunable.is_some() {
        spans.push(tree_label("prunable", Color::Yellow));
    }
    spans
}

fn tree_label(text: &str, color: Color) -> Span<'static> {
    Span::styled(format!(" · {text}"), Style::default().fg(color))
}

fn single_line_tree_item(spans: Vec<Span<'static>>, line_width: usize) -> ListItem<'static> {
    ListItem::new(Line::from(truncate_spans(spans, line_width)))
}

fn truncate_label(label: &str, width: usize) -> String {
    if display_width(label) <= width {
        return label.to_owned();
    }
    if width == 0 {
        return String::new();
    }
    let budget = width - 1;
    let mut used = 0;
    let mut truncated = String::new();
    for character in label.chars() {
        let character_width = display_width(&character.to_string());
        if used + character_width > budget {
            break;
        }
        truncated.push(character);
        used += character_width;
    }
    truncated.push('…');
    truncated
}

fn display_width(text: &str) -> usize {
    Span::raw(text.to_owned()).width()
}

#[cfg(test)]
fn pr_number_spans(text: &str, base_style: Style) -> Vec<Span<'static>> {
    let Some(hash) = text.find('#') else {
        return vec![Span::styled(text.to_owned(), base_style)];
    };
    let digit_count = text[hash + 1..]
        .chars()
        .take_while(char::is_ascii_digit)
        .count();
    if digit_count == 0 {
        return vec![Span::styled(text.to_owned(), base_style)];
    }
    let end = hash + 1 + digit_count;
    let mut spans = Vec::new();
    if hash > 0 {
        spans.push(Span::styled(text[..hash].to_owned(), base_style));
    }
    spans.push(Span::styled(
        text[hash..end].to_owned(),
        Style::default().fg(PR_NUMBER).add_modifier(Modifier::BOLD),
    ));
    if end < text.len() {
        spans.push(Span::styled(text[end..].to_owned(), base_style));
    }
    spans
}

fn url_spans(text: &str, base_style: Style) -> Vec<Span<'static>> {
    let start = text.find("https://").or_else(|| text.find("http://"));
    let Some(start) = start else {
        return vec![Span::styled(text.to_owned(), base_style)];
    };
    let end = text[start..]
        .find(char::is_whitespace)
        .map(|offset| start + offset)
        .unwrap_or(text.len());
    let mut spans = Vec::new();
    if start > 0 {
        spans.push(Span::styled(text[..start].to_owned(), base_style));
    }
    spans.push(Span::styled(
        text[start..end].to_owned(),
        Style::default().fg(LINK).add_modifier(Modifier::UNDERLINED),
    ));
    if end < text.len() {
        spans.push(Span::styled(text[end..].to_owned(), base_style));
    }
    spans
}

fn semantic_text_style(text: &str) -> Style {
    if text.starts_with("title:") {
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD)
    } else if text.starts_with("url:") {
        Style::default().fg(MUTED)
    } else if text.starts_with("local repo:")
        && (text.contains("none") || text.contains("no local repo"))
    {
        Style::default().fg(WARNING)
    } else if text.starts_with("repository:") || text.starts_with("local repo:") {
        Style::default().fg(REMOTE)
    } else if text.starts_with("base:") || text.starts_with("head:") {
        Style::default().fg(BRANCH)
    } else if text.starts_with("head sha:") {
        Style::default().fg(MUTED)
    } else if text.starts_with("warning:") {
        Style::default().fg(WARNING)
    } else if text.starts_with("details stale:") {
        Style::default().fg(DANGER)
    } else if text.starts_with("attention details:") {
        Style::default().fg(MUTED)
    } else {
        status_text_style(text)
    }
}

fn status_text_style(text: &str) -> Style {
    if [
        "failure",
        "error",
        "changes requested",
        "changes_requested",
        "conflicting",
        "cancelled",
        "timed out",
    ]
    .iter()
    .any(|status| text.contains(status))
    {
        Style::default().fg(DANGER).add_modifier(Modifier::BOLD)
    } else if [
        "pending",
        "expected",
        "queued",
        "in progress",
        "requested:",
        "waiting",
    ]
    .iter()
    .any(|status| text.contains(status))
    {
        Style::default().fg(WARNING)
    } else if [
        "success", "approved", "clean", "merged", "enabled", "open", "neutral", "skipped",
    ]
    .iter()
    .any(|status| text.contains(status))
    {
        Style::default().fg(SUCCESS)
    } else if text.contains("draft") {
        Style::default().fg(BRANCH)
    } else if text.contains("unknown")
        || text.contains("unavailable")
        || text.contains("off")
        || text.contains("dismissed")
        || text.contains("closed")
        || text.contains("not checked")
    {
        Style::default().fg(MUTED)
    } else {
        Style::default()
    }
}

fn shortcut_line(shortcuts: &[(&str, &str)]) -> Line<'static> {
    let mut spans = Vec::new();
    for (index, (key, description)) in shortcuts.iter().enumerate() {
        if index > 0 {
            spans.push(Span::raw("  "));
        }
        spans.push(Span::styled(
            (*key).to_owned(),
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(
            format!(" {description}"),
            Style::default().fg(MUTED),
        ));
    }
    Line::from(spans)
}

fn render_footer(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let top = if app.filter_active {
        Line::from(vec![
            Span::styled(
                "/",
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("{}█", app.filter), Style::default().fg(WARNING)),
        ])
    } else if !app.filter.is_empty() {
        Line::from(vec![
            Span::styled("filter: ", Style::default().fg(MUTED)),
            Span::styled(app.filter.clone(), Style::default().fg(WARNING)),
            Span::styled(
                " · Esc clear · / replace · h/l fold",
                Style::default().fg(MUTED),
            ),
        ])
    } else {
        shortcut_line(&[
            ("j/k", "move"),
            ("[/]", "attention"),
            ("h/l", "fold"),
            ("/", "filter"),
            ("r", "refresh"),
            ("?", "actions"),
            ("Enter", "select/create"),
        ])
    };
    let bottom = app.inline_error.as_ref().map_or_else(
        || {
            if app.filter_active {
                return shortcut_line(&[("Enter", "apply"), ("Esc", "cancel search")]);
            }
            let exit_key = if app.filter.is_empty() { "q/Esc" } else { "q" };
            shortcut_line(&[
                ("w", "web"),
                ("c", "prompt"),
                ("p", "review"),
                ("b", "Backburner"),
                ("C/n", "create"),
                ("P", "prune"),
                ("m", "move"),
                (exit_key, "cancel"),
            ])
        },
        |error| {
            Line::styled(
                format!("error: {error}"),
                Style::default().fg(DANGER).add_modifier(Modifier::BOLD),
            )
        },
    );
    frame.render_widget(Paragraph::new(vec![top, bottom]), area);
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
                    let style = if availability.enabled {
                        Style::default()
                    } else {
                        Style::default().fg(MUTED).add_modifier(Modifier::DIM)
                    };
                    let shortcut_style = if availability.enabled {
                        Style::default().fg(ACCENT)
                    } else {
                        style
                    };
                    ListItem::new(Line::from(vec![
                        Span::styled(format!("[{}]", action.shortcut()), shortcut_style),
                        Span::styled(format!(" {}{suffix}", action.label()), style),
                    ]))
                })
                .collect();
            let mut state = ListState::default().with_selected(Some(*selected));
            frame.render_stateful_widget(
                List::new(items)
                    .block(Block::default().title(" Actions ").borders(Borders::ALL))
                    .highlight_style(Style::default().bg(SELECTION).add_modifier(Modifier::BOLD)),
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
            if matches!(
                action,
                crate::app::Action::Create | crate::app::Action::NewWorktree
            ) && let Some((repository, _)) = app.selected_repository()
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
                    Style::default().fg(MUTED),
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
                        Style::default().fg(ACCENT)
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
                    Style::default().fg(DANGER),
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
                Style::default().fg(WARNING),
            ));
            frame.render_widget(
                Paragraph::new(lines).wrap(Wrap { trim: false }).block(
                    Block::default()
                        .title(format!(" Confirm {} ", action.label()))
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(WARNING)),
                ),
                popup,
            );
        }
    }
}

#[cfg(test)]
fn field(label: &str, value: String) -> Line<'static> {
    let value_style = field_value_style(label, &value);
    let mut spans = vec![Span::styled(
        format!("{label:<11}"),
        Style::default().fg(MUTED),
    )];
    if matches!(label.to_ascii_lowercase().as_str(), "pr" | "pull request") {
        spans.extend(pr_number_spans(&value, value_style));
    } else {
        spans.push(Span::styled(value, value_style));
    }
    Line::from(spans)
}

#[cfg(test)]
fn field_value_style(label: &str, value: &str) -> Style {
    let label = label.to_ascii_lowercase();
    let value = value.to_ascii_lowercase();
    match label.as_str() {
        "url" => Style::default().fg(LINK).add_modifier(Modifier::UNDERLINED),
        "branch" | "base" | "head" | "upstream" => Style::default().fg(BRANCH),
        "pr" | "pull request" => Style::default().fg(PR_NUMBER),
        "repository" | "host" => Style::default().fg(REMOTE),
        "anchor" | "path" | "head sha" | "pr updated" | "rate limit" => Style::default().fg(MUTED),
        "state" | "review" | "auto-merge" => status_text_style(&value),
        "locked" | "prunable" => {
            if value == "no" || value == "-" {
                Style::default().fg(MUTED)
            } else {
                Style::default().fg(WARNING)
            }
        }
        "catalog" => {
            if value == "registered" {
                Style::default().fg(SUCCESS)
            } else {
                Style::default().fg(WARNING)
            }
        }
        "local repo" if value.contains("no local repo") || value.contains("none") => {
            Style::default().fg(WARNING)
        }
        "local repo" => Style::default().fg(REMOTE),
        "github" if value.contains("loading") => Style::default().fg(WARNING),
        "local" => {
            if value.starts_with("0 staged, 0 unstaged, 0 untracked") {
                Style::default().fg(SUCCESS)
            } else {
                Style::default().fg(DANGER)
            }
        }
        _ => Style::default(),
    }
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

#[cfg(test)]
fn check_color(checks: CheckRollup) -> Color {
    match checks {
        CheckRollup::Success => Color::Green,
        CheckRollup::Failure | CheckRollup::Error => Color::Red,
        CheckRollup::Pending | CheckRollup::Expected => Color::Yellow,
        CheckRollup::Unknown => Color::DarkGray,
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
    use crate::app::{BranchId, GitHubState, RepositoryView, RowId, VirtualRepositoryView};
    use crate::model::{
        AuthoredPullRequest, CanonicalPullRequestId, CheckRollup, CheckState, FeedbackKind,
        GitHubBranchData, GitHubRepositoryIdentity, MergeConflictState, PullRequest,
        PullRequestCheck, PullRequestDetails, PullRequestFeedback, PullRequestIdentity,
        PullRequestState, RateLimit, RepositoryConfig, ReviewerReview, SubmittedReviewState,
        Worktree, WorktreeStatus,
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
                locked: Some("in use".to_owned()),
                prunable: Some("missing".to_owned()),
            }],
        };
        let mut app = App::new(vec![repository], PathBuf::from("/elsewhere"));
        app.statuses.insert(
            PathBuf::from("/repo"),
            StatusState::Ready(WorktreeStatus {
                staged: 1,
                unstaged: 2,
                untracked: 3,
                ..WorktreeStatus::default()
            }),
        );
        app.selected = Some(RowId::Worktree(PathBuf::from("/repo")));
        let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        assert_eq!(app.viewport_height, 19);
        let content = buffer_text(terminal.backend().buffer());
        assert!(content.contains("project [session-only]"));
        assert!(content.contains("Worktree"));
        let row = buffer_lines(terminal.backend().buffer())
            .into_iter()
            .find(|line| line.contains("main"))
            .unwrap();
        assert!(row.contains("└─ ▾   main"));
        assert!(row.contains("main · [+1 ~2 ?3] · locked · prunable"));
        assert!(colored_text(terminal.backend().buffer(), SUCCESS).contains("+1"));
        assert!(colored_text(terminal.backend().buffer(), WARNING).contains("~2"));
        assert!(colored_text(terminal.backend().buffer(), MUTED).contains("?3"));
        assert!(!row.contains("/repo"));
        assert!(!row.contains("12345678"));
        assert!(!content.contains(" Details "));
        assert!(!content.contains("Tab"));

        app.selected = Some(RowId::Section(
            BranchId::Worktree(PathBuf::from("/repo")),
            InlineSection::Worktree,
        ));
        app.handle_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('l'),
            crossterm::event::KeyModifiers::NONE,
        ));
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        let expanded = buffer_text(terminal.backend().buffer());
        assert!(expanded.contains("path: /repo"));
        assert!(expanded.contains("HEAD: 1234567890"));
        let mut narrow_terminal = Terminal::new(TestBackend::new(80, 12)).unwrap();
        narrow_terminal
            .draw(|frame| render(frame, &mut app))
            .unwrap();
        let narrow_row = buffer_lines(narrow_terminal.backend().buffer())
            .into_iter()
            .find(|line| line.contains("main"))
            .unwrap();
        assert!(narrow_row.contains("main · [+1 ~2 ?3] · locked · prunable"));
        assert!(!narrow_row.contains("/repo"));
        assert!(!narrow_row.contains("12345678"));

        app.current_directory = PathBuf::from("/repo");
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        assert!(colored_text(terminal.backend().buffer(), SUCCESS).contains("●"));
        assert!(!buffer_text(terminal.backend().buffer()).contains("current main"));
    }

    #[test]
    fn renders_outer_branch_disclosures_and_stacked_worktree_connectors() {
        let repository = RepositoryView {
            config: RepositoryConfig {
                path: PathBuf::from("/repo"),
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
                    path: PathBuf::from("/repo"),
                    head: Some("parent-head".to_owned()),
                    branch: Some("refs/heads/parent".to_owned()),
                    detached: false,
                    bare: false,
                    locked: None,
                    prunable: None,
                },
                Worktree {
                    path: PathBuf::from("/repo-child"),
                    head: Some("child-head".to_owned()),
                    branch: Some("refs/heads/child".to_owned()),
                    detached: false,
                    bare: false,
                    locked: None,
                    prunable: None,
                },
            ],
        };
        let mut app = App::new(vec![repository], PathBuf::from("/outside"));
        app.branch_parents
            .insert(PathBuf::from("/repo-child"), PathBuf::from("/repo"));
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();

        terminal.draw(|frame| render(frame, &mut app)).unwrap();

        let buffer = terminal.backend().buffer();
        let content = buffer_text(buffer);
        assert!(content.contains("▾   parent"));
        assert!(content.contains("Stacked worktrees"));
        assert!(content.contains("▾   child"));
        assert!(colored_text(buffer, MUTED).contains("Stacked worktrees"));
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
                    branch: "feature/compact-attention-indicators-with-a-very-long-name".to_owned(),
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
        app.pull_request_details.insert(
            pull_request_id.clone(),
            PullRequestDetails {
                checks: vec![
                    PullRequestCheck {
                        name: "required".to_owned(),
                        state: CheckState::Success,
                        target_url: None,
                        required: true,
                        source_order: 0,
                        completed_at: None,
                    },
                    PullRequestCheck {
                        name: "optional".to_owned(),
                        state: CheckState::Failure,
                        target_url: None,
                        required: false,
                        source_order: 1,
                        completed_at: None,
                    },
                ],
                check_contexts_complete: true,
                reviewer_reviews: vec![ReviewerReview {
                    id: "review".to_owned(),
                    database_id: Some(9),
                    reviewer: "reviewer".to_owned(),
                    state: SubmittedReviewState::ChangesRequested,
                    submitted_at: None,
                }],
                reviews_complete: true,
                feedback: vec![PullRequestFeedback {
                    id: "comment".to_owned(),
                    database_id: Some(10),
                    thread_id: Some("thread".to_owned()),
                    kind: FeedbackKind::InlineThread,
                    author: "reviewer".to_owned(),
                    body: "fix this".to_owned(),
                    path: Some("src/lib.rs".to_owned()),
                    permalink: None,
                    outdated: false,
                }],
                feedback_complete: true,
                merge_conflict: MergeConflictState::Conflicting,
                warnings: Vec::new(),
                ..PullRequestDetails::default()
            },
        );
        app.selected = Some(RowId::Section(
            BranchId::VirtualPullRequest(pull_request_id.clone()),
            InlineSection::Overview,
        ));
        app.handle_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('l'),
            crossterm::event::KeyModifiers::NONE,
        ));
        let mut terminal = Terminal::new(TestBackend::new(200, 70)).unwrap();
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        let buffer = terminal.backend().buffer();
        let content = buffer_text(buffer);
        assert!(content.contains("base/project [no local repo]"));
        assert!(content.contains("└─ ▾   feature/compact-attention"));
        assert!(content.contains("feature/compact-attention-indicators-with-a-very-long-name"));
        assert!(content.contains("PR #42"));
        assert!(content.contains("virtual feature"));
        assert!(content.contains("changes requested"));
        assert!(content.contains("conflicts present"));
        assert!(content.contains("virtual-only"));
        assert!(
            colored_text(buffer, Color::LightMagenta)
                .contains("feature/compact-attention-indicators-with-a-very-long-name")
        );
        assert!(colored_text(buffer, PR_NUMBER).contains("#42"));
        let red = colored_text(buffer, Color::Red);
        assert!(red.contains("changes requested"));
        assert!(red.contains("conflicts present"));
        assert!(content.contains("head: viewer/fork:"));
        assert!(content.contains("head SHA: head-sha"));
        assert!(content.contains("changes requested"));
        assert!(content.contains("auto-merge: enabled"));
        assert!(content.contains("h/l collapse/expand · Enter/w opens"));
        assert!(content.contains("Overview · draft · auto-merge enabled · conflicts conflicting"));
        assert!(content.contains("Checks  ✓ 1/1 required · 1 optional failure"));
        assert!(content.contains("Reviewers  [✗ changes]"));
        assert!(content.contains("Open comments  1 unresolved"));
        assert!(!content.contains("unresolved comment"));
        assert!(content.contains("fix this"));

        let mut narrow_terminal = Terminal::new(TestBackend::new(80, 30)).unwrap();
        narrow_terminal
            .draw(|frame| render(frame, &mut app))
            .unwrap();
        let narrow_buffer = narrow_terminal.backend().buffer();
        let narrow_content = buffer_text(narrow_buffer);
        assert!(narrow_content.contains("PR #42"));
        assert!(narrow_content.contains("draft"));
        let narrow_lines = buffer_lines(narrow_buffer);
        let branch_line = narrow_lines
            .iter()
            .position(|line| line.contains("feature/compact-attention"))
            .unwrap();
        assert!(narrow_lines[branch_line].contains('…'));
        assert!(narrow_lines[branch_line + 1].contains("Overview"));

        app.virtual_repositories[0].pull_requests[0]
            .pull_request
            .auto_merge = false;
        narrow_terminal
            .draw(|frame| render(frame, &mut app))
            .unwrap();
        let tree_row = buffer_lines(narrow_terminal.backend().buffer())
            .into_iter()
            .find(|line| line.contains("feature/compact-attention"))
            .unwrap();
        assert!(!tree_row.contains("auto-merge"));
        app.virtual_repositories[0].pull_requests[0]
            .pull_request
            .auto_merge = true;

        app.backburner.insert(pull_request_id.clone());
        app.selected = Some(RowId::Backburner(pull_request_id.repository.clone()));
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        let collapsed = buffer_text(terminal.backend().buffer());
        assert!(collapsed.contains("Backburner"));
        assert!(!collapsed.contains("#42"));
        app.handle_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('l'),
            crossterm::event::KeyModifiers::NONE,
        ));
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        let expanded = buffer_text(terminal.backend().buffer());
        assert!(expanded.contains("PR #42"));
        assert!(expanded.contains("virtual feature"));
        assert!(expanded.contains("backburner"));
    }

    #[test]
    fn check_states_use_semantic_colors() {
        assert_eq!(check_color(CheckRollup::Success), Color::Green);
        assert_eq!(check_color(CheckRollup::Failure), Color::Red);
        assert_eq!(check_color(CheckRollup::Error), Color::Red);
        assert_eq!(check_color(CheckRollup::Pending), Color::Yellow);
    }

    #[test]
    fn inline_checks_and_reviewers_use_rollup_glyphs_tokens_and_login_colors() {
        let checks = inline_row_spans(
            InlineRowKind::Section,
            InlineSection::Checks,
            "Checks · failure · 1/2 required",
            Some(false),
            String::new(),
            80,
        );
        assert_eq!(
            checks
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>(),
            "▸ Checks  ✗ 1/2 required"
        );
        let failure = checks.iter().find(|span| span.content == "✗").unwrap();
        assert_eq!(failure.style.fg, Some(DANGER));
        assert!(failure.style.add_modifier.contains(Modifier::BOLD));

        let requested = inline_row_spans(
            InlineRowKind::Reviewer,
            InlineSection::Reviewers,
            "OctoCat · approved · requested",
            None,
            String::new(),
            80,
        );
        assert_eq!(
            requested
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>(),
            "✓ OctoCat  [req]"
        );
        assert_eq!(
            requested
                .iter()
                .find(|span| span.content == "OctoCat")
                .unwrap()
                .style
                .fg,
            Some(color_for_login("octocat"))
        );
        assert_eq!(
            requested
                .iter()
                .find(|span| span.content == "  [req]")
                .unwrap()
                .style
                .fg,
            Some(ACCENT)
        );

        let changes = inline_row_spans(
            InlineRowKind::Reviewer,
            InlineSection::Reviewers,
            "reviewer · changes requested · reviewed",
            None,
            String::new(),
            80,
        );
        let changes_glyph = changes.iter().find(|span| span.content == "✗").unwrap();
        assert_eq!(changes_glyph.style.fg, Some(DANGER));
        assert!(changes.iter().any(|span| span.content == "  (reviewed)"));

        let summary = inline_row_spans(
            InlineRowKind::ReviewSummary,
            InlineSection::Reviewers,
            "Please fix the edge case",
            None,
            "└─ ".to_owned(),
            80,
        );
        assert_eq!(
            summary
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>(),
            "└─ Please fix the edge case"
        );
    }

    #[test]
    fn check_rows_cover_pending_valid_unknown_and_optional_styles() {
        for (state, glyph, color) in [
            ("expected", "◉", WARNING),
            ("neutral", "○", MUTED),
            ("unknown", "○", MUTED),
            ("skipped", "⊘", MUTED),
        ] {
            let spans = inline_row_spans(
                InlineRowKind::Check,
                InlineSection::Checks,
                &format!("ci · {state} · optional"),
                None,
                String::new(),
                80,
            );
            let glyph_span = spans.iter().find(|span| span.content == glyph).unwrap();
            assert_eq!(glyph_span.style.fg, Some(color));
            assert!(spans.iter().any(|span| span.content == " (not required)"));
            if state == "skipped" {
                assert!(
                    glyph_span
                        .style
                        .add_modifier
                        .contains(Modifier::CROSSED_OUT)
                );
            }
        }
    }

    #[test]
    fn open_comments_color_authors_mark_outdated_and_use_true_display_width() {
        let full = inline_row_spans(
            InlineRowKind::OpenComment,
            InlineSection::OpenComments,
            "@Reviewer please inspect 🧪 unicode (src/lib.rs) [outdated]",
            None,
            "└─ ".to_owned(),
            80,
        );
        assert_eq!(
            full.iter()
                .find(|span| span.content == "@Reviewer ")
                .unwrap()
                .style
                .fg,
            Some(color_for_login("reviewer"))
        );
        assert_eq!(
            full.iter()
                .find(|span| span.content == " (src/lib.rs)")
                .unwrap()
                .style
                .fg,
            Some(MUTED)
        );
        assert_eq!(
            full.iter()
                .find(|span| span.content == " [outdated]")
                .unwrap()
                .style
                .fg,
            Some(MUTED)
        );

        let narrow = inline_row_spans(
            InlineRowKind::OpenComment,
            InlineSection::OpenComments,
            "@Reviewer please inspect 🧪 unicode (src/lib.rs) [outdated]",
            None,
            "└─ ".to_owned(),
            24,
        );
        let line = Line::from(narrow.clone());
        assert_eq!(line.width(), 24);
        assert_eq!(narrow.last().unwrap().content.as_ref(), "…");
    }

    #[test]
    fn truncates_labels_by_terminal_columns() {
        assert_eq!(truncate_label("界界界", 5), "界界…");
        assert_eq!(display_width(&truncate_label("界界界", 5)), 5);
        assert_eq!(truncate_label("界界界", 4), "界…");
        assert_eq!(truncate_label("界", 1), "…");
        assert_eq!(truncate_label("界", 0), "");
    }

    #[test]
    fn truncates_unicode_tree_content_to_one_line_and_preserves_styles() {
        let connector_style = Style::default().fg(MUTED);
        let branch_style = Style::default().fg(BRANCH);
        let pr_style = Style::default().fg(PR_NUMBER).add_modifier(Modifier::BOLD);
        let spans = vec![
            Span::styled("└─ ▾ ", connector_style),
            Span::styled("界界-branch", branch_style),
            Span::styled(" · PR #42", pr_style),
            Span::styled(" · 長い Unicode title", Style::default()),
        ];
        let visible = truncate_spans(spans.clone(), 26);
        let line = Line::from(visible.clone());
        let item = single_line_tree_item(spans, 26);

        assert_eq!(item.height(), 1);
        assert!(item.width() <= 26);
        assert!(line.width() <= 26);
        assert!(
            visible.iter().any(|span| {
                span.content.contains("└─ ▾") && span.style == connector_style
            })
        );
        assert!(
            visible
                .iter()
                .any(|span| span.content.contains("界界") && span.style == branch_style)
        );
        assert!(
            visible
                .iter()
                .any(|span| span.content.contains("#42") && span.style == pr_style)
        );

        let single_column = truncate_spans(vec![Span::styled("界", branch_style)], 1);
        assert_eq!(single_column[0].content.as_ref(), "…");
        assert!(Line::from(single_column).width() <= 1);
    }

    #[test]
    fn bounds_long_unicode_reviewer_and_comment_rows_by_display_width() {
        let reviewer = inline_row_spans(
            InlineRowKind::Reviewer,
            InlineSection::Reviewers,
            "審査者審査者審査者 · changes requested · reviewed",
            None,
            "│  └─ ".to_owned(),
            24,
        );
        let comment = inline_row_spans(
            InlineRowKind::OpenComment,
            InlineSection::OpenComments,
            "@審査者審査者 長いコメント本文 🧪 (src/界.rs) [outdated]",
            None,
            "│  └─ ".to_owned(),
            24,
        );
        for spans in [reviewer, comment] {
            assert!(spans.iter().any(|span| span.content.contains("審査者")));
            let item = single_line_tree_item(spans, 24);
            assert_eq!(item.height(), 1);
            assert!(item.width() <= 24);
        }
    }

    #[test]
    fn tree_prefixes_render_siblings_ancestry_and_roots() {
        assert_eq!(
            tree_prefixes_from_depths(&[0, 1, 2, 1, 0, 1]),
            ["", "├─ ", "│  └─ ", "└─ ", "", "└─ "]
        );
    }

    #[test]
    fn semantic_palette_covers_navigation_identity_metadata_and_status() {
        assert_eq!(
            field_value_style("URL", "https://example.test").fg,
            Some(LINK)
        );
        assert_eq!(
            field_value_style("branch", "feature/colors").fg,
            Some(BRANCH)
        );
        assert_eq!(
            field_value_style("repository", "team/project").fg,
            Some(REMOTE)
        );
        assert_eq!(field_value_style("PR", "#42 open").fg, Some(PR_NUMBER));
        assert_eq!(field_value_style("path", "/tmp/project").fg, Some(MUTED));
        assert_eq!(status_text_style("success").fg, Some(SUCCESS));
        assert_eq!(status_text_style("pending").fg, Some(WARNING));
        assert_eq!(status_text_style("failure").fg, Some(DANGER));
        assert_eq!(status_text_style("unknown").fg, Some(MUTED));

        let footer = shortcut_line(&[("j/k", "move")]);
        assert_eq!(footer.spans[0].style.fg, Some(ACCENT));
        assert_eq!(footer.spans[1].style.fg, Some(MUTED));

        let url = url_spans(
            "URL: https://example.test/path trailing",
            Style::default().fg(MUTED),
        );
        assert_eq!(url.len(), 3);
        assert!(!url[0].style.add_modifier.contains(Modifier::UNDERLINED));
        assert!(url[1].style.add_modifier.contains(Modifier::UNDERLINED));
        assert_eq!(url[1].content.as_ref(), "https://example.test/path");
        assert!(!url[2].style.add_modifier.contains(Modifier::UNDERLINED));

        let pull_request = field("PR", "#42 open".to_owned());
        assert_eq!(pull_request.spans[1].content.as_ref(), "#42");
        assert_eq!(pull_request.spans[1].style.fg, Some(PR_NUMBER));
    }

    #[test]
    fn committed_filter_footer_explains_clear_replace_and_folding() {
        let repository = RepositoryView {
            config: RepositoryConfig {
                path: PathBuf::from("/repo"),
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
        let mut app = App::new(vec![repository], PathBuf::from("/elsewhere"));
        let mut shortcuts = Terminal::new(TestBackend::new(160, 12)).unwrap();
        shortcuts.draw(|frame| render(frame, &mut app)).unwrap();
        let shortcut_content = buffer_text(shortcuts.backend().buffer());
        for expected in ["c prompt", "p review", "C/n create", "P prune"] {
            assert!(shortcut_content.contains(expected), "missing {expected}");
        }

        app.filter = "project".to_owned();
        let mut terminal = Terminal::new(TestBackend::new(100, 12)).unwrap();
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        assert!(
            buffer_text(terminal.backend().buffer())
                .contains("filter: project · Esc clear · / replace · h/l fold")
        );
    }

    #[test]
    fn renders_empty_stale_and_action_palette_states() {
        let mut empty = App::new(Vec::new(), PathBuf::from("/outside"));
        let mut terminal = Terminal::new(TestBackend::new(100, 40)).unwrap();
        terminal.draw(|frame| render(frame, &mut empty)).unwrap();
        assert!(buffer_text(terminal.backend().buffer()).contains("No repositories or authored"));

        empty.modal = Some(Modal::Palette { selected: 0 });
        terminal.draw(|frame| render(frame, &mut empty)).unwrap();
        let palette = buffer_text(terminal.backend().buffer());
        assert!(palette.contains("Actions"));
        for action in crate::app::Action::ALL {
            let expected = format!("[{}] {}", action.shortcut(), action.label());
            assert!(palette.contains(&expected), "missing {expected}");
        }
        assert!(palette.contains("select a repository or worktree"));

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
        assert!(content.contains("exists but is not a usable Git repository"));
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
        let generation = app.begin_github_refresh(std::slice::from_ref(&path));
        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        let loading = buffer_text(terminal.backend().buffer());
        assert!(loading.contains("loading GitHub PRs"));
        assert!(loading.contains("· ⠋"));
        assert!(!loading.contains("GitHub refreshing"));

        app.advance_github_spinner();
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        assert!(buffer_text(terminal.backend().buffer()).contains("· ⠙"));

        app.apply_pull_request_details(generation, Default::default());
        app.github.insert(
            path.clone(),
            GitHubState::Ready(GitHubBranchData {
                pull_request: Some(PullRequest {
                    number: 42,
                    title: "merged change".to_owned(),
                    url: "https://github.com/base/project/pull/42".to_owned(),
                    state: PullRequestState::Merged,
                    updated_at: "2026-01-01T00:00:00Z".to_owned(),
                    review_decision: Some("APPROVED".to_owned()),
                    auto_merge: false,
                    base: PullRequestIdentity {
                        repository: Some("base/project".to_owned()),
                        branch: "main".to_owned(),
                        oid: Some("base-sha".to_owned()),
                    },
                    head: PullRequestIdentity {
                        repository: Some("base/project".to_owned()),
                        branch: "topic".to_owned(),
                        oid: Some("1234567890".to_owned()),
                    },
                    checks: CheckRollup::Success,
                }),
                warnings: Vec::new(),
                rate_limit: None,
            }),
        );
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        let merged_buffer = terminal.backend().buffer();
        assert!(buffer_text(merged_buffer).contains("PR #42 · merged"));
        assert!(colored_text(merged_buffer, Color::Green).contains("merged"));

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
        assert!(!stale.contains("GitHub stale"));
        assert!(!stale.contains("network unavailable"));
        app.selected = Some(RowId::Section(
            BranchId::Worktree(PathBuf::from("/repo")),
            InlineSection::Worktree,
        ));
        app.handle_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('l'),
            crossterm::event::KeyModifiers::NONE,
        ));
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        let scrolled = buffer_text(terminal.backend().buffer());
        assert!(scrolled.contains("GitHub stale: network unavailable"));
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
    fn renders_header_progress_footer_error_and_confirmation() {
        let mut app = App::new(Vec::new(), PathBuf::from("/outside"));
        app.progress = Some("performing operation…".to_owned());
        app.inline_error = Some("clipboard unavailable".to_owned());
        let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        let status = buffer_text(terminal.backend().buffer());
        assert!(status.contains("·  performing operation…"));
        assert!(status.contains("error: clipboard unavailable"));

        app.inline_error = None;
        app.modal = Some(Modal::Confirm {
            action: crate::app::Action::Prune,
            summary: vec!["remove 2 stale worktree records".to_owned()],
        });
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        let confirmation = buffer_text(terminal.backend().buffer());
        assert!(confirmation.contains("Confirm prune stale records"));
        assert!(confirmation.contains("remove 2 stale worktree records"));
        assert!(confirmation.contains("Enter/y confirms · n/Esc cancels"));
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

    fn buffer_lines(buffer: &ratatui::buffer::Buffer) -> Vec<String> {
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect()
    }

    fn colored_text(buffer: &ratatui::buffer::Buffer, color: Color) -> String {
        buffer
            .content()
            .iter()
            .filter(|cell| cell.fg == color)
            .map(|cell| cell.symbol())
            .collect()
    }
}
