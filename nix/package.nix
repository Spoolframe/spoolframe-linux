# spoolframe-linux — Spoolframe Linux client: Relm4, libadwaita, GStreamer.
#
# Built with crane. flake.nix passes `src` (the Rust sources, the fonts and
# the launcher data) and its craneLib.
{
  craneLib,
  src,
  pkg-config,
  glib,
  wrapGAppsHook4,
  gtk4,
  libadwaita,
  libGL,
  systemdLibs,
  dbus,
  libfido2,
  libsecret,
  openssl,
  rustPlatform,
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
    version = "0.6.1";
    # glib: glib-compile-resources, which build.rs runs for the icons.
    # bindgenHook: libclang for fido-hid-rs's bindings.
    nativeBuildInputs = [pkg-config glib wrapGAppsHook4 rustPlatform.bindgenHook];
    # libGL: egl.pc, for the video sink's GL path (see devenv.nix);
    # Passkeys: systemdLibs (libudev) for security keys; dbus (BlueZ) and
    # openssl (the tunnel's TLS) for the phone flow.
    # libfido2: a security key as a portable identity's root (P7-08).
    # libsecret: the device key and sessions in the Secret Service.
    buildInputs = [gtk4 libadwaita libGL systemdLibs dbus openssl libfido2 libsecret] ++ gstPlugins;
  };

  # Build dependencies separately for caching.
  cargoArtifacts = craneLib.buildDepsOnly commonArgs;
in
  craneLib.buildPackage (commonArgs
    // {
      inherit cargoArtifacts;

      cargoExtraArgs = "-p spoolframe-linux-app";
      # The launcher and its icons: the app's id is app.spoolframe.relm4,
      # which MPRIS names as its desktop entry too.
      postInstall = ''
        data=crates/spoolframe-linux-app/data
        install -Dm644 $data/app.spoolframe.relm4.desktop \
          $out/share/applications/app.spoolframe.relm4.desktop
        mkdir -p $out/share/icons
        cp -r $data/icons/hicolor $out/share/icons/
      '';
      # The tests run in the dev shell's ci:test; running them here too would
      # build the workspace a second time in test mode.
      doCheck = false;

      meta = {
        description = "Spoolframe Linux client: Relm4, libadwaita, GStreamer.";
        license = lib.licenses.gpl3Plus;
        mainProgram = "spoolframe-linux";
      };
    })
