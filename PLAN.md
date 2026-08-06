# Next Up

## Catalog settings and mutation locking for authored PRs

Establish the configuration and concurrency foundation needed to discover and materialize authored pull requests safely.

- Extend `Catalog` in `src/model.rs` and persistence in `src/config.rs` with backward-compatible optional data:
  - `repository_root`, preserving the configured expression and defaulting at runtime to `~/src`.
  - `github_hosts`, with a runtime view that is always unioned with `github.com` and can include hosts inferred from tracked remotes.
  - Expand only a leading `~`, `$VAR`, or `${VAR}` at runtime—never shell syntax, command substitution, backticks, or globbing. Undefined variables are configuration errors. Require an absolute expanded path, allow canonicalized directory symlinks, canonicalize the longest existing prefix, create the root recursively on first use, and reject non-directories or unwritable targets.
- Add `wt config set repository-root <expression>` and `wt config show` in `src/cli.rs`. Preserve the expression exactly in JSON while showing both configured and resolved values. Update completion and CLI tests.
- Add an advisory sidecar lock at `<catalog-path>.lock`; do not lock `wt.json` itself because saves atomically replace its inode. Provide a cancellable retry API for later TUI progress. All existing CLI and TUI catalog mutations must acquire the sidecar lock and reload the catalog before saving so concurrent mutations cannot overwrite one another.
- Add focused coverage for config expansion/rejection and symlinks, root validation/creation, lock serialization with atomic saves, exact CLI output/persistence, completion, and stale-writer prevention.

### Implementation plan

- Modify `Cargo.toml`/`Cargo.lock` to use a cross-platform advisory file-lock implementation.
- Modify `src/model.rs` with optional serialized settings, runtime defaults, and deterministic host union helpers.
- Modify `src/config.rs` with strict leading-expression expansion, longest-existing-prefix canonicalization, recursive root creation and writability checks, sidecar-path construction, and cancellable exclusive lock acquisition.
- Modify `src/cli.rs` to add `config show` and `config set repository-root`, complete their command words, and acquire/reload under the lock for every CLI catalog mutation.
- Modify `src/tui.rs` so repository registration, editing, and removal lock and reload before applying their mutation.
- Extend `tests/repo_cli.rs`, unit tests, and Bash completion coverage for the new public behavior and concurrency invariants.

Risks: catalog writers currently mutate an in-memory snapshot, so merely locking `save` would still lose updates; lock scope must include the reload and mutation. Root writability needs an actual create probe rather than permission-bit guesses so symlinks and platform ACLs behave correctly.

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

## GitHub authored-PR discovery and remote identity cache

- Extend `RepositoryConfig` with a backward-compatible derived map from every Git remote name to canonical `(host, owner, repository)` identities, plus the preferred remote. Refresh this cache opportunistically; warn rather than silently rewriting conflicting identities.
- Infer configured GitHub hosts from every tracked remote and union them with explicit hosts and `github.com`.
- Extend `src/github.rs` to discover, per configured/inferred host, the authenticated viewer and all open PRs authored by that viewer only—include drafts, exclude review-requested and merely team-authored PRs. Paginate in the background through GitHub Search's practical 1,000-result ceiling, surface truncation and later-page warnings, and coalesce this work with startup, manual, scheduled, and post-mutation GitHub refreshes.
- Render local worktrees immediately and publish authored-PR pages progressively. Keep the last fully successful authored-PR snapshot while loading or after any page/host failure; only remove disappeared PRs after a complete successful refresh. Preserve existing rate-limit suppression, classified errors, direct HTTP/token handling, and stale-data semantics.
- Canonically identify a PR as `(host, base owner/repository, PR number)`. Suppress it only when GitHub associated-PR data ties an active local worktree to that identity; group fork PRs under their base repository.
- Map against every GitHub remote: prefer configured `github_remote`, then `origin`, then catalog order; display once, and treat missing/invalid catalog paths as unmapped.
- Cover viewer filtering, pagination/truncation, Enterprise hosts, progressive/stale snapshots, canonical suppression, all-remotes mapping, duplicate preference, and fork/base grouping.

## Virtual authored-PR repository and row models

- Extend `src/app.rs`, `src/ui.rs`, and `src/tui.rs` with stable canonical IDs and explicit virtual-repository/virtual-PR models.
- Preserve selection and expansion as pages arrive or refresh reorder occurs; on completed removal, select the repository header or nearest row.
- Preserve catalog order for mapped repositories; append unmapped base repositories sorted by `owner/repository`; sort their virtual PRs by newest `updated_at`.
- Label groups `owner/repository`; show a grey `[no local repo]` marker for unusable repositories.
- Render `#<number> <head-branch> — <title> [virtual] [checks]`; show complete base/head/SHA/draft/check/review/URL detail and `Enter to create worktree`.
- Filter on repository, branch, PR number, title, and author. Disable every palette/direct action on virtual rows with a reason; Enter is the sole materialization gesture with no confirmation.
- Cover stable selection/order/filtering, progressive updates, `[no local repo]`, detail rendering, and action disabling.

## Safe repository bootstrap for virtual PRs

- Re-fetch selected PR metadata and head SHA. Allow checkout after close/merge while displaying state; abort only when the PR/repository is gone or inaccessible.
- Reuse a usable mapped repository. Otherwise resolve `repository_root` and try `<repo>.git`, `<owner>-<repo>.git`, then `<host>-<owner>-<repo>.git`, reusing only canonical identity matches and never adopting, overwriting, or deleting unrelated paths.
- Clone the base as a bare partial clone with `--filter=blob:none`, falling back to normal bare clone when unsupported.
- Prefer SSH, then authenticated noninteractive HTTPS without exposing tokens in arguments, logs, progress, or errors; disable Git/SSH prompts.
- Register the validated bare repository with a unique label. When relinking a stale matching entry, preserve label, `worktree_root`, and preferred remote and never touch the stale path.
- Keep validated/registered repositories after later failures; clean only clearly marked incomplete clone artifacts.
- Cover name collisions, stale relinking, partial-clone fallback, transport credential safety, and failure-stage retention/cleanup.

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
