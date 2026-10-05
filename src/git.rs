//! The git commands atelier runs.

use std::collections::HashSet;
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

/// Fast-forwards the branch at `path`, pruning remote-tracking refs whose branch is gone, so
/// a pull on a main worktree also refreshes which branches are gone.
pub fn pull_ff_only(runner: &dyn Runner, path: &Path) -> Result<()> {
    let path = path.to_string_lossy();
    runner
        .output("git", &["-C", &path, "pull", "--ff-only", "--prune"])
        .map(drop)
}

/// Fetches `repo`'s remotes, pruning remote-tracking refs whose branch is gone.
pub fn fetch_prune(runner: &dyn Runner, repo: &Path) -> Result<()> {
    let repo = repo.to_string_lossy();
    runner
        .output("git", &["-C", &repo, "fetch", "--prune"])
        .map(drop)
}

/// The local branches whose configured upstream no longer exists. Local and cheap: it reads
/// the remote-tracking refs as the last fetch left them.
pub fn gone_branches(runner: &dyn Runner, repo: &Path) -> Result<HashSet<String>> {
    let repo = repo.to_string_lossy();
    let refs = runner.output(
        "git",
        &[
            "-C",
            &repo,
            "for-each-ref",
            "refs/heads",
            "--format=%(refname:short)%00%(upstream:track)",
        ],
    )?;
    Ok(parse_gone(&refs))
}

/// `for-each-ref` lines of `<branch>\0<track>`: the branches whose track is `[gone]`.
fn parse_gone(refs: &str) -> HashSet<String> {
    (refs.lines())
        .filter_map(|line| line.split_once('\0'))
        .filter(|(_, track)| track.trim() == "[gone]")
        .map(|(branch, _)| branch.to_owned())
        .collect()
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

pub fn add(runner: &dyn Runner, path: &Path, file: &str) -> Result<()> {
    let path = path.to_string_lossy();
    runner.output("git", &["-C", &path, "add", file]).map(drop)
}

/// Commits only `file`, so nothing else staged is swept in.
pub fn commit(runner: &dyn Runner, path: &Path, message: &str, file: &str) -> Result<()> {
    let path = path.to_string_lossy();
    runner
        .output("git", &["-C", &path, "commit", "-m", message, "--", file])
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
    fn gone_branches_are_those_whose_upstream_track_is_gone() {
        let refs = "main\0\nABC-1-login\0[gone]\nABC-2-fix\0[behind 2]\nwip\0\nodd";
        let gone = parse_gone(refs);
        assert_eq!(gone, HashSet::from(["ABC-1-login".to_owned()]));
        assert!(parse_gone("").is_empty());
        let fake = Fake::default().always("git", Some("x\0[gone]"));
        assert!(gone_branches(&fake, Path::new("/r")).unwrap().contains("x"));
        assert_eq!(
            fake.calls(),
            ["git -C /r for-each-ref refs/heads --format=%(refname:short)%00%(upstream:track)"]
        );
    }

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
