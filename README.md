# atelier

A lazygit-style TUI and CLI that organise git worktrees into zellij sessions, on top of [worktrunk](https://worktrunk.dev).

Work in progress. See [docs/design.md](docs/design.md) and [CONTEXT.md](CONTEXT.md).

## Home Manager

```nix
{ inputs, config, pkgs, ... }:
{
  imports = [ inputs.atelier.homeModules.default ];

  programs.atelier = {
    enable = true;
    settings.default_workspace = "main"; # rendered to config.toml
  };

  # The module never writes worktrunk's config: merge atelier's hooks wherever you generate it.
  xdg.configFile."worktrunk/config.toml".source =
    (pkgs.formats.toml { }).generate "worktrunk.toml" config.programs.atelier.worktrunk.hooks;
}
```

Without Nix, `atelier hooks install` adds the hooks to worktrunk's config.

## Scripts and agents

`atelier context [PATH] [--json]` describes the worktree or carnet holding a directory: its group, its issues as last cached, the worktrees and carnets in its group or sharing its issue keys, and the open reviews linking them. `atelier context --issue-key ABC-5` describes an issue's linked work and reviews instead. Neither writes or fetches anything.

`atelier carnet new <name> [-g GROUP] [-i ISSUE_KEY]... [-s SUMMARY] [--json]` creates a carnet and prints its path (its record with `--json`). `atelier carnet set [PATH] [-g GROUP] [-i ISSUE_KEY]... [-s SUMMARY]` changes the carnet holding a directory in one commit: only the flags given change, `-i` replaces every key, and `-g ""` ungroups. `atelier carnet close [PATH]` and `atelier carnet reopen [PATH]` close and reopen it.

Licensed under either of [Apache-2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT), at your option.
