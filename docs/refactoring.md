<!-- SPDX-FileCopyrightText: 2026 Zhengyi Zhang -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# Region-Parallel Synthesis Cutover Record

This document records the completed architectural cutover. The normative design is
[architecture.md](architecture.md); the rationale and invariants are in
[RFC 0006](rfcs/0006-region-parallel-synthesis.md).

This is a historical implementation record, not the current synthesis front
half. [RFC 0007](rfcs/0007-timing-driven-partitioning.md) replaced the global
Word optimization and canonical target-lowering locality described below with
region-owned structure and region-private mapping. The mapped ownership,
cross-boundary repair, global post-map closure, deterministic publication, and
reporting sections remain
applicable. In particular, RFC 0007 permits bounded pairwise region work and
keeps physical/post-map optimization global; “cross-boundary” does not by
itself mean “forbidden.”

The cutover replaces speculative regional architecture portfolios with one
construction per region and one canonical target-mapping subject. It does not
introduce a second mapper, an EHM, a compatibility path, or runtime memory
admission.

## Corrected Model

The former RFC 0006 front-half model was:

```text
global semantic normalization
  -> stable region graph
  -> one construction vector per region
  -> one canonical lowering
  -> parallel regional cut/cover analysis
  -> direct mapped region artifacts
  -> incremental global feedback on one mapped generation
  -> transactional post-map optimization
```

This is one synthesis path. The public `synth` command traverses it with the
same representations; typed effort settings differ only in bounded search and
convergence policy.

A construction vector answers “which semantic implementation is this region
building?” It is not a mapped candidate. A `RegionCoverPlan` answers “what
exact compact cover did the canonical target subject select for this region?”
There is exactly one active value of each kind per region and epoch.

Initial mapping and post-map optimization share the mapped transaction and
MMMC owner model. Cross-region repair is decomposed by exact driver-to-single-
sink region edges. Each committed edge is owned once by the live implementation
database and post-map reconstructs physical repair topology on every synthesis
run; the regional decision cache does not retain a second portable repair
representation. No multi-region ownership shell remains. The only open
items in [architecture.md](architecture.md)'s “Known Architectural Gaps” are
benchmark qualification results, not representation migration.

There is no synthesis “winner.” The words Top-K, Pareto, candidate winner, and
winner selection no longer describe regional synthesis. The only unrelated
remaining use of “winner” in the documentation is RFC 0005's precisely defined
path-exception arbitration rule.

## Cutover State

| Area | State | Result |
|---|---|---|
| Region identity and SCC-safe partition | Complete | Stable typed regions, packed boundary CSR, deterministic work estimates |
| Construction planning | Complete | One context-keyed regional cache record per region; memory choice is part of its stable decision payload |
| Memory admission removal | Complete | No predicted-allocation rejection or hidden size gate |
| Target lowering ownership | Complete | One canonical selected construction; no target-local Word cone |
| Boundary identity | Complete | Whole protected equivalence classes and ownership rules survive canonicalization |
| Regional mapping | Complete | Canonical regional slices are analyzed and covered against Liberty in keyed parallel tasks |
| Worker allocation | Complete | Deterministic weighted allocation gives inner lanes only to dominating regions |
| Feedback | Complete | Dirty regions reproject exact contracts, reselect from frozen AXM/cut/truth records, and replace only changed owned footprints |
| Mapped commit | Complete | Direct artifacts replace stable footprints transactionally |
| Mapped ownership | Complete | Global, single-region, or exact driver-to-sink boundary-edge atoms with stable reverse footprints |
| Post-map | Complete | Design-rule, fanout, sizing, and MFS changes use measured transactional evaluation |
| Diagnostics | Complete for typed synthesis failures | Source spans, related locations, notes/help, and command invocation render together |
| Timing reporting | Complete for the supported core surface | Real mapped max/min path reporting with typed selectors |
| Large-design qualification | In progress | CVA6 and larger scale/RSS gates remain benchmark work, not architecture work |

## Source Changes

### Procedural Control Normalization

Process lowering now canonicalizes the sealed CFG before emitting Word IR. It
removes unreachable blocks, folds constant branch and switch decisions, threads
empty jumps, merges equal successors, models a common exit, and computes
dominator/post-dominator structure. Per-target sparse state propagation then
extracts typed event-aware reset and enable semantics into one canonical
Predicate DAG. Unrelated side effects no longer create muxes or guards for a
target that passes through the branch unchanged.

### Region Construction

The partitioner now:

- condenses combinational SCCs before packing;
- never splits one SCC across region tasks;
- preserves resource-affinity groups where possible;
- uses deterministic complexity and connectivity weights;
- emits explicit typed boundary ports;
- carries source origins into cycle diagnostics.

The old behavior that diagnosed a partition-unit graph cycle caused by cutting
through a legal dependency component was replaced by SCC-aware construction.
True unsupported combinational cycles still fail explicitly.

### One Regional Decision

Regional architecture selection returns one context-keyed cache record for
every region. The record contains the selected implementation of every owned
first-class memory and an optional compact plan restored only after topology
reconstruction.

Selection uses deterministic target/scenario structural estimates and a
regional depth budget. It does not materialize several regional target
netlists and compare them. Cache records persist this one reconstructible
decision.

`RegionalArchitecturePreparation` publishes the selected construction and
memory decisions into the one Liberty mapping seed. There is no alternate
GTECH plan or empty-library fallback.

### Canonical Target Subject

The target path lowers the selected construction once. Region ownership is
extended through memory lowering and bit lowering. `RegionalLogicPartition`
then derives immutable slices from the canonical subject rather than cloning
Word cones.

The key boundary invariant is implemented by
`optimize_combinational_dataflow_by_preserving_classes`:

1. resolve every value to its terminal canonical representative;
2. identify every equivalence class containing a protected value;
3. reject all rewrites in those classes;
4. reject cross-region and owned/unowned equivalences;
5. close representatives again and apply the checked rewrite.

Protected roots include region-port identities and all mapping observability
roots before and after lowering. This is an IR invariant, not an accidental
side effect of keeping an obsolete local cover alive.

### Regional Cover

Target regions independently perform:

- regional root/input projection;
- cached Boolean rewriting;
- packed cut enumeration and truth computation;
- Liberty cell and pin binding;
- phase, sharing, demand, and multi-output recovery;
- exact sparse boundary response measurement;
- compact portable plan serialization.

`CompiledRegionCover` retains one region's optimized AXM subject, cuts, truth
rows, and stable catalog frontier only through the initial mapping epochs.
`AnalyzedRegionCover` is one current selection. `RegionCoverPlan` owns only the
selected portable topology, costs, stable identity, boundary response, and
implementation-cell summary. Input and owner-output bindings are frozen beside
the plan and survive global lowering as explicit provenance. A cache hit is
accepted only when current private source semantics reconstruct the same
topology and binding obligations; a cached payload is never connectivity or
ownership evidence.

Feedback does not reopen architecture search. When contracts change, only
dirty regions reproject exact timing onto the retained local identities,
reselect a cover from the same canonical construction, and compact the result.
They do not rerun lowering, Boolean rewriting, cut enumeration, or truth
computation. The retained compilation is discarded when the epoch loop ends;
it is neither a second mapped plan nor persistent cache state.

The coordinator owns one moved `RegionContractSet`, not an input copy plus a
mutable working copy. Each region has one `RegionalWorkingRow` containing its
active plan, immutable binding, and transient compiler. Best-epoch rollback
stores compact plans only; it does not duplicate bindings or compiler state.

### Deterministic Parallelism

Region tasks use one shared `ExecutionContext`. Estimated work is apportioned
with a deterministic largest-remainder calculation:

- balanced regions keep outer parallelism;
- a small number of dominant regions receive additional inner capacity;
- every task has at least one logical lane;
- no nested private pool is created.

Workers own local immutable artifact builders and return keyed outputs. Stable
reduction resolves boundary nets, appends the first generation in canonical
order, records footprints and owners, and validates that committed topology
matches the plans. Later epochs replace only dirty footprints.

### Post-Map And Timing

The mapped-generation boundary now publishes a compact generation-stamped
fanout/load profile before post-map begins. It contains complete sink counts,
fanout loads, and mapped-pin capacitances without retaining copied sink lists.
The same handoff freezes every top-level port net and the resolved output pins
of retained source instances. Every speculative post-map transaction checks
only its affected boundary nets before QoR evaluation; deleting a boundary net
or orphaning/multiply driving an observable output rejects the transaction.
Publication revalidates the complete frozen contract.

Post-map evaluates changes against real mapped timing in a fixed semantic
order: whole-net HFNS and electrical legalization, global STA refresh,
residual-branch cloning, then MFS, sizing, and pin swapping. HFNS discovers the
union of negative-slack mapped nets across enabled views and mapped nets with
explicit transition, capacitance, or fanout violations. Workers plan each
complete sink set independently, but stable reduction commits the eligible
trees as one fanout-forest transaction and therefore normally performs one
STA. A rejected forest is bisected only at stable net boundaries; individual
trees remain indivisible.

Residual critical branches are likewise collected into one clone forest after
the legalized-topology STA. There is no open-ended
clone-one-branch/recompute/restart loop. Early/late scenario data and electrical
limits participate in acceptance. Cloning or sizing before HFNS is
intentionally impossible.

HFNS, electrical buffering, and cloning group sink pins by their explicit
implementation-owner endpoint before constructing edits. Each cross-region
segment is owned by one `BoundaryEdgeId(driver_region -> sink_region)` and has
an independently maintained cell footprint. Driver provenance and ownership
lineage are recorded separately, so a sink affects placement of the boundary
artifact without becoming a semantic origin of the driver logic.

The boundary segment is a live implementation artifact, not merely an ownership
label on a temporary `RegionDelta`. Its exact edge cell footprint belongs to the
current mapped generation and is committed with the existing MMMC transaction.
Post-map reconstructs the physical repair topology canonically on every
synthesis run from graph boundary keys, endpoint contexts, and the current
implementation. `RegionalCacheRecord` retains regional construction decisions,
not a portable boundary topology, and the boundary path has no region/global
fallback.

HFNS uses actual mapped sink-pin capacitance and fanout in every early/late
library view. Deterministic load-balanced leaf groups and topology-depth
branching candidates keep planning scalable without extrapolating beyond a
Liberty load table. Residual electrical work is one plan per violating source
net and one generation-wide forest; accepted generations refresh exact STA and
continue to a fixed point. Topology and legalization evaluations are accounted
separately from the later QoR search budget.

Sizing candidate generation remains parallel and estimate-driven, and exact
evaluation first tries one atomic replacement forest per timing frontier.
Rejection bisects only at stable cell boundaries under the fixed QoR budget;
there is no independent per-cell control path. Pin permutation uses the same
forest executor and transaction objective.

Timing paths carry typed contributions. Diagnostics and `report_timing`
separate cell arcs, interconnect, and boundary effects; an interconnect step
also exposes fanout, load, resistance, wire delay, parasitic delay, and derate.
Optimization policy therefore follows measured path decomposition instead of
increasing pass counts when the dominant delay source is unknown.

`report_timing` is connected to the exact mapped timing engine. The current
supported command surface includes:

- `-from` and `-to` collections;
- the canonical `-delay_type max|min` spelling;
- a bounded global worst-path count through `-max_paths` (default 1);
- `-significant_digits`;
- full path output.

Unsupported `min_max` or alternate path formats return explicit
“not implemented” diagnostics rather than silently approximating behavior.

## Deleted Practices

The cutover removes or rejects:

- EHM/equivalence-hypergraph ownership of the full design;
- full-design repositories of structural alternatives;
- operator Top-K followed by regional Top-K combinations;
- bounded regional Pareto sets;
- a “winner” selected among speculative region implementations;
- uncontracted target-mode local lowering that competes with a second
  canonical cover;
- duplicate target cover during architecture preparation;
- fake target plans, fake bindings, and fake analyses;
- mapper-stage semantic extraction;
- predicted-memory admission and size thresholds;
- thread-dependent partitioning;
- global mutable target arenas;
- final-ID allocation from worker tasks;
- complete mapped-design clones per feedback candidate;
- fixed-size fake repair super-regions;
- sink-count-only or incomplete-view fanout-tree planning;
- QoR budgets that truncate topology or electrical legalization;
- fallback/legacy mapper switches and deprecated cache shells.

These are deleted concepts, not future extension points.

## Memory Ownership

The live structural state is bounded by:

- one normalized Word revision;
- one selected construction plan;
- one canonical target Boolean subject;
- region-partitioned packed analysis and bounded task scratch;
- one plan and binding per region;
- one retained best plan/binding checkpoint;
- the published mapped artifact;
- one artifact-owned incremental snapshot containing only reachable regional
  records.

An individual large region can use more inner workers but cannot cause all
regions to duplicate its subject. Regional reuse lives and dies with the
published artifact or its detached session snapshot; no process-global cache
retains unrelated histories. No RSS observation or heap estimate changes a
mapping decision.

## Diagnostic Contract

Typed diagnostics now separate:

- stable diagnostic code and title;
- primary source location;
- related source locations;
- explanatory notes;
- concrete help;
- Tcl invocation context.

Cycle reporting identifies the participating operation kind and source span.
Capacity and invariant errors name the failed ownership or typed range. Library
and target-binding failures name the relevant cell/pin contract. The renderer
still has a parser for frontend text that has not yet been converted, but typed
diagnostics are authoritative when present.

## Measured Regression

The first target regression used the public Ibex core with the SKY130 HD
typical Liberty view, a 10 ns clock, and eight synthesis workers.

| Revision state | Wall time | Mapping area | Cells | Post-map area | Post-map cells | WNS |
|---|---:|---:|---:|---:|---:|---:|
| Old duplicate local+canonical target cover | about 33.9 s | about 86k–88k | about 7.8k–7.9k | not canonical for comparison | not canonical | — |
| Incorrect removal without class preservation | — | 121,924 | 11,127 | — | — | severe regression |
| Canonical subject with protected equivalence classes | 7.7–8.2 s | 83,627.7 | 7,433 | 84,513.6 | 7,610 | -12.87 ns |

The important result is causal:

- regional construction vectors were identical before and after the bad
  change;
- the removed local binding had accidentally preserved alias classes;
- encoding class preservation in global dataflow restored and improved QoR;
- the obsolete duplicate target cover remained deleted.

This is a regression proof for the ownership fix, not evidence of a
large-scale performance objective. Ibex is too small and not
production-shaped enough for that claim.

## Verification

The cutover is guarded by tests for:

- protected canonical equivalence classes;
- hard regional identity through lowering;
- SCC partition behavior and cycle diagnostics;
- stable region identities and typed boundaries;
- one construction vector and memory decision keys;
- regional worker apportionment;
- target cover truth, binding, multi-output cells, and response scoring;
- explicit architecture-only versus covered regional analysis states;
- direct mapped-region commit and provenance audit;
- max/min scenario timing, aligned MMMC fanout loads, characterized-load
  domains, electrical-forest fixed points, and post-map rollback;
- structured diagnostic rendering;
- mapped `report_timing` and selector behavior;
- artifact-owned incremental snapshot reconstruction.

Before delivery the required repository gates are:

```text
cargo fmt --all
cargo check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
git diff --check
```

Private PDKs, license configuration, and non-redistributable inputs or reports
remain outside the repository.

## Remaining Qualification

The architecture cutover is complete; industrial qualification is not. The
next work is evidence-driven:

1. run CVA6 and resolve only reproducible semantic/scale failures;
2. add stage-level wall-time and peak-RSS baselines;
3. profile large regional rewrite/cut/cover tasks with sampling tools;
4. qualify one hundred-thousand-, one-million-, and ten-million-gate tier;
5. compare identical public inputs against the last accepted Opto baseline for
   area, WNS/TNS, cell mix, runtime, and RSS;
6. improve only measured bottlenecks without adding alternate owners.

A future optimization follows RFC 0007 for front-half locality and preserves
the unaffected explicit-boundary, deterministic-commit, mapped-ownership, and
typed-diagnostic contracts recorded here.
