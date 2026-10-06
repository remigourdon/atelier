# Design

Goals: a fast lazygit-style TUI and CLI over worktrunk and zellij, and no assumptions about one person's setup beyond zellij and worktrunk. Vocabulary is in [CONTEXT.md](../CONTEXT.md).

## Scope

- CLI: `ws add|rm|ls`, `add`, `update`, `rm`, `ls`, `open`, `carnet new|ls [--closed]|search <text>`, `context [PATH] [--key KEY] [--json]`, `tui`, `hooks install|uninstall|status`, `shell init fish`, hidden `hook <phase>`.
- External tools stay subprocesses: `wt`, `zellij`, `git`, `gh`, `glab`, `acli`. Review and issue access sit behind traits so native APIs can come later.

## Architecture

Single crate. `src/tui` depends on core modules, never the reverse.

```
src/  cli  config  state  items  finish  hooks  shell  process  git  zellij  worktrunk  reviews  issues  carnet  context  tui/{app,update,view,widgets,jobs,schedule,lists}
```

- **Processes**: every external command goes through the `process::Runner` trait, so orchestration is tested against a fake that records calls.
- **TUI loop**: Elm architecture. `tokio::select!` over crossterm events, timers and task results produces `Action`s; `update(&mut Model, Action) -> Vec<Effect>` is pure; effects run as tokio tasks and send result actions back; the UI redraws only when state is dirty.
- **Crates**: ratatui, crossterm (`event-stream`), tokio, clap (dynamic completions), serde/serde_json, rusqlite (bundled), toml_edit, tui-input, catppuccin, tui-markdown (carnet README only), color-eyre, tracing (file log), insta.
- **Jobs**: each effect runs on a blocking thread with its own database connection and a recording runner, so every command lands in the command log. Refreshes and commit listings log only failures, since they run constantly.
- **worktrunk**: `wt list` always runs with `--config-set list.json-schema=2`, so the user's config cannot change the schema. A new worktree is `wt switch [--create] <branch> --no-cd`, with `ATELIER_WORKSPACE`, `ATELIER_GROUP` and `ATELIER_ISSUE_KEYS` (comma-separated) telling atelier's hook its workspace, group and issue keys. The hook normalises the group (trimmed, uppercased); without `ATELIER_GROUP` the worktree gets no group, and without `ATELIER_ISSUE_KEYS` it links the issue keys in its branch, as a worktree a refresh discovers does. Atelier picks both before switching, and they seed a worktree once, when it is recorded: a new worktree takes the selected row's group and the keys in its branch; a checkout, the keys in the review's branch then its title, and the one group linked to any of them, else none; a start, the issue's key then the branch's, and the one group among the issue's linked work, else none. A worktree that already exists keeps its group and keys, except that a start adds the issue's key. Keys are edited afterwards, one item at a time ([ADR 0002](adr/0002-groups-are-labels-links-live-on-items.md)).
- **Refresh**: a fast `wt list` every 10 s after 10 s idle, a full refresh (`--full`, reviews, issues) every 5 min, and remote responses cached in sqlite for 4 min, so each full refresh fetches anew while other TUIs and restarts reuse it. `R` bypasses the cache; a failed fetch falls back to the cached response at any age. `tui::schedule` decides when, with no I/O. Reviews and issues are feeds, one per provider and tracker, listed through one job. On a full refresh, issues are listed at once and reviews once the snapshot names their hosts. A feed still listing keeps its own turn for later, so `R` is not lost and idle feeds are not listed twice. A job refreshes once done only when it changes items, workspaces, repos or tabs, so browsing or switching workspace does not. A refresh asked for while one runs waits for it, and several such requests make one.
- **Reviews**: listed per host of the registered repos' forges, across every project on it: GitHub through one paginated `gh api graphql` search per sub-tab (`review-requested:@me`, `author:@me`; it reports the head branch, which `gh search prs` does not), GitLab through `glab api --paginate /merge_requests` with `scope=reviews_for_me` or `created_by_me`. A review maps to a registered repo by its project's web page; `Space` runs `wt switch pr:N`/`mr:N` in that repo's default workspace and focuses the worktree's tab, and on an unregistered project says so.
- **Carnets**: items of their own kind, never registered repos, so refreshes do not list them through worktrunk and they stay out of the repo menus; the hooks and `add` refuse a carnet's path. The folder is the record ([ADR 0001](adr/0001-carnet-folder-is-the-record.md)): every dated git repo directly under the root is a carnet, found by each refresh, so nothing adds or forgets one. Its README starts with TOML front matter, `+++` `group = "LOGIN REWRITE"` `issues = ["ABC-123", "remigourdon/atelier#14"]` `closed = false` `summary = "…"` `+++`, every key optional; without front matter it has no group and links nothing, whatever its folder's name. Reading normalises it without rewriting the file: the group is uppercased, and a short GitHub key (`atelier#14`) resolves to `owner/name#14` when exactly one configured `[tracker]` GitHub repo has that name (a registered repo's alias never counts), else it stays as written. Atelier writes the normal form the next time it edits that front matter for another reason. `e` on a carnet sets its group; `c` closes or reopens it; `d` never removes one. `Space` on a closed carnet opens its tab to read it and leaves it closed. Atelier commits each front matter edit on its own, `git commit -m "Set group LOGIN REWRITE" -- README.md`, so it never sweeps in other staged changes. The database keeps a carnet's workspace and tab, and caches its group (to name its tab) and issue keys; one never placed is in the default workspace, and removing a workspace moves its carnets to the default one (worktrees still block it). A refresh that finds no carnet at all, as when the root's drive is not mounted, keeps those rows. `carnet new` names the folder `<date>-<name in kebab case>`, keeping an issue key typed at the start of the name, and writes the README (the selected group, whatever it is, and that key, if any; never the selected item's keys), runs `git init` and commits the README as `Create carnet`. `carnet ls` lists open carnets, newest first, with their group, issue keys and summary (`--closed` adds the closed ones); `carnet search` runs `rg` over the root and says so when `rg` is missing or nothing matches.
- **Issues**: listed on each full refresh as soon as it starts. GitHub through two paginated `gh api graphql` listings of each configured repo, most recently updated first: its open issues, then those closed and updated in the last 14 days. Jira through `acli jira workitem search --jql … --json --paginate`, in the search's order. Each scope is cached on its own. An issue's key is `ABC-123`, or always `owner/repo#12` on GitHub, shown as `repo#12` unless another configured repo shares the name. Its linked work is every item linking that key, in any group. `Space` opens its linked worktrees; with none, like `n`, it offers every registered repo, the linked work's repos first, then the issue's own GitHub repo, and never picks one, since a tracker-only repo holds issues whose work happens elsewhere. It then asks for a branch (`ABC-123-title` or `12-title`), and the worktree goes to the workspace of the linked work in that repo, else of any linked work, else the repo's default one. It links the issue's key, even when its branch names none, and joins the one group among the issue's linked work, else no group.
- **Finish**: `f` plans what is left once a review merged. A worktree other than a main one is finished when worktrunk's `wt list` reports its branch integrated (`default_branch.integration` set, or `display.state` `integrated` or `empty`; a `null` integration is undetermined, so not integrated), or when its branch has an upstream whose remote-tracking ref is gone (`git for-each-ref refs/heads --format='%(refname:short)%00%(upstream:track)'` says `[gone]`, which `wt list` cannot tell from never pushed). Integrated wins when both hold; nothing else counts, no forge query and no issue state. Each refresh runs the gone check with `wt list`, which is local, and the Work panel dims finished rows with `⊂` (integrated) or `⊘` (gone), Nerd Font icons with `icons = "nerd"`, as fresh as the last fetch: nothing fetches in the background. `f` on Work plans the selection's whole groups across repos and workspaces, and an item in no group alone; on Workspaces, a sweep that touches only that workspace's items, worktrees, carnets and main worktrees alike: a part per group with a finished worktree there, then each finished worktree in no group (every group when none has one); on an issue, the whole groups of its linked work, across workspaces, and its linked items in no group alone, its state in the title. It first runs `git fetch --prune` once per repo involved, then builds the plan (`finish`, pure) from a fresh snapshot; a failed fetch is logged and the plan warns `fetch failed in <repo>: showing last known state`. Lines: remove each finished worktree with `wt remove`, checked when clean, unchecked and `--force` when dirty, never `-D`, so worktrunk deletes an integrated branch and keeps any other with its unpushed commits, noted `branch kept: N commits not in <default branch>` from `default_branch.ahead`; an info line, never checkable, for each unfinished worktree; close each open carnet of the group, checked only when every worktree of the group but the main ones, in any workspace, has a checked remove line, so a sweep leaves it unchecked while the group has worktrees elsewhere; pull the main worktree of each repo the plan touches, `git pull --ff-only --prune`, checked when it is clean, on the default branch and behind, else an info line. Main worktrees are never removed. `Enter` runs the checked lines, removals, then closes, then pulls, each failure logged and the rest still run, then refreshes. Closing tracker issues and removing emptied workspaces stay out.
- **Context**: `context` describes the innermost recorded item holding a directory (`.` by default), or an issue key with `--key`, for scripts and coding agents: lines to read, or JSON with `--json`. `item` (path, kind, repo, branch), `workspace` (name, whether it is the current session, and `session`, that of the item's recorded tab), `group` (the item's, a carnet's as its folder records it), `issue_keys` (the item's, in order, or the key given), `issue` (the first key's cached issue with every field and `cached_at`, from the configured scopes' last fetch, never fetched), `worktrees` (every recorded worktree in the group across repos, or linking the key given, with repo, branch, workspace, tab session, whether it holds the directory, and the `wt list` status: dirty, diff, upstream, head, symbols; or an `error` when its repo's listing failed), `carnets` (every carnet in the group or linking the first key, with its group and issue keys, closed ones included, newest first) and `carnet` (the newest open one). A directory in no item gives `null`s and empty lists and still succeeds; an item in no group still gives its `item` and `workspace`. Nothing is written: the database is opened read-only (an empty one stands in when there is none yet), the worktree holding the directory is read with `wt list statusline --format json` (same schema pin), which adds its CI (state, stale, branch workflow), its `review` (number, url, decision) and `finished` (`integrated`, or `upstream_gone` from the local gone check; enum values are snake_case) to its `status`; the other worktrees come from `wt list` without `--full`, once per other repo, without CI. Its paths are canonicalised as the database records them, and unrecorded worktrees are not discovered. So `context` makes one network call, through worktrunk: the statusline's CI lookup, which worktrunk caches for 30–60 s in `.git/wt/` and which costs a second or two when the cache is stale. Atelier itself never fetches. The JSON is not versioned: its readers are expected to tolerate fields being added.
- **Statusline**: `statusline` prints one ANSI line, for zjstatus, about the item holding the current directory, resolved as `context` resolves it and from the same `wt list statusline` call. A worktree shows its first issue key, shown short, the Work row's cells in its colours and glyphs: worktrunk's symbols (dirty, ahead), `↓N`, the CI `◆`, the review's reference and decision, and `⊂`/`⊘` when finished, then the cached issue title (any age, never fetched); a main worktree shows `main worktree`, a house with `icons = "nerd"`, instead of a key. A carnet shows its first issue key, `closed` when closed, and its summary; worktrunk is not run. Anything else prints nothing and succeeds. Absent cells take no space. zjstatus passes no width and never truncates (an overlong bar is clipped at its right edge, or with `format_hide_on_overlength` a whole part is hidden), so the line orders by importance instead of fitting a width: the key or main worktree, `closed`, the cells, then the title or summary last, the first to be cut. zjstatus runs commands in its own directory, so its command sets `cwd` to `{focused_pane_cwd}`: each tab's bar shows the focused pane's item, which is the visible tab's. The gone check is the same local `git for-each-ref` as the Work panel's. When the `[zellij] zjstatus` plugin file exists, both built-in layouts add a one-row zjstatus pane under the tab bar running `atelier statusline` every 10 s on the theme's mantle, the tab bar's background (the line undoes only the styles it sets, never a full reset, so that background survives), found through `PATH` (the binary's own path would go stale across updates); checked when the layouts are written, at startup; atelier never installs it.

## State

Database: `$XDG_STATE_HOME/atelier/atelier.db`, tables `workspaces`, `repos`, `items`, `tabs`, `cache`. Migrations are an ordered list applied in one transaction, tracked by `PRAGMA user_version`. Migration 1 is the baseline schema, written idempotently so it is a no-op on databases that already have it. The configured default workspace is created at startup rather than seeded by a migration, and code always writes `items.workspace` explicitly instead of relying on a column default. Migration 2 replaces `items.group_key` with `group` and `issue_keys` (a JSON array) without converting it, so an older binary can no longer read items.

## Config

`$XDG_CONFIG_HOME/atelier/config.toml`. Every key is optional.

```toml
default_workspace = "default"   # created on first use; cannot be removed
editor = "hx"                  # else $VISUAL, else $EDITOR, else a plain shell; never nvim by default
agent_command = "claude"
browser = "firefox"            # else $BROWSER
tool = "lazygit"               # what `g` runs in an item's directory; {path} and {branch} are replaced, shell-quoted
theme = "mocha"                # latte | frappe | macchiato | mocha
icons = "unicode"              # unicode | nerd
issue_key_pattern = "[A-Z][A-Z0-9]{1,9}-[1-9][0-9]{0,5}"   # the issue keys found in branches and names

[carnets]                      # absent: carnets disabled. Legacy top-level `carnet_root` still read.
root = "~/Data"

[zellij]                       # absent: built-in layouts, written to $XDG_CACHE_HOME/atelier/layouts
session_layout = "…"
worktree_layout = "…"
anchor_pane = "editor"
zjstatus = "…"                 # default $XDG_CONFIG_HOME/zellij/plugins/zjstatus.wasm; when it exists, the built-in layouts add the statusline row

[tracker.github]               # and/or [tracker.jira] with `jql` and `url` (fallback $ATLASSIAN_URL / $JIRA_URL)
repos = ["owner/name"]

[reviews]                      # absent or empty: no review fetching; independent of [tracker]
providers = ["gitlab", "github"] # hosts come from registered repos; requires glab and gh respectively

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

Jira search requests `key,summary,status,labels,assignee,issuetype,priority`: ACLI rejects
`updated` as a search field. The parser keeps an updated timestamp when returned and leaves it
empty when omitted. Issue order still follows the configured JQL.

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
- `g` runs `tool` with `zellij run --floating --close-on-exit --cwd <path> -- sh -c <tool>`, so it needs no tab and opens none.
- The core also owns reconcile, tab naming and elision, and cross-session focus.

## TUI

Lazygit model: numbered side panels on the left, the main view on the right showing the selection, the command log under the main view, and a key-hint bar with per-source loading indicators at the bottom.

| Panel | Sub-tabs | Content |
|---|---|---|
| 1 | Workspaces │ Repos | Workspaces (current session first); repos with alias and default workspace |
| 2 | Work │ Carnets | Work: worktrees and open carnets of the selected workspace, grouped by group, foldable; carnets newest first, ungrouped ones in a `Carnets` group folded by default. Main worktrees always shown. Carnets, only while carnets are enabled: every carnet in every workspace, closed ones included, newest first with its group, issue keys and summary; `/` matches name, group, issue keys and summary. `s` runs `rg` over the root and narrows the list to the carnets with hits, whose hit lines the main view shows above the README, until `Esc`. |
| 3 | To review │ Mine | Reviews |
| 4 | one per section | Issues; when the sections do not fit the title, only the active one shows, with its position |

The main view is a structured key/value detail for each kind, plus recent commits. A worktree or a carnet shows its group and issue keys; a carnet also its summary and, under a `README` label, its rendered README without the front matter, read like the commits only for the selected carnet; an issue shows its linked work. Errors go to the command log, not toasts.

The command log retains up to 500 entries in memory. `E` captures those entries and writes a new
`command-log-<timestamp>-<pid>.log` under `$XDG_STATE_HOME/atelier/logs`, defaulting to
`~/.local/state/atelier/logs`, on a background job. Exports include full commands and multiline
errors, oldest first, and never overwrite earlier files. The result's path or error is added to
the command log. Exporting does not refresh items.

Layout: below ~100 columns the main view is hidden (`+` shows it). On short terminals the focused side panel expands and the others collapse to their titles. Mouse: click to focus or select, wheel to scroll. Colours follow Catppuccin's style guide: the accent, mauve, marks navigation (the focused panel, the active tab, hint keys, section labels); colour marks state (success green, warnings yellow, errors red); the detail's keys are subtext, and what is secondary, placeholders such as `none`, counts of zero, commit hashes, dates and authors, is the subtle overlay. A filter is yellow. The README is drawn as Catppuccin's theme for glamour draws Markdown: rainbow headings, maroon code on mantle, blue links. Work rows show worktrunk's CI column as a `◆` coloured by status (passed green, running blue, failed red, conflicts yellow, changes requested pink, approval pending teal, `⚠` when it could not be fetched, dimmed when stale or a draft, nothing without CI), `↓N` when behind upstream, a spinner while `p` runs, and finished worktrees dimmed with `⊂` or `⊘`; `icons = "nerd"` swaps the row glyphs for Nerd Font icons. CI comes from the full refresh's `wt list --full`, and a fast refresh keeps the last one found. The main view's detail carries each row's colours and marks, and spells the CI out with its review and that review's decision; `o` on a worktree with a review opens the review.

### Keys

Lazygit defaults. The keymap is one table in code that also feeds `?` and the hint bar; it is not user-configurable.

| Keys | Action |
|---|---|
| `j` `k` `↑` `↓` · `,` `.` · `<` `>` `Home` `End` | item · page · top/bottom |
| `h` `l` `←` `→` `Tab` `S-Tab` · `1`–`4` · `0` | previous/next panel · jump · focus main view |
| `J` `K` `C-d` `C-u` `PgUp` `PgDn` · `H` `L` | scroll the main view from any panel · horizontally |
| `[` `]` | previous/next sub-tab |
| `Space` | open/focus tab · check out review · open an issue's linked work or start it · switch workspace |
| `Enter` | fold group header · focus main view on an item |
| `-` `=` | collapse/expand all |
| `n` | new worktree (menu with carnet when carnets are enabled) · new worktree for an issue · new workspace in Workspaces |
| `e` · `m` · `d` · `x` | edit group or repo alias · move to workspace or set a repo's workspace · remove worktree or workspace, forget repo (confirm) · close tab |
| `p` | `git pull --ff-only --prune` on the worktree, so on a main worktree it also refreshes which branches are gone; carnets are skipped |
| `f` | finish plan: the selection's groups in Work, a sweep in Workspaces, the linked work in Issues; `j` `k` move, `Space` toggles, `Enter` runs the checked lines, `Esc` cancels |
| `c` · `s` | close or reopen a carnet (in the hint bar while carnets are enabled) · search inside carnets with `rg` (Carnets sub-tab) |
| `g` | open `tool` (lazygit by default) on the worktree or carnet in Work or Carnets: a floating pane over the atelier tab inside zellij, the terminal outside; quitting returns to the same row |
| `o` · `y` `C-o` | open in browser · copy path/branch/URL via OSC 52 |
| `/` | substring filter on the focused panel |
| `R` · `?` · `+` `_` · `@` | refresh · actions menu · screen mode · toggle command log |
| `E` | export the retained command log to a new file |
| `Esc` · `q` `C-c` | back · quit |

`Space`, `x`, `d` and `p` on a group header act on every item in the group. `?` lists the focused panel's actions, then the global ones.

Each session runs its own TUI. One starts with Work focused and the Workspaces cursor on its own session, listed first, and `Space` on a workspace inside zellij leaves the TUI in that same state (clearing the Workspaces filter) as it switches away, so switching back into any session lands on its Work. Attaching from outside zellij leaves the TUI as it is.

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

- Unit tests: tab naming, issue key extraction, section rules, migrations (including a fixture of a baseline-schema DB), `update()`.
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
