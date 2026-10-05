# Refactor plan

A temporary working document: the architecture review of 2026-10-04 and the carnet redesign ([ADR 0001](adr/0001-carnet-folder-is-the-record.md)), split into steps. Each step is one session and one stacked pull request. The stack collects into `record-refactor-plan` (PR #15), which merges into `main` last. The last step deletes this file, so it never reaches `main`.

Vocabulary is in [CONTEXT.md](../CONTEXT.md) and the target behaviour in [design.md](design.md), which already describes the carnet redesign. Where this plan and `design.md` disagree, `design.md` wins; fix this plan.

## Running a step

1. Read `AGENTS.md`, `CONTEXT.md`, the step below, and the parts of `docs/design.md` it names. Take the first step whose status is `todo`.
2. Branch from the previous step's branch. If that step's PR is already merged into `record-refactor-plan`, branch from `record-refactor-plan` instead. Never branch from `main`.
   ```sh
   git fetch origin
   git switch -c <branch> origin/<base>
   ```
3. Implement the step. Behaviour not named in the step stays as it is. Keep `docs/design.md` in step with any behaviour that changes.
4. Write tests at the interface of the module the step creates. When they cover what older helper-level tests covered, delete the old ones rather than keeping both.
5. Run what CI runs, and fix everything it reports:
   ```sh
   cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test
   ```
6. In this file, set the step's status to `done (#<PR number>)`.
7. Commit: each message is one imperative subject line and nothing else. No body, no prefix, no trailers.
8. Push and open the PR against the base branch: `gh pr create --base <base> --title "Refactor <n>: <title>"`. The description says what changed and why, and ends there, with no footer.
9. Stop and report what was done, anything skipped, and anything for the next step.

## Merging the stack

- Merge the step PRs in order, each into its base branch, with a merge commit rather than a squash.
- Delete each head branch once it's merged. GitHub then retargets the next step's PR to `record-refactor-plan`, and its commits still apply cleanly.
- Once the step 6 PR, which deletes this file, is merged into `record-refactor-plan`, merge PR #15 into `main`. It can be squashed.

## Step 0 — Record the carnet redesign and this plan

- **Status:** done
- **Branch:** `record-refactor-plan`, from `main`
- `CONTEXT.md` (Carnet, Closed carnet, Group, Linked work), ADR 0001, `docs/design.md` (carnet redesign, keys `c` and `s`, the Carnets sub-tab, the new `carnet` CLI), and this file.

## Step 1 — Small fixes

- **Status:** done (#16)
- **Branch:** `refactor-small-fixes`, from `record-refactor-plan`
- **Files:** `src/state.rs`, `src/sync.rs`, `src/zellij.rs`, `src/cli.rs`, `src/tui/jobs.rs`, `src/tui/update.rs`

1. **Open the database once.** `State::from_connection` runs the pragmas, every migration and the default-workspace insert, and each TUI job calls it, every 10 s.
   - Add `State::connect(path, default_workspace)`. It opens the connection and sets `busy_timeout` and `foreign_keys`, but neither migrates nor inserts. WAL mode persists in the file.
   - `tui::jobs::Context::new` calls `State::open` once. `Context::state()` uses `connect`.
   - The CLI and the hooks keep `State::open`.
2. **Remove N+1 queries in `State`.**
   - `tab(path)`, `repo_by_path` and `has_workspace` query one row with `WHERE`.
   - `items`, `carnets` and `repo_items` select every column in one query, through one shared row-to-`Item` function, instead of `SELECT path` followed by `require_item` per row.
3. **Drop `DEFAULT 'vrac'`.** It sits on `items.workspace` in migration 1 (`state.rs`). New databases then have no default. Existing ones are untouched, and code always writes the column. The baseline fixture test must still pass.
4. **Forget unlisted worktrees.** In `sync::sync`, when a repo's `wt list` succeeds, forget its worktree items that the listing doesn't name, even when their folder still exists. Repos whose listing failed keep their items, as today. Carnets keep today's rule: forgotten when the folder is gone. Add a test.
5. **Keep commits across fast refreshes.** `Action::Loaded` clears `model.commits` and `model.readmes` on every refresh.
   - Instead, drop only the commits of worktrees whose `tree.short_sha` changed between the old and new snapshot, plus the entries of items no longer listed.
   - Clear both maps on a full refresh.
   - Add a test in `update.rs`.
6. **Reconcile once per job.** `Zellij::open_tab` and `close_repo_tabs` call `reconcile` each time, so `Job::Open` with n paths reconciles n+1 times. Give `Zellij` a `reconciled: Cell<bool>` so `reconcile` runs at most once per `Zellij` value. A `Zellij` lives for one job, hook or CLI command.
7. **Fix the alias bug.** `atelier update --alias` doesn't rename open tabs. Call `sync_names` after `update_repo` when an alias is given, as the TUI does. Step 2 moves this into the Items module.
8. **Delete the dead arm.** `Cmd::Jump` with no such panel notes "that panel comes in a later phase" (`update.rs`); do nothing instead.

**Done when** CI passes and each fix has a test where one is practical (1, 2 and 3 are covered by the existing tests).

## Step 2 — Items module

- **Status:** done (#17)
- **Branch:** `refactor-items-module`, from `refactor-small-fixes`
- **Files:** new `src/items.rs` and `src/git.rs`; `src/tui/jobs.rs`, `src/tui/app.rs`, `src/hooks.rs`, `src/cli.rs`, `src/sync.rs`, `src/worktrunk.rs`, `src/zellij.rs`, `src/process.rs`, `src/carnet.rs`, `src/state.rs`

**Why.** The Item operations (create, switch, start, checkout, move, regroup, remove, forget) are private to `tui/jobs.rs`. The CLI can't reach them, and the rule for which workspace and group a new Item gets is written in six places.

**What moves.**
1. **`src/items.rs`.** A core module, `pub struct Items<'a>`, built from `&State`, `&dyn Runner`, `&Config` and the current session. It has one verb per Item operation, each moved from `jobs::execute` and its helpers:
   - `open(paths)`, `close(paths)`, `pull(paths)` (skips carnets)
   - `create(repo, branch, workspace, group) -> PathBuf`
   - `checkout(repo, workspace, &Review)`
   - `start(repo, branch, workspace, issue_key)`
   - `remove(&[Removal])`, `move_to(paths, workspace)`, `regroup(paths, group)`
   - `set_alias(repo, alias)`, `set_repo_workspace(repo, workspace)`, `forget_repo(repo)`
   - `add_workspace(name)`, `remove_workspace(name, carnets)`
   - `snapshot(full) -> (Snapshot, Vec<Logged>)`: today's `jobs::load` plus `sync::sync`. `sync.rs` folds into `items.rs`.
   - `create_carnet(name, workspace, group) -> PathBuf`
   - for the hooks: `record(path, repo, branch, workspace, group) -> Option<Tab>` (`None` for a carnet's worktree) and `forget(path)`
   - Verbs over several paths keep today's behaviour: run every path, then join the errors.
2. **Core types.** `Snapshot`, `Work`, `WorkKind`, `Removal` and `RemovedWorktree` move from `tui/app.rs` into `items.rs`, and `app.rs` re-exports them.
   - `Removal::of(&Work)` replaces the construction in `update.rs::remove`.
   - The kind questions (`is_carnet`, `repo`, `tree`, `branch`, `removable`, `title`) stay as methods on `Work`.
3. **One placement rule**, owned by `Items`:
   - **Workspace:** `Items::workspace(explicit, fallback)` returns `explicit` when it is a known workspace, else the current session when known, else `fallback`.
     - The hooks call it with `ATELIER_WORKSPACE` and the repo's default workspace.
     - `carnet new` calls it with `--workspace` (still required to exist) and the default workspace.
     - Sync keeps the repo's default workspace for worktrees it finds itself.
   - **Group:**
     - `create`: the ticket key in the branch, else the `group` argument verbatim (a selected group, which may be a GitHub key or a hand-set label).
     - `checkout`: the key in the branch, else in the review's title. This moves out of `jobs::execute`.
     - `start`: always the issue key. If the worktree already existed with another group, set it and rename its tabs.
4. **The hook takes the group verbatim.**
   - Replace `ATELIER_GROUP_HINT` with `ATELIER_GROUP`. Atelier sets it to the final group when it runs `wt switch`.
   - In `hooks::handle`, a set `ATELIER_GROUP` is the group as-is. Otherwise the group comes from the ticket key in the branch.
   - `Hints.group` becomes `Option<String>`.
   - This is what keeps GitHub keys like `repo#12`.
   - Update `docs/design.md` (the worktrunk bullet) and the tests that spell out the `env … wt switch` command.
5. **worktrunk and git commands get owners.**
   - `worktrunk.rs` gains `switch(runner, repo, target, workspace, group)` (the `env ATELIER_WORKSPACE=… ATELIER_GROUP=… wt -C … switch … --no-cd --yes` command) and `remove(runner, repo, target, force)`.
   - New `git.rs` holds the git commands now spread around:
     - `branch_exists`, `pull_ff_only`, `log` from `jobs.rs`
     - `branch` from `process.rs`
     - `main_worktree` from `cli.rs`
     - `init` from `carnet.rs`
6. **Zellij stops carrying the runner.**
   - `Zellij::new(runner, &config, layouts)` replaces the four hand-built `Zellij { … }` values (`cli.rs`, `jobs.rs`, and the tests in `jobs.rs`, `zellij.rs` and `hooks.rs`).
   - Its fields become private, with `here()` as an accessor. That includes `reconciled`, added in step 1 as a public field that every literal sets.
   - Callers that ran other commands through `zellij.runner` use their own runner.
   - `name_for` and `open_siblings` ask `item.is_carnet()` instead of treating `item.repo == None` as "carnet".
7. **Callers become thin.**
   - `jobs::execute` builds `Items` and maps each `Job` to one verb.
   - `hooks::handle` parses the payload and calls `record` or `forget`.
   - The CLI's `rm`, `update` (alias and workspace), `ws rm` and `carnet new` call the matching verbs.
   - `remove_repo` takes `&self`, through `unchecked_transaction`, so nothing needs `&mut State`.

**Tests.**
- Move the helper tests from `tui/jobs.rs` to `items.rs`, driven through `Items` with `process::fake::Fake` and an in-memory `State`. Delete them from `jobs.rs`.
- Add tests for the untested verbs:
  - `move_to` reopens a tab only if one was open
  - `regroup` renames tabs and keeps a GitHub key
  - `forget_repo` closes its tabs
  - `set_alias` renames tabs, which covers the CLI bug
  - the workspace placement rule
  - a hook test where `ATELIER_GROUP=atelier#14` is kept
- `update.rs` tests stay as they are.

**Done when** `tui/jobs.rs` holds no orchestration (only `Context`, `run`, the listing fetches and `execute` as a dispatch), `sync.rs` is gone, and CI passes.

## Step 3 — The carnet folder is the record

- **Status:** done (#18)
- **Branch:** `refactor-carnet-record`, from `refactor-items-module`
- **Files:** `src/carnet.rs`, `src/items.rs`, `src/state.rs`, `src/cli.rs`, `src/tui/{app,update,view,jobs}.rs`, `src/git.rs`
- **Since step 2:** carnet operations are `Items` verbs (`create_carnet` is there already), the hooks' "a carnet is never a repo" check lives in `Items::record`, and git commands belong in `git.rs`.
- **Spec:** the Carnets bullet and CLI scope in `docs/design.md`, and ADR 0001.

1. **Read and write carnets in `carnet.rs`.**
   - `pub struct Carnet { path, name, date, tickets: Vec<String>, closed: bool, summary: String }`
   - `scan(root, &Names) -> Vec<Carnet>`: every directory directly under `root` named `YYYY-MM-DD-…` that has a `.git`. Others are ignored.
   - **Front matter:** the README begins with a `+++` line and the block ends at the next `+++` line. Parse and edit it with `toml_edit` (already a dependency), keeping other keys, comments and the README body intact.
     - Keys: `tickets` (array of strings), `closed` (bool), `summary` (string). All are optional.
     - Without `tickets`, the key right after the date (`Names::group`) is the one ticket.
     - A README without front matter, or a missing README, reads as empty and gets a block when first edited.
   - **Edits:** `set_first_ticket(path, key)`, where an empty key removes the first ticket and keeps the rest, and `set_closed(path, bool)`.
     - Each edit writes the README, then runs `git add README.md` and `git commit -m <message> -- README.md`, through new `git::add` and `git::commit` helpers.
     - Messages: `Link <key>`, `Unlink <key>`, `Close`, `Reopen`.
   - `create`:
     - Folder: `<date>-<name in kebab case>`, keeping a key typed at the start of the name.
     - README: `# <name>`, under front matter whose `tickets` holds the typed key, else the selected group when it is a ticket key (`Names` pattern) or a GitHub issue key (`repo#12`, `owner/repo#12`).
     - Then `git::init`, and a first commit, `Create carnet`, of the README.
     - It still records the carnet's row in the workspace `Items::create_carnet` is given, so a new carnet does not wait for the scan to be placed.
   - `add` is deleted.
2. **Items scans the root on each snapshot**, when carnets are enabled.
   - Each carnet found gets an `items` row, kind `carnet`: an existing row keeps its workspace, a new one goes to the default workspace. Its `group_key` is set to its first ticket, as a cache that tab names and grouping read.
   - Carnet rows not found by the scan are deleted, with their tabs. This replaces the carnet rule in `Items::sync` (forgotten when the folder is gone).
   - When carnets are disabled, carnet rows are left alone and none are listed.
3. **Snapshot.**
   - `WorkKind::Carnet` carries `tickets`, `closed` and `summary`.
   - `Snapshot.work` holds open carnets only.
   - `Snapshot.carnets` holds every carnet with its workspace and tab, for step 5. `Snapshot.all_carnets` is deleted.
   - Since step 1, a fast refresh keeps the loaded READMEs and commits, so a carnet's stay stale until the next full refresh. The scan already reads each README: when a carnet's README differs from the loaded one, `Action::Loaded` drops that carnet's README and commits.
4. **Verbs.**
   - `regroup` on a carnet calls `set_first_ticket` and renames its tab. `Items::regroup` already renames a carnet's tab on its own; worktrees keep today's path.
   - New `set_carnets_closed(paths, closed)`; closing also closes the tab.
   - The TUI's `c` key (new `Cmd::CloseCarnet`) on a Work carnet row, or on a group header for its carnets, runs `Job::CloseCarnet(paths)`.
   - `d` never touches a carnet: `Removal` describes only worktrees, and `Removal::of` returns `None` for carnets and main worktrees.
5. **Workspace removal moves carnets.** It no longer forgets them:
   - `State::remove_workspace` moves the workspace's carnets to the default workspace. Worktrees still block it.
   - Delete `--forget-carnets`, `State::workspace_carnets`, the `carnets` field of `Job::RemoveWorkspace`, the `carnets` argument of `Items::remove_workspace` and `State::remove_workspace`, and the carnet lines in the TUI confirmation.
6. **Linked work.** `Model::issue_work` also returns carnets whose `tickets` contain the issue key.
7. **README view.** Strip the front matter before rendering. The carnet detail shows its tickets and summary.
8. **CLI** (`atelier carnet …`):
   - `ls [--closed]` prints one line per carnet, newest first: `<folder name>\t<tickets, comma-separated>\t<summary>`.
   - `search <text>` runs `rg --line-number --ignore-case --fixed-strings -- <text> <root>` interactively, so its output goes to the terminal. When `rg` is not on `PATH`, it fails with "carnet search needs ripgrep (rg) on PATH". Put the `rg` arguments and that message in `carnet.rs`, so step 5's search reuses them.
   - `path [KEY]` prints the path of the newest open carnet whose `tickets` contain `KEY`.
     - Without `KEY`, it uses the group of the recorded item containing the current directory: the longest recorded path that is a prefix of the canonical current directory. Inside a carnet, that is the carnet itself.
     - With no match it prints nothing and exits non-zero. The message says which case applied: unknown directory, no group, or no open carnet for the key. The last also suggests `atelier carnet new`.
     - It runs no git or network command.

**Tests** in `carnet.rs` and `items.rs`:
- the scan: dated git folder, undated folder, non-git folder, front matter, fallback key
- a front matter round trip that keeps unknown keys and the body
- each edit's git calls, through `Fake`
- `create`'s files and calls
- workspace removal moving carnets
- every `carnet path` case
- a carnet listing an issue's key showing as that issue's linked work

**Done when** `carnet add` and every carnet "forget" path are gone, `docs/design.md` matches, and CI passes.

## Step 4 — One module per list

- **Status:** done (#19)
- **Branch:** `refactor-list-modules`, from `refactor-carnet-record`
- **Files:** new `src/tui/lists/{mod,workspaces,repos,work,reviews,issues}.rs`; `src/tui/{app,update,view}.rs`
- **Since step 3:** Work has one more operation, `c` (`Cmd::CloseCarnet`, today an arm in `update::command`), which belongs in `work.rs` like `d`, `x` and `p`. Work's `remove` builds `Removal::of(work)`, which skips carnets and main worktrees. The Work rows hold open carnets only; closed ones are in `Snapshot.carnets`.

**Why.** Each operation (`activate`, `new`, `edit`, `move_to`, `remove`, `copy_path`, `branch`, `url`, `listed_ids`, `len`, `title`, `rows`, `detail`) matches on every `List`, about 17 match sites over 3 files, so a new list touches all of them. Step 5 adds one.

1. **One trait per list.** `lists/mod.rs` defines a trait, one implementation per list kind, and `fn of(list: List) -> &'static dyn ListKind`, the only `match` on `List` that picks behaviour.
   - Methods take the `List` value, so the issue sections share one implementation.
   - Operations a list doesn't support default to doing nothing.
   - A starting point; adjust it as the code requires:
   ```rust
   pub trait ListKind: Sync {
       fn title<'a>(&self, model: &'a Model, list: List) -> &'a str;
       fn len(&self, model: &Model, list: List) -> usize;
       /// Each row's identity, which keeps the selection across a refresh.
       fn ids(&self, model: &Model, list: List) -> Vec<String>;
       fn rows<'a>(&self, model: &'a Model, palette: &Palette, list: List) -> Vec<Line<'a>>;
       fn detail(&self, model: &Model, list: List) -> Vec<(String, String)>;
       fn activate(&self, model: &mut Model, list: List) -> Vec<Effect> { Vec::new() }
       fn new(&self, model: &mut Model, list: List) -> Vec<Effect> { Vec::new() }
       fn edit(&self, model: &mut Model, list: List) -> Vec<Effect> { Vec::new() }
       fn move_to(&self, model: &mut Model, list: List) -> Vec<Effect> { Vec::new() }
       fn remove(&self, model: &mut Model, list: List) -> Vec<Effect> { Vec::new() }
       fn copy_path(&self, model: &Model, list: List) -> Option<String> { None }
       fn branch(&self, model: &Model, list: List) -> Option<String> { None }
       fn url(&self, model: &Model, list: List) -> Option<String> { None }
   }
   ```
2. **Each list's code moves into its module.**
   - `work.rs` takes `work_rows`, folding, `CARNETS_KEY`, `targets`, `work_row`, and the Work arms of each operation.
   - `issues.rs` takes `ask_start`, `issue_work` and the section filtering.
   - `reviews.rs` takes `project_repo`, `review_project` and `review_work`.
   - `Keep` uses `ids` instead of `listed_ids` and `line_key`, and keeps every list's selection the same way.
3. **`update.rs` and `view.rs` dispatch.**
   - `command` and `view::render_panel` call through `lists::of(model.active())` or `lists::of(list)`.
   - Modals, prompts, filters, scrolling, mouse handling and the refresh cadence stay in `update.rs`.

**Tests.**
- The `update.rs` key-press tests are the interface and must pass unchanged.
- The insta snapshots must not change. Do not accept new snapshots.
- Add direct unit tests for `work.rs` grouping:
  - named groups first, then ungrouped worktrees, then the `Carnets` group
  - the main worktree first within a repo
  - carnets newest first
  - the `Carnets` group folded by default

**Done when** no operation outside `lists/` matches on `List` (except `Panel::tabs` and `lists::of`), and CI passes.

## Step 5 — Carnets sub-tab

- **Status:** todo
- **Branch:** `refactor-carnets-subtab`, from `refactor-list-modules`
- **Files:** new `src/tui/lists/carnets.rs`; `src/tui/app.rs` (`List::Carnets`, `Panel::tabs`, keymap), `src/tui/jobs.rs`, `src/tui/view.rs`, `src/tui/update.rs`, `src/carnet.rs`
- **Spec:** the panel 2 row and keys table in `docs/design.md`.
- **Since step 3:**
  - `Snapshot.carnets` is a `Vec<Work>`, already newest first. Read a carnet's `tickets`, `closed`, `summary` and `readme` from `WorkKind::Carnet`, or through `Work::tickets()` and `Work::closed()`.
  - Closing and reopening is one verb, `Items::set_carnets_closed(paths, closed)`. Only `Job::CloseCarnet(paths)` exists; add the reopen job.
  - `c` is bound to `Cmd::CloseCarnet` for Work, with `hint: NONE`, so it is not yet in the hint bar.
  - `carnet::search_args`, `carnet::search`, `carnet::on_path` and `carnet::NO_RIPGREP` are the CLI's search. Step 5 adds `--no-heading` for its own search.
  - `carnet::body(readme)` strips the front matter for rendering.
- **Since step 4:**
  - Add `List::Carnets` to `Panel::tabs` and `lists::of`, and a new `lists/carnets.rs` implementing `ListKind`; add `Kind::Carnets`, which its `kind()` returns.
  - The trait holds only what every list can have. Its `new` is called `create`, and beyond the plan's sketch it has `kind`, `empty` (the empty-list message) and `enter`. Keys that only some lists act on (`x`, `c`, `p`) go to `ListKind::command(model, list, cmd)`, which Work matches on and the others ignore. The Carnets list handles `c` there as the toggle; rename `Cmd::CloseCarnet` to match, since it no longer only closes. `s` goes through `command` too.
  - A list's `Model` accessors live in its module, in an `impl Model` block, as `work.rs` does for `work_rows` and `targets`. `Keep` restores each list's selection through `ids`, so the Carnets list only needs a stable one, its path.
  - The main view's README and commits come from `work::selected`, and `update::commits` fetches them for the Work row only. The Carnets list needs the same for its selected carnet.

1. **Panel 2 becomes `Work │ Carnets`.** The sub-tab exists only while carnets are enabled.
2. **Rows:** every carnet in `Snapshot.carnets`, across all workspaces, newest first. Each row shows its date and name, its tickets, a closed marker, and its summary.
3. **`/`** matches folder name, tickets and summary.
4. **`s`** prompts for text and runs `Job::SearchCarnets(text)`.
   - The job runs `rg --line-number --ignore-case --fixed-strings --no-heading -- <text> <root>` and groups the hits by carnet folder. The search and the grouping are a function in `carnet.rs`, next to step 3's CLI search. `jobs::run` handles the job like `Job::Commits`, as a listing that reports an action, not through `execute`.
   - The sub-tab then lists only carnets with hits, and the main view shows the selected carnet's hit lines above its README.
   - `Esc` clears the search.
   - A missing `rg` goes to the command log with the same message as the CLI.
5. **Keys.**
   - `Space` opens the carnet's tab. A closed carnet stays closed.
   - `c` closes an open carnet or reopens a closed one. Add `Job::ReopenCarnet(paths)`, run through `Items::set_carnets_closed(paths, false)`.
   - `y` / `C-o` copy its path.
   - The detail view is the same as for a carnet in Work.
6. **Keymap.** Add `List::Carnets` to the keymap's `Kind` tables:
   - `s` is bound for Carnets only.
   - `c` is bound for Work and Carnets.
   - Both appear in the hint bar and in `?`.

**Tests:**
- key-press tests: `Space` on a closed carnet emits `Job::Open` and no close/reopen; `c` toggles; `s` asks, then emits the search job; a search result narrows the list; `Esc` restores it
- an insta snapshot of the sub-tab with one closed and one open carnet

## Step 6 — Remote listing pipeline and refresh schedule

- **Status:** todo
- **Branch:** `refactor-remote-listings`, from `refactor-carnets-subtab`
- **Files:** new `src/tui/schedule.rs`; `src/tui/{app,update,jobs,view}.rs`, `src/reviews.rs`, `src/issues.rs`

**Why.** Reviews and Issues run through two copies of one pipeline: two `Job` variants, two `Action` variants, two update arms, two `fetch_*` functions, two job loops and two due flags. The refresh cadence is spread over Model fields that tests poke directly.

1. **One pipeline.**
   - `enum Feed { Reviews(Provider), Issues(Tracker) }`
   - `Job::Fetch { feed, keys: Vec<String>, force }`, where the keys are hosts or scopes
   - `Action::Fetched { feed, rows: Result<Rows, String>, log }`, with `enum Rows { Reviews(Vec<Review>), Issues(Vec<Issue>) }`
   - One job function loops over the keys, calls that family's `fetch`, and collects `fetch_failures`.
   - One update arm replaces the feed's rows (reviews sorted by `updated_at`, issues by tracker), keeping the selection.
   - `Source` becomes `Wt`, `Git`, `Feed(Feed)`, `Run`.
   - `reviews.rs` and `issues.rs` keep their adapters and parsing.
2. **`schedule.rs`** owns `loading`, `pulling`, `idle`, `since_refresh`, `since_full` and a `Due` per `Feed`, moved out of `Model`.
   - Methods: `tick()`, `input()`, `refresh(force)`, `started(&Job)`, `finished(Source)`, `is_loading(Source)`. They return the jobs to start.
   - Today's rules stay:
     - fast refresh every 10 s after 10 s idle
     - full refresh every 5 min
     - `R` fetches past the cache
     - a feed still listing keeps its turn
   - New rules:
     - **Issues** are due on a full refresh and start at once, without waiting for `Action::Loaded`.
     - **Reviews** still wait for the snapshot that names their hosts.
     - **Due per feed:** a busy feed keeps only its own due flag, so the idle one isn't force-fetched again.
     - **Refresh after a job only when it changes something:** `Action::Finished` refreshes only when the job changes items, workspaces, repos or tabs. Not after `Browse` or `SwitchWorkspace`.
     - **No stacked refreshes:** when a refresh is already running, mark one pending and start it when the current one finishes.
3. **Tests.**
   - Unit tests for `schedule.rs` cover each rule above.
   - `update.rs` tests stop poking `model.loading`, `since_full` and `*_due`. Give the test helpers a `finish_all` that feeds `finished` for each started job.
4. **Finish the plan.** Update `docs/design.md` (the Refresh and Issues bullets), and delete this file.

**Done when** there is one `Job`, `Action` and update arm for remote listings, no cadence field left on `Model`, this file is gone, and CI passes.
