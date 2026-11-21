#!/usr/bin/env python3
"""Generate THIRD_PARTY_LICENSES.txt from the real dependency graph (+ the non-Rust components).

Run from the repo root:  python3 tools/gen_notices.py
Writes THIRD_PARTY_LICENSES.txt and copies it, PRIVACY.md and the license summary into the
Android app's assets so the in-app "Privacy & licenses" screen shows them.
"""
import hashlib, json, os, shutil, subprocess, sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SPDX = Path("/usr/share/licenses/spdx")
LICENSE_GLOBS = ("LICENSE", "LICENCE", "COPYING", "NOTICE", "UNLICENSE", "COPYRIGHT")


def metadata(target):
    out = subprocess.check_output(
        ["cargo", "metadata", "--format-version", "1", "--filter-platform", target, "--locked"], cwd=ROOT)
    return json.loads(out)


def reachable(meta, roots):
    """Packages reachable from `roots` through normal/build dependencies (no dev-deps)."""
    nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}
    seen, stack = set(), list(roots)
    while stack:
        i = stack.pop()
        if i in seen:
            continue
        seen.add(i)
        for d in nodes[i]["deps"]:
            if any(k["kind"] in (None, "build") for k in d["dep_kinds"]):
                stack.append(d["pkg"])
    return seen


def collect():
    used = {}  # (name, version) -> dict
    for target, member_names, tag in [
        ("aarch64-linux-android", ["soundovern-android-ffi"], "Android app"),
        ("x86_64-unknown-linux-gnu", ["soundovern-server", "soundovern-cli-client"], "Desktop server/client"),
    ]:
        meta = metadata(target)
        pk = {p["id"]: p for p in meta["packages"]}
        roots = [i for i, p in pk.items() if p["name"] in member_names]
        for i in reachable(meta, roots):
            p = pk[i]
            if p["source"] is None:  # our own workspace crates
                continue
            e = used.setdefault((p["name"], p["version"]), {
                "name": p["name"], "version": p["version"], "license": p.get("license") or "UNKNOWN",
                "repo": p.get("repository") or p.get("homepage") or "", "dir": Path(p["manifest_path"]).parent, "tags": set()})
            e["tags"].add(tag)
    return sorted(used.values(), key=lambda e: (e["name"], e["version"]))


def license_files(d):
    files = []
    for f in sorted(d.iterdir()):
        if f.is_file() and f.name.upper().startswith(LICENSE_GLOBS):
            files.append(f)
    # some crates keep licenses in a subfolder (e.g. opus/COPYING)
    return files


def main():
    crates = collect()
    groups = {}  # text-hash -> (text, [labels])
    missing = []
    for c in crates:
        files = license_files(c["dir"])
        if not files:
            missing.append(c)
            continue
        for f in files:
            txt = f.read_text(errors="replace").strip()
            h = hashlib.sha256(txt.encode()).hexdigest()
            groups.setdefault(h, (txt, []))[1].append(f"{c['name']} {c['version']}")

    out = []
    w = out.append
    w("THIRD-PARTY LICENSES AND NOTICES\n")
    w("Soundovern itself is licensed under MIT OR Apache-2.0 (see LICENSE-MIT and LICENSE-APACHE).")
    w("It includes or links the following third-party software. Each component keeps its own license.\n")
    w("=" * 78)
    w("1. NON-RUST COMPONENTS")
    w("=" * 78)
    w("""Opus codec (libopus), BSD-3-Clause — statically linked into the app and the server.
https://opus-codec.org/

Oboe, Apache-2.0 — Copyright (C) 2018 The Android Open Source Project. Statically linked into the Android app.
https://github.com/google/oboe

LLVM libc++ and libc++abi, Apache-2.0 WITH LLVM-exception — statically linked into the Android app (Android NDK).
https://libcxx.llvm.org/  /  https://llvm.org/LICENSE.txt

AndroidX, Jetpack Compose, Material 3 (Apache-2.0, Copyright The Android Open Source Project);
Kotlin standard library and kotlinx.coroutines (Apache-2.0, Copyright JetBrains s.r.o. and contributors).
These are included in the Android app. https://developer.android.com/jetpack  /  https://kotlinlang.org

The Rust standard library and compiler runtime (MIT OR Apache-2.0, Copyright The Rust Project Contributors).

PipeWire (client library libpipewire, MIT) is linked dynamically by the desktop server from the user's system.
""")
    opus = next((p for p in (Path.home() / ".cargo/registry/src").glob("*/opusic-sys-*/opus/COPYING")), None)
    if opus:
        w("--- libopus license text ---\n")
        w(opus.read_text().strip() + "\n")
    w("--- LLVM exception to the Apache-2.0 license (libc++ / libc++abi) ---\n")
    w("""As an exception, if, as a result of your compiling your source code, portions
of this Software are embedded into an Object form of such source code, you
may redistribute such embedded portions in such Object form without complying
with the conditions of Sections 4(a), 4(b) and 4(d) of the License.

In addition, if you combine or link compiled forms of this Software with
software that is licensed under the GPLv2 ("Combined Software") and if a
court of competent jurisdiction determines that the patent provision (Section
3), the indemnity provision (Section 9) or other Section of the License
conflicts with the conditions of the GPLv2, you may retroactively and
prospectively choose to deem waived or otherwise exclude such Section(s) of
the License, but only in their entirety and only with respect to the Combined
Software.
""")
    apache = SPDX / "Apache-2.0.txt"
    w("=" * 78)
    w("2. APACHE LICENSE 2.0 (applies wherever Apache-2.0 is named above and below)")
    w("=" * 78)
    w((apache.read_text().strip() if apache.exists() else "See https://www.apache.org/licenses/LICENSE-2.0.txt") + "\n")

    w("=" * 78)
    w("3. RUST CRATES")
    w("=" * 78)
    w("Crate, version, SPDX license expression, and where it is used:\n")
    for c in crates:
        w(f"  {c['name']} {c['version']}  [{c['license']}]  ({', '.join(sorted(c['tags']))})")
    w("\nLicense and notice texts shipped with these crates (identical texts are listed once):\n")
    for h, (txt, who) in sorted(groups.items(), key=lambda kv: kv[1][1][0]):
        w("-" * 78)
        w("Applies to: " + ", ".join(sorted(set(who))))
        w("-" * 78)
        w(txt + "\n")
    if missing:
        w("-" * 78)
        w("Crates that ship no license file in their package; their license is the SPDX expression listed above:")
        w("-" * 78)
        for c in missing:
            w(f"  {c['name']} {c['version']}  [{c['license']}]  {c['repo']}")
    text = "\n".join(out) + "\n"
    (ROOT / "THIRD_PARTY_LICENSES.txt").write_text(text)

    assets = ROOT / "android/app/src/main/assets"
    assets.mkdir(parents=True, exist_ok=True)
    (assets / "THIRD_PARTY_LICENSES.txt").write_text(text)
    shutil.copy(ROOT / "PRIVACY.md", assets / "PRIVACY.txt")
    lic = ("Soundovern is licensed under either of\n\n  * Apache License, Version 2.0 (LICENSE-APACHE)\n  * MIT license (LICENSE-MIT)\n\n"
           "at your option.\n\nCopyright (c) Soundovern contributors.\n\n" + (ROOT / "LICENSE-MIT").read_text())
    (assets / "LICENSE.txt").write_text(lic)
    print(f"{len(crates)} crates, {len(groups)} distinct license texts, {len(missing)} without a license file, {len(text)//1024} KiB")
    for c in missing:
        print("  no license file:", c["name"], c["version"], c["license"])


if __name__ == "__main__":
    sys.exit(main())
