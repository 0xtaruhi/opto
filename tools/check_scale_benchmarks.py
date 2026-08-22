#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Zhengyi Zhang
# SPDX-License-Identifier: GPL-3.0-only

"""Validate the generated scale manifest and, optionally, the emitted sources.

The scale corpus is generated rather than fetched, so the manifest's job is to
prove that a measurement used the intended input: every tier pins a SHA-256 per
emitted file, and `--regenerate` re-runs the generator to confirm those pins
still describe what the generator produces.
"""

import argparse
import hashlib
import re
import subprocess
import sys
import tempfile
import tomllib
from pathlib import Path


ID = re.compile(r"^[a-z0-9][a-z0-9-]*$")
TOP = re.compile(r"^[A-Za-z_][A-Za-z0-9_$]*$")
SHA256 = re.compile(r"^[0-9a-f]{64}$")
SOURCE_NAME = re.compile(r"^[a-z0-9_]+\.sv$")

# Mirrors RFC 0013 "Phase 3: hierarchical local execution". The checker rejects
# a manifest that weakens a gate, because the RFC allows revision only through
# an amendment with checked evidence.
REQUIRED_GUARDS = {
    "minimum_speedup_at_sixteen_workers": 6.0,
    "minimum_average_worker_utilization": 0.70,
    "minimum_ready_tasks_per_worker": 8,
}
MAXIMUM_GUARDS = {
    "maximum_coordinator_fraction": 0.15,
    "maximum_peak_memory_ratio": 1.5,
}


def require(condition, message):
    if not condition:
        raise ValueError(message)


def check_guard(guard):
    for name, floor in REQUIRED_GUARDS.items():
        require(name in guard, f"[guard] is missing '{name}'")
        require(
            guard[name] >= floor,
            f"[guard] {name} is {guard[name]}, weaker than the RFC 0013 floor {floor}",
        )
    for name, ceiling in MAXIMUM_GUARDS.items():
        require(name in guard, f"[guard] is missing '{name}'")
        require(
            guard[name] <= ceiling,
            f"[guard] {name} is {guard[name]}, weaker than the RFC 0013 ceiling {ceiling}",
        )
    workers = guard.get("worker_counts")
    require(isinstance(workers, list) and workers, "[guard] worker_counts must be a non-empty list")
    require(
        workers == sorted(set(workers)),
        "[guard] worker_counts must be strictly increasing without duplicates",
    )
    require(
        workers[0] == 1 and workers[-1] == 16,
        "[guard] worker_counts must span the RFC's one- and sixteen-worker points",
    )


def check_tier(tier, index):
    label = tier.get("id", f"tier #{index}")
    require(ID.match(tier.get("id", "")), f"{label}: id must be lowercase kebab-case")
    require(tier.get("tier") in {"small", "medium", "large"}, f"{label}: unknown tier name")
    target = tier.get("target_normalized_operations")
    require(isinstance(target, int) and target > 0, f"{label}: target_normalized_operations must be a positive integer")
    measured = tier.get("measured_normalized_operations")
    require(isinstance(measured, int) and measured >= 0, f"{label}: measured_normalized_operations must be a non-negative integer")

    gates = tier.get("gates_phase_three")
    require(isinstance(gates, bool), f"{label}: gates_phase_three must be a boolean")
    if gates:
        # This is the tier the phase gate reads, so an uncalibrated or
        # undersized measurement must fail loudly rather than silently
        # qualifying a design that never reached the RFC's threshold.
        require(
            measured > 0,
            f"{label}: gates Phase 3 but is uncalibrated "
            "(measured_normalized_operations is 0); run the calibration first",
        )
        require(
            measured >= 1_000_000,
            f"{label}: gates Phase 3 with only {measured} normalized operations; "
            "RFC 0013 requires at least one million after sealing",
        )

    files = tier.get("files")
    require(isinstance(files, list) and files, f"{label}: files must be a non-empty list")
    seen = set()
    for entry in files:
        name = entry.get("name", "")
        require(SOURCE_NAME.match(name), f"{label}: bad source name '{name}'")
        require(name not in seen, f"{label}: duplicate source '{name}'")
        seen.add(name)
        require(SHA256.match(entry.get("sha256", "")), f"{label}: {name} has a malformed sha256")
    return {entry["name"]: entry["sha256"] for entry in files}


def regenerate(generator, tier_name):
    """Run the generator into a scratch directory and hash what it emits."""
    with tempfile.TemporaryDirectory() as scratch:
        subprocess.run(
            [sys.executable, str(generator), "--tier", tier_name, scratch],
            check=True,
        )
        produced = {}
        for path in sorted(Path(scratch).iterdir()):
            produced[path.name] = hashlib.sha256(path.read_bytes()).hexdigest()
        return produced


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("manifest", type=Path, help="path to scale.toml")
    parser.add_argument(
        "--regenerate",
        action="store_true",
        help="re-run the generator and verify the pinned hashes still match",
    )
    arguments = parser.parse_args(argv)

    document = tomllib.loads(arguments.manifest.read_text(encoding="utf-8"))

    try:
        require(document.get("format") == 1, "format must be 1")
        require(TOP.match(document.get("top", "")), "top must be a valid identifier")
        clocks = document.get("clocks")
        require(isinstance(clocks, list) and len(clocks) >= 2, "at least two clocks must be declared")
        for clock in clocks:
            require(TOP.match(clock.get("port", "")), "clock port must be a valid identifier")
            require(clock.get("period", 0) > 0, "clock period must be positive")

        check_guard(document.get("guard", {}))

        tiers = document.get("tiers")
        require(isinstance(tiers, list) and tiers, "tiers must be a non-empty list")
        require(
            sum(1 for tier in tiers if tier.get("gates_phase_three")) == 1,
            "exactly one tier must carry the Phase 3 gate",
        )

        pinned = {}
        for index, tier in enumerate(tiers):
            pinned[tier["tier"]] = check_tier(tier, index)
    except ValueError as error:
        print(f"{arguments.manifest}: {error}", file=sys.stderr)
        return 1

    if arguments.regenerate:
        generator = arguments.manifest.parent / document["generator"]
        if not generator.is_file():
            print(f"generator not found: {generator}", file=sys.stderr)
            return 1
        for tier_name, expected in pinned.items():
            produced = regenerate(generator, tier_name)
            if produced != expected:
                print(f"tier '{tier_name}': generated sources do not match the manifest", file=sys.stderr)
                for name in sorted(set(produced) | set(expected)):
                    if produced.get(name) != expected.get(name):
                        print(
                            f"  {name}: manifest={expected.get(name, 'absent')} "
                            f"generated={produced.get(name, 'absent')}",
                            file=sys.stderr,
                        )
                return 1

    uncalibrated = [
        tier["id"] for tier in document["tiers"] if tier["measured_normalized_operations"] == 0
    ]
    print(f"{arguments.manifest}: {len(document['tiers'])} tiers validated")
    if uncalibrated:
        # Not a failure: an uncalibrated non-gating tier is a normal
        # intermediate state. The gating tier is already rejected above.
        print(f"  uncalibrated tiers (no measured operation count): {', '.join(uncalibrated)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
