//! Popups: prompts, confirmations and menus.

use ratatui::Frame;
use ratatui::layout::{Constraint, Flex, Layout, Position, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph, Wrap};

use super::app::Modal;
use super::view::{Theme, offset};

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let [row] = Layout::vertical([Constraint::Length(height)])
        .flex(Flex::Center)
        .areas(area);
    let [rect] = Layout::horizontal([Constraint::Length(width)])
        .flex(Flex::Center)
        .areas(row);
    rect
}

fn popup(frame: &mut Frame, title: &str, rect: Rect, theme: &Theme) -> Rect {
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme.accent))
        .title(format!(" {title} "));
    let inner = block.inner(rect);
    frame.render_widget(Clear, rect);
    frame.render_widget(block, rect);
    inner
}

pub fn modal(frame: &mut Frame, modal: &Modal, theme: &Theme) {
    let area = frame.area();
    let width = area.width.saturating_sub(4).min(72);
    match modal {
        Modal::Prompt { title, input, .. } => {
            let rect = centered(area, width, 3);
            let inner = popup(frame, title, rect, theme);
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
            let height = (lines.len() as u16 + 4).min(area.height);
            let rect = centered(area, width, height);
            let inner = popup(frame, title, rect, theme);
            let mut text: Vec<Line> = lines.iter().map(|line| Line::raw(line.as_str())).collect();
            text.push(Line::raw(""));
            text.push(Line::styled(
                "Enter/y confirm · Esc/n cancel",
                Style::new().fg(theme.dim),
            ));
            frame.render_widget(Paragraph::new(text).wrap(Wrap { trim: false }), inner);
        }
        Modal::Menu {
            title,
            entries,
            selected,
        } => {
            let height = (entries.len() as u16 + 2).min(area.height.saturating_sub(2));
            let rect = centered(area, width, height);
            let inner = popup(frame, title, rect, theme);
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
                            Style::new().fg(theme.accent),
                        ),
                        Span::raw(entry.label.as_str()),
                    ]);
                    if index == *selected {
                        line.style(Style::new().bg(theme.selection).bold())
                    } else {
                        line
                    }
                })
                .collect();
            frame.render_widget(Paragraph::new(lines), inner);
        }
    }
}
