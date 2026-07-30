# Next Up

## Safe worktree operation service and scriptable CRUD

Build reusable local operation services and `wt worktree ...` commands shared by the future TUI.

- Create from an unattached branch, a new branch with editable start point, or a detached commit-ish; suggest configured-root or sibling destinations and validate refs, branches, parents, collisions, and already-checked-out branches. Confirm the repository, destination, branch/start point, and any parent-directory creation.
- Inspect full worktree details and status using `git status --porcelain=v2 -z`, including HEAD, branch/upstream, lock reason, dirty/staged/untracked state, anchor, detached, prunable, stale, and bare states.
- Move, lock with optional reason, unlock after confirmation, and repair administrative links using safe Git argument arrays and exact-path previews.
- Remove without deleting branches, showing the repository, branch/detached commit, and absolute path before confirmation. Refuse normal removal of bare anchors, main worktrees, the checkout containing current PWD, locked worktrees, and dirty worktrees.
- If force removal is exposed, make it distinct, repeat the dirty summary, and require typed branch/full-path confirmation; never apply `--force` implicitly.
- Preview `git worktree prune --dry-run --verbose` and require parity-preserving confirmation before pruning; treat races and Git refusals as recoverable refresh conditions.
- Cover normal and bare repositories, every creation mode and conflict, move/lock/unlock/remove/repair/prune, safeguards, branch preservation, and prune-preview parity with temporary real repositories.

## Local-first global TUI and terminal lifecycle

Implement the interactive catalog/worktree browser and all local action workflows.

- Group rows by repository and show label/anchor, current-PWD marker, path, branch/detached identity, short HEAD, lock/prunable state, and asynchronously populated local status.
- Select the containing worktree initially, preserve identity across refreshes/mutations, and support collapse/expand, cross-field filtering, resize, scrolling, a detail pane, progress, and actionable inline errors.
- Implement Vim-oriented navigation (`j`/`k`, arrows, contextual `h`/`l`, `g`/`G`, `Ctrl-d`/`Ctrl-u`, `/`, `r`, `Enter`, `Esc`) plus an action palette and documented CRUD/catalog shortcuts with correct availability for headers, bare anchors, and stale records.
- Draw local catalog/worktree data before slow status work. Use bounded workers and generation-tagged channels so stale background updates cannot overwrite current state; coalesce refreshes.
- When launched inside an unregistered repository, show it as session-only with explicit registration; show actionable onboarding for an empty catalog.
- Render on the controlling terminal or stderr, reserve stdout for an accepted absolute worktree-root path and newline, print nothing on cancellation, and restore raw mode/cursor/alternate screen after success, cancellation, error, Ctrl-C, and panic.
- Send errors to stderr with nonzero status and use `ratatui`/`crossterm` with a guaranteed-restoration terminal layer.
- Test navigation/filter/collapse, empty/session-only/stale states, action availability and transitions, selection preservation, refresh coalescing/generation rejection, resizing, output protocol, and terminal restoration.

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
