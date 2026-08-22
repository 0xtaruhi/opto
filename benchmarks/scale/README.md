<!-- SPDX-FileCopyrightText: 2026 Zhengyi Zhang -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# Tiered scale corpus

This corpus exists to answer one question: does the RFC 0013 ownerless work
graph scale? It supplies the design that
[RFC 0013](../../docs/rfcs/0013-ownerless-structural-epochs.md) Phase 3 requires
— "at least one million logical operations" after sealing — and the sweep that
measures the phase's five acceptance gates.

It implements the tiered corpus promised by the `Regional scale contract` in
[`../README.md`](../README.md).

## Why this corpus is generated

Every other qualification input in this repository is fetched from a
checksum-pinned public source. This one is generated, for three reasons.

**No public design reaches the target.** A million post-sealing word-level
operations is roughly an order of magnitude past CVA6. Reaching it from public
RTL would mean assembling a full SoC build with its own dependency manager,
which is a fragile input for a gate that must be reproducible years from now.

**Replication would corrupt the measurement.** Instantiating one core N times
also reaches any size, but it produces homogeneous, independent work. That is
the best case for a scheduler: it would inflate speedup and utilization and
would prove nothing about the work graph. The generated tiles are heterogeneous
and consume each other through a ring, so the graph has real depth and real
contention.

**Generation is exactly reproducible.** The same tier always emits
byte-identical SystemVerilog. `scale.toml` pins a SHA-256 per file, and both the
checker and the runner verify those pins before a measurement counts.

## Shape

`scale_top` is a ring of tiles over a shared broadcast bus. Tile *N* consumes
tile *N-1*; the broadcast beat is derived from the ring tail, so it is a genuine
shared dependency rather than a free-running input.

Tile classes rotate around the ring, covering the six classes the scale contract
names:

| Tile | Class | What it stresses |
| --- | --- | --- |
| `scale_arith_tile` | arithmetic | wide multiply-accumulate lanes joined by a reduction tree |
| `scale_control_tile` | control-dense | a wide distinct-armed opcode decode feeding a sparse FSM |
| `scale_fanout_tile` | high-fanout | one control net observed by many independent sinks |
| `scale_pipeline_tile` | deep-pipeline | a long register chain with differing per-stage logic |
| `scale_memory_tile` | memory | an inferred synchronous-read register file with write priority |
| `scale_cdc_tile` | multi-clock | a second clock domain joined by two-flop synchronizers |

Tiles are instantiated from a `generate` loop, so the source stays around 30 KB
at every tier while the elaborated design grows with the parameters. Each tier's
source is small enough to review; only the parameters change.

| Tier | Target operations after sealing | Gates Phase 3 |
| --- | ---: | --- |
| `small` | 100,000 | no |
| `medium` | 1,000,000 | **yes** |
| `large` | 10,000,000 | no |

The neighbouring tiers exist so a scaling claim can be read as a trend instead
of a single point.

## Calibration is required before the gate means anything

`target_normalized_operations` is an intent. `measured_normalized_operations` is
the record of what a real run produced, and it starts at `0`.

Normalization folds constants and shares common subexpressions, so the sealed
operation count is not a function of the source alone and cannot be predicted
from the generator parameters. **The tier parameters in `generate.py` are an
estimate until a measured run confirms them.**

Both the checker and the runner refuse to let an uncalibrated tier gate Phase 3:

```
benchmarks/scale/scale.toml: scale-medium: gates Phase 3 but is uncalibrated
(measured_normalized_operations is 0); run the calibration first
```

To calibrate, synthesize the tier once and read `normalized_operations` from the
`Sealed design:` line Opto prints when the artifact completes, then record it in
`scale.toml`. If the measured count falls short of one million, raise the tier's
`tiles` parameter in `generate.py`, re-pin the hashes, and measure again.

## Running

Validate the manifest, and confirm the pinned hashes still describe what the
generator emits:

```sh
python3 tools/check_scale_benchmarks.py benchmarks/scale/scale.toml --regenerate
```

Emit a tier by hand:

```sh
benchmarks/scale/generate.py --tier medium /var/tmp/opto-scale-medium --print-hashes
```

Run the sweep. It generates the RTL itself, verifies the hashes, and synthesizes
the gating tier once per worker count:

```sh
OPTO_LIBRARY_SCALE=/var/tmp/sky130-hd.lib \
OPTO_REGRESSION_OUTPUT=/var/tmp/opto-scale \
  cargo test -p opto --test qualification \
    scale_phase_three_scaling \
    -- --exact --ignored --nocapture
```

This is a long run: it synthesizes a million-operation design once per worker
count, and the one-worker point is by construction the slowest measurement in
the suite. Use a `--release` build; development-profile measurements must never
be published as runtime results.

## Gates

The thresholds live in `[guard]` in `scale.toml` so a policy change is reviewed
as data rather than hidden in the runner. `tools/check_scale_benchmarks.py`
rejects a manifest that weakens any of them, because RFC 0013 permits revision
only through an amendment backed by checked benchmark evidence.

| Guard | RFC 0013 Phase 3 requirement |
| --- | --- |
| `minimum_speedup_at_sixteen_workers` | at least 6x end-to-end speedup at sixteen workers over one |
| `minimum_average_worker_utilization` | at least 70% average worker utilization |
| `maximum_coordinator_fraction` | coordinator, partition-publication and commit below 15% of wall time |
| `minimum_ready_tasks_per_worker` | at least eight ready fine tasks per worker |
| `maximum_peak_memory_ratio` | peak resident memory no more than 1.5x the one-worker path |

Two measurement decisions are worth stating, because they are the runner's
interpretation rather than the RFC's words:

* **Coordinator fraction** is the residual: wall time not accounted for by
  measured scheduler batches. It cannot undercount the serial surface, and it
  charges any unmeasured stage against the gate rather than excusing it.
* **Ready-task depth** is not enforced at one worker. The RFC exempts a graph
  that genuinely exposes less parallelism, and the single-worker point cannot
  demonstrate the difference.

The runner also asserts that the sealed operation count is identical at every
worker count. If it is not, the sweep is comparing different designs and no
ratio computed from it means anything.

`results.json` is written before any gate is asserted, so a failing run still
leaves the evidence that explains it.
