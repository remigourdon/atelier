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

`atelier statusline` prints one line about the worktree or carnet holding the current directory: its ticket, the cached issue title, and worktrunk's dirty, ahead/behind, CI, review and finished cells. Outside one it prints nothing.

When [zjstatus](https://github.com/dj95/zjstatus) is at `$XDG_CONFIG_HOME/zellij/plugins/zjstatus.wasm` (or `[zellij] zjstatus`), the built-in layouts add a row under the tab bar that runs `atelier statusline` every 10 seconds in the focused pane's directory, so it follows the visible tab. Atelier does not install the plugin: without it the layouts keep only zellij's tab and status bars. On first launch the row asks for zjstatus's permissions: focus it, press `y`. zjstatus never truncates, so an overlong line is clipped at its right edge; the issue title comes last, to be cut first.

To show the line in your own zjstatus bar, add to its plugin block, and place `{command_atelier}` in `format_left` or `format_right`:

```kdl
command_atelier_command    "atelier statusline"
command_atelier_format     "{stdout}"
command_atelier_interval   "10"
command_atelier_rendermode "raw"
command_atelier_cwd        "{focused_pane_cwd}"
```

Licensed under either of [Apache-2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT), at your option.
