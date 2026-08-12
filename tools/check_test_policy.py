# SPDX-FileCopyrightText: 2026 Zhengyi Zhang
# SPDX-License-Identifier: GPL-3.0-only

"""Enforce structural parts of Opto's test ownership and execution policy."""

from __future__ import annotations

import re
import sys
import tomllib
from pathlib import Path
from typing import Iterable

from check_license_headers import ROOT, repository_files


SELF = Path("tools/check_test_policy.py")
CLI_TARGET = Path("crates/opto/tests/cli/main.rs")
QUALIFICATION_TARGET = Path("crates/opto/tests/qualification/main.rs")
REMOVED_INTEGRATION_ROOT = Path("crates/opto/tests/integration")
IGNORE_ATTRIBUTE = re.compile(r'^\s*#\[ignore(?:\s*=\s*"([^"]+)")?\]\s*$')
FUNCTION = re.compile(r"^\s*fn\s+([A-Za-z_][A-Za-z0-9_]*)\s*\(")
BARE_SHOULD_PANIC = re.compile(r"(?m)^\s*#\[should_panic\]\s*$")
RUST_TEST = re.compile(r"(?m)^\s*#\[(?:[A-Za-z0-9_]+::)?test\]\s*$")
TEST_COMMAND = "--test " + "integration"
OLD_TEST_PATH = "tests/" + "integration"
OWNER_MANIFESTS = (
    Path("crates/opto-timing/test-owners.toml"),
    Path("crates/opto-synth/test-owners.toml"),
)


def relative(path: Path) -> str:
    """Return a repository-relative POSIX path."""

    return path.relative_to(ROOT).as_posix()


def text_files() -> Iterable[Path]:
    """Yield first-party text files relevant to test-policy references."""

    suffixes = {".md", ".py", ".rs", ".toml", ".yaml", ".yml"}
    for path in repository_files():
        if not path.is_file() or path.suffix not in suffixes:
            continue
        if path.relative_to(ROOT).parts[:1] == ("third_party",):
            continue
        yield path


def ignored_tests(path: Path, errors: list[str]) -> list[str]:
    """Validate ignored-test reasons and return their function names."""

    lines = path.read_text(encoding="utf-8").splitlines()
    names = []
    for index, line in enumerate(lines):
        match = IGNORE_ATTRIBUTE.match(line)
        if match is None:
            continue
        reason = match.group(1)
        location = f"{relative(path)}:{index + 1}"
        if reason is None or len(reason.strip()) < 8:
            errors.append(f"{location}: ignored test requires a specific reason")
        for candidate in lines[index + 1 : index + 8]:
            function = FUNCTION.match(candidate)
            if function is not None:
                names.append(function.group(1))
                break
            if candidate.strip() and not candidate.lstrip().startswith("#["):
                break
        else:
            errors.append(f"{location}: ignored attribute is not attached to a test function")
    return names


def check_ignored_tests(paths: list[Path], errors: list[str]) -> None:
    """Require every ignored test to have a reason and a scheduled CI owner."""

    workflows = "\n".join(
        path.read_text(encoding="utf-8")
        for path in sorted((ROOT / ".github/workflows").glob("*.yml"))
    )
    for path in paths:
        if path.suffix != ".rs":
            continue
        contents = path.read_text(encoding="utf-8")
        if BARE_SHOULD_PANIC.search(contents):
            errors.append(
                f"{relative(path)}: #[should_panic] requires an expected message; "
                "prefer a structured error assertion"
            )
        for name in ignored_tests(path, errors):
            names_function = re.search(rf"\b{re.escape(name)}\b", workflows) is not None
            runs_complete_target = (
                path.name != "main.rs"
                and f"--test {path.stem}" in workflows
                and "--ignored" in workflows
            )
            if not names_function and not runs_complete_target:
                errors.append(
                    f"{relative(path)}: ignored test {name!r} has no checked-in workflow owner"
                )


def load_toml(path: Path, errors: list[str]) -> dict:
    """Load one TOML document while retaining a policy-quality diagnostic."""

    try:
        with path.open("rb") as handle:
            return tomllib.load(handle)
    except (OSError, tomllib.TOMLDecodeError) as error:
        errors.append(f"{relative(path)}: cannot read test manifest: {error}")
        return {}


def check_case_inventory(errors: list[str]) -> None:
    """Verify case identity, suite references, and reviewed corpus membership."""

    corpus_roots = (
        ROOT / "qualification/cases",
        ROOT / "qualification/upstream",
        ROOT / "benchmarks/qor/cases",
    )
    cases = sorted(
        path for corpus in corpus_roots for path in corpus.rglob("case.toml")
    )
    by_id: dict[str, str] = {}
    known_paths = {relative(path) for path in cases}
    for path in cases:
        document = load_toml(path, errors)
        case_id = document.get("id")
        if not isinstance(case_id, str) or not case_id.strip():
            errors.append(f"{relative(path)}: case manifest requires a non-empty string id")
            continue
        previous = by_id.get(case_id)
        if previous is not None:
            errors.append(
                f"duplicate case id {case_id!r}: {previous} and {relative(path)}"
            )
        by_id[case_id] = relative(path)
        if path.is_relative_to(ROOT / "qualification/cases"):
            covers = document.get("covers")
            if (
                not isinstance(covers, list)
                or not covers
                or any(not isinstance(item, str) or len(item.strip()) < 16 for item in covers)
            ):
                errors.append(
                    f"{relative(path)}: static case requires one or more specific covers entries"
                )

    suite_roots = (
        ROOT / "qualification/suites",
        ROOT / "benchmarks/qor/suites",
    )
    referenced: set[str] = set()
    for suite in sorted(path for root in suite_roots for path in root.glob("*.toml")):
        document = load_toml(suite, errors)
        entries = document.get("cases")
        if not isinstance(entries, list):
            errors.append(f"{relative(suite)}: suite requires a cases array")
            continue
        for entry in entries:
            if not isinstance(entry, str):
                errors.append(f"{relative(suite)}: suite case paths must be strings")
                continue
            normalized = Path(entry).as_posix()
            if normalized not in known_paths:
                errors.append(
                    f"{relative(suite)}: references unknown case manifest {normalized}"
                )
            referenced.add(normalized)

    for missing in sorted(known_paths - referenced):
        errors.append(f"{missing}: case is not owned by a checked-in suite")


def owner_matches(source: Path, owner_root: Path) -> bool:
    """Return whether one source is covered by an exact file or directory owner."""

    return source == owner_root or (
        owner_root.is_dir() and source.is_relative_to(owner_root)
    )


def check_rust_test_owners(errors: list[str]) -> None:
    """Require broad Rust suites to retain exact, non-overlapping domain owners."""

    for manifest_relative in OWNER_MANIFESTS:
        manifest = ROOT / manifest_relative
        document = load_toml(manifest, errors)
        if document.get("format") != 1:
            errors.append(f"{manifest_relative}: unsupported or missing format")
        expected_total = document.get("expected_tests")
        owners = document.get("owners")
        if not isinstance(expected_total, int) or expected_total < 1:
            errors.append(f"{manifest_relative}: expected_tests must be a positive integer")
            continue
        if not isinstance(owners, list) or not owners:
            errors.append(f"{manifest_relative}: owners must be a non-empty array")
            continue

        crate_root = manifest.parent
        source_counts: dict[Path, int] = {}
        for source in sorted((crate_root / "src").rglob("*.rs")):
            count = len(RUST_TEST.findall(source.read_text(encoding="utf-8")))
            if count:
                source_counts[source] = count

        parsed_owners: list[tuple[Path, str, int]] = []
        declared_total = 0
        for index, owner in enumerate(owners):
            location = f"{manifest_relative}:owners[{index}]"
            if not isinstance(owner, dict):
                errors.append(f"{location}: owner entry must be a table")
                continue
            owner_path = owner.get("path")
            owner_name = owner.get("owner")
            contract = owner.get("contract")
            declared = owner.get("tests")
            if not isinstance(owner_path, str) or not owner_path:
                errors.append(f"{location}: path must be a non-empty string")
                continue
            if not isinstance(owner_name, str) or not owner_name.strip():
                errors.append(f"{location}: owner must be a non-empty string")
            if not isinstance(contract, str) or len(contract.strip()) < 16:
                errors.append(f"{location}: contract must describe the owned behavior")
            if not isinstance(declared, int) or declared < 1:
                errors.append(f"{location}: tests must be a positive integer")
                continue
            resolved = crate_root / owner_path
            if not resolved.exists():
                errors.append(f"{location}: owner path does not exist: {owner_path}")
            parsed_owners.append((resolved, owner_name, declared))
            declared_total += declared

        actual_total = sum(source_counts.values())
        if actual_total != expected_total:
            errors.append(
                f"{manifest_relative}: expected {expected_total} tests but found {actual_total}"
            )
        if declared_total != expected_total:
            errors.append(
                f"{manifest_relative}: owner rows declare {declared_total} tests, "
                f"expected {expected_total}"
            )

        actual_by_owner = [0] * len(parsed_owners)
        for source, count in source_counts.items():
            matches = [
                index
                for index, (owner_root, _, _) in enumerate(parsed_owners)
                if owner_matches(source, owner_root)
            ]
            if len(matches) != 1:
                errors.append(
                    f"{relative(source)}: expected exactly one test owner, found {len(matches)}"
                )
                continue
            actual_by_owner[matches[0]] += count

        for (owner_root, owner_name, declared), actual in zip(
            parsed_owners, actual_by_owner, strict=True
        ):
            if actual != declared:
                errors.append(
                    f"{relative(owner_root)}: owner {owner_name!r} declares {declared} "
                    f"tests but owns {actual}"
                )

        focused = document.get("focused_contracts", [])
        if not isinstance(focused, list):
            errors.append(f"{manifest_relative}: focused_contracts must be an array")
            continue
        for index, contract_entry in enumerate(focused):
            location = f"{manifest_relative}:focused_contracts[{index}]"
            if not isinstance(contract_entry, dict):
                errors.append(f"{location}: focused contract must be a table")
                continue
            contract_path = contract_entry.get("path")
            owner_name = contract_entry.get("owner")
            contract = contract_entry.get("contract")
            declared = contract_entry.get("tests")
            if not isinstance(contract_path, str) or not contract_path:
                errors.append(f"{location}: path must be a non-empty string")
                continue
            if not isinstance(owner_name, str) or not owner_name.strip():
                errors.append(f"{location}: owner must be a non-empty string")
            if not isinstance(contract, str) or len(contract.strip()) < 16:
                errors.append(f"{location}: contract must describe the owned behavior")
            if not isinstance(declared, int) or declared < 1:
                errors.append(f"{location}: tests must be a positive integer")
                continue
            source = crate_root / contract_path
            if not source.is_file():
                errors.append(f"{location}: focused contract file does not exist")
                continue
            actual = source_counts.get(source, 0)
            if actual != declared:
                errors.append(
                    f"{relative(source)}: focused owner {owner_name!r} declares "
                    f"{declared} tests but owns {actual}"
                )

    obsolete_synth_root = ROOT / "crates/opto-synth/src/tests.rs"
    if obsolete_synth_root.exists():
        errors.append(
            "crates/opto-synth/src/tests.rs: root-wide synthesis test module is prohibited; "
            "place assertions under the architecture owner"
        )


def check_target_cutover(paths: list[Path], errors: list[str]) -> None:
    """Keep product CLI and qualification in independently runnable targets."""

    for required in (CLI_TARGET, QUALIFICATION_TARGET):
        if not (ROOT / required).is_file():
            errors.append(f"missing Cargo integration target {required.as_posix()}")
    if (ROOT / REMOVED_INTEGRATION_ROOT).exists():
        errors.append(
            f"{REMOVED_INTEGRATION_ROOT.as_posix()} reintroduces the coupled integration target"
        )

    for path in paths:
        if path.relative_to(ROOT) == SELF:
            continue
        contents = path.read_text(encoding="utf-8")
        if TEST_COMMAND in contents:
            errors.append(
                f"{relative(path)} still invokes the removed unified integration target"
            )
        if OLD_TEST_PATH in contents:
            errors.append(f"{relative(path)} still references the removed integration layout")


def main() -> int:
    """Run the repository test-policy checks."""

    errors: list[str] = []
    paths = list(text_files())
    check_target_cutover(paths, errors)
    check_ignored_tests(paths, errors)
    check_case_inventory(errors)
    check_rust_test_owners(errors)
    if errors:
        print("Test policy violations:", file=sys.stderr)
        for error in errors:
            print(f"  {error}", file=sys.stderr)
        return 1
    print("Test ownership and execution policy passed.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
