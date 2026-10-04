# spoolframe-linux

Spoolframe Linux client: Relm4, libadwaita, GStreamer.

## Development

```bash
devenv shell
cargo run -p spoolframe-linux-app -- --version
devenv --no-tui tasks run --profile ci ci:static ci:style ci:test
```

## License

GPL-3.0-or-later (see `LICENSE`). The client links the permissively
licensed Spoolframe protocol, types and client SDK (MIT OR Apache-2.0);
the server is a separate program under the AGPL and is reached only over
the wire.

Bundled with the app:

- Fonts in `crates/spoolframe-linux-app/fonts/`: Figtree, Outfit,
  JetBrains Mono and Bricolage Grotesque, each under the SIL Open Font
  License 1.1 (the `*-OFL.txt` beside it).
- Icons in `crates/spoolframe-linux-app/icons/`: Lucide, under the ISC
  license (`icons/LICENSE`).
