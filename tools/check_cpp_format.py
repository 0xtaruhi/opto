# SPDX-FileCopyrightText: 2026 Zhengyi Zhang
# SPDX-License-Identifier: GPL-3.0-only

"""Check or rewrite first-party C++ sources with the pinned clang-format."""

import argparse
import os
import re
import shutil
import subprocess
import sys
from pathlib import Path
ROOT = Path(__file__).resolve().parents[1]
SOURCE_ROOT = Path("crates/opto-slang-sys/native")
SOURCE_SUFFIXES = {".cc", ".cpp", ".cxx", ".h", ".hh", ".hpp", ".hxx"}
CLANG_FORMAT_MAJOR = 18


def cpp_files() -> list[Path]:
    """Return tracked and untracked first-party C++ files in stable order."""
    result = subprocess.run(
        [
            "git",
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
            "--",
            SOURCE_ROOT.as_posix(),
        ],
        cwd=ROOT,
        check=True,
        stdout=subprocess.PIPE,
    )
    files = set()
    for raw_name in result.stdout.split(b"\0"):
        if not raw_name:
            continue
        path = ROOT / raw_name.decode()
        if path.is_file() and path.suffix.lower() in SOURCE_SUFFIXES:
            files.add(path)
    return sorted(files)


def clang_format_binary() -> str:
    """Find the configured formatter and require the repository-pinned major."""
    configured = os.environ.get("CLANG_FORMAT")
    candidates = [configured] if configured else []
    candidates.extend([f"clang-format-{CLANG_FORMAT_MAJOR}", "clang-format"])
    binary = next((shutil.which(candidate) for candidate in candidates if candidate), None)
    if binary is None:
        raise RuntimeError(
            f"clang-format {CLANG_FORMAT_MAJOR} was not found; install clang-format-"
            f"{CLANG_FORMAT_MAJOR} or set CLANG_FORMAT"
        )

    version = subprocess.run(
        [binary, "--version"],
        check=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
    ).stdout.strip()
    match = re.search(r"clang-format version (\d+)", version)
    if match is None or int(match.group(1)) != CLANG_FORMAT_MAJOR:
        raise RuntimeError(
            f"expected clang-format {CLANG_FORMAT_MAJOR}, but {binary} reports: {version}"
        )
    return binary


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--fix",
        action="store_true",
        help="rewrite first-party C++ sources instead of checking them",
    )
    args = parser.parse_args()

    try:
        binary = clang_format_binary()
        files = cpp_files()
    except (OSError, RuntimeError, subprocess.CalledProcessError) as error:
        print(f"C++ formatting setup failed: {error}", file=sys.stderr)
        return 2

    if not files:
        print("No first-party C++ files found.")
        return 0

    command = [binary, "--style=file", "--fallback-style=none"]
    if args.fix:
        command.append("-i")
    else:
        command.extend(["--dry-run", "--Werror"])
    command.extend(str(path) for path in files)

    result = subprocess.run(command, cwd=ROOT, check=False)
    if result.returncode != 0:
        print(
            "First-party C++ formatting is stale; run "
            f"`python3 tools/check_cpp_format.py --fix` with clang-format "
            f"{CLANG_FORMAT_MAJOR}.",
            file=sys.stderr,
        )
        return result.returncode

    action = "Formatted" if args.fix else "Checked"
    print(
        f"{action} {len(files)} first-party C++ files with clang-format "
        f"{CLANG_FORMAT_MAJOR}."
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
