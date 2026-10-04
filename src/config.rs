//! `$XDG_CONFIG_HOME/atelier/config.toml`. Every key is optional.

use std::path::{Path, PathBuf};

use color_eyre::eyre::{Result, WrapErr, eyre};
use regex::Regex;
use serde::Deserialize;

pub const DEFAULT_TICKET_PATTERN: &str = "[A-Z][A-Z0-9]{1,9}-[1-9][0-9]{0,5}";

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    pub default_workspace: Option<String>,
    pub editor: Option<String>,
    pub agent_command: Option<String>,
    pub ticket_pattern: Option<String>,
    pub browser: Option<String>,
    pub theme: Theme,
    pub icons: Icons,
    pub zellij: Zellij,
    pub carnets: Option<Carnets>,
    /// The legacy spelling of `[carnets] root`.
    pub carnet_root: Option<String>,
    pub tracker: crate::issues::TrackerConfig,
}

/// Plain Unicode glyphs work in any font; Nerd Font icons need one installed.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Icons {
    #[default]
    Unicode,
    Nerd,
}

/// A Catppuccin flavor.
#[derive(Debug, Default, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    Latte,
    Frappe,
    Macchiato,
    #[default]
    Mocha,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Zellij {
    pub session_layout: Option<String>,
    pub worktree_layout: Option<String>,
    pub anchor_pane: Option<String>,
}

/// Absent: carnets are disabled.
#[derive(Debug, Deserialize)]
pub struct Carnets {
    pub root: String,
}

impl Config {
    pub fn load() -> Result<Self> {
        let path = config_home().join("atelier/config.toml");
        match std::fs::read_to_string(&path) {
            Ok(text) => Self::parse(&text).wrap_err_with(|| format!("reading {}", path.display())),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(err) => Err(err.into()),
        }
    }

    pub fn parse(text: &str) -> Result<Self> {
        Ok(toml_edit::de::from_str(text)?)
    }

    pub fn default_workspace(&self) -> &str {
        self.default_workspace.as_deref().unwrap_or("default")
    }

    pub fn agent_command(&self) -> &str {
        self.agent_command.as_deref().unwrap_or("claude")
    }

    pub fn anchor_pane(&self) -> &str {
        self.zellij.anchor_pane.as_deref().unwrap_or("editor")
    }

    /// Where carnets live, or `None` when they are disabled.
    pub fn carnet_root(&self) -> Option<PathBuf> {
        let root = match &self.carnets {
            Some(carnets) => &carnets.root,
            None => self.carnet_root.as_ref()?,
        };
        Some(expand(root))
    }

    pub fn carnets_enabled(&self) -> bool {
        self.carnet_root().is_some()
    }

    /// Where carnets live, or why there are none.
    pub fn require_carnet_root(&self) -> Result<PathBuf> {
        self.carnet_root()
            .ok_or_else(|| eyre!("carnets are disabled: set `root` under [carnets] in the config"))
    }

    /// The configured browser, else `$BROWSER`; `None` means the platform opener.
    pub fn browser(&self) -> Option<String> {
        self.browser.clone().or_else(|| non_empty_var("BROWSER"))
    }

    pub fn flavor(&self) -> catppuccin::Flavor {
        let palette = &catppuccin::PALETTE;
        match self.theme {
            Theme::Latte => palette.latte,
            Theme::Frappe => palette.frappe,
            Theme::Macchiato => palette.macchiato,
            Theme::Mocha => palette.mocha,
        }
    }

    /// The configured editor, else `$VISUAL`, else `$EDITOR`; `None` means a plain shell.
    pub fn editor(&self) -> Option<String> {
        self.editor
            .clone()
            .or_else(|| non_empty_var("VISUAL"))
            .or_else(|| non_empty_var("EDITOR"))
    }

    /// The ticket key pattern, undelimited.
    pub fn ticket_pattern(&self) -> &str {
        self.ticket_pattern
            .as_deref()
            .unwrap_or(DEFAULT_TICKET_PATTERN)
    }

    /// The ticket key pattern, delimited so it never matches inside a longer word.
    pub fn ticket_regex(&self) -> Result<Regex> {
        let pattern = self.ticket_pattern();
        Ok(Regex::new(&format!(
            "(?:^|[^A-Za-z0-9])({pattern})(?:$|[^A-Za-z0-9])"
        ))?)
    }
}

/// The group a branch or name belongs to: its first ticket key, or `""`.
pub fn group_from_name(ticket: &Regex, name: &str) -> String {
    ticket
        .captures(name)
        .map(|captures| captures[1].to_owned())
        .unwrap_or_default()
}

/// A path with a leading `~/` resolved against the home directory.
pub fn expand(path: &str) -> PathBuf {
    match path.strip_prefix("~/") {
        Some(rest) => home().join(rest),
        None => path.into(),
    }
}

fn non_empty_var(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

fn xdg(var: &str, fallback: &str) -> PathBuf {
    match std::env::var_os(var) {
        Some(value) if Path::new(&value).is_absolute() => value.into(),
        _ => home().join(fallback),
    }
}

pub fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
}

pub fn config_home() -> PathBuf {
    xdg("XDG_CONFIG_HOME", ".config")
}

pub fn state_home() -> PathBuf {
    xdg("XDG_STATE_HOME", ".local/state")
}

pub fn cache_home() -> PathBuf {
    xdg("XDG_CACHE_HOME", ".cache")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn group(name: &str) -> String {
        group_from_name(&Config::default().ticket_regex().unwrap(), name)
    }

    #[test]
    fn ticket_in_branch() {
        assert_eq!(group("feature/ORD-3479-investigate"), "ORD-3479");
        assert_eq!(group("ORD-1"), "ORD-1");
    }

    #[test]
    fn lowercase_dates_and_underscores_are_not_tickets() {
        assert_eq!(group("atelier-verification-20260930"), "");
        assert_eq!(group("feature/ord-3479-investigate"), "");
        assert_eq!(group("feature/ord_3479-investigate"), "");
        assert_eq!(group("meeting_notes"), "");
        assert_eq!(group("XORD-12a"), "");
    }

    #[test]
    fn custom_ticket_pattern() {
        let config = Config::parse(r##"ticket_pattern = "#[0-9]+""##).unwrap();
        assert_eq!(
            group_from_name(&config.ticket_regex().unwrap(), "fix-#42-x"),
            "#42"
        );
    }

    #[test]
    fn defaults_and_overrides() {
        let config = Config::parse("").unwrap();
        assert_eq!(config.default_workspace(), "default");
        assert_eq!(config.anchor_pane(), "editor");
        assert_eq!(config.agent_command(), "claude");
        assert_eq!(config.flavor().name, catppuccin::PALETTE.mocha.name);
        assert_eq!(config.icons, Icons::Unicode);
        let config = Config::parse(
            "default_workspace = \"vrac\"\ntheme = \"latte\"\n[zellij]\nanchor_pane = \"main\"\n",
        )
        .unwrap();
        assert_eq!(config.default_workspace(), "vrac");
        assert_eq!(config.anchor_pane(), "main");
        assert_eq!(config.flavor().name, catppuccin::PALETTE.latte.name);
        let config = Config::parse("browser = \"firefox\"\n").unwrap();
        assert_eq!(config.browser().as_deref(), Some("firefox"));
        assert!(Config::parse("theme = \"neon\"").is_err());
        assert_eq!(
            Config::parse("icons = \"nerd\"").unwrap().icons,
            Icons::Nerd
        );
        assert!(Config::parse("icons = \"emoji\"").is_err());
    }

    #[test]
    fn carnets_are_off_unless_a_root_is_set() {
        assert_eq!(Config::parse("").unwrap().carnet_root(), None);
        let config = Config::parse("[carnets]\nroot = \"/data\"\n").unwrap();
        assert_eq!(config.carnet_root(), Some(PathBuf::from("/data")));
        let config = Config::parse("carnet_root = \"~/Data\"\n").unwrap();
        assert_eq!(config.carnet_root(), Some(home().join("Data")));
        let config = Config::parse("carnet_root = \"/old\"\n[carnets]\nroot = \"/new\"\n").unwrap();
        assert_eq!(config.carnet_root(), Some(PathBuf::from("/new")));
    }
}
