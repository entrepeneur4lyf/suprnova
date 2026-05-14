#!/usr/bin/env python3
"""Rebrand: rename `suprnova` → `suprnova` across the workspace.

Order matters — longest patterns first so substrings don't get half-replaced.

Excluded from rename: target/, .git/, reference/ (Laravel sources + preserved
nation-x app docs), node_modules/, scripts/ (this file).

Run from repo root:  python3 scripts/rename_suprnova_to_suprnova.py [--dry-run]
"""
from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

EXCLUDE_DIRS = {
    "target",
    ".git",
    "reference",
    "node_modules",
    "scripts",
    "frontend",  # app frontend
}

INCLUDE_SUFFIXES = {".rs", ".toml", ".md", ".sh", ".yml", ".yaml", ".json"}

# Each substitution is (regex, replacement, description). Order matters.
SUBS: list[tuple[re.Pattern[str], str, str]] = [
    # 1. Special-case the dep-rename in app/Cargo.toml first — strip the
    #    `, package = "suprnova-rs"` because new package name matches the alias.
    (
        re.compile(
            r'suprnova\s*=\s*\{\s*path\s*=\s*"\.\./framework"\s*,\s*package\s*=\s*"suprnova-rs"\s*\}'
        ),
        'suprnova = { path = "../framework" }',
        "app/Cargo.toml dep-rename",
    ),
    # 2. Compound names — longest first so they win over bare `suprnova-rs` etc.
    (re.compile(r"\bsuprnova-macros\b"), "suprnova-macros", "suprnova-macros package"),
    (re.compile(r"\bsuprnova-cli\b"), "suprnova-cli", "suprnova-cli package"),
    (re.compile(r"\bsuprnova-rs\b"), "suprnova", "suprnova-rs package"),
    (re.compile(r"\bsuprnova_macros\b"), "suprnova_macros", "suprnova_macros identifier"),
    (re.compile(r"\bsuprnova_rs\b"), "suprnova", "suprnova_rs identifier"),
    # 3. Code paths and imports.
    (re.compile(r"\bsuprnova::"), "suprnova::", "suprnova:: path"),
    (re.compile(r"\buse suprnova;"), "use suprnova;", "use suprnova;"),
    (re.compile(r"\bextern crate suprnova\b"), "extern crate suprnova", "extern crate suprnova"),
    # 4. The CLI binary name in suprnova-cli/Cargo.toml: literal `name = "suprnova"`.
    (re.compile(r'^\s*name = "suprnova"\s*$', re.MULTILINE), '    name = "suprnova"', "CLI bin name"),
    # 5. Prose: capitalized brand "suprnova" → "Suprnova" (in markdown / doc comments).
    #    Match whole-word only, and not preceded by - or _ (already handled).
    (re.compile(r"(?<![A-Za-z_\-])suprnova(?![A-Za-z_])"), "Suprnova", "suprnova prose"),
    # 6. Lowercase bare `suprnova` as a whole word — rare; mostly in CLI invocations
    #    like `suprnova serve`, `suprnova make:controller`. Match only when surrounded by
    #    word boundaries and NOT preceded by `_` / `-` (those are compound names
    #    already handled). Also exclude `suprnova-` and `suprnova_` left-overs that
    #    somehow survived.
    (
        re.compile(r"(?<![A-Za-z0-9_\-/])suprnova(?![A-Za-z0-9_\-:])"),
        "suprnova",
        "bare suprnova word",
    ),
]


def should_process(path: Path) -> bool:
    rel = path.relative_to(ROOT)
    for part in rel.parts:
        if part in EXCLUDE_DIRS:
            return False
    return path.suffix in INCLUDE_SUFFIXES


def apply_subs(text: str) -> tuple[str, dict[str, int]]:
    counts: dict[str, int] = {}
    for pattern, replacement, label in SUBS:
        new_text, n = pattern.subn(replacement, text)
        if n:
            counts[label] = counts.get(label, 0) + n
            text = new_text
    return text, counts


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--dry-run", action="store_true", help="report only, write nothing")
    args = parser.parse_args()

    file_changes: list[tuple[Path, dict[str, int]]] = []
    total_files = 0

    for path in sorted(ROOT.rglob("*")):
        if not path.is_file():
            continue
        if not should_process(path):
            continue
        total_files += 1
        try:
            text = path.read_text(encoding="utf-8")
        except UnicodeDecodeError:
            continue
        new_text, counts = apply_subs(text)
        if counts:
            file_changes.append((path.relative_to(ROOT), counts))
            if not args.dry_run:
                path.write_text(new_text, encoding="utf-8")

    print(f"scanned {total_files} files")
    print(f"modified {len(file_changes)} files")
    if args.dry_run:
        print("(dry run — no writes)")

    # Per-label totals.
    label_totals: dict[str, int] = {}
    for _, counts in file_changes:
        for label, n in counts.items():
            label_totals[label] = label_totals.get(label, 0) + n
    print("\nsubstitution totals:")
    for label, n in sorted(label_totals.items(), key=lambda x: -x[1]):
        print(f"  {n:5d}  {label}")

    if args.dry_run:
        print("\nfirst 20 files that would change:")
        for path, counts in file_changes[:20]:
            print(f"  {path}: {sum(counts.values())} edits")

    return 0


if __name__ == "__main__":
    sys.exit(main())
