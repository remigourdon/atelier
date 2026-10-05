//! The selectable lists: one module per kind of list, each owning its rows, its detail and
//! what each key does on its selection.

pub mod carnets;
mod issues;
mod repos;
mod reviews;
pub mod work;
mod workspaces;

use std::path::PathBuf;

use ratatui::text::Line;

use super::app::{Cmd, Effect, Job, Kind, List, Model, Work};
use super::update::run;
use super::view::Palette;
use crate::finish::Scope;

/// A kind of list. Methods take the `List`, so the issue sections share one implementation;
/// operations a list doesn't support do nothing.
pub trait ListKind: Sync {
    /// What the keymap calls it.
    fn kind(&self) -> Kind;
    fn title<'a>(&self, model: &'a Model, list: List) -> &'a str;
    fn len(&self, model: &Model, list: List) -> usize;
    /// Each row's identity, which keeps the selection across a refresh.
    fn ids(&self, model: &Model, list: List) -> Vec<String>;
    fn rows<'a>(&self, model: &'a Model, palette: &Palette, list: List) -> Vec<Line<'a>>;
    /// The key/value detail of the selection, atop the main view.
    fn detail(&self, model: &Model, list: List) -> Vec<(String, String)>;
    /// What an empty list says.
    fn empty(&self, model: &Model, _list: List) -> &'static str {
        if model.loaded {
            "nothing here"
        } else {
            "loading…"
        }
    }
    /// The selected item whose README and commits the main view shows.
    fn item<'a>(&self, _model: &'a Model, _list: List) -> Option<&'a Work> {
        None
    }
    fn activate(&self, _model: &mut Model, _list: List) -> Vec<Effect> {
        Vec::new()
    }
    /// `Enter` on the selection; `false` when it focuses the main view instead.
    fn enter(&self, _model: &mut Model, _list: List) -> bool {
        false
    }
    fn create(&self, _model: &mut Model, _list: List) -> Vec<Effect> {
        Vec::new()
    }
    fn edit(&self, _model: &mut Model, _list: List) -> Vec<Effect> {
        Vec::new()
    }
    fn move_to(&self, _model: &mut Model, _list: List) -> Vec<Effect> {
        Vec::new()
    }
    fn remove(&self, _model: &mut Model, _list: List) -> Vec<Effect> {
        Vec::new()
    }
    /// `f`: fetches, then shows the finish plan of what the selection covers.
    fn finish(&self, _model: &mut Model, _list: List) -> Vec<Effect> {
        Vec::new()
    }
    /// `Esc` on the list, once its filter is clear; `false` when it does nothing.
    fn back(&self, _model: &mut Model, _list: List) -> bool {
        false
    }
    /// A command only some lists act on, such as `x`, `c` or `p`; the others ignore it.
    fn command(&self, _model: &mut Model, _list: List, _cmd: Cmd) -> Vec<Effect> {
        Vec::new()
    }
    fn copy_path(&self, _model: &Model, _list: List) -> Option<String> {
        None
    }
    fn branch(&self, _model: &Model, _list: List) -> Option<String> {
        None
    }
    fn url(&self, _model: &Model, _list: List) -> Option<String> {
        None
    }
}

/// The behaviour of a list.
pub fn of(list: List) -> &'static dyn ListKind {
    match list {
        List::Workspaces => &workspaces::Workspaces,
        List::Repos => &repos::Repos,
        List::Work => &work::WorkList,
        List::Carnets => &carnets::Carnets,
        List::ToReview | List::Mine => &reviews::Reviews,
        List::Section(_) => &issues::Issues,
    }
}

/// Fetches the repos `scope` may touch, then builds its plan.
fn plan(model: &mut Model, scope: Scope) -> Vec<Effect> {
    let repos = scope.repos(&model.snapshot);
    vec![run(model, Job::Plan { scope, repos })]
}

fn paths(works: &[&Work]) -> Vec<PathBuf> {
    works.iter().map(|work| work.path().clone()).collect()
}

/// Closes the given tabs, doing nothing for an empty selection.
fn close_tabs(model: &mut Model, paths: Vec<PathBuf>) -> Vec<Effect> {
    if paths.is_empty() {
        Vec::new()
    } else {
        vec![run(model, Job::Close(paths))]
    }
}

/// What the main view calls an item.
fn kind(work: &Work) -> &'static str {
    if work.is_carnet() {
        "Carnet"
    } else {
        "Worktree"
    }
}

fn pair(key: &str, value: String) -> (String, String) {
    (key.to_owned(), value)
}
