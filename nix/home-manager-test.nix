# Builds a Home Manager configuration that enables atelier and checks what it writes.
{
  pkgs,
  home-manager,
  module,
}:
let
  hm =
    (home-manager.lib.homeManagerConfiguration {
      inherit pkgs;
      modules = [
        module
        {
          home = {
            username = "test";
            homeDirectory = if pkgs.stdenv.hostPlatform.isDarwin then "/Users/test" else "/home/test";
            stateVersion = "25.11";
          };
          manual.manpages.enable = false;
          programs.fish.enable = true;
          programs.atelier = {
            enable = true;
            settings.default_workspace = "main";
          };
        }
      ];
    }).config;
  hooks = (pkgs.formats.json { }).generate "hooks.json" hm.programs.atelier.worktrunk.hooks;
in
pkgs.runCommand "atelier-home-manager"
  {
    nativeBuildInputs = [
      pkgs.jq
      pkgs.remarshal
    ];
  }
  ''
    files=${hm.home-files}

    # settings are rendered to config.toml
    toml2json $files/.config/atelier/config.toml | jq -e '.default_workspace == "main"'

    # the fish integration sources the `wt` wrapper
    grep -q 'bin/atelier shell init fish | source' $files/.config/fish/config.fish

    # the package is installed
    test -x ${hm.home.path}/bin/atelier

    # worktrunk.hooks matches what `atelier hooks install` writes
    export HOME=$TMPDIR XDG_CONFIG_HOME=$TMPDIR/config
    ${pkgs.lib.getExe hm.programs.atelier.package} hooks install
    toml2json $XDG_CONFIG_HOME/worktrunk/config.toml > installed.json
    jq -e --slurpfile want ${hooks} '. == $want[0]' installed.json

    touch $out
  ''
