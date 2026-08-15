# SPDX-FileCopyrightText: 2026 Zhengyi Zhang
# SPDX-License-Identifier: GPL-3.0-only

"""Enforce workspace ownership, dependency direction, and shared invariants."""

import json
import re
import subprocess
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]

ALLOWED_DEPENDENCIES = {
    "opto": {
        "opto-command-macros",
        "opto-core",
        "opto-formats",
        "opto-session",
        "opto-tcl-sys",
    },
    "opto-archive": set(),
    "opto-command-macros": set(),
    "opto-core": set(),
    "opto-db": {"opto-archive", "opto-core"},
    "opto-formal": {"opto-ir"},
    "opto-formats": {"opto-ir", "opto-power", "opto-timing"},
    "opto-hdl": {"opto-ir", "opto-runtime", "opto-slang-sys"},
    "opto-ir": {"opto-core"},
    "opto-library": {"opto-archive", "opto-core"},
    "opto-power": {"opto-core", "opto-library", "opto-runtime", "opto-timing"},
    "opto-runtime": {"opto-core"},
    "opto-session": {
        "opto-archive",
        "opto-core",
        "opto-db",
        "opto-formats",
        "opto-hdl",
        "opto-ir",
        "opto-library",
        "opto-power",
        "opto-runtime",
        "opto-synth",
        "opto-timing",
    },
    "opto-slang-sys": set(),
    "opto-synth": {
        "opto-archive",
        "opto-core",
        "opto-formal",
        "opto-ir",
        "opto-library",
        "opto-runtime",
        "opto-timing",
    },
    "opto-tcl-sys": set(),
    "opto-timing": {
        "opto-archive",
        "opto-core",
        "opto-db",
        "opto-ir",
        "opto-library",
        "opto-runtime",
    },
}

# There must be exactly one definition of each cross-cutting MMMC decision, or
# the four analysis sites drift apart again.
# The regional "winner" concept must not exist in code. Banning the word in prose
# also flagged the sentences that say the concept is absent, and the unrelated
# SDC exception-priority winner, so the ban applies to identifiers instead.
FORBIDDEN_SYNTHESIS_IDENTIFIERS = (
    (
        re.compile(r"(?m)^\s*(pub(\([a-z]+\))?\s+)?(fn|struct|enum|const|type)\s+\w*[Ww]inner"),
        "declares a regional winner; a region has one construction vector",
    ),
)

SINGLE_DEFINITION_SYMBOLS = [
    ("fn library_has_timing_arcs", "which MMMC views are analyzable"),
    ("fn path_timing_lane", "how a path value projects onto a check's lanes"),
    ("fn validated_dynamic_power", "what an evaluator may return as power"),
    ("fn worst_arrival", "how arrival reduces within one corner"),
]


def check_runtime_invariants(errors) -> None:
    removed_partition = ROOT / "crates/opto-ir/src/mapped/partition.rs"
    if removed_partition.exists():
        errors.append(
            "crates/opto-ir/src/mapped/partition.rs reintroduces the dead mapped "
            "partition shell; post-map scheduling is not an ownership index"
        )

    for path in sorted((ROOT / "crates/opto-synth/src").rglob("*.rs")):
        contents = path.read_text(encoding="utf8")
        if "MappedPartitionIndex" in contents:
            errors.append(
                f"{path.relative_to(ROOT).as_posix()} reintroduces MappedPartitionIndex; "
                "mapped ownership belongs to ImplementationDb"
            )
        for pattern, message in FORBIDDEN_SYNTHESIS_IDENTIFIERS:
            if pattern.search(contents):
                errors.append(f"{path.relative_to(ROOT).as_posix()} {message}")

    sources = [
        (path, path.read_text(encoding="utf8"))
        for path in sorted((ROOT / "crates").rglob("*.rs"))
    ]
    for symbol, why in SINGLE_DEFINITION_SYMBOLS:
        definition = re.compile(
            r"(?m)^\s*(pub(\([a-z]+\))?\s+)?(const\s+)?" + re.escape(symbol)
        )
        owners = [
            path.relative_to(ROOT).as_posix()
            for path, contents in sources
            if symbol in contents and definition.search(contents)
        ]
        if len(owners) != 1:
            errors.append(
                f"{symbol!r} must have exactly one definition ({why}), found {owners}"
            )


def fail(messages):
    for message in messages:
        print(f"architecture error: {message}", file=sys.stderr)
    raise SystemExit(1)


def main() -> None:
    result = subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--no-deps"],
        cwd=ROOT,
        check=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        universal_newlines=True,
    )
    metadata = json.loads(result.stdout)
    members = set(metadata["workspace_members"])
    packages = {
        package["name"]: package
        for package in metadata["packages"]
        if package["id"] in members
    }
    expected = set(ALLOWED_DEPENDENCIES)
    errors = []
    if set(packages) != expected:
        errors.append(
            "workspace packages differ: "
            f"missing={sorted(expected - set(packages))}, "
            f"unexpected={sorted(set(packages) - expected)}"
        )

    for name, package in sorted(packages.items()):
        workspace_dependencies = {
            dependency["name"]
            for dependency in package["dependencies"]
            if dependency["name"] in expected and dependency["kind"] != "dev"
        }
        unexpected = workspace_dependencies - ALLOWED_DEPENDENCIES.get(name, set())
        if unexpected:
            errors.append(f"{name} has forbidden workspace dependencies {sorted(unexpected)}")

    binaries = sorted(
        (package["name"], target["name"])
        for package in packages.values()
        for target in package["targets"]
        if "bin" in target["kind"]
    )
    if binaries != [("opto", "opto")]:
        errors.append(f"expected only the opto executable, found {binaries}")

    application = ROOT / "crates" / "opto"
    for source in sorted((ROOT / "crates").glob("*/src/**/*.rs")):
        relative = source.relative_to(ROOT).as_posix()
        is_application = application in source.parents
        contents = source.read_text(encoding="utf-8")
        is_test_source = source.name == "tests.rs" or "tests" in source.parts
        if not is_application and (
            "std::env::var(" in contents or "std::env::var_os(" in contents
        ):
            errors.append(
                f"{relative} reads process environment below the application boundary"
            )
        if "#[deprecated" in contents:
            errors.append(f"{relative} introduces a deprecated compatibility surface")

        if not is_test_source:
            if re.search(
                r"#\[\s*(?:allow|expect)\s*\([^]]*clippy::too_many_arguments",
                contents,
                re.DOTALL,
            ):
                errors.append(
                    f"{relative} suppresses too_many_arguments in production code"
                )
            if re.search(
                r"BTreeMap\s*<\s*String\s*,\s*(?:opto_timing::|opto_db::|crate::)?PortId",
                contents,
            ):
                errors.append(
                    f"{relative} uses string-keyed port identity instead of dense PortBindings"
                )
            if (
                relative != "crates/opto-core/src/rows.rs"
                and re.search(r"offsets:\s*Box<\[(?:u32|usize)\]>", contents)
            ):
                errors.append(
                    f"{relative} duplicates packed-row storage instead of using opto-core"
                )
    check_runtime_invariants(errors)

    if errors:
        fail(errors)
    print("Workspace architecture policy passed.")


if __name__ == "__main__":
    main()
