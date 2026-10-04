//! The lazygit-style TUI: an Elm loop over terminal events, timers and job results.

mod app;
mod jobs;
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
                    crossterm::execute!(std::io::stdout(), DisableMouseCapture)?;
                    ratatui::restore();
                    let log = jobs::attach(&context, &session);
                    *terminal = ratatui::init();
                    crossterm::execute!(std::io::stdout(), EnableMouseCapture)?;
                    terminal.clear()?;
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

    use super::app::{Action, Model};
    use super::update::update;
    use super::*;
    use crate::config::Icons;

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
            Action::Commits(path, vec!["abc1234 Add login (2 hours ago, R)".into()]),
        );
        model.loading.clear();
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
        update(&mut model, Action::Key(key('3')));
        insta::assert_snapshot!(render(&model, 120, 30));
    }

    #[test]
    fn issues_panel() {
        let mut model = update::tests::with_issues(loaded(120, 30));
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
        let mut issues = parse_gh(tests::GH, false).unwrap();
        issues.extend(parse_gh(tests::GH_CLOSED, false).unwrap());
        update(
            &mut model,
            Action::Issues {
                tracker: crate::issues::Tracker::GitHub,
                issues: Ok(issues),
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
        let enter = crossterm::event::KeyEvent::from(crossterm::event::KeyCode::Enter);
        for key in [key('G'), enter, key('j')] {
            update(&mut model, Action::Key(key));
        }
        let readme = "# 2026-10-02-ideas\n\nWhat I found **so far**.\n";
        update(
            &mut model,
            Action::Readme("/data/2026-10-02-ideas".into(), Some(readme.into())),
        );
        model.loading.clear();
        insta::assert_snapshot!(render(&model, 120, 30));
    }

    #[test]
    fn actions_menu() {
        let mut model = loaded(100, 30);
        update(&mut model, Action::Key(key('?')));
        insta::assert_snapshot!(render(&model, 100, 30));
    }
}
