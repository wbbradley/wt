# Completed

## Persistent catalog and Git worktree discovery foundation

Implemented a versioned, atomically persisted repository catalog; normal/linked/bare anchor resolution and common-Git-directory deduplication; isolated NUL-porcelain worktree discovery; stale/relink behavior; and scriptable repo add/list/edit/remove commands. Added unit and real-Git integration tests covering configuration safety, worktree states and unusual paths, registration, metadata, deduplication, stale entries, relinking, and unregister-only removal.

## Persistent catalog and Git worktree discovery foundation

Establish the reusable local-data foundation for `wt`, including scriptable repository catalog management and robust discovery for normal, linked, bare, and stale repositories.

- Add the versioned `~/.config/wt.json` catalog with `WT_CONFIG_PATH`, optional labels/worktree roots/GitHub remotes, stable derived labels, missing-config defaults, future-version rejection, atomic parent-creating writes, and visibly retained stale entries.
- Resolve linked worktrees to their main-worktree or bare-repository anchor and deduplicate registrations by canonical common Git directory.
- Add `wt repo add [PATH]`, `wt repo list`, `wt repo edit <selector>`, and `wt repo remove <selector>` with label, worktree-root, and GitHub-remote options. Removal must only update the catalog.
- Put Git execution behind an injectable argument-array runner. Discover with `git -C <anchor> worktree list --porcelain -z`, preserving paths as `PathBuf` and explicitly modeling detached, locked, prunable, and bare records.
- Treat bare records as non-navigable repository headers and isolate discovery failures per repository.
- Add focused unit tests plus temporary real-repository tests for configuration versions/defaults, atomic writes, normal/linked/bare registration and deduplication, stale entries, and porcelain parsing including spaces, non-UTF-8 paths, malformed input, detached HEAD, locks, prunable records, and bare anchors.

## Safe worktree operation service and scriptable CRUD

Implemented reusable worktree status/detail and mutation services plus the complete scriptable `wt worktree` command family. Creation supports existing branches, new branches and start points, detached commits, validated suggestions, and explicit parent creation; update/removal flows provide previews, confirmations, main/current/bare/locked/dirty safeguards, distinct typed force removal, repair, and parity-preserving prune previews. Real normal and bare repository tests cover all modes, conflicts, moves, locks, repairs, safe and forced removal, branch preservation, and pruning.

## Safe worktree operation service and scriptable CRUD

Build reusable local operation services and `wt worktree ...` commands shared by the future TUI.

- Create from an unattached branch, a new branch with editable start point, or a detached commit-ish; suggest configured-root or sibling destinations and validate refs, branches, parents, collisions, and already-checked-out branches. Confirm the repository, destination, branch/start point, and any parent-directory creation.
- Inspect full worktree details and status using `git status --porcelain=v2 -z`, including HEAD, branch/upstream, lock reason, dirty/staged/untracked state, anchor, detached, prunable, stale, and bare states.
- Move, lock with optional reason, unlock after confirmation, and repair administrative links using safe Git argument arrays and exact-path previews.
- Remove without deleting branches, showing the repository, branch/detached commit, and absolute path before confirmation. Refuse normal removal of bare anchors, main worktrees, the checkout containing current PWD, locked worktrees, and dirty worktrees.
- If force removal is exposed, make it distinct, repeat the dirty summary, and require typed branch/full-path confirmation; never apply `--force` implicitly.
- Preview `git worktree prune --dry-run --verbose` and require parity-preserving confirmation before pruning; treat races and Git refusals as recoverable refresh conditions.
- Cover normal and bare repositories, every creation mode and conflict, move/lock/unlock/remove/repair/prune, safeguards, branch preservation, and prune-preview parity with temporary real repositories.
