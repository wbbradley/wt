# Next Up

## Safe PR branch and linked-worktree materialization

- If a local remote represents the head repository, fetch and track the real head branch, including forks. Otherwise fetch GitHub's PR head ref into `pr/<number>-<sanitized-head-branch>` without permanently adding the fork remote.
- Record canonical PR identity in branch-local Git config. Fast-forward an unattached intended branch only when safe; preserve ahead/diverged/other-PR branches and choose a disambiguated name. Never discard local commits or reset solely because of a PR marker.
- Create through shared operation helpers: use `<worktree_root>/<sanitized-local-branch>` when configured, otherwise `<repository_root>/<repo>-pr-<number>`. Fail if the destination belongs to something else; never add numeric path suffixes.
- Refresh success into an ordinary row and return its canonical path through the existing stdout/Bash `chdir` protocol.
- Cover real versus synthetic branches, divergence preservation, PR markers, destination rules, and exact shell selection.

## Background materialization, cancellation, and documentation

- Run lock wait, clone, fetch, branch, and worktree creation in a background worker with cancellable progress states. `Ctrl-c` must terminate the active Git child, clean only owned incomplete artifacts, retain completed safe stages, leave the shell directory unchanged, and restore the TUI.
- Hold the catalog sidecar lock from bootstrap through registration and worktree creation and show `waiting for catalog lock` while retrying.
- Do not auto-remove bootstrapped repositories/worktrees when PRs close. Do not make direct selectors such as `wt owner/repo:#123` materialize PRs.
- Update `README.md` with authored-PR discovery, host configuration, virtual rows, root expansion, bootstrap naming/transport, locking, branch safety, persistence, and cancellation.
- Cover child cancellation and failure cleanup, then run formatting, lint, the full test suite, and Bash syntax checks.

Complete the authored-PR feature when every open authored PR appears once under its base repository, Enter safely creates or reuses a persistent repository and ordinary linked worktree, unmapped repositories bootstrap under the configured root without destructive behavior, and all project checks pass.
