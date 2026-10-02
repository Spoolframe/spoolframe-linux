# spoolframe-linux

Spoolframe Linux client: Relm4, libadwaita, GStreamer.

## Development

```bash
devenv shell
cargo run -p spoolframe-linux-cli -- --version
devenv --no-tui tasks run --profile ci ci:static ci:style ci:test
```
