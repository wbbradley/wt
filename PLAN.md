# Next Up

## Render a compact PR-attention tree

Make the main tree answer “what needs attention?” without spending horizontal space on information better shown in Details.

Requires Hydrate canonical PR attention data.

Affected areas: `src/app.rs`, `src/ui.rs`, and their unit/render tests.

- Keep repository and existing local/PR ancestry behavior, including local ancestry taking precedence over GitHub stack metadata.
- Render worktree and virtual-PR rows with the branch, PR number, compact check/review/feedback indicators, and essential local state.
- Remove PR titles, full paths, and commit SHAs from ordinary child rows; retain them in Details and in filter matching.
- Distinguish required-check readiness from actionable optional failures.
- Define actionable attention as a failed/error check, changes requested, unresolved feedback, or merge conflict. Pending checks and outstanding review requests are waiting states, not actionable attention.
- Show unresolved-feedback counts and clear semantic colors/glyphs for success, failure, pending, unknown, draft, dirty, virtual, and conflict states.
- Keep rendering useful at narrow terminal widths without horizontal overflow; truncate only branch/repository labels as a last resort.
- Preserve selection, collapse state, filtering, scrolling, current-worktree marking, and materialization behavior.

Complete when:

- Common rows fit within an 80-column terminal without emitting long paths, SHAs, or PR titles.
- Required and optional check failures are visually distinguishable.
- Review changes, unresolved feedback counts, conflicts, dirty state, and virtual/local status are legible at a glance.
- Filtering still finds hidden title/path/check/reviewer/comment text.
- Every local worktree remains represented exactly once.

Verification:

- Add render assertions at narrow and wide widths.
- Test badge precedence, attention classification, filtering hidden detail text, ancestry, deduplication, and selection preservation.
- Update the README's TUI examples and status legend.
- Run the formatting, clippy, and full test gates.

### Implementation plan

- Modify `src/model.rs` to expose a compact actionable-attention classification over the existing required-check, review, optional-failure, feedback, and conflict summaries, with precedence tests proving waiting states are not actionable.
- Modify `src/app.rs` to resolve local branch summaries and virtual rows to their shared canonical detail record, include detailed checks/reviewers/comments and hidden title/path/SHA fields in filtering, and retain the existing local-ancestry-first, deduplicated visible-row structure and selection reconciliation.
- Modify `src/ui.rs` to replace path/SHA/title-heavy child lines with width-budgeted branch/PR rows and independently styled compact indicators for required checks, optional failures, review state, feedback, conflicts, draft, dirty, locked/prunable, virtual, loading, and stale states; keep full metadata in Details.
- Extend `src/app.rs` and `src/ui.rs` tests with canonical local/virtual resolution, hidden-detail filtering, actionable classification, narrow/wide render assertions, badge precedence, local-row uniqueness, and selection-preservation coverage.
- Update `README.md` with compact-row examples and an exact indicator/color legend while retaining the existing TUI navigation and materialization documentation.

Risks and open questions: detailed hydration can be absent or stale, so compact rows must show unknown rather than infer required readiness from the legacy aggregate rollup. Extremely deep stacks and long branch names compete with indicators at narrow widths; reserve indicator space first and truncate only the branch/repository label. ASCII prefixes keep indicator meaning available even when terminal glyph support or color perception is limited.

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

## Add selectable PR details

Turn the existing scroll-only Details pane into a structured, selectable view for PR metadata, checks, reviewers, and feedback while preserving ordinary worktree details.

Requires Hydrate canonical PR attention data. It may proceed alongside Render a compact PR-attention tree once the detailed model is stable.

Affected areas: `src/app.rs`, `src/ui.rs`, `src/tui.rs`, and focused tests.

- Introduce stable detail-row identities and sections for Attention, Checks, Reviews, and Feedback.
- Show PR title/URL, base and head, state, update time, auto-merge, conflict state, local status/path, warnings, and stale/loading state.
- Sort checks attention-first while preserving source order within equal states; mark required and optional checks explicitly.
- Show requested users/teams and each reviewer's latest state.
- Show unresolved inline threads and review summaries with author, normalized body, path, outdated marker, and permalink.
- Make `l` focus Details and `h` return to the tree. In Details, make `j`/`k`, arrows, `g`/`G`, and paging move among selectable detail rows rather than raw wrapped lines.
- Make `Enter` open the selected check, comment, review, or PR URL; rows without their own URL fall back to the owning PR.
- Preserve the selected detail identity across refreshes when it still exists, otherwise choose the nearest sensible row.
- Continue supporting read-only scrolling/details for selections without a PR.

Complete when:

- Every detailed check, reviewer, and feedback item can be reached by keyboard.
- Selection and viewport remain valid after resize, wrapping, refresh, collapse, and disappearing items.
- Opening a detail item never materializes or selects a worktree accidentally.
- Loading/stale data remains usable and visibly labeled.

Verification:

- Test navigation, focus, resize/wrapping, refresh reconciliation, URL routing, empty sections, and non-PR selections.
- Inject the URL opener in tests; do not launch external applications.
- Update the README key guide and Details description.
- Run the formatting, clippy, and full test gates.

## Copy scoped agent prompts

Provide fast clipboard prompts for addressing check failures and review feedback at item, section, PR-stack, and repository scopes.

Requires Add selectable PR details.

Affected areas: `src/app.rs`, `src/tui.rs`, a small clipboard/prompt module if useful, `Cargo.toml`, README, and tests.

- Add `C` as the agent-prompt shortcut and expose it in the action palette, leaving the existing lowercase `c` create shortcut unchanged.
- Scope prompts as follows:
  - selected check: that check;
  - selected feedback item: that comment or review summary;
  - Checks or Feedback section: actionable items of that class on the PR;
  - PR summary/worktree/virtual PR: that PR and its GitHub-stacked descendants;
  - repository: every non-backburnered actionable PR represented for that repository.
- Aggregate unresolved feedback plus failed/error checks. Include optional failed checks while labeling that they are not merge-required.
- Group output by branch and PR with title, URL, stable comment/review IDs, concise body excerpts, paths, and check URLs.
- Use stored GitHub IDs in investigation commands and instructions.
- Report “nothing to address here” without modifying the clipboard for empty scopes.
- Surface clipboard errors in the footer without crashing.
- Keep prompt gathering independent of collapsed UI state.

Complete when:

- Copying a single item excludes unrelated items.
- PR, stack, and repository prompts include all and only their actionable descendants in deterministic order.
- Prompts remain useful when a check/comment URL or database ID is absent.
- Clipboard access is isolated behind an injectable interface for tests.

Verification:

- Add exact prompt-format tests for comments, review summaries, checks, mixed stacks, forks/hosts, missing IDs/URLs, optional failures, and empty scopes.
- Add scope tests proving collapse state does not change prompt contents.
- Add success/failure clipboard tests using a fake clipboard.
- Update the README key guide and prompt examples.
- Run the formatting, clippy, and full test gates.

## Persist Backburner and attention navigation

Allow users to quiet a PR stack without hiding local worktrees, using canonical host-aware identities.

Requires Render a compact PR-attention tree and Add selectable PR details. Prompt copying must honor Backburner grouping once present.

Affected areas: a new `src/state.rs` or equivalent, `src/app.rs`, `src/ui.rs`, `src/tui.rs`, README, and tests.

- Persist Backburner membership by `CanonicalPullRequestId`, including host, under the XDG state directory with an overrideable path for tests.
- Use atomic writes, create parent directories safely, and treat a missing state file as empty.
- Do not prune absent IDs merely because an authored search is capped, inaccessible, or temporarily incomplete.
- Make `b` on a PR toggle that PR and its GitHub-stacked descendants.
- Keep PRs attached to local worktrees in their normal local ancestry position, dimmed and marked `[backburner]`.
- Move virtual-only backburnered PRs beneath a collapsed Backburner group within their canonical repository. Mixed local/virtual stacks must not duplicate or hide local worktrees.
- Keep backburnered PR status details reachable, but exclude those PRs from actionable-attention counts, repository-scoped prompts, and next/previous-attention navigation.
- Permit prompts copied explicitly from a dimmed PR or the Backburner group; the group scope covers only its backburnered PRs.
- Add `]`/`[` navigation to the next/previous non-backburnered actionable PR, wrapping at the ends and leaving selection unchanged when none exists.
- Preserve sensible selection when toggling moves virtual rows, and preserve Backburner state across refreshes, cache reloads, materialization, and restart.
- Keep membership when a backburnered virtual PR is materialized, converting it to the dimmed local representation.

Complete when:

- Restarting restores host-aware membership without conflating same-named repositories on different hosts.
- Backburnering a mixed stack never hides a local worktree.
- Virtual rows appear exactly once, either normally or under Backburner.
- Attention counts, repository prompts, and attention navigation ignore Backburner while ordinary tree/detail navigation and explicit prompts can still reach it.
- Save/load errors are visible but do not prevent startup or local worktree operations.

Verification:

- Add state round-trip, atomic replacement, missing/corrupt file, and host-collision tests.
- Test whole-stack toggles, mixed local/virtual stacks, collapsed groups, selection fallback, filtering, refresh, materialization, counts, prompt scopes, and wraparound navigation.
- Update README behavior, state-file location, and key guide.
- Run the formatting, clippy, and full test gates.
