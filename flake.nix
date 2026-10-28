{
  description = "spoolframe-linux — Spoolframe Linux client: Relm4, libadwaita, GStreamer.";

  # Public inputs only: a fresh clone builds with no SSH key. nixpkgs is the
  # same input devenv.yaml names; keep the two locks on one revision so the
  # shell and the package share a store.
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    flake-parts = {
      url = "github:hercules-ci/flake-parts";
      inputs.nixpkgs-lib.follows = "nixpkgs";
    };
    crane.url = "github:ipetkov/crane";
  };

  outputs = inputs @ {flake-parts, ...}:
    flake-parts.lib.mkFlake {inherit inputs;} {
      flake.nixosModules = let
        module = import ./nix/nixos-module.nix {
          packageFor = pkgs: inputs.self.packages.${pkgs.stdenv.hostPlatform.system}.default;
        };
      in {
        default = module;
        spoolframe-linux = module;
      };

      # GTK, libadwaita and GStreamer from nixpkgs: Linux only.
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];

      perSystem = {pkgs, ...}: let
        # Every crate comes from crates.io, the SDK's too, except noq-proto
        # from its git commit (Cargo.toml's [patch.crates-io]), which crane
        # fetches by the commit in Cargo.lock.
        craneLib = inputs.crane.mkLib pkgs;
        spoolframe-linux = pkgs.callPackage ./nix/package.nix {
          inherit craneLib;
          src = let
            inherit (pkgs.lib) fileset;
          in
            fileset.toSource {
              root = ./.;
              fileset = fileset.unions [
                (fileset.difference
                  (fileset.fileFilter (
                      file:
                        builtins.any file.hasExt ["rs" "toml" "css" "svg"]
                        || file.name == "Cargo.lock"
                    )
                    ./.)
                  # Build scripts write .rs files under target/.
                  (fileset.maybeMissing ./target))
                # Embedded with include_bytes! (the fonts), and installed
                # (the launcher and its icons).
                ./crates/spoolframe-linux-app/fonts
                ./crates/spoolframe-linux-app/data
              ];
            };
        };
      in {
        packages = {
          default = spoolframe-linux;
          inherit spoolframe-linux;
        };
        formatter = pkgs.alejandra;
      };
    };
}
