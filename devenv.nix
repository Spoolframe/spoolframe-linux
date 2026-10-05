{
  pkgs,
  lib,
  ...
}: let
  # GStreamer plugins the player loads at run time: demuxers and decoders
  # for the common containers and codecs, libav for the rest, and VA-API
  # hardware decoding (in -bad). gtk4paintablesink is not here: the app
  # links it in (gst-plugin-gtk4), so it uses the app's own GTK.
  gstPlugins = with pkgs.gst_all_1; [
    gstreamer
    gst-plugins-base
    gst-plugins-good
    gst-plugins-bad
    gst-plugins-ugly
    gst-libav
  ];
in {
  # Rust toolchain, cargo-nextest and the ci:* tasks come from
  # molds/devenv/rust — nothing to redefine here.

  # GTK 4 and libadwaita for the UI, GStreamer for the player; pkg-config
  # finds them for the -sys crates. The theme picker needs libadwaita 1.6
  # or newer.
  packages =
    [
      pkgs.cargo-deny
      pkgs.pkg-config
      pkgs.gtk4
      pkgs.libadwaita
      # egl.pc: the video sink's GL path (gst-plugin-gtk4's wayland and
      # x11egl features) links GStreamer's EGL library.
      pkgs.libGL
      # Passkeys (P3-19): libudev finds security keys; the phone flow
      # reaches BlueZ over D-Bus (libdbus) and its tunnel over TLS
      # (OpenSSL, through native-tls).
      pkgs.systemdLibs
      pkgs.dbus
      pkgs.openssl
    ]
    ++ gstPlugins;

  # fido-hid-rs generates its Linux bindings with bindgen.
  env.LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";

  # Nix keeps each plugin set in its own store path; GStreamer finds them
  # only through this list.
  env.GST_PLUGIN_SYSTEM_PATH_1_0 =
    lib.makeSearchPathOutput "lib" "lib/gstreamer-1.0" gstPlugins;

  tasks."spoolframe-linux:licenses" = {
    description = "Check the Linux client license boundary";
    exec = "cargo deny --locked check licenses";
  };
  tasks."ci:static".after = ["spoolframe-linux:licenses"];

  enterShell = ''
    echo "spoolframe-linux dev environment"
  '';
}
