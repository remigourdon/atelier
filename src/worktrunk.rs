//! worktrunk's user config: atelier's entries among the user's own hooks.

use std::path::{Path, PathBuf};

use color_eyre::eyre::{Result, WrapErr, bail};
use toml_edit::{DocumentMut, Item, Table, value};

use crate::hooks::Phase;

/// The key atelier's command sits under in each hook's named table.
const ENTRY: &str = "atelier";

/// worktrunk's user config file.
pub fn config_path() -> PathBuf {
    crate::config::config_home().join("worktrunk/config.toml")
}

/// worktrunk's config as read from disk, so edits keep the user's formatting.
pub struct HooksConfig {
    path: PathBuf,
    original: String,
    doc: DocumentMut,
}

impl HooksConfig {
    /// Reads the file; a missing one is empty.
    pub fn load(path: &Path) -> Result<Self> {
        let original = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(err) => return Err(err.into()),
        };
        let doc = original
            .parse()
            .wrap_err_with(|| format!("parsing {}", path.display()))?;
        Ok(Self {
            path: path.to_owned(),
            original,
            doc,
        })
    }

    /// Writes the file back if it changed. Returns whether it did.
    pub fn save(&self) -> Result<bool> {
        let text = self.doc.to_string();
        if text == self.original {
            return Ok(false);
        }
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&self.path, text)?;
        Ok(true)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Whether `phase` runs atelier's hook, in either named or plain-string form.
    pub fn installed(&self, phase: Phase) -> bool {
        let ours = phase.command();
        match self.doc.get(phase.name()) {
            Some(item) if item.is_str() => item.as_str() == Some(&ours),
            Some(item) => entry(item) == Some(&ours),
            None => false,
        }
    }

    /// Adds a named `atelier` entry to each phase, keeping the user's other hooks.
    pub fn install(&mut self) -> Result<()> {
        for phase in Phase::ALL {
            let ours = phase.command();
            let item = self
                .doc
                .entry(phase.name())
                .or_insert_with(|| Item::Table(Table::new()));
            if let Some(existing) = item.as_str().map(str::to_owned) {
                let mut table = Table::new();
                if existing != ours {
                    table.insert("default", value(existing));
                }
                *item = Item::Table(table);
            }
            let Some(table) = item.as_table_like_mut() else {
                bail!(
                    "{} is a hook pipeline; add `{ENTRY} = \"{ours}\"` to a step by hand",
                    phase.name()
                );
            };
            table.insert(ENTRY, value(ours));
        }
        Ok(())
    }

    /// Removes atelier's entries, dropping phases left empty.
    pub fn uninstall(&mut self) {
        for phase in Phase::ALL {
            let ours = phase.command();
            let Some(item) = self.doc.get_mut(phase.name()) else {
                continue;
            };
            let empty = if item.is_str() {
                item.as_str() == Some(&ours)
            } else if let Some(table) = item.as_table_like_mut() {
                if table.get(ENTRY).and_then(Item::as_str) == Some(&ours) {
                    table.remove(ENTRY);
                }
                table.is_empty()
            } else {
                false
            };
            if empty {
                self.doc.remove(phase.name());
            }
        }
    }
}

fn entry(item: &Item) -> Option<&str> {
    item.as_table_like()?.get(ENTRY)?.as_str()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(text: &str) -> (tempfile::TempDir, HooksConfig) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("worktrunk/config.toml");
        if !text.is_empty() {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, text).unwrap();
        }
        let config = HooksConfig::load(&path).unwrap();
        (dir, config)
    }

    #[test]
    fn install_adds_named_entries_and_keeps_other_hooks() {
        let (_dir, mut c) = config(
            "worktree-path = \"x\"\npre-start = \"npm ci\"\n\n[post-remove]\nstop = \"kill\"\n",
        );
        c.install().unwrap();
        assert!(Phase::ALL.iter().all(|&phase| c.installed(phase)));
        assert_eq!(c.doc["pre-start"]["default"].as_str(), Some("npm ci"));
        assert_eq!(c.doc["post-remove"]["stop"].as_str(), Some("kill"));
        assert_eq!(
            c.doc["pre-switch"]["atelier"].as_str(),
            Some("atelier hook pre-switch")
        );
        let once = c.doc.to_string();
        c.install().unwrap();
        assert_eq!(c.doc.to_string(), once);
    }

    #[test]
    fn install_converts_atelier_plain_strings() {
        let (_dir, mut c) = config("\"pre-start\" = \"atelier hook pre-start\"\n");
        assert!(c.installed(Phase::PreStart));
        c.install().unwrap();
        assert_eq!(c.doc["pre-start"].as_table().unwrap().len(), 1);
        assert_eq!(
            c.doc["pre-start"]["atelier"].as_str(),
            Some("atelier hook pre-start")
        );
    }

    #[test]
    fn install_refuses_pipelines() {
        let (_dir, mut c) = config("[[pre-start]]\ninstall = \"npm ci\"\n");
        assert!(c.install().is_err());
    }

    #[test]
    fn uninstall_removes_only_atelier() {
        let (_dir, mut c) =
            config("pre-switch = \"atelier hook pre-switch\"\n[post-remove]\nstop = \"kill\"\n");
        c.install().unwrap();
        c.uninstall();
        assert!(Phase::ALL.iter().all(|&phase| !c.installed(phase)));
        assert!(c.doc.get("pre-start").is_none());
        assert!(c.doc.get("pre-switch").is_none());
        assert_eq!(c.doc["post-remove"]["stop"].as_str(), Some("kill"));
    }

    #[test]
    fn save_writes_only_changes_and_creates_the_directory() {
        let (_dir, mut c) = config("");
        assert!(!c.save().unwrap());
        assert!(!c.path().exists());
        c.install().unwrap();
        assert!(c.save().unwrap());
        let reloaded = HooksConfig::load(c.path()).unwrap();
        assert!(Phase::ALL.iter().all(|&phase| reloaded.installed(phase)));
        assert!(!reloaded.save().unwrap());
    }

    #[test]
    fn unparsable_config_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "pre-start = ").unwrap();
        assert!(HooksConfig::load(&path).is_err());
    }
}
