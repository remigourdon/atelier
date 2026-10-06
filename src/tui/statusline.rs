//! `atelier statusline`: one ANSI line for the item holding a directory, for zjstatus.

use std::fmt::Write;
use std::path::Path;

use color_eyre::eyre::Result;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;

use super::lists::work::{
    behind, ci_mark, decision_style, finished_mark, review_reference, symbols,
};
use super::view::Palette;
use crate::config::Config;
use crate::context;
use crate::finish;
use crate::issues;
use crate::process::Runner;
use crate::state::{ItemKind, State};
use crate::worktrunk::{Forge, Worktree};

/// What the line is about.
#[derive(Debug, Default)]
pub struct Subject {
    /// The ticket key, empty for none.
    pub ticket: String,
    /// The cached issue title, or a carnet's summary: elided first when space is short.
    pub title: String,
    /// A repo's main worktree, named so instead of a ticket.
    pub main: bool,
    /// A closed carnet.
    pub closed: bool,
    /// A worktree's state as `wt list statusline` reports it.
    pub tree: Option<(Worktree, Option<Forge>)>,
}

/// The line for `dir`: empty outside a recorded worktree or carnet. Reads the database, the
/// issue cache at any age and, for a worktree, `wt list statusline`; never fetches itself.
pub fn line(
    state: &State,
    config: &Config,
    runner: &dyn Runner,
    dir: &Path,
    width: usize,
) -> Result<String> {
    let Some(here) = context::here(state, config, dir)? else {
        return Ok(String::new());
    };
    let subject = match here.item.kind {
        ItemKind::Carnet => Subject {
            title: (here.carnet.as_ref()).map_or(String::new(), |carnet| carnet.summary.clone()),
            closed: here.carnet.is_some_and(|carnet| carnet.closed),
            ticket: here.group,
            ..Subject::default()
        },
        ItemKind::Worktree => {
            let tree = context::current_tree(runner, &here.item).ok();
            let title = match here.group.as_str() {
                "" => None,
                key => issues::cached(state, &config.tracker, key)?.map(|(issue, _)| issue.title),
            };
            Subject {
                main: tree.as_ref().is_some_and(|(tree, _)| tree.main),
                title: title.unwrap_or_default(),
                ticket: here.group,
                tree,
                ..Subject::default()
            }
        }
    };
    let palette = Palette::new(config.flavor(), config.icons);
    Ok(ansi(&spans(&subject, &palette, width)))
}

/// The line's spans, the title elided to fit `width` columns and dropped when nothing of it fits.
pub fn spans(subject: &Subject, palette: &Palette, width: usize) -> Vec<Span<'static>> {
    let mut head = Vec::new();
    if subject.main {
        let glyph = palette.glyphs.main;
        let name = if glyph.is_empty() {
            "main worktree"
        } else {
            glyph
        };
        head.push(Span::styled(name, Style::new().fg(palette.accent).bold()));
    } else if !subject.ticket.is_empty() {
        head.push(Span::styled(
            subject.ticket.clone(),
            Style::new().fg(palette.accent).bold(),
        ));
    }
    let mut tail = Vec::new();
    if subject.closed {
        tail.push(Span::styled("closed", Style::new().fg(palette.dim)));
    }
    if let Some((tree, forge)) = &subject.tree {
        tail.extend(cells(tree, forge.as_ref(), palette));
    }
    let used: usize = (head.iter().chain(&tail))
        .map(|span| span.width() + 1)
        .sum::<usize>();
    let room = width.saturating_sub(used);
    let title = (!subject.title.is_empty() && room > 1).then(|| elide(&subject.title, room));
    let title = title.map(|title| Span::styled(title, Style::new().fg(palette.text)));
    let mut line = Vec::new();
    for span in head.into_iter().chain(title).chain(tail) {
        if !line.is_empty() {
            line.push(Span::raw(" "));
        }
        line.push(span);
    }
    line
}

/// A worktree's cells, as its Work row draws them: dirty and ahead, behind, CI, review and
/// finished. Absent cells take no space.
fn cells(tree: &Worktree, forge: Option<&Forge>, palette: &Palette) -> Vec<Span<'static>> {
    let mut cells = Vec::new();
    if !tree.symbols.is_empty() {
        cells.push(symbols(tree, palette));
    }
    cells.extend(behind(tree, palette));
    if let Some(ci) = &tree.ci {
        cells.push(ci_mark(ci, palette));
        if let Some(review) = &ci.review {
            let reference = review_reference(review, forge);
            cells.push(Span::styled(reference, Style::new().fg(palette.text)));
            if let Some(decision) = review.decision {
                let style = decision_style(ci, decision, palette);
                cells.push(Span::styled(decision.label(), style));
            }
        }
    }
    if let Some(signal) = finish::tree_signal(tree) {
        cells.push(finished_mark(signal, palette));
    }
    cells
}

/// `text` cut to `width` columns, ending in `…` when cut.
fn elide(text: &str, width: usize) -> String {
    if Span::raw(text).width() <= width {
        return text.to_owned();
    }
    let mut out = String::new();
    for c in text.chars() {
        if Span::raw(format!("{out}{c}…")).width() > width {
            break;
        }
        out.push(c);
    }
    out.trim_end().to_owned() + "…"
}

/// Spans as raw ANSI: truecolour foregrounds, bold and dim, reset after each styled span.
pub fn ansi(spans: &[Span]) -> String {
    let mut out = String::new();
    for span in spans {
        let mut codes = Vec::new();
        if span.style.add_modifier.contains(Modifier::BOLD) {
            codes.push("1".to_owned());
        }
        if span.style.add_modifier.contains(Modifier::DIM) {
            codes.push("2".to_owned());
        }
        if let Some(Color::Rgb(r, g, b)) = span.style.fg {
            codes.push(format!("38;2;{r};{g};{b}"));
        }
        if codes.is_empty() {
            out.push_str(&span.content);
        } else {
            let _ = write!(out, "\x1b[{}m{}\x1b[0m", codes.join(";"), span.content);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use catppuccin::PALETTE;
    use rusqlite::Connection;

    use super::*;
    use crate::config::Icons;
    use crate::issues::tests::issue;
    use crate::process::fake::Fake;
    use crate::worktrunk::Listing;

    fn palette(icons: Icons) -> Palette {
        Palette::new(PALETTE.mocha, icons)
    }

    /// The text without its ANSI codes.
    fn plain(line: &str) -> String {
        let mut out = String::new();
        let mut chars = line.chars();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                chars.by_ref().find(|&c| c == 'm');
            } else {
                out.push(c);
            }
        }
        out
    }

    fn text(subject: &Subject, icons: Icons, width: usize) -> String {
        plain(&ansi(&spans(subject, &palette(icons), width)))
    }

    /// The recorded `wt list statusline`: dirty, one ahead and two behind, CI running and #31
    /// approved.
    fn recorded() -> (Worktree, Option<Forge>) {
        let listing = Listing::parse(include_str!("../../tests/fixtures/wt-statusline.json"));
        let mut listing = listing.unwrap();
        (listing.worktrees.remove(0), listing.forge)
    }

    #[test]
    fn a_worktree_shows_its_ticket_title_and_cells() {
        let subject = Subject {
            ticket: "ABC-1".into(),
            title: "Fix the login".into(),
            tree: Some(recorded()),
            ..Subject::default()
        };
        assert_eq!(
            text(&subject, Icons::Unicode, 80),
            "ABC-1 Fix the login !?↕ ↓2 ◆ #31 approved"
        );
        let line = ansi(&spans(&subject, &palette(Icons::Unicode), 80));
        let blue = PALETTE.mocha.colors.blue.rgb;
        assert!(
            line.contains(&format!(
                "\x1b[38;2;{};{};{}m◆\x1b[0m",
                blue.r, blue.g, blue.b
            )),
            "running CI in the Work row's colour: {line:?}"
        );
    }

    #[test]
    fn the_title_is_elided_first_then_dropped() {
        let subject = Subject {
            ticket: "ABC-1".into(),
            title: "Fix the login".into(),
            tree: Some(recorded()),
            ..Subject::default()
        };
        assert_eq!(
            text(&subject, Icons::Unicode, 35),
            "ABC-1 Fix th… !?↕ ↓2 ◆ #31 approved"
        );
        assert_eq!(
            text(&subject, Icons::Unicode, 20),
            "ABC-1 !?↕ ↓2 ◆ #31 approved"
        );
    }

    #[test]
    fn a_main_worktree_is_named_so_or_by_a_house() {
        let (mut tree, forge) = recorded();
        tree.main = true;
        tree.ci = None;
        tree.upstream = None;
        tree.symbols = "^".into();
        let subject = Subject {
            main: true,
            tree: Some((tree, forge)),
            ..Subject::default()
        };
        assert_eq!(text(&subject, Icons::Unicode, 80), "main worktree ^");
        assert_eq!(text(&subject, Icons::Nerd, 80), "\u{f015} ^");
    }

    #[test]
    fn a_finished_worktree_shows_why() {
        let (mut tree, _) = recorded();
        tree.ci = None;
        tree.symbols.clear();
        tree.upstream = None;
        tree.gone = true;
        let subject = Subject {
            ticket: "ABC-1".into(),
            tree: Some((tree, None)),
            ..Subject::default()
        };
        assert_eq!(text(&subject, Icons::Unicode, 80), "ABC-1 ⊘");
    }

    #[test]
    fn a_carnet_shows_its_ticket_summary_and_whether_closed() {
        let subject = Subject {
            ticket: "ABC-1".into(),
            title: "Notes".into(),
            closed: true,
            ..Subject::default()
        };
        assert_eq!(text(&subject, Icons::Unicode, 80), "ABC-1 Notes closed");
    }

    fn state() -> State {
        let state =
            State::from_connection(Connection::open_in_memory().unwrap(), "default").unwrap();
        state.add_repo("/a", None, "default").unwrap();
        state
    }

    #[test]
    fn outside_an_item_the_line_is_empty_and_nothing_runs() {
        let state = state();
        let fake = Fake::default();
        let config = Config::parse("").unwrap();
        let line = line(&state, &config, &fake, Path::new("/elsewhere"), 80).unwrap();
        assert_eq!(line, "");
        assert!(fake.calls().is_empty());
    }

    #[test]
    fn a_recorded_worktree_reads_the_cache_and_its_statusline() {
        let state = state();
        let dir = tempfile::tempdir().unwrap();
        let tree = dir.path().canonicalize().unwrap();
        (state.add_item(
            &tree,
            ItemKind::Worktree,
            Some(Path::new("/a")),
            "ABC-1",
            "default",
        ))
        .unwrap();
        let cached = serde_json::to_string(&[issue("ABC-1", &[], false)]).unwrap();
        state.store_cache("gh", "issues o/api", &cached).unwrap();
        let config = Config::parse("[tracker.github]\nrepos = [\"o/api\"]\n").unwrap();
        let fake = Fake::default().always(
            "wt",
            Some(include_str!("../../tests/fixtures/wt-statusline.json")),
        );
        let line = line(&state, &config, &fake, &tree, 80).unwrap();
        assert_eq!(plain(&line), "ABC-1 Issue ABC-1 !?↕ ↓2 ◆ #31 approved");
        let wt = (fake.calls().into_iter())
            .filter(|call| call.starts_with("wt"))
            .count();
        assert_eq!(wt, 1, "only worktrunk's statusline");
    }
}
