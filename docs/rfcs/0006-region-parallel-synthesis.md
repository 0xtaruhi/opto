<!-- SPDX-FileCopyrightText: 2026 Zhengyi Zhang -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# RFC 0006: Canonical Region-Parallel Synthesis

- Status: Accepted and implemented
- Scope: synthesis architecture, regional mapping, deterministic publication
- Supersedes: speculative regional Top-K/Pareto design described by earlier
  revisions of this RFC

RFC 0007 replaced this RFC's global pre-freeze optimization and canonical
target-lowering locality. For RFC 0007 implementation work, those mechanisms
are current-tree evidence rather than compatibility requirements. This RFC
remains authoritative for the unaffected single-ownership, compact-plan,
deterministic-publication, mapped-feedback, and post-map contracts.

## Summary

Opto maps one selected semantic construction through one canonical Boolean
subject. The subject is divided into immutable region-owned slices for
parallel cut, match, and cover analysis. Each region returns one compact cover
plan. A coordinator commits those plans as direct mapped artifacts and uses
authoritative incremental timing feedback to replace dirty footprints without
reopening construction search.

The architecture has no synthesis “winner,” no regional portfolio, no EHM, no
runtime memory admission, and no alternate empty-library backend. Every synthesis
uses one canonical technology-independent construction followed by regional
Liberty cover and direct mapped-region commit.

## Motivation

Industrial synthesis must scale in several dimensions simultaneously:

- millions of operations and mapped gates;
- enough parallel work to use many cores;
- predictable peak memory;
- target-aware QoR that meets the versioned public-suite gates;
- identical output across worker counts;
- local incremental invalidation;
- failures that identify the RTL, constraint, or library cause.

A whole-design alternative graph gives transformations broad visibility but
retains too much state and creates global mutation ownership. Fully independent
region-local target mappers bound memory but duplicate canonicalization and can
disagree about boundary aliases. Speculative Top-K/Pareto region search
multiplies both problems.

This RFC chooses a middle boundary:

1. global semantic normalization and one selected construction;
2. one canonical Boolean topology;
3. region-owned parallel analysis of that topology;
4. deterministic global publication and feedback.

## Goals

- Make region-level mapping the primary unit of parallel work.
- Keep exact region boundaries through every canonical rewrite.
- Store one selected construction and one selected cover per region.
- Use actual Liberty cells, pins, loads, slews, and sparse scenarios.
- Avoid duplicate target lowering and complete candidate netlist clones.
- Bound memory structurally rather than denying valid work.
- Preserve stable region identity across worker counts and unrelated edits.
- Permit inner parallelism only when a few regions dominate total work.
- Keep final IDs, names, connectivity, provenance, and reports deterministic.
- Produce structured, source-oriented failures.

## Non-Goals

- A physical placement or congestion database.
- A public Opto-specific region-control Tcl surface.
- A global EHM, e-graph, or repository of mapped alternatives.
- Thread-count-dependent partitioning.
- Runtime RSS or predicted-heap admission.
- A second target mapper or compatibility path.
- Choosing several constructions and calling the best one a winner.
- Treating source hierarchy as a hard optimization boundary.

## Terms

- `SynthesisRegion`: stable semantic ownership unit over frozen Word IR.
- `RegionAnchorId`: stable seed identity used across compatible revisions.
- `RegionRevision`: content revision validating one anchored region.
- `RegionRowId`: dense typed index valid only for one region graph revision.
- `RegionBoundaryPort`: typed value crossing a hard region edge.
- `RegionalDecisionVector`: exactly one semantic construction for a region.
- `BoundaryContract`: immutable sparse timing/electrical interface for an
  epoch.
- `canonical subject`: the one Boolean topology for all selected target
  constructions.
- `RegionLogicSlice`: immutable region-owned view of the canonical subject.
- `AnalyzedRegionCover`: task-owned cut/match/cover analysis for one slice.
- `RegionCoverPlan`: compact selected portable cover plus response and keys.
- `MappedRegionFootprint`: committed stable mapped slots owned by one region;
  replacement appends new slots and tombstones the old footprint.

The unqualified word “winner” is intentionally absent from regional synthesis.
It is not an alias for a decision vector, a plan, an epoch, or a worker result.

## Invariants

### Single Ownership

Every semantic and mapped object has one owner:

- Proc owns procedural scheduling until normalization;
- Word owns language-neutral values and operations;
- the region graph owns regional membership and boundary schema;
- the canonical subject owns selected Boolean topology;
- a region analysis owns mutable cuts and cover scratch;
- `MappedNetlist` owns published target topology;
- timing and power own sealed analysis generations.

No algorithm reconstructs ownership from a name or pointer.

Within the mapped topology, `ImplementationDb` stores semantic provenance and
ownership as separate relations. Every live cell has one owner atom: the
global substrate, one synthesis region, or one exact
`BoundaryEdgeId(driver_region -> sink_region)`. A fanout repair with sinks in
several regions is split into one segment per sink-region endpoint, never a
multi-region owner set. The database maintains the reverse boundary-edge cell
footprint across edit commit, dense publication, serialization, and checkpoint
validation.

Each exact edge footprint is additionally captured as a portable
`BoundaryRepairArtifactRecord`. Its semantic identity includes the driver and
single sink region, both selected plan contexts, and the sorted semantic keys
of all graph ports on that edge; its content generation includes the canonical
cell/net/reconnection payload. The checkpoint stores stable names and library
identities only. Restore occurs after the winning regional epoch is installed:
all anchors are validated against that generation, then the complete repair
forest and its exact runtime footprint enter mapped IR and every enabled timing
view atomically. A non-crossing or incomplete endpoint is handled explicitly as
a region/global segment before boundary lineage is recorded; the boundary
owner itself has no fallback case.

### One Construction Per Region

Every operator and first-class memory owned by a region appears exactly once in
its `RegionalDecisionVector`. The vector has a stable key and is reconstructible
from the target/scenario catalog.

It is legal for structural planning to rank possible recipes internally in
order to choose the vector. It is not legal to retain several complete vectors
for later mapped competition. Once lowering begins, construction is frozen.

### One Canonical Target Subject

Target mode lowers the complete selected construction exactly once. Regions do
not own private target-mode Word cones or independently canonicalized Boolean
copies.

The subject may contain nodes owned by different regions, but every owned
operation and boundary value has an explicit typed owner. Regional analyses
receive only an immutable slice.

### Hard Boundary Equivalence Classes

A region boundary is a dataflow identity, not merely a net that materialization
might recreate.

Let `rep(v)` be the terminal representative found by canonicalization and let
`P` be the protected values. The class

```text
C(r) = { v | rep(v) = r }
```

is protected when any `v` in `C(r)` is in `P`. Every equivalence
`v -> rep(v)` in a protected class is rejected. Protection of only the
individual port is insufficient because a sibling alias could still erase the
representative and change regional roots.

In addition:

- values with different region owners do not merge;
- an owned value does not merge into an unowned value;
- observable roots before and after lowering are protected;
- representative chains are closed and checked for cycles before application.

### Immutable Analysis, Deterministic Commit

Workers read sealed Word, region, subject, library, constraint, and contract
views. A worker mutates only its own builder or assigned output row.

All global changes are reduced through stable keys and total orders. Workers
prepare artifact-local topology; the coordinator appends it deterministically
and records stable footprints. Worker completion order has no semantic effect.

## Pipeline

### 1. Global Semantic Phase

One linked root is normalized to process-free Word IR. A procedure first
follows:

```text
Proc CFG
  -> CFG canonicalization
  -> per-target sparse state propagation
  -> typed event-aware control extraction
  -> canonical Predicate DAG
  -> Word IR
```

Canonicalization removes unreachable blocks, folds constant decisions, threads
empty jumps, coalesces equal successors, models a common exit, and computes
dominator/post-dominator structure. Unsupported loops fail explicitly. Only
then, before regions are frozen, the global flow performs:

1. resolved-driver lowering and validation;
2. exact canonicalization and CSE;
3. constants, known bits, liveness, demand, and DCE;
4. sequential equivalence and FSM optimization;
5. arithmetic sharing and semantic operator discovery;
6. first-class memory and target preparation.

Semantic recognition ends here. Mapping does not contain an “Extract” phase
that rediscovers operators from Boolean structure.

### 2. Region Graph

The graph is built from the frozen Word revision and optional resource
affinities.

Natural anchors include:

- state and clock/reset semantics;
- first-class memory interfaces;
- top-level interfaces;
- hard macro interfaces.

The partitioner constructs operation dependencies, computes SCCs, and treats
each SCC as an indivisible unit. It then packs units using deterministic work,
connectivity, boundary cost, and affinity. Oversized acyclic work is cut into
bounded regions. True unsupported cycles are diagnosed before scheduling.

Every cross-region value becomes an explicit typed port. Predecessor and
successor edges are packed rows, not object pointers.

### 3. Stable Identity

`RegionAnchorId` is derived only from the stable cone seed. `RegionRevision` is
derived from domain-separated canonical bytes covering:

- region kind and semantic anchors;
- exact local operations and resource contracts;
- typed boundary schema;
- relevant origin-independent content.

Digest equality is followed by canonical identity-byte validation.
`RegionRowId` never enters persistent identity, cache keys, provenance, or
output naming.

Timing object bindings use stable flat names as their persistent lookup keys.
Mapped publication may repack generation-local dense IDs, but it must preserve
cell, pin, and net names exactly so SDC object bindings remain valid.

`RegionContextKey` additionally seals:

- boundary contracts;
- scenario generation;
- target fingerprint;
- effort/search ABI;
- direct predecessor summaries.

### 4. Sparse Contracts

Each active scenario explicitly binds its constraints, early/late timing
libraries, interconnect view, activity, and enabled checks. No implicit
mode-by-corner Cartesian product is created.

Contracts carry sparse rows for:

- early/late rise/fall input arrival and transition;
- early/late rise/fall output required time;
- early/late capacitive and fanout load;
- transition, capacitance, and fanout limits;
- timing tag and check context;
- region/scenario/target/epoch generations.

Contracts are immutable for one epoch.

### 5. Construction Planning

`StructuralTargetModel` converts target and scenario properties to bounded
structural estimates. `RegionCostEnvelopeSet` derives deterministic regional
depth and cost budgets.

For each region, construction planning:

1. ranks the legal recipes for every semantic operator;
2. builds the operator dependency DAG;
3. allocates the regional depth budget along that DAG;
4. chooses one recipe per operator with a total deterministic order;
5. chooses one exact memory implementation per owned memory;
6. seals one `RegionalDecisionVector`.

If the minimum-depth construction cannot meet a structural budget, it is still
the single construction supplied to exact mapping; the failure is not hidden
by a portfolio. Exact mapped timing decides the resulting violation.

### 6. Lowering

Selected memory and operator decisions are published before Boolean lowering.
Region ownership is extended to newly materialized operations.

The mapping path:

1. capture pre-lowering mapping roots and region identities;
2. lower selected Word operations to bits;
3. capture lowered identities and post-lowering roots;
4. canonicalize only equivalences allowed by owner rules;
5. preserve every equivalence class containing a protected identity;
6. produce one compact canonical subject.

### 7. Regional Target Analysis

`RegionalLogicPartition` derives one slice per graph row from the canonical
subject. It records owned roots, hard inputs, cross-region observations, and
contract guidance.

Each region task:

1. builds or reuses its canonical subject view;
2. runs bounded Boolean rewrite;
3. enumerates packed cuts;
4. computes exact truth and phase;
5. matches legal Liberty cells and pins;
6. resolves multi-output sharing and demand;
7. chooses one cover with a deterministic mapping policy;
8. measures its sparse boundary response;
9. serializes one compact `RegionCoverPlan`.

The serialized target plan carries canonical-slice input and output ordinals,
not revision-local Word IDs. On a cache hit, Opto validates the region,
context, construction key, payload topology, and stable plan key, then rebuilds
the binding from the current canonical slice. That row skips rewrite, cuts,
cover selection, response measurement, and compaction. Mixed hit/miss runs
analyze only the missing rows.

The analysis may keep alternate cuts required by the cover algorithm. Those
are internal dynamic-programming state, not alternative regional
implementations and not a Pareto plan set.

### 8. Parallel Scheduling

Outer region tasks are primary. The runtime is shared with the rest of
synthesis.

For estimated regional work `w_i` and available lanes `T`, initial inner lane
quotas use Hamilton apportionment:

```text
q_i = floor(T * w_i / sum(w))
```

Remaining lanes go to the largest fractional remainders with row order as the
tie break. A task-local limit is clamped to at least one, but the shared pool
still prevents physical oversubscription.

This produces:

- broad outer concurrency for balanced designs;
- cooperative inner analysis for a few dominant regions;
- stable allocation independent of completion timing.

### 9. Commit

Plans contain typed artifact-local IDs only. The coordinator:

1. verifies one plan and binding per graph row;
2. builds ports, source instances, memories, and clock infrastructure once;
3. freezes lowered-value bindings and boundary aliases in that substrate;
4. prepares independent sequential and regional mapped artifacts;
5. appends all first-generation artifacts in one `RegionDelta`;
6. records a stable `MappedRegionFootprint` and provenance per region;
7. replaces dirty footprints transactionally, appending new slots and
   tombstoning old slots without renumbering survivors.

Worker artifacts never allocate final IDs and target topology is never emitted
back into Word.

Post-map cells carry semantic-source lineage separately from ownership
lineage. Ordinary rewrites inherit one existing owner atom. HFNS and cloning
name a driver lineage and a single sink endpoint, producing either a local
region owner or an exact boundary edge; they never fold sink provenance into
the driver implementation.

### 10. Global Feedback

The mapped substrate resolves explicit observations for every live regional
boundary. One sparse MMMC owner service analyzes the complete first generation;
later region deltas update those same owners incrementally and return exact
measured boundary responses.

MMMC view construction, early/late ownership, lane projection, aggregation, and
exact closure ordering have one definition. The regional epoch coordinator and
the post-map transaction boundary both compare full-design closure through that
definition. Phase-local boundary legality, connectivity validation, and stable
tie breaking wrap it without redefining timing or electrical quality.

The epoch coordinator marks contracts that violate timing/electrical
expectations and propagates a deterministic dirty set. For target mode:

- clean region plans are retained;
- dirty contracts and context keys are rebuilt;
- the region's transient AXM subject, cuts, and truth rows are retained;
- the cover is selected again under the new context without repeating Boolean
  rewriting, cut enumeration, or truth computation;
- the construction vector remains unchanged.

The coordinator keeps one best legal plan/binding checkpoint by a total global
objective. At convergence or the deterministic effort limit, it either keeps
the current state or restores changed footprints with region deltas. It never
stores complete mapped clones for every epoch.

Stable slots remain append-only while any timing owner is live. After post-map,
the publication barrier performs one dense repack and applies the returned cell
translation to provenance; tombstones are not serialized.

### 11. Post-Map

The committed mapped generation publishes a compact fanout/load profile with
the complete sink count, abstract fanout load, and mapped-pin capacitance of
every multi-sink net. Post-map consumes that profile and the sole mapped
topology in this fixed order:

1. initial max/min STA and typed path-increment decomposition;
2. all-violating-net whole-net HFNS and atomic balanced fanout-forest
   insertion;
3. transition/capacitance/fanout legalization;
4. authoritative global STA refresh;
5. atomic residual critical-branch clone-forest insertion;
6. bounded MFS-style local replacement, compatible sizing, and pin swapping.

HFNS uses the full sink set and actual mapped pin loads after commit. A branch
clone, local resize, or MFS edit cannot run ahead of it. In particular, repeated
single-branch cloning is not a legal substitute for synthesizing a
thousand-sink net: the next sink would simply inherit the same dominant wire
delay.

Every enabled timing view contributes all mapped nets on negative-slack paths,
and explicit transition/capacitance/fanout violations contribute their mapped
nets even when timing slack is nonnegative.
Read-only workers plan their complete balanced trees in parallel. Stable
reduction combines eligible plans into one fanout forest, so the normal case
uses one global STA rather than one STA per net. Rejection triggers
deterministic bisection at net boundaries; no bisection may divide one net's
tree or sink set.

Planning uses the actual sink-pin capacitance and fanout from every aligned
late/max and early/min view. Leaf groups are deterministically load-balanced;
cell arcs, wire models, and characterized output-load domains must be complete.
Branching-factor search evaluates the endpoints of distinct tree-depth
intervals, so planning cost grows with topology depth rather than sink count.

Residual electrical violations are reduced to at most one ranked branch per
source net and committed as one generation-wide forest. Accepted generations
refresh exact STA and repeat to a fixed point; rejection is bisected only at
source-net boundaries. These finite topology evaluations are not charged to
the later QoR-search budget.

Residual critical branches are reduced into one clone forest under the same
transaction and bisection rules. There is no serial loop that clones a branch,
recomputes the worst path, and restarts discovery. Topology, timing, power, and
provenance update atomically or roll back together. The legalized topology is
globally refreshed before residual local optimization begins.

Compatible sizing similarly reduces one semantic timing frontier to one atomic
replacement forest. A rejected forest is bisected only at stable cell
boundaries and only within the fixed QoR evaluation budget; there is no second
per-cell optimization loop. Eligible critical pin permutations use the same
forest executor and bounded rejection splitting.

## Memory Model

The architecture retains:

- one compact normalized Word revision;
- one region graph and one construction plan;
- one canonical selected Boolean subject;
- region-partitioned packed analysis state;
- bounded active-task scratch;
- one compact cover plan/binding per region;
- one retained best plan/binding checkpoint;
- one prior artifact-owned incremental snapshot borrowed read-only while its
  replacement is built.

It does not retain:

- a full-design EHM or e-graph;
- all possible operator constructions;
- multiple complete regional target lowerings;
- Top-K complete regional vectors;
- a Pareto set of regional plans;
- cloned mapped designs for every feedback step;
- a process-global regional decision cache.

No allocation estimate decides whether valid RTL may synthesize. Capacity
errors identify the exact typed arena or unsupported representation that
overflowed.

## Incremental Reuse

Process-scoped content-addressed caches may retain:

- Word rewrite recipes;
- Boolean rewrite recipes;
- target-derived immutable catalogs.

Each root artifact owns one `IncrementalSnapshot` containing its source
identity and canonical regional decision/plan records. A synthesis explicitly
borrows one prior snapshot, reads it without mutation, and publishes a new
snapshot containing only contexts reachable from the current base regions and
epoch journal. Artifact invalidation moves the snapshot into session state;
checkpoint installation validates snapshots but never restores them into an
engine-global map. Concurrent and failed synthesis runs therefore have no regional
cache side effects.

Every record is validated against canonical region identity, local semantics,
boundary schema, context generation, target, scenario, and effort. Raw arena
IDs and pointers are forbidden.

A source edit invalidates affected semantic regions and their dependent
contexts. Worker chunking is not part of invalidation.

## Diagnostics

Regional failures must distinguish at least:

- invalid RTL/dataflow cycle;
- unsupported semantic operation or memory contract;
- missing/incompatible Liberty cell or pin;
- boundary ownership mismatch;
- stale generation or incremental snapshot reconstruction failure;
- typed capacity overflow;
- internal invariant violation.

Whenever source provenance exists, a diagnostic provides a primary source span
and related spans for the cycle or ownership edge. Notes explain the failed
invariant; help describes a supported user action when one exists. The Tcl
command is secondary invocation context, not the only location.

## Determinism

With identical complete inputs and effort, output is invariant across worker
counts:

- region identities and construction vectors;
- selected plans and committed mapped topology;
- final IDs, names, and connectivity;
- provenance and cache reachability;
- area, timing, power, and diagnostics.

Hash maps cannot define output order. Every comparison is total. Floating-point
input rejects NaN and reductions use stable canonical order.

## Rejected Alternatives

### Global EHM Or Alternative Graph

Rejected because alternatives, proof state, and mutation ownership expand
across the full design. This does not provide a predictable multi-million-gate
memory bound.

### Independent Local Target Lowering

Rejected because each region would canonicalize a different view of shared
aliases and sequential roots. Direct materialization would then become a
semantic repair stage instead of a mechanical artifact commit.

### Top-K And Pareto Regional Mapping

Rejected because complete vector materialization multiplies lowering, rewrite,
cut, cover, and response work. It also conflates construction choice with exact
cover selection and makes memory proportional to speculative implementations.

### Worker Race And Winner

Rejected because “first/best completed worker” has no stable semantic scope.
Even a deterministic later comparison would merely recreate the rejected
regional portfolio.

### Runtime Memory Admission

Rejected because predicted heap cost is neither a semantic limit nor a stable
optimization input. Industrial capacity comes from compact representations,
partitioned ownership, bounded scratch, and measurable failure modes.

### Thread-Dependent Regions

Rejected because changing thread count would change identities, cache reuse,
cross-boundary optimization, and output.

### Mapper-Stage Extraction

Rejected because semantic discovery after construction freeze creates a second
owner for arithmetic, mux, memory, and control meaning.

## Implementation Record

The accepted cutover:

1. added stable typed region and boundary identities;
2. made partitioning SCC-aware;
3. removed memory admission;
4. reduced regional architecture search to one vector;
5. made target lowering canonical and owner-aware;
6. protected complete boundary equivalence classes;
7. mapped canonical regional slices in parallel;
8. allocated inner workers by deterministic weighted work;
9. committed compact plans as mapped artifacts and measured global boundaries;
10. reused/refreshed canonical analyses for dirty feedback;
12. removed fake target plans, duplicate local cover, Top-K/Pareto, fake repair
    super-regions, and dead migration structures;
13. aligned MMMC whole-net HFNS and generation-wide electrical legalization;
14. added typed diagnostics and exact mapped timing reports.

There is no feature flag or production path that restores the superseded
architecture.

## Qualification

Required tests and benchmarks cover:

- canonical equivalence-class protection;
- cross-region ownership and frozen boundary aliases;
- SCC condensation and true-cycle diagnostics;
- one-vector construction and artifact-owned snapshot reconstruction;
- target truth, pin binding, phase, demand, and multi-output cover;
- sparse early/late boundary response;
- explicit empty/covered regional analysis states;
- deterministic worker allocation and byte-identical output;
- feedback convergence and best-footprint restoration;
- aligned MMMC fanout loads, characterized-load bounds, electrical fixed
  points, and post-map transaction commit/rollback;
- mapped `report_timing`;
- incremental clean-region reuse;
- wall time, peak RSS, area, timing, and cell composition at increasing scale.

The architecture is considered industrially qualified only after the public,
production-shaped million-gate tiers satisfy the versioned gates in
`docs/architecture.md`.
