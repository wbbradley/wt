# Configured ignored file tree implementation

Work on main. The untracked-file task is committed and its original plan entry is archived in the locally ignored COMPLETED.md.

- src/model.rs: add default-empty catalog ignored_files (Vec<PathBuf>) and independent status ignored_paths.
- src/config.rs: normalize literal relative paths at load/save, reject empty/root/parent components, deduplicate while preserving order. Test defaults, invalid input and round trips.
- src/git.rs: expose exit codes on runner output to distinguish check-ignore no-match from errors. Stat configured candidates first; skip missing/broken links/directories, accept links to regular files, surface other errors. Run check-ignore -z --stdin with NUL-delimited input/output for surviving candidates only, preserving raw output paths. Add real repository tests for ignored parents, negations, tracked files, info/global excludes, literal special names and symlinks; fake runner tests prove filtering and no-command behavior.
- src/background.rs: carry ignored_files in each StatusTask and loader; status and ignore classification stay on workers.
- src/tui.rs: snapshot configuration into status tasks; invalidate old generations and clear stale ignored lists on configuration replacement before old results can apply. Test reload and queued results.
- src/app.rs and src/ui.rs: reuse shared file rows, add distinct IgnoredFiles section; default expand, preserve folds, reconcile removal, reuse editor intent. Test coexistence, singleton/bare/virtual visibility, dirty counts and editor path.
- tests/editor_pty.rs: exercise ignored files through the same real editor handoff and disappearance failure path.
- README.md: document JSON setting, literal file paths, validation, filesystem-first filtering, Git rules, visibility and refresh.
- Risks: symlink checks must classify the configured link path; deleted files can race every check and must produce safe errors; generation invalidation must not reset unrelated selection or folds.

Verification: cargo fmt --check; cargo clippy --all-targets --all-features -- -D warnings; cargo test. Commit implementation, archive the verbatim original plan entry and remove it from PLAN.md. Plan files are ignored: do not stage them.

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

### Update the plan file

Read the plan file at `/home/wbbradley/src/wt/PLAN.md`. **Remove** the completed task entirely from the "Next Up" section — do not leave it in place with a [DONE] tag, strikethrough, or any other marker. The task and its related subsections should no longer appear in the plan file at all. The plan file should not have any sort of "Done" section. Then append a new entry to the completed file at `/home/wbbradley/src/wt/COMPLETED.md` with two parts, in this order:

1. A brief summary, written now, of what was actually implemented.
2. The full text of the plan entry as it existed before work began, verbatim, not paraphrased, to preserve the original.

If upcoming plan items need modifications due to a change during this implementation then update those. If new future work items were discovered, add them. If the plan file or completed file is outside the source repository or is ignored, do not try to stage it; otherwise commit it with the other changes.

## Completion audit

Both requested features are implemented. Evidence from the final code and tests:

- Raw untracked discovery: git parser byte test and real-repository test cover root/nested files, ignored/tracked exclusion and overriding status.showUntrackedFiles.
- Tree behavior: app tests cover ordinary/singleton worktrees, flat paths, default expansion, independent disclosure, refresh selection/fallback, empty/bare/virtual exclusion and identical editor intents. UI tests cover both section labels and literal file rows.
- Editor handoff: editor argument tests preserve quoting and raw paths; Bash and Zsh PTY tests exercise the actual CLI, terminal restoration before exec, controlling-terminal streams, editor failure, vanished files and unchanged shell directory.
- Ignored configuration: config tests cover defaults, rejection, normalization, deduplication, literal patterns and round trips.
- Ignored discovery: fake-runner tests prove filesystem filtering and skipped Git calls; real Git tests cover tracked/negated/ordinary/missing/directory candidates, parent directory rules, info/global excludes, links, raw bytes and refresh changes. Counts and dirty state are independent of ignored paths.
- Asynchronous refresh: status tasks carry the catalog setting; the controller reload test proves old generations cannot restore stale configured files and workers receive the replacement setting.
- README documents both sections, editor behavior, literal configuration semantics and the filesystem/Git check order.
- Verification: formatting and strict Clippy passed; full suite passed with 294 tests plus one existing release-mode latency benchmark marked ignored. One existing streaming-progress test timed out on an intermediate run, then passed in isolation and in the final full run.
