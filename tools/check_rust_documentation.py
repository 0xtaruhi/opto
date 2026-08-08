# SPDX-FileCopyrightText: 2026 Zhengyi Zhang
# SPDX-License-Identifier: GPL-3.0-only

"""Reject Rustdoc blocks split by item attributes."""

import subprocess
import sys
from pathlib import Path
from typing import List, Optional, Tuple


ROOT = Path(__file__).resolve().parents[1]


def rust_files() -> List[Path]:
    result = subprocess.run(
        [
            "git",
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
            "*.rs",
        ],
        cwd=ROOT,
        check=True,
        stdout=subprocess.PIPE,
    )
    files = []
    for raw_name in result.stdout.split(b"\0"):
        if not raw_name:
            continue
        name = raw_name.decode()
        if name.startswith(("third_party/", "qualification/upstream/", "private/")):
            continue
        path = ROOT / name
        if path.is_file():
            files.append(path)
    return files


def split_item_documentation_line(path: Path) -> Optional[int]:
    """Find a doc block incorrectly split by one or more item attributes."""
    lines = path.read_text(encoding="utf-8").splitlines()
    index = 0
    while index < len(lines):
        if not lines[index].lstrip().startswith("///"):
            index += 1
            continue
        while index < len(lines) and lines[index].lstrip().startswith("///"):
            index += 1

        attribute_start = index
        saw_attribute = False
        while index < len(lines) and lines[index].lstrip().startswith("#["):
            saw_attribute = True
            bracket_depth = 0
            while index < len(lines):
                bracket_depth += lines[index].count("[") - lines[index].count("]")
                index += 1
                if bracket_depth <= 0:
                    break

        if (
            saw_attribute
            and index < len(lines)
            and lines[index].lstrip().startswith("///")
        ):
            return index + 1
        index = max(index, attribute_start + 1)
    return None


def main() -> int:
    invalid: List[Tuple[str, str]] = []
    for path in rust_files():
        relative = path.relative_to(ROOT).as_posix()
        line = split_item_documentation_line(path)
        if line is not None:
            invalid.append(
                (
                    relative,
                    f"line {line} starts a second doc block after item attributes",
                )
            )

    if invalid:
        print("Invalid Rust documentation:", file=sys.stderr)
        for name, reason in invalid:
            print(f"  {name}: {reason}", file=sys.stderr)
        return 1

    print("All first-party Rust files avoid split item documentation.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
