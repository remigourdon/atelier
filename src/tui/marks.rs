//! What a worktree's marks mean, and how badly a worktree needs attention.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;

use super::view::{Glyphs, Palette};
use crate::worktrunk::{Checks, Ci, Decision, Worktree};

/// What part of a worktree a status symbol is about, as the detail's sections are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Part {
    Changes,
    Checkout,
    /// Against the default branch.
    Default,
    Remote,
}

impl Part {
    /// The heading of its section, in the detail and the legend.
    pub const fn section(self) -> &'static str {
        match self {
            Self::Changes => "Changes",
            Self::Checkout => "Checkout",
            Self::Default => "Default branch",
            Self::Remote => "Remote",
        }
    }
}

/// A mark's colour, by what it asks of the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    /// Something is broken: red.
    Broken,
    /// Something needs you: yellow.
    NeedsYou,
    /// Someone else's turn, as a review waiting for approval: pink.
    Waiting,
    /// Something is under way: blue.
    Busy,
    /// Done, and well: green.
    Done,
    /// Plain text.
    Neutral,
    /// Nothing to act on: grey.
    Quiet,
}

impl Tone {
    /// A status symbol's tone, grey for one worktrunk added since.
    pub fn of(mark: char) -> Self {
        lookup(mark).map_or(Self::Quiet, |symbol| symbol.tone)
    }

    pub fn color(self, palette: &Palette) -> Color {
        match self {
            Self::Broken => palette.error,
            Self::NeedsYou => palette.warn,
            Self::Waiting => palette.approval_pending,
            Self::Busy => palette.info,
            Self::Done => palette.ok,
            Self::Neutral => palette.text,
            Self::Quiet => palette.dim,
        }
    }
}

/// One of worktrunk's status symbols.
#[derive(Debug)]
pub struct Symbol {
    /// One character.
    pub mark: &'static str,
    pub part: Part,
    pub tone: Tone,
    /// What it means, in plain words.
    pub help: &'static str,
}

const fn symbol(mark: &'static str, part: Part, tone: Tone, help: &'static str) -> Symbol {
    Symbol {
        mark,
        part,
        tone,
        help,
    }
}

/// Every status symbol `wt list` draws, each in exactly one part, in the legend's order.
#[rustfmt::skip]
pub const SYMBOLS: &[Symbol] = &[
    symbol("+", Part::Changes, Tone::Neutral, "staged changes"),
    symbol("!", Part::Changes, Tone::Neutral, "unstaged changes"),
    symbol("?", Part::Changes, Tone::Neutral, "untracked files"),
    symbol("✘", Part::Checkout, Tone::Broken, "unresolved conflicts in the checkout"),
    symbol("↻", Part::Checkout, Tone::Busy, "rebase, merge or other operation in progress"),
    symbol("⊟", Part::Checkout, Tone::NeedsYou, "prunable: directory or .git missing"),
    symbol("⊞", Part::Checkout, Tone::NeedsYou, "locked"),
    symbol("⊘", Part::Checkout, Tone::NeedsYou, "detached HEAD"),
    symbol("⚐", Part::Checkout, Tone::NeedsYou, "branch checked out elsewhere, or at another path"),
    symbol("/", Part::Checkout, Tone::Quiet, "branch with no worktree"),
    symbol("^", Part::Default, Tone::Quiet, "is the main worktree"),
    symbol("∅", Part::Default, Tone::NeedsYou, "no shared history"),
    symbol("_", Part::Default, Tone::Quiet, "same commit, clean"),
    symbol("–", Part::Default, Tone::Quiet, "same commit, uncommitted changes"),
    symbol("⊂", Part::Default, Tone::Quiet, "merged"),
    symbol("✗", Part::Default, Tone::Broken, "would conflict when merged"),
    symbol("↕", Part::Default, Tone::Quiet, "ahead and behind"),
    symbol("↑", Part::Default, Tone::Quiet, "ahead"),
    symbol("↓", Part::Default, Tone::Quiet, "behind"),
    symbol("|", Part::Remote, Tone::Quiet, "in sync"),
    symbol("⇡", Part::Remote, Tone::Quiet, "ahead: unpushed commits"),
    symbol("⇣", Part::Remote, Tone::NeedsYou, "behind: commits to pull"),
    symbol("⇅", Part::Remote, Tone::NeedsYou, "diverged"),
];

/// What a status symbol means; `None` for one worktrunk added since.
pub fn lookup(mark: char) -> Option<&'static Symbol> {
    SYMBOLS.iter().find(|symbol| symbol.mark.chars().eq([mark]))
}

/// The status symbol `mark`, found while compiling: a mark not in [`SYMBOLS`] fails the build.
pub const fn find(mark: &str) -> &'static Symbol {
    let mut index = 0;
    while index < SYMBOLS.len() {
        if same(SYMBOLS[index].mark, mark) {
            return &SYMBOLS[index];
        }
        index += 1;
    }
    panic!("not a status symbol");
}

/// `a == b`, in a `const fn`.
const fn same(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut index = 0;
    while index < a.len() {
        if a[index] != b[index] {
            return false;
        }
        index += 1;
    }
    true
}

/// A worktree's status symbols about `part`.
pub fn symbols(tree: &Worktree, part: Part) -> impl Iterator<Item = &'static Symbol> + '_ {
    (tree.symbols.chars().filter_map(lookup)).filter(move |symbol| symbol.part == part)
}

/// A fact's mark: its glyph, its tone and what it means.
#[derive(Debug, Clone, Copy)]
pub struct Mark {
    pub glyph: fn(&Glyphs) -> &'static str,
    pub tone: Tone,
    pub words: &'static str,
}

impl Mark {
    pub fn style(self, palette: &Palette) -> Style {
        Style::new().fg(self.tone.color(palette))
    }
}

/// A branch's checks.
pub const fn checks(checks: Checks) -> Mark {
    match checks {
        Checks::Passed => Mark {
            glyph: |g| g.passed,
            tone: Tone::Done,
            words: "passed",
        },
        Checks::Running => Mark {
            glyph: |g| g.running,
            tone: Tone::Busy,
            words: "running",
        },
        Checks::Failed => Mark {
            glyph: |g| g.failed,
            tone: Tone::Broken,
            words: "failed",
        },
        Checks::Unavailable => Mark {
            glyph: |g| g.unavailable,
            tone: Tone::NeedsYou,
            words: "unavailable",
        },
    }
}

/// A review's decision; `None` for a draft's, which the review's line says instead.
pub const fn decision(decision: Decision) -> Option<Mark> {
    match decision {
        Decision::ChangesRequested => Some(Mark {
            glyph: |g| g.changes_requested,
            tone: Tone::NeedsYou,
            words: "changes requested",
        }),
        Decision::Pending => Some(Mark {
            glyph: |g| g.approval,
            tone: Tone::Waiting,
            words: "waiting for approval",
        }),
        Decision::Approved => Some(Mark {
            glyph: |g| g.passed,
            tone: Tone::Done,
            words: "approved",
        }),
        Decision::Draft => None,
    }
}

/// A review that conflicts with its base.
pub const CONFLICTS: Mark = Mark {
    glyph: |g| g.conflicts,
    tone: Tone::Broken,
    words: "conflicts",
};

/// The checks' mark, dimmed when stale or for a draft; `None` without checks.
pub fn checks_span(ci: &Ci, palette: &Palette) -> Option<Span<'static>> {
    let mark = checks(ci.checks?);
    let style = mark.style(palette);
    let style = if ci.checks_dimmed() {
        style.add_modifier(Modifier::DIM)
    } else {
        style
    };
    Some(Span::styled((mark.glyph)(&palette.glyphs), style))
}

/// How badly an item needs attention, least first. Only its name takes the colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Fine,
    /// Its review waits for an approval.
    Waiting,
    NeedsYou,
    Broken,
}

impl Severity {
    /// The name's colour; `None` leaves it as it is.
    pub fn color(self, palette: &Palette) -> Option<Color> {
        let tone = match self {
            Self::Fine => return None,
            Self::Waiting => Tone::Waiting,
            Self::NeedsYou => Tone::NeedsYou,
            Self::Broken => Tone::Broken,
        };
        Some(tone.color(palette))
    }

    /// A worktree's most severe fact: its status symbols, its checks unless dimmed, its review's
    /// decision and whether that review conflicts.
    pub fn of(tree: &Worktree) -> Self {
        let symbols = (tree.symbols.chars().filter_map(lookup)).map(|symbol| symbol.tone);
        let ci = tree.ci.iter().flat_map(|ci| {
            // Dimmed checks, stale or a draft's, colour no name.
            let marks = [
                (ci.checks.filter(|_| !ci.checks_dimmed())).map(checks),
                ci.decision().and_then(decision),
                ci.conflicts.then_some(CONFLICTS),
            ];
            marks.into_iter().flatten()
        });
        let tones = symbols.chain(ci.map(|mark| mark.tone));
        tones.map(Self::from).max().unwrap_or(Self::Fine)
    }
}

impl From<Tone> for Severity {
    fn from(tone: Tone) -> Self {
        match tone {
            Tone::Broken => Self::Broken,
            Tone::NeedsYou => Self::NeedsYou,
            Tone::Waiting => Self::Waiting,
            Tone::Busy | Tone::Done | Tone::Neutral | Tone::Quiet => Self::Fine,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worktrunk::{Ci, CiReview, CiState};

    fn tree(symbols: &str) -> Worktree {
        Worktree {
            symbols: symbols.into(),
            ..Worktree::default()
        }
    }

    fn with_ci(checks: Option<Checks>, decision: Option<Decision>, conflicts: bool) -> Worktree {
        Worktree {
            ci: Some(Ci {
                state: Some(CiState::Passed),
                checks,
                conflicts,
                stale: false,
                branch_workflow: false,
                review: Some(CiReview {
                    number: Some(1),
                    url: None,
                    decision,
                }),
            }),
            ..tree("")
        }
    }

    #[test]
    fn every_symbol_belongs_to_one_part() {
        let all = "+!?✘↻⊟⊞⊘⚐/^∅_–⊂✗↕↑↓|⇡⇣⇅";
        assert_eq!(SYMBOLS.len(), all.chars().count());
        assert!(all.chars().all(|mark| lookup(mark).is_some()));
        let parts: Vec<&str> = symbols(&tree("!?↑⇡"), Part::Changes)
            .map(|symbol| symbol.mark)
            .collect();
        assert_eq!(parts, ["!", "?"]);
        assert_eq!(find("⇅").help, "diverged");
    }

    #[test]
    fn severity_is_the_most_severe_fact() {
        assert_eq!(Severity::of(&tree("")), Severity::Fine);
        assert_eq!(
            Severity::of(&tree("+!?↑⇡")),
            Severity::Fine,
            "dirty is fine"
        );
        assert_eq!(Severity::of(&tree("↻")), Severity::Fine, "blue is fine");
        assert_eq!(Severity::of(&tree("!⇣")), Severity::NeedsYou);
        assert_eq!(Severity::of(&tree("⊘∅")), Severity::NeedsYou);
        assert_eq!(Severity::of(&tree("⇅✗")), Severity::Broken);
        assert_eq!(Severity::of(&tree("✘")), Severity::Broken);
        let ci = |checks, decision, conflicts| Severity::of(&with_ci(checks, decision, conflicts));
        assert_eq!(ci(Some(Checks::Failed), None, false), Severity::Broken);
        assert_eq!(ci(Some(Checks::Passed), None, true), Severity::Broken);
        assert_eq!(
            ci(Some(Checks::Unavailable), None, false),
            Severity::NeedsYou
        );
        assert_eq!(
            ci(
                Some(Checks::Running),
                Some(Decision::ChangesRequested),
                false
            ),
            Severity::NeedsYou
        );
        assert_eq!(
            ci(Some(Checks::Passed), Some(Decision::Pending), false),
            Severity::Waiting
        );
        assert_eq!(
            ci(Some(Checks::Failed), Some(Decision::Pending), false),
            Severity::Broken,
            "the worst wins"
        );
        assert_eq!(
            ci(Some(Checks::Passed), Some(Decision::Approved), false),
            Severity::Fine
        );
        assert_eq!(
            ci(Some(Checks::Failed), Some(Decision::Draft), false),
            Severity::Fine,
            "a draft's checks are dimmed, so colour nothing"
        );
        let mut stale = with_ci(Some(Checks::Failed), None, false);
        stale.ci.as_mut().unwrap().stale = true;
        assert_eq!(Severity::of(&stale), Severity::Fine, "nor do stale ones");
    }
}
