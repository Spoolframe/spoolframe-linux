#!/usr/bin/env python3
"""Writes the list behind Settings › About › Open source licenses.

    scripts/gen-licenses.py           write crates/spoolframe-linux-app/data/licenses.json
    scripts/gen-licenses.py --check   fail if that file is not what this would write

Run it inside `devenv shell` after Cargo.lock or the Flatpak manifest
changes; CI runs it with --check (devenv.nix, spoolframe-linux:licenses-list).

The file is committed, not made by build.rs: the Flatpak and Nix builds
have no network, and `cargo metadata` would need it.

What is listed is what the app's binary links, on Linux:

- `cargo tree -e normal,no-proc-macro` from spoolframe-linux-app, for
  x86_64 and aarch64 Linux. Normal dependencies only: no build-dependencies,
  no dev-dependencies, no proc-macros (they run in the compiler, none of
  their code is in the binary), no crates for other platforms. cargo tree
  resolves features as a build of the app would; `cargo metadata`'s resolve
  turns on what dev-dependencies ask for too, and lists more.
- Fixed entries, read from the repo (fixed_components): the Catppuccin
  palette, the Lucide icons (icons/LICENSE, VERSION in icons/convert.sh)
  and the fonts in fonts/ (their name tables and OFL texts).
- The C libraries the Flatpak manifest builds as modules (FLATPAK below).

GTK, libadwaita, GStreamer and the rest of the GNOME runtime are not
listed: the runtime carries their notices.

Groups: the Spoolframe SDK crates (spoolframe-* from the Coldsteel
registry) and every crate reached through them are "sdk"; this
workspace's crates and the rest are "app"; the Flatpak's modules are
"flatpak".

Texts: each crate's own license files, matched to the SPDX ids of its
license expression by their wording. An id with no file of its own gets
the standard text from scripts/licenses/spdx/<id>.txt. Texts are kept
once each, however many crates share them.

Python 3 standard library only.
"""

import argparse
import json
import os
import re
import struct
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
OUT = os.path.join(ROOT, "crates", "spoolframe-linux-app", "data", "licenses.json")
SPDX_DIR = os.path.join(ROOT, "scripts", "licenses", "spdx")
FLATPAK_DIR = os.path.join(ROOT, "scripts", "licenses", "flatpak")
ICONS_DIR = os.path.join(ROOT, "crates", "spoolframe-linux-app", "icons")
FONTS_DIR = os.path.join(ROOT, "crates", "spoolframe-linux-app", "fonts")
CATPPUCCIN_LICENSE = os.path.join(ROOT, "scripts", "licenses", "catppuccin.LICENSE")
MANIFEST = os.path.join(ROOT, "flatpak", "casa.coldsteel.Spoolframe.yml")

APP = "spoolframe-linux-app"
TARGETS = ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"]
SDK_REGISTRY = "cargo.coldsteel.casa"

# The Flatpak manifest's modules: license and the license file, copied
# from the module's release archive (the URL and sha256 in the manifest).
# `version` is the release that file came from: when the manifest moves to
# another, --check fails until the file is looked at again.
FLATPAK = {
    "libcbor": {"version": "0.13.0", "license": "MIT", "file": "libcbor.LICENSE.md"},
    "libfido2": {"version": "1.17.0", "license": "BSD-2-Clause", "file": "libfido2.LICENSE"},
}

# Fonts in the app's fonts/ folder: file name to the name shown. Each has
# its own OFL text next to it (<stem>-OFL.txt). --check fails for a .ttf
# with no entry here, and for one whose name table no longer says OFL 1.1
# or whose copyright differs from the first line of that text.
FONTS = {
    "BricolageGrotesque.ttf": "Bricolage Grotesque",
    "Figtree.ttf": "Figtree",
    "JetBrainsMono.ttf": "JetBrains Mono",
    "Outfit.ttf": "Outfit",
}

# A license file's id, by its wording. The first that matches wins, so the
# more specific come first.
SIGNATURES = [
    ("Apache-2.0 WITH LLVM-exception", ["Apache License", "LLVM Exceptions"]),
    ("Apache-2.0", ["Apache License", "Version 2.0, January 2004"]),
    ("MIT", ["Permission is hereby granted, free of charge",
             "The above copyright notice and this permission notice shall be included"]),
    ("MIT-0", ["Permission is hereby granted, free of charge"]),
    ("MPL-2.0", ["Mozilla Public License", "2.0"]),
    ("BSD-3-Clause", ["Redistribution and use in source and binary forms", "Neither the name"]),
    ("BSD-2-Clause", ["Redistribution and use in source and binary forms"]),
    ("ISC", ["Permission to use, copy, modify, and/or distribute this software for any"]),
    ("Unlicense", ["This is free and unencumbered software released into the public domain"]),
    ("CC0-1.0", ["CC0 1.0 Universal"]),
    ("Unicode-3.0", ["UNICODE LICENSE V3"]),
    ("Zlib", ["This software is provided 'as-is', without any express or implied"]),
    ("BSL-1.0", ["Boost Software License - Version 1.0"]),
    ("CDLA-Permissive-2.0", ["Community Data License Agreement", "Permissive", "Version 2.0"]),
    ("GPL-3.0", ["GNU GENERAL PUBLIC LICENSE", "Version 3, 29 June 2007"]),
]

LICENSE_FILE = re.compile(r"(?i)^(licen[cs]e|copying|unlicense|copyright)")
NOTICE_FILE = re.compile(r"(?i)^notice")
# Copyright lines in a license's own template or about the license itself.
NOT_A_HOLDER = re.compile(
    r"(?i)\[yyyy\]|\{yyyy\}|<year>|\[year\]|<copyright|\[name of|"
    r"notice|license|Free Software Foundation|holders? or contributors"
)


def cargo(*args):
    return subprocess.run(
        ["cargo", *args], cwd=ROOT, check=True, capture_output=True, text=True
    ).stdout


def linked_graph():
    """The (name, version) of every crate the app links, and its edges."""
    edges = {}
    for target in TARGETS:
        tree = cargo(
            "tree", "--locked", "-p", APP, "-e", "normal,no-proc-macro",
            "--target", target, "--prefix", "depth", "-f", "{p}",
        )
        stack = []
        for line in tree.splitlines():
            m = re.match(r"^(\d+)(\S+) v(\S+)", line)
            if not m:
                continue
            depth, key = int(m.group(1)), (m.group(2), m.group(3))
            del stack[depth:]
            edges.setdefault(key, set())
            if stack:
                edges[stack[-1]].add(key)
            stack.append(key)
    return edges


def normalize(text):
    text = text.replace("\r\n", "\n").replace("\r", "\n")
    return "\n".join(line.rstrip() for line in text.split("\n")).strip("\n") + "\n"


def read(path):
    with open(path, encoding="utf-8", errors="replace") as f:
        return normalize(f.read())


def classify(text):
    """The ids a file's wording matches: more than one for a file that
    holds several licenses."""
    found = []
    for spdx, phrases in SIGNATURES:
        if all(p in text for p in phrases):
            # Apache-2.0 WITH LLVM-exception is Apache-2.0 too; MIT is MIT-0's wording and more.
            if spdx == "Apache-2.0" and "Apache-2.0 WITH LLVM-exception" in found:
                continue
            if spdx == "MIT-0" and "MIT" in found:
                continue
            if spdx == "BSD-2-Clause" and "BSD-3-Clause" in found:
                continue
            found.append(spdx)
    return found


def ids(expression):
    """The license ids an SPDX expression names, `X WITH Y` as one."""
    tokens = re.findall(r"\(|\)|[^\s()]+", expression)
    out, i = [], 0
    while i < len(tokens):
        t = tokens[i]
        if t in ("(", ")", "AND", "OR"):
            i += 1
            continue
        if i + 2 < len(tokens) and tokens[i + 1] == "WITH":
            t = f"{t} WITH {tokens[i + 2]}"
            i += 2
        if t not in out:
            out.append(t)
        i += 1
    return out


def holders(texts):
    lines = []
    for text in texts:
        for line in text.split("\n"):
            line = line.strip().strip("*#/ ").strip()
            if not re.match(r"^Copyright\b", line) or NOT_A_HOLDER.search(line):
                continue
            if len(line) <= len("Copyright (c)") or line in lines:
                continue
            lines.append(line)
    return lines[:12]


def standard(spdx):
    path = os.path.join(SPDX_DIR, f"{spdx}.txt")
    if not os.path.exists(path):
        sys.exit(f"gen-licenses: no standard text for {spdx}: add {os.path.relpath(path, ROOT)}")
    return read(path)


def crate_texts(package, workspace_root, workspace):
    """{id: text} for each id in the crate's expression, and its copyright lines."""
    expression = package["license"]
    if not expression:
        sys.exit(f"gen-licenses: {package['name']} {package['version']} has no license expression")
    expression = expression.replace("/", " OR ")
    wanted = ids(expression)
    crate_dir = os.path.dirname(package["manifest_path"])
    dirs = [crate_dir] + ([workspace_root] if package["id"] in workspace else [])
    files, notices = [], []
    for d in dirs:
        for name in sorted(os.listdir(d)):
            path = os.path.join(d, name)
            if not os.path.isfile(path):
                continue
            if LICENSE_FILE.match(name):
                files.append(read(path))
            elif NOTICE_FILE.match(name):
                notices.append(read(path))
        if files:
            break
    if package.get("license_file"):
        files.append(read(os.path.join(crate_dir, package["license_file"])))

    texts = {}
    for text in files:
        matched = classify(text)
        # A file holding several licenses is not any one of them.
        if len(matched) != 1:
            continue
        for spdx in wanted:
            fits = spdx == matched[0] or (matched[0] == "GPL-3.0" and spdx.startswith("GPL-3.0"))
            if fits and spdx not in texts:
                texts[spdx] = text
    for spdx in wanted:
        if spdx not in texts:
            texts[spdx] = standard(spdx)
    # Apache-2.0 §4(d): a NOTICE file goes with the work.
    for spdx in wanted:
        if spdx.startswith("Apache-2.0") and notices:
            texts[spdx] = texts[spdx] + "\n\nNOTICE\n\n" + "\n".join(notices)

    lines = holders(files)
    if not lines and package.get("authors"):
        names = [re.sub(r"\s*<[^>]*>", "", a).strip() for a in package["authors"]]
        names = [n for n in names if n]
        if names:
            lines = ["Copyright (c) " + ", ".join(names)]
    return expression, texts, lines


def flatpak_modules():
    with open(MANIFEST, encoding="utf-8") as f:
        manifest = f.read()
    modules = {}
    name = None
    for line in manifest.splitlines():
        m = re.match(r"^  - name: (\S+)", line)
        if m:
            name = m.group(1)
            continue
        m = re.match(r"^\s+url: (\S+)", line)
        if m and name:
            version = re.search(r"(\d+(?:\.\d+)+)\.tar", m.group(1))
            modules[name] = version.group(1) if version else None
    # The app's own module is the cargo build, listed from Cargo.lock.
    modules.pop("spoolframe-linux", None)
    return modules


def font_names(path):
    """{nameID: string} of a TTF's `name` table (first record of each id)."""
    with open(path, "rb") as f:
        data = f.read()
    for i in range(struct.unpack(">H", data[4:6])[0]):
        tag, _, offset, _ = struct.unpack(">4sIII", data[12 + 16 * i:28 + 16 * i])
        if tag != b"name":
            continue
        count, strings = struct.unpack(">HH", data[offset + 2:offset + 6])
        out = {}
        for j in range(count):
            platform, _, _, name_id, length, at = struct.unpack(
                ">6H", data[offset + 6 + 12 * j:offset + 18 + 12 * j]
            )
            raw = data[offset + strings + at:offset + strings + at + length]
            out.setdefault(name_id, raw.decode("utf-16-be" if platform in (0, 3) else "latin-1"))
        return out
    sys.exit(f"gen-licenses: {os.path.relpath(path, ROOT)} has no name table")


def fixed_components(text_id):
    """Catppuccin, Lucide and the fonts: what the app ships besides crates,
    read from the repo so that --check sees drift."""
    out = []

    text = read(CATPPUCCIN_LICENSE)
    if "Copyright (c) 2021 Catppuccin" not in text or classify(text) != ["MIT"]:
        sys.exit("gen-licenses: scripts/licenses/catppuccin.LICENSE is no longer MIT, "
                 "Copyright (c) 2021 Catppuccin: update the entry in fixed_components")
    out.append({"name": "Catppuccin", "version": "palette", "group": "app", "license": "MIT",
                "copyright": holders([text]), "texts": {"MIT": text_id(text)}})

    with open(os.path.join(ICONS_DIR, "convert.sh"), encoding="utf-8") as f:
        m = re.search(r"(?m)^VERSION=(\S+)\s*$", f.read())
    if not m:
        sys.exit("gen-licenses: icons/convert.sh has no VERSION= line")
    text = read(os.path.join(ICONS_DIR, "LICENSE"))
    # Lucide's file holds ISC for Lucide and MIT for the icons from Feather.
    if classify(text) != ["MIT", "ISC"] or "Lucide Icons and Contributors" not in text:
        sys.exit("gen-licenses: icons/LICENSE is no longer ISC plus Feather's MIT: "
                 "read it and update the entry in fixed_components")
    out.append({"name": "Lucide", "version": m.group(1), "group": "app",
                "license": "ISC AND MIT", "copyright": holders([text]),
                "texts": {"ISC": text_id(text), "MIT": text_id(text)}})

    on_disk = sorted(n for n in os.listdir(FONTS_DIR) if n.endswith(".ttf"))
    for name in on_disk:
        if name not in FONTS:
            sys.exit(f"gen-licenses: fonts/{name} has no entry in FONTS in this script: "
                     "add it, with its OFL text next to it")
    for name, title in sorted(FONTS.items(), key=lambda kv: kv[1]):
        if name not in on_disk:
            sys.exit(f"gen-licenses: FONTS lists {name}, which is not in fonts/: remove it")
        names = font_names(os.path.join(FONTS_DIR, name))
        grant = names.get(13, "") + " " + names.get(14, "")
        if "SIL Open Font License, Version 1.1" not in grant:
            sys.exit(f"gen-licenses: fonts/{name} no longer says SIL OFL 1.1 in its name "
                     f"table ({names.get(13)!r}): read its license and update the entry")
        text = read(os.path.join(FONTS_DIR, name[:-4] + "-OFL.txt"))
        copyright = names[0].strip()
        if text.split("\n", 1)[0].strip() != copyright:
            sys.exit(f"gen-licenses: fonts/{name[:-4]}-OFL.txt does not start with the font's "
                     f"copyright, {copyright!r}: replace the text")
        version = re.match(r"Version\s+(\S+?)(?:;|$|\s)", names.get(5, ""))
        if not version:
            sys.exit(f"gen-licenses: fonts/{name} has no version in its name table")
        out.append({"name": title, "version": version.group(1), "group": "app",
                    "license": "OFL-1.1", "copyright": [copyright],
                    "texts": {"OFL-1.1": text_id(text)}})
    return out


def generate():
    metadata = json.loads(cargo("metadata", "--locked", "--format-version", "1"))
    workspace = set(metadata["workspace_members"])
    by_key = {}
    for package in metadata["packages"]:
        key = (package["name"], package["version"])
        if key in by_key:
            sys.exit(f"gen-licenses: two packages are {key[0]} {key[1]}")
        by_key[key] = package
    edges = linked_graph()

    def is_sdk(key):
        package = by_key[key]
        return key[0].startswith("spoolframe-") and SDK_REGISTRY in (package["source"] or "")

    sdk, todo = set(), [k for k in edges if is_sdk(k)]
    while todo:
        key = todo.pop()
        if key in sdk:
            continue
        sdk.add(key)
        todo.extend(edges[key])

    texts, text_index, components = [], {}, []

    def text_id(text):
        if text not in text_index:
            text_index[text] = len(texts)
            texts.append(text)
        return text_index[text]

    for key in sorted(edges):
        package = by_key[key]
        expression, by_id, lines = crate_texts(package, metadata["workspace_root"], workspace)
        group = "app" if package["id"] in workspace else ("sdk" if key in sdk else "app")
        components.append({
            "name": key[0],
            "version": key[1],
            "group": group,
            "license": expression,
            "copyright": lines,
            "texts": {spdx: text_id(text) for spdx, text in by_id.items()},
        })

    components.extend(fixed_components(text_id))

    modules = flatpak_modules()
    for name, version in sorted(modules.items()):
        known = FLATPAK.get(name)
        if known is None:
            sys.exit(f"gen-licenses: the Flatpak builds {name}: add it to FLATPAK in this script")
        if known["version"] != version:
            sys.exit(
                f"gen-licenses: the Flatpak builds {name} {version}, the license file is "
                f"from {known['version']}: check it in that release and update FLATPAK"
            )
        text = read(os.path.join(FLATPAK_DIR, known["file"]))
        components.append({
            "name": name,
            "version": version,
            "group": "flatpak",
            "license": known["license"],
            "copyright": holders([text]),
            "texts": {known["license"]: text_id(text)},
        })

    data = {"components": components, "texts": texts}
    # One entry a line, so a change in Cargo.lock reads as a small diff.
    return json.dumps(data, indent=0, sort_keys=True, ensure_ascii=False) + "\n"


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("--check", action="store_true", help="fail if the committed list is stale")
    args = parser.parse_args()
    data = generate()
    if args.check:
        try:
            with open(OUT, encoding="utf-8") as f:
                current = f.read()
        except FileNotFoundError:
            current = None
        if current != data:
            sys.exit(
                f"gen-licenses: {os.path.relpath(OUT, ROOT)} is out of date with Cargo.lock "
                "or the Flatpak manifest: run scripts/gen-licenses.py in devenv shell and commit it"
            )
        return
    with open(OUT, "w", encoding="utf-8") as f:
        f.write(data)
    summary = {}
    for c in json.loads(data)["components"]:
        summary[c["group"]] = summary.get(c["group"], 0) + 1
    print(f"wrote {os.path.relpath(OUT, ROOT)}: {summary}, {len(json.loads(data)['texts'])} texts")


if __name__ == "__main__":
    main()
