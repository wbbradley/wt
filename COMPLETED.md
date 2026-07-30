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
