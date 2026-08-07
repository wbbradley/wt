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

## Local-first global TUI and terminal lifecycle

Implemented the default global Ratatui browser with grouped repository/worktree rows, current-directory selection and markers, details, filtering, collapse/expand, scrolling, Vim navigation, contextual action palette and direct shortcuts, inline progress/errors, session-only onboarding, stale relinking, and all local CRUD/catalog forms and confirmations. Added bounded generation-tagged status workers with coalescing, local-first drawing, identity-preserving refreshes, `/dev/tty` rendering, exact stdout selection, and RAII raw/cursor/alternate-screen restoration across cancellation, errors, Ctrl-C, and unwinding. Reducer, controller, renderer, worker, lifecycle, and pseudo-terminal tests cover the complete phase.

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

Implemented direct, non-blocking GitHub and GitHub Enterprise PR enrichment with remote and repository-scoped credential precedence, redacted secrets, reusable timed HTTP transport, variable-only bounded GraphQL batches, fork-aware PR normalization, partial-result warnings, classified failures, rate-limit suppression, and stale-data retention. Integrated startup/manual/post-mutation/automatic single-flight refreshes plus PR summaries, details, filtering, warnings, and rate metadata into the TUI. Added fake-server, parser, app, and controller coverage for endpoints, headers, batching, precedence, normalization, failures, coalescing, cadence floors, generation rejection, and stale retention.

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

Implemented a Bash 3.2-compatible navigation wrapper that safely changes directories only after successful nonempty selections while passing scriptable/help/version commands through unchanged. Added exact repository-qualified navigation by branch, basename, or path with ambiguity routed into a prefiltered TUI, plus a hidden local-only completion endpoint covering commands, flags, repositories, branches, selectors, and filesystem paths. Added real-repository and shell tests for exact roots, spaces, cancellation and failure preservation, passthrough, ambiguity, completion, and Bash syntax compatibility.

## Bash navigation, selectors, and completion

Complete shell-facing navigation and local-only discovery ergonomics.

- Add `shell/wt.bash` with a Bash 3.2-compatible `wt` function: navigation captures `command wt` stdout and calls `builtin cd -- "$destination"` only after successful nonempty selection; catalog/worktree/help/version commands pass through unchanged.
- Ensure accepted selections always navigate to the worktree root and that cancellation/failure leaves PWD unchanged, including paths containing spaces.
- Add `wt <repo-label>:<branch-or-worktree>` unique resolution by branch/basename/path, opening the TUI prefiltered when ambiguous rather than guessing.
- Add Bash 3.2-compatible completion for flags, subcommands, labels, qualified selectors, branches, and paths through a local-only endpoint that preserves spaces, avoids GitHub, and never opens the TUI.
- Shell-test navigation, exact roots, spaces, cancellation/failure preservation, passthrough, selectors, ambiguity handling, and completion.

## Documentation, integration hardening, and completion audit

Added comprehensive product documentation for installation, catalog and bare-repository workflows, TUI navigation and actions, safe scriptable CRUD, Bash navigation/completion, selectors, configuration, GitHub/GHE credentials and failure behavior, troubleshooting, and development checks. Strengthened renderer coverage for progressive and stale GitHub states, ran the complete formatting/lint/test suite, and manually exercised isolated normal/bare registration, confirmed CRUD, exact selection, sourced Bash navigation, progressive local/GitHub rendering, cancellation, and terminal restoration. The final requirement audit found no remaining PLAN work.

## Documentation, integration hardening, and completion audit

Finish product documentation and verify the complete normal/bare/global workflow end to end.

- Document installation, registration, unregistered-session onboarding, bare repositories, CRUD safety and confirmations, keys/action palette, shell setup/completion, selectors, config, GitHub/GHE authentication, rate/error behavior, and troubleshooting in `README.md`.
- Verify repository-level error isolation, progressive local/GitHub rendering, action confirmations, selection output, and terminal cleanup across integrated flows.
- Ensure `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`, and `cargo test` pass.
- Manually verify normal and bare workflows, progressive GitHub updates, CRUD confirmations and safeguards, terminal restoration, cancellation/failure behavior, and sourced Bash navigation.
- Audit every requirement preserved across the preceding phases and add/fix any missing coverage before declaring the product complete.

## Self-contained Bash shell initialization

Added `wt shell-init bash` as a self-contained, config-independent initialization command that emits the checked-in Bash integration byte-for-byte with normal stdout error handling. Extended wrapper passthrough and local completion, made quoted command substitution the documented setup path while retaining direct sourcing for checkout development, and added CLI and eval-based shell coverage for validation, malformed catalogs, navigation, spaces, cancellation, failures, passthrough, completion, repeated initialization, and Bash syntax.

## Self-contained Bash shell initialization

Make Bash navigation and completion installable directly from the `wt` binary, without requiring users to locate the repository's `shell/wt.bash` file.

- Add a public `wt shell-init bash` CLI command that writes the Bash 3.2-compatible integration script to stdout and exits successfully without opening the TUI, emitting a navigation selection, reading the repository catalog, or contacting GitHub.
- Embed or otherwise derive the command output from `shell/wt.bash` so the installed binary is self-contained and the checked-in script cannot drift from the emitted integration.
- Add `shell-init` to the Bash wrapper's passthrough commands so `wt shell-init bash` continues to work after the `wt` function has already been registered.
- Keep unsupported or missing shell names as clear, nonzero CLI usage errors; Bash is the only required shell for this task.
- Update `README.md` installation, shell setup, and troubleshooting guidance to use the safe form `eval "$(wt shell-init bash)"`, while retaining direct sourcing only as an optional development alternative if useful.
- Extend `tests/bash_shell.rs` or add CLI coverage proving that the emitted script matches the maintained Bash integration, evaluates successfully in Bash, enables navigation and completion, preserves passthrough behavior, remains Bash 3.2-compatible, and works independently of catalog validity.
- Run `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo test`, and `bash -n shell/wt.bash`.

Complete when a user with only the installed `wt` binary can add `eval "$(wt shell-init bash)"` to `.bashrc` and receive the existing navigation wrapper and completion behavior.

## Catalog settings and mutation locking for authored PRs

Added backward-compatible `repository_root` and `github_hosts` catalog settings with a `~/src` runtime default, deterministic host unioning, strict leading home/environment expansion, canonical symlink-aware root creation, and actual writability validation. Added `wt config show` and `wt config set repository-root` with exact expression persistence, Bash passthrough, and completion. Added a stable advisory `<catalog>.lock` with cancellable retry, and changed every CLI/TUI catalog mutation to lock, reload, mutate, and atomically save so concurrent writers cannot lose updates. Added focused model, resolver, symlink, rejection, lock cancellation/serialization, stale-writer, persistence, output, and completion coverage.

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

## Background materialization, cancellation, and documentation

Moved authoritative refresh, catalog lock waiting, repository bootstrap, fetch, safe branch preparation, and worktree creation into a responsive background job with visible progress. A shared cancellable process runner drains output concurrently, checks child completion before cancellation, and kills/reaps Git plus its helper process group. Ctrl-C cancels the job without returning a shell path; lock waits and active children terminate, incomplete clones and uniquely marked staging worktrees are cleaned, and completed repositories, refs, branches, and final worktrees persist. The sidecar lock remains held through registration and exact worktree creation. Added controller/process/lock/cleanup/retention tests, a direct-selector regression, and comprehensive authored-PR configuration, bootstrap, branch-safety, persistence, locking, and cancellation documentation.

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

## Safe PR branch and linked-worktree materialization

Added a dedicated PR materialization service that selects and tracks a matching real head remote or falls back to GitHub's base PR ref without adding fork remotes, validates fetched OIDs against refreshed metadata, and passes private-fetch credentials only through redacted environment state. Canonical branch markers and ancestry checks safely fast-forward unattached branches while preserving checked-out, ahead, diverged, and other-PR branches under deterministic disambiguated names. Worktrees now reuse exact same-PR checkouts or use the shared creation path at the single configured/default destination, treating broken symlinks and all other objects as occupied. Enter refreshes the ordinary local view and returns the exact canonical path through the shell-selection protocol. Added real-Git and controller coverage for tracking, fallback, race rejection, credential safety, branch preservation, markers, destination rules/collisions, reuse, and exact selection.

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

## Safe repository bootstrap for virtual PRs

Added authoritative selected-PR refreshes that accept closed and merged PRs while updating the current head SHA and rejecting missing or inaccessible PRs. Added deterministic, collision-safe bare-repository bootstrap with existing-repository reuse, staged partial clones and filter fallback, SSH-first transport, noninteractive environment-only HTTPS credentials, token redaction, post-clone identity validation, and atomic installation. Catalog registration now preserves stale-entry metadata and preferred remotes, selects unique labels, and cleans only owned incomplete-clone artifacts. Added focused coverage for metadata refresh, collisions and broken symlinks, transport fallback and credential safety, cleanup, stale relinking, matching reuse, and label selection.

## Safe repository bootstrap for virtual PRs

- Re-fetch selected PR metadata and head SHA. Allow checkout after close/merge while displaying state; abort only when the PR/repository is gone or inaccessible.
- Reuse a usable mapped repository. Otherwise resolve `repository_root` and try `<repo>.git`, `<owner>-<repo>.git`, then `<host>-<owner>-<repo>.git`, reusing only canonical identity matches and never adopting, overwriting, or deleting unrelated paths.
- Clone the base as a bare partial clone with `--filter=blob:none`, falling back to normal bare clone when unsupported.
- Prefer SSH, then authenticated noninteractive HTTPS without exposing tokens in arguments, logs, progress, or errors; disable Git/SSH prompts.
- Register the validated bare repository with a unique label. When relinking a stale matching entry, preserve label, `worktree_root`, and preferred remote and never touch the stale path.
- Keep validated/registered repositories after later failures; clean only clearly marked incomplete clone artifacts.
- Cover name collisions, stale relinking, partial-clone fallback, transport credential safety, and failure-stage retention/cleanup.

### Implementation plan

- Modify `src/github.rs` to re-fetch one canonical PR by base repository and number, normalize current base/head SHA and open/draft/closed/merged state, and classify missing/inaccessible repositories without rejecting closed or merged PRs.
- Create `src/bootstrap.rs` with:
  - `CloneRunner`/`CloneRequest` abstractions that expose argument arrays and controlled environment without logging secrets.
  - deterministic candidate resolution for `<repo>.git`, `<owner>-<repo>.git`, and `<host>-<owner>-<repo>.git`, treating symlinks and every existing filesystem object as occupied unless validated Git remotes canonically match.
  - staged bare cloning with `--filter=blob:none`, transport-aware fallback to normal clone only for filter rejection, SSH-first then noninteractive HTTPS using an environment-only authorization header, and secret redaction from errors.
  - post-clone bare/remote validation, atomic installation, catalog reuse/registration, stale matching-entry relinking with metadata preservation, and unique label selection.
- Modify `src/main.rs` to register the bootstrap module and expose the minimum credential helpers needed for safe HTTPS cloning.
- Add focused unit/integration fixtures for current PR re-fetch including closed/merged states, candidate collisions and reuse, broken symlinks, partial-filter fallback, SSH-to-HTTPS fallback, argument/log redaction, staged cleanup, stale relinking, unique labels, and successful repository retention.

Risks: a failed `git clone` can leave arbitrary partial contents, so retries must occur only inside an owned marked staging directory. HTTPS authentication must never put a token in arguments or returned errors; tests will inject a recognizable secret and inspect every observable command/output path. A cached stale identity can safely authorize relinking the catalog entry, but never deletion or mutation of the stale filesystem path.

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

## Virtual authored-PR repository and row models

Added stable canonical virtual repository and PR row models driven by progressive authored snapshots and canonical mappings. Mapped groups render in catalog order, unmapped groups sort afterward by base repository, and PRs sort newest-first. Identity-based replacement preserves selection and expansion across pages/reorders, with repository-header or nearest-row fallback on authoritative removal. Filtering covers repository, head branch, `#number`, title, and author. Virtual rows render the required label, `[no local repo]`, `[virtual]`, checks, and complete base/head/SHA/state/review/URL details. All palette/direct actions explain that virtual PRs are Enter-only, and Enter emits materialization without confirmation. Added focused reducer and renderer coverage for ordering, filtering, stability, fallback, markers, details, and action behavior.

## Virtual authored-PR repository and row models

- Extend `src/app.rs`, `src/ui.rs`, and `src/tui.rs` with stable canonical IDs and explicit virtual-repository/virtual-PR models.
- Preserve selection and expansion as pages arrive or refresh reorder occurs; on completed removal, select the repository header or nearest row.
- Preserve catalog order for mapped repositories; append unmapped base repositories sorted by `owner/repository`; sort their virtual PRs by newest `updated_at`.
- Label groups `owner/repository`; show a grey `[no local repo]` marker for unusable repositories.
- Render `#<number> <head-branch> — <title> [virtual] [checks]`; show complete base/head/SHA/draft/check/review/URL detail and `Enter to create worktree`.
- Filter on repository, branch, PR number, title, and author. Disable every palette/direct action on virtual rows with a reason; Enter is the sole materialization gesture with no confirmation.
- Cover stable selection/order/filtering, progressive updates, `[no local repo]`, detail rendering, and action disabling.

### Implementation plan

- Extend `src/app.rs` with canonical virtual repository/PR row IDs and views, merge mapped groups into catalog order, append sorted unmapped groups, sort PRs by `updated_at`, and preserve/fallback selection and expansion during progressive replacement.
- Extend filtering, selection helpers, key handling, and action availability so repository/branch/number/title/author match, all actions explain why they are disabled on virtual rows, and Enter emits the sole materialization intent.
- Extend `src/tui.rs` to rebuild virtual views after each authored page/final outcome and accept the materialization intent for the next implementation phase.
- Extend `src/ui.rs` with virtual group/PR rows, grey `[no local repo]`, checks/state tags, complete details, loading/stale context, and updated empty/footer text.
- Add reducer and render tests for ordering, deduplication, expansion/selection stability, selected-removal fallback, filtering, action disabling, rows, markers, and details.

Risks: progressive replacement can reorder rows while a user is navigating; identity, not indices, must anchor selection. A completed removal needs a deterministic repository-header/nearest fallback without affecting the stale baseline behavior already implemented.

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

Added host-wide viewer-authored PR discovery to the existing single-flight GitHub worker. It issues variable-only `author:@me` searches, validates each result against `viewer.login`, includes drafts, paginates in 100-item pages, supports Enterprise endpoints, reuses token/error/rate suppression, and treats later failures or the 1,000-result truncation ceiling as non-authoritative. Branch enrichment arrives first and authored pages stream progressively. A generation-aware snapshot reducer overlays pages on the last complete baseline while loading, atomically commits successful replacements/removals, and restores the baseline after any host/page failure. Added fake-server and reducer coverage for exact filtering, cursors, drafts, Enterprise transport, later failures, truncation, progressive overlays, rollback, removal, and stale generations.

## Viewer-authored PR discovery and progressive snapshots

- Extend `src/github.rs` to discover, per configured/inferred host, the authenticated viewer and every open PR authored by that viewer only—include drafts, exclude review-requested and merely team-authored PRs.
- Paginate in the background through GitHub Search's practical 1,000-result ceiling, publishing pages as they arrive and surfacing truncation and later-page warnings.
- Coalesce host-wide authored discovery with startup, manual, scheduled, and post-mutation GitHub refreshes while preserving existing rate-limit suppression, classified errors, direct HTTP/token handling, and branch-enrichment semantics.
- Render local worktrees immediately and retain the last fully successful authored-PR snapshot while loading and after any page/host failure. Show progressive pages during a refresh, revert partial data after failure, and remove disappeared PRs only after every host completes successfully.
- Cover exact viewer-author filtering, drafts, pagination/cursors/truncation, Enterprise hosts, partial/later-page failures, progressive publication, stale snapshot retention, successful removals, generation rejection, and refresh coalescing.

### Implementation plan

- Add authored-PR/page/outcome models in `src/model.rs` and an authored snapshot reducer in `src/app.rs` that retains a complete baseline, overlays current pages only while loading, rejects stale generations, commits removals only on total success, and reverts partial data on failure.
- Extend `src/github.rs` with host descriptors, a variable-only GraphQL viewer/search query, exact response-author validation, 100-item cursor pages, a ten-page/1,000-result cap, classified per-host/page failures, rate suppression, and a page callback.
- Extend `src/tui.rs` so the existing single-flight background refresh publishes branch enrichment first, forwards authored pages incrementally, completes only after every host, and coalesces startup/manual/scheduled/post-mutation requests.
- Add fake-server and reducer/controller tests for viewer filtering, drafts, cursors, Enterprise URLs, truncation, later failures, progressive data, stale rollback, successful disappearance, and generation/coalescing behavior.

Risks: a partially successful multi-host refresh cannot become authoritative; pages may be displayed transiently, but the reducer must retain a separate complete baseline and restore it if any host or later page fails. Search results must still be checked against `viewer.login` rather than trusting query qualifiers alone.

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

## Canonical GitHub remote identities and authored-PR mapping

Added backward-compatible canonical GitHub identity caches for every remote and a derived preferred remote. Background refreshes now reconcile and persist caches under the catalog sidecar lock, remove disappeared/unsupported remotes, retain and warn on repointed conflicts, update in-memory views, and infer explicit/cached/default discovery hosts. Added base-repository canonical PR IDs, extraction of every GitHub-associated PR for active worktrees, canonical suppression, deduplication, and deterministic configured-remote/origin/catalog-order mapping that rejects unusable paths. Added focused fake-Git, mapping, fork/base, host, association, serialization compatibility, and background persistence coverage.

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

## Materialized pull request cache reconciliation

Reconciled the remote cache immediately after a virtual pull request becomes a local worktree, recording its branch enrichment, refreshed authored data, and active canonical identity without triggering a catalog-wide refresh. Startup now trusts cached active identities only when the complete navigable local-branch topology matches, while still hydrating individually matching branches; bare anchors are consistently excluded. Added regression coverage for deterministic cache upserts and newly created branches absent from an older cache.

## Materialized pull request cache reconciliation

This task was supplied directly by the user while `PLAN.md` contained no queued entry:

> I also noticed that on boot, if on the last time we ran, we converted a virtual branch to a local worktree, then that branch appears twice because of the cache. We should probably invalidate the cache when we create a new worktree that makes a branch no longer virtual.
