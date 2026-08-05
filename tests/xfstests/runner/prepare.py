#!/usr/bin/env python3
"""Validate reviewed xfstests exceptions and generate the exact exclude file."""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import re
import tempfile
import tomllib


DISPOSITIONS = {"excluded", "deferred", "allowed-notrun", "expected-failure", "reduced"}
COMMON_KEYS = {"id", "backend", "disposition", "reason"}
OPTIONAL_KEYS = {
    "tracking_issue",
    "notrun_reason",
    "output_digest",
    "classification",
    "reduction",
    "full_iterations",
    "reduced_iterations",
    "focused_coverage",
}
TEST_ID = re.compile(r"^generic/[0-9]{3}$")
DIGEST = re.compile(r"^sha256:[0-9a-f]{64}$")


def load_exceptions(path: Path) -> list[dict[str, object]]:
    with path.open("rb") as source:
        document = tomllib.load(source)
    if set(document) - {"schema", "exceptions"}:
        raise ValueError(f"unknown top-level keys: {sorted(set(document) - {'schema', 'exceptions'})}")
    if document.get("schema") != 1:
        raise ValueError("exceptions.toml must declare schema = 1")

    records = document.get("exceptions", [])
    if not isinstance(records, list):
        raise ValueError("exceptions must be an array of tables")

    seen: set[tuple[str, str]] = set()
    for index, record in enumerate(records):
        if not isinstance(record, dict):
            raise ValueError(f"exception {index} is not a table")
        missing = COMMON_KEYS - set(record)
        unknown = set(record) - COMMON_KEYS - OPTIONAL_KEYS
        if missing or unknown:
            raise ValueError(f"exception {index}: missing={sorted(missing)} unknown={sorted(unknown)}")

        test_id = record["id"]
        backend = record["backend"]
        disposition = record["disposition"]
        reason = record["reason"]
        if not isinstance(test_id, str) or not TEST_ID.fullmatch(test_id):
            raise ValueError(f"exception {index}: id must be exact generic/NNN")
        if not isinstance(backend, str) or not backend or "*" in backend:
            raise ValueError(f"exception {index}: backend must be exact and non-wildcard")
        if disposition not in DISPOSITIONS:
            raise ValueError(f"exception {index}: invalid disposition {disposition!r}")
        if not isinstance(reason, str) or len(reason.strip()) < 12:
            raise ValueError(f"exception {index}: reason must be concrete")

        key = (test_id, backend)
        if key in seen:
            raise ValueError(f"duplicate exception for {test_id} on {backend}")
        seen.add(key)

        if disposition in {"deferred", "allowed-notrun", "expected-failure", "reduced"}:
            tracking = record.get("tracking_issue")
            if not isinstance(tracking, str) or not tracking.strip():
                raise ValueError(f"exception {index}: {disposition} requires tracking_issue")
        if disposition == "allowed-notrun":
            expected = record.get("notrun_reason")
            if not isinstance(expected, str) or not expected:
                raise ValueError(f"exception {index}: allowed-notrun requires exact notrun_reason")
        if disposition == "expected-failure":
            digest = record.get("output_digest")
            classification = record.get("classification")
            if not isinstance(digest, str) or not DIGEST.fullmatch(digest):
                raise ValueError(f"exception {index}: expected-failure requires sha256 output_digest")
            if not isinstance(classification, str) or not classification:
                raise ValueError(f"exception {index}: expected-failure requires classification")
        if disposition == "reduced":
            reduction = record.get("reduction")
            full_iterations = record.get("full_iterations")
            reduced_iterations = record.get("reduced_iterations")
            focused_coverage = record.get("focused_coverage")
            expected_full_iterations = {
                ("generic/011", "dirstress-files"): 1000,
                ("generic/014", "truncfile-iterations"): 10000,
                ("generic/069", "append-stream-iterations"): 3000000,
                ("generic/129", "looptest-iterations"): 162000,
                ("generic/371", "parallel-enospc-iterations"): 100,
                ("generic/404", "insert-range-blocks"): 500,
                ("generic/471", "rewinddir-files"): 10000,
                ("generic/488", "open-unlink-files"): 10000,
                ("generic/676", "seekdir-files"): 4000,
                ("generic/736", "readdir-renames-files"): 5000,
            }.get((test_id, reduction))
            if expected_full_iterations is None:
                raise ValueError(
                    f"exception {index}: unsupported exact test/reduction pair "
                    f"{test_id!r}/{reduction!r}"
                )
            if (
                not isinstance(full_iterations, int)
                or isinstance(full_iterations, bool)
                or full_iterations != expected_full_iterations
            ):
                raise ValueError(
                    f"exception {index}: reduced {test_id} requires "
                    f"full_iterations={expected_full_iterations}"
                )
            if (
                not isinstance(reduced_iterations, int)
                or isinstance(reduced_iterations, bool)
                or not 1 <= reduced_iterations < full_iterations
            ):
                raise ValueError(
                    f"exception {index}: reduced_iterations must be in 1..full_iterations"
                )
            if not isinstance(focused_coverage, str) or not focused_coverage.strip():
                raise ValueError(f"exception {index}: reduced requires focused_coverage")
        elif any(
            key in record
            for key in ("reduction", "full_iterations", "reduced_iterations", "focused_coverage")
        ):
            raise ValueError(f"exception {index}: reduction fields require disposition='reduced'")
    return records


def rendered_excludes(records: list[dict[str, object]]) -> str:
    excluded = sorted(
        {str(record["id"]) for record in records if record["disposition"] == "excluded"}
    )
    lines = ["# Generated from exceptions.toml by runner/prepare.py. Do not edit."]
    lines.extend(excluded)
    return "\n".join(lines) + "\n"


def write_atomic(path: Path, content: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile("w", dir=path.parent, delete=False, encoding="utf-8") as output:
        output.write(content)
        temp_path = Path(output.name)
    os.replace(temp_path, path)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--exceptions", type=Path, required=True)
    parser.add_argument("--exclude", type=Path, required=True)
    parser.add_argument("--json", type=Path)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()

    content = rendered_excludes(load_exceptions(args.exceptions))
    if args.check:
        current = args.exclude.read_text(encoding="utf-8") if args.exclude.exists() else ""
        if current != content:
            raise SystemExit(f"{args.exclude} is stale; run make -C tests/xfstests stage")
    else:
        write_atomic(args.exclude, content)
    if args.json:
        write_atomic(args.json, json.dumps(load_exceptions(args.exceptions), indent=2) + "\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
