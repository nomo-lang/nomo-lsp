#!/usr/bin/env python3
"""Reject non-canonical embedded Nomo fixtures outside compatibility coverage."""

from __future__ import annotations

import re
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SOURCE_FILES = sorted((ROOT / "src").glob("*.rs"))
LEGACY_MAIN = re.compile(r"\b(?:package|import) [A-Za-z_][A-Za-z0-9_]*\.main\b")
EXPLICIT_VOID_BODY = re.compile(r"(?:suspend\s+)?fn [^\"\n]*\) -> void \{")
EXPLICIT_VOID_SIGNATURE = re.compile(r"fn [^\"\n]*\) -> void\\n")
FORMATTER_COMPATIBILITY_FIXTURE = (
    'let text = "package app\\n\\nfn main() -> void {\\n}\\n";'
)
LEGACY_ROOT_COMPATIBILITY_FIXTURE = (
    'let text = "package app.main\\n\\nfn main() {\\n}\\n";'
)
LEGACY_ROOT_ERROR_COMPATIBILITY_FIXTURE = (
    '"package app.main\\n\\nfn main() {\\n    let value: i32 = '
    '\\"not an integer\\"\\n}\\n";'
)


def main() -> int:
    failures: list[str] = []
    legacy_main_occurrences: list[tuple[Path, str]] = []
    explicit_void_occurrences: list[tuple[Path, str]] = []

    for path in SOURCE_FILES:
        text = path.read_text(encoding="utf-8")
        for match in LEGACY_MAIN.finditer(text):
            legacy_main_occurrences.append((path, match.group(0)))
        for pattern in (EXPLICIT_VOID_BODY, EXPLICIT_VOID_SIGNATURE):
            for match in pattern.finditer(text):
                explicit_void_occurrences.append((path, match.group(0)))

    expected_path = ROOT / "src" / "formatting.rs"
    expected_text = expected_path.read_text(encoding="utf-8")
    if expected_text.count(FORMATTER_COMPATIBILITY_FIXTURE) != 1:
        failures.append(
            "src/formatting.rs: expected exactly one explicit-void compatibility fixture"
        )
    if len(explicit_void_occurrences) != 1 or explicit_void_occurrences[0][0] != expected_path:
        rendered = ", ".join(
            f"{path.relative_to(ROOT)}: {text}"
            for path, text in explicit_void_occurrences
        )
        failures.append(
            "declaration-level explicit void is only allowed in the formatter "
            f"compatibility fixture; found [{rendered}]"
        )

    legacy_path = ROOT / "src" / "backend.rs"
    legacy_text = legacy_path.read_text(encoding="utf-8")
    if legacy_text.count(LEGACY_ROOT_COMPATIBILITY_FIXTURE) != 1:
        failures.append(
            "src/backend.rs: expected exactly one legacy module-root diagnostic fixture"
        )
    if legacy_text.count(LEGACY_ROOT_ERROR_COMPATIBILITY_FIXTURE) != 1:
        failures.append(
            "src/backend.rs: expected exactly one legacy module-root plus error fixture"
        )
    if len(legacy_main_occurrences) != 2 or any(
        path != legacy_path for path, _ in legacy_main_occurrences
    ):
        rendered = ", ".join(
            f"{path.relative_to(ROOT)}: {text}"
            for path, text in legacy_main_occurrences
        )
        failures.append(
            "legacy .main roots are only allowed in the W0904 compatibility fixture; "
            f"found [{rendered}]"
        )

    if failures:
        for failure in failures:
            print(failure)
        return 1

    print("canonical module roots and implicit-void LSP fixtures verified")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
