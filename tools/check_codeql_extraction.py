# SPDX-FileCopyrightText: 2026 Zhengyi Zhang
# SPDX-License-Identifier: GPL-3.0-only

"""Require complete Rust extraction from CodeQL's decoded summary query results."""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path


def extraction_count(result: object) -> int:
    """Read a scalar count from `codeql bqrs decode --format=json` output.

    Missing or changed query output must fail closed instead of being interpreted
    as zero errors. These summary queries each return exactly one integer row.
    """

    try:
        rows = result["#select"]["tuples"]
        if len(rows) == 1 and len(rows[0]) == 1:
            count = rows[0][0]
            if type(count) is int and count >= 0:
                return count
    except (KeyError, TypeError):
        pass
    raise ValueError("expected exactly one nonnegative CodeQL extraction count")


def require_complete_extraction(success: object, errors: object) -> int:
    """Reject partial or empty scans even when CodeQL's analyze action succeeds."""

    successful_files = extraction_count(success)
    failed_files = extraction_count(errors)
    if failed_files:
        raise ValueError(
            f"Rust CodeQL extraction reported errors or warnings in {failed_files} files"
        )
    if not successful_files:
        raise ValueError("Rust CodeQL did not successfully extract any source files")
    return successful_files


def main() -> int:
    """Validate the two decoded summary files produced by the analysis job."""

    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("successful_files", type=Path)
    parser.add_argument("failed_files", type=Path)
    args = parser.parse_args()
    try:
        count = require_complete_extraction(
            json.loads(args.successful_files.read_text(encoding="utf-8")),
            json.loads(args.failed_files.read_text(encoding="utf-8")),
        )
    except (OSError, ValueError) as error:
        print(f"CodeQL extraction check failed: {error}", file=sys.stderr)
        return 1
    print(f"Rust CodeQL extracted {count} files without errors or warnings.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
