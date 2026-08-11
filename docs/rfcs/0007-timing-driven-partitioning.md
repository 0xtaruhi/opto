<!-- SPDX-FileCopyrightText: 2026 Zhengyi Zhang -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# RFC 0007: Timing-driven partitioning and region-private optimization

- Status: accepted
- Implementation: production architecture and phase 0-5 code cutover complete;
  large-design runtime, incremental, equivalence, and QoR qualification remains
  a future qualification target rather than a claimed result
- Author: Zhengyi Zhang
- Date: 2026-08-03

## Summary

RFC 0006 made everything after the region freeze content-addressed and
parallel. Everything before it is still position-addressed and serial: global
Word optimization runs on the whole value arena before any region exists,
repeats itself to a fixpoint, and renumbers the arena. That serializes the
synthesis and destroys the identities incremental reuse depends on.

This RFC closes the front half with one rule: **the region is the only
optimization unit.** The coordinator may establish ownership and commit
owner-confined structural rewrites in stable region order, but it may neither
discover nor apply an optimization across owners.

```
Normalization
Anchoring              read-only structural estimate and criticality ranking
Ownership Partition    cone claiming from stable anchors, then local matching
Owned Structure        FSM/sharing/control rewrites, confined to one owner
Final Region Freeze    rebuild identities, boundaries, estimates, and contracts
Region Work            private IR: optimize, plan, lower, map (parallel)
  <-> Timing Contract  sparse timing/electrical feedback only
Assembly               deterministic publication from region artifacts
```

No opportunity analysis may influence the ownership partition. FSM
re-encoding, common-subexpression elimination, sequential equivalence merging,
resource sharing, and control preparation may run only after every source
operation has one provisional owner, and every candidate must prove that all of
its operations have that same owner. Architecture selection, operator fusion,
bit lowering, and cover remain work on final region-private modules. Feedback
never repartitions the design.

This scope is the pre-freeze semantic optimization stage. Post-map optimization
remains global and is unaffected; see *Cross-boundary optimization after this
RFC*.

Five pillars:

1. **Timing-driven partitioning** — each flip-flop keeps its fan-in cone; shared
   logic gets a single owner; regions coarsen by local matching on criticality.
2. **No false invalidation** — partition, identity, and budget depend only on
   real structural, fact, and contract closures.
3. **Region-private authority and IR** — structural rewrites have exactly one
   region owner; after the final freeze each region owns a local module and
   returns a complete artifact.
4. **One dataflow authority** — the frozen Word graph defines connectivity,
   constants, liveness, and region ports; boundary contracts carry only
   timing/electrical measurements.
5. **Locally dependent budget** — absolute estimated delay, no design-wide
   normalization.

Pillars 2 and 5 exist because removing global passes is not sufficient: if the
partition, the identities, or the budget depend on design-wide state, one edit
invalidates every region no matter where optimization runs.

## Motivation and compatibility evidence

### What the tree does today

`engine::plan_regions` performs, serially, in one function body: constant
validation, combinational canonicalization, derived-FSM optimization, a second
canonicalization, sequential equivalence sharing, a third canonicalization,
`compact_netlist`, pre-freeze mapping
preparation, muxed-arithmetic sharing, architecture decisions, partitioning,
contract allocation, and regional architecture search.

- **Serial section.** All whole-module. `rewrite_value_uses` and
  `compact_netlist` are whole-arena barriers, executed up to four times.
- **Fixpoint by re-running.** Canonicalization runs three times because passes
  communicate only through the IR.
- **Total invalidation.** `SourceSnapshot::changes_from` compares fingerprints
  **by arena index** (`changed_entries`). One inserted value shifts every later
  position, so a one-line edit reports the whole arena as changed.

### What the tree already has, and what it does not

| Capability | Where | Status |
| --- | --- | --- |
| Order-independent semantic value keys | `partition::semantic::value_keys` | exists, private to partitioning |
| Per-operation source span | `word::Operation { source: SourceSpan }` | exists, **unusable as identity** |
| Timing context clip | `BoundaryInputContract`, `BoundaryOutputContract` | exists, sparse MMMC |
| Region-private bindings | `mapping/region_binding.rs` | exists in the single Liberty path |
| Per-operator structural estimate | `StructuralEstimate { logic_depth, logic_units, wiring_units }` | exists, **reachable only through `ArchitectureDecisions`** |
| Boundary alias wiring | `regional_boundary_aliases` | exists |
| Deterministic ordered parallelism | `ExecutionContext::map_ordered` | exists, single machine |
| **Stable operation identity** | — | **does not exist** |
| **Syntax-path provenance from the frontend** | — | **does not exist** |
| **Hierarchy occurrence provenance on operations** | — | **does not exist** |
| **Stable region identity** | — | **does not exist** |
| **Collision-free boundary port identity** | — | **does not exist** |
| **Locally dependent budget** | — | **does not exist** |
| **Edit-stable region formation** | — | **does not exist** |
| **Estimation independent of architecture selection** | — | **does not exist** |

The absent rows are corrections to earlier drafts, which claimed the identity
rows existed and treated the estimator as freely reusable. The evidence:

- `seal_region_identities` derives `identity` from region content plus sorted
  boundary semantic keys, then
  `SynthesisRegionId = H(REGION_ID_DOMAIN ‖ identity ‖ ordinal)`. That is a
  content hash, and identical-content regions are disambiguated by **enumeration
  ordinal**. `RegionLocalKey` hashes the same bytes, so the two are one level.
- Boundary ports are identified by transitive `semantic_key` — a content hash
  serving simultaneously as wiring identity.
- `allocate_path_budgets` computes `critical_work = max(completion_work)` over
  the whole design and scales every region by `total_budget / critical_work`.
  `reallocate_contracts(dirty, ...)` bounds *which* rows are recomputed; it does
  not make the *result* locally dependent.
- Packing consumes units sorted by semantic key — a content hash.
- `ArchitectureDecisions::for_module_with_sharing` runs
  `ObservableBits::analyze(module)` over the whole module, builds an
  `OperatorCatalog` performing additive-chain fusion (`absorb_into`,
  `collect_additive_region`), enumerates implementation candidates, and accepts
  a `ResourceSharing` result. Partitioning consumes it today.
- `SourceSpan` carries absolute `line` and `column`, so inserting one line
  shifts every operation anchor below it in that file.
- `linked_elaboration::copy_values_and_operations` writes
  `source: operation.source.clone()` with no hierarchical component. The
  `prefix` argument reaches signal, memory, register, latch, and instance names
  only. Every occurrence of a module instantiated *N* times therefore produces
  *N* operations sharing one span, kind, and width; disambiguating them by
  ordinal reduces to instance enumeration order, so adding one instance
  renumbers every later occurrence.

### Gap against the target architecture

A scalable timing-driven partitioner must cut across source hierarchy, retain
the timing context of every region, keep state boundaries explicit, and avoid
cutting critical launch-to-capture cones arbitrarily. Coarse regions amortize
scheduling and publication costs; finer subregions bound local optimization
work. The current implementation does not yet satisfy that complete contract.

| Criterion | Target | Opto today |
| --- | --- | --- |
| Partition objective | timing criticality; cut only non-critical edges | equal work, `target_work: 32_768` |
| Flip-flop treatment | FF's fan-in path stays with the FF | every `is_state` op is its own single-op region |
| Fine granularity | 10K+ instances | 1 operation for state regions |
| Cross-region context | timing/physical clip | timing contract; no dataflow facts |
| Optimization locality | region-resident | whole-module, pre-freeze |

### What is deliberately not copied

**Physical awareness.** Opto has no placement model; criticality is
logic-depth driven and will misrank wire-dominated designs.

**Global architecture solve.** Rejected: a cross-region joint optimizer makes any
region's cost change reallocate every other region's decision. **We trade a
known QoR mechanism for incremental isolation; we do not get both.**

**Cross-synthesis feedback.** No measurement from a previous synthesis may change the
partition. Deterministic output is a correctness property.

## Detailed design and invariants

### Global layer authority

The global layer validates, partitions, commits owner-confined structural
rewrites in deterministic region order, rebuilds and seals the final graph,
allocates and propagates contracts, and assembles artifacts. It may **not**
perform an optimization whose candidate spans provisional owners, architecture
selection or operator fusion outside a final private module, or repartitioning
from feedback.

Opportunity analysis starts only after provisional ownership exists. FSM,
sequential-equivalence, muxed-arithmetic, combinational canonicalization, and
target sequential preparation must reject every candidate whose complete
read/write footprint is not owned by one region. Their results are never an
input to the ownership partition. The coordinator's mutation of the source
module is a deterministic commit mechanism, not shared optimization authority;
the final graph is rebuilt from the committed structure and then frozen.

The reason is not purity. Partition inputs are partition dependencies: an
affinity derived from a design-wide search makes region membership depend on
design-wide state. A partition that is locally computed but globally seeded is
globally dirty.

**Synthesis root closure.** The global layer partitions the *synthesis root
closure*: the operations reachable backward from output ports, state inputs,
memory writes, and preserved signals. Operations outside that closure are not
part of the work to be partitioned, receive no owner, and never enter a region.

This is a definition of scope, not a global DCE exception. Operations outside
the closure are absent from ownership and from optimization; they are removed
only by ordinary compaction after owner-confined commits.

Sharing candidates in different mux branches or equivalent sequential cones
are retained when the ownership partition puts their full footprint in one
region. A candidate spanning owners is deliberately unavailable. Improving
that coverage requires a better structural cone definition, not a global
proposer or a cross-owner rewrite.

### Identity

Five identities, none serving two purposes.

| Identity | Derived from | Purpose |
| --- | --- | --- |
| `OperationAnchorId` | hierarchy occurrence, syntax-path identity, operation role | stable identity for an operation and for anything anchored to it |
| `RegionAnchorId` | the cone seed's anchor | locate a cache entry |
| `RegionRevision` | transitive hash of the region's private module | validate that entry |
| `BoundaryPortId` | owner anchor, peer anchor, role, endpoint | stable wiring identity |
| `BoundaryValueRevision` | content of the value crossing that port | invalidate downstream work |

`OperationAnchorId` is the foundation everything else rests on, and it may not
be derived from `SourceSpan`. A span carries absolute line and column, so
inserting one line invalidates every anchor below it; and
`linked_elaboration` copies spans verbatim, so all occurrences of a
multiply-instantiated module share one span and can only be told apart by
instance enumeration order. Both defects reproduce exactly the positional
identity this RFC exists to remove. **`SourceSpan` is diagnostic information and
is never an identity.**

The anchor has three components:

- **Hierarchy occurrence** — the elaborated instance path owning this
  operation. Instance names are source declarations and are stable; adding a
  sibling instance does not renumber others. `linked_elaboration` must therefore
  carry occurrence provenance onto operations, which today it applies only to
  signal, memory, register, latch, and instance names.
- **Syntax-path identity** — the declaration the operation was lowered from
  (a named signal, port, register, or procedural assignment) plus its path
  within that declaration's expression tree. A path such as
  *(assignment to `y`, right operand, left operand)* is unchanged by inserting
  comments, reordering unrelated declarations, or editing other lines.
- **Operation role** — one source expression may lower to several Word
  operations (extend, add, truncate). The role distinguishes them without
  reference to arena order.

Operations the frontend synthesizes with no direct syntactic counterpart derive
their anchor from the nearest anchored ancestor plus the operand role and the
transformation kind that produced them. The recursion bottoms out at
syntactically anchored operations. No component of any anchor is an arena index,
a span ordinal, or an enumeration position.

*Consequence for scope:* neither syntax-path provenance nor operation-level
hierarchy occurrence exists in the tree today. Both are prerequisites for this
RFC rather than details of it, and they span the frontend, `opto-ir`, and
`opto-synth`.

`RegionAnchorId` derives from the seed alone, never from membership, avoiding
the circularity of defining a region's identity by what it contains. A coarsened
region takes its lowest-ordered constituent anchor.

`BoundaryPortId` requires an endpoint component because a region pair commonly
has several boundary values in the same direction. The endpoint is the
declaration identity plus `(lsb, width)` for a value on a declared signal or
port, and the driving operation's `OperationAnchorId` for an unnamed
intermediate. Partition cuts land on unnamed intermediates routinely, so this
case is not treated as marginal.

### Pillar 1: timing-driven partitioning

**Structural estimate without architecture.** Pre-freeze estimation uses a new
`StructuralEstimateIndex` deriving per-operation depth, logic units, and wiring
units from the **original operation kind, operand widths, and target unit cost**
alone. No operator recognition, no additive-chain fusion, no candidate
enumeration, no sharing input. `ArchitectureDecisions` stays after the freeze,
inside region work.

*Cost:* an additive chain that would fuse is estimated as a chain, overstating
its depth. Criticality is a ranking, so uniform overstatement is tolerable;
systematic misranking of fusible datapath against non-fusible control is the
real risk, measured in phase 1.

**Criticality.** Arrival and required times propagate over the Word graph
between state anchors and ports, seeded by `primary_scenario().constraints()`,
using `StructuralEstimateIndex` delays. Only the **ranking** is used.

**Cone seeding and frontier closure.** Seeds are state operations and output
ports. A cone grows backward over fan-in until it reaches another state
operation, a port, or its size limit.

When the size limit truncates growth at an edge `u -> v` with `v` inside the
cone and `u` outside, `u` becomes a **frontier seed** with
`RegionAnchorId = OperationAnchorId(u)`, and grows its own cone under identical
rules. Frontier cones are ordinary cones thereafter: they claim, they coarsen,
they own boundaries.

This closes the algorithm. Termination: each truncation creates one seed, seeds
are distinct operations, and the operation count is finite. Coverage: every
operation is either claimed by a cone, promoted to a frontier seed, or marked
unreachable during anchoring. Uniqueness: claiming is exclusive, and an
operation reached by several cones goes to the highest-criticality claimant with
ties broken by anchor identity.

> **Ownership invariant.** After partitioning, every reachable operation has
> exactly one owning region, and every unreachable operation has none.

**Claim-based ownership.** There is no bin packing over a global sequence. Cones
are processed in `RegionAnchorId` order and claim unowned operations by
deterministic backward traversal. Adding or deleting a seed perturbs only what
that seed claimed or released, plus its immediate neighbours.

**Coarsening by local matching.** Cone count is on the order of the flip-flop
count and must come down by orders of magnitude. Coarsening never uses global
packing:

- a unit whose size has reached the target withdraws from matching entirely —
  it neither nominates nor accepts. This is a purely local predicate;
- an active unit nominates one neighbour: the incident edge of highest estimated
  criticality whose merge would not exceed the size limit, ties by
  `RegionAnchorId`;
- a mutually nominating pair merges, taking the lower-ordered anchor and the
  union of members;
- rounds repeat up to a fixed policy round count, identical for every design.

**Termination and region count are separate concerns.** The round count is a
constant, not a function of the design, and no rule stops matching when a
design-wide region total is reached. A design-wide stopping condition would let
a local edit change the number of rounds executed and therefore re-form every
region — the same class of defect as sorting by content hash. Region count is an
outcome measured at acceptance, never a control input.

No lower bound is claimed on merges per round. A strictly increasing weight
chain can yield one merge per round, so the achieved region count varies by
design; it is calibrated, not guaranteed.

A cone's nomination depends only on its incident edges, so an edit perturbs
nominations within a radius equal to the round count — bounded, and never
cascading along a global sequence.

**Cut selection is a consequence, not a rule.** Matching consumes the highest-
criticality edges first, so the edges left uncut at the end — the region
boundaries — are the least critical ones.

### Pillar 2: no false invalidation

The property to guarantee is precision, not size:

> **No-false-invalidation invariant.** An edit may invalidate a region only if
> that region lies in the closure of real structural, fact, or contract
> dependencies of the edit. No region is invalidated by design-wide ordering, a
> design-wide stopping condition, a shared normalization factor, a content hash
> used as an identity, or a partition input derived from a design-wide search.

A bounded-size guarantee would be false: editing a high-fanout control signal or
a shared decoder legitimately reaches thousands of regions, and those regions
genuinely depend on it. Invalidating them is correct; invalidating unrelated
regions is the defect.

### Pillar 3: region-private IR

Privacy begins with authority, not allocation. The provisional graph gives
every live source operation one owner. Stateful and sharing transformations
analyze only one owner's footprint and the coordinator commits their edits in
stable owner order. Rebuilding after a topology-changing stage prevents stale
row IDs from becoming implicit authority.

After the final freeze, each region imports a local `WordModule` plus a
`source_to_local` value map and returns a complete artifact. A register or latch
is not copied into that combinational module: its `Q` is a typed boundary input,
while its `D` and controls are observable roots. This explicit state cut avoids
recursive placeholders, backpatching, and duplicate state ownership.

`DataflowScope` is deleted rather than generalized. Final architecture
selection, multi-operand CSA/Wallace/Dadda reduction, fused arithmetic, bit
lowering, Boolean optimization, and cover operate only on the private module.
Assembly is deterministic reconstruction from region artifacts in region
order.

### Pillar 4: one frozen dataflow authority

Boundary contracts do not duplicate Word semantics. The final Word graph is
the only authority for connectivity, constants, liveness, aliases, and region
ports. Region optimization may simplify an owned implementation, but its local
care set cannot publish an alias into the global substrate or delete another
owner's endpoint.

`BoundaryContract` carries only sparse timing and electrical rows. Feedback may
change a region's optimization budget, but it does not rewrite connectivity,
repartition ownership, or manufacture a dataflow fact. `BoundaryPortId` keeps
wiring identity and `BoundaryValueRevision` keeps invalidation identity;
neither licenses merging implementations across owners.

### Pillar 5: locally dependent budget

The present allocator normalizes by a design-wide maximum, making every contract
depend on every region. It is replaced:

```
arrival(R)  = max over predecessors P of (arrival(P) + delay(P)), 0 at a state or port source
required(R) = min over successors S of (required(S) - delay(S)), period - setup at a sink
```

`delay` is the region's absolute estimated delay. No design-wide scaling factor
is computed or applied.

Locality comes from the `max`/`min`: a region off the critical path can change
its delay without changing any downstream `max`, so propagation stops there.

**Infeasibility is reported, not absorbed.** If accumulated estimated delay
exceeds the period, regions carry negative slack and the synthesis reports it. The
present scaling silently compresses every budget so the total always fits,
hiding an infeasible constraint and making every contract depend on the worst
path in the design.

### FSM re-encoding

A machine's next-state logic is in the state flip-flop's fan-in, so cone seeding
co-locates it and no FSM anchor is needed. Its output logic is in the fanout and
is not co-located. This RFC takes the conservative rule: **only a machine wholly
contained in one region, including its output logic, is re-encoded.** No global
FSM analysis is reintroduced to raise coverage. Fanout-aware cone definitions
are left to a future RFC; phase 4 measures the coverage loss.

### Cross-boundary optimization after this RFC

The prohibitions above are easy to read as "nothing crosses a region boundary
again." That reading is wrong, and the distinction matters for future work.
Three different things cross boundaries, and this RFC treats them differently.

| Kind | Examples | Disposition |
| --- | --- | --- |
| Information | timing budget and electrical limits | **explicit** — boundary contracts, pillar 4 |
| Dataflow | constants, liveness, aliases, region ports | **frozen** — derived only from the Word graph |
| Semantic structure | CSE, sequential merging, resource sharing | **owner-confined** — retained inside one provisional owner, unavailable across owners |
| Physical and post-map | buffering, resynthesis, boundary repair, load fixing | **globally scheduled, owner-confined mutation** |

**Post-map is globally measured but not ownerless.** `engine::optimize_postmap`
passes the whole `MappedNetlist` to `closure::postmap` for full-design timing
and electrical measurement. A mutation remains inside one exact implementation
owner; immutable external nets and nets between different owners are frozen
optimization boundaries. Boundary repair receives one explicit directed-edge
owner, and affected owners are invalidated from durable provenance rather than
cell names or reconstructed hierarchy.

The stage is global in analysis and transaction ordering, not in write
authority.

**Why the semantic-structure row stops at the owner.** A cross-boundary
structural optimization is compensation for a partition that put the wrong
things in different regions. Two multipliers that should be shared but are not
co-resident represent a partitioning failure, not a missing pass. The owned
structural stage preserves the optimization for co-resident candidates without
creating a design-wide search dependency.

**Where that argument runs out.** Some optimizations are inherently global and
cannot be recovered by better partitioning:

- multibit register banking, which merges flip-flops scattered across arbitrary
  boundaries;
- design-level area and power tradeoffs, which is exactly the architecture solve
  recorded as a deliberate loss above;
- very high-fanout driver trees, which cross regions by construction — these
  are already handled by post-map buffering.

**Paths that restore cross-boundary reach without reopening the architecture.**
In increasing order of intrusiveness, and all compatible with the
no-false-invalidation invariant:

1. **Datapath-aware cone seeding** — place structures that should be shared in
   one region to begin with. This does not cross a boundary; it moves the
   boundary. Named above as the preferred remedy.
2. **Pairwise region optimization** — open two adjacent regions together for one
   joint optimization, deriving the result's identity from both. The dependency
   closure is those two regions, so locality holds.
3. **Within-synthesis epoch merging** — deterministic, deferred in Alternatives
   for lack of evidence rather than for a correctness objection.

What the invariant forbids is not crossing a boundary; it is depending on
design-wide state. An optimization whose scope is a bounded set of regions
remains admissible.

## Determinism, scalability, and QoR impact

**Determinism.** All ordering is a total order over stable anchors — never over
content hashes, worker count, completion order, address, or a previous synthesis.
Clean and incremental builds of identical RTL produce identical region topology
and identical output.

**Incrementality.** Guaranteed by construction: partition inputs are structural,
locally computed, and locally terminated (pillar 1); identity separates location
from content at every level (identity section); work is region-private
(pillar 3); dataflow has one frozen authority (pillar 4); budget has no global
normalizer (pillar 5).

**Scalability.** Serial work is criticality estimation (parallel by level), cone
claiming, a fixed number of matching rounds, deterministic commits of
owner-confined structural plans, timing-contract propagation, and assembly.
Expensive architecture discovery and Boolean mapping scale over final private
modules; the owned structural stage never searches across the whole design for
a shared candidate.

**QoR.** Not neutral.

*Expected gains* — flip-flops keep their fan-in cones, so a critical path is
optimized as one path inside one region instead of being cut at every register
boundary.

*Deliberate losses* — cross-region CSE; resource sharing and sequential
equivalence across provisional owners; design-level architecture solve; FSM
re-encoding for machines whose output logic is not co-resident; estimation
accuracy for fusible operator chains.

*Risks* — criticality estimate has no wirelength and no fusion awareness;
non-owner cones of shared logic have a cut path; frontier fragmentation may
produce more regions than the target on deeply chained logic.

**No switches.** No phase introduces a flag, environment variable, or Tcl control
selecting old versus new behavior. Each phase replaces its predecessor and
deletes it.

## Alternatives

**Fix the fingerprints only.** Removes total invalidation; leaves the serial
section, the fixpoint, and the barriers.

**A global fact layer over the value graph.** An earlier draft kept a
whole-module fact lattice plus an arbitrated global rewrite transaction.
Rejected: the arbiter is a serial section proportional to proposal count, and
every fact it computed is expressible as a boundary contract at three orders of
magnitude less scale.

**Pre-freeze affinity proposers for resource sharing and sequential
equivalence.** Held by an earlier draft under a "content-local" admission rule.
Rejected: the rule does not exclude a design-wide bucketing search, and any
partition input derived from a design-wide search makes region membership
globally dependent.

**Stopping coarsening at a design-wide region count.** Rejected: a local edit
could change the number of rounds executed and re-form every region. Local size
withdrawal plus a fixed round count achieves coarsening without a global
stopping condition.

**Deriving operation identity from `SourceSpan`.** Held by an earlier draft.
Rejected: absolute line and column shift when a line is inserted anywhere above,
and `linked_elaboration` copies spans verbatim so occurrences of a
multiply-instantiated module are distinguishable only by enumeration order. Both
are positional identity wearing a source-level disguise.

**Weakening the no-false-invalidation invariant for anonymous boundaries.**
Considered when unnamed intermediates lacked stable identity. Rejected in favour
of `OperationAnchorId`, since partition cuts land on unnamed intermediates
routinely rather than exceptionally.

**A design-level architecture solve.** Rejected: no `RegionContextKey` survives
an unrelated edit.

**Scoped optimization over a shared arena.** Rejected: a mask over shared mutable
state is not isolation.

**Bin-packing cones into regions.** Rejected: any global-sequence packing makes
each decision depend on all earlier ones.

**Merging cones that share fan-in.** Rejected: shared control networks would
merge into one giant region.

**Duplicating shared logic across partitions**, as US8261220 permits. Rejected:
single ownership is load-bearing for provenance and incremental repair.

**Feedback repartitioning from measured timing.** Rejected: cross-synthesis
feedback breaks reproducibility.

**Multi-machine distribution.** Out of scope; `ExecutionContext` is
single-machine. Region-private artifacts keep the door open.

**A generic placer.** Out of scope.

## Validation and rollout

**Phase 0a — stable provenance.** Carry syntax-path provenance from the frontend
onto lowered operations, and hierarchy occurrence provenance through
`linked_elaboration`. Define `OperationAnchorId` over the two plus operation
role. `SourceSpan` is reduced to diagnostics.
This is prerequisite infrastructure spanning the frontend, `opto-ir`, and
`opto-synth`, and it is on the critical path: frontier seeds, `BoundaryPortId`,
matching order, and the no-false-invalidation invariant all rest on it. It is
not a preliminary cleanup, and earlier drafts of this RFC underestimated it.
*Accept:* anchors are invariant under comment insertion, unrelated-line edits,
reordering of unrelated declarations, and the addition of a sibling instance;
anchors distinguish all occurrences of a multiply-instantiated module.

**Phase 0b — content-side identity.** Introduce `RegionRevision` and
`BoundaryValueRevision`. Promote `value_keys` to a crate-level index. Store
sorted key multisets in `SourceSnapshot`; delete `changed_entries`. Replace
Delete the process-global `WordRecipeCache`; reusable recipes and complete
regional plans are keyed by their actual local identities instead.
`RegionAnchorId` and `BoundaryPortId` are **not** introduced here: both are
defined in terms of cone seeds, and today's work-packed combinational regions
have no seed. They land with the partitioner in phase 1.
*Accept:* value and operation diffs no longer widen from arena displacement —
a one-line ibex edit reports changed entries proportional to the edit rather
than to the arena. Region-level invalidation precision cannot be accepted here,
because stable region identity does not exist until phase 1; mapped netlist
bit-identical.

**Phase 1 — partitioning.** Add `StructuralEstimateIndex` independent of
`ArchitectureDecisions`. Add arrival/required estimation and edge criticality.
Replace `partition_operations` with cone seeding, frontier promotion,
claim-based ownership, single-owner shared fan-in, and coarsening by local
matching with fixed rounds. Introduce `RegionAnchorId` and `BoundaryPortId`.
Delete the pre-freeze resource-sharing and sequential-equivalence searches.
Calibrate the size target.
*Measure and record:* the QoR cost of losing both optimizations, the coverage
cost of the conservative FSM rule, and the ranking error from fusion-unaware
estimation. These size future work; they do not reopen architecture decisions.
*Accept:* ownership invariant verified — every operation in the synthesis root
closure has exactly one owner; region count within the calibrated range on CVA6;
QoR regression within the stated threshold; **region-level
no-false-invalidation accepted here** rather than in phase 0, since stable
region identity first exists at this phase — a one-line edit must invalidate
only regions in the real dependency closure; determinism holds across worker
counts.

**Phase 2 — region-private IR.** Introduce one region-private Word mechanism for
the production path. Delete `DataflowScope`, the repeated
canonicalization calls, and the whole-arena rewrites. Assembly becomes
deterministic reconstruction.
*Prerequisite:* a working equivalence flow for the benchmark designs. yosys
cannot read ibex's SystemVerilog directly, so an sv2v conversion step is part of
this phase's setup rather than of the phase itself.
*Accept:* **equivalence with state and memory elements as cutpoints**, checked
per region, **plus** verification that the state element set and their
connectivity are unchanged. Bit-identity is not required and not expected, since
removing whole-module visibility necessarily changes the netlist. QoR regression
within a stated threshold that phase 3 is expected to recover; pre-mapping wall
time scales with core count.

**Phase 3 — timing contracts.** Add sparse timing/electrical boundary rows while
keeping the frozen Word graph as the sole dataflow authority.
*Accept:* feedback changes bounded regional optimization policy without
changing connectivity, ownership, or publication identity.

**Phase 4 — region-owned structural passes.** Establish provisional ownership,
then run `optimize_derived_fsms`, `share_equivalent_sequential_values`, and
target sequential preparation with complete candidate footprints confined to
one owner. Generated operations inherit ownership from stable source
provenance; no intermediate compaction or repartition is required. Rebuild and
seal the graph once before private architecture mapping. Muxed-arithmetic
sharing, multi-operand CSA/Wallace/Dadda, and fused arithmetic live only in the
final private module, catalog, and lowering path.
*Accept:* `plan_regions` contains only stage ordering; no structural candidate
crosses an owner; FSM coverage loss documented.
Note that re-encoding changes state encoding, so the phase 2 cutpoint method
does not apply here: re-encoded machines require sequential equivalence or an
explicit state-mapping check.

**Phase 5 — local budget.** Replace `allocate_path_budgets` with the absolute
arrival/required formulation. Report infeasibility instead of absorbing it.
*Accept:* an edit confined to one region leaves unrelated regions'
`RegionContextKey` reusable, measured through `IncrementalReuseMetrics`; an
infeasible period reports negative slack instead of silently compressed budgets.

`docs/architecture.md` is updated when phase 2 lands. RFC 0006 is amended, not
superseded: its post-freeze contract is unchanged, and this RFC supplies the
pre-freeze half it left open.
