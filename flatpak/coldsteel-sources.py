#!/usr/bin/env python3
"""Fix up flatpak-cargo-generator's output for the Coldsteel registry.

flatpak-cargo-generator downloads every registry crate from crates.io and
replaces only crates.io with the vendored sources. The Spoolframe SDK crates
come from the Coldsteel registry instead (.cargo/config.toml), so this
rewrites their download URLs to that registry's and replaces it with the
vendored sources too. Run it after the generator, from the repo root:

    python3 flatpak/coldsteel-sources.py Cargo.lock flatpak/cargo-sources.json
"""

import json
import sys
import tomllib

INDEX = "sparse+https://cargo.coldsteel.casa/api/v1/crates/"
# The registry's config.json `dl`.
DOWNLOAD = "https://cargo.coldsteel.casa/api/v1/crates/dl/{name}/{version}/download"
CRATES_IO = "https://static.crates.io/crates/{name}/{name}-{version}.crate"
REPLACE = f'\n[source.coldsteel-registry]\nregistry = "{INDEX}"\nreplace-with = "vendored-sources"\n'


def main(lock_path: str, sources_path: str) -> None:
    with open(lock_path, "rb") as f:
        lock = tomllib.load(f)
    ours = {
        CRATES_IO.format(name=p["name"], version=p["version"]): DOWNLOAD.format(
            name=p["name"], version=p["version"]
        )
        for p in lock["package"]
        if p.get("source") == INDEX
    }
    with open(sources_path) as f:
        sources = json.load(f)

    rewritten = 0
    configs = 0
    for source in sources:
        url = source.get("url")
        if url in ours:
            source["url"] = ours[url]
            rewritten += 1
        if source.get("dest") == "cargo" and source.get("dest-filename") == "config":
            if REPLACE not in source["contents"]:
                source["contents"] += REPLACE
            configs += 1

    already = sum(1 for s in sources if s.get("url") in ours.values())
    if already != len(ours) or configs != 1:
        sys.exit(
            f"expected {len(ours)} Coldsteel crates and one cargo config, "
            f"found {already} and {configs}"
        )
    with open(sources_path, "w") as f:
        json.dump(sources, f, indent=4)
        f.write("\n")
    print(f"{rewritten} crate(s) pointed at the Coldsteel registry")


if __name__ == "__main__":
    if len(sys.argv) != 3:
        sys.exit(__doc__)
    main(sys.argv[1], sys.argv[2])
