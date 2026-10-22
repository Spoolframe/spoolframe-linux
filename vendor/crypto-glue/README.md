# Crypto Glue - Now Safe for Human Consumption! &emsp; [![Latest Version]][crates.io] [![Latest Docs]][githubdocs]

[Latest Version]: https://img.shields.io/crates/v/crypto_glue.svg
[crates.io]: https://crates.io/crates/crypto_glue
[Latest Docs]: https://github.com/kanidm/crypto-glue/actions/workflows/docs.yml/badge.svg?branch=main
[githubdocs]: https://kanidm.github.io/crypto-glue/crypto_glue/

This is a glue crate that combines the rustcrypto ecosystem into a single place.

## Why?

The rustcrypto ecosystem is a highquality cryptographic provider, however it is made up of many
micro-crates. Additionally, those crates are bound together by trait-crates, which requires a
delecate process to add any single crate from the rustcrypto ecosystem. This also enables as many
traits and features as possible so that documentation is complete.

For example, say that I add the `rsa` crate. Now I want to encode my public key to pkcs8. If I were
to run `cargo doc` I would not find a method to do this. That's because the method is from the
pkcs8 crate as a trait, and without it in scope the documentation won't show me it exists.

Generally this means if you want something from the rustcrypto ecosystem, you can reach for your
glue bottle instead, and trust that it has nice type aliases and all the features you need.
