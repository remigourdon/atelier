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

## Status bar

`atelier statusline` prints one line about the worktree or carnet holding the current directory: its ticket, the cached issue title, and worktrunk's dirty, ahead/behind, CI, review and finished cells. Outside one it prints nothing. To show it in [zjstatus](https://github.com/dj95/zjstatus), add to its plugin block, and place `{command_atelier}` in `format_left` or `format_right`:

```kdl
command_atelier_command    "atelier statusline"
command_atelier_format     "{stdout}"
command_atelier_interval   "5"
command_atelier_rendermode "raw"
command_atelier_cwd        "{focused_pane_cwd}"
```

zjstatus runs commands in its own directory, so `{focused_pane_cwd}` points it at the focused pane's, and the bar follows the visible tab. zjstatus never truncates: an overlong bar is clipped at its right edge, so the line puts the issue title last, to be cut first. Give the segment the room it needs on the left, or set `format_hide_on_overlength true` to drop the lowest-precedence part whole. The built-in session layout keeps `zellij:status-bar`; this is opt-in.

Licensed under either of [Apache-2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT), at your option.
