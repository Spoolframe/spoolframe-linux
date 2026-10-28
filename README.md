# spoolframe-linux

Spoolframe Linux client: Relm4, libadwaita, GStreamer.

## Building

Every crate comes from crates.io, the Spoolframe SDK crates
(`spoolframe-types`, `-protocol`, `-client-sdk`, `-policy`, `-test-dir`)
too. The one exception is noq-proto, from a git commit of
github.com/n0-computer/noq that has its fix for playback on a lossy link
(Cargo.toml's `[patch.crates-io]`).

With Nix, the package (the app, its launcher and icons):

```bash
nix build
./result/bin/spoolframe-linux --version
```

In the dev shell, which has the Rust toolchain, GTK 4, libadwaita and
GStreamer:

```bash
devenv shell
cargo run -p spoolframe-linux-app -- --version
```

CI runs the same phases in the `ci` profile:

```bash
devenv --no-tui tasks run --profile ci ci:style ci:static ci:test
```

`ci:static` runs the license check (below) and clippy; `ci:test` runs the
tests with nextest. Tests that need a display, GStreamer's plugins or a
running server are `#[ignore]`d; each says what it needs.

A Flatpak bundle builds from `flatpak/` (see `flatpak/README.md`).

To see the app in plain libadwaita, as on a desktop where no theme of
Spoolframe's is chosen, without changing your own settings or desktop
theme: `scripts/adwaita.sh` (light) or `scripts/adwaita.sh dark`.

## Installing on NixOS

This flake has a NixOS module for the client:

```nix
{
  inputs.spoolframe-linux.url = "git+https://git.coldsteel.casa/Spoolframe/spoolframe-linux.git";
  # ...
  imports = [inputs.spoolframe-linux.nixosModules.default];
  programs.spoolframe.enable = true;
}
```

It installs the app (with its launcher and icons) and sets up what a user
cannot alone:

- `securityKeys` (default on): libfido2's udev rules, so the logged-in user
  can open FIDO2 security keys for passkey sign-in.
- `phonePasskeys` (default on): Bluetooth, for a passkey on a phone (a
  default only; a host's own Bluetooth setting wins).
- the graphics stack (a default), for VA-API hardware decoding.

## License

GPL-3.0-or-later (see `LICENSE`). The client links the permissively
licensed Spoolframe protocol, types and client SDK (MIT OR Apache-2.0);
the server is a separate program under the AGPL and is reached only over
the wire.

Run `devenv tasks run spoolframe-linux:deny` to check the Rust dependency
graph with cargo-deny (`deny.toml`: licenses, advisories, sources and
bans). This also runs before `ci:static`. The license policy permits the
named GPL application crates and LGPL/MPL dependencies, rejects AGPL, and
requires an explicit decision for new licenses. Native libraries, codecs
and bundled assets are outside the Cargo dependency graph.

Bundled with the app:

- Fonts in `crates/spoolframe-linux-app/fonts/`: Figtree, Outfit,
  JetBrains Mono and Bricolage Grotesque, each under the SIL Open Font
  License 1.1 (the `*-OFL.txt` beside it).
- Icons in `crates/spoolframe-linux-app/icons/`: Lucide, under the ISC
  license (`icons/LICENSE`).
