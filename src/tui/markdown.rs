//! A carnet's README drawn in the flavor's colours, after Catppuccin's theme for glamour, the
//! Markdown renderer of glow.

use ratatui::style::{Color, Style};
use ratatui::text::Text;
use tui_markdown::{AlertKind, Options, StyleSheet};

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
