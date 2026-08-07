use crate::model::{
    CanonicalPullRequestId, FeedbackKind, PullRequest, PullRequestCheck, PullRequestFeedback,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PromptPullRequest {
    pub identity: CanonicalPullRequestId,
    pub pull_request: PullRequest,
    pub checks: Vec<PullRequestCheck>,
    pub feedback: Vec<PullRequestFeedback>,
}

pub fn format_agent_prompt(pull_requests: &[PromptPullRequest]) -> Option<String> {
    if pull_requests
        .iter()
        .all(|pull_request| pull_request.checks.is_empty() && pull_request.feedback.is_empty())
    {
        return None;
    }

    let mut output = String::from(
        "Address the following pull request feedback and failing checks. Preserve unrelated changes, use the stored GitHub IDs when investigating, and verify each fix.\n",
    );
    for pull_request in pull_requests {
        if pull_request.checks.is_empty() && pull_request.feedback.is_empty() {
            continue;
        }
        let repository = &pull_request.identity.repository;
        output.push_str(&format!(
            "\n## {} — PR #{}: {}\nRepository: {}/{} ({})\nPR: {}\n",
            pull_request.pull_request.head.branch,
            pull_request.identity.number,
            pull_request.pull_request.title,
            repository.owner,
            repository.repository,
            repository.host,
            pull_request.pull_request.url,
        ));
        if !pull_request.checks.is_empty() {
            output.push_str("\n### Failing checks\n");
            for check in &pull_request.checks {
                let requirement = if check.required {
                    "required"
                } else {
                    "optional; not merge-required"
                };
                output.push_str(&format!(
                    "- {} [{}; {:?}]\n  URL: {}\n",
                    check.name,
                    requirement,
                    check.state,
                    check.target_url.as_deref().unwrap_or("unavailable"),
                ));
            }
        }
        if !pull_request.feedback.is_empty() {
            output.push_str("\n### Feedback\n");
            for feedback in &pull_request.feedback {
                let kind = match feedback.kind {
                    FeedbackKind::InlineThread => "inline comment",
                    FeedbackKind::ReviewSummary => "review summary",
                };
                let path = feedback.path.as_deref().unwrap_or("no path");
                let database_id = feedback
                    .database_id
                    .map(|id| id.to_string())
                    .unwrap_or_else(|| "unavailable".to_owned());
                let thread_id = feedback.thread_id.as_deref().unwrap_or("unavailable");
                output.push_str(&format!(
                    "- {kind} by {} at {path} [node ID: {}; database ID: {database_id}; thread ID: {thread_id}]{}\n  Body: {}\n  URL: {}\n",
                    feedback.author,
                    feedback.id,
                    if feedback.outdated { " [outdated]" } else { "" },
                    excerpt(&feedback.body),
                    feedback.permalink.as_deref().unwrap_or("unavailable"),
                ));
                if let Some(database_id) = feedback.database_id {
                    let endpoint = match feedback.kind {
                        FeedbackKind::InlineThread => format!(
                            "repos/{}/{}/pulls/comments/{database_id}",
                            repository.owner, repository.repository
                        ),
                        FeedbackKind::ReviewSummary => format!(
                            "repos/{}/{}/pulls/{}/reviews/{database_id}",
                            repository.owner, repository.repository, pull_request.identity.number
                        ),
                    };
                    output.push_str(&format!(
                        "  Investigate: gh api --hostname {} {endpoint}\n",
                        repository.host
                    ));
                } else {
                    output.push_str(&format!(
                        "  Investigate using stored node ID {} on {}.\n",
                        feedback.id, repository.host
                    ));
                }
            }
        }
    }
    Some(output)
}

fn excerpt(body: &str) -> String {
    let normalized = body.split_whitespace().collect::<Vec<_>>().join(" ");
    const LIMIT: usize = 180;
    if normalized.chars().count() <= LIMIT {
        return normalized;
    }
    let mut excerpt = normalized.chars().take(LIMIT - 1).collect::<String>();
    excerpt.push('…');
    excerpt
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
                    oid: None,
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
                    body: "  split   this line  ".to_owned(),
                    path: Some("src/lib.rs".to_owned()),
                    permalink: Some("https://git.example.com/comment/91".to_owned()),
                    outdated: false,
                },
                PullRequestFeedback {
                    id: "PRR_node".to_owned(),
                    database_id: None,
                    thread_id: None,
                    kind: FeedbackKind::ReviewSummary,
                    author: "lead".to_owned(),
                    body: "Please add coverage".to_owned(),
                    path: None,
                    permalink: None,
                    outdated: true,
                },
            ],
        }
    }

    #[test]
    fn formats_checks_comments_reviews_hosts_forks_and_missing_fields_exactly() {
        let actual = format_agent_prompt(&[pull_request()]).unwrap();
        assert_eq!(
            actual,
            "Address the following pull request feedback and failing checks. Preserve unrelated changes, use the stored GitHub IDs when investigating, and verify each fix.\n\n## feature — PR #42: Fix feedback\nRepository: base/project (git.example.com)\nPR: https://git.example.com/base/project/pull/42\n\n### Failing checks\n- build [required; Error]\n  URL: https://checks/build\n- lint [optional; not merge-required; Failure]\n  URL: unavailable\n\n### Feedback\n- inline comment by reviewer at src/lib.rs [node ID: IC_node; database ID: 91; thread ID: PRRT_thread]\n  Body: split this line\n  URL: https://git.example.com/comment/91\n  Investigate: gh api --hostname git.example.com repos/base/project/pulls/comments/91\n- review summary by lead at no path [node ID: PRR_node; database ID: unavailable; thread ID: unavailable] [outdated]\n  Body: Please add coverage\n  URL: unavailable\n  Investigate using stored node ID PRR_node on git.example.com.\n"
        );
    }

    #[test]
    fn empty_scopes_do_not_produce_a_prompt() {
        let mut pull_request = pull_request();
        pull_request.checks.clear();
        pull_request.feedback.clear();
        assert_eq!(format_agent_prompt(&[pull_request]), None);
        assert_eq!(format_agent_prompt(&[]), None);
    }
}
