# SPDX-FileCopyrightText: 2026 Zhengyi Zhang
# SPDX-License-Identifier: GPL-3.0-only

"""Protect the required Linux context when parallel quality jobs do not succeed."""

from __future__ import annotations

import os
from pathlib import Path
import re
import subprocess
import unittest


WORKFLOW = Path(__file__).resolve().parents[1] / ".github/workflows/ci.yml"


class LinuxQualityGateTests(unittest.TestCase):
    """Exercise the actual workflow gate instead of a copied result predicate."""

    def setUp(self) -> None:
        workflow = WORKFLOW.read_text(encoding="utf-8")
        block = re.search(
            r"^  rust-linux-gate:\n(.*?)(?=^  [\w-]+:|\Z)",
            workflow,
            re.MULTILINE | re.DOTALL,
        )
        self.assertIsNotNone(block, "the required Linux aggregate must exist")
        self.gate = block.group(1)

    def test_cancelled_dependencies_still_evaluate_the_required_gate(self) -> None:
        # GitHub treats skipped required jobs as passing. !cancelled() would
        # skip this gate before its shell could reject a cancelled matrix.
        self.assertRegex(self.gate, r"(?m)^    if: \$\{\{ always\(\) \}\}$")
        self.assertRegex(self.gate, r"(?m)^    needs: rust-linux$")

    def test_only_a_successful_matrix_passes_the_actual_shell_gate(self) -> None:
        self.assertRegex(
            self.gate,
            r"(?m)^          RESULT: \$\{\{ needs\.rust-linux\.result \}\}$",
        )
        command = re.search(r"(?m)^        run: (.+)$", self.gate)
        self.assertIsNotNone(command, "the aggregate must check its result")
        for result in ("success", "failure", "cancelled", "skipped", ""):
            with self.subTest(result=result):
                completed = subprocess.run(
                    ["bash", "-c", command.group(1)],
                    env=os.environ | {"RESULT": result},
                    capture_output=True,
                    check=False,
                )
                self.assertEqual(completed.returncode == 0, result == "success")


if __name__ == "__main__":
    unittest.main()
