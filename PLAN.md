# Next Up

## Canonical GitHub remote identities and authored-PR mapping

- Extend `RepositoryConfig` with a backward-compatible derived map from every Git remote name to canonical `(host, owner, repository)` identities, plus the preferred remote.
- Enumerate every remote and refresh the cache opportunistically. Add new identities and remove disappeared remotes, but retain a prior identity and surface a warning when the same remote name now resolves to a conflicting repository rather than silently rewriting it.
- Infer discovery hosts from all cached/tracked remotes and union them with explicit `github_hosts` and `github.com`.
- Introduce canonical PR identity `(host, base owner/repository, PR number)`. Derive it from GitHub associated-PR results for active worktrees rather than branch names or commits.
- Map each authored PR against every GitHub remote, displaying it once: prefer a catalog entry whose configured `github_remote` maps to the base repository, then one whose `origin` maps, then the earliest catalog entry. Treat missing or invalid catalog paths as unmapped even if their cache matches. Group fork PRs by the base identity.
- Integrate cache refresh/persistence with catalog refresh without blocking or overwriting concurrent catalog mutations, and surface reconciliation warnings.
- Cover all supported remote forms, cache additions/removals/conflicts/preference and backward compatibility, host inference, canonical active-worktree suppression, all-remotes mapping, duplicate preference, unusable paths, deduplication, and fork/base grouping.

### Implementation plan

- Modify `src/model.rs` with serialized canonical repository identities, cached per-remote identities/preference, and canonical PR IDs.
- Modify `src/github.rs` with injectable all-remote enumeration, conservative cache reconciliation, preferred-remote selection, host inference, canonical associated-PR extraction, suppression, and deterministic catalog mapping.
- Modify `src/tui.rs` to reconcile and persist registered repository caches through the sidecar lock/reload transaction before GitHub refreshes, while exposing conflict warnings without discarding local rows.
- Update all `RepositoryConfig` construction sites and add focused unit/controller/config compatibility tests.

Risks: remote names can be repointed, so overwriting an established canonical identity would silently remap PRs; conflicts must retain the established mapping until the user resolves it. Catalog cache persistence must reload under the sidecar lock to preserve unrelated concurrent edits.

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

## Viewer-authored PR discovery and progressive snapshots

- Extend `src/github.rs` to discover, per configured/inferred host, the authenticated viewer and every open PR authored by that viewer only—include drafts, exclude review-requested and merely team-authored PRs.
- Paginate in the background through GitHub Search's practical 1,000-result ceiling, publishing pages as they arrive and surfacing truncation and later-page warnings.
- Coalesce host-wide authored discovery with startup, manual, scheduled, and post-mutation GitHub refreshes while preserving existing rate-limit suppression, classified errors, direct HTTP/token handling, and branch-enrichment semantics.
- Render local worktrees immediately and retain the last fully successful authored-PR snapshot while loading and after any page/host failure. Show progressive pages during a refresh, revert partial data after failure, and remove disappeared PRs only after every host completes successfully.
- Cover exact viewer-author filtering, drafts, pagination/cursors/truncation, Enterprise hosts, partial/later-page failures, progressive publication, stale snapshot retention, successful removals, generation rejection, and refresh coalescing.

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
