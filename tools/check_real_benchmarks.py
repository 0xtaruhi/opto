#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Zhengyi Zhang
# SPDX-License-Identifier: GPL-3.0-only

"""Validate the pinned real-world benchmark manifest and optional sources."""

import argparse
import re
import tomllib
from pathlib import Path, PurePosixPath


ID = re.compile(r"^[a-z0-9][a-z0-9-]*$")
TOP = re.compile(r"^[A-Za-z_][A-Za-z0-9_$]*$")
SHA256 = re.compile(r"^[0-9a-f]{64}$")


def require(condition, message):
    if not condition:
        raise ValueError(message)


def safe_relative_path(value, field):
    require(isinstance(value, str) and value, f"{field} must be a non-empty string")
    path = PurePosixPath(value)
    require(not path.is_absolute() and ".." not in path.parts, f"unsafe {field}: {value}")
    return path


def validate(manifest_path, source_root):
    document = tomllib.loads(manifest_path.read_text(encoding="utf-8"))
    require(document.get("format") == 1, "unsupported benchmark manifest format")
    suite_name = document.get("name")
    require(suite_name in {"real-medium-30", "real-medium-gate"}, "unexpected benchmark suite name")
    guard = document.get("guard")
    if suite_name == "real-medium-gate":
        require(document.get("threads") == 8, "the qualification contract requires 8 threads")
        require(document.get("maximum_parallel_cases", 0) > 0,
                "gate has no case-level parallelism")
        require(isinstance(guard, dict), "gate manifest has no regression guard")
        require(guard.get("minimum_cases", 0) >= 12, "gate requires fewer than 12 cases")
        require(guard.get("minimum_timing_cases", 0) >= 3, "gate requires fewer than 3 timing cases")
        require(guard.get("minimum_baseline_cells", 0) >= 256,
                "gate admits circuits below 256 mapped cells")
        require(SHA256.fullmatch(document.get("library_sha256", "")),
                "gate has no pinned Liberty SHA-256")
        for metric in ("area", "delay"):
            aggregate = guard.get(f"maximum_{metric}_geomean_ratio")
            per_case = guard.get(f"maximum_{metric}_case_ratio")
            require(isinstance(per_case, (int, float)), f"invalid {metric} per-case guard")
            require(isinstance(aggregate, (int, float)) and 1.0 <= aggregate <= per_case,
                    f"invalid {metric} aggregate guard")
    else:
        require(guard is None, "the 30-case coverage pool must not carry regression policy")
        for field in ("threads", "maximum_parallel_cases", "repetitions", "warmups"):
            require(field not in document, f"coverage pool carries execution field {field}")

    sources = document.get("sources")
    require(isinstance(sources, list) and sources, "manifest has no sources")
    source_ids = set()
    for source in sources:
        source_id = source.get("id")
        require(isinstance(source_id, str) and ID.fullmatch(source_id), f"invalid source id: {source_id!r}")
        require(source_id not in source_ids, f"duplicate source id: {source_id}")
        source_ids.add(source_id)
        require(source.get("url", "").startswith("https://"), f"source {source_id} must use HTTPS")
        require(SHA256.fullmatch(source.get("sha256", "")), f"source {source_id} has no pinned SHA-256")
        for field in ("revision", "license", "citation"):
            require(isinstance(source.get(field), str) and source[field], f"source {source_id} has no {field}")

    cases = document.get("cases")
    minimum_cases = 30 if suite_name == "real-medium-30" else guard["minimum_cases"]
    require(isinstance(cases, list) and len(cases) >= minimum_cases,
            "suite contains fewer cases than required")
    case_ids = set()
    categories = set()
    scenarios = set()
    for case in cases:
        case_id = case.get("id")
        require(isinstance(case_id, str) and ID.fullmatch(case_id), f"invalid case id: {case_id!r}")
        require(case_id not in case_ids, f"duplicate case id: {case_id}")
        case_ids.add(case_id)
        source_id = case.get("source")
        require(source_id in source_ids, f"case {case_id} uses unknown source {source_id!r}")
        require(TOP.fullmatch(case.get("top", "")), f"case {case_id} has an invalid top module")
        category = case.get("category")
        require(category in {"arithmetic", "control", "datapath", "sequential"}, f"case {case_id} has an invalid category")
        categories.add(category)
        scenario = case.get("scenario")
        require(scenario in {"area_unconstrained", "timing_constrained"}, f"case {case_id} has an invalid scenario")
        scenarios.add(scenario)
        rtl = case.get("rtl")
        require(isinstance(rtl, list) and rtl, f"case {case_id} has no RTL inputs")
        paths = [safe_relative_path(value, f"RTL path for {case_id}") for value in rtl]
        for value in case.get("include_dirs", []):
            safe_relative_path(value, f"include path for {case_id}")
        defines = case.get("defines", [])
        require(isinstance(defines, list) and all(isinstance(value, str) and value for value in defines),
                f"case {case_id} has invalid defines")
        if scenario == "timing_constrained":
            require(TOP.fullmatch(case.get("clock_port", "")), f"case {case_id} has no valid clock port")
            require(isinstance(case.get("clock_period"), (int, float)) and case["clock_period"] > 0, f"case {case_id} has no positive clock period")
        else:
            require("clock_port" not in case and "clock_period" not in case, f"unconstrained case {case_id} declares a clock")

        if source_root is not None:
            source_text = []
            for path in paths:
                rtl_path = source_root / source_id / path
                require(rtl_path.is_file(), f"missing RTL input: {rtl_path}")
                source_text.append(rtl_path.read_text(encoding="utf-8", errors="replace"))
            module = re.compile(rf"\bmodule\s+{re.escape(case['top'])}\b")
            require(module.search("\n".join(source_text)), f"top {case['top']} is absent from the RTL inputs for {case_id}")

    required_categories = ({"arithmetic", "control", "datapath", "sequential"}
                           if suite_name == "real-medium-30"
                           else {"arithmetic", "control", "datapath"})
    require(categories == required_categories, "suite does not cover all required structural categories")
    require(scenarios == {"area_unconstrained", "timing_constrained"}, "suite must cover constrained and unconstrained scenarios")
    if suite_name == "real-medium-gate":
        require(sum(case["scenario"] == "timing_constrained" for case in cases) >= guard["minimum_timing_cases"],
                "suite contains fewer timing cases than its guard requires")
        require(any(case["source"] == "epfl" for case in cases), "gate has no EPFL case")
        require(any(case["source"] == "iwls2005" for case in cases), "gate has no IWLS case")
    else:
        require(sum(case["source"] == "epfl" for case in cases) >= 19, "suite must contain at least 19 EPFL cases")
        require(sum(case["source"] == "iwls2005" for case in cases) >= 11, "suite must contain at least 11 IWLS cases")
    print(f"validated {len(cases)} cases from {len(sources)} pinned public sources")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("manifest", type=Path)
    parser.add_argument("--sources", type=Path)
    args = parser.parse_args()
    validate(args.manifest, args.sources)


if __name__ == "__main__":
    main()
