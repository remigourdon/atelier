//! The git commands atelier runs.

use std::path::{Path, PathBuf};

use color_eyre::eyre::{Result, WrapErr, bail};

use crate::process::Runner;

/// The branch checked out at `path`, if any.
pub fn branch(runner: &dyn Runner, path: &Path) -> Option<String> {
    let path = path.to_string_lossy();
    runner
        .output(
            "git",
            &["-C", &path, "symbolic-ref", "--quiet", "--short", "HEAD"],
        )
        .ok()
        .filter(|branch| !branch.is_empty())
}

/// Whether `repo` has a local branch named `branch`. The command succeeds either way, so a new
/// branch does not show as a failure in the command log.
pub fn branch_exists(runner: &dyn Runner, repo: &Path, branch: &str) -> Result<bool> {
    let repo = repo.to_string_lossy();
    let listed = runner.output("git", &["-C", &repo, "branch", "--list", branch])?;
    Ok(!listed.is_empty())
}

pub fn pull_ff_only(runner: &dyn Runner, path: &Path) -> Result<()> {
    let path = path.to_string_lossy();
    runner
        .output("git", &["-C", &path, "pull", "--ff-only"])
        .map(drop)
}

/// The last commits at `path`, one line each.
pub fn log(runner: &dyn Runner, path: &Path) -> Result<Vec<String>> {
    let path = path.to_string_lossy();
    let log = runner.output(
        "git",
        &["-C", &path, "log", "-n", "20", "--format=%h %s (%cr, %an)"],
    )?;
    Ok(log.lines().map(Into::into).collect())
}

pub fn init(runner: &dyn Runner, path: &Path) -> Result<()> {
    runner
        .output("git", &["-C", &path.to_string_lossy(), "init", "--quiet"])
        .map(drop)
}

/// The main worktree of the repository containing `path`.
pub fn main_worktree(runner: &dyn Runner, path: &Path) -> Result<PathBuf> {
    let path = path.to_string_lossy();
    let listing = runner
        .output("git", &["-C", &path, "worktree", "list", "--porcelain"])
        .wrap_err_with(|| format!("{path} is not in a git repository"))?;
    let Some(main) = listing
        .lines()
        .next()
        .and_then(|line| line.strip_prefix("worktree "))
    else {
        bail!("git worktree list printed nothing for {path}");
    };
    Ok(std::fs::canonicalize(main)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::fake::Fake;

    #[test]
    fn main_worktree_is_the_first_listed() {
        let dir = tempfile::tempdir().unwrap();
        let listing = format!(
            "worktree {}\nHEAD abc\n\nworktree /elsewhere\n",
            dir.path().display()
        );
        let fake = Fake::default().always("git -C .", Some(&listing));
        let main = main_worktree(&fake, Path::new(".")).unwrap();
        assert_eq!(main, dir.path().canonicalize().unwrap());
        assert!(main_worktree(&Fake::default().always("git", None), Path::new(".")).is_err());
    }
}
