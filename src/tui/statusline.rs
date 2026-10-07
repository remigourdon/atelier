//! `atelier statusline`: one ANSI line for the item holding a directory, for zjstatus.

use std::fmt::Write;
use std::path::Path;

use color_eyre::eyre::Result;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;

use super::lists::work::{colored_symbols, finished_mark, review_reference};
use super::lists::{group_style, key_style};
use super::marks::{self, Mark};
use super::view::Palette;
use crate::config::Config;
use crate::context;
use crate::finish;
use crate::items::RecordedKind;
use crate::links::Group;
use crate::process::Runner;
use crate::state::{State, dir_name};
use crate::worktrunk::{Forge, Statusline, Worktree};

/// What the line is about.
#[derive(Debug, Default)]
pub struct Subject {
    pub group: Option<Group>,
    /// A worktree's repo, by its alias else its name; or `carnet`.
    pub name: String,
    /// A repo's main worktree, which shows neither group nor keys.
    pub main: bool,
    /// A closed carnet.
    pub closed: bool,
    /// A worktree's state as `wt list statusline` reports it.
    pub tree: Option<Statusline>,
    /// Every issue key it links, as shown.
    pub keys: Vec<String>,
    /// A carnet's summary: last, so the first cut when space is short.
    pub summary: String,
}

/// The line for `dir`: empty outside a recorded worktree or carnet. Reads the database, the
/// carnet's folder and, for a worktree, `wt list statusline`; never fetches itself.
pub fn line(state: &State, config: &Config, runner: &dyn Runner, dir: &Path) -> Result<String> {
    let Some(located) = context::locate(state, config, dir)? else {
        return Ok(String::new());
    };
    let keys = (located.links.issue_keys.iter())
        .map(|key| key.display(&config.tracker))
        .collect();
    let subject = match &located.kind {
        RecordedKind::Carnet(carnet) => Subject {
            name: "carnet".into(),
            summary: carnet.summary.clone(),
            closed: carnet.closed,
            ..Subject::default()
        },
        RecordedKind::Worktree { repo } => {
            let name = (state.repos()?.into_iter())
                .find(|registered| registered.path == *repo)
                .map_or_else(|| dir_name(repo), |registered| registered.name());
            let tree = context::current_tree(runner, &located).ok();
            Subject {
                name,
                main: tree.as_ref().is_some_and(|statusline| statusline.tree.main),
                tree,
                ..Subject::default()
            }
        }
    };
    let subject = Subject {
        group: located.links.group,
        keys,
        ..subject
    };
    let palette = Palette::new(config.icons);
    Ok(ansi(&spans(&subject, &palette)))
}

/// The line's spans, most important first: zjstatus never truncates a command's output, and
/// zellij clips the bar at its right edge, so the variable-length keys and summary go last
/// and are the first to be cut.
pub fn spans(subject: &Subject, palette: &Palette) -> Vec<Span<'static>> {
    let name = Span::styled(subject.name.clone(), Style::new().fg(palette.accent).bold());
    let mut head = vec![name];
    if subject.main {
        let glyph = palette.glyphs.main;
        let main = if glyph.is_empty() {
            "main worktree"
        } else {
            glyph
        };
        head.push(Span::styled(main, Style::new().fg(palette.accent).bold()));
    } else if let Some(group) = &subject.group {
        head.insert(
            0,
            Span::styled(group.to_string(), group_style(palette).bold()),
        );
    }
    let mut parts = Vec::new();
    for (index, span) in head.into_iter().enumerate() {
        if index > 0 {
            parts.push(Span::styled(" · ", Style::new().fg(palette.dim)));
        }
        parts.push(span);
    }
    let mut words = Vec::new();
    if subject.closed {
        words.push(vec![Span::styled("closed", Style::new().fg(palette.dim))]);
    }
    if let Some(Statusline { tree, forge }) = &subject.tree {
        words.extend(cells(tree, forge.as_ref(), palette));
    }
    if !subject.main {
        let keys = subject.keys.iter();
        words.extend(keys.map(|key| vec![Span::styled(key.clone(), key_style(palette))]));
    }
    if !subject.summary.is_empty() {
        words.push(vec![Span::styled(
            subject.summary.clone(),
            Style::new().fg(palette.text),
        )]);
    }
    for word in words {
        parts.push(Span::raw(" "));
        parts.extend(word);
    }
    parts
}

/// A worktree's cells, as its detail marks them: its status symbols, its checks, its review
/// with its decision and conflicts, and finished. Absent cells take no space.
/// Each cell is its spans, drawn with no space between them.
fn cells(tree: &Worktree, forge: Option<&Forge>, palette: &Palette) -> Vec<Vec<Span<'static>>> {
    let mut cells = Vec::new();
    if !tree.symbols.is_empty() {
        cells.push(colored_symbols(tree, palette));
    }
    if let Some(ci) = &tree.ci {
        cells.extend(marks::checks_span(ci, palette).map(|span| vec![span]));
        if let Some(review) = &ci.review {
            let reference = review_reference(review, forge);
            let mut review_cell = vec![Span::styled(reference, Style::new().fg(palette.text))];
            let decision = review.decision.and_then(marks::decision);
            let conflicts = ci.conflicts.then_some(marks::CONFLICTS);
            review_cell.extend(
                [decision, conflicts]
                    .into_iter()
                    .flatten()
                    .map(|mark: Mark| {
                        let glyph = (mark.glyph)(&palette.glyphs);
                        Span::styled(format!(" {glyph}"), mark.style(palette))
                    }),
            );
            cells.push(review_cell);
        }
    }
    if let Some(signal) = finish::tree_signal(tree) {
        cells.push(vec![finished_mark(signal, palette)]);
    }
    cells
}

/// Spans as raw ANSI: truecolour foregrounds, bold and dim, each undone after its span. Only
/// what was set is undone, never a full reset, so a background the bar paints behind survives.
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
            let _ = write!(out, "\x1b[{}m{}\x1b[22;39m", codes.join(";"), span.content);
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
    use crate::links::tests::links;
    use crate::process::fake::Fake;
    use crate::worktrunk::Listing;

    fn palette(icons: Icons) -> Palette {
        Palette::new(icons)
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

    fn text(subject: &Subject, icons: Icons) -> String {
        plain(&ansi(&spans(subject, &palette(icons))))
    }

    /// The recorded `wt list statusline`: dirty, one ahead and two behind, CI running and #31
    /// approved.
    fn recorded() -> Statusline {
        let listing = Listing::parse(include_str!("../../tests/fixtures/wt-statusline.json"));
        let mut listing = listing.unwrap();
        Statusline {
            tree: listing.worktrees.remove(0),
            forge: listing.forge,
        }
    }

    /// A worktree of `api` in `group`, linking `ABC-1` and `o/web#3`, as `wt list statusline`
    /// recorded it.
    fn worktree(group: &str) -> Subject {
        Subject {
            group: Group::parse(group),
            name: "api".into(),
            keys: vec!["ABC-1".into(), "web#3".into()],
            tree: Some(recorded()),
            ..Subject::default()
        }
    }

    /// The ANSI code that sets a palette colour as the foreground.
    fn fg(color: Color) -> String {
        let Color::Rgb(r, g, b) = color else {
            panic!("{color:?}")
        };
        format!("38;2;{r};{g};{b}m")
    }

    #[test]
    fn a_worktree_shows_its_group_and_repo_then_cells_then_every_key() {
        assert_eq!(
            text(&worktree("login"), Icons::Unicode),
            "LOGIN · api !?↕ ◷ #31 ✓ ABC-1 web#3"
        );
        assert_eq!(
            text(&worktree(""), Icons::Unicode),
            "api !?↕ ◷ #31 ✓ ABC-1 web#3",
            "without a group, it starts at the repo"
        );
        let palette = palette(Icons::Unicode);
        let line = ansi(&spans(&worktree("login"), &palette));
        let blue = PALETTE.mocha.colors.blue.rgb;
        assert!(
            line.contains(&format!(
                "\x1b[38;2;{};{};{}m◷\x1b[22;39m",
                blue.r, blue.g, blue.b
            )),
            "running checks in the detail's colour: {line:?}"
        );
        assert!(
            line.contains(&format!("{}LOGIN\x1b[", fg(palette.group))),
            "{line:?}"
        );
        assert!(
            line.contains(&format!("{}ABC-1\x1b[", fg(palette.issue_key)))
                && line.contains(&format!("{}web#3\x1b[", fg(palette.issue_key))),
            "{line:?}"
        );
        assert_ne!(palette.group, palette.issue_key);
        assert!(
            !line.contains("\x1b[0m"),
            "a full reset clears the bar's background"
        );
    }

    #[test]
    fn a_main_worktree_is_named_so_or_by_a_house_after_its_repo() {
        let Statusline { mut tree, forge } = recorded();
        tree.main = true;
        tree.ci = None;
        tree.upstream = None;
        tree.symbols = "^".into();
        let subject = Subject {
            main: true,
            group: Group::parse("login"),
            keys: vec!["ABC-1".into()],
            tree: Some(Statusline { tree, forge }),
            ..worktree("")
        };
        assert_eq!(text(&subject, Icons::Unicode), "api · main worktree ^");
        assert_eq!(text(&subject, Icons::Nerd), "api · \u{f015} ^");
    }

    #[test]
    fn a_finished_worktree_shows_why() {
        let Statusline { mut tree, .. } = recorded();
        tree.ci = None;
        tree.symbols.clear();
        tree.upstream = None;
        tree.gone = true;
        let subject = Subject {
            tree: Some(Statusline { tree, forge: None }),
            keys: vec!["ABC-1".into()],
            ..worktree("")
        };
        assert_eq!(text(&subject, Icons::Unicode), "api ⊗ ABC-1");
    }

    #[test]
    fn a_carnet_shows_its_group_whether_closed_its_keys_then_summary() {
        let subject = Subject {
            group: Group::parse("login"),
            name: "carnet".into(),
            keys: vec!["ABC-1".into(), "DEF-2".into()],
            summary: "Notes".into(),
            closed: true,
            ..Subject::default()
        };
        assert_eq!(
            text(&subject, Icons::Unicode),
            "LOGIN · carnet closed ABC-1 DEF-2 Notes"
        );
        let subject = Subject {
            group: None,
            closed: false,
            keys: Vec::new(),
            ..subject
        };
        assert_eq!(text(&subject, Icons::Unicode), "carnet Notes");
    }

    fn state() -> State {
        let state =
            State::from_connection(Connection::open_in_memory().unwrap(), "default").unwrap();
        state.add_repo("/a", Some("api"), "default").unwrap();
        state
    }

    #[test]
    fn outside_an_item_the_line_is_empty_and_nothing_runs() {
        let state = state();
        let fake = Fake::default();
        let config = Config::parse("").unwrap();
        let line = line(&state, &config, &fake, Path::new("/elsewhere")).unwrap();
        assert_eq!(line, "");
        assert!(fake.calls().is_empty());
    }

    #[test]
    fn a_recorded_worktree_reads_its_links_and_its_statusline() {
        let state = state();
        let dir = tempfile::tempdir().unwrap();
        let tree = dir.path().canonicalize().unwrap();
        (state.add_worktree(
            &tree,
            Path::new("/a"),
            &links("LOGIN", &["ABC-1", "o/web#3"]),
            "default",
        ))
        .unwrap();
        let config = Config::parse("[tracker.github]\nrepos = [\"o/web\"]\n").unwrap();
        let fake = Fake::default().always(
            "wt",
            Some(include_str!("../../tests/fixtures/wt-statusline.json")),
        );
        let line = line(&state, &config, &fake, &tree).unwrap();
        assert_eq!(
            plain(&line),
            "LOGIN · api !?↕ ◷ #31 ✓ ABC-1 web#3",
            "its group, repo alias, cells and keys shown short"
        );
        let wt = (fake.calls().into_iter())
            .filter(|call| call.starts_with("wt"))
            .count();
        assert_eq!(wt, 1, "only worktrunk's statusline");
    }
}
