//! The tool `g` opens on an item: lazygit unless `tool` is configured.

use std::path::Path;

pub const DEFAULT: &str = "lazygit";

/// `template` with `{path}` and `{branch}` replaced, each shell-quoted.
pub fn command(template: &str, path: &Path, branch: &str) -> String {
    template
        .replace("{path}", &quote(&path.to_string_lossy()))
        .replace("{branch}", &quote(branch))
}

/// The program a command runs: its first word.
pub fn program(command: &str) -> &str {
    command.split_whitespace().next().unwrap_or("")
}

/// `value` in single quotes, safe for `sh`.
pub fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substitutes_quoted_paths_and_branches() {
        assert_eq!(
            command("tig --all {branch}", Path::new("/r/a"), "feat/x"),
            "tig --all 'feat/x'"
        );
        assert_eq!(
            command(
                "tool -C {path} {branch}",
                Path::new("/my dir/it's"),
                "a b;rm"
            ),
            r"tool -C '/my dir/it'\''s' 'a b;rm'"
        );
        assert_eq!(command("lazygit", Path::new("/r/a"), "main"), "lazygit");
    }

    #[test]
    fn program_is_the_first_word() {
        assert_eq!(program("  tig --all 'x'"), "tig");
        assert_eq!(program(""), "");
    }
}
