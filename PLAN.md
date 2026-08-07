# Next Up

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
