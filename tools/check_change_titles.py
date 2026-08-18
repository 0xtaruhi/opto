# SPDX-FileCopyrightText: 2026 Zhengyi Zhang
# SPDX-License-Identifier: GPL-3.0-only

"""Validate pull-request titles and commit subjects against Opto's taxonomy."""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
from dataclasses import dataclass


PREFIXES = (
    "synth",
    "db",
    "cli",
    "docs",
    "test",
    "build",
    "deps",
    "misc",
)
PREFIX_PATTERN = re.compile(
    rf"^\[(?:{'|'.join(map(re.escape, PREFIXES))})\] (?P<summary>\S(?:.*\S)?)$"
)
OBJECT_ID = re.compile(r"^[0-9a-fA-F]{40}$")


@dataclass(frozen=True)
class CommitSubject:
    """One commit identity and its first-line subject."""

    object_id: str
    subject: str


def title_error(title: str) -> str | None:
    """Return the policy violation for one title, or ``None`` when valid."""

    match = PREFIX_PATTERN.fullmatch(title)
    if match is not None and not any(
        match.group("summary").startswith(f"[{prefix}]") for prefix in PREFIXES
    ):
        return None
    allowed = ", ".join(f"[{prefix}]" for prefix in PREFIXES)
    return f"must match '[prefix] Summary'; allowed prefixes: {allowed}"


def _commit_exists(object_id: str) -> bool:
    """Return whether an exact Git object ID names a locally available commit."""

    if OBJECT_ID.fullmatch(object_id) is None:
        return False
    result = subprocess.run(
        ["git", "cat-file", "-e", f"{object_id}^{{commit}}"],
        check=False,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    return result.returncode == 0


def commit_subjects(base: str, head: str) -> list[CommitSubject]:
    """Return subjects introduced between exact base and head commit IDs."""

    if not _commit_exists(head):
        raise ValueError(f"head commit is unavailable or invalid: {head!r}")
    revision = head
    if _commit_exists(base) and base != head:
        revision = f"{base}..{head}"
    result = subprocess.run(
        ["git", "log", "--format=%H%x00%s", "--reverse", revision],
        check=True,
        capture_output=True,
        text=True,
    )
    subjects = []
    for line in result.stdout.splitlines():
        object_id, separator, subject = line.partition("\0")
        if not separator:
            raise ValueError("git log returned an invalid commit record")
        subjects.append(CommitSubject(object_id=object_id, subject=subject))
    return subjects


def parse_args() -> argparse.Namespace:
    """Parse the policy check invocation."""

    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--pull-request-title",
        default="",
        help="pull-request title; omit outside a pull_request event",
    )
    parser.add_argument("--base", default="", help="exact base commit object ID")
    parser.add_argument("--head", required=True, help="exact head commit object ID")
    return parser.parse_args()


def main() -> int:
    """Validate the requested pull request and commit range."""

    arguments = parse_args()
    errors = []
    if arguments.pull_request_title:
        error = title_error(arguments.pull_request_title)
        if error is not None:
            errors.append(f"pull-request title {error}: {arguments.pull_request_title!r}")
    try:
        subjects = commit_subjects(arguments.base, arguments.head)
    except (OSError, subprocess.CalledProcessError, ValueError) as error:
        errors.append(f"cannot inspect commit subjects: {error}")
        subjects = []
    if not subjects:
        errors.append("commit range contains no commits")
    for commit in subjects:
        error = title_error(commit.subject)
        if error is not None:
            errors.append(
                f"commit {commit.object_id[:12]} subject {error}: {commit.subject!r}"
            )
    if errors:
        print("Change title policy violations:", file=sys.stderr)
        for error in errors:
            print(f"  {error}", file=sys.stderr)
        return 1
    print("Change title policy passed.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
