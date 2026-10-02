{
  description = "spoolframe-linux — Spoolframe Linux client: Relm4, libadwaita, GStreamer.";

  inputs = {
    molds.url = "git+ssh://forgejo@git.coldsteel.casa/Coldsteel/molds.git";
    nixpkgs.follows = "molds/nixpkgs-unstable";
    flake-parts.follows = "molds/flake-parts";
  };

  outputs = inputs @ {flake-parts, ...}:
    flake-parts.lib.mkFlake {inherit inputs;} {
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
      ];

      perSystem = {pkgs, ...}: {
        # No package here: the client builds against ../spoolframe, which a
        # subflake copied to the store does not have. The root flake builds
        # it as `.#spoolframe-linux` until spoolframe-types comes from the
        # coldsteel registry (see nix/package.nix).
        formatter = pkgs.alejandra;
      };
    };
}
