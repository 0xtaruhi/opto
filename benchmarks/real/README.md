<!-- SPDX-FileCopyrightText: 2026 Zhengyi Zhang -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# Real medium-scale qualification

`medium.toml` pins 30 public RTL designs from the EPFL and IWLS 2005 suites. It
is the long-term coverage pool: unsupported and slow designs remain visible
instead of silently disappearing. `gate.toml` is the measured, currently
executable regression subset. Its baseline-size floor is 256 mapped cells, so
the dozens-of-cells FSM smoke tests cannot qualify. A case moves from the pool
into the gate only after it completes under the pinned Liberty and satisfies
that scale floor.

The manifests, rather than mutable upstream branches, are the source of truth.
Fetch and verify them outside the checkout:

```sh
benchmarks/real/fetch.sh /var/tmp/opto-real-medium-30
python3 tools/check_real_benchmarks.py \
  --sources /var/tmp/opto-real-medium-30 benchmarks/real/medium.toml
python3 tools/check_real_benchmarks.py \
  --sources /var/tmp/opto-real-medium-30 benchmarks/real/gate.toml
```

Build the commit under review and its merge base with the same optimized
profile, then run the same-host regression gate:

```sh
benchmarks/qor/libraries/fetch_sky130.sh /var/tmp/sky130-hd.lib
OPTO_SOURCE_REAL_MEDIUM=/var/tmp/opto-real-medium-30 \
OPTO_LIBRARY_REAL_MEDIUM=/var/tmp/sky130-hd.lib \
OPTO_QOR_BASELINE_BINARY=/path/to/base/opto \
OPTO_QOR_BINARY=/path/to/head/opto \
OPTO_REGRESSION_OUTPUT=/var/tmp/opto-regression \
  cargo test -p opto --test integration \
    qualification::real_medium_qor_regression \
    -- --exact --ignored --nocapture
```

Baseline and candidate each run once per case as fresh processes. Independent
cases run concurrently; the runner derives the job count from available CPUs
and the per-case thread budget, capped by `maximum_parallel_cases`. The gate
writes `results.json` before reporting any failure.

The current gate contains 14 distinct real tops spanning arithmetic, control,
datapath, crypto lookup logic, and three clocked designs. With the pinned
Sky130 library, the validated mapped range is 353–10,225 cells; the full pool
retains slower designs up to roughly twenty thousand cells and beyond. These
measurements describe the present implementation, not frozen per-case
baselines—the merge-base binary is always the baseline.

Every comparison uses the same RTL bytes, top, Liberty bytes, constraints,
host and eight-worker limit. End-to-end wall time, CPU time and peak RSS remain
in the result as diagnostics, but are not regression metrics because concurrent
cases contend for host resources. Runtime regression is measured separately by
the repeated Criterion synthesis benchmarks in the nightly workflow.

Combinational cases are deliberately `area_unconstrained`. Clocked OpenCores
cases share a 10 ns constraint and are `timing_constrained`. Area, mapped cell count,
the complete cell histogram, wall/CPU time, peak RSS and failure diagnostics
are mandatory. A timing result is accepted only as a complete tuple of clock
period, critical delay, WNS, TNS and violating-path count.

## Reproducible reference runs

Each accepted Opto baseline exports one normalized
`benchmarks/qor/schema/reference-result.schema.json` document per case. Run the
candidate and baseline binaries from the same fixed host image, and record the
exact Opto commits and binary hashes, library hash, input hashes, worker count,
and machine identity. All published inputs must be redistributable or fetched
from a checksum-pinned public source. Failures are first-class results; a
failed or missing design never disappears from aggregate statistics.

## Regression policy

Thresholds live in `[guard]` in the manifest so policy changes are reviewed as
data, not hidden in the runner. The current commit-to-commit policy is:

- area and critical-delay geometric-mean ratios no worse than `1.00`, while an
  individual case may regress by at most `1.05`;
- no new negative slack, additional violating paths or incomplete timing tuple;
- every gate case present, with matching input and pinned Liberty hashes.

The aggregate-plus-tail policy deliberately permits local trade-offs: one
design may lose while another gains. It rejects both a net corpus regression
and sacrificing one design to improve the average. The executable gate is
expected to grow toward all 30 public cases; reducing its case count or size
floor is a policy change, not a baseline update.
