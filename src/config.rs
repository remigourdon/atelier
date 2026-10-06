//! `$XDG_CONFIG_HOME/atelier/config.toml`. Every key is optional.

use std::path::{Path, PathBuf};

use color_eyre::eyre::{Result, WrapErr, eyre};
use regex::Regex;
use serde::Deserialize;

pub const DEFAULT_ISSUE_KEY_PATTERN: &str = "[A-Z][A-Z0-9]{1,9}-[1-9][0-9]{0,5}";

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    pub default_workspace: Option<String>,
    pub editor: Option<String>,
    pub agent_command: Option<String>,
    pub issue_key_pattern: Option<String>,
    pub browser: Option<String>,
    /// What `g` runs on an item, in its directory.
    pub tool: Option<String>,
    pub theme: Theme,
    pub icons: Icons,
    pub zellij: Zellij,
    pub carnets: Option<Carnets>,
    /// The legacy spelling of `[carnets] root`.
    pub carnet_root: Option<String>,
    pub tracker: crate::issues::TrackerConfig,
    pub reviews: crate::reviews::ReviewConfig,
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
    pub zjstatus: Option<String>,
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

    pub fn tool(&self) -> &str {
        self.tool.as_deref().unwrap_or(crate::tool::DEFAULT)
    }

    pub fn anchor_pane(&self) -> &str {
        self.zellij.anchor_pane.as_deref().unwrap_or("editor")
    }

    /// Where the zjstatus plugin is looked for: the built-in layouts add its row when it exists.
    pub fn zjstatus(&self) -> PathBuf {
        match &self.zellij.zjstatus {
            Some(path) => expand(path),
            None => config_home().join("zellij/plugins/zjstatus.wasm"),
        }
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

    /// The issue key pattern, undelimited.
    pub fn issue_key_pattern(&self) -> &str {
        self.issue_key_pattern
            .as_deref()
            .unwrap_or(DEFAULT_ISSUE_KEY_PATTERN)
    }

    /// The issue key pattern, delimited so it never matches inside a longer word.
    pub fn issue_key_regex(&self) -> Result<Regex> {
        let pattern = self.issue_key_pattern();
        Ok(Regex::new(&format!(
            "(?:^|[^A-Za-z0-9])({pattern})(?:$|[^A-Za-z0-9])"
        ))?)
    }
}

/// Every issue key in a branch or name, in order and without duplicates. A delimiter between
/// two keys serves both.
pub fn issue_keys(pattern: &Regex, name: &str) -> Vec<String> {
    let mut keys: Vec<String> = Vec::new();
    let mut start = 0;
    while let Some(key) = pattern
        .captures_at(name, start)
        .and_then(|found| found.get(1))
    {
        if !keys.iter().any(|known| known == key.as_str()) {
            keys.push(key.as_str().to_owned());
        }
        start = key.end();
    }
    keys
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

    fn keys(name: &str) -> Vec<String> {
        issue_keys(&Config::default().issue_key_regex().unwrap(), name)
    }

    #[test]
    fn issue_key_in_branch() {
        assert_eq!(keys("feature/ORD-3479-investigate"), ["ORD-3479"]);
        assert_eq!(keys("ORD-1"), ["ORD-1"]);
    }

    #[test]
    fn every_key_in_order_without_duplicates() {
        assert_eq!(keys("ABC-1-DEF-2-fix"), ["ABC-1", "DEF-2"]);
        assert_eq!(keys("DEF-2 then ABC-1, DEF-2 again"), ["DEF-2", "ABC-1"]);
        assert_eq!(keys("xABC-1 ABC-12"), ["ABC-12"]);
    }

    #[test]
    fn lowercase_dates_and_underscores_are_not_issue_keys() {
        assert!(keys("atelier-verification-20260930").is_empty());
        assert!(keys("feature/ord-3479-investigate").is_empty());
        assert!(keys("feature/ord_3479-investigate").is_empty());
        assert!(keys("meeting_notes").is_empty());
        assert!(keys("XORD-12a").is_empty());
    }

    #[test]
    fn custom_issue_key_pattern() {
        let config = Config::parse(r##"issue_key_pattern = "#[0-9]+""##).unwrap();
        assert_eq!(
            issue_keys(&config.issue_key_regex().unwrap(), "fix-#42-x"),
            ["#42"]
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
