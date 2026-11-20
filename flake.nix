{
  description = "Nabi: clear rules, free as a butterfly";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  # The release archives, wrapped as they are: nabic has its own backend
  # and linker and ships each target's C library, so there is nothing to
  # build. Linux takes the static musl archives (no interpreter or rpath
  # to patch).
  # nix/sources.json is rewritten for each release (scripts/nix.sh).
  # Until the first release it is a template with zero checksums, and
  # evaluating a package says so.
  # flake.lock pins nixpkgs: `nix flake lock` makes it, `nix flake update`
  # moves it (docs/releasing.md).
  outputs =
    { self, nixpkgs }:
    let
      raw = builtins.fromJSON (builtins.readFile ./nix/sources.json);
      sources =
        if raw.template or false then
          throw "nix/sources.json is a template until nabi ${raw.version} is released; after the release, run scripts/nix.sh ${raw.version}"
        else
          raw;
      forAll =
        f:
        nixpkgs.lib.genAttrs (builtins.attrNames raw.archives) (
          system: f system nixpkgs.legacyPackages.${system}
        );
    in
    {
      packages = forAll (
        system: pkgs: {
          nabi = pkgs.stdenvNoCC.mkDerivation {
            pname = "nabi";
            inherit (sources) version;
            src = pkgs.fetchurl { inherit (sources.archives.${system}) url sha256; };
            dontConfigure = true;
            dontBuild = true;
            dontStrip = true;
            dontPatchELF = true;
            installPhase = ''
              mkdir -p $out
              cp -R bin lib $out/
            '';
            meta = {
              description = "Nabi toolchain: nabi, nabic and nabils";
              homepage = "https://nabi-lang.org";
              license = with nixpkgs.lib.licenses; [
                mit
                asl20
              ];
              mainProgram = "nabi";
              platforms = [ system ];
            };
          };
          default = self.packages.${system}.nabi;
        }
      );

      apps = forAll (
        system: _: {
          default = {
            type = "app";
            program = "${self.packages.${system}.nabi}/bin/nabi";
          };
        }
      );
    };
}
