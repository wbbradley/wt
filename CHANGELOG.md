# Changelog

## [0.2.3] - 2026-08-18

### Added

- Add a release-mode regression benchmark and profiling guide for keeping cursor navigation and redraw latency below one 60 Hz frame on large trees.

### Changed

- Improve cursor navigation and redraw performance by reusing visible-row snapshots, rendering only the viewport, computing tree prefixes in a linear pass, and caching current-worktree path resolution outside hot paths.

### Fixed

- Prevent periodic background status refreshes from flashing loading states or redrawing when visible status and progress are unchanged.
- Preserve selection visibility, scrolling, tree connectors, current-worktree markers, and selected-row actions with optimized viewport rendering.

## [0.2.2] - 2026-08-16

### Added

- Show repository filesystem paths alongside labels in the worktree tree and include them in filtering.
- Discover open pull requests assigned to the authenticated user in addition to authored pull requests.

### Changed

- Fetch authored and assigned pull-request searches concurrently, with independent pagination and failure reporting.

### Fixed

- Keep local worktrees under mapped or bare repositories visible and recoverable after backburnering their pull requests.
- Correct selector syntax formatting in generated CLI and Rust documentation.

## [0.2.1] - 2026-08-15

## [0.2.0] - 2026-08-14

### Breaking Changes

- Move a backburnered branch together with its complete represented local and virtual subtree, including descendants discovered after the Backburner state was saved. Un-backburner the stack root and reapply Backburner at the desired branch boundary to keep a local branch in the normal tree.

### Added

- Refresh local catalog, worktree, ancestry, and status data every 60 seconds without generating GitHub traffic.
- Include the mapped checkout path, checked-out branch, and local-versus-pull-request HEAD status in copied agent prompts.

### Changed

- Include complete represented subtrees in explicit Backburner copy and review-request scopes.
- Run manual refresh as an ordered local snapshot followed by a GitHub refresh.
- Bind cached branch enrichment to repository identity, worktree path, and full branch ref, discarding obsolete local and remote results.

### Fixed

- Map authored pull requests by stable repository paths so catalog reloads, reordered repositories, and session-only rows cannot attach pull requests to the wrong repository.
- Revalidate repository mappings when materializing authored pull requests.
- Hide disclosure markers and make expansion a no-op for leaf branches.
- Keep Backburner tree connectors visible while dimming branch content.
- Place the review-comment resolution instruction once at the end of copied prompts.

## [0.1.4] - 2026-08-13

### Added

- Show context-sensitive `Enter` and `w` action hints for the selected repository, worktree, pull request, section, check, or comment.

### Changed

- Use a shorter list heading and describe horizontal navigation as folding.

### Fixed

- Open a pull request's Checks page when activating its Checks section with `Enter` or `w`, instead of opening the pull request overview.

## [0.1.3] - 2026-08-13

### Added

- Add Zsh navigation and local tab completion through `wt shell-init zsh`.

### Fixed

- Preserve authored pull-request stacks when worktrees move, disappear, or point at commits shared by multiple pull requests.
- Select the pull request represented by a local branch using exact head-branch and head-commit matches before deterministic fallback selection.
- Prevent stale cache identities and partial GitHub refresh failures from hiding unrepresented authored pull requests.

## [0.1.2] - 2026-08-13

### Added

- Show the age of the latest successful GitHub refresh in the TUI header.

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
