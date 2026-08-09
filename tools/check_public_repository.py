# SPDX-FileCopyrightText: 2026 Zhengyi Zhang
# SPDX-License-Identifier: GPL-3.0-only

"""Reject private, secret, or non-redistributable artifacts from the public tree."""

import re
import sys
from pathlib import Path
from typing import List

from check_license_headers import ROOT, repository_files


FORBIDDEN_PREFIXES = (
    "benchmarks/iscas89/",
    "benchmarks/qor/private/",
    "benchmarks/reference_private/",
    "qualification/reference_private/",
    "tests/reference_private/",
)
FORBIDDEN_PATHS = {
    "docs/big-refactor-execution-plan.md",
}
FORBIDDEN_SUFFIXES = (
    ".7z",
    ".db",
    ".db.gz",
    ".ddc",
    ".ock",
    ".def",
    ".fsdb",
    ".gds",
    ".gds.gz",
    ".gdsii",
    ".itf",
    ".lef",
    ".lef.gz",
    ".lib",
    ".lib.gz",
    ".ndm",
    ".nlib",
    ".oas",
    ".oasis",
    ".p12",
    ".pem",
    ".pfx",
    ".key",
    ".rpt",
    ".sdf",
    ".spf",
    ".spef",
    ".svf",
    ".tar",
    ".tar.gz",
    ".tf",
    ".tgz",
    ".vcd",
    ".wlf",
    ".zip",
)
FORBIDDEN_FILENAMES = {
    ".npmrc",
    ".pypirc",
    "credentials.json",
    "id_dsa",
    "id_ecdsa",
    "id_ed25519",
    "id_rsa",
    "license.dat",
    "service-account.json",
}
FORBIDDEN_TEXT = (
    ("foundry/process marker", "".join(("ts", "mc"))),
    ("private PDK path", "".join(("/data", "/pdk"))),
    ("private workspace path", "".join(("/data", "/eda-work"))),
)
SECRET_PATTERNS = (
    (
        "private key",
        re.compile(rb"-----BEGIN (?:RSA |EC |OPENSSH |DSA |PGP )?PRIVATE KEY-----"),
        True,
    ),
    ("AWS access key", re.compile(rb"\b(?:AKIA|ASIA)[A-Z0-9]{16}\b"), True),
    (
        "GitHub token",
        re.compile(
            rb"\b(?:ghp|gho|ghu|ghs|ghr)_[A-Za-z0-9]{30,}\b|"
            rb"\bgithub_pat_[A-Za-z0-9_]{40,}\b"
        ),
        True,
    ),
    (
        "OpenAI API key",
        re.compile(rb"\bsk-(?:proj-)?[A-Za-z0-9_-]{20,}\b"),
        True,
    ),
    (
        "Slack token",
        re.compile(rb"\bxox[baprs]-[A-Za-z0-9-]{20,}\b"),
        True,
    ),
    ("Google API key", re.compile(rb"\bAIza[0-9A-Za-z_-]{35}\b"), True),
    (
        "credentials embedded in URI",
        re.compile(
            rb"(?i)\b(?:https?|postgres(?:ql)?|mysql|mongodb(?:\+srv)?|redis)://"
            rb"[^\s/:]+:[^\s/@]+@"
        ),
        False,
    ),
)
SELF = Path("tools/check_public_repository.py")
PUBLIC_SYNTHETIC_LIBRARIES = {
    "benchmarks/qor/libraries/cover_test.lib",
    "qualification/libraries/frontend_sequential.lib",
    "qualification/libraries/opto_test.lib",
}


def public_files() -> List[Path]:
    files = []
    for path in repository_files():
        if not path.is_file():
            continue
        files.append(path)
    return files


def path_violations(paths: List[Path]) -> List[str]:
    violations = []
    for path in paths:
        relative_path = path.relative_to(ROOT)
        if relative_path.parts[:1] == ("third_party",):
            continue
        relative = relative_path.as_posix()
        if relative in PUBLIC_SYNTHETIC_LIBRARIES:
            continue
        filename = path.name.casefold()
        if filename == ".env" or (
            filename.startswith(".env.") and filename != ".env.example"
        ):
            violations.append("forbidden environment file: {}".format(relative))
            continue
        if filename in FORBIDDEN_FILENAMES:
            violations.append("forbidden credential file: {}".format(relative))
            continue
        if relative in FORBIDDEN_PATHS or relative.startswith(FORBIDDEN_PREFIXES):
            violations.append("forbidden path: {}".format(relative))
        elif relative.lower().endswith(FORBIDDEN_SUFFIXES):
            violations.append("forbidden artifact type: {}".format(relative))
    return violations


def contains_secret(paths: List[Path]) -> bool:
    for path in paths:
        relative = path.relative_to(ROOT)
        if relative == SELF:
            continue
        data = path.read_bytes()
        for _label, pattern, scan_third_party in SECRET_PATTERNS:
            if not scan_third_party and relative.parts[:1] == ("third_party",):
                continue
            match = pattern.search(data)
            if match is None:
                continue
            return True
    return False


def text_violations(paths: List[Path]) -> List[str]:
    violations = []
    for path in paths:
        relative = path.relative_to(ROOT)
        if relative == SELF:
            continue
        data = path.read_bytes()
        if b"\0" in data:
            continue
        try:
            lines = data.decode("utf-8").splitlines()
        except UnicodeDecodeError:
            continue
        for line_number, line in enumerate(lines, start=1):
            folded = line.casefold()
            for label, needle in FORBIDDEN_TEXT:
                if needle in folded:
                    violations.append(
                        "{}:{}: {}".format(relative.as_posix(), line_number, label)
                    )
    return violations


def main() -> int:
    paths = public_files()
    violations = path_violations(paths) + text_violations(paths)
    secret_found = contains_secret(paths)
    if violations or secret_found:
        print("Public repository policy violations:", file=sys.stderr)
        for violation in violations:
            print("  {}".format(violation), file=sys.stderr)
        if secret_found:
            # Keep all secret-derived data, including its location and count,
            # out of logs that may be visible to untrusted pull requests.
            print("  secret-like credential content detected", file=sys.stderr)
        return 1
    print("Public repository policy passed.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
