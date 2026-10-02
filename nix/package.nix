# spoolframe-linux — Spoolframe Linux client: Relm4, libadwaita, GStreamer.
#
# Built with crane. The root flake calls this, not the subflake: the client
# takes spoolframe-types as a path dependency on ../spoolframe, and only the
# root sees both directories. `src` is that subset of the repo, with this
# project at `spoolframe-linux/` and the server workspace at `spoolframe/`.
# Once spoolframe-types comes from the coldsteel registry, this moves back
# into the subflake.
{
  craneLib,
  src,
  pkg-config,
  glib,
  wrapGAppsHook4,
  gtk4,
  libadwaita,
  libGL,
  gst_all_1,
  lib,
}: let
  # The same plugins as the dev shell (devenv.nix). wrapGAppsHook4 puts them
  # on GST_PLUGIN_SYSTEM_PATH_1_0 in the wrapper, with GTK's icon and schema
  # paths, so the binary runs outside the dev shell.
  gstPlugins = with gst_all_1; [
    gstreamer
    gst-plugins-base
    gst-plugins-good
    gst-plugins-bad
    gst-plugins-ugly
    gst-libav
  ];

  commonArgs = {
    inherit src;
    strictDeps = true;
    pname = "spoolframe-linux";
    version = "0.1.0";
    cargoLock = src + "/spoolframe-linux/Cargo.lock";
    cargoToml = src + "/spoolframe-linux/Cargo.toml";
    # Build in the project's directory, so the path dependency on
    # ../spoolframe resolves inside the unpacked source.
    postUnpack = ''
      cd "$sourceRoot/spoolframe-linux"
      sourceRoot="."
    '';
    # glib: glib-compile-resources, which build.rs runs for the icons.
    nativeBuildInputs = [pkg-config glib wrapGAppsHook4];
    # libGL: egl.pc, for the video sink's GL path (see devenv.nix).
    buildInputs = [gtk4 libadwaita libGL] ++ gstPlugins;
  };

  # Build dependencies separately for caching.
  cargoArtifacts = craneLib.buildDepsOnly commonArgs;
in
  craneLib.buildPackage (commonArgs
    // {
      inherit cargoArtifacts;

      cargoExtraArgs = "-p spoolframe-linux-app";
      # The tests run in the dev shell's ci:test; running them here too would
      # build the workspace a second time in test mode.
      doCheck = false;

      meta = {
        description = "Spoolframe Linux client: Relm4, libadwaita, GStreamer.";
        license = lib.licenses.gpl3Plus;
        mainProgram = "spoolframe-linux";
      };
    })
