# atelier

A lazygit-style TUI and CLI that organise git worktrees into zellij sessions, on top of [worktrunk](https://worktrunk.dev).

Work in progress. See [docs/design.md](docs/design.md) and [CONTEXT.md](CONTEXT.md).

## Home Manager

```nix
{ inputs, config, pkgs, ... }:
{
  imports = [ inputs.atelier.homeManagerModules.default ];

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

Licensed under either of [Apache-2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT), at your option.
