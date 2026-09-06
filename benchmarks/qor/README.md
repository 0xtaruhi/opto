<!-- SPDX-FileCopyrightText: 2026 Zhengyi Zhang -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# QoR qualification

The public suite compares Opto against Yosys+ABC with identical RTL and
Liberty inputs. Semantic correctness is gated independently by the equivalence
and generated-semantic suites. Cases are classified as `kernel` or `block`;
tiny smoke tests are deliberately excluded.

The current public suite contains wide and invariant arithmetic, ALU/control,
priority, crossbar and sequential pipeline structures. `weekly.toml` may add
public-library cases that are unsuitable for every run. A useful new case must
add a missing structural class or realistic scale, not merely increase the
count.

`presubmit.toml` contains self-contained mapping-mechanism fixtures. It uses a
checked-in synthetic library and covers shared-inverter selection, cheap
shared nodes, reconvergent multi-fanout crossing, a mixed logic cone, and
known-zero arithmetic width reduction. These tiny cases catch local mechanism
changes and run combinational CEC, but they are not representative QoR
evidence. FSM semantics and equivalence live in the sequential qualification
suite without area baselines. The real medium-scale acceptance gate is
documented in `benchmarks/real/README.md`. Run the mapping fixtures locally
with:

```sh
OPTO_YOSYS=/path/to/yosys \
  cargo test -p opto --test qualification presubmit_qor \
  -- --exact --ignored --nocapture
```

The known-zero addition fixture keeps its 30-unit delay requirement and zero
violation tolerance. Path-aware selection and feasible-area recovery reduce
its synthetic-library area from 102.75 to 89.0 with the same 57 cells, using
23 of the 30 available delay units. Its reviewed histogram records the cheaper
carry chain; end-to-end CEC still checks all 32 output bits, including the
proved-zero upper bits. This fixture update does not change real-corpus gates.

`extended.toml` retains the slower self-contained multi-operand sum,
multiply-accumulate, four-term dot-product, and scaled address-generation
gates. They run in weekly CI rather than on every commit:

```sh
OPTO_YOSYS=/path/to/yosys \
  cargo test -p opto --test qualification extended_qor \
  -- --exact --ignored --nocapture
```

Build a named optimized binary, fetch the checksum-pinned public Liberty, and
run the Rust integration test:

```sh
cargo build --profile fast-release --bin opto --locked
benchmarks/qor/libraries/fetch_sky130.sh /tmp/sky130.lib

OPTO_QOR_BINARY="$PWD/target/fast-release/opto" \
OPTO_YOSYS=/path/to/yosys \
OPTO_LIBRARY_SKY130_HD=/tmp/sky130.lib \
OPTO_REGRESSION_OUTPUT=/tmp/opto-regression \
  cargo test -p opto --test qualification public_qor \
  -- --exact --ignored --nocapture
```

Most checked-in cases are explicitly `area_unconstrained`. A
`timing_constrained` case declares a positive `clock_period`, executes the
matching constraints before Opto compilation, and enables `report_timing`.
The current harness records Opto's critical delay, WNS, TNS, and violating-path
count; reference timing fields remain empty unless a common analysis contract
produces them. The harness writes
`results.json`, `summary.tsv`, tool binary hashes, wall/CPU/RSS metrics, cell
histograms, reports, mapped netlists and CEC logs. It does not infer timing QoR
from mapper log text. Comparative slack or critical delay is publishable only
when the same SDC and independent STA engine evaluate every mapped netlist.
Commercial reference reports remain outside the repository and may exchange
only the normalized constrained fields defined by
`reference-result.schema.json`.
Never compare measurements made with different build profiles without saying
so. Performance publication should use `release`; `fast-release` is useful for
developer and CI trend detection and must be labeled accordingly.

Each `case.toml` is the machine-readable source of truth for its accepted
baseline. `expected_area` plus the fractional `area_tolerance` form an upper
bound; a run exceeding it is recorded as a failure. Timing-constrained cases also
declare `expected_worst_slack` and an absolute `worst_slack_tolerance`, forming
a lower bound. Cases with established resource baselines may additionally gate
cell count, exact cell histogram, TNS, violating paths, wall/CPU time, and peak
RSS. Performance limits are machine-independent safety ceilings, not claims
that measurements from different hosts are directly comparable. Improvements
pass without changing the baseline, but an intentional QoR tradeoff must update
the affected case in the same pull request and explain the area, timing, and
cell-mix change. The harness runs every selected case, records per-case failure
diagnostics, always writes `results.json` and `summary.tsv`, and then fails the
test if any gate or equivalence check failed. Checked-in prose tables are not
baselines. One aggregate geometric mean must not hide a severe single-design
regression.

Accepted Opto baselines export only the normalized fields in
`schema/reference-result.schema.json`. The schema records the Opto version,
exact command, Rust toolchain, `release` profile, binary SHA-256, worker count,
host information, input hashes, scenario, area/cells, resources, and optional
complete timing triplets. Published runtime and QoR baselines must use
`--release`; published inputs are redistributable or fetched from a
checksum-pinned public source.

## Target regional qualification

The region-parallel architecture is qualified separately from the current
public small-block gate. Its public corpus is tiered at approximately one
hundred thousand, one million and ten million mapped gates and includes
control, arithmetic, reconvergence, high fanout, pipelines, first-class
memories and explicit sparse MMMC scenarios.

The gate compares a candidate against the last accepted public Opto baseline
on the same host and worker count. Area and achieved frequency or critical
delay may regress by at most five percent per case; no new DRC or negative
slack may appear. Peak RSS and bytes per mapped gate must not regress at the
million- and ten-million-gate tiers. Suite manifests may additionally define
versioned absolute throughput and memory ceilings.

The public schema carries normalized measurements, input hashes, and complete
reproduction metadata. It never carries non-redistributable scripts, raw logs,
private libraries, PDK data, or tool configuration.
