use std::process::Command;

#[test]
fn help_succeeds() {
    let output = Command::new(env!("CARGO_BIN_EXE_atelier"))
        .arg("--help")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("Usage: atelier"));
}
