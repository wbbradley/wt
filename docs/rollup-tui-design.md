# Rollup-style inline TUI design

## Goal

Replace `wt`'s split tree/detail layout with one full-width, selectable tree that exposes worktree
and pull-request detail inline. The result should closely follow Rollup's Authored-tree language and
behavior while retaining `wt`'s repository and worktree ownership model, local-first loading,
virtual-PR materialization, Backburner rules, and worktree operations.

This document compares the current implementations and defines the intended end state. The
implementation is divided into independently reviewable tasks in `PLAN.md`.

## Current implementations

### `wt`

`src/ui.rs::render` assigns 63% of the body to `render_list` and 37% to `render_detail`.
`src/app.rs` consequently maintains two navigation systems:

- `VisibleRow` and `RowId` describe repository, worktree, virtual repository, virtual PR, and
  Backburner rows.
- `DetailRow`, `DetailRowId`, and `DetailSection` independently describe PR Overview, Checks,
  Reviews, and Feedback.
- `Pane`, `detail_selected`, `detail_scroll`, `detail_viewport_height`, and `detail_expanded` switch
  focus and retain detail-pane state.

The main tree is already strong at repository/worktree concerns: local commit ancestry takes
precedence, active PRs are represented by their worktree instead of duplicated virtual rows,
virtual-only PRs use merge-target ancestry, and stable `RowId`s preserve selection through refresh.
Filtering already searches undisplayed PR details as well as local fields. The PR detail builder
already has most of the data and summaries needed by an inline tree, including required-check
counts, folded reviewer state, unresolved feedback, URLs, and stable GitHub IDs.

The split is the limiting factor. Attention details are visible for only one branch, `Tab` and
`Pane` create a second navigation mode, `l` on a leaf transfers focus instead of expanding a tree
node, and subtree actions need special logic to reconcile tree selection with detail selection.

### Rollup

Rollup's Authored pane has one flattened semantic tree:

- `report::Row` contains repo, PR, section, reviewer, comment, and check variants.
- `SectionId` names every disclosure node, including Repo, Backburner, PR Subtree, Checks, Pending,
  Valid Results, Reviewers, Open comments, and Stacked PRs.
- `ToggledSet` stores an explicit Boolean by stable `(repo, PR number, section)` identity. An
  explicit choice wins over a data-driven default and survives refreshes.
- `push_pr` recursively emits a PR, its section headers, their visible children, and stacked PRs.
  Prefix strings are built at the same time as the semantic rows, so selection, ancestry, and
  connectors cannot drift apart.
- `selected_row` resolves a selectable index to its semantic target. `section_ctx_at` additionally
  resolves a child row to its enclosing disclosure, allowing `h` on a check or comment to collapse
  the right section and return selection to its header.

Rollup's defaults are an important part of its signal density:

- repositories, PR subtrees, Open comments, and Stacked PRs start expanded;
- Backburner, Checks, Pending, Valid Results, and Reviewers start collapsed;
- Reviewers starts expanded when a human review-summary body needs attention;
- collapsed Checks and Reviewers headers retain useful rollups;
- filtering temporarily exposes matching descendants and their ancestor path without overwriting
  saved fold state;
- explicit fold choices remain stable if a check changes category during refresh.

The `c` and `p` commands resolve selection semantically rather than from visible descendants.
Collapsed state therefore never changes their scope. A PR covers its full stack, Stacked PRs covers
descendants only, a repository covers all of its PRs, and a leaf covers its owning PR or exact
actionable item as appropriate.

## Target tree

The tree remains repository-owned and worktree-first. A local branch with a PR is one worktree node,
not a separate PR node. A virtual-only authored PR uses the same branch-node presentation with a
`virtual-only` marker and keeps Enter-to-materialize behavior.

```text
▾ acme/web
  ├─ ▾ ● feature/login · PR #42 · Fix login race [feature/login] · [~1]
  │     ├─ ▸ Worktree · clean · tracks origin/feature/login
  │     ├─ ▸ Overview · open · auto-merge off · conflicts clean
  │     ├─ ▸ Checks ✗ 3/4 required
  │     ├─ ▸ Reviewers [req, ✗ changes]
  │     ├─ ▾ Open comments
  │     │  └─ @reviewer Handle cancellation (src/login.rs) [outdated]
  │     └─ ▾ Stacked branches
  │        └─ ▾ feature/login-ui · PR #43 · Polish login UI [feature/login-ui]
  ├─   chores · clean
  └─ ▸ Backburner
```

The exact line is width-aware, but its information order is intentional:

1. connector and outer disclosure;
2. current-worktree marker and branch/worktree identity;
3. orange PR number, PR title, and muted head branch when a PR exists;
4. compact attention-only badges and local status.

Repository, worktree/virtual-PR, Backburner, section, and leaf rows are all selectable. A branch node
gets an outer disclosure only when it has rendered children. A non-PR worktree remains a compact
single row unless its Worktree section is expanded.

### `wt`-specific sections

Rollup can put all useful PR identity on its PR row. `wt` also needs to retain local and
administrative information currently available only in Details, so it adds two deliberately quiet
sections:

- **Worktree** starts collapsed. Its header summarizes local state and upstream; expansion exposes
  path, anchor, full HEAD, upstream, lock reason, and prunable state. It exists only for local
  worktrees.
- **Overview** starts collapsed. Its header summarizes PR state, auto-merge, and conflict state;
  expansion exposes URL, base/head repositories and branches, full head SHA, update time, stale
  detail errors, and warnings.

These are information-preservation adaptations, not parallel panes. `/` continues matching their
hidden values.

### Rollup-parity PR sections

- **Checks** is emitted only when check detail exists or is still loading. It starts collapsed and
  shows a colored readiness glyph plus the required ratio. When expanded, failed/error checks are
  direct children in attention-first order. Pending/Expected checks live under a nested,
  default-collapsed **Pending** node. Success/Neutral/Skipped checks live under a nested,
  default-collapsed **Valid Results** node. Unknown checks remain direct children with a muted
  unknown state so incomplete data is not presented as success.
- **Reviewers** merges outstanding user/team requests with each reviewer's latest submitted review.
  It starts collapsed unless a retained human review-summary body needs attention. Its collapsed
  header shows stable distinct tokens such as `req`, `✓ approved`, `✗ changes`, `◉ commented`, and
  `⊘ dismissed`. Review-summary feedback nests beneath its reviewer. Bot summaries stay omitted by
  the existing GitHub normalization.
- **Open comments** contains unresolved inline-thread feedback only, starts expanded, uses the full
  available width for `@author excerpt (path)`, and marks outdated threads. It is omitted when
  empty.
- **Stacked branches** starts expanded and owns local worktree descendants plus virtual-only PR
  descendants. The label says `Stacked PRs` when every descendant is virtual and `Stacked
  worktrees` when every descendant is local; mixed trees use `Stacked branches`. Local ancestry
  remains authoritative when local and GitHub topology disagree.

Backburner retains `wt`'s existing semantics: virtual-only subtrees move under the final,
default-collapsed group; local worktrees stay in their ordinary ancestry position, dimmed and
marked. Prompt and attention traversal continue to skip Backburner unless it is explicitly
selected.

## Unified row and disclosure model

Replace the two current row systems with one flattened list. Names below describe intent; exact
Rust layout can evolve during implementation.

```rust
enum InlineRowId {
    Repository(PathBuf),
    Worktree(PathBuf),
    VirtualPullRequest(CanonicalPullRequestId),
    Backburner(GitHubRepositoryIdentity),
    Section(BranchNodeId, InlineSection),
    Metadata(BranchNodeId, MetadataField),
    Check(CanonicalPullRequestId, String),
    Reviewer(CanonicalPullRequestId, String),
    Feedback(CanonicalPullRequestId, String),
}

enum InlineSection {
    BranchSubtree,
    Worktree,
    Overview,
    Checks,
    PendingChecks,
    ValidResults,
    Reviewers,
    OpenComments,
    StackedBranches,
}
```

Each emitted row carries its stable ID, semantic kind, connector prefix, optional disclosure state,
owning repository/branch/PR context, parent disclosure key, and openable URL. Rendering consumes
this structure; it does not rediscover ancestry from a separate depth list.

Disclosure state is an explicit Boolean map keyed by stable owner plus `InlineSection`. Repository
and Backburner state can use the same mechanism or retain their existing sets behind one helper API.
Defaults are computed only when a key has no explicit value. Refresh reconciles selection by
`InlineRowId`, then nearest surviving ancestor, then previous visible index.

Building rows and resolving actions must share the same semantic branch topology:

1. create one node for every non-bare local worktree;
2. attach its active PR details when available;
3. create one node for every remaining virtual-only authored PR;
4. choose local worktree ancestry first;
5. attach remaining virtual nodes by unambiguous PR head/base ancestry;
6. break cycles and ambiguous parentage into repository roots, as today;
7. apply filtering, Backburner visibility, and disclosure while flattening.

This prevents a local PR from reappearing as a virtual row and lets a mixed local/virtual stack have
one selection and one subtree scope.

## Interaction behavior

- `j`/`k`, arrows, `g`/`G`, and half-page movement operate on the single selectable row list.
- `l`/Right expands a selected disclosure header. It is a no-op on a leaf.
- `h`/Left collapses a selected disclosure. On a leaf it collapses the closest enclosing section
  and moves selection to that header. On a root branch row it collapses the complete branch
  subtree while retaining inner fold choices.
- Enter opens a check/comment URL, opens the PR URL for PR/reviewer/section rows, toggles repository
  or disclosure headers where that is the established action, selects a local worktree, and
  materializes a virtual-only PR. The action palette remains the unambiguous route for every
  worktree mutation.
- `w` opens the selected item's URL with PR fallback.
- `Tab` and all pane-focus state disappear.
- Attention navigation lands on the actionable branch node and expands only the ancestor path
  needed to reveal it; it does not overwrite inner user folds.

Filtering adopts Rollup's temporary-expansion model. Matching is case-insensitive over rendered
text plus the hidden Worktree/Overview fields already searched by `wt`. Only matching rows and
their ancestors remain. Normally collapsed sections are temporarily expanded to reveal matches;
temporary `h`/`l` changes live in filter-only state. Clearing the filter restores the exact saved
unfiltered folds.

## Copy workflows and shortcut migration

Rollup's lower-case copy keys are part of the desired UI:

- `c` copies the existing agent-ready actionable prompt. Exact check/comment rows remain exact;
  Checks, Reviewers, and Open comments scope to that class on the owning PR; a branch node includes
  itself and all stacked descendants; Stacked branches excludes the parent; a repository covers
  all represented PRs; Backburner covers its explicit members. Scope is independent of visibility
  and fold state.
- `p` copies one terse line per PR in the same structural scope:
  `{url} - {title-with-conventional-prefix-removed}`, appending ` - DRAFT` for drafts. Any leaf or
  non-stacking section resolves to its owning PR. Ordering is deterministic tree pre-order and
  duplicate PR identities are removed.

These keys conflict with `wt`'s advanced create and prune shortcuts. The migration is:

- `c`: copy agent prompt (currently `C`);
- `p`: copy review request;
- `C`: advanced create (currently `c`);
- `P`: prune (currently `p`).

`n` remains the common new tracked-worktree flow. All four actions remain in the palette, and footer,
palette, README, and reducer tests change together. Copy failures and empty scopes continue to use
the existing progress/error line rather than a modal.

## Rendering details worth copying

- One full-width bordered tree replaces the 63/37 split.
- Tree connectors and disclosure glyphs are muted; selection uses the existing full-row background.
- PR numbers remain orange per `wt`'s established palette. Reviewer names should use stable
  hash-derived colors as Rollup does, making repeated reviewers scannable.
- Checks use `✓` green, `✗` red/bold, `◉` yellow, `○` muted unknown, and `⊘` muted skipped.
- Reviewers use the same glyph family. Requested reviewers retain `[req]`; submitted reviewers use
  `(reviewed)` when useful.
- Open comments use the attention color and truncate only after consuming the actual remaining pane
  width. URLs and IDs stay available to actions/filtering rather than crowding the visible line.
- Loading retains the animated yellow spinner and prior snapshot behavior. A failed refresh does not
  add noisy badges when usable prior data remains.
- The footer becomes two compact lines describing tree navigation and the remapped direct actions;
  pane terminology disappears.

## Delivery slices

### Inline row foundation

Move existing Worktree, Overview, Checks, Reviews, and Feedback detail rows beneath their owning
branch nodes, render one full-width list, and remove pane focus. This slice deliberately preserves
the current four PR section shapes so the structural migration can be reviewed separately from
Rollup-parity section semantics.

### Disclosure and topology parity

Add branch-subtree and stacked-branch disclosures, nested check groups, combined reviewer rows and
summaries, Open comments presentation, data-driven defaults, and explicit stable fold state. This is
the slice that makes the tree visually and behaviorally match Rollup.

### Search and navigation parity

Make filtering ancestor-aware with temporary expansion, implement child-to-header collapse and
selection reconciliation, and cover refresh/category-change behavior with reducer tests.

### Copy command parity

Add the terse review-request formatter and semantic scope resolver, route both copy commands through
the unified row context, migrate the four conflicting shortcuts, and update palette/footer/help.

### Documentation and completion audit

Update the README examples and key tables, add representative renderer snapshots for narrow and wide
terminals plus mixed local/virtual stacks, and run the complete project verification and manual TUI
audit.

## Principal risks and constraints

- The mixed local/virtual topology is the highest-risk area. It must preserve the invariant that a
  branch/PR appears once and local ancestry wins without losing virtual descendants.
- A flattened row's visible index is ephemeral. Every persistent selection, fold, action, and
  refresh reconciliation must use stable semantic IDs.
- Width calculations must use terminal display width rather than byte length; existing helpers that
  count `char`s should be audited when comment excerpts and reviewer colors move inline.
- GitHub detail is asynchronous and can be incomplete. Unknown and loading states must remain
  explicit, and an absent detail snapshot must not erase an explicit fold choice.
- Remapping `c`/`p` is user-visible. The palette and uppercase replacements must land in the same
  change as the copy bindings and documentation.
- Removing Details must not remove access to paths, SHAs, warnings, stale errors, or action URLs;
  the collapsed Worktree/Overview sections and filter/action context preserve them.
