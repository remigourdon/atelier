//! External commands, behind a trait so orchestration can be tested without them.

use std::path::Path;
use std::process::{Command, Stdio};

use color_eyre::eyre::{Result, bail};

pub trait Runner {
    /// Runs a command to completion and returns its trimmed stdout, failing on a non-zero exit.
    fn output(&self, program: &str, args: &[&str]) -> Result<String>;

    /// Runs a command attached to the terminal, as `zellij attach` needs.
    fn interactive(&self, program: &str, args: &[&str]) -> Result<()>;
}

pub struct System;

impl Runner for System {
    fn output(&self, program: &str, args: &[&str]) -> Result<String> {
        let output = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .output()?;
        if !output.status.success() {
            bail!(
                "{program} {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    }

    fn interactive(&self, program: &str, args: &[&str]) -> Result<()> {
        let status = Command::new(program).args(args).status()?;
        if !status.success() {
            bail!("{program} {} failed", args.join(" "));
        }
        Ok(())
    }
}

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

#[cfg(test)]
pub mod fake {
    use std::cell::RefCell;
    use std::collections::VecDeque;

    use super::*;

    /// Records every command and answers from scripted `(prefix, result)` pairs, first match wins.
    /// A pair whose result is `None` fails. Unmatched commands succeed with empty output.
    #[derive(Default)]
    pub struct Fake {
        pub calls: RefCell<Vec<String>>,
        replies: RefCell<VecDeque<(String, Option<String>)>>,
        sticky: RefCell<Vec<(String, Option<String>)>>,
    }

    impl Fake {
        /// Answers the next command starting with `prefix` once.
        pub fn once(self, prefix: &str, reply: Option<&str>) -> Self {
            self.replies
                .borrow_mut()
                .push_back((prefix.into(), reply.map(Into::into)));
            self
        }

        /// Answers every command starting with `prefix`.
        pub fn always(self, prefix: &str, reply: Option<&str>) -> Self {
            self.sticky
                .borrow_mut()
                .push((prefix.into(), reply.map(Into::into)));
            self
        }

        pub fn calls(&self) -> Vec<String> {
            self.calls.borrow().clone()
        }

        fn reply(&self, line: &str) -> Result<String> {
            let mut replies = self.replies.borrow_mut();
            let found = match replies
                .iter()
                .position(|(p, _)| line.starts_with(p.as_str()))
            {
                Some(index) => replies.remove(index).map(|(_, r)| r),
                None => self
                    .sticky
                    .borrow()
                    .iter()
                    .find(|(p, _)| line.starts_with(p.as_str()))
                    .map(|(_, r)| r.clone()),
            };
            match found {
                Some(Some(out)) => Ok(out),
                Some(None) => bail!("{line} failed"),
                None => Ok(String::new()),
            }
        }
    }

    impl Runner for Fake {
        fn output(&self, program: &str, args: &[&str]) -> Result<String> {
            let line = std::iter::once(program)
                .chain(args.iter().copied())
                .collect::<Vec<_>>()
                .join(" ");
            self.calls.borrow_mut().push(line.clone());
            self.reply(&line)
        }

        fn interactive(&self, program: &str, args: &[&str]) -> Result<()> {
            self.output(program, args).map(drop)
        }
    }
}
