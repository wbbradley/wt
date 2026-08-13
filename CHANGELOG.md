# Changelog

## [0.1.2] - 2026-08-13

### Added

- Show the age of the latest successful GitHub refresh in the TUI header.
- Add Zsh navigation and local tab completion through `wt shell-init zsh`.

### Changed

- Render clearer, continuous tree connectors across repositories and nested rows.
- Preserve priority and local-status details when truncating long branch labels.

### Fixed

- Label copied actionable check selections as `Checks (all failed)` to make their status explicit.
- Describe `]` accurately as the next-issue shortcut in the footer; `[` still navigates to the previous issue.

## [0.1.1] - 2026-08-12

### Added

- Manage registered repositories and their Git worktrees from a global terminal UI or scriptable `repo`, `config`, and `worktree` commands.
- Create, inspect, move, lock, repair, prune, safely remove, and explicitly force-remove worktrees with previews and guarded confirmations.
- Navigate directly to worktrees with repository-qualified selectors and Bash shell integration with local tab completion.
- Enrich local branches with GitHub and GitHub Enterprise pull-request status, checks, reviews, unresolved comments, conflicts, and rate-limit diagnostics.
- Discover authored pull requests, display stacked branch relationships, and safely materialize virtual pull requests into repositories and linked worktrees.
- Search the full tree with regular expressions, navigate matches, preserve disclosure state, and copy scoped agent prompts or review-request links.
- Persist Backburner membership and cache GitHub snapshots for fast, resilient startup and background refreshes.
- Remove clean linked worktrees whose exact pull-request heads are confirmed merged through live GitHub checks.

### Changed

- Present repositories, worktrees, pull requests, checks, reviewers, comments, and stacked branches in a compact inline tree with actionable status summaries.
- Load local data immediately and perform status and GitHub work asynchronously, skipping unnecessary pull-request refreshes for trunk branches.

### Fixed

- Keep merged, stale, and required-check statuses accurate, including hiding misleading pull-request state on trunk branches.
- Preserve search highlights and fold state while limiting matches to visible row text and sanitizing copied review content.
- Prefer canonical GitHub base relationships when local and remote stack metadata disagree while ensuring each branch is rendered once.
- Handle invalid catalog paths, unrecognized Git status headers, resolved review comments, bare repositories, and configured worktree-root creation safely.
