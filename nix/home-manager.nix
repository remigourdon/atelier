# `programs.atelier`: installs atelier, writes its config and sources its fish integration.
# worktrunk's config is left to whoever generates it, who merges `worktrunk.hooks` into it.
self:
{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.programs.atelier;
  tomlFormat = pkgs.formats.toml { };
in
{
  options.programs.atelier = {
    enable = lib.mkEnableOption "atelier, which organises git worktrees into zellij sessions";

    package = lib.mkOption {
      type = lib.types.package;
      default = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
      defaultText = lib.literalExpression "atelier.packages.\${pkgs.stdenv.hostPlatform.system}.default";
      description = "The atelier package to install.";
    };

    settings = lib.mkOption {
      inherit (tomlFormat) type;
      default = { };
      example = lib.literalExpression ''
        {
          default_workspace = "main";
          carnets.root = "~/carnets";
        }
      '';
      description = ''
        Configuration written to {file}`$XDG_CONFIG_HOME/atelier/config.toml`.
        Every key is optional.
      '';
    };

    enableFishIntegration = lib.hm.shell.mkFishIntegrationOption {
      inherit config;
      extraDescription = ''
        Sources `atelier shell init fish`, which wraps worktrunk's `wt` so
        `wt switch` lands in the worktree's tab.
      '';
    };

    worktrunk.hooks = lib.mkOption {
      type = lib.types.attrsOf (lib.types.attrsOf lib.types.str);
      readOnly = true;
      default = lib.genAttrs [ "pre-start" "pre-switch" "post-remove" ] (phase: {
        atelier = "atelier hook ${phase}";
      });
      description = ''
        atelier's hooks in worktrunk's named-table form. This module never writes
        worktrunk's config: merge these into it wherever it is generated.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    home.packages = [ cfg.package ];

    xdg.configFile."atelier/config.toml" = lib.mkIf (cfg.settings != { }) {
      source = tomlFormat.generate "atelier-config.toml" cfg.settings;
    };

    # After other init, so the wrapper wraps any `wt` function defined there.
    programs.fish.interactiveShellInit = lib.mkIf cfg.enableFishIntegration (
      lib.mkAfter ''
        ${lib.getExe cfg.package} shell init fish | source
      ''
    );
  };
}
