# Design

Goals: a fast lazygit-style TUI and CLI over worktrunk and zellij, and no assumptions about one person's setup beyond zellij and worktrunk. Vocabulary is in [CONTEXT.md](../CONTEXT.md).

## Scope

- CLI: `ws add|rm|ls`, `add`, `update`, `rm`, `ls`, `open`, `carnet new|ls [--closed]|search <text>|path [KEY]`, `tui`, `hooks install|uninstall|status`, `shell init fish`, hidden `hook <phase>`.
- External tools stay subprocesses: `wt`, `zellij`, `git`, `gh`, `glab`, `acli`. Review and issue access sit behind traits so native APIs can come later.

## Architecture

Single crate. `src/tui` depends on core modules, never the reverse.

```
src/  cli  config  state  sync  hooks  shell  process  zellij  worktrunk  reviews  issues  carnet  tui/{app,update,view,widgets,jobs}
```

- **Processes**: every external command goes through the `process::Runner` trait, so orchestration is tested against a fake that records calls.
- **TUI loop**: Elm architecture. `tokio::select!` over crossterm events, timers and task results produces `Action`s; `update(&mut Model, Action) -> Vec<Effect>` is pure; effects run as tokio tasks and send result actions back; the UI redraws only when state is dirty.
- **Crates**: ratatui, crossterm (`event-stream`), tokio, clap (dynamic completions), serde/serde_json, rusqlite (bundled), toml_edit, tui-input, catppuccin, tui-markdown (carnet README only), color-eyre, tracing (file log), insta.
- **Jobs**: each effect runs on a blocking thread with its own database connection and a recording runner, so every command lands in the command log. Refreshes and commit listings log only failures, since they run constantly.
- **worktrunk**: `wt list` always runs with `--config-set list.json-schema=2`, so the user's config cannot change the schema. A new worktree is `wt switch [--create] <branch> --no-cd`, with `ATELIER_WORKSPACE` and `ATELIER_GROUP_HINT` telling atelier's hook its workspace and group.
- **Refresh**: a fast `wt list` every 10 s after 10 s idle, a full refresh (`--full`, reviews, issues) every 5 min, and remote responses cached in sqlite for 4 min, so each full refresh fetches anew while other TUIs and restarts reuse it. `R` bypasses the cache; a failed fetch falls back to the cached response at any age.
- **Reviews**: listed per host of the registered repos' forges, across every project on it: GitHub through one paginated `gh api graphql` search per sub-tab (`review-requested:@me`, `author:@me`; it reports the head branch, which `gh search prs` does not), GitLab through `glab api --paginate /merge_requests` with `scope=reviews_for_me` or `created_by_me`. A review maps to a registered repo by its project's web page; `Space` runs `wt switch pr:N`/`mr:N` in that repo's default workspace and focuses the worktree's tab, and on an unregistered project says so.
- **Carnets**: items of their own kind, never registered repos, so refreshes do not list them through worktrunk and they stay out of the repo menus; the hooks and `add` refuse a carnet's path. The folder is the record ([ADR 0001](adr/0001-carnet-folder-is-the-record.md)): every dated git repo directly under the root is a carnet, found by each refresh, so nothing adds or forgets one. Its README starts with TOML front matter, `+++` `tickets = ["ABC-123", "atelier#14"]` `closed = false` `summary = "…"` `+++`, every key optional; without `tickets`, the key right after the date is its one ticket. Its group is its first ticket, and every ticket links it to that issue. `e` on a carnet sets its first ticket and keeps the others; `c` closes or reopens it. `Space` on a closed carnet opens its tab to read it and leaves it closed. Atelier commits each front matter edit on its own, `git commit -m "Link ABC-123" -- README.md`, so it never sweeps in other staged changes. The database keeps only a carnet's workspace and tab; one never placed is in the default workspace, and removing a workspace moves its carnets to the default one (worktrees still block it). `carnet new` names the folder `<date>-<name in kebab case>`, keeping a key typed at the start of the name, and writes the README (that key, else the selected group when it is a ticket key, as its ticket), and `git init`. `carnet ls` lists open carnets, newest first, with their summary (`--closed` adds the closed ones); `carnet search` runs `rg` over the root and says so when `rg` is missing; `carnet path` prints the open carnet whose ticket is the given key, else the current directory's item's group, the newest when several match, so a script or an agent working in a worktree finds the carnet of its ticket.
- **Issues**: GitHub through two paginated `gh api graphql` listings of each configured repo, most recently updated first: its open issues, then those closed and updated in the last 14 days. Jira through `acli jira workitem search --jql … --json --paginate`, in the search's order. Each scope is cached on its own. An issue's key (`ABC-123`, or `repo#12` on GitHub, `owner/repo#12` when two configured repos share a name) is the group of its linked work. `Space` opens its linked worktrees; with none, like `n`, it offers every registered repo, the linked work's repos first, then the issue's own GitHub repo, and never picks one, since a tracker-only repo holds issues whose work happens elsewhere. It then asks for a branch (`ABC-123-title` or `12-title`), and the worktree goes to the workspace of the linked work in that repo, else of any linked work, else the repo's default one, in the issue's group even when its branch names no ticket key.

## State

Database: `$XDG_STATE_HOME/atelier/atelier.db`, tables `workspaces`, `repos`, `items`, `tabs`, `cache`. Migrations are an ordered list applied in one transaction, tracked by `PRAGMA user_version`. Migration 1 is the baseline schema, written idempotently so it is a no-op on databases that already have it. The configured default workspace is created at startup rather than seeded by a migration, and code always writes `items.workspace` explicitly instead of relying on a column default. Migrations only add things, so older binaries can still read the DB.

## Config

`$XDG_CONFIG_HOME/atelier/config.toml`. Every key is optional.

```toml
default_workspace = "default"   # created on first use; cannot be removed
editor = "hx"                  # else $VISUAL, else $EDITOR, else a plain shell; never nvim by default
agent_command = "claude"
browser = "firefox"            # else $BROWSER
theme = "mocha"                # latte | frappe | macchiato | mocha
icons = "unicode"              # unicode | nerd
ticket_pattern = "[A-Z][A-Z0-9]{1,9}-[1-9][0-9]{0,5}"

[carnets]                      # absent: carnets disabled. Legacy top-level `carnet_root` still read.
root = "~/Data"

[zellij]                       # absent: built-in layouts, written to $XDG_CACHE_HOME/atelier/layouts
session_layout = "…"
worktree_layout = "…"
anchor_pane = "editor"

[tracker.github]               # and/or [tracker.jira] with `jql` and `url` (fallback $ATLASSIAN_URL / $JIRA_URL)
repos = ["owner/name"]

[tracker]
hide = { labels = ["wontfix"] }  # same conditions as a section, applied first; none: nothing hidden

[[tracker.sections]]           # ordered, first match wins; no conditions = catch-all; no sections = one per state
title = "Ready for agent"
labels = ["ready-for-agent"]   # any of
not_labels = []                # none of
state = ["todo"]               # todo | in_progress | done
blocked = false
```

Issues are normalised from each source. `state` comes from Jira's `statusCategory`. A GitHub issue is `done` once closed, completed or not planned; `in_progress` while a pull request that closes it is open; else `todo`. `blocked` is true for a GitHub issue with an open `blockedBy`, or a Jira issue in status `Blocked`. Labels compare ignoring case. A hidden issue is not listed; one that matches no section goes to a last `Other` section, shown only while it lists any.

A triage label scheme, for example:

```toml
[tracker]
hide = { labels = ["wontfix", "duplicate"] }

[[tracker.sections]]
title = "Blocked"
blocked = true

[[tracker.sections]]
title = "Ready for agent"
labels = ["ready-for-agent"]

[[tracker.sections]]
title = "Triage"
labels = ["needs-triage", "needs-info"]

[[tracker.sections]]
title = "Backlog"                  # the catch-all
```

## Zellij

- The built-in layouts are `session.kdl` and `worktree.kdl`. The worktree layout runs the resolved editor and `agent_command`.
- A tab's anchor pane is found by `title == anchor_pane`. A name set in the layout survives programs setting the terminal title (verified on zellij 0.45). Custom layouts must name one pane `editor`.
- The core also owns reconcile, tab naming and elision, and cross-session focus.

## TUI

Lazygit model: numbered side panels on the left, the main view on the right showing the selection, the command log under the main view, and a key-hint bar with per-source loading indicators at the bottom.

| Panel | Sub-tabs | Content |
|---|---|---|
| 1 | Workspaces │ Repos | Workspaces (current session first); repos with alias and default workspace |
| 2 | Work │ Carnets | Work: worktrees and open carnets of the selected workspace, grouped by group, foldable; carnets newest first, ungrouped ones in a `Carnets` group folded by default. Main worktrees always shown. Carnets: every carnet in every workspace, closed ones included, newest first with its summary; `/` matches name, tickets and summary, and a content search runs `rg` over the root. |
| 3 | To review │ Mine | Reviews |
| 4 | one per section | Issues; when the sections do not fit the title, only the active one shows, with its position |

The main view is a structured key/value detail for each kind, plus recent commits. A carnet shows its rendered README, read like the commits only for the selected carnet; an issue shows its linked work. Errors go to the command log, not toasts.

Layout: below ~100 columns the main view is hidden (`+` shows it). On short terminals the focused side panel expands and the others collapse to their titles. Mouse: click to focus or select, wheel to scroll. The accent colour is Catppuccin mauve. Work rows show `↓N` when behind upstream and a spinner while `p` runs; `icons = "nerd"` swaps the row glyphs for Nerd Font icons.

### Keys

Lazygit defaults. The keymap is one table in code that also feeds `?` and the hint bar; it is not user-configurable.

| Keys | Action |
|---|---|
| `j` `k` `↑` `↓` · `,` `.` · `<` `>` `Home` `End` `gg` `G` | item · page · top/bottom |
| `h` `l` `←` `→` `Tab` `S-Tab` · `1`–`4` · `0` | previous/next panel · jump · focus main view |
| `J` `K` `C-d` `C-u` `PgUp` `PgDn` · `H` `L` | scroll the main view from any panel · horizontally |
| `[` `]` | previous/next sub-tab |
| `Space` | open/focus tab · check out review · open an issue's linked work or start it · switch workspace |
| `Enter` | fold group header · focus main view on an item |
| `-` `=` | collapse/expand all |
| `n` | new worktree (menu with carnet when carnets are enabled) · new worktree for an issue · new workspace in Workspaces |
| `e` · `m` · `d` · `x` | edit group or repo alias · move to workspace or set a repo's workspace · remove worktree or workspace, forget repo (confirm) · close tab |
| `p` | `git pull --ff-only` on the worktree; carnets are skipped |
| `c` · `s` | close or reopen a carnet · search inside carnets with `rg` (Carnets sub-tab) |
| `o` · `y` `C-o` | open in browser · copy path/branch/URL via OSC 52 |
| `/` | substring filter on the focused panel |
| `R` · `?` · `+` `_` · `@` | refresh · actions menu · screen mode · toggle command log |
| `Esc` · `q` `C-c` | back · quit |

`Space`, `x`, `d` and `p` on a group header act on every item in the group. `?` lists the focused panel's actions, then the global ones.

## Nix

Flake outputs:

- `packages.default`: `rustPlatform.buildRustPackage`.
- `devShells.default`: the nixpkgs Rust toolchain.
- `homeModules.default`:

```nix
programs.atelier = {
  enable; package;
  settings = { };                 # rendered to config.toml
  enableFishIntegration = true;   # sources `atelier shell init fish` (the `wt` wrapper)
  worktrunk.hooks;                # read-only, worktrunk named-table form: { pre-start.atelier = "atelier hook pre-start"; … }
};
```

The module never writes worktrunk's config; whoever generates it merges `worktrunk.hooks`. Users without Nix run `atelier hooks install`, which adds named `atelier` entries with `toml_edit`.

## Testing and CI

- Unit tests: tab naming, ticket regex, section rules, migrations (including a fixture of a baseline-schema DB), `update()`.
- Parser tests on recorded JSON from `wt`, `gh`, `glab` and `acli`.
- A few insta snapshots of the main layout.
- One live zellij smoke test: a client attached through `script` (headless sessions don't spawn new tabs' panes), open a tab, find the anchor pane, close it.
- CI: GitHub Actions on Ubuntu, `cargo fmt`/`clippy`/`test` with a cache, plus one `nix build` job.

## Phases

0. Scaffold: Cargo, flake, devShell, CI.
1. Core: state, config, CLI, hooks, `hooks install`, `shell init fish`, zellij orchestration, layouts.
2. TUI frame, plus the Workspaces │ Repos and Work panels.
3. Reviews.
4. Issues.
5. Carnets.
6. Home Manager module.
