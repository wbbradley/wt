# Next Up

## Direct GitHub API enrichment

Add non-blocking GitHub pull-request enrichment using direct HTTP, following the transport/error patterns in `../git-stack/src/github.rs` and refresh/partial-result patterns in `../rollup`.

- Parse SSH/HTTPS remotes for github.com and GitHub Enterprise. Prefer each branch's upstream remote, then catalog `github_remote`, then `origin`.
- Resolve host-appropriate tokens from environment, repository-scoped Git config, or `gh auth token --hostname`; never serialize, log, or display tokens and never use `gh api` for data.
- Use a reusable timed `ureq::Agent` with explicit Bearer, Accept, and `User-Agent: wt` headers, preserving non-2xx bodies for classification.
- Batch branches per repository/host in bounded GraphQL requests using variables. Fetch PR number/title/URL/base/head, draft/open/merged/closed state, update time, review decision, and latest-commit check rollup; prefer open/draft, then most recently updated associated PR.
- Accept partial GraphQL data with deduplicated warnings and treat errors-only responses as failures. Classify auth, permission, SSO/SAML, classic-PAT, rate-limit, network, malformed-response, and unsupported-remote failures.
- Track and display remaining/reset metadata, suppress retries through exhausted resets, enforce a 30-second minimum on the configurable 300-second default refresh, coalesce overlaps, and retain visibly stale prior data after failure.
- Integrate async PR summaries/details/filter text without ever blocking local navigation or CRUD.
- Test with a local fake server: remote/Enterprise parsing, precedence, redaction, batching/variables, fork PRs, normalization/preference, partial data, classified failures, rate suppression, and stale retention.

## Bash navigation, selectors, and completion

Complete shell-facing navigation and local-only discovery ergonomics.

- Add `shell/wt.bash` with a Bash 3.2-compatible `wt` function: navigation captures `command wt` stdout and calls `builtin cd -- "$destination"` only after successful nonempty selection; catalog/worktree/help/version commands pass through unchanged.
- Ensure accepted selections always navigate to the worktree root and that cancellation/failure leaves PWD unchanged, including paths containing spaces.
- Add `wt <repo-label>:<branch-or-worktree>` unique resolution by branch/basename/path, opening the TUI prefiltered when ambiguous rather than guessing.
- Add Bash 3.2-compatible completion for flags, subcommands, labels, qualified selectors, branches, and paths through a local-only endpoint that preserves spaces, avoids GitHub, and never opens the TUI.
- Shell-test navigation, exact roots, spaces, cancellation/failure preservation, passthrough, selectors, ambiguity handling, and completion.

## Documentation, integration hardening, and completion audit

Finish product documentation and verify the complete normal/bare/global workflow end to end.

- Document installation, registration, unregistered-session onboarding, bare repositories, CRUD safety and confirmations, keys/action palette, shell setup/completion, selectors, config, GitHub/GHE authentication, rate/error behavior, and troubleshooting in `README.md`.
- Verify repository-level error isolation, progressive local/GitHub rendering, action confirmations, selection output, and terminal cleanup across integrated flows.
- Ensure `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`, and `cargo test` pass.
- Manually verify normal and bare workflows, progressive GitHub updates, CRUD confirmations and safeguards, terminal restoration, cancellation/failure behavior, and sourced Bash navigation.
- Audit every requirement preserved across the preceding phases and add/fix any missing coverage before declaring the product complete.
