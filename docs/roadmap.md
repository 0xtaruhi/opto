<!-- SPDX-FileCopyrightText: 2026 Zhengyi Zhang -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# Industrial synthesis product roadmap

This roadmap sequences the evidence and product capabilities required for Opto
to become a production-grade RTL and physically aware synthesis system. It is a
planning document, not a statement that a feature exists. The normative
description of the current tree remains [`architecture.md`](architecture.md),
and only its conformance matrix may label a capability implemented.

The milestones below are ordered by dependency rather than by calendar date.
Work may be explored early, but a milestone is complete only when its exit
evidence is public, reproducible, and accepted. An RFC records an architectural
decision; neither an RFC nor an implementation without qualification completes
a roadmap milestone.

## Product objective

Opto targets commercial-class logical and physically aware synthesis with one
documented Tcl flow, deterministic parallel execution, explicit failure for
unsupported behavior, and standards-based handoff to implementation and
signoff tools.

The first objective is synthesis closure, not immediate ownership of every
signoff algorithm. Advanced variation, signal-integrity, extraction, placement,
routing, manufacturing test, and physical ECO capabilities may be implemented
inside Opto or consumed through explicit data contracts. In either case, Opto
must preserve their intent and must not claim signoff accuracy from a
pre-layout approximation.

Product maturity is measured by all of the following:

- correctness on the documented language and command surface;
- representative power, performance, and area quality;
- predictable runtime and peak memory at production scale;
- correlation with downstream physical implementation and independent timing;
- deterministic results across supported worker counts and hosts;
- actionable diagnostics and explicit unsupported cases;
- durable formats, release provenance, and sustained multi-owner maintenance.

## Non-negotiable boundaries

Roadmap work preserves the existing product boundaries unless an accepted RFC
changes them explicitly:

- one user-facing `opto` executable and one production synthesis path;
- no project manager, manifest-driven alternate flow, or compatibility shell;
- effort controls bounded search policy, not a hidden implementation pipeline;
- domain algorithms do not depend on session or presentation state;
- parallel analysis publishes through deterministic, transactionally validated
  commits;
- unsupported commands, formats, constraints, power intent, and test intent
  fail explicitly;
- another product's command spelling, report text, or internal behavior is not
  the Opto specification.

## Milestone 0: close known correctness and bounded-work gaps

The current architecture must become a stable base before its public surface
or physical scope grows.

Target capabilities:

- include latch-enable dependencies in the timing topology order so every
  supported latch model constructs a valid propagation plan;
- impose a deterministic structural or solver-work bound on each formal proof,
  without making mapped results depend on wall-clock timing;
- complete structured diagnostics for every supported input boundary and
  remove remaining raw invariant failures reachable from user input;
- retain exact rollback, generation, and object-identity checks across failed
  frontend, synthesis, timing, power, and checkpoint operations;
- publish a machine-readable inventory of supported and intentionally rejected
  SystemVerilog, Liberty, SDC, SPEF, and checkpoint behavior.

Exit evidence:

- focused regressions for every closed correctness gap;
- independent equivalence evidence for every affected synthesis transform;
- fuzz targets for every untrusted parser and persistent decoder;
- deterministic failure results for inputs that exceed a declared work bound;
- no known supported input that fails because an internal dependency or
  transaction invariant is incomplete.

## Milestone 1: prove representative scale and QoR

The region-parallel architecture must be demonstrated on designs large enough
to expose memory amplification, boundary traffic, high fanout, reconvergence,
memories, and uneven regional work.

Target capabilities:

- grow the executable public real-design gate beyond the current medium cases;
- qualify approximately one-hundred-thousand-, one-million-, and ten-million-
  gate tiers with pinned, redistributable inputs;
- report wall time, CPU time, peak RSS, bytes per mapped gate, stage costs,
  mapped area, cell composition, WNS, TNS, and failure diagnostics;
- exercise control-heavy, arithmetic, pipelined, high-fanout, memory, and
  hierarchy-heavy designs;
- preserve incremental and worker-count-independent behavior at every tier.

Exit evidence:

- versioned scale manifests and resource ceilings;
- complete results for every declared case, including failures;
- byte-identical netlists and reports at the declared worker counts;
- equivalence for every publishable mapped result;
- no aggregate QoR improvement that hides a severe per-case regression;
- an explicit accounting of checkpoint, frontend, timing, and synthesis peak
  memory rather than only end-to-end RSS.

This milestone qualifies Opto against its accepted public baseline. Comparative
commercial measurements may be recorded outside the repository when license or
data terms require it, but public readiness claims still require reproducible
public inputs and normalized metrics.

## Milestone 2: expose complete multi-scenario synthesis

Opto's internal sparse scenario model becomes a public multi-mode,
multi-corner synthesis contract.

Target capabilities:

- Tcl commands to create, inspect, activate, and remove modes, corners, and
  scenarios;
- per-scenario early and late Liberty data, constraints, parasitics, operating
  conditions, and analysis policies;
- concurrent optimization against all active scenarios rather than independent
  single-scenario netlists;
- deterministic aggregation of WNS, TNS, design-rule violations, power, and
  the scenario responsible for each limiting result;
- sparse invalidation and shared computation without mixing scenario
  generations;
- standards-based exchange for independent timing comparison.

Exit evidence:

- a single-scenario flow remains behaviorally compatible with the existing
  contract;
- generated matrices cover conflicting setup, hold, power, and design-rule
  objectives;
- every accepted edit is checked in every scenario it can affect;
- reports identify the limiting mode, process/voltage/temperature corner, RC
  corner, and analysis view;
- independent STA evaluates the same netlist, SDC, Liberty, and parasitics
  through a normalized comparison schema.

Advanced on-chip variation follows the public scenario contract. Table-driven
AOCV is the first target; LVF-backed statistical propagation and POCV require a
separate RFC, library validation, correlation data, and bounded-memory design.
No variation mode is called signoff-capable solely because its input syntax is
accepted.

## Milestone 3: add physically aware synthesis

Logical choices must be evaluated against a real floorplan and a reproducible
interconnect estimate instead of only topology and wire-load abstractions.

Target capabilities:

- typed physical identities for die/core geometry, rows, sites, layers, vias,
  macro and port locations, blockages, regions, and cell placement;
- reviewed LEF/DEF or equivalently standard physical-data import and export;
- placement- and layer-aware wire RC estimation with explicit provenance;
- congestion and routability estimates that cannot silently become timing
  truth;
- physically aware buffering, cloning, sizing, multi-bit mapping, partitioning,
  and high-fanout repair;
- incremental exchange of placement, routing estimates, clock latency, and
  parasitics with downstream implementation tools;
- one canonical mapped netlist and ownership database across logical and
  physical feedback.

Exit evidence:

- pre-implementation critical paths, wirelength, congestion, and timing are
  compared with a pinned downstream implementation flow;
- correlation targets and tail limits are versioned by process/library class;
- accepted transforms obey placement legality, utilization, dont-touch, power-
  domain, and routing-capacity constraints;
- a physically aware result never scores better by dropping an unmodeled
  physical cost;
- physical feedback updates only the affected timing, power, ownership, and
  regional records.

This milestone does not require Opto to become a complete place-and-route
system. It requires a durable physical contract and measured downstream
correlation.

## Milestone 4: implement low-power intent

Power analysis expands from switching estimates to explicit multi-voltage and
power-state semantics.

Target capabilities:

- a reviewed IEEE 1801 UPF subset covering power domains, supply sets, power
  states, isolation, level shifting, retention, always-on logic, and power
  switches;
- automatic insertion and technology mapping of supported low-power cells;
- power-domain-aware optimization, buffering, clock gating, timing, physical
  ownership, checkpointing, and reporting;
- explicit rejection of every unsupported UPF construct;
- power-aware formal or independently checked equivalence.

Exit evidence:

- generated and curated multi-domain cases cover every supported crossing and
  power-state transition;
- isolation clamp, level-shifter direction, retention save/restore, and
  always-on reachability are proved rather than inferred from cell counts;
- no transform moves, duplicates, or removes logic across a power boundary
  without preserving the power contract;
- downstream implementation and low-power verification consume the emitted
  netlist and power intent without private repair scripts.

## Milestone 5: produce test-ready netlists

The initial objective is correct scan-ready synthesis and interoperable test
intent, not an immediate replacement for every ATPG and diagnosis product.

Target capabilities:

- DFT design-rule checks and typed test clocks, modes, resets, and exclusions;
- scan-capable register selection and replacement;
- deterministic scan-chain planning and stitching across clock and power
  domains;
- clock-gating test bypass and test-mode timing constraints;
- scan-aware physical guidance and incremental updates;
- standard handoff models for external ATPG and fault grading.

Later RFCs may add compression, on-chip clock control, memory BIST/BISR, logic
BIST, boundary scan, ATPG, and diagnosis. Each addition requires its own fault
model, coverage metric, physical cost, and independent oracle.

Exit evidence:

- functional equivalence in normal mode and exact scan-shift behavior in test
  mode;
- deterministic chain membership and ordering;
- test coverage and untestable-fault reports from an independent ATPG flow;
- setup, hold, congestion, power-domain, and clock-domain checks after scan
  insertion;
- explicit reports for excluded or unsupported state elements.

## Milestone 6: close timing and functional ECO loops

Stable mapped identity and transactional edits become a supported engineering-
change workflow.

Target capabilities:

- public mapped-object selection, inspection, and controlled edit commands;
- typed dont-touch, size-only, cell-footprint, threshold-voltage, and spare-cell
  constraints;
- setup and hold repair using propagated clock and physical feedback;
- sizing, threshold-voltage swap, pin swap, buffering, cloning, and delay-cell
  insertion with bounded iteration;
- functional difference-cone discovery and targeted resynthesis;
- deterministic patch netlists or scripts for downstream implementation;
- pre-mask and, if separately approved, metal-only ECO constraints;
- equivalence, timing, physical legality, and rollback checks for every patch.

Exit evidence:

- setup repair does not introduce unapproved hold or design-rule violations,
  and hold repair does not introduce unapproved setup violations;
- a localized RTL change preserves unaffected mapped identities and physical
  ownership;
- independent equivalence verifies the intended functional delta and unchanged
  logic outside it;
- ECO runtime and changed-object count are compared with full reimplementation;
- failed or rejected ECOs leave the published generation unchanged.

## Long-horizon signoff depth

Full signal-integrity, statistical timing, waveform propagation, extraction,
and noise signoff are distinct product commitments. They are not prerequisites
for production-grade synthesis when Opto can exchange complete intent and
correlate with an independent signoff flow.

Owning those domains inside Opto would require separate RFCs and qualification
for at least:

- CCS/ECSM waveform propagation over distributed RC;
- coupling-aware aggressor/victim timing windows, crosstalk delay, and glitch
  propagation;
- LVF moment propagation, path-based analysis, and POCV;
- layout-dependent effects, wire/via variation, and advanced noise models;
- distributed multi-scenario capacity and signoff-correlated ECO guidance.

Until that evidence exists, Opto reports its timing and power model precisely
and describes independent signoff as a required downstream step.

## Cross-cutting release gates

Every milestone also advances the product-wide release contract:

| Gate | Required evidence |
| --- | --- |
| Correctness | Independent equivalence or another domain-appropriate oracle, plus the narrowest regression |
| Determinism | Exact output identity across declared worker counts and stable ordering on every supported host |
| Capacity | Versioned runtime, peak-RSS, working-set, and persistent-size limits on representative inputs |
| QoR | Area, timing, power, cell composition, and per-case tail limits under identical inputs |
| Interoperability | Round trips or independent consumption of every public exchange format |
| Diagnostics | Stable codes, source context, remediation, and explicit unsupported behavior |
| Security | Fuzzing and resource bounds for untrusted inputs; dependency, license, and native-boundary review |
| Release | Reproducible toolchain record, checksums, provenance, compatibility notes, and supported-version policy |
| Maintenance | Named owners for each public domain and at least two reviewers able to maintain every release-critical boundary |

## Immediate planning sequence

The next roadmap work should be planning and evidence, not parallel feature
expansion:

1. close the latch dependency-ordering and per-proof bounded-work gaps;
2. define and populate the hundred-thousand- and million-gate public suites;
3. write the public scenario and result-aggregation RFC;
4. define the independent STA and downstream physical-correlation schema;
5. write the physical database and feedback RFC before selecting new format or
   geometry dependencies;
6. assign maintainers and qualification owners for timing, physical synthesis,
   low power, test, and ECO domains before those surfaces become public.

The conformance matrix moves only after a milestone's implementation and exit
evidence land. This roadmap should be revised when qualification invalidates an
assumption, not preserved as a promise after the architecture changes.
