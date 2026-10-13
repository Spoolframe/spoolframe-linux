# Flatpak

`casa.coldsteel.Spoolframe.yml` builds the client as a Flatpak on the GNOME
runtime, for a bundle installed with `flatpak install`. Not on Flathub: the
SDK crates come from the Coldsteel cargo registry, not crates.io.

## Build a bundle

From the repository root, with the runtime installed once:

```sh
flatpak --user remote-add --if-not-exists flathub https://dl.flathub.org/repo/flathub.flatpakrepo
flatpak --user install flathub org.gnome.Sdk//51 org.gnome.Platform//51 \
  org.freedesktop.Sdk.Extension.rust-stable//26.08 \
  org.freedesktop.Sdk.Extension.llvm22//26.08

flatpak-builder --user --force-clean --state-dir=target/flatpak-state \
  --repo=target/flatpak-repo target/flatpak-build flatpak/casa.coldsteel.Spoolframe.yml
flatpak build-bundle target/flatpak-repo target/spoolframe.flatpak casa.coldsteel.Spoolframe \
  --runtime-repo=https://dl.flathub.org/repo/flathub.flatpakrepo
```

Everything it makes stays under `target/`, which git ignores.

On NixOS without Flatpak installed, `nix shell nixpkgs#flatpak
nixpkgs#flatpak-builder nixpkgs#appstream` gives the commands and the
appstreamcli flatpak-builder runs.

Install it with `flatpak install --user target/spoolframe.flatpak`; the runtime
comes from Flathub. Run it from the desktop or with
`flatpak run casa.coldsteel.Spoolframe`.

## When Cargo.lock changes

The build has no network: every crate is listed in `cargo-sources.json`.
Make it again after any change to `Cargo.lock`, with
[flatpak-cargo-generator](https://github.com/flatpak/flatpak-builder-tools/tree/master/cargo):

```sh
python3 flatpak-cargo-generator.py Cargo.lock -o flatpak/cargo-sources.json
python3 flatpak/coldsteel-sources.py Cargo.lock flatpak/cargo-sources.json
```

The generator takes every registry crate from crates.io; the second step
points the Spoolframe SDK crates at the Coldsteel registry they come from,
and lets the offline build use them from the vendored sources too.

## Permissions

| grant | for |
|---|---|
| network, IPC, Wayland/X11, PulseAudio | the app itself |
| `--device=all` | the GPU (VA-API, GL) and security keys (`/dev/hidraw*`) for passkeys |
| Bluetooth, `org.bluez` | a passkey on a phone |
| `org.mpris.MediaPlayer2.spoolframe.*` | media keys and the desktop's player controls |

Saved sign-ins go through the Secret portal, and `spoolframe://` invite
links open the app through its desktop entry. The app still confines itself
(Landlock and seccomp) inside Flatpak's sandbox; `SPOOLFRAME_NO_SANDBOX=1`
turns that off as anywhere else.
