<!-- SPDX-FileCopyrightText: 2026 Zhengyi Zhang -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# Benchmarks

Benchmarks measure quality and resources; they do not replace correctness
tests. Tiny smoke circuits live under `qualification/cases`, while this directory keeps
representative kernels and blocks whose area, timing, cell composition,
runtime, CPU time, peak RSS and equivalence status are meaningful. A suite may
omit timing only when its scenario is explicitly named `area_unconstrained`;
such a result must never be presented as timing QoR.

## Runtime regression

QoR suites gate resources with per-case wall, CPU and peak-RSS ceilings, but a
ceiling only fails after a large regression has already landed. Criterion
benchmarks in `crates/opto-synth/benches/synthesis.rs` measure the public
`SynthesisEngine::synthesize` path directly, so normalization, resource planning,
cut enumeration, Boolean rewriting, technology mapping and post-map
optimization are all covered by proportional measurements.

```sh
cargo bench -p opto-synth --bench synthesis
```

Every push builds the benchmarks so they cannot bit-rot. The nightly
`Synthesis runtime regression` job measures the previous commit and the current
one on the same runner and fails when any case regresses by more than 25%.
Case sizes are kept near 200 ms; a larger design would lengthen the comparison
without making it more sensitive, because the gate compares ratios.

Public, reproducible QoR infrastructure is under [`qor/`](qor/README.md).
Non-redistributable inputs and results, private PDKs, and license configuration
are never stored in this repository. Published measurements use checksum-pinned
public inputs and retain enough metadata to be reproduced independently.

## Regional scale contract

The accepted architecture adds a tiered public scale corpus at roughly one
hundred thousand, one million and ten million mapped gates. It must cover
control-dense, arithmetic, high-fanout, deep-pipeline, memory and multi-clock
designs. These are target qualification classes, not a claim that the current
full-root mapper has passed them.

Each published run records the same host image, thread count, RTL, Tcl, SDC,
Liberty, scenarios, and interconnect inputs. End-to-end time includes read,
elaboration, technology-independent optimization, mapping, and post-map
closure. Commit-to-commit gates cover throughput, area, achieved frequency or
critical delay, DRC status, slack, peak RSS, and bytes per mapped gate. The
million- and ten-million-gate tiers must demonstrate bounded memory growth;
absolute targets are versioned with the public suite rather than defined
relative to a closed-source product.
