//! The lazygit-style TUI: an Elm loop over terminal events, timers and job results.

mod app;
mod jobs;
mod lists;
mod markdown;
mod schedule;
pub mod statusline;
mod update;
mod view;
mod widgets;

use std::io::Write;
use std::sync::Arc;
use std::time::Duration;

use color_eyre::eyre::Result;
use crossterm::event::{DisableMouseCapture, EnableMouseCapture, Event, EventStream, KeyEventKind};
use futures::StreamExt;
use tokio::sync::mpsc;

use crate::config::Config;
use crate::zellij;
use app::{Action, Effect, Job, Model};
use jobs::Context;
use view::Palette;

pub fn run(config: Config) -> Result<()> {
    let palette = Palette::new(config.flavor(), config.icons);
    let context = Arc::new(Context::new(config)?);
    tokio::runtime::Runtime::new()?.block_on(event_loop(context, palette))
}

async fn event_loop(context: Arc<Context>, palette: Palette) -> Result<()> {
    let mut terminal = ratatui::init();
    crossterm::execute!(std::io::stdout(), EnableMouseCapture)?;
    let result = drive(&mut terminal, context, palette).await;
    crossterm::execute!(std::io::stdout(), DisableMouseCapture)?;
    ratatui::restore();
    result
}

async fn drive(
    terminal: &mut ratatui::DefaultTerminal,
    context: Arc<Context>,
    palette: Palette,
) -> Result<()> {
    let size = terminal.size()?;
    let mut model = Model::new((size.width, size.height));
    model.tracker_config = context.config.tracker.clone();
    model.review_config = context.config.reviews.clone();
    model.carnets = context.config.carnets_enabled();
    let (sender, mut results) = mpsc::unbounded_channel::<Action>();
    let mut events = EventStream::new();
    let mut ticks = tokio::time::interval(Duration::from_secs(1));
    let mut effects = update::update(&mut model, Action::Run(Job::Refresh { full: false }));
    let mut dirty = true;
    loop {
        for effect in effects.drain(..) {
            match effect {
                Effect::Run(job) => {
                    let context = context.clone();
                    let sender = sender.clone();
                    tokio::task::spawn_blocking(move || {
                        let _ = sender.send(jobs::run(&context, job));
                    });
                }
                Effect::Attach(session) => {
                    let log = handed_over(terminal, || jobs::attach(&context, &session))?;
                    model.push_log(log);
                    dirty = true;
                }
                Effect::Tool { path, branch } if zellij::current_session().is_some() => {
                    let context = context.clone();
                    let sender = sender.clone();
                    tokio::task::spawn_blocking(move || {
                        let log = jobs::tool(&context, &path, branch);
                        let _ = sender.send(Action::Logged(log));
                    });
                }
                Effect::Tool { path, branch } => {
                    let log = handed_over(terminal, || jobs::tool(&context, &path, branch))?;
                    model.push_log(log);
                    dirty = true;
                }
                Effect::Copy(text) => {
                    let mut stdout = std::io::stdout();
                    write!(stdout, "\x1b]52;c;{}\x07", base64(text.as_bytes()))?;
                    stdout.flush()?;
                }
                Effect::Quit => return Ok(()),
            }
        }
        if dirty {
            terminal.draw(|frame| view::render(frame, &model, &palette))?;
        }
        let action = tokio::select! {
            event = events.next() => match event {
                Some(Ok(Event::Key(key))) if key.kind != KeyEventKind::Release => Action::Key(key),
                Some(Ok(Event::Mouse(mouse))) => Action::Mouse(mouse),
                Some(Ok(Event::Resize(width, height))) => Action::Resize(width, height),
                Some(Ok(_)) => continue,
                Some(Err(err)) => return Err(err.into()),
                None => return Ok(()),
            },
            Some(action) = results.recv() => action,
            _ = ticks.tick() => Action::Tick,
        };
        // A quiet tick changes nothing on screen, unless a spinner is turning.
        let tick = action == Action::Tick;
        effects = update::update(&mut model, action);
        dirty = !tick || !effects.is_empty() || model.animating();
    }
}

/// Runs `run` with the terminal handed over, then takes it back.
fn handed_over<T>(terminal: &mut ratatui::DefaultTerminal, run: impl FnOnce() -> T) -> Result<T> {
    crossterm::execute!(std::io::stdout(), DisableMouseCapture)?;
    ratatui::restore();
    let result = run();
    *terminal = ratatui::init();
    crossterm::execute!(std::io::stdout(), EnableMouseCapture)?;
    terminal.clear()?;
    Ok(result)
}

/// Standard base64, for OSC 52.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, &b)| n | (b as u32) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::app::{Action, Feed, Model, Rows, WorkKind};
    use super::update::update;
    use super::*;
    use crate::config::Icons;
    use crate::git::Commit;
    use crate::links::tests::key as issue_key;

    #[test]
    fn base64_pads() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"/home/r"), "L2hvbWUvcg==");
    }

    fn render(model: &Model, width: u16, height: u16) -> String {
        render_with(model, width, height, Icons::Unicode)
    }

    fn render_with(model: &Model, width: u16, height: u16, icons: Icons) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        let palette = Palette::new(catppuccin::PALETTE.mocha, icons);
        terminal
            .draw(|frame| view::render(frame, model, &palette))
            .unwrap();
        terminal.backend().to_string()
    }

    fn loaded(width: u16, height: u16) -> Model {
        let mut model = update::tests::with_reviews(update::tests::model());
        model.size = (width, height);
        let path = model.snapshot.work[1].path().clone();
        model.snapshot.work[1].tab = true;
        model.snapshot.work[1].tree_mut().dirty = true;
        model.snapshot.work[1].tree_mut().symbols = "!".into();
        model.snapshot.work[0].tree_mut().upstream = Some((0, 3));
        update(&mut model, Action::Key(key('j')));
        update(
            &mut model,
            Action::Commits(path, vec![Commit::fake("abc1234", "Add login")]),
        );
        model.schedule.finish_all();
        model.log.push(crate::process::Logged {
            command: "git -C /src/api pull --ff-only".into(),
            error: None,
        });
        model
    }

    fn key(c: char) -> crossterm::event::KeyEvent {
        crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char(c),
            crossterm::event::KeyModifiers::NONE,
        )
    }

    #[test]
    fn main_layout() {
        insta::assert_snapshot!(render(&loaded(120, 30), 120, 30));
    }

    #[test]
    fn nerd_icons_and_a_pull_in_flight() {
        let mut model = loaded(120, 16);
        update(&mut model, Action::Key(key('p')));
        insta::assert_snapshot!(render_with(&model, 120, 16, Icons::Nerd));
    }

    #[test]
    fn narrow_terminal_hides_the_main_view() {
        insta::assert_snapshot!(render(&loaded(80, 30), 80, 30));
    }

    #[test]
    fn short_terminal_folds_the_other_panels() {
        insta::assert_snapshot!(render(&loaded(120, 16), 120, 16));
    }

    #[test]
    fn reviews_panel() {
        let mut model = loaded(120, 30);
        model.snapshot.work[0].tree_mut().branch = Some("change-2".into());
        model.snapshot.work[0].links.group = crate::links::Group::parse("login");
        link_reviews(&mut model, "ABC-1");
        update(&mut model, Action::Key(key('3')));
        insta::assert_snapshot!(render(&model, 120, 30));
    }

    /// Both of api's to-review list's reviews link `key`; the last is api#2.
    fn link_reviews(model: &mut Model, key: &str) {
        for review in model.reviews.iter_mut().filter(|review| review.number < 3) {
            review.issue_keys.push(issue_key(key));
        }
    }

    #[test]
    fn issues_panel() {
        let mut model = update::tests::with_issues(loaded(120, 30));
        model.snapshot.work[0].tree_mut().branch = Some("change-2".into());
        link_reviews(&mut model, "ABC-1");
        update(&mut model, Action::Key(key('4')));
        update(&mut model, Action::Key(key(']')));
        update(&mut model, Action::Key(key('j')));
        insta::assert_snapshot!(render(&model, 120, 30));
    }

    /// The recorded GitHub issues of this repo through the label scheme docs/design.md shows.
    #[test]
    fn issues_panel_from_github() {
        use crate::issues::{parse_gh, tests};
        let mut model = loaded(120, 30);
        model.tracker_config = crate::config::Config::parse(tests::SCHEME).unwrap().tracker;
        let mut issues = parse_gh(tests::GH).unwrap();
        issues.extend(parse_gh(tests::GH_CLOSED).unwrap());
        update(
            &mut model,
            Action::Fetched {
                feed: Feed::Issues(crate::issues::Tracker::GitHub),
                rows: Ok(Rows::Issues(issues)),
                log: Vec::new(),
            },
        );
        // Backlog, half screen so every section fits the title.
        for c in ['4', '[', '+'] {
            update(&mut model, Action::Key(key(c)));
        }
        insta::assert_snapshot!(render(&model, 120, 30));
    }

    #[test]
    fn carnet_shows_its_rendered_readme() {
        let mut model = update::tests::with_carnets(loaded(120, 30));
        for key in [key('>'), key('k')] {
            update(&mut model, Action::Key(key));
        }
        let text =
            "+++\nsummary = \"hidden\"\n+++\n# 2026-10-02-ideas\n\nWhat I found **so far**.\n";
        let readme = app::Readme {
            path: "/data/2026-10-02-ideas".into(),
            stamp: None,
            text: Some(text.into()),
        };
        update(&mut model, Action::Readme(readme));
        model.schedule.finish_all();
        insta::assert_snapshot!(render(&model, 120, 30));
    }

    /// The style of the first cell of `text`'s first occurrence on screen.
    fn style_of(buffer: &ratatui::buffer::Buffer, text: &str) -> ratatui::style::Style {
        let width = buffer.area.width as usize;
        let chars: Vec<char> = text.chars().collect();
        let cells = &buffer.content;
        let at = (0..cells.len())
            .find(|&start| {
                start % width + chars.len() <= width
                    && (chars.iter().enumerate())
                        .all(|(i, c)| cells[start + i].symbol() == c.to_string())
            })
            .unwrap_or_else(|| panic!("{text:?} is not on screen"));
        cells[at].style()
    }

    /// The style of the cell `offset` characters into `text`'s first occurrence on screen,
    /// before it when negative.
    fn style_in(
        buffer: &ratatui::buffer::Buffer,
        text: &str,
        offset: isize,
    ) -> ratatui::style::Style {
        let width = buffer.area.width as usize;
        let chars: Vec<char> = text.chars().collect();
        let cells = &buffer.content;
        let at = (0..cells.len())
            .find(|&start| {
                start % width + chars.len() <= width
                    && (chars.iter().enumerate())
                        .all(|(i, c)| cells[start + i].symbol() == c.to_string())
            })
            .unwrap_or_else(|| panic!("{text:?} is not on screen"));
        cells[at.checked_add_signed(offset).unwrap()].style()
    }

    #[test]
    fn groups_and_issue_keys_are_coloured_apart() {
        let palette = Palette::new(catppuccin::PALETTE.mocha, Icons::Unicode);
        assert_ne!(palette.group, palette.issue_key);
        let model = loaded(120, 30);
        let buffer = draw(&model, &palette);
        assert_eq!(
            style_in(&buffer, "▾ ABC-1", 2).fg,
            Some(palette.group),
            "the header"
        );
        let group = "Group       ABC-1";
        assert_eq!(style_in(&buffer, group, 12).fg, Some(palette.group));
        let keys = "Issue keys  ABC-1";
        assert_eq!(style_in(&buffer, keys, 12).fg, Some(palette.issue_key));
        let mut model = update::tests::with_issues(loaded(120, 30));
        for c in ['4', ']'] {
            update(&mut model, Action::Key(key(c)));
        }
        let buffer = draw(&model, &palette);
        let linked = "ABC-1 Issue ABC-1";
        assert_eq!(
            style_in(&buffer, linked, -2).fg,
            Some(palette.issue_key),
            "the linked-work marker"
        );
        assert_eq!(
            style_in(&buffer, linked, 0).fg,
            Some(palette.dim),
            "the issue's own key, subtle as any"
        );
    }

    fn draw(model: &Model, palette: &Palette) -> ratatui::buffer::Buffer {
        let (width, height) = model.size;
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| view::render(frame, model, palette))
            .unwrap();
        terminal.backend().buffer().clone()
    }

    #[test]
    fn the_readme_reads_in_catppuccin_under_its_label() {
        use ratatui::style::{Color, Modifier};
        let mut model = update::tests::with_carnets(loaded(120, 30));
        for key in [key('>'), key('k')] {
            update(&mut model, Action::Key(key));
        }
        let text = "# 2026-10-02-ideas\n\nWhat I found **so far**, in `notes.md`.\n";
        let readme = app::Readme {
            path: "/data/2026-10-02-ideas".into(),
            stamp: None,
            text: Some(text.into()),
        };
        update(&mut model, Action::Readme(readme));
        model.schedule.finish_all();
        let palette = Palette::new(catppuccin::PALETTE.mocha, Icons::Unicode);
        let colors = catppuccin::PALETTE.mocha.colors;
        let buffer = draw(&model, &palette);
        let label = style_of(&buffer, "README");
        assert_eq!(label.fg, Some(palette.accent));
        assert!(label.add_modifier.contains(Modifier::BOLD));
        let heading = style_of(&buffer, "# 2026-10-02-ideas");
        assert_eq!(heading.fg, Some(colors.red.into()), "the rainbow's first");
        assert_eq!(heading.bg, Some(Color::Reset), "no background");
        assert_eq!(style_of(&buffer, "What").fg, Some(palette.text));
        let code = style_of(&buffer, "notes.md");
        assert_eq!(code.fg, Some(colors.maroon.into()));
        assert_eq!(code.bg, Some(colors.mantle.into()));
        assert_eq!(style_of(&buffer, "Issue keys").fg, Some(palette.label));
        assert_eq!(
            style_of(&buffer, "Issue keys  none").fg,
            Some(palette.label),
            "an empty value reads none"
        );
        assert_eq!(style_of(&buffer, "none").fg, Some(palette.dim));
    }

    #[test]
    fn commits_and_counts_of_zero_recede_and_warnings_are_yellow() {
        let mut model = loaded(120, 30);
        model.snapshot.work[1].tree_mut().upstream = Some((2, 0));
        let palette = Palette::new(catppuccin::PALETTE.mocha, Icons::Unicode);
        let buffer = draw(&model, &palette);
        assert_eq!(palette.warn, catppuccin::PALETTE.mocha.colors.yellow.into());
        assert_eq!(
            style_of(&buffer, "↓3").fg,
            Some(palette.warn),
            "main is behind"
        );
        assert_eq!(style_of(&buffer, "↑2").fg, Some(palette.text));
        assert_eq!(style_of(&buffer, "↓0").fg, Some(palette.dim));
        assert_eq!(style_of(&buffer, "abc1234 Add").fg, Some(palette.dim));
        assert_eq!(style_of(&buffer, "Add login").fg, Some(palette.text));
        assert_eq!(style_of(&buffer, "(2 hours ago, R)").fg, Some(palette.dim));
    }

    #[test]
    fn carnets_sub_tab() {
        let mut model = update::tests::with_carnets(loaded(120, 30));
        // One open carnet, with a tab, a second issue key and a summary, and one closed.
        model.snapshot.carnets.retain(|work| {
            work.path.ends_with("2026-10-01-ABC-1-logs") || work.path.ends_with("2026-08-01-done")
        });
        model.snapshot.carnets[0].tab = true;
        model.snapshot.carnets[0]
            .links
            .issue_keys
            .push(issue_key("o/api#4"));
        if let WorkKind::Carnet { summary, .. } = &mut model.snapshot.carnets[0].kind {
            *summary = "Login fails after the token refresh".into();
        }
        update(&mut model, Action::Key(key(']')));
        model.schedule.finish_all();
        insta::assert_snapshot!(render(&model, 120, 30));
    }

    /// The ABC-1 group's work finished: the login worktree integrated with its tab open, the
    /// form one's upstream gone with two unmerged commits, and a third still in flight.
    fn finished(width: u16, height: u16) -> Model {
        let mut model = update::tests::with_carnets(loaded(width, height));
        model.snapshot.work[1].tree_mut().dirty = false;
        model.snapshot.work[1].tree_mut().symbols = String::new();
        model.snapshot.work[1].tree_mut().integrated = true;
        let form = model.snapshot.work[2].tree_mut();
        (form.gone, form.ahead_of_default) = (true, Some(2));
        let mut wip = update::tests::work("web", "ABC-1-wip", "ABC-1", "side");
        wip.tree_mut().upstream = Some((1, 0));
        model.snapshot.work.push(wip);
        model
    }

    #[test]
    fn finished_rows_are_dimmed_and_marked() {
        insta::assert_snapshot!(render(&finished(120, 30), 120, 30));
    }

    #[test]
    fn finish_plan() {
        let mut model = finished(120, 30);
        let scope = crate::finish::Scope::group("ABC-1");
        let plan = crate::finish::plan(&model.snapshot, &scope, &["web".into()]);
        update(
            &mut model,
            Action::Planned {
                plan: Ok(plan),
                log: Vec::new(),
            },
        );
        model.schedule.finish_all();
        insta::assert_snapshot!(render(&model, 120, 30));
    }

    #[test]
    fn group_prompt_lists_its_completions() {
        let mut model = loaded(100, 30);
        model.snapshot.work[3].links.group = crate::links::Group::parse("slow pages");
        for c in ['>', 'e', 'g', '\t'] {
            let code = match c {
                '\t' => crossterm::event::KeyCode::Tab,
                c => crossterm::event::KeyCode::Char(c),
            };
            update(&mut model, Action::Key(code.into()));
        }
        model.schedule.finish_all();
        insta::assert_snapshot!(render(&model, 100, 30));
    }

    #[test]
    fn actions_menu() {
        let mut model = loaded(100, 30);
        update(&mut model, Action::Key(key('?')));
        insta::assert_snapshot!(render(&model, 100, 30));
    }

    #[test]
    fn ci_marks_rows_and_carries_its_colour_to_the_detail() {
        use crate::worktrunk::{Ci, CiReview, CiState, Decision};
        let mut model = loaded(120, 30);
        model.snapshot.work[0].tree_mut().ci = Some(Ci {
            state: CiState::Passed,
            stale: false,
            branch_workflow: true,
            review: None,
        });
        model.snapshot.work[1].tree_mut().ci = Some(Ci {
            state: CiState::Failed,
            stale: true,
            branch_workflow: false,
            review: Some(CiReview {
                number: Some(27),
                url: Some("https://github.com/o/api/pull/27".into()),
                decision: Some(Decision::ChangesRequested),
            }),
        });
        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        let palette = Palette::new(catppuccin::PALETTE.mocha, Icons::Unicode);
        terminal
            .draw(|frame| view::render(frame, &model, &palette))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let marks: Vec<_> = (buffer.content.iter())
            .filter(|cell| cell.symbol() == "◆")
            .map(|cell| {
                (
                    cell.fg,
                    cell.modifier.contains(ratatui::style::Modifier::DIM),
                )
            })
            .collect();
        assert_eq!(
            marks,
            [
                (palette.error, true),
                (palette.ok, false),
                (palette.error, true)
            ],
            "the grouped stale row and its detail, then main's row"
        );
        insta::assert_snapshot!(terminal.backend().to_string());
    }
}
