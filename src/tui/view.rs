//! Rendering: the side panels, the main view, the command log and the hint bar.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};

use super::app::{
    Cmd, Focus, KEYMAP, Kind, List, Model, On, Panel, Popup, Screen, Source, Work, popup_hints,
};
use super::lists;
use super::lists::carnets;
use super::markdown::{self, Markdown};
use super::marks::{self, Tone};
use super::widgets;
use crate::config::Icons;
use crate::finish::Signal;
use crate::git::Commit;
use crate::worktrunk::Checks;

/// Below this width the main view is hidden until `+`.
pub const NARROW: u16 = 100;
/// Below this height the focused side panel takes the room and the others fold to their titles.
pub const SHORT: u16 = 24;
const LOG_HEIGHT: u16 = 8;

pub struct Palette {
    pub accent: Color,
    pub text: Color,
    /// The keys of the main view's detail.
    pub label: Color,
    pub dim: Color,
    pub selection: Color,
    pub ok: Color,
    pub error: Color,
    pub warn: Color,
    pub info: Color,
    /// A review's reviewers requested changes.
    pub changes_requested: Color,
    /// A review's required approval is not given yet.
    pub approval_pending: Color,
    /// A list's filter, as it is typed and once applied.
    pub filter: Color,
    /// A group's label, set apart from issue keys.
    pub group: Color,
    /// An issue key, set apart from groups.
    pub issue_key: Color,
    /// The name of the workspace an item is in.
    pub workspace: Color,
    /// How a carnet's README is drawn.
    pub markdown: Markdown,
    pub glyphs: Glyphs,
}

/// The symbols rows are drawn with; an empty one is left out.
pub struct Glyphs {
    pub workspace: &'static str,
    pub repo: &'static str,
    pub worktree: &'static str,
    /// A repo's main worktree, in place of its name when there is a glyph.
    pub main: &'static str,
    pub carnet: &'static str,
    pub review: &'static str,
    /// An issue's mark when an open review links it.
    pub reviewed: &'static str,
    pub issue: &'static str,
    pub open: &'static str,
    pub closed: &'static str,
    pub folded: &'static str,
    pub unfolded: &'static str,
    /// A finished worktree's mark: integrated, or its upstream gone.
    pub integrated: &'static str,
    pub gone: &'static str,
    /// A branch's checks.
    pub passed: &'static str,
    pub running: &'static str,
    pub failed: &'static str,
    pub unavailable: &'static str,
    /// A review's merge conflicts.
    pub conflicts: &'static str,
    /// A review's decision, short of approval.
    pub changes_requested: &'static str,
    pub approval: &'static str,
    pub spinner: [&'static str; 4],
}

impl Glyphs {
    pub fn new(icons: Icons) -> Self {
        let spinner = ["◐", "◓", "◑", "◒"];
        match icons {
            Icons::Unicode => Self {
                workspace: "",
                repo: "",
                worktree: "",
                main: "",
                carnet: "✎",
                review: "",
                reviewed: "⑂",
                issue: "",
                open: "◉",
                closed: "○",
                folded: "▸",
                unfolded: "▾",
                integrated: "⊂",
                gone: "⊗",
                passed: "✔",
                running: "◔",
                failed: "✖",
                unavailable: "⚠",
                conflicts: "✗",
                changes_requested: "±",
                approval: "◇",
                spinner,
            },
            // Nerd Fonts: fa-desktop, oct-repo, dev-git_branch, fa-home, fa-book, oct-git_pull_request,
            // oct-issue_opened, fa-dot_circle_o, fa-circle_o, fa-folder, fa-folder_open,
            // oct-git_merge, fa-chain_broken, oct-check_circle, oct-clock, oct-x_circle, fa-warning,
            // oct-file_diff, oct-eye.
            Icons::Nerd => Self {
                workspace: "\u{f108}",
                repo: "\u{f401}",
                worktree: "\u{e725}",
                main: "\u{f015}",
                carnet: "\u{f02d}",
                review: "\u{f407}",
                reviewed: "\u{f407}",
                issue: "\u{f41b}",
                open: "\u{f192}",
                closed: "\u{f10c}",
                folded: "\u{f07b}",
                unfolded: "\u{f07c}",
                integrated: "\u{f419}",
                gone: "\u{f127}",
                passed: "\u{f49e}",
                running: "\u{f43a}",
                failed: "\u{f52f}",
                unavailable: "\u{f071}",
                conflicts: "✗",
                changes_requested: "\u{f4d2}",
                approval: "\u{f441}",
                spinner,
            },
        }
    }
}

impl Glyphs {
    /// A finished worktree's mark.
    pub fn signal(&self, signal: Signal) -> &'static str {
        match signal {
            Signal::Integrated => self.integrated,
            Signal::Gone => self.gone,
        }
    }
}

/// What a mark on screen means, as the `?` menu's legend lists it.
#[derive(Debug)]
pub struct Legend {
    /// A glyph, or a sample of text in its colour; left out when empty.
    pub mark: fn(&Glyphs) -> &'static str,
    pub style: fn(&Palette) -> Style,
    pub help: &'static str,
    /// The lists that show it, or `Global` for anywhere.
    pub on: On,
}

const WORK: &[Kind] = &[Kind::Work];
const ITEMS: &[Kind] = &[Kind::Work, Kind::Carnets];
const GROUPED: &[Kind] = &[Kind::Work, Kind::Carnets, Kind::Reviews];
const KEYED: &[Kind] = &[Kind::Work, Kind::Carnets, Kind::Reviews, Kind::Issues];

fn dim(palette: &Palette) -> Style {
    Style::new().fg(palette.dim)
}

fn warn(palette: &Palette) -> Style {
    Style::new().fg(palette.warn)
}

fn checks(checks: Checks, palette: &Palette) -> Style {
    Style::new().fg(lists::work::checks_mark(checks, palette).1)
}

/// A status symbol's colour.
fn symbol(mark: char, palette: &Palette) -> Style {
    let tone = marks::lookup(mark).map_or(Tone::Quiet, |symbol| symbol.tone);
    Style::new().fg(tone.color(palette))
}

/// The legend: every mark the lists, the main view and the command log draw, in the order `?`
/// lists them. The `status:` marks are worktrunk's, from `wt list`'s Status column.
#[rustfmt::skip]
pub const LEGEND: &[Legend] = &[
    Legend { mark: |g| g.workspace, style: dim, help: "workspace", on: On::Lists(&[Kind::Workspaces]) },
    Legend { mark: |g| g.repo, style: dim, help: "repo", on: On::Lists(&[Kind::Repos]) },
    Legend { mark: |g| g.worktree, style: dim, help: "worktree", on: On::Lists(WORK) },
    Legend { mark: |g| g.carnet, style: |p| carnets::glyph_style(false, p), help: "carnet, open", on: On::Lists(ITEMS) },
    Legend { mark: |g| g.carnet, style: |p| carnets::glyph_style(true, p), help: "carnet, closed, the row dimmed", on: On::Lists(ITEMS) },
    Legend { mark: |g| g.review, style: dim, help: "review", on: On::Lists(&[Kind::Reviews]) },
    Legend { mark: |g| g.issue, style: dim, help: "issue", on: On::Lists(&[Kind::Issues]) },
    Legend { mark: |g| g.open, style: |p| lists::tab(true, p).1, help: "tab open", on: On::Lists(ITEMS) },
    Legend { mark: |g| g.closed, style: |p| lists::tab(false, p).1, help: "tab closed", on: On::Lists(ITEMS) },
    Legend { mark: |g| g.open, style: |p| lists::tab(true, p).1, help: "checked out, its tab open", on: On::Lists(&[Kind::Reviews]) },
    Legend { mark: |g| g.closed, style: |p| lists::tab(false, p).1, help: "checked out, its tab closed", on: On::Lists(&[Kind::Reviews]) },
    Legend { mark: |g| g.open, style: |p| lists::tab(true, p).1, help: "linked work, a tab open", on: On::Lists(&[Kind::Issues]) },
    Legend { mark: |g| g.closed, style: |p| lists::tab(false, p).1, help: "linked work, no tab open", on: On::Lists(&[Kind::Issues]) },
    Legend { mark: |g| g.reviewed, style: lists::review_style, help: "an open review links it", on: On::Lists(&[Kind::Issues]) },
    Legend { mark: |g| g.spinner[0], style: |p| Style::new().fg(p.info), help: "pulling", on: On::Lists(WORK) },
    Legend { mark: |g| g.folded, style: |p| lists::group_style(p).bold(), help: "folded group", on: On::Lists(WORK) },
    Legend { mark: |g| g.unfolded, style: |p| lists::group_style(p).bold(), help: "unfolded group", on: On::Lists(WORK) },
    Legend { mark: |_| "group", style: lists::group_style, help: "a group", on: On::Lists(GROUPED) },
    Legend { mark: |_| "KEY-1", style: lists::key_style, help: "an issue key", on: On::Lists(KEYED) },
    Legend { mark: |g| g.passed, style: |p| checks(Checks::Passed, p), help: "checks passed", on: On::Lists(WORK) },
    Legend { mark: |g| g.running, style: |p| checks(Checks::Running, p), help: "checks running", on: On::Lists(WORK) },
    Legend { mark: |g| g.failed, style: |p| checks(Checks::Failed, p), help: "checks failed", on: On::Lists(WORK) },
    Legend { mark: |g| g.unavailable, style: |p| checks(Checks::Unavailable, p), help: "checks unavailable", on: On::Lists(WORK) },
    Legend { mark: |g| g.passed, style: |p| checks(Checks::Passed, p).add_modifier(Modifier::DIM), help: "dimmed: stale, or a draft", on: On::Lists(WORK) },
    Legend { mark: |g| g.conflicts, style: |p| Style::new().fg(p.error), help: "merge conflicts", on: On::Lists(WORK) },
    Legend { mark: |g| g.changes_requested, style: |p| Style::new().fg(p.changes_requested), help: "changes requested", on: On::Lists(WORK) },
    Legend { mark: |g| g.approval, style: |p| Style::new().fg(p.approval_pending), help: "waiting for approval", on: On::Lists(WORK) },
    Legend { mark: |g| g.integrated, style: dim, help: "finished, the row dimmed: integrated into the default branch", on: On::Lists(WORK) },
    Legend { mark: |g| g.gone, style: dim, help: "finished, the row dimmed: its upstream branch is gone", on: On::Lists(WORK) },
    Legend { mark: |_| "✓", style: dim, help: "command log: the command succeeded", on: On::Global },
    Legend { mark: |_| "✗", style: |p| Style::new().fg(p.error), help: "command log: the command failed", on: On::Global },
    Legend { mark: |_| "⟳", style: dim, help: "hint bar: loading", on: On::Global },
];

impl Legend {
    /// The legend of a kind of list, its marks empty with these glyphs left out.
    pub fn of(kind: Kind, glyphs: &Glyphs) -> Vec<&'static Legend> {
        let here =
            |legend: &&Legend| matches!(legend.on, On::Lists(kinds) if kinds.contains(&kind));
        let global = |legend: &&Legend| legend.on == On::Global;
        (LEGEND.iter().filter(here))
            .chain(LEGEND.iter().filter(global))
            .filter(|legend| !(legend.mark)(glyphs).is_empty())
            .collect()
    }
}

/// A glyph and its separating space, or nothing for an empty glyph.
pub(super) fn icon(glyph: &'static str, style: Style) -> Option<Span<'static>> {
    (!glyph.is_empty()).then(|| Span::styled(format!("{glyph} "), style))
}

impl Palette {
    /// Catppuccin Mocha's colours.
    pub fn new(icons: Icons) -> Self {
        let colors = catppuccin::PALETTE.mocha.colors;
        Self {
            glyphs: Glyphs::new(icons),
            accent: colors.mauve.into(),
            text: colors.text.into(),
            label: colors.subtext0.into(),
            dim: colors.overlay1.into(),
            selection: colors.surface0.into(),
            ok: colors.green.into(),
            error: colors.red.into(),
            warn: colors.yellow.into(),
            info: colors.blue.into(),
            changes_requested: colors.yellow.into(),
            approval_pending: colors.pink.into(),
            filter: colors.yellow.into(),
            group: colors.lavender.into(),
            issue_key: colors.peach.into(),
            workspace: colors.teal.into(),
            markdown: Markdown::new(&colors),
        }
    }
}

/// Where everything goes for a terminal of `area`.
#[derive(Debug, Default, PartialEq)]
pub struct Areas {
    pub panels: Vec<(Panel, Rect)>,
    pub main: Option<Rect>,
    pub log: Option<Rect>,
    pub hints: Rect,
}

pub fn areas(model: &Model, area: Rect) -> Areas {
    let [body, hints] = Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(area);
    let narrow = area.width < NARROW;
    let main_shown = model.screen != Screen::Normal || !narrow;
    let (side, right) = match (model.screen, model.focus) {
        (Screen::Full, Focus::Main) => (None, Some(body)),
        (Screen::Full, Focus::Panel(_)) => (Some(body), None),
        _ if !main_shown => (Some(body), None),
        (screen, _) => {
            let side = if screen == Screen::Half || narrow {
                Constraint::Percentage(50)
            } else {
                Constraint::Percentage(33)
            };
            let [side, right] = Layout::horizontal([side, Constraint::Fill(1)]).areas(body);
            (Some(side), Some(right))
        }
    };
    let mut panels = Vec::new();
    if let Some(side) = side {
        let shown: Vec<Panel> = match (model.screen, model.focus) {
            (Screen::Full, Focus::Panel(panel)) => vec![panel],
            _ => Panel::ALL.to_vec(),
        };
        let short = side.height < SHORT;
        let constraints = shown.iter().map(|&panel| match panel {
            _ if short && panel != model.panel => Constraint::Length(1),
            _ if short => Constraint::Fill(1),
            Panel::Workspaces | Panel::Reviews | Panel::Issues => Constraint::Fill(1),
            Panel::Work => Constraint::Fill(2),
        });
        let rects = Layout::vertical(constraints).split(side);
        panels = shown.into_iter().zip(rects.iter().copied()).collect();
    }
    let (main, log) = match right {
        Some(right) if model.show_log && right.height > LOG_HEIGHT * 2 => {
            let [main, log] =
                Layout::vertical([Constraint::Fill(1), Constraint::Length(LOG_HEIGHT)])
                    .areas(right);
            (Some(main), Some(log))
        }
        right => (right, None),
    };
    Areas {
        panels,
        main,
        log,
        hints,
    }
}

/// The first row to show so `selected` stays in a list of `height` rows.
pub fn offset(selected: usize, height: u16) -> usize {
    selected.saturating_sub((height as usize).saturating_sub(1))
}

pub fn render(frame: &mut Frame, model: &Model, palette: &Palette) {
    let areas = areas(model, frame.area());
    for &(panel, rect) in &areas.panels {
        render_panel(frame, model, palette, panel, rect);
    }
    if let Some(rect) = areas.main {
        render_main(frame, model, palette, rect);
    }
    if let Some(rect) = areas.log {
        render_log(frame, model, palette, rect);
    }
    render_hints(frame, model, palette, areas.hints);
    if let Some(modal) = &model.modal {
        widgets::modal(frame, modal, palette);
    }
}

fn block<'a>(title: Line<'a>, focused: bool, palette: &Palette) -> Block<'a> {
    let color = if focused { palette.accent } else { palette.dim };
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(color))
        .title(title)
}

fn render_panel(frame: &mut Frame, model: &Model, palette: &Palette, panel: Panel, rect: Rect) {
    let focused = model.focus == Focus::Panel(panel);
    let list = model.list(panel);
    let tab = |label: &str, active: bool| {
        if active {
            Span::styled(label.to_owned(), Style::new().fg(palette.accent).bold())
        } else {
            Span::styled(label.to_owned(), Style::new().fg(palette.dim))
        }
    };
    let mut title = vec![Span::raw(format!("[{}] ", panel.number()))];
    let tabs = model.tabs(panel);
    let width: usize = (tabs
        .iter()
        .map(|&other| model.title(other).chars().count() + 3))
    .sum();
    if width + 4 > rect.width as usize {
        // Too many to fit: only the active one, and where it is.
        let at = tabs.iter().position(|&other| other == list).unwrap_or(0);
        title.push(tab(model.title(list), true));
        title.push(Span::styled(
            format!(" {}/{}", at + 1, tabs.len()),
            Style::new().fg(palette.dim),
        ));
    } else {
        for (index, other) in tabs.into_iter().enumerate() {
            if index > 0 {
                title.push(Span::raw(" │ "));
            }
            title.push(tab(model.title(other), other == list));
        }
    }
    if list == List::Work
        && let Some(workspace) = model.workspace()
    {
        title.push(Span::styled(
            format!(" · {workspace}"),
            Style::new().fg(palette.dim),
        ));
    }
    let filter = model.filter(list);
    if !filter.is_empty() || model.filtering == Some(list) {
        title.push(Span::styled(
            format!(" /{filter}"),
            Style::new().fg(palette.filter),
        ));
    }
    if rect.height < 3 {
        let block = Block::new()
            .borders(Borders::TOP)
            .border_style(Style::new().fg(palette.dim))
            .title(Line::from(title));
        frame.render_widget(block, rect);
        return;
    }
    let block = block(Line::from(title), focused, palette);
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    let rows = lists::of(list).rows(model, palette, list);
    if rows.is_empty() {
        let empty = lists::of(list).empty(model, list);
        frame.render_widget(
            Paragraph::new(Span::styled(empty, Style::new().fg(palette.dim))),
            inner,
        );
        return;
    }
    let selected = model.index(list).min(rows.len() - 1);
    let start = offset(selected, inner.height);
    let lines: Vec<Line> = rows
        .into_iter()
        .enumerate()
        .skip(start)
        .take(inner.height as usize)
        .map(|(index, line)| {
            if index == selected {
                let style = Style::new().bg(palette.selection);
                let style = if focused {
                    style.add_modifier(Modifier::BOLD)
                } else {
                    style
                };
                line.style(style)
            } else {
                line
            }
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
}

/// The active list's selected item, whose README and commits the main view shows.
fn selected(model: &Model) -> Option<&Work> {
    let list = model.active();
    lists::of(list).item(model, list)
}

/// The selected carnet's README, rendered once read and wrapped at the main view's width,
/// capped at [`markdown::MAX_WIDTH`].
fn readme(model: &Model, palette: &Palette) -> Option<Vec<Line<'static>>> {
    let path = selected(model)?.path();
    let readme = (model.readme.as_ref()).filter(|readme| readme.path == *path)?;
    let text = (palette.markdown).render(crate::carnet::body(readme.text.as_deref()?));
    let main = areas(model, Rect::new(0, 0, model.size.0, model.size.1)).main?;
    let width = main.width.saturating_sub(2).min(markdown::MAX_WIDTH);
    Some(markdown::wrap(text, width))
}

/// The selected worktree's recent commits, once loaded.
fn commits(model: &Model) -> Option<&Vec<Commit>> {
    model.commits.get(selected(model)?.path())
}

fn detail(model: &Model, palette: &Palette) -> Vec<(String, Line<'static>)> {
    let list = model.active();
    lists::of(list).detail(model, palette, list)
}

/// How many lines the main view holds, so scrolling stops at its end. Any palette lays out
/// the same lines.
pub fn main_len(model: &Model) -> usize {
    let palette = Palette::new(Icons::Unicode);
    detail(model, &palette).len()
        + model.carnet_hits().map_or(0, |hits| hits.len() + 2)
        + readme(model, &palette).map_or(0, |readme| readme.len() + 2)
        + commits(model).map_or(0, |commits| commits.len() + 2)
}

fn render_main(frame: &mut Frame, model: &Model, palette: &Palette, rect: Rect) {
    let focused = model.focus == Focus::Main;
    let block = block(Line::from(" Main "), focused, palette);
    let pairs = detail(model, palette);
    let width = pairs.iter().map(|(key, _)| key.len()).max().unwrap_or(0);
    let mut lines: Vec<Line> = pairs
        .into_iter()
        .map(|(key, mut value)| {
            let key = Span::styled(format!("{key:width$}  "), Style::new().fg(palette.label));
            if value.width() == 0 {
                value = lists::subtle("none", palette).into();
            }
            Line::from_iter(std::iter::once(key).chain(value.spans))
        })
        .collect();
    let section = |lines: &mut Vec<Line>, title| {
        lines.push(Line::raw(""));
        lines.push(Line::styled(title, Style::new().fg(palette.accent).bold()));
    };
    if let Some(hits) = model.carnet_hits() {
        section(&mut lines, "Matches");
        lines.extend(hits.iter().map(|hit| Line::raw(hit.as_str())));
    }
    if let Some(readme) = readme(model, palette) {
        section(&mut lines, "README");
        lines.extend(readme);
    }
    if let Some(commits) = commits(model) {
        section(&mut lines, "Recent commits");
        let dim = Style::new().fg(palette.dim);
        lines.extend(commits.iter().map(|commit| {
            Line::from(vec![
                Span::styled(format!("{} ", commit.sha), dim),
                Span::raw(commit.subject.as_str()),
                Span::styled(format!(" ({}, {})", commit.age, commit.author), dim),
            ])
        }));
    }
    // What sets no colour of its own is text.
    let text = Text::from(lines).style(Style::new().fg(palette.text));
    let paragraph = Paragraph::new(text).block(block).scroll(model.scroll);
    frame.render_widget(paragraph, rect);
}

fn render_log(frame: &mut Frame, model: &Model, palette: &Palette, rect: Rect) {
    let block = block(Line::from(" Command log "), false, palette);
    let height = block.inner(rect).height as usize;
    let lines: Vec<Line> = model.log[model.log.len().saturating_sub(height)..]
        .iter()
        .map(|entry| match &entry.error {
            None => Line::from(vec![
                Span::styled("✓ ", Style::new().fg(palette.dim)),
                Span::styled(entry.command.as_str(), Style::new().fg(palette.dim)),
            ]),
            Some(error) => Line::from(vec![
                Span::styled("✗ ", Style::new().fg(palette.error)),
                Span::raw(entry.command.as_str()),
                Span::styled(format!(": {error}"), Style::new().fg(palette.error)),
            ]),
        })
        .collect();
    frame.render_widget(Paragraph::new(lines).block(block), rect);
}

fn render_hints(frame: &mut Frame, model: &Model, palette: &Palette, rect: Rect) {
    let mut spans = Vec::new();
    if let Some(list) = model.filtering {
        spans.push(Span::styled(
            format!(
                "filter {}: {}",
                model.title(list).to_lowercase(),
                popup_hints(Popup::Filter)
            ),
            Style::new().fg(palette.filter),
        ));
    } else {
        let active = model.active();
        for binding in KEYMAP
            .iter()
            .filter(|binding| binding.hint.contains(&active.kind()))
            .filter(|binding| model.carnets || binding.cmd != Cmd::ToggleCarnet)
        {
            if !spans.is_empty() {
                spans.push(Span::styled(" · ", Style::new().fg(palette.dim)));
            }
            spans.push(Span::styled(binding.label, Style::new().fg(palette.accent)));
            spans.push(Span::raw(format!(" {}", short_help(binding.help))));
        }
    }
    let loading: Vec<&str> = model.schedule.loading().map(Source::label).collect();
    let [left, right] = Layout::horizontal([
        Constraint::Fill(1),
        Constraint::Length(if loading.is_empty() {
            0
        } else {
            loading.join(" ").len() as u16 + 3
        }),
    ])
    .areas(rect);
    frame.render_widget(Paragraph::new(Line::from(spans)), left);
    if !loading.is_empty() {
        frame.render_widget(
            Paragraph::new(Span::styled(
                format!("⟳ {}", loading.join(" ")),
                Style::new().fg(palette.dim),
            )),
            right,
        );
    }
}

/// Short help for the hint bar, keeping tab closing distinct from carnet closing.
fn short_help(help: &str) -> &str {
    if help == "close tab" {
        help
    } else {
        help.split(' ').next().unwrap_or(help)
    }
}
