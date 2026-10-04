//! Live smoke test against a real zellij: a client attached through `script` (headless sessions
//! don't spawn new tabs' panes), open a tab through the pre-start hook, find its anchor pane,
//! and close it through post-remove. Needs zellij and util-linux `script`, so it only runs on
//! request: `cargo test --test zellij_smoke -- --ignored`.

use std::io::Write;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn installed(program: &str) -> bool {
    Command::new(program)
        .arg("--version")
        .output()
        .is_ok_and(|out| out.status.success())
}

struct Session {
    name: String,
    client: Child,
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.client.kill();
        let _ = Command::new("zellij")
            .args(["kill-session", &self.name])
            .output();
        let _ = Command::new("zellij")
            .args(["delete-session", "--force", &self.name])
            .output();
    }
}

fn wait_for(what: &str, mut ready: impl FnMut() -> bool) {
    let start = Instant::now();
    while !ready() {
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "timed out waiting for {what}"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn action(session: &str, args: &[&str]) -> Option<String> {
    let out = Command::new("zellij")
        .args(["--session", session, "action"])
        .args(args)
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

#[test]
#[ignore = "needs a live zellij"]
fn open_find_anchor_and_close_a_tab() {
    assert!(installed("zellij"), "zellij is not installed");
    assert!(installed("script"), "util-linux script is not installed");
    let home = tempfile::tempdir().unwrap();
    let envs = [
        ("XDG_CONFIG_HOME", home.path().join("config")),
        ("XDG_STATE_HOME", home.path().join("state")),
        ("XDG_CACHE_HOME", home.path().join("cache")),
    ];
    let atelier = |args: &[&str], stdin: Option<String>| {
        let mut child = Command::new(env!("CARGO_BIN_EXE_atelier"))
            .args(args)
            .envs(envs.clone())
            .env_remove("ZELLIJ")
            .env_remove("ZELLIJ_SESSION_NAME")
            .env_remove("ZELLIJ_PANE_ID")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(stdin.unwrap_or_default().as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(
            out.status.success() && out.stderr.is_empty(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    };

    let repo = home.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    assert!(
        Command::new("git")
            .args(["init", "-q", "-b", "main"])
            .current_dir(&repo)
            .status()
            .unwrap()
            .success()
    );
    let repo = repo.canonicalize().unwrap().to_string_lossy().into_owned();

    let name = format!("atelier-smoke-{}", std::process::id());
    atelier(&["ws", "add", &name], None);
    atelier(&["add", &repo, "-w", &name], None);

    let layout = home.path().join("session.kdl");
    std::fs::write(&layout, "layout {\n    pane\n}\n").unwrap();
    let attach = format!(
        "zellij --layout {} attach --create {name}",
        layout.display()
    );
    let client = Command::new("script")
        .args(["-qfc", &attach, "/dev/null"])
        .env("TERM", "xterm-256color")
        .env("COLUMNS", "200")
        .env("LINES", "50")
        .env_remove("ZELLIJ")
        .env_remove("ZELLIJ_SESSION_NAME")
        .env_remove("ZELLIJ_PANE_ID")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let session = Session {
        name: name.clone(),
        client,
    };
    wait_for("the session", || {
        action(&session.name, &["list-tabs", "--json"]).is_some()
    });

    let payload = serde_json::json!({
        "worktree_path": repo, "branch": "main", "primary_worktree_path": repo,
    })
    .to_string();
    atelier(&["hook", "pre-start"], Some(payload.clone()));

    let panes = action(&name, &["list-panes", "--all", "--json"]).unwrap();
    let panes: Vec<serde_json::Value> = serde_json::from_str(&panes).unwrap();
    let anchor = panes
        .iter()
        .find(|pane| pane["title"] == "editor" && pane["is_plugin"] == false)
        .expect("an anchor pane named editor");
    let tabs = action(&name, &["list-tabs", "--json"]).unwrap();
    assert!(
        tabs.contains("\"name\": \"repo\"") || tabs.contains("\"name\":\"repo\""),
        "{tabs}"
    );
    let tab_id = anchor["tab_id"].as_u64().unwrap();

    atelier(&["hook", "post-remove"], Some(payload));
    wait_for("the tab to close", || {
        let tabs: Vec<serde_json::Value> =
            serde_json::from_str(&action(&name, &["list-tabs", "--json"]).unwrap()).unwrap();
        !tabs
            .iter()
            .any(|tab| tab["tab_id"].as_u64() == Some(tab_id))
    });
}
