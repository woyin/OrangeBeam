#!/usr/bin/env python3
"""Regenerate notices from `cargo metadata --format-version 1 --locked` JSON.

Usage: python3 scripts/collect-licenses.py metadata.json THIRD-PARTY-LICENSES.txt
objc2 publishes licenses at the monorepo root; fetch their exact crate VCS revision.
"""
import concurrent.futures
import json
from pathlib import Path
import re
import sys
import urllib.request
import urllib.error

metadata = json.loads(Path(sys.argv[1]).read_text())
packages = [p for p in metadata["packages"] if p["name"] != "orange-beam"]
local, missing, revisions = {}, {}, set()
for package in packages:
    root = Path(package["manifest_path"]).parent
    files = sorted(p for p in root.iterdir() if p.is_file()
                   and p.name.upper().startswith(("LICENSE", "LICENCE", "COPYING", "NOTICE")))
    if files:
        local[package["id"]] = [(p.name, p.read_text()) for p in files]
    else:
        if package.get("repository") != "https://github.com/madsmtm/objc2":
            raise SystemExit(f"Manual license collection required: {package['name']}")
        revision = json.loads((root / ".cargo_vcs_info.json").read_text())["git"]["sha1"]
        if not re.fullmatch(r"[0-9a-f]{40}", revision):
            raise SystemExit("Invalid VCS revision")
        missing[package["id"]] = revision
        revisions.add(revision)

names = ["LICENSE.md", "LICENSE-MIT.txt", "LICENSE-APACHE.txt", "LICENSE-ZLIB.txt"]

def fetch(key):
    revision, name = key
    url = f"https://raw.githubusercontent.com/madsmtm/objc2/{revision}/{name}"
    try:
        with urllib.request.urlopen(url, timeout=30) as response:
            return key, response.read().decode()
    except urllib.error.HTTPError as error:
        if error.code == 404:
            return key, None
        raise

with concurrent.futures.ThreadPoolExecutor(max_workers=8) as pool:
    remote = dict(pool.map(fetch, [(r, name) for r in sorted(revisions) for name in names]))

parts = ["Third-party notices for orange-beam 0.2.0. Generated from Cargo.lock.\n"]
# Older objc2 revisions published an SPDX declaration and license statement only.
# Include the project's full MIT text as supplementary text, with its exact source.
url = "https://raw.githubusercontent.com/madsmtm/objc2/main/LICENSE-MIT.txt"
with urllib.request.urlopen(url, timeout=30) as response:
    parts.append(f"Supplementary objc2 project MIT text. Source: {url}\n" + response.read().decode())
seen = set()
for package in sorted(packages, key=lambda p: p["name"]):
    parts.append(f"\n{'=' * 72}\n{package['name']} {package['version']}\n"
                 f"Declared license: {package.get('license')}\n"
                 f"Repository: {package.get('repository')}\nAuthors (package metadata): {package.get('authors')}\n")
    if package["id"] in missing:
        revision = missing[package["id"]]
        parts.append(f"License bundle: objc2 revision {revision}\n")
        if revision in seen:
            parts.append("Full texts reproduced in the earlier entry for this revision.\n")
            continue
        seen.add(revision)
        texts = [(name, remote[(revision, name)]) for name in names if remote[(revision, name)] is not None]
        if not texts:
            raise SystemExit(f"License statement missing at objc2 {revision}")
    else:
        texts = local[package["id"]]
    for name, content in texts:
        parts.append(f"\n--- {name} ---\n{content}\n")
Path(sys.argv[2]).write_text("\n".join(parts))
print(f"Collected notices for {len(packages)} packages")
