//! Popups: prompts, confirmations, menus and finish plans.

use ratatui::Frame;
use ratatui::layout::{Constraint, Flex, Layout, Position, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph, Wrap};

use super::app::{Modal, Popup, finish_hints, popup_hints};
use super::view::{Palette, offset};
use crate::finish::{Line as PlanLine, Plan, Signal};

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let [row] = Layout::vertical([Constraint::Length(height)])
        .flex(Flex::Center)
        .areas(area);
    let [rect] = Layout::horizontal([Constraint::Length(width)])
        .flex(Flex::Center)
        .areas(row);
    rect
}

/// A bordered popup with its accept and cancel keys, `hints`, on the bottom border.
fn popup(frame: &mut Frame, hints: String, title: &str, rect: Rect, palette: &Palette) -> Rect {
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(palette.accent))
        .title(format!(" {title} "))
        .title_bottom(Line::styled(
            format!(" {hints} "),
            Style::new().fg(palette.dim),
        ));
    let inner = block.inner(rect);
    frame.render_widget(Clear, rect);
    frame.render_widget(block, rect);
    inner
}

pub fn modal(frame: &mut Frame, modal: &Modal, palette: &Palette) {
    let area = frame.area();
    let width = area.width.saturating_sub(4).min(72);
    match modal {
        Modal::Prompt { title, input, .. } => {
            let rect = centered(area, width, 3);
            let inner = popup(frame, popup_hints(Popup::Prompt), title, rect, palette);
            let scroll = input.visual_scroll(inner.width.saturating_sub(1) as usize);
            frame.render_widget(
                Paragraph::new(input.value()).scroll((0, scroll as u16)),
                inner,
            );
            frame.set_cursor_position(Position::new(
                inner.x + (input.visual_cursor().saturating_sub(scroll)) as u16,
                inner.y,
            ));
        }
        Modal::Confirm { title, lines, .. } => {
            let height = (lines.len() as u16 + 2).min(area.height);
            let rect = centered(area, width, height);
            let inner = popup(frame, popup_hints(Popup::Confirm), title, rect, palette);
            let text: Vec<Line> = lines.iter().map(|line| Line::raw(line.as_str())).collect();
            frame.render_widget(Paragraph::new(text).wrap(Wrap { trim: false }), inner);
        }
        Modal::Menu {
            title,
            entries,
            selected,
        } => {
            let height = (entries.len() as u16 + 2).min(area.height.saturating_sub(2));
            let rect = centered(area, width, height);
            let inner = popup(frame, popup_hints(Popup::Menu), title, rect, palette);
            let key_width = entries
                .iter()
                .map(|e| e.key.chars().count())
                .max()
                .unwrap_or(0);
            let start = offset(*selected, inner.height);
            let lines: Vec<Line> = entries
                .iter()
                .enumerate()
                .skip(start)
                .take(inner.height as usize)
                .map(|(index, entry)| {
                    let line = Line::from(vec![
                        Span::styled(
                            format!("{:key_width$}  ", entry.key),
                            Style::new().fg(palette.accent),
                        ),
                        Span::raw(entry.label.as_str()),
                    ]);
                    if index == *selected {
                        line.style(Style::new().bg(palette.selection).bold())
                    } else {
                        line
                    }
                })
                .collect();
            frame.render_widget(Paragraph::new(lines), inner);
        }
        Modal::Finish { plan, selected } => {
            let lines = finish_lines(plan, palette);
            let hints = finish_hints(plan.checked().len());
            // As wide as its longest line, title or keys, so notes are not cut.
            let widest = (lines.iter().map(Line::width))
                .chain([plan.title.chars().count(), hints.chars().count()].map(|len| len + 4))
                .max()
                .unwrap_or(0);
            let width = (widest as u16 + 2).max(width).min(area.width);
            let height = (plan.lines.len() as u16 + 2).min(area.height.saturating_sub(2));
            let rect = centered(area, width, height);
            let inner = popup(frame, hints, &plan.title, rect, palette);
            let start = offset(*selected, inner.height);
            let lines: Vec<Line> = (lines.into_iter().enumerate())
                .skip(start)
                .take(inner.height as usize)
                .map(|(index, line)| {
                    if index == *selected {
                        line.style(Style::new().bg(palette.selection).bold())
                    } else {
                        line
                    }
                })
                .collect();
            frame.render_widget(Paragraph::new(lines), inner);
        }
    }
}

/// A finish plan's lines: a checkbox on each step, a mark and a note after each label.
fn finish_lines<'a>(plan: &'a Plan, palette: &Palette) -> Vec<Line<'a>> {
    let dim = Style::new().fg(palette.dim);
    let label_width = (plan.lines.iter())
        .filter_map(|line| match line {
            PlanLine::Step { label, .. } | PlanLine::Info { label, .. } => {
                Some(label.chars().count())
            }
            _ => None,
        })
        .max()
        .unwrap_or(0);
    (plan.lines.iter())
        .map(|line| match line {
            PlanLine::Warning(text) => {
                Line::styled(format!("! {text}"), Style::new().fg(palette.warn))
            }
            PlanLine::Section(name) => {
                Line::styled(name.as_str(), Style::new().fg(palette.info).bold())
            }
            PlanLine::Info { label, note } => Line::from(vec![
                Span::styled(format!("    {label:label_width$}    "), dim),
                Span::styled(note.as_str(), dim),
            ]),
            PlanLine::Step {
                label,
                note,
                signal,
                dirty,
                checked,
                ..
            } => {
                let box_ = if *checked { "[x] " } else { "[ ] " };
                let mark = match (dirty, signal) {
                    (true, _) => Some(Span::styled("! ", Style::new().fg(palette.warn))),
                    (_, Some(Signal::Integrated)) => {
                        Some(Span::styled(format!("{} ", palette.glyphs.integrated), dim))
                    }
                    (_, Some(Signal::Gone)) => {
                        Some(Span::styled(format!("{} ", palette.glyphs.gone), dim))
                    }
                    (_, None) => Some(Span::raw("  ")),
                };
                let mut spans = vec![
                    Span::styled(box_, Style::new().fg(palette.accent)),
                    Span::raw(format!("{label:label_width$}  ")),
                ];
                spans.extend(mark);
                spans.push(Span::styled(note.as_str(), dim));
                Line::from(spans)
            }
        })
        .collect()
}
