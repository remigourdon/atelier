//! A carnet's README drawn in the flavor's colours, after Catppuccin's theme for glamour, the
//! Markdown renderer of glow.

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span, Text};
use tui_markdown::{AlertKind, Options, StyleSheet};
use unicode_width::UnicodeWidthChar;

/// The widest a README is drawn, however wide its pane.
pub const MAX_WIDTH: u16 = 120;

/// tui-markdown's styles in a Catppuccin flavor.
#[derive(Clone, Debug)]
pub struct Markdown {
    /// H1 to H6: the flavor's rainbow.
    headings: [Color; 6],
    code: Color,
    code_background: Color,
    link: Color,
    quote: Color,
    list_marker: Color,
    table_header: Color,
    table_border: Color,
    math: Color,
    /// Front matter, as `subtle` text.
    metadata: Color,
    note: Color,
    tip: Color,
    important: Color,
    warning: Color,
    caution: Color,
}

impl Markdown {
    pub fn new(colors: &catppuccin::FlavorColors) -> Self {
        Self {
            headings: [
                colors.red.into(),
                colors.peach.into(),
                colors.yellow.into(),
                colors.green.into(),
                colors.sapphire.into(),
                colors.lavender.into(),
            ],
            code: colors.maroon.into(),
            code_background: colors.mantle.into(),
            link: colors.blue.into(),
            quote: colors.subtext0.into(),
            list_marker: colors.teal.into(),
            table_header: colors.text.into(),
            table_border: colors.overlay0.into(),
            math: colors.blue.into(),
            metadata: colors.overlay1.into(),
            note: colors.blue.into(),
            tip: colors.green.into(),
            important: colors.mauve.into(),
            warning: colors.yellow.into(),
            caution: colors.red.into(),
        }
    }

    /// `text` rendered with these styles.
    pub fn render<'a>(&self, text: &'a str) -> Text<'a> {
        tui_markdown::from_str_with_options(text, &Options::new(self.clone()))
    }
}

/// One character of a line and its style, as [`wrap`] lays a line out.
#[derive(Clone, Copy)]
struct Cell {
    c: char,
    style: Style,
}

impl Cell {
    fn is_space(self) -> bool {
        self.c == ' '
    }

    /// The columns it takes.
    fn width(self) -> usize {
        self.c.width().unwrap_or(0)
    }
}

/// `text`'s lines wrapped at `width` columns as glow wraps them: between words, a word wider
/// than a line broken where it reaches the edge, code blocks included. A blank line stays.
pub fn wrap<'a>(text: Text<'a>, width: u16) -> Vec<Line<'static>> {
    let width = usize::from(width.max(1));
    let mut lines = Vec::new();
    for line in text.lines {
        let cells: Vec<Cell> = (line.spans.iter())
            .flat_map(|span| {
                (span.content.chars()).map(move |c| Cell {
                    c,
                    style: span.style,
                })
            })
            .collect();
        let mut wrapped: Vec<Vec<Cell>> = Vec::new();
        let mut current: Vec<Cell> = Vec::new();
        let mut used = 0;
        let mut rest = &cells[..];
        while !rest.is_empty() {
            // A word, then the spaces after it.
            let body = rest.iter().take_while(|cell| !cell.is_space()).count();
            let spaces = rest[body..]
                .iter()
                .take_while(|cell| cell.is_space())
                .count();
            let (word, after) = (&rest[..body], &rest[body..body + spaces]);
            let word_width: usize = word.iter().map(|cell| cell.width()).sum();
            if used > 0 && used + word_width > width {
                trim_end(&mut current);
                wrapped.push(std::mem::take(&mut current));
                used = 0;
            }
            for &cell in word.iter().chain(after) {
                if used + cell.width() > width {
                    if cell.is_space() {
                        continue;
                    }
                    wrapped.push(std::mem::take(&mut current));
                    used = 0;
                }
                current.push(cell);
                used += cell.width();
            }
            rest = &rest[body + spaces..];
        }
        wrapped.push(current);
        lines.extend(wrapped.into_iter().map(|cells| {
            let mut wrapped = Line::from(spans(cells)).style(line.style);
            wrapped.alignment = line.alignment;
            wrapped
        }));
    }
    lines
}

fn trim_end(cells: &mut Vec<Cell>) {
    while cells.last().is_some_and(|cell| cell.is_space()) {
        cells.pop();
    }
}

/// Cells back into spans, one per run of a style.
fn spans(cells: Vec<Cell>) -> Vec<Span<'static>> {
    let mut spans: Vec<(String, Style)> = Vec::new();
    for Cell { c, style } in cells {
        match spans.last_mut() {
            Some((text, last)) if *last == style => text.push(c),
            _ => spans.push((c.to_string(), style)),
        }
    }
    (spans.into_iter())
        .map(|(text, style)| Span::styled(text, style))
        .collect()
}

impl StyleSheet for Markdown {
    /// The top two levels bold, as glamour's.
    fn heading(&self, level: u8) -> Style {
        let index = usize::from(level.clamp(1, 6)) - 1;
        let style = Style::new().fg(self.headings[index]);
        if level <= 2 { style.bold() } else { style }
    }

    fn code(&self) -> Style {
        Style::new().fg(self.code).bg(self.code_background)
    }

    fn link(&self) -> Style {
        Style::new().fg(self.link).underlined()
    }

    fn blockquote(&self) -> Style {
        Style::new().fg(self.quote).italic()
    }

    fn metadata_block(&self) -> Style {
        Style::new().fg(self.metadata)
    }

    fn math_inline(&self) -> Style {
        Style::new().fg(self.math).italic()
    }

    fn math_display(&self) -> Style {
        Style::new().fg(self.math)
    }

    fn alert(&self, kind: AlertKind) -> Style {
        Style::new().fg(match kind {
            AlertKind::Note => self.note,
            AlertKind::Tip => self.tip,
            AlertKind::Important => self.important,
            AlertKind::Warning => self.warning,
            AlertKind::Caution => self.caution,
        })
    }

    fn table_header(&self) -> Style {
        Style::new().fg(self.table_header).bold()
    }

    fn table_border(&self) -> Style {
        Style::new().fg(self.table_border)
    }

    fn list_marker(&self) -> Style {
        Style::new().fg(self.list_marker)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(lines: &[Line]) -> Vec<String> {
        lines.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn wraps_between_words_and_breaks_a_word_too_wide() {
        let text = Text::from(vec![
            Line::from("one two three four"),
            Line::from(""),
            Line::from(vec![
                Span::raw("ab"),
                Span::styled("cdefgh", Style::new().bold()),
            ]),
        ]);
        let lines = wrap(text, 9);
        assert_eq!(plain(&lines), ["one two", "three", "four", "", "abcdefgh"]);
        assert_eq!(
            plain(&wrap(Text::from("abcdefghij"), 4)),
            ["abcd", "efgh", "ij"]
        );
        assert_eq!(lines[4].spans[1].style, Style::new().bold(), "styles kept");
    }

    #[test]
    fn code_keeps_its_indent_on_its_first_line() {
        let lines = wrap(Text::from("    let x = 1;"), 10);
        assert_eq!(plain(&lines), ["    let x", "= 1;"]);
    }
}
