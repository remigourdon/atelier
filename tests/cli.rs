use std::path::Path;
use std::process::{Command, Output};

use tempfile::TempDir;

/// An isolated home: XDG directories under a temporary directory, outside zellij.
struct Home(TempDir);

impl Home {
    fn new() -> Self {
        Self(tempfile::tempdir().unwrap())
    }

    /// A home whose database is the prototype's baseline schema, with no user_version.
    fn baseline() -> Self {
        let home = Self::new();
        let dir = home.0.path().join("state/atelier");
        std::fs::create_dir_all(&dir).unwrap();
        let db = rusqlite::Connection::open(dir.join("atelier.db")).unwrap();
        db.execute_batch(include_str!("fixtures/baseline.sql"))
            .unwrap();
        home
    }

    fn path(&self, rest: &str) -> std::path::PathBuf {
        self.0.path().join(rest)
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_atelier"));
        command
            .args(args)
            .env("HOME", self.0.path())
            .env("XDG_CONFIG_HOME", self.path("config"))
            .env("XDG_STATE_HOME", self.path("state"))
            .env("XDG_CACHE_HOME", self.path("cache"))
            // Atelier commits carnet READMEs.
            .env("GIT_AUTHOR_NAME", "me")
            .env("GIT_AUTHOR_EMAIL", "me@example.com")
            .env("GIT_COMMITTER_NAME", "me")
            .env("GIT_COMMITTER_EMAIL", "me@example.com")
            .env_remove("ZELLIJ")
            .env_remove("ZELLIJ_SESSION_NAME")
            .env_remove("ZELLIJ_PANE_ID");
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command(args).output().unwrap()
    }

    fn ok(&self, args: &[&str]) -> String {
        let output = self.run(args);
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }

    fn fails(&self, args: &[&str]) -> String {
        let output = self.run(args);
        assert!(!output.status.success(), "{args:?} succeeded");
        String::from_utf8(output.stderr).unwrap()
    }

    fn git_repo(&self, name: &str) -> String {
        let path = self.path(name);
        std::fs::create_dir_all(&path).unwrap();
        let status = Command::new("git")
            .args(["init", "-q"])
            .current_dir(&path)
            .status()
            .unwrap();
        assert!(status.success());
        path.canonicalize().unwrap().to_string_lossy().into_owned()
    }
}

#[test]
fn help_succeeds() {
    let output = Command::new(env!("CARGO_BIN_EXE_atelier"))
        .arg("--help")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("Usage: atelier"));
}

#[test]
fn workspaces_on_a_fresh_database() {
    let home = Home::new();
    assert_eq!(home.ok(&["ws", "ls"]), "default\n");
    home.ok(&["ws", "add", "w"]);
    assert!(home.fails(&["ws", "add", "w"]).contains("already exists"));
    assert_eq!(home.ok(&["ws", "ls"]), "default\nw\n");
    assert!(
        home.fails(&["ws", "rm", "default"])
            .contains("cannot be removed")
    );
    home.ok(&["ws", "rm", "w"]);
    assert_eq!(home.ok(&["ws", "ls"]), "default\n");
}

#[test]
fn default_workspace_comes_from_config() {
    let home = Home::new();
    std::fs::create_dir_all(home.path("config/atelier")).unwrap();
    std::fs::write(
        home.path("config/atelier/config.toml"),
        "default_workspace = \"vrac\"\n",
    )
    .unwrap();
    assert_eq!(home.ok(&["ws", "ls"]), "vrac\n");
}

#[test]
fn repos_on_a_fresh_database() {
    let home = Home::new();
    let repo = home.git_repo("proj");
    let added = home.ok(&["add", &repo, "-a", "p"]);
    assert_eq!(added, format!("registered {repo} in default\n"));
    assert_eq!(home.ok(&["ls"]), format!("p\tdefault\t{repo}\n"));
    assert!(home.fails(&["update", "p"]).contains("provide"));
    assert!(
        home.fails(&["update", "p", "-w", "nope"])
            .contains("unknown workspace")
    );
    home.ok(&["ws", "add", "w"]);
    home.ok(&["update", "p", "-w", "w", "-a", ""]);
    assert_eq!(home.ok(&["ls"]), format!("proj\tw\t{repo}\n"));
    assert!(home.fails(&["ws", "rm", "w"]).contains("repo default"));
    home.ok(&["rm", "proj"]);
    assert_eq!(home.ok(&["ls"]), "");
    assert!(
        home.fails(&["add", &home.path("").to_string_lossy()])
            .contains("not in a git repository")
    );
}

#[test]
fn baseline_database_is_read_and_extended() {
    let home = Home::baseline();
    assert_eq!(home.ok(&["ws", "ls"]), "conf\ndefault\nvrac\n");
    assert_eq!(home.ok(&["ls"]), "configue\tconf\t/home/me/configue\n");
    assert!(home.fails(&["ws", "rm", "conf"]).contains("repo default"));
    home.ok(&["ws", "rm", "vrac"]);
    assert_eq!(
        home.ok(&["ws", "ls"]),
        "conf\ndefault\n",
        "its carnets moved"
    );
    home.ok(&["ws", "add", "new"]);
    home.ok(&["ws", "rm", "new"]);
}

#[test]
fn hooks_install_status_and_uninstall() {
    let home = Home::new();
    let config = home.path("config/worktrunk/config.toml");
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    std::fs::write(&config, "worktree-path = \"x\"\npre-start = \"npm ci\"\n").unwrap();
    assert!(home.ok(&["hooks", "status"]).contains("pre-start\tmissing"));
    home.ok(&["hooks", "install"]);
    let text = std::fs::read_to_string(&config).unwrap();
    assert!(text.contains("worktree-path = \"x\""), "{text}");
    assert!(text.contains("default = \"npm ci\""), "{text}");
    assert!(
        text.contains("atelier = \"atelier hook post-remove\""),
        "{text}"
    );
    assert_eq!(
        home.ok(&["hooks", "status"]),
        "pre-start\tinstalled\npre-switch\tinstalled\npost-remove\tinstalled\n"
    );
    home.ok(&["hooks", "uninstall"]);
    let text = std::fs::read_to_string(&config).unwrap();
    assert!(!text.contains("atelier"), "{text}");
    assert!(text.contains("npm ci"), "{text}");
}

#[test]
fn hook_errors_never_fail_worktrunk() {
    let home = Home::new();
    let mut child = home
        .command(&["hook", "pre-start"])
        .stdin(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write;
    child.stdin.take().unwrap().write_all(b"not json").unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("hook error"));
    assert!(Path::new(&home.path("state/atelier/hooks.log")).exists());
    assert!(home.fails(&["hook", "pre-merge"]).contains("invalid value"));
}

#[test]
fn shell_init_fish_wraps_wt() {
    let script = Home::new().ok(&["shell", "init", "fish"]);
    assert!(script.contains("function wt"));
    assert!(script.contains("ATELIER_HOOK_TARGET"));
    assert!(script.contains("atelier open"));
}

#[test]
fn context_never_creates_the_database() {
    let home = Home::new();
    let json = home.ok(&["context", "--json"]);
    let context: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(context["item"], serde_json::Value::Null);
    assert!(!home.path("state/atelier").exists());
}

#[test]
fn carnets_are_folders_under_the_configured_root() {
    let home = Home::new();
    assert!(
        home.fails(&["carnet", "new", "notes"])
            .contains("[carnets]")
    );
    std::fs::create_dir_all(home.path("config/atelier")).unwrap();
    std::fs::write(
        home.path("config/atelier/config.toml"),
        "[carnets]\nroot = \"~/Data\"\n",
    )
    .unwrap();
    let created = home.ok(&["carnet", "new", "ABC-1 slow login"]);
    let path = created
        .strip_prefix("created ")
        .and_then(|rest| rest.strip_suffix(" in default\n"))
        .unwrap_or_else(|| panic!("{created}"));
    let path = Path::new(path);
    assert_eq!(
        path.parent().unwrap(),
        home.path("Data").canonicalize().unwrap()
    );
    assert!(path.to_string_lossy().ends_with("-ABC-1-slow-login"));
    let readme = std::fs::read_to_string(path.join("README.md")).unwrap();
    assert!(
        readme.starts_with("+++\ntickets = [\"ABC-1\"]\n"),
        "{readme}"
    );
    let log = Command::new("git")
        .args(["log", "--format=%s"])
        .current_dir(path)
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&log.stdout), "Create carnet\n");

    home.git_repo("Data/2026-01-02-ORD-7-old-notes");
    let closed = home.git_repo("Data/2026-01-03-done");
    std::fs::write(
        Path::new(&closed).join("README.md"),
        "+++\ntickets = [\"ORD-7\"]\nclosed = true\nsummary = \"Fixed\"\n+++\n",
    )
    .unwrap();
    home.git_repo("Data/undated");
    let name = path.file_name().unwrap().to_string_lossy();
    assert_eq!(
        home.ok(&["carnet", "ls"]),
        format!("{name}\tABC-1\t\n2026-01-02-ORD-7-old-notes\tORD-7\t\n")
    );
    assert!(
        home.ok(&["carnet", "ls", "--closed"])
            .contains("2026-01-03-done\tORD-7\tFixed\n")
    );

    let json = home.ok(&["context", "--json", &path.join("notes").to_string_lossy()]);
    let context: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(context["item"]["kind"], "carnet");
    assert_eq!(context["workspace"]["name"], "default");
    assert_eq!(context["group"], "ABC-1");
    assert_eq!(context["carnet"], path.to_string_lossy().as_ref());
    let json = home.ok(&["context", "--json", "--key", "ORD-7"]);
    let context: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(context["item"], serde_json::Value::Null);
    assert_eq!(context["carnets"].as_array().unwrap().len(), 2);
    assert!(
        (context["carnet"].as_str().unwrap()).ends_with("2026-01-02-ORD-7-old-notes"),
        "{context}"
    );
    assert_eq!(
        home.ok(&["context", &home.path("Data").to_string_lossy()]),
        "item       not in an atelier item\n"
    );
    assert!(
        home.fails(&["context", ".", "--key", "ORD-7"])
            .contains("cannot be used with")
    );

    let search = home
        .command(&["carnet", "search", "x"])
        .env("PATH", "")
        .output()
        .unwrap();
    assert!(!search.status.success());
    assert!(String::from_utf8_lossy(&search.stderr).contains("needs ripgrep (rg) on PATH"));
}
