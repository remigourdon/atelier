//! `atelier shell init <shell>`: shell integration.

/// Wraps worktrunk's `wt` so `wt switch` lands in the worktree's tab: inside zellij the
/// hook opens it; outside, the hook names the session and the wrapper attaches to it.
pub const FISH: &str = r#"if functions -q wt; and not functions -q __atelier_original_wt
    functions --copy wt __atelier_original_wt
else if not functions -q __atelier_original_wt
    function __atelier_original_wt
        command wt $argv
    end
end

function wt --wraps wt --description 'worktrunk, opening worktrees in atelier tabs'
    if test "$argv[1]" != switch; or contains -- --no-hooks $argv
        __atelier_original_wt $argv
        return $status
    end

    if set -q ZELLIJ
        __atelier_original_wt switch --no-cd $argv[2..-1]
        return $status
    end

    set -l target_file (mktemp)
    set -lx ATELIER_HOOK_TARGET $target_file
    __atelier_original_wt switch --no-cd $argv[2..-1]
    set -l result $status
    if test $result -eq 0; and test -s $target_file
        read -l target_session < $target_file
        atelier open $target_session
        set result $status
    end
    command rm -f $target_file
    return $result
end

COMPLETE=fish atelier | source
"#;
