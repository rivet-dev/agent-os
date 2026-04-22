#!/usr/bin/env python3
"""Build the pure-Python wheel index for the dbt closure.

Resolves the transitive dependency closure for dbt-core + dbt-duckdb,
downloads matching wheels (preferring `*-py3-none-any.whl`, building
sdists on demand), generates the warehouse-JSON-shaped index that
`micropip.set_index_urls` consumes, and writes a lockfile pinning
exact versions and sha256s.

Usage:
    python build_pure_index.py \
        --out wheels/ \
        --index wheels/index/ \
        --lockfile wheels/lockfile.json \
        --python-tag cp313 \
        --abi-tag pyodide_2025_0_wasm32
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import shutil
import subprocess
import sys
import tempfile
from datetime import datetime, timezone
from pathlib import Path

# Top-level requirements. uv pip compile resolves the transitive closure.
REQUIREMENTS = [
    "dbt-core",
    "dbt-duckdb",
    # mashumaro extras pulled in transitively via dbt-core
]

# Packages handled by separate build pipelines, NOT downloaded via pip.
# These are added to the wheels/ directory by other scripts:
#   - dbt-extractor: built via build_extractor.sh (Rust + Pyodide cross-compile)
#                    or replaced by the pure-Python shim via build_shim.sh
#   - duckdb:        downloaded via fetch_duckdb.sh (xlwings prebuilt wheel)
# We must skip them here so pip download doesn't try to compile from sdist.
SEPARATELY_VENDORED = {"dbt-extractor", "duckdb"}

def _normalize(name: str) -> str:
    """PEP 503 name normalization: lowercase, replace _ . - with single -."""
    import re as _re
    return _re.sub(r"[-_.]+", "-", name).lower()


# Packages that Pyodide already bundles — we DON'T mirror these,
# they're available in the default Pyodide package set.
# Stored here as raw names; we compare via _normalize() at lookup time
# so PEP 503 spelling differences (Jinja2 vs jinja2, pydantic_core vs
# pydantic-core, PyYAML vs pyyaml) all match correctly.
_PYODIDE_BUNDLED_RAW = {
    "Jinja2",
    "MarkupSafe",
    "click",
    "jsonschema",
    "jsonschema-specifications",
    "msgpack",
    "networkx",
    "packaging",
    "protobuf",
    "pydantic",
    "pydantic_core",
    "PyYAML",
    "python-dateutil",
    "pytz",
    "referencing",
    "requests",
    "rpds-py",
    "more-itertools",
    "typing-extensions",
    "urllib3",
    "charset-normalizer",
    "certifi",
    "idna",
    "six",
    "attrs",
    "annotated-types",
}
PYODIDE_BUNDLED = {_normalize(n) for n in _PYODIDE_BUNDLED_RAW}

# Packages that we KEEP in the index even if Pyodide bundles them, for
# version pinning safety. Stored normalized.
PINNED_OVERRIDES: set[str] = set()


def is_bundled(name: str) -> bool:
    """Return True if the package is part of Pyodide's bundled set."""
    n = _normalize(name)
    return n in PYODIDE_BUNDLED and n not in PINNED_OVERRIDES


def sha256(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(65536), b""):
            h.update(chunk)
    return h.hexdigest()


def resolve_closure(out_dir: Path) -> list[dict]:
    """Run `uv pip compile` to lock the dbt closure."""
    requirements_in = out_dir / "_requirements.in"
    requirements_in.write_text("\n".join(REQUIREMENTS) + "\n")
    requirements_lock = out_dir / "_requirements.lock"
    cmd = [
        "uv",
        "pip",
        "compile",
        "--quiet",
        "--python-version",
        "3.13",
        "--output-file",
        str(requirements_lock),
        str(requirements_in),
    ]
    subprocess.run(cmd, check=True)
    pinned = []
    for line in requirements_lock.read_text().splitlines():
        line = line.strip()
        if not line or line.startswith("#") or "==" not in line:
            continue
        name, _, ver = line.partition("==")
        name = name.split("[")[0].strip()
        ver = ver.split(";")[0].strip()
        pinned.append({"name": name, "version": ver})
    return pinned


def download_wheel(name: str, version: str, dest: Path) -> Path | None:
    """Download a pure-Python wheel via pip download. Returns the wheel path or None."""
    with tempfile.TemporaryDirectory() as tmp:
        cmd = [
            sys.executable,
            "-m",
            "pip",
            "download",
            "--no-deps",
            "--only-binary=:all:",
            "--platform",
            "any",
            "--python-version",
            "3.13",
            "--implementation",
            "py",
            "--abi",
            "none",
            "--dest",
            tmp,
            f"{name}=={version}",
        ]
        try:
            subprocess.run(cmd, check=True, capture_output=True)
        except subprocess.CalledProcessError:
            # No pure-Python wheel available; try sdist + build.
            return build_from_sdist(name, version, dest)
        for f in os.listdir(tmp):
            if f.endswith(".whl"):
                src = Path(tmp) / f
                final = dest / f
                shutil.copy2(src, final)
                return final
    return None


def build_from_sdist(name: str, version: str, dest: Path) -> Path | None:
    """Download an sdist and run `pip wheel` against it."""
    with tempfile.TemporaryDirectory() as tmp:
        cmd = [
            sys.executable,
            "-m",
            "pip",
            "download",
            "--no-deps",
            "--no-binary=:all:",
            "--dest",
            tmp,
            f"{name}=={version}",
        ]
        try:
            subprocess.run(cmd, check=True, capture_output=True)
        except subprocess.CalledProcessError as e:
            print(f"  ERROR: cannot fetch sdist for {name}=={version}: {e}")
            return None
        sdist = next((Path(tmp) / f for f in os.listdir(tmp) if f.endswith((".tar.gz", ".zip"))), None)
        if sdist is None:
            return None
        cmd = [
            sys.executable,
            "-m",
            "pip",
            "wheel",
            "--no-deps",
            "--wheel-dir",
            tmp,
            str(sdist),
        ]
        try:
            subprocess.run(cmd, check=True, capture_output=True)
        except subprocess.CalledProcessError as e:
            print(f"  ERROR: cannot wheel {name}=={version}: {e}")
            return None
        for f in os.listdir(tmp):
            if f.endswith(".whl"):
                src = Path(tmp) / f
                final = dest / f
                shutil.copy2(src, final)
                return final
    return None


def write_index_entry(name: str, version: str, wheel_path: Path, index_dir: Path) -> dict:
    """Write a warehouse-JSON-shaped index entry for one package."""
    sha = sha256(wheel_path)
    size = wheel_path.stat().st_size
    entry = {
        "info": {
            "name": name,
            "version": version,
        },
        "releases": {
            version: [
                {
                    "filename": wheel_path.name,
                    "url": f"emfs:/wheels/{wheel_path.name}",
                    "digests": {"sha256": sha},
                    "size": size,
                    "packagetype": "bdist_wheel",
                    "yanked": False,
                }
            ]
        },
        "urls": [
            {
                "filename": wheel_path.name,
                "url": f"emfs:/wheels/{wheel_path.name}",
                "digests": {"sha256": sha},
                "size": size,
                "packagetype": "bdist_wheel",
                "yanked": False,
            }
        ],
    }
    index_dir.mkdir(parents=True, exist_ok=True)
    safe_name = name.lower().replace("_", "-")
    (index_dir / f"{safe_name}.json").write_text(json.dumps(entry, indent=2))
    return {"sha256": sha, "size": size, "filename": wheel_path.name}


def write_lockfile(
    lockfile: Path,
    wheels: list[dict],
    pyodide_abi: str,
    python_tag: str,
    install_order: list[str],
) -> None:
    payload = {
        "generatedAt": datetime.now(tz=timezone.utc).isoformat(),
        "pyodideAbi": pyodide_abi,
        "pythonTag": python_tag,
        "wheels": wheels,
        "installOrder": install_order,
    }
    lockfile.write_text(json.dumps(payload, indent=2) + "\n")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--out", required=True, type=Path)
    parser.add_argument("--index", required=True, type=Path)
    parser.add_argument("--lockfile", required=True, type=Path)
    parser.add_argument("--python-tag", default="cp313")
    parser.add_argument("--abi-tag", default="pyodide_2025_0_wasm32")
    parser.add_argument(
        "--finalize-lockfile",
        action="store_true",
        help="Skip resolution; just write a lockfile from existing wheels.",
    )
    args = parser.parse_args()

    args.out.mkdir(parents=True, exist_ok=True)

    if args.finalize_lockfile:
        # Walk the wheels dir, infer name/version from each filename, build lockfile.
        wheels = []
        order = []
        for whl in sorted(args.out.glob("*.whl")):
            stem = whl.name[:-4]
            parts = stem.split("-")
            name = parts[0].replace("_", "-")
            version = parts[1]
            kind = "native" if "wasm32" in stem else "pure"
            wheels.append({
                "name": name,
                "version": version,
                "filename": whl.name,
                "sha256": sha256(whl),
                "size": whl.stat().st_size,
                "kind": kind,
            })
            order.append(name)
        write_lockfile(args.lockfile, wheels, args.abi_tag, args.python_tag, order)
        print(f"=== Lockfile written: {args.lockfile} ({len(wheels)} wheels) ===")
        return 0

    print("=== Resolving dbt closure with uv pip compile ===")
    pinned = resolve_closure(args.out)
    print(f"  resolved {len(pinned)} packages")

    separately_vendored_normalized = {_normalize(n) for n in SEPARATELY_VENDORED}

    wheels = []
    for entry in pinned:
        name = entry["name"]
        version = entry["version"]
        # Skip Pyodide-bundled packages unless we explicitly need to override.
        # Uses PEP 503 name normalization so spelling differences match.
        if is_bundled(name):
            print(f"  SKIP {name}=={version} (bundled in Pyodide)")
            continue
        # Skip packages that have their own build pipeline (Rust crate /
        # native binding wheel). Those land in wheels/ via separate scripts.
        if _normalize(name) in separately_vendored_normalized:
            print(
                f"  SKIP {name}=={version} (separately vendored — "
                f"see build_extractor.sh / fetch_duckdb.sh)"
            )
            continue
        print(f"  FETCH {name}=={version}")
        wheel = download_wheel(name, version, args.out)
        if wheel is None:
            print(f"  ERROR: could not produce wheel for {name}=={version}")
            return 1
        meta = write_index_entry(name, version, wheel, args.index)
        wheels.append({
            "name": name,
            "version": version,
            "filename": meta["filename"],
            "sha256": meta["sha256"],
            "size": meta["size"],
            "kind": "pure",
        })

    install_order = [w["name"] for w in wheels]
    write_lockfile(args.lockfile, wheels, args.abi_tag, args.python_tag, install_order)
    print(f"=== Pure-py index complete: {len(wheels)} wheels, lockfile {args.lockfile} ===")
    return 0


if __name__ == "__main__":
    sys.exit(main())
