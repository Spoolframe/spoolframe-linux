# Vendored crates

Four crates of the webauthn-rs family, patched so that the `rsa` crate is
not built. The root `Cargo.toml` takes them through `[patch.crates-io]`.

## Why

`rsa` 0.9 has RUSTSEC-2023-0071 (the Marvin timing attack), and no fixed
release exists. `crypto-glue` 0.1.15 depends on `rsa` without a feature to
turn it off, and every webauthn-rs crate takes `crypto-glue` with its
default features. The client needs no RSA: it checks no signature or
attestation of a key, it passes them to the server.

## What changed

Each copy is the crates.io release, with the changes below. The registry
files `Cargo.lock`, `.cargo_vcs_info.json`, `.cargo-ok`, and crypto-glue's
`.github/` and `.gitignore` are removed. `LICENSE` (the MPL-2.0 text, a
copy of `scripts/licenses/spdx/MPL-2.0.txt`) is added, because the releases
include no license text.
`Cargo.toml.orig` has the same changes as `Cargo.toml`, for upstream.

| Crate | Version | Upstream commit | Change |
|---|---|---|---|
| crypto-glue | 0.1.15 | kanidm/crypto-glue `d39bfc11` | `rsa` is an optional dependency behind the feature `rsa` (in `default`). RSA code and RSA tests are behind `#[cfg(feature = "rsa")]`. The signature traits come through `ecdsa::signature` (the same crate). With `rsa` off, an RSA certificate signature is `SignatureAlgorithmNotImplemented`. |
| webauthn-rs-core | 0.6.1-dev | kanidm/webauthn-rs `8a80623a` | Feature `rsa` (in `default`) forwards to `crypto-glue/rsa`. `crypto-glue` without default features (`alloc` only). With `rsa` off, an RS256 COSE key is `COSEKeyInvalidType`. The four tests with RSA keys or RSA-signed CAs need `rsa`; two new tests cover RS256 with `rsa` on and off. |
| webauthn-authenticator-rs | 0.6.1-dev | kanidm/webauthn-rs `8a80623a` | Feature `rsa` (in `default`) forwards to `webauthn-rs-core/rsa`. `webauthn-rs-core` and `crypto-glue` without default features. |
| webauthn-attestation-ca | 0.6.1-dev | kanidm/webauthn-rs `8a80623a` | `crypto-glue` without default features (`alloc` only). It only holds certificates. |

With default features, each crate builds and behaves as the release does.
The client uses `webauthn-authenticator-rs` with `default-features = false`,
so `rsa` is off.

## When to remove

Remove a copy, and its `[patch.crates-io]` line, when its upstream releases
a version where `rsa` is optional, or when `rsa` has a fixed release. Then
run `cargo tree -i rsa`. Upstream patches for these changes are kept
outside this repository.
