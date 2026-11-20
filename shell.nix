# Development shell for Linux hosts with nix (NixOS has no FHS paths):
# rustup, what targets/build.sh needs, and the tools the tests run.
#
#   nix-shell            # then cargo test, ...
{ pkgs ? import <nixpkgs> { } }:
pkgs.mkShell {
  # The wrapper's hardening flags (fortify, stack protector, ...) would
  # reach C that links against the toolchain's own C libraries (the REPL
  # host).
  hardeningDisable = [ "all" ];
  packages = with pkgs; [
    curl
    xz
    python3
    rustup
    git
    nodejs
  ];
}
