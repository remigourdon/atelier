//! External commands, behind a trait so orchestration can be tested without them.

use std::cell::RefCell;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use color_eyre::eyre::{Report, Result, WrapErr, eyre};

/// Raises this process's soft limit on open files, which every command it starts inherits: the
/// zellij server among them, which panics once its tabs' panes and plugins use the limit up, and
/// macOS starts shells at 256, about fifteen worktree tabs. 10240 is macOS's `OPEN_MAX`, beyond
/// which it refuses the call however high the hard limit.
pub fn raise_open_file_limit() {
    const WANTED: libc::rlim_t = 10240;
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: both calls only read or write the `rlimit` passed to them.
    unsafe {
        if libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) == 0 && limit.rlim_cur < WANTED {
            limit.rlim_cur = WANTED.min(limit.rlim_max);
            libc::setrlimit(libc::RLIMIT_NOFILE, &limit);
        }
    }
}

pub trait Runner {
    /// Runs a command to completion and returns its trimmed stdout, failing on a non-zero exit.
    fn output(&self, program: &str, args: &[&str]) -> Result<String>;

    /// Runs a command attached to the terminal, as `zellij attach` needs.
    fn interactive(&self, program: &str, args: &[&str]) -> Result<()>;

    /// Starts a command detached, without waiting for it, as a browser needs.
    fn spawn(&self, program: &str, args: &[&str]) -> Result<()>;
}

/// A command that ran and exited unsuccessfully.
#[derive(Debug)]
pub struct Failed {
    message: String,
    /// `None` when a signal ended it.
    code: Option<i32>,
}

impl std::fmt::Display for Failed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Failed {}

/// Whether `err` is a command that exited with `code`.
pub fn exited_with(err: &color_eyre::Report, code: i32) -> bool {
    err.downcast_ref::<Failed>()
        .is_some_and(|failed| failed.code == Some(code))
}

pub struct System;

fn launch_error(program: &str, error: std::io::Error) -> Report {
    if error.kind() == std::io::ErrorKind::NotFound {
        eyre!("command '{program}' not found on PATH")
    } else {
        Report::from(error).wrap_err(format!("starting command '{program}'"))
    }
}

impl Runner for System {
    fn output(&self, program: &str, args: &[&str]) -> Result<String> {
        let output = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .output()
            .map_err(|error| launch_error(program, error))?;
        if !output.status.success() {
            return Err(Failed {
                message: format!(
                    "{program} {} failed: {}",
                    args.join(" "),
                    String::from_utf8_lossy(&output.stderr).trim()
                ),
                code: output.status.code(),
            }
            .into());
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    }

    fn interactive(&self, program: &str, args: &[&str]) -> Result<()> {
        let status = Command::new(program)
            .args(args)
            .status()
            .map_err(|error| launch_error(program, error))?;
        if !status.success() {
            return Err(Failed {
                message: format!("{program} {} failed", args.join(" ")),
                code: status.code(),
            }
            .into());
        }
        Ok(())
    }

    fn spawn(&self, program: &str, args: &[&str]) -> Result<()> {
        Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| launch_error(program, error))?;
        Ok(())
    }
}

/// One command run through a [`Recorder`], as the command log shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct Logged {
    pub command: String,
    /// The error, when it failed.
    pub error: Option<String>,
}

/// Saves every retained entry without terminal clipping, keeping earlier exports intact.
pub fn export_log(directory: &Path, entries: &[Logged]) -> Result<PathBuf> {
    std::fs::create_dir_all(directory)
        .wrap_err_with(|| format!("creating {}", directory.display()))?;
    let timestamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let path = directory.join(format!(
        "command-log-{timestamp}-{}.log",
        std::process::id()
    ));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&path)
        .wrap_err_with(|| format!("creating {}", path.display()))?;
    for entry in entries {
        let line = match &entry.error {
            Some(error) => format!("✗ {}: {error}", entry.command),
            None => format!("✓ {}", entry.command),
        };
        writeln!(file, "{line}").wrap_err_with(|| format!("writing {}", path.display()))?;
    }
    Ok(path)
}

/// Runs commands through another runner and records each one.
pub struct Recorder<'a> {
    inner: &'a dyn Runner,
    log: RefCell<Vec<Logged>>,
}

impl<'a> Recorder<'a> {
    pub fn new(inner: &'a dyn Runner) -> Self {
        Self {
            inner,
            log: RefCell::default(),
        }
    }

    pub fn take(&self) -> Vec<Logged> {
        self.log.take()
    }

    fn record<T>(&self, program: &str, args: &[&str], result: Result<T>) -> Result<T> {
        let command = std::iter::once(program)
            .chain(args.iter().copied())
            .collect::<Vec<_>>()
            .join(" ");
        let error = result.as_ref().err().map(|err| err.to_string());
        self.log.borrow_mut().push(Logged { command, error });
        result
    }
}

impl Runner for Recorder<'_> {
    fn output(&self, program: &str, args: &[&str]) -> Result<String> {
        self.record(program, args, self.inner.output(program, args))
    }

    fn interactive(&self, program: &str, args: &[&str]) -> Result<()> {
        self.record(program, args, self.inner.interactive(program, args))
    }

    fn spawn(&self, program: &str, args: &[&str]) -> Result<()> {
        self.record(program, args, self.inner.spawn(program, args))
    }
}

#[cfg(test)]
mod tests {
    use super::fake::Fake;
    use super::*;

    #[test]
    fn exports_complete_entries_and_keeps_previous_exports() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("logs");
        let long_command = format!("git {}", "long-argument".repeat(100));
        let error = "first line\nsecond line\n".to_owned() + &"details".repeat(100);
        let entries = vec![
            Logged {
                command: "git status".into(),
                error: None,
            },
            Logged {
                command: long_command.clone(),
                error: Some(error.clone()),
            },
        ];
        let path = export_log(&directory, &entries).unwrap();
        let expected = format!("✓ git status\n✗ {long_command}: {error}\n");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), expected);
        let empty = export_log(&directory, &[]).unwrap();
        assert_ne!(path, empty);
        assert_eq!(std::fs::read_to_string(empty).unwrap(), "");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), expected);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn export_reports_an_unwritable_destination() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("file");
        std::fs::write(&directory, "keep").unwrap();
        let error = export_log(&directory, &[]).unwrap_err();
        assert!(format!("{error:#}").contains(&directory.display().to_string()));
        assert_eq!(std::fs::read_to_string(directory).unwrap(), "keep");
    }

    #[test]
    fn recorder_logs_successes_and_failures() {
        let fake = Fake::default().always("git fail", None);
        let recorder = Recorder::new(&fake);
        assert!(recorder.output("git", &["ok"]).is_ok());
        assert!(recorder.output("git", &["fail"]).is_err());
        let log = recorder.take();
        assert_eq!(log[0].command, "git ok");
        assert_eq!(log[0].error, None);
        assert_eq!(log[1].error.as_deref(), Some("git fail failed"));
        assert!(recorder.take().is_empty());
    }

    #[test]
    fn missing_commands_report_the_program_and_path() {
        let error = System.output("atelier-test-missing-cli", &[]).unwrap_err();
        assert_eq!(
            error.to_string(),
            "command 'atelier-test-missing-cli' not found on PATH"
        );
    }

    #[test]
    fn failures_keep_their_exit_code() {
        let failed = System.output("sh", &["-c", "exit 1"]).unwrap_err();
        assert!(exited_with(&failed, 1));
        assert!(!exited_with(&failed, 2));
        let recorder = Recorder::new(&System);
        let failed = recorder.interactive("sh", &["-c", "exit 2"]).unwrap_err();
        assert!(exited_with(&failed, 2), "through a recorder too");
    }
}

#[cfg(test)]
pub mod fake {
    use std::collections::VecDeque;

    use color_eyre::eyre::bail;

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

        fn spawn(&self, program: &str, args: &[&str]) -> Result<()> {
            self.output(program, args).map(drop)
        }
    }
}
