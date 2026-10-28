# NixOS module for the Spoolframe Linux client: the app, and what it needs
# from the system that a user cannot set up alone.
#
# - Security keys (passkeys, P3-19): the app opens FIDO keys itself through
#   /dev/hidraw*, which is root-only unless a udev rule hands it to the
#   logged-in user. libfido2's 70-u2f.rules does that (uaccess).
# - A passkey on a phone proves the phone is near over Bluetooth, through
#   BlueZ on the system bus.
# - Hardware video decoding goes through VA-API (vah265dec and friends),
#   which needs the system's graphics drivers.
# - Invite links (spoolframe://invite?…, P6-01) open in the app: its
#   desktop entry handles the scheme, and the module makes it the default.
#
# flake.nix passes `packageFor`, so the module finds this flake's package
# for the system it is used on.
{packageFor}: {
  config,
  lib,
  pkgs,
  ...
}: let
  cfg = config.programs.spoolframe;
in {
  options.programs.spoolframe = {
    enable = lib.mkEnableOption "the Spoolframe Linux client";

    package = lib.mkOption {
      type = lib.types.package;
      default = packageFor pkgs;
      defaultText = lib.literalMD "this flake's `spoolframe-linux` package";
      description = "The Spoolframe Linux client to install.";
    };

    securityKeys = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = ''
        Let logged-in users open FIDO2 security keys, for signing in with a
        passkey on a key: adds libfido2's udev rules.
      '';
    };

    phonePasskeys = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = ''
        Turn Bluetooth on (BlueZ), for signing in with a passkey on a phone.
        Only sets a default: a host that configures Bluetooth itself keeps
        its setting.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    environment.systemPackages = [cfg.package];
    services.udev.packages = lib.mkIf cfg.securityKeys [pkgs.libfido2];
    hardware.bluetooth.enable = lib.mkIf cfg.phonePasskeys (lib.mkDefault true);
    # VA-API decoding; the drivers come from the host's GPU configuration.
    hardware.graphics.enable = lib.mkDefault true;
    # An invite link opened in a browser or a file manager.
    xdg.mime.defaultApplications."x-scheme-handler/spoolframe" =
      lib.mkDefault "app.spoolframe.relm4.desktop";
  };
}
