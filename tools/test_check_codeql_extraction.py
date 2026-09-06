# SPDX-FileCopyrightText: 2026 Zhengyi Zhang
# SPDX-License-Identifier: GPL-3.0-only

"""Regression tests for incomplete CodeQL scans hidden by a successful action."""

from __future__ import annotations

import unittest

from check_codeql_extraction import extraction_count, require_complete_extraction


def summary(count: object) -> dict:
    """Match the scalar BQRS JSON emitted by the Rust summary queries."""

    return {"#select": {"columns": [{"kind": "Integer"}], "tuples": [[count]]}}


class CodeqlExtractionTests(unittest.TestCase):
    """Require usable coverage and reject malformed or partial metrics."""

    def test_accepts_a_complete_nonempty_scan(self) -> None:
        self.assertEqual(require_complete_extraction(summary(12), summary(0)), 12)

    def test_rejects_cached_generated_source_extraction_failures(self) -> None:
        # Restored dependency build outputs previously produced eight failed
        # files while the CodeQL action itself still returned success.
        with self.assertRaisesRegex(ValueError, "errors or warnings in 8 files"):
            require_complete_extraction(summary(12), summary(8))

    def test_rejects_an_empty_scan(self) -> None:
        with self.assertRaisesRegex(ValueError, "any source files"):
            require_complete_extraction(summary(0), summary(0))

    def test_rejects_missing_or_multiple_result_rows(self) -> None:
        for result in (
            None,
            {},
            {"#select": {"tuples": []}},
            {"#select": {"tuples": [[0], [1]]}},
            {"#select": {"tuples": [[0, 1]]}},
        ):
            with self.subTest(result=result), self.assertRaises(ValueError):
                extraction_count(result)

    def test_rejects_noninteger_or_negative_counts(self) -> None:
        for value in (True, False, -1, 0.0, "0", None, {}):
            with self.subTest(value=value), self.assertRaises(ValueError):
                extraction_count(summary(value))


if __name__ == "__main__":
    unittest.main()
