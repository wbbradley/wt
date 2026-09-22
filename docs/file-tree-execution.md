# Untracked file tree implementation

Work on main as requested. Preserve the original PLAN.md entries in COMPLETED.md upon completion.

- Extend src/model.rs WorktreeStatus with raw PathBuf untracked paths; src/git.rs requests all files and parses NUL-separated raw bytes. Add parser and real repository discovery tests, including status.showUntrackedFiles=no, ignored files and non-UTF-8 names.
- Reuse VisibleRow::Inline in src/app.rs with an UntrackedFiles section and distinct File row identity carrying owner, section and relative PathBuf. Add children before PR details so singleton and ordinary local trees share the implementation. Default expansion, navigation, refresh reconciliation and file Enter intent must use existing disclosure and fallback mechanisms.
- Render file labels plainly in src/ui.rs; test flattened nesting and section appearance.
- Add src/editor.rs for quoted EDITOR argument parsing and Unix exec with all three streams attached to /dev/tty. Add module in src/main.rs and distinct control flow in src/tui.rs; explicitly restore TerminalGuard before exec. Missing files/configuration and exec failures return clear errors without directory output.
- Update README.md tree, keyboard and editor documentation.
- Add focused unit tests and PTY subprocess coverage for safe arguments, terminal restoration, exec failure and captured stdout compatibility.
- Risks: full untracked discovery costs more on large directories; lossy labels must never become opening paths; exec bypasses destructors; tests must not mutate shared process environment.

Verification: cargo fmt --check; cargo clippy --all-targets --all-features -- -D warnings; cargo test. Commit implementation using Conventional Commits, then remove the completed entry and append its verbatim original with an implementation summary to COMPLETED.md.

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
