# Next Up

## Global Git worktree manager TUI, repository catalog, and shell navigation

Build `wt` as a global Rust TUI for discovering, creating, inspecting, updating, and removing Git worktrees across a persistent repository catalog. It must work from any directory, support normal and bare repositories, asynchronously enrich worktrees with GitHub pull-request information through direct API requests, and integrate with Bash so accepting a worktree changes the caller's directory.

### Persistent repository catalog

- Store a versioned catalog at `~/.config/wt.json`; support `WT_CONFIG_PATH` for tests and unusual installations. Begin with this evolvable shape:

  ```json
  {
    "version": 1,
    "github_refresh_interval_secs": 300,
    "repositories": [
      {
        "path": "/absolute/path/to/project",
        "label": "project",
        "worktree_root": "/absolute/optional/worktree-parent",
        "github_remote": "origin"
      }
    ]
  }
  ```

- Treat `path` as the canonical repository anchor: the main worktree root for a normal repository or the bare repository directory. Make `label`, `worktree_root`, and `github_remote` optional and derive a stable label from the directory name.
- When given a linked worktree, resolve and store its main-worktree or bare-repository anchor. Deduplicate entries by canonical common Git directory rather than path spelling.
- Load a missing config as an empty catalog. Reject unsupported future schema versions clearly. Update the file atomically, creating its parent directory when needed, and never store GitHub tokens in it.
- Retain missing or moved entries as visibly stale until the user relinks, edits, or explicitly unregisters them.
- Provide scriptable catalog commands and equivalent TUI actions: `wt repo add [PATH]`, `wt repo list`, `wt repo edit <selector>`, and `wt repo remove <selector>`, with options for label, worktree root, and GitHub remote. Unregistering only edits the catalog; it never deletes a repository or worktree.
- If launched inside an unregistered repository, include it as a clearly marked session-only repository and offer explicit registration. An empty catalog should show actionable onboarding rather than silently mutating config.

### Git discovery and bare repositories

- Use installed Git subprocesses with argument arrays rather than shell command strings or `git2`. Put execution behind an injectable runner for deterministic tests.
- For each anchor, use `git -C <anchor> worktree list --porcelain -z`. Parse NUL-delimited records, preserve paths as `PathBuf`/`OsString`, use lossy conversion only for display, and model detached, locked, prunable, and bare records explicitly.
- Treat a bare porcelain record as the repository anchor/header, not a navigable or removable checkout. Use the same operation layer for normal and bare anchors. Local validation with Git 2.50.1 confirmed that a bare repository can create and manage linked worktrees using `git -C <bare> worktree ...` without special handling beyond anchor classification.
- Isolate discovery and operation errors per repository so one stale, corrupt, or inaccessible entry cannot block the rest of the catalog.

### Global TUI

- Group rows by repository and show its label/anchor plus each worktree's current-PWD marker, path, branch or detached label, short HEAD, lock/prunable state, asynchronous dirty/staged/untracked summary, and asynchronous PR summary.
- Initially select the worktree containing the invocation directory when possible; otherwise select the first navigable worktree. Preserve selection identity across refreshes and mutations.
- Support repository collapse/expand, filtering across repository/branch/path/PR text, terminal resize, scrolling, a detail pane, operation progress, and actionable inline errors.
- Use Vim-oriented navigation: `j`/`k` and Down/Up move, `h`/`l` collapse/expand or change panes contextually, `g`/`G` jump, `Ctrl-d`/`Ctrl-u` page, `/` filters, `r` refreshes, `Enter` accepts, and `Esc` cancels.
- Provide an action palette and documented direct shortcuts for create, move, lock/unlock, remove, repair, prune, and catalog operations. Disable actions for inapplicable headers, bare anchors, or stale records.
- Draw local catalog/worktree data before starting filesystem status or GitHub work. Run slow work with bounded concurrency and deliver results through channels carrying refresh-generation IDs so stale updates cannot overwrite current state.

### Worktree CRUD and maintenance

Expose all operations through reusable services, TUI flows, and scriptable `wt worktree ...` subcommands.

- Create worktrees from an existing unattached branch, a new branch with editable start point, or a detached commit-ish. Confirm repository, destination, branch/start point, and any parent-directory creation. Suggest `<worktree_root>/<sanitized-branch>` when configured and an editable sibling path otherwise. Validate refs, destination collisions, missing parents, existing branches, and branches already checked out.
- Read and inspect every catalogued worktree, including detached, locked, prunable, dirty, stale, and bare states. The detail view should include full path, HEAD, branch/upstream, anchor, lock reason, local status, and GitHub data or errors.
- Update by moving a worktree with `git worktree move`, locking with an optional reason, unlocking after confirmation, repairing administrative links with `git worktree repair`, and independently editing catalog metadata. Validate destinations and show exact affected paths before mutation.
- Remove with `git worktree remove`; never delete the associated branch as a side effect. Always show repository, branch or detached commit, and absolute path before confirmation.
- Refuse normal removal of a bare anchor, the main worktree, the checkout containing the process's current directory, a locked worktree, or a worktree with dirty/staged/untracked files as determined by `git status --porcelain=v2 -z`.
- Never apply `--force` implicitly. If force removal is exposed, make it a distinct action with typed confirmation containing the branch or full path and repeat the dirty-file summary immediately beforehand.
- Run `git worktree prune --dry-run --verbose`, display the exact records proposed for removal, and confirm before real pruning. Treat races and Git refusals as recoverable errors and refresh instead of assuming success.

### Selection protocol and Bash integration

- Render the interactive UI on the controlling terminal or stderr so stdout remains machine-readable. On `Enter`, restore the terminal and print exactly the selected absolute worktree-root path plus a newline. On `Esc`, restore the terminal, exit successfully, and print nothing. Errors go to stderr with nonzero status.
- Always navigate to the selected worktree root; do not preserve the caller's repository-relative subdirectory. Restore raw mode, cursor, and alternate screen after success, cancellation, error, Ctrl-C, and panic.
- Add `shell/wt.bash` defining a `wt` function. Navigation invocations call `command wt`, capture stdout, and run `builtin cd -- "$destination"` only after successful nonempty selection. Catalog/worktree/help/version subcommands pass through directly so their stdout is never treated as a path. Quote paths and ensure cancellation or failure leaves `PWD` unchanged.
- Support an optional global selector such as `wt <repo-label>:<branch-or-worktree>`. Resolve a unique branch, basename, or path immediately; open the TUI prefiltered when ambiguous rather than guessing.
- Provide Bash 3.2-compatible completion for flags, subcommands, repository labels, qualified selectors, branches, and paths. Its local-only endpoint must preserve spaces, avoid GitHub calls, and never open the TUI.

### Direct GitHub API enrichment

Follow the direct HTTP and error-classification patterns in `../git-stack/src/github.rs`, together with the background refresh and partial-result patterns in `../rollup/src/github.rs` and `../rollup/src/app.rs`.

- Use a reusable `ureq::Agent` with timeouts and explicit `Authorization: Bearer`, `Accept`, and `User-Agent: wt` headers. Preserve non-2xx response bodies for useful classification.
- Parse GitHub host/owner/repository from SSH and HTTPS remotes. For each branch, prefer its upstream remote, then the catalog's `github_remote`, then `origin`. Support github.com and GitHub Enterprise REST/GraphQL base URLs.
- Resolve host-appropriate tokens from environment, repository-scoped Git config, or `gh auth token --hostname <host>` as a credential source. Make all data requests through the Rust HTTP client, never `gh api`, and never log, display, or serialize tokens.
- Batch worktree branches per repository/host in GraphQL requests using variables and bounded batch sizes. Fetch PR number, title, URL, base/head identity, draft/open/merged/closed state, update time, review decision, and latest-commit check rollup. Prefer an open/draft associated PR, then the most recently updated PR.
- Accept GraphQL partial data alongside deduplicated warnings; treat errors-only responses as failures. Classify missing/expired auth, permission and SSO/SAML restrictions, classic-PAT restrictions, rate limiting, network failures, malformed responses, and unsupported remotes.
- Track rate-limit metadata, show remaining/reset information, and suppress retries until reset after exhaustion. Default automatic refresh to 300 seconds with a 30-second minimum, coalesce overlapping refresh requests, and retain previous PR data as visibly stale after failures. GitHub failure must never block local navigation or CRUD.

### Suggested implementation structure

- `src/main.rs`: CLI dispatch and selection output.
- `src/cli.rs`: Clap commands, selectors, and completion endpoint.
- `src/config.rs`: versioned catalog, canonicalization, and atomic updates.
- `src/model.rs`: repository, worktree, status, PR, and operation types.
- `src/git.rs`: discovery, porcelain parsing, status, and safe Git operations.
- `src/github.rs`: remote parsing, auth, HTTP/GraphQL transport, rate limits, and PR normalization.
- `src/app.rs`: reducer-style TUI state, background generations, and action workflows.
- `src/ui.rs`: Ratatui layout, forms, confirmations, details, and errors.
- `src/terminal.rs`: Crossterm lifecycle and guaranteed restoration.
- `shell/wt.bash`: Bash navigation wrapper and completion.
- `README.md`: installation, registration, bare-repository use, CRUD safety, keys, shell setup, config, GitHub auth, and troubleshooting.

Use `ratatui`, `crossterm`, `clap`, `serde`/`serde_json`, `ureq`, and a typed error layer. Prefer standard threads/channels unless an async runtime demonstrably simplifies the design.

### Verification and completion criteria

- Test missing/valid/future config versions, defaults, normal and bare registration, common-directory deduplication, stale/relinked entries, and atomic-write failure safety.
- Test porcelain parsing for multiple worktrees, spaces and non-UTF-8 paths, detached HEAD, lock reasons, prunable records, malformed data, and bare anchors.
- Use temporary real Git repositories to cover registration from main/linked worktrees; bare-repository add/list/move/lock/unlock/remove/repair/prune; all creation modes and conflicts; dirty, locked, main, current-PWD, and forced-removal safeguards; branch preservation; and prune preview/confirmation parity.
- Test GitHub behavior with a local fake HTTP server: remote and Enterprise parsing, remote/auth precedence, token redaction, GraphQL batching/variables, fork PRs, PR preference/state normalization, partial data, auth/permission/rate/network/JSON failures, and stale-data retention.
- Test all navigation/filter/collapse behavior, empty and stale states, action availability, CRUD transitions, selection preservation, refresh coalescing/stale-generation rejection, and resizing.
- Shell-test accepted navigation, exact root selection, paths with spaces, cancellation/failure PWD preservation, subcommand passthrough, and local-only completion.
- Ensure `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`, and `cargo test` pass. Manually verify normal and bare workflows, progressive GitHub updates, CRUD confirmations, terminal restoration, and sourced Bash navigation.

