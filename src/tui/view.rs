//! Rendering: the side panels, the main view, the command log and the hint bar.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};

use super::app::{Focus, KEYMAP, List, Model, Panel, Popup, Row, Screen, popup_hints};
use super::widgets;
use crate::config::Icons;

/// Below this width the main view is hidden until `+`.
pub const NARROW: u16 = 100;
/// Below this height the focused side panel takes the room and the others fold to their titles.
pub const SHORT: u16 = 24;
const LOG_HEIGHT: u16 = 8;

pub struct Palette {
    pub accent: Color,
    pub text: Color,
    pub dim: Color,
    pub selection: Color,
    pub ok: Color,
    pub error: Color,
    pub warn: Color,
    pub info: Color,
    pub glyphs: Glyphs,
}

/// The symbols rows are drawn with; an empty one is left out.
pub struct Glyphs {
    pub workspace: &'static str,
    pub repo: &'static str,
    pub worktree: &'static str,
    pub open: &'static str,
    pub closed: &'static str,
    pub folded: &'static str,
    pub unfolded: &'static str,
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
                open: "●",
                closed: "○",
                folded: "▸",
                unfolded: "▾",
                spinner,
            },
            // Nerd Fonts: fa-desktop, oct-repo, dev-git_branch, fa-circle, fa-circle_o,
            // fa-folder, fa-folder_open.
            Icons::Nerd => Self {
                workspace: "\u{f108}",
                repo: "\u{f401}",
                worktree: "\u{e725}",
                open: "\u{f111}",
                closed: "\u{f10c}",
                folded: "\u{f07b}",
                unfolded: "\u{f07c}",
                spinner,
            },
        }
    }
}

/// A glyph and its separating space, or nothing for an empty glyph.
fn icon(glyph: &'static str, style: Style) -> Option<Span<'static>> {
    (!glyph.is_empty()).then(|| Span::styled(format!("{glyph} "), style))
}

impl Palette {
    pub fn new(flavor: catppuccin::Flavor, icons: Icons) -> Self {
        let colors = flavor.colors;
        Self {
            glyphs: Glyphs::new(icons),
            accent: colors.mauve.into(),
            text: colors.text.into(),
            dim: colors.overlay1.into(),
            selection: colors.surface0.into(),
            ok: colors.green.into(),
            error: colors.red.into(),
            warn: colors.peach.into(),
            info: colors.blue.into(),
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
            Panel::Workspaces => Constraint::Fill(1),
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
    let tab = |label: &'static str, active: bool| {
        if active {
            Span::styled(label, Style::new().fg(palette.accent).bold())
        } else {
            Span::styled(label, Style::new().fg(palette.dim))
        }
    };
    let mut title = vec![Span::raw(format!("[{}] ", panel.number()))];
    match panel {
        Panel::Workspaces => {
            title.push(tab("Workspaces", list == List::Workspaces));
            title.push(Span::raw(" │ "));
            title.push(tab("Repos", list == List::Repos));
        }
        Panel::Work => {
            title.push(tab("Work", true));
            if let Some(workspace) = model.workspace() {
                title.push(Span::styled(
                    format!(" · {workspace}"),
                    Style::new().fg(palette.dim),
                ));
            }
        }
    }
    let filter = model.filter(list);
    if !filter.is_empty() || model.filtering == Some(list) {
        title.push(Span::styled(
            format!(" /{filter}"),
            Style::new().fg(palette.warn),
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
    let rows = rows(model, palette, list);
    if rows.is_empty() {
        let empty = if model.loaded {
            "nothing here"
        } else {
            "loading…"
        };
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

fn rows<'a>(model: &'a Model, palette: &Palette, list: List) -> Vec<Line<'a>> {
    let dim = Style::new().fg(palette.dim);
    match list {
        List::Workspaces => model
            .workspaces()
            .into_iter()
            .map(|name| {
                let open = model
                    .snapshot
                    .work
                    .iter()
                    .filter(|work| work.workspace == *name && work.tab)
                    .count();
                let mut spans: Vec<Span> =
                    icon(palette.glyphs.workspace, dim).into_iter().collect();
                spans.push(Span::raw(name.as_str()));
                if model.snapshot.here.as_ref() == Some(name) {
                    spans.push(Span::styled(" (here)", Style::new().fg(palette.accent)));
                }
                if open > 0 {
                    spans.push(Span::styled(format!(" {open} open"), dim));
                }
                Line::from(spans)
            })
            .collect(),
        List::Repos => model
            .repos()
            .into_iter()
            .map(|repo| {
                let mut spans: Vec<Span> = icon(palette.glyphs.repo, dim).into_iter().collect();
                spans.push(Span::raw(repo.name()));
                spans.push(Span::styled(format!(" → {}", repo.default_workspace), dim));
                Line::from(spans)
            })
            .collect(),
        List::Work => model
            .work_rows()
            .into_iter()
            .map(|line| match line {
                Row::Group {
                    name,
                    members,
                    folded,
                    ..
                } => {
                    let open = members
                        .iter()
                        .filter(|&&index| model.snapshot.work[index].tab)
                        .count();
                    Line::from(vec![
                        Span::styled(
                            format!(
                                "{} {name}",
                                if folded {
                                    palette.glyphs.folded
                                } else {
                                    palette.glyphs.unfolded
                                }
                            ),
                            Style::new().fg(palette.info).bold(),
                        ),
                        Span::styled(format!(" {} · {open} open", members.len()), dim),
                    ])
                }
                Row::Item(index) => {
                    let work = &model.snapshot.work[index];
                    let glyphs = &palette.glyphs;
                    let indent = if work.group.is_empty() { "" } else { "  " };
                    let marker = if model.pulling.contains(work.path()) {
                        let frame = glyphs.spinner[model.frame % glyphs.spinner.len()];
                        Span::styled(format!("{frame} "), Style::new().fg(palette.info))
                    } else if work.tab {
                        Span::styled(format!("{} ", glyphs.open), Style::new().fg(palette.ok))
                    } else {
                        Span::styled(format!("{} ", glyphs.closed), dim)
                    };
                    let status = if work.tree.dirty {
                        Style::new().fg(palette.warn)
                    } else {
                        dim
                    };
                    let mut spans = vec![Span::raw(indent), marker];
                    spans.extend(icon(glyphs.worktree, dim));
                    spans.push(Span::raw(work.title()));
                    if !work.tree.symbols.is_empty() {
                        spans.push(Span::styled(format!(" {}", work.tree.symbols), status));
                    }
                    if let Some((_, behind)) = work.tree.upstream.filter(|&(_, behind)| behind > 0)
                    {
                        spans.push(Span::styled(
                            format!(" ↓{behind}"),
                            Style::new().fg(palette.warn),
                        ));
                    }
                    Line::from(spans)
                }
            })
            .collect(),
    }
}

/// The key/value detail of the selection, then its recent commits.
fn detail(model: &Model) -> Vec<(String, String)> {
    let pair = |key: &str, value: String| (key.to_owned(), value);
    match model.active() {
        List::Workspaces => {
            let Some(name) = model.workspace() else {
                return Vec::new();
            };
            let work: Vec<_> = model
                .snapshot
                .work
                .iter()
                .filter(|work| work.workspace == name)
                .collect();
            let repos: Vec<String> = model
                .snapshot
                .repos
                .iter()
                .filter(|repo| repo.default_workspace == name)
                .map(|repo| repo.name())
                .collect();
            vec![
                pair("Workspace", name.to_owned()),
                pair(
                    "Session",
                    if model.snapshot.here.as_deref() == Some(name) {
                        "current".into()
                    } else {
                        "other".into()
                    },
                ),
                pair("Worktrees", work.len().to_string()),
                pair(
                    "Open tabs",
                    work.iter().filter(|work| work.tab).count().to_string(),
                ),
                pair("Default for", repos.join(", ")),
            ]
        }
        List::Repos => {
            let Some(repo) = model.repo() else {
                return Vec::new();
            };
            let count = model
                .snapshot
                .work
                .iter()
                .filter(|work| work.repo == repo.path)
                .count();
            vec![
                pair("Repo", repo.name()),
                pair("Alias", repo.alias.clone().unwrap_or_default()),
                pair("Path", repo.path.display().to_string()),
                pair("Workspace", repo.default_workspace.clone()),
                pair("Worktrees", count.to_string()),
                pair(
                    "Forge",
                    model
                        .snapshot
                        .forges
                        .get(&repo.path)
                        .map(|forge| forge.url.clone())
                        .unwrap_or_default(),
                ),
            ]
        }
        List::Work => match model.work_row() {
            Some(Row::Group { name, members, .. }) => {
                let mut pairs = vec![pair("Group", name)];
                pairs.extend(members.iter().map(|&index| {
                    let work = &model.snapshot.work[index];
                    pair("Worktree", work.title())
                }));
                pairs
            }
            Some(Row::Item(index)) => {
                let work = &model.snapshot.work[index];
                let tree = &work.tree;
                vec![
                    pair("Repo", work.repo_name.clone()),
                    pair("Branch", work.branch()),
                    pair("Path", tree.path.display().to_string()),
                    pair("Workspace", work.workspace.clone()),
                    pair("Group", work.group.clone()),
                    pair("Tab", if work.tab { "open" } else { "closed" }.into()),
                    pair(
                        "Status",
                        if tree.dirty {
                            format!("dirty +{} -{}", tree.diff.0, tree.diff.1)
                        } else {
                            "clean".into()
                        },
                    ),
                    pair(
                        "Upstream",
                        tree.upstream
                            .map(|(ahead, behind)| format!("↑{ahead} ↓{behind}"))
                            .unwrap_or_else(|| "none".into()),
                    ),
                    pair(
                        "Commit",
                        format!(
                            "{} {} ({})",
                            tree.short_sha, tree.subject, tree.committed_at
                        ),
                    ),
                ]
            }
            None => Vec::new(),
        },
    }
}

/// The selected worktree's recent commits, once loaded.
fn commits(model: &Model) -> Option<&Vec<String>> {
    match (model.active(), model.work_row()?) {
        (List::Work, Row::Item(index)) => model.commits.get(model.snapshot.work[index].path()),
        _ => None,
    }
}

/// How many lines the main view holds, so scrolling stops at its end.
pub fn main_len(model: &Model) -> usize {
    detail(model).len() + commits(model).map_or(0, |commits| commits.len() + 2)
}

fn render_main(frame: &mut Frame, model: &Model, palette: &Palette, rect: Rect) {
    let focused = model.focus == Focus::Main;
    let block = block(Line::from(" Main "), focused, palette);
    let pairs = detail(model);
    let width = pairs.iter().map(|(key, _)| key.len()).max().unwrap_or(0);
    let mut lines: Vec<Line> = pairs
        .into_iter()
        .map(|(key, value)| {
            Line::from(vec![
                Span::styled(format!("{key:width$}  "), Style::new().fg(palette.accent)),
                Span::styled(value, Style::new().fg(palette.text)),
            ])
        })
        .collect();
    if let Some(commits) = commits(model) {
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            "Recent commits",
            Style::new().fg(palette.accent).bold(),
        ));
        lines.extend(commits.iter().map(|commit| Line::raw(commit.as_str())));
    }
    frame.render_widget(
        Paragraph::new(lines).block(block).scroll(model.scroll),
        rect,
    );
}

fn render_log(frame: &mut Frame, model: &Model, palette: &Palette, rect: Rect) {
    let block = block(Line::from(" Command log "), false, palette);
    let height = block.inner(rect).height as usize;
    let lines: Vec<Line> = model.log[model.log.len().saturating_sub(height)..]
        .iter()
        .map(|entry| match &entry.error {
            None => Line::from(vec![
                Span::styled("✓ ", Style::new().fg(palette.ok)),
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
            format!("filter {}: {}", name(list), popup_hints(Popup::Filter)),
            Style::new().fg(palette.warn),
        ));
    } else {
        let active = model.active();
        for binding in KEYMAP
            .iter()
            .filter(|binding| binding.hint.contains(&active))
        {
            if !spans.is_empty() {
                spans.push(Span::styled(" · ", Style::new().fg(palette.dim)));
            }
            spans.push(Span::styled(binding.label, Style::new().fg(palette.accent)));
            spans.push(Span::raw(format!(" {}", short_help(binding.help))));
        }
    }
    let loading: Vec<&str> = model.loading.keys().copied().collect();
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
                Style::new().fg(palette.info),
            )),
            right,
        );
    }
}

/// The help's first word, so the hint bar fits.
fn short_help(help: &str) -> &str {
    help.split(' ').next().unwrap_or(help)
}

fn name(list: List) -> &'static str {
    match list {
        List::Workspaces => "workspaces",
        List::Repos => "repos",
        List::Work => "work",
    }
}
