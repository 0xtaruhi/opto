# SPDX-FileCopyrightText: 2026 Zhengyi Zhang
# SPDX-License-Identifier: GPL-3.0-only

"""Verify SPDX headers on first-party source, scripts, configuration, and docs."""

import subprocess
import sys
from pathlib import Path
from typing import List


ROOT = Path(__file__).resolve().parents[1]
COPYRIGHT = "SPDX-FileCopyrightText: 2026 Zhengyi Zhang"
LICENSE_ID = "SPDX-License-Identifier: GPL-3.0-only"
HEADER_SUFFIXES = {
    ".c",
    ".cpp",
    ".h",
    ".inc",
    ".md",
    ".py",
    ".rs",
    ".sh",
    ".sv",
    ".tcl",
    ".toml",
    ".v",
    ".yaml",
    ".yml",
}
HEADER_NAMES = {".gitignore", ".gitmodules", "CMakeLists.txt"}


def repository_files() -> List[Path]:
    result = subprocess.run(
        [
            "git",
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
        ],
        cwd=ROOT,
        check=True,
        stdout=subprocess.PIPE,
    )
    return [ROOT / name.decode() for name in result.stdout.split(b"\0") if name]


def requires_header(path: Path) -> bool:
    relative = path.relative_to(ROOT)
    if not path.is_file():
        return False
    if relative == Path("LICENSE") or path.name == "Cargo.lock":
        return False
    if relative.parts[:1] == ("third_party",):
        return relative == Path("third_party/README.md")
    if relative.parts[:4] == ("crates", "opto-synth", "data", "rewrite"):
        return False
    return path.suffix in HEADER_SUFFIXES or path.name in HEADER_NAMES


def main() -> int:
    missing: List[str] = []
    for path in repository_files():
        if not requires_header(path):
            continue
        prefix = "\n".join(path.read_text(encoding="utf-8").splitlines()[:5])
        if COPYRIGHT not in prefix or LICENSE_ID not in prefix:
            missing.append(path.relative_to(ROOT).as_posix())

    if missing:
        print("Missing or incomplete SPDX header:", file=sys.stderr)
        for name in missing:
            print(f"  {name}", file=sys.stderr)
        return 1

    print("All first-party files have the required SPDX header.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
