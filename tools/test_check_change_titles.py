# SPDX-FileCopyrightText: 2026 Zhengyi Zhang
# SPDX-License-Identifier: GPL-3.0-only

"""Tests for the pull-request and commit title policy."""

from __future__ import annotations

import os
import subprocess
import tempfile
import unittest
from pathlib import Path

from check_change_titles import commit_subjects, title_error


class ChangeTitlePolicyTests(unittest.TestCase):
    """Protect the public naming grammar and exact commit-range semantics."""

    def test_accepts_every_documented_prefix(self) -> None:
        for prefix in ("synth", "db", "cli", "docs", "test", "build", "deps", "misc"):
            with self.subTest(prefix=prefix):
                self.assertIsNone(title_error(f"[{prefix}] Describe the change"))

    def test_rejects_missing_unknown_or_empty_prefixes(self) -> None:
        for title in (
            "Describe the change",
            "[frontend] Describe the change",
            "[SYNTH] Describe the change",
            "[synth]",
            "[synth]  Describe the change",
            "[synth][db] Describe the change",
            "[synth] [db] Describe the change",
            "[synth] Describe the change ",
        ):
            with self.subTest(title=title):
                self.assertIsNotNone(title_error(title))

    def test_reads_only_commits_after_the_base(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            repository = Path(directory)
            environment = os.environ | {
                "GIT_AUTHOR_NAME": "Opto Test",
                "GIT_AUTHOR_EMAIL": "opto-test@example.invalid",
                "GIT_COMMITTER_NAME": "Opto Test",
                "GIT_COMMITTER_EMAIL": "opto-test@example.invalid",
            }

            def git(*arguments: str) -> str:
                result = subprocess.run(
                    ["git", *arguments],
                    cwd=repository,
                    env=environment,
                    check=True,
                    capture_output=True,
                    text=True,
                )
                return result.stdout.strip()

            git("init", "--quiet")
            git("commit", "--allow-empty", "--quiet", "-m", "Historical subject")
            base = git("rev-parse", "HEAD")
            git("commit", "--allow-empty", "--quiet", "-m", "[docs] First change")
            git("commit", "--allow-empty", "--quiet", "-m", "[test] Second change")
            head = git("rev-parse", "HEAD")
            previous = Path.cwd()
            try:
                os.chdir(repository)
                subjects = commit_subjects(base, head)
            finally:
                os.chdir(previous)
            self.assertEqual(
                [subject.subject for subject in subjects],
                ["[docs] First change", "[test] Second change"],
            )


if __name__ == "__main__":
    unittest.main()
