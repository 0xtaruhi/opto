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
  cargo test -p opto --test integration qualification::presubmit_qor \
  -- --exact --ignored --nocapture
```

`extended.toml` retains the slower self-contained multi-operand sum,
multiply-accumulate, four-term dot-product, and scaled address-generation
gates. They run in weekly CI rather than on every commit:

```sh
OPTO_YOSYS=/path/to/yosys \
  cargo test -p opto --test integration qualification::extended_qor \
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
  cargo test -p opto --test integration qualification::public_qor \
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

Reference runs performed in a separate licensed environment should export
only the normalized fields in `schema/reference-result.schema.json`. The
schema records tool version, input hashes, scenario, area/cells, resources and
optional complete timing triplets. Scripts, libraries, logs and raw reports
from that environment stay outside the public checkout.

## Target regional qualification

The region-parallel architecture is qualified separately from the current
public small-block gate. Its external corpus is tiered at approximately one
hundred thousand, one million and ten million mapped gates and includes
control, arithmetic, reconvergence, high fanout, pipelines, first-class
memories and explicit sparse MMMC scenarios.

For the short-term cutover gate, same-host and same-thread end-to-end
geometric-mean throughput must be at least equal to Genus; no qualifying
million-gate-or-larger case may fall below 0.8 times its throughput. Area and
achieved frequency or critical delay must remain within five percent, with no
new DRC or negative slack absent from the reference. Peak RSS must be no higher
than Genus and bytes per mapped gate must not worsen at the ten-million-gate
tier. Ten-times geometric-mean throughput under the same limits is the
long-term target.

The public schema may carry normalized aggregate measurements from that
external environment. It never carries licensed scripts, raw logs, private
libraries, PDK data or tool configuration.
