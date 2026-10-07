//! What worktrunk's status symbols mean, and how badly a worktree needs attention.

use ratatui::style::Color;

use super::view::Palette;
use crate::worktrunk::{Checks, Decision, Worktree};

/// What part of a worktree a status symbol is about, as the detail's sections are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Part {
    Changes,
    Checkout,
    /// Against the default branch.
    Default,
    Remote,
}

/// A mark's colour, by what it asks of the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    /// Something is broken: red.
    Broken,
    /// Something needs you: yellow.
    NeedsYou,
    /// Something is under way: blue.
    Busy,
    /// Plain text.
    Neutral,
    /// Nothing to act on: grey.
    Quiet,
}

impl Tone {
    pub fn color(self, palette: &Palette) -> Color {
        match self {
            Self::Broken => palette.error,
            Self::NeedsYou => palette.warn,
            Self::Busy => palette.info,
            Self::Neutral => palette.text,
            Self::Quiet => palette.dim,
        }
    }
}

/// One of worktrunk's status symbols.
#[derive(Debug)]
pub struct Symbol {
    pub mark: char,
    pub part: Part,
    pub tone: Tone,
    /// What it means, in plain words.
    pub help: &'static str,
}

const fn symbol(mark: char, part: Part, tone: Tone, help: &'static str) -> Symbol {
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
    symbol('+', Part::Changes, Tone::Neutral, "staged changes"),
    symbol('!', Part::Changes, Tone::Neutral, "unstaged changes"),
    symbol('?', Part::Changes, Tone::Neutral, "untracked files"),
    symbol('✘', Part::Checkout, Tone::Broken, "merge conflicts"),
    symbol('↻', Part::Checkout, Tone::Busy, "rebase, merge or other operation in progress"),
    symbol('⊟', Part::Checkout, Tone::NeedsYou, "prunable: directory or .git missing"),
    symbol('⊞', Part::Checkout, Tone::NeedsYou, "locked"),
    symbol('⊘', Part::Checkout, Tone::NeedsYou, "detached HEAD"),
    symbol('⚐', Part::Checkout, Tone::NeedsYou, "branch checked out elsewhere, or at another path"),
    symbol('/', Part::Checkout, Tone::Quiet, "branch with no worktree"),
    symbol('^', Part::Default, Tone::Quiet, "is the main worktree"),
    symbol('∅', Part::Default, Tone::NeedsYou, "no shared history"),
    symbol('_', Part::Default, Tone::Quiet, "same commit, clean"),
    symbol('–', Part::Default, Tone::Quiet, "same commit, uncommitted changes"),
    symbol('⊂', Part::Default, Tone::Quiet, "merged"),
    symbol('✗', Part::Default, Tone::Broken, "would conflict when merged"),
    symbol('↕', Part::Default, Tone::Quiet, "ahead and behind"),
    symbol('↑', Part::Default, Tone::Quiet, "ahead"),
    symbol('↓', Part::Default, Tone::Quiet, "behind"),
    symbol('|', Part::Remote, Tone::Quiet, "in sync"),
    symbol('⇡', Part::Remote, Tone::Quiet, "ahead: unpushed commits"),
    symbol('⇣', Part::Remote, Tone::NeedsYou, "behind: commits to pull"),
    symbol('⇅', Part::Remote, Tone::NeedsYou, "diverged"),
];

/// What a status symbol means; `None` for one worktrunk added since.
pub fn lookup(mark: char) -> Option<&'static Symbol> {
    SYMBOLS.iter().find(|symbol| symbol.mark == mark)
}

/// A worktree's status symbols about `part`.
pub fn symbols(tree: &Worktree, part: Part) -> impl Iterator<Item = &'static Symbol> + '_ {
    (tree.symbols.chars().filter_map(lookup)).filter(move |symbol| symbol.part == part)
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
        match self {
            Self::Fine => None,
            Self::Waiting => Some(palette.approval_pending),
            Self::NeedsYou => Some(Tone::NeedsYou.color(palette)),
            Self::Broken => Some(Tone::Broken.color(palette)),
        }
    }

    /// A worktree's most severe fact: its status symbols, its checks, its review's decision and
    /// whether that review conflicts.
    pub fn of(tree: &Worktree) -> Self {
        let symbols = (tree.symbols.chars().filter_map(lookup)).map(|symbol| match symbol.tone {
            Tone::Broken => Self::Broken,
            Tone::NeedsYou => Self::NeedsYou,
            Tone::Busy | Tone::Neutral | Tone::Quiet => Self::Fine,
        });
        let ci = tree.ci.iter().flat_map(|ci| {
            let checks = match ci.checks {
                Some(Checks::Failed) => Self::Broken,
                Some(Checks::Unavailable) => Self::NeedsYou,
                Some(Checks::Passed | Checks::Running) | None => Self::Fine,
            };
            let conflicts = if ci.conflicts {
                Self::Broken
            } else {
                Self::Fine
            };
            let decision = match ci.decision() {
                Some(Decision::ChangesRequested) => Self::NeedsYou,
                Some(Decision::Pending) => Self::Waiting,
                Some(Decision::Approved | Decision::Draft) | None => Self::Fine,
            };
            [checks, conflicts, decision]
        });
        symbols.chain(ci).max().unwrap_or(Self::Fine)
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
        let parts: Vec<char> = symbols(&tree("!?↑⇡"), Part::Changes)
            .map(|symbol| symbol.mark)
            .collect();
        assert_eq!(parts, ['!', '?']);
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
    }
}
