# Next Up

## Background materialization, cancellation, and documentation

- Run lock wait, clone, fetch, branch, and worktree creation in a background worker with cancellable progress states. `Ctrl-c` must terminate the active Git child, clean only owned incomplete artifacts, retain completed safe stages, leave the shell directory unchanged, and restore the TUI.
- Hold the catalog sidecar lock from bootstrap through registration and worktree creation and show `waiting for catalog lock` while retrying.
- Do not auto-remove bootstrapped repositories/worktrees when PRs close. Do not make direct selectors such as `wt owner/repo:#123` materialize PRs.
- Update `README.md` with authored-PR discovery, host configuration, virtual rows, root expansion, bootstrap naming/transport, locking, branch safety, persistence, and cancellation.
- Cover child cancellation and failure cleanup, then run formatting, lint, the full test suite, and Bash syntax checks.

Complete the authored-PR feature when every open authored PR appears once under its base repository, Enter safely creates or reuses a persistent repository and ordinary linked worktree, unmapped repositories bootstrap under the configured root without destructive behavior, and all project checks pass.

### Implementation plan

- Extend `src/background.rs` with a single cancellable materialization job, progress/result messages, and a process runner shared across `GitRunner`, `CloneRunner`, and `FetchRunner`. Spawn Git with piped readers, poll cancellation, kill and reap the active child, redact transport errors through the existing request boundaries, and join workers on completion/drop.
- Refactor `src/tui.rs` so Enter snapshots the canonical PR/mapping input and starts the worker; the worker authoritatively refreshes metadata, resolves credentials, waits cancellably for the catalog sidecar lock, reloads under the lock, bootstraps and immediately persists a validated repository, then fetches/creates the safe branch and worktree without releasing the lock. Pump progress/results on the UI thread, refresh ordinary rows on success, and make Ctrl-C cancel an active job and restore interaction without returning a shell selection.
- Extend `src/materialize.rs` with owned incomplete-worktree cleanup around the exact destination. On a failed or cancelled `git worktree add`, clean only the destination proven absent before this operation and its stale administrative record; retain successfully completed worktrees, registered repositories, fetched refs, and safe branches.
- Add worker/process/controller tests for non-blocking startup, lock-wait progress and cancellation, active child termination, no selection on cancellation, incomplete clone/worktree cleanup, and retention of completed repository/branch stages. Add a navigation regression proving direct `owner/repo:#number` selectors do not invoke materialization.
- Update `README.md` with authored-PR discovery and host configuration, virtual-row interaction, root expansion, deterministic bootstrap naming and SSH/HTTPS behavior, catalog locking, real/synthetic branch safety and markers, persistence after closure/failure, exact destinations, and cancellation semantics.

Risks: a child can finish concurrently with cancellation, so process status must be checked before honoring the cancellation flag; a completed successful stage must never be rolled back. Captured child pipes must be drained concurrently to avoid deadlock during verbose clones. Worker completion and cancellation race with terminal events, so only a successful result may produce `ControlFlow::Exit(Some(path))`; cancelled/stale results must remain in the TUI with empty stdout.

## Post-Plan Execution Steps

Execute these steps in order:

### Implement
Execute the plan above.

**Naming gate:** before creating any file, identifier, run-id, or env var, ask "would this name
make sense to someone who never read the plan?" If it encodes a sequence position (`Stage N` /
`Phase N` / `stepN`), rename it now — cheap before a checkpoint or downstream reference pins it.

### Verify

1. Run the project's build/lint command. Fix all warnings.
2. Run the project's test suite.
3. If tests fail, fix them before proceeding.
4. If test coverage for the new work is insufficient, add tests.

### Commit

Use Conventional Commits commit message style. If there are pre-existing modified files and they don't look harmful, go ahead and commit them, too.

### Update PLAN.md

Read `PLAN.md`. **Remove** the completed task entirely from the "Next Up" section — do not leave it in place with a [DONE] tag, strikethrough, or any other marker. The task and its related subsections should no longer appear in PLAN.md at all. PLAN.md should not have any sort of "Done" section. Then append a new entry to `COMPLETED.md` with two parts, in this order:

1. A brief summary, written now, of what was actually implemented.
2. The full text of the PLAN.md entry as it existed before work began, verbatim, not paraphrased, to preserve the original.

If upcoming PLAN.md items need modifications due to a change during this implementation then update those. If new future work items were discovered, add them. If PLAN.md or COMPLETED.md are ignored, don't force add them, otherwise commit them with other changes.
