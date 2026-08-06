# Next Up

## Safe PR branch and linked-worktree materialization

- If a local remote represents the head repository, fetch and track the real head branch, including forks. Otherwise fetch GitHub's PR head ref into `pr/<number>-<sanitized-head-branch>` without permanently adding the fork remote.
- Record canonical PR identity in branch-local Git config. Fast-forward an unattached intended branch only when safe; preserve ahead/diverged/other-PR branches and choose a disambiguated name. Never discard local commits or reset solely because of a PR marker.
- Create through shared operation helpers: use `<worktree_root>/<sanitized-local-branch>` when configured, otherwise `<repository_root>/<repo>-pr-<number>`. Fail if the destination belongs to something else; never add numeric path suffixes.
- Refresh success into an ordinary row and return its canonical path through the existing stdout/Bash `chdir` protocol.
- Cover real versus synthetic branches, divergence preservation, PR markers, destination rules, and exact shell selection.

### Implementation plan

- Create `src/materialize.rs` with a PR materialization service that:
  - selects a configured local remote matching the head repository and fetches its real branch, or fetches the base repository's `refs/pull/<number>/head` into a private ref without adding a fork remote;
  - validates the fetched commit against the authoritatively refreshed head SHA;
  - records the canonical base-repository PR identity in branch-local Git config and sets a real remote branch as upstream when applicable;
  - creates or safely fast-forwards only an unattached branch whose prior tip is an ancestor, while preserving checked-out, ahead, diverged, and other-PR branches under deterministic disambiguated names;
  - reuses an existing exact same-PR worktree, otherwise creates through `operations::create` at the single required destination and returns its canonical path.
- Extend `src/operations.rs` with reusable public sanitization and PR-destination helpers, and treat every existing filesystem object including broken symlinks as an occupied destination.
- Modify `src/tui.rs` so Enter saves any successful bootstrap registration before later stages, invokes materialization with the refreshed PR, refreshes the local repository view into an ordinary row, and exits with the created/reused canonical path for the stdout/Bash selection protocol.
- Register the new module in `src/main.rs` and add real-repository tests for local fork-remote tracking, synthetic PR refs without remote creation, safe fast-forwarding, preservation/disambiguation for ahead/diverged/other-PR branches, canonical markers, exact configured/default destinations, collision refusal, worktree reuse, and controller-level exact selection.

Risks: branch names may contain Git-valid punctuation that is unsafe in refspecs or config keys, so all generated names and ref arguments must remain separate argument-array values and synthetic names must use shared sanitization. A branch marker proves intent but never authorizes discarding commits. Fetches can race a newly pushed PR head, so a fetched OID mismatch must fail safely rather than creating a worktree at an unverified commit.

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

## Background materialization, cancellation, and documentation

- Run lock wait, clone, fetch, branch, and worktree creation in a background worker with cancellable progress states. `Ctrl-c` must terminate the active Git child, clean only owned incomplete artifacts, retain completed safe stages, leave the shell directory unchanged, and restore the TUI.
- Hold the catalog sidecar lock from bootstrap through registration and worktree creation and show `waiting for catalog lock` while retrying.
- Do not auto-remove bootstrapped repositories/worktrees when PRs close. Do not make direct selectors such as `wt owner/repo:#123` materialize PRs.
- Update `README.md` with authored-PR discovery, host configuration, virtual rows, root expansion, bootstrap naming/transport, locking, branch safety, persistence, and cancellation.
- Cover child cancellation and failure cleanup, then run formatting, lint, the full test suite, and Bash syntax checks.

Complete the authored-PR feature when every open authored PR appears once under its base repository, Enter safely creates or reuses a persistent repository and ordinary linked worktree, unmapped repositories bootstrap under the configured root without destructive behavior, and all project checks pass.
