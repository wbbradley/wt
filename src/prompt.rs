use crate::model::{
    CanonicalPullRequestId, FeedbackKind, PullRequest, PullRequestCheck, PullRequestFeedback,
};
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PromptWorktree {
    pub path: PathBuf,
    pub branch: Option<String>,
    pub head: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PromptPullRequest {
    pub identity: CanonicalPullRequestId,
    pub pull_request: PullRequest,
    pub checks: Vec<PullRequestCheck>,
    pub feedback: Vec<PullRequestFeedback>,
    pub worktree: Option<PromptWorktree>,
}

pub fn format_agent_prompt(pull_requests: &[PromptPullRequest]) -> Option<String> {
    let actionable = pull_requests
        .iter()
        .filter(|pull_request| !pull_request.checks.is_empty() || !pull_request.feedback.is_empty())
        .collect::<Vec<_>>();
    if actionable.is_empty() {
        return None;
    }

    let mut output = String::new();
    let mut has_thread_comments = false;
    for pull_request in actionable {
        let repository = &pull_request.identity.repository;
        if pull_request.pull_request.head.branch.is_empty() {
            output.push_str(&format!(
                "#{} {}:\n",
                pull_request.identity.number, pull_request.pull_request.title,
            ));
        } else {
            output.push_str(&format!(
                "In {} (#{} {}):\n",
                pull_request.pull_request.head.branch,
                pull_request.identity.number,
                pull_request.pull_request.title,
            ));
        }

        output.push('\n');
        format_worktree_guidance(&mut output, pull_request);

        let thread_comments = pull_request
            .feedback
            .iter()
            .filter(|feedback| feedback.kind == FeedbackKind::InlineThread)
            .filter_map(|feedback| feedback.database_id.map(|id| (feedback, id)))
            .collect::<Vec<_>>();
        if !thread_comments.is_empty() {
            has_thread_comments = true;
            output.push_str("\nComment IDs:\n");
            for (feedback, id) in thread_comments {
                output.push_str(&format!("  - {id} - {}\n", excerpt(&feedback.body)));
            }
            output.push_str(
                "\nPlease use the following command to investigate each review comment.\n```\n",
            );
            output.push_str(&format!(
                "gh api --hostname {} repos/{}/{}/pulls/comments/$comment_id --jq '{{id,path,line,body,created_at,updated_at}}'\n```\n",
                repository.host, repository.owner, repository.repository,
            ));
        }

        let review_summaries = pull_request
            .feedback
            .iter()
            .filter(|feedback| feedback.kind == FeedbackKind::ReviewSummary)
            .filter_map(|feedback| feedback.database_id.map(|id| (feedback, id)))
            .collect::<Vec<_>>();
        if !review_summaries.is_empty() {
            output.push_str("\nReview IDs:\n");
            for (feedback, id) in review_summaries {
                output.push_str(&format!("  - {id} - {}\n", excerpt(&feedback.body)));
            }
            output.push_str(
                "\nPlease use the following command to investigate each review summary and respond as appropriate.\n```\n",
            );
            output.push_str(&format!(
                "gh api --hostname {} repos/{}/{}/pulls/{}/reviews/$review_id --jq '{{id,body,state,submitted_at}}'\n```\n",
                repository.host,
                repository.owner,
                repository.repository,
                pull_request.identity.number,
            ));
        }

        let unidentified = pull_request
            .feedback
            .iter()
            .filter(|feedback| feedback.database_id.is_none())
            .collect::<Vec<_>>();
        if !unidentified.is_empty() {
            output.push_str("\nComments:\n");
            for feedback in unidentified {
                let reference = feedback.permalink.as_deref().unwrap_or(&feedback.id);
                output.push_str(&format!("  - {reference} - {}\n", excerpt(&feedback.body)));
            }
        }

        if !pull_request.checks.is_empty() {
            if pull_request
                .checks
                .iter()
                .all(|check| check.state.is_actionable())
            {
                output.push_str("\nChecks (all failed):\n");
            } else {
                output.push_str("\nChecks:\n");
            }
            for check in &pull_request.checks {
                let url = check
                    .target_url
                    .as_deref()
                    .unwrap_or(&pull_request.pull_request.url);
                output.push_str(&format!("  - {} ({url})\n", check.name));
            }
        }
        output.push('\n');
    }
    output.truncate(output.trim_end_matches('\n').len());
    if has_thread_comments {
        output.push_str("\n\nFix, reply to the comments, and mark as resolved as appropriate.");
    }
    Some(output)
}

fn format_worktree_guidance(output: &mut String, pull_request: &PromptPullRequest) {
    let Some(worktree) = &pull_request.worktree else {
        output.push_str("No existing worktree is mapped to this pull request.\n");
        return;
    };

    let branch = worktree
        .branch
        .as_deref()
        .and_then(|branch| branch.strip_prefix("refs/heads/").or(Some(branch)));
    output.push_str("Use this existing checkout:\n```bash\ncd -- ");
    output.push_str(&shell_quote(&worktree.path.to_string_lossy()));
    output.push_str("\n```\n");
    if let Some(branch) = branch {
        output.push_str("Branch `");
        output.push_str(branch);
        output.push_str("` is checked out there. ");
    }

    match (
        worktree.head.as_deref(),
        pull_request.pull_request.head.oid.as_deref(),
    ) {
        (Some(local), Some(remote)) if local == remote => {
            output.push_str("Local HEAD `");
            output.push_str(&short_commit(local));
            output.push_str("` matches the PR head.\n");
        }
        (Some(local), Some(remote)) => {
            output.push_str("Local HEAD `");
            output.push_str(&short_commit(local));
            output.push_str("` does not match PR head `");
            output.push_str(&short_commit(remote));
            output.push_str("`; synchronize it before addressing this feedback.\n");
        }
        (Some(local), None) => {
            output.push_str("Local HEAD is `");
            output.push_str(&short_commit(local));
            output.push_str("`; the PR head commit is unavailable.\n");
        }
        (None, Some(remote)) => {
            output.push_str("Local HEAD is unavailable; confirm it matches PR head `");
            output.push_str(&short_commit(remote));
            output.push_str("` before making changes.\n");
        }
        (None, None) => output.push('\n'),
    }
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn short_commit(commit: &str) -> String {
    commit.chars().take(8).collect()
}

pub fn format_review_request(pull_requests: &[PromptPullRequest]) -> Option<String> {
    if pull_requests.is_empty() {
        return None;
    }
    Some(
        pull_requests
            .iter()
            .map(|pull_request| {
                let title = strip_conventional_commit_prefix(&pull_request.pull_request.title);
                let mut line = format!("{} - {title}", pull_request.pull_request.url);
                if pull_request.pull_request.state == crate::model::PullRequestState::Draft {
                    line.push_str(" - DRAFT");
                }
                line
            })
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

fn strip_conventional_commit_prefix(title: &str) -> &str {
    const TYPES: [&str; 11] = [
        "feat", "fix", "docs", "style", "refactor", "perf", "test", "build", "ci", "chore",
        "revert",
    ];
    let Some((head, rest)) = title.split_once(':') else {
        return title;
    };
    let head = head.strip_suffix('!').unwrap_or(head);
    let type_token = match head.split_once('(') {
        Some((kind, scope)) if scope.ends_with(')') => kind,
        Some(_) => return title,
        None => head,
    };
    if !TYPES
        .iter()
        .any(|kind| kind.eq_ignore_ascii_case(type_token))
    {
        return title;
    }
    let stripped = rest.trim_start_matches(' ');
    if stripped.is_empty() { title } else { stripped }
}

fn excerpt(body: &str) -> String {
    concise_comment_text(body).chars().take(100).collect()
}

pub fn concise_comment_text(text: &str) -> String {
    let stripped = strip_html_comments(text);
    let mut concise = String::with_capacity(stripped.len());

    for line in stripped.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let line = strip_markdown_line_prefix(line);
        let line = line
            .replace("**", "")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        if line.is_empty() {
            continue;
        }

        if !concise.is_empty() {
            concise.push_str(" • ");
        }
        concise.push_str(&line);
    }

    concise
}

fn strip_markdown_line_prefix(line: &str) -> &str {
    if let Some(rest) = line.strip_prefix("- ") {
        return rest.trim_start();
    }

    let heading_marks = line.bytes().take_while(|byte| *byte == b'#').count();
    if heading_marks > 0 && line.as_bytes().get(heading_marks) == Some(&b' ') {
        return line[heading_marks + 1..].trim_start();
    }

    line
}

fn strip_html_comments(text: &str) -> String {
    let mut stripped = String::with_capacity(text.len());
    let mut remainder = text;

    while let Some(comment_start) = remainder.find("<!--") {
        stripped.push_str(&remainder[..comment_start]);
        remainder = &remainder[comment_start + "<!--".len()..];
        let Some(comment_end) = remainder.find("-->") else {
            return stripped;
        };
        remainder = &remainder[comment_end + "-->".len()..];
    }
    stripped.push_str(remainder);
    stripped
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        CheckRollup, CheckState, GitHubRepositoryIdentity, PullRequestIdentity, PullRequestState,
    };

    fn pull_request() -> PromptPullRequest {
        PromptPullRequest {
            identity: CanonicalPullRequestId {
                repository: GitHubRepositoryIdentity::canonical(
                    "git.example.com",
                    "Base",
                    "Project",
                ),
                number: 42,
            },
            pull_request: PullRequest {
                number: 42,
                title: "Fix feedback".to_owned(),
                url: "https://git.example.com/base/project/pull/42".to_owned(),
                state: PullRequestState::Open,
                updated_at: "2026-08-07T00:00:00Z".to_owned(),
                review_decision: None,
                auto_merge: false,
                base: PullRequestIdentity {
                    repository: Some("base/project".to_owned()),
                    branch: "main".to_owned(),
                    oid: None,
                },
                head: PullRequestIdentity {
                    repository: Some("fork/project".to_owned()),
                    branch: "feature".to_owned(),
                    oid: Some("98c549d29609856a5f29851fbea2a0a5607c65e0".to_owned()),
                },
                checks: CheckRollup::Failure,
            },
            checks: vec![
                PullRequestCheck {
                    name: "build".to_owned(),
                    state: CheckState::Error,
                    target_url: Some("https://checks/build".to_owned()),
                    required: true,
                    source_order: 0,
                    completed_at: None,
                },
                PullRequestCheck {
                    name: "lint".to_owned(),
                    state: CheckState::Failure,
                    target_url: None,
                    required: false,
                    source_order: 1,
                    completed_at: None,
                },
            ],
            feedback: vec![
                PullRequestFeedback {
                    id: "IC_node".to_owned(),
                    database_id: Some(91),
                    thread_id: Some("PRRT_thread".to_owned()),
                    kind: FeedbackKind::InlineThread,
                    author: "reviewer".to_owned(),
                    body: concat!(
                        "<!-- review metadata -->\n",
                        "## Summary\n",
                        "**split** this line\n",
                        "- follow up"
                    )
                    .to_owned(),
                    path: Some("src/lib.rs".to_owned()),
                    permalink: Some("https://git.example.com/comment/91".to_owned()),
                    outdated: false,
                },
                PullRequestFeedback {
                    id: "PRR_node".to_owned(),
                    database_id: Some(92),
                    thread_id: None,
                    kind: FeedbackKind::ReviewSummary,
                    author: "lead".to_owned(),
                    body: "Please add coverage".to_owned(),
                    path: None,
                    permalink: None,
                    outdated: true,
                },
            ],
            worktree: Some(PromptWorktree {
                path: PathBuf::from("/worktrees/feature"),
                branch: Some("refs/heads/feature".to_owned()),
                head: Some("98c549d29609856a5f29851fbea2a0a5607c65e0".to_owned()),
            }),
        }
    }

    #[test]
    fn formats_checks_comments_reviews_hosts_forks_and_missing_fields_exactly() {
        let actual = format_agent_prompt(&[pull_request()]).unwrap();
        assert_eq!(
            actual,
            "In feature (#42 Fix feedback):\n\nUse this existing checkout:\n```bash\ncd -- '/worktrees/feature'\n```\nBranch `feature` is checked out there. Local HEAD `98c549d2` matches the PR head.\n\nComment IDs:\n  - 91 - Summary • split this line • follow up\n\nPlease use the following command to investigate each review comment.\n```\ngh api --hostname git.example.com repos/base/project/pulls/comments/$comment_id --jq '{id,path,line,body,created_at,updated_at}'\n```\n\nReview IDs:\n  - 92 - Please add coverage\n\nPlease use the following command to investigate each review summary and respond as appropriate.\n```\ngh api --hostname git.example.com repos/base/project/pulls/42/reviews/$review_id --jq '{id,body,state,submitted_at}'\n```\n\nChecks (all failed):\n  - build (https://checks/build)\n  - lint (https://git.example.com/base/project/pull/42)\n\nFix, reply to the comments, and mark as resolved as appropriate."
        );
        assert!(!actual.contains("https://git.example.com/comment/91"));
        assert!(!actual.contains("not merge-required"));
    }

    #[test]
    fn empty_scopes_do_not_produce_a_prompt() {
        let mut pull_request = pull_request();
        pull_request.checks.clear();
        pull_request.feedback.clear();
        assert_eq!(format_agent_prompt(&[pull_request]), None);
        assert_eq!(format_agent_prompt(&[]), None);
    }

    #[test]
    fn reports_stale_and_missing_worktree_state_without_guessing() {
        let mut stale = pull_request();
        stale.worktree.as_mut().unwrap().head =
            Some("0123456789abcdef0123456789abcdef01234567".to_owned());
        let stale_prompt = format_agent_prompt(&[stale]).unwrap();
        assert!(stale_prompt.contains(
            "Local HEAD `01234567` does not match PR head `98c549d2`; \
             synchronize it before addressing this feedback."
        ));

        let mut missing = pull_request();
        missing.worktree = None;
        let missing_prompt = format_agent_prompt(&[missing]).unwrap();
        assert!(missing_prompt.contains("No existing worktree is mapped to this pull request."));
        assert!(!missing_prompt.contains("Use a worktree if"));
    }

    #[test]
    fn checkout_command_shell_quotes_the_worktree_path() {
        let mut pull_request = pull_request();
        pull_request.worktree.as_mut().unwrap().path = PathBuf::from("/work trees/reviewer's");

        let prompt = format_agent_prompt(&[pull_request]).unwrap();
        assert!(prompt.contains("cd -- '/work trees/reviewer'\\''s'"));
    }

    #[test]
    fn unidentified_comments_use_their_best_reference_and_rollup_excerpt() {
        let mut pull_request = pull_request();
        pull_request.checks.clear();
        pull_request.feedback = vec![
            PullRequestFeedback {
                id: "node-with-link".to_owned(),
                database_id: None,
                thread_id: None,
                kind: FeedbackKind::InlineThread,
                author: "reviewer".to_owned(),
                body: format!("\n  {} trailing", "x".repeat(101)),
                path: None,
                permalink: Some("https://git.example.com/comment/fallback".to_owned()),
                outdated: false,
            },
            PullRequestFeedback {
                id: "node-only".to_owned(),
                database_id: None,
                thread_id: None,
                kind: FeedbackKind::ReviewSummary,
                author: "lead".to_owned(),
                body: "  review\n summary  ".to_owned(),
                path: None,
                permalink: None,
                outdated: false,
            },
        ];

        let actual = format_agent_prompt(&[pull_request]).unwrap();
        assert!(actual.contains(&format!(
            "Comments:\n  - https://git.example.com/comment/fallback - {}\n",
            "x".repeat(100)
        )));
        assert!(actual.contains("  - node-only - review • summary"));
        assert!(!actual.contains("Comment IDs:"));
        assert!(!actual.contains("Review IDs:"));
    }

    #[test]
    fn review_requests_strip_conventional_prefixes_and_mark_drafts_exactly() {
        assert_eq!(format_review_request(&[]), None);

        let mut first = pull_request();
        first.pull_request.title = "feat: add widget".to_owned();
        first.pull_request.url = "https://example.test/pull/1".to_owned();
        let mut second = pull_request();
        second.pull_request.title = "Fix(ui)!: crash: now".to_owned();
        second.pull_request.url = "https://example.test/pull/2".to_owned();
        second.pull_request.state = PullRequestState::Draft;

        assert_eq!(
            format_review_request(&[first, second]).as_deref(),
            Some(
                "https://example.test/pull/1 - add widget\n\
                 https://example.test/pull/2 - crash: now - DRAFT"
            )
        );
    }

    #[test]
    fn conventional_prefix_stripping_rejects_unknown_or_malformed_prefixes() {
        for (title, expected) in [
            ("chore!: drop", "drop"),
            ("chore(deps)!: bump", "bump"),
            ("Refactor: X", "X"),
            ("WIP: x", "WIP: x"),
            ("update: x", "update: x"),
            ("fix(scope: broken", "fix(scope: broken"),
            ("feat:", "feat:"),
            ("no colon here", "no colon here"),
        ] {
            assert_eq!(strip_conventional_commit_prefix(title), expected);
        }
    }
}
