# wt

`wt` is a global Git worktree manager. It keeps a catalog of repositories, presents all of their worktrees in one terminal UI, performs guarded worktree operations, and optionally enriches branches with GitHub pull-request status.

Local repository and worktree data renders first. Status and GitHub requests run in the background, so unavailable repositories, missing credentials, network failures, and rate limits do not block navigation or local operations.

## Install

Build and install with a current Rust toolchain:

```bash
cargo install --path .
```

Register repositories, including normal checkouts, linked worktrees, and bare repositories:

```bash
wt repo add ~/src/project --label project
wt repo add ~/src/service.git --label service
wt repo list
```

Running `wt` opens the global TUI. Running it inside an unregistered Git repository also shows that repository as session-only; press `a` to register it. An empty catalog displays onboarding instructions.

## Bash navigation and completion

Source the bundled Bash 3.2-compatible integration from `.bashrc`:

```bash
source /path/to/wt/shell/wt.bash
```

With the function loaded, `wt` changes the current shell to the worktree selected in the TUI. Cancellation, an empty selection, and failures leave `$PWD` unchanged. Scriptable `repo` and `worktree` commands plus help and version requests pass directly to the binary.

Navigate without opening the TUI when a branch, worktree basename, or path is unique:

```bash
wt project:feature/login
wt project:project-login
wt project:/absolute/path/to/project-login
```

An ambiguous qualified selector opens the TUI prefiltered instead of guessing. Tab completion is local-only: it completes commands, flags, repository labels, qualified selectors, branches, and paths without contacting GitHub or opening the TUI. Spaces in paths are preserved.

## TUI

The initial selection is the worktree containing the current directory. Repository rows can be collapsed, and the detail pane shows local status, administrative state, and GitHub PR data when available.

Navigation keys:

- `j`/`k` or arrows: move; `g`/`G`: first/last; `Ctrl-d`/`Ctrl-u`: half-page
- `h`/`l` or left/right: collapse, expand, or switch panes
- `/`: filter across repository, path, branch, status, PR, checks, review, warning, and error text
- `r`: coalesced local and GitHub refresh
- `Enter`: select a worktree or toggle a repository; `Esc`/`Ctrl-c`: cancel
- `?` or Space: action palette

Direct action shortcuts:

- `c`: create; `m`: move; `L`/`U`: lock/unlock; `d`: remove
- `R`: repair; `p`: prune; `a`: register; `e`: edit/relink; `x`: unregister

Forms show the exact operation inputs before a separate confirmation. Disabled palette actions explain why they are unavailable.

## Scriptable worktree operations

Inspect or list without opening the TUI:

```bash
wt worktree list
wt worktree inspect project feature/login
```

Creation supports an existing unattached branch, a new branch with a start point, or a detached commit:

```bash
wt worktree create project --branch existing-branch
wt worktree create project ~/trees/new --new-branch new --start-point main
wt worktree create project ~/trees/review --detach abc123 --create-parents
```

Mutating commands print an exact preview and prompt unless `--yes` is supplied where supported. See `wt worktree --help` for move, lock, unlock, repair, remove, force-remove, prune-preview, and prune syntax.

### Safety rules

- Normal removal refuses bare anchors, main worktrees, the worktree containing `$PWD`, locked worktrees, and dirty worktrees.
- Removal does not delete the branch.
- Force removal is a distinct command and requires `--confirm` to exactly match the branch or full worktree path.
- Parent directories are created only with `--create-parents`.
- Prune displays `git worktree prune --dry-run --verbose` output and confirms the same preview before acting.
- Catalog removal only unregisters metadata; it never deletes a repository or worktree.
- Git is always invoked with argument arrays rather than shell command strings.

Bare repositories appear as non-navigable anchors while their linked worktrees remain fully manageable. Create, repair, prune, and catalog operations work from bare anchors; checkout-only actions remain disabled on the anchor itself.

## Catalog and configuration

The default catalog is `~/.config/wt.json`. Set `WT_CONFIG_PATH` to use a different file. Writes are atomic and create missing parent directories. Missing repositories remain visible as stale entries so they can be relinked with `wt repo edit ... --path ...` or unregistered.

Example:

```json
{
  "version": 1,
  "github_refresh_interval_secs": 300,
  "repositories": [
    {
      "path": "/Users/me/src/project",
      "label": "project",
      "worktree_root": "/Users/me/src/worktrees/project",
      "github_remote": "upstream"
    }
  ]
}
```

`label`, `worktree_root`, and `github_remote` are optional. The GitHub refresh interval defaults to 300 seconds and is clamped to a minimum of 30 seconds.

## GitHub and GitHub Enterprise

For each local branch, `wt` prefers its configured upstream remote, then the catalog's `github_remote`, then `origin`. SSH and HTTPS remotes for `github.com` and GitHub Enterprise are supported. Detached and bare rows do not make PR requests.

Tokens are resolved per host in this order:

1. `GITHUB_TOKEN`, then `GH_TOKEN`, for `github.com`; or `GITHUB_ENTERPRISE_TOKEN`, then `GH_ENTERPRISE_TOKEN`, for Enterprise hosts
2. repository-local Git config `github.<normalized-host>.token`, then `github.token`
3. `gh auth token --hostname <host>`

For example:

```bash
git -C ~/src/project config --local github.token TOKEN
git -C ~/src/project config --local github.ghe-example-com.token TOKEN
```

Tokens are used only in direct HTTP authorization headers. They are never serialized, logged, or displayed, and `gh api` is never used for data access.

The TUI shows PR number, title, URL, base/head repository and branch, draft/open/merged/closed state, review decision, latest-commit check rollup, update time, warnings, and remaining/reset rate metadata. Requests are bounded and batched with GraphQL variables. Partial data remains usable with visible warnings. Authentication, permission, SSO/SAML, classic-PAT policy, rate-limit, network, malformed-response, and unsupported-remote failures appear inline.

When a refresh fails, the last successful PR data remains visible as stale. Exhausted hosts are not retried until their reset time. Manual, automatic, and post-mutation refreshes coalesce into one background catalog request at a time.

## Troubleshooting

- **Repository is stale:** relink it with `wt repo edit <label> --path <new-path>`, or unregister it with `wt repo remove <label>`.
- **No PR data:** verify the selected remote with `git remote -v`, ensure the branch has an upstream when appropriate, and configure a token for that host.
- **SSO/SAML or classic PAT error:** authorize the token for the organization or use a token type allowed by its policy.
- **Rate limited:** the detail pane shows the reset value; `wt` suppresses requests until then while retaining stale data.
- **Removal disabled:** inspect dirtiness, locks, whether the row is the main/bare worktree, and whether it contains the current directory.
- **Shell does not change directory:** ensure `shell/wt.bash` is sourced in the current interactive shell and that `command -v wt` finds the binary.
- **Terminal looks altered after an external kill:** run `reset`. Normal success, cancellation, errors, Ctrl-C, and panics restore raw mode, cursor visibility, and the alternate screen automatically.

## Development checks

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
bash -n shell/wt.bash
```
