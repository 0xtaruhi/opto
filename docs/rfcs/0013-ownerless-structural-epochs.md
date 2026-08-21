<!-- SPDX-FileCopyrightText: 2026 Zhengyi Zhang -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# RFC 0013: Ownerless structural epochs and hierarchical compilation shards

- Status: proposed
- Author: Zhengyi Zhang
- Date: 2026-08-21
- Implementation: unimplemented. The current tree still performs provisional
  structural ownership over a mutable global `WordModule`, propagates owner
  rows across structural rewrites, and rebuilds a final frozen region graph.
- Supersedes: the provisional-owner, owner-confined global-mutation, and final
  owner-freeze contracts of RFC 0007.
- Amends: RFC 0011's compilation-shard execution model. Semantic decision
  groups and analytical regions remain independent of scheduling shards;
  compilation shards become epoch-local and may be deterministically regrouped
  without changing semantic identity.

## Summary

Opto shall remove structural ownership as mutable synthesis provenance.
Operations shall no longer acquire, inherit, lose, or repair a region owner
while a shared `WordModule` is being rewritten.

The replacement architecture has four concepts:

1. `DesignRevision` is the immutable, canonical cell/net graph for one logical
   or mapped generation.
2. `WorkGraph` is an epoch-local graph of explicit semantic work items and a
   hierarchy-independent batching of those items into coarse and fine
   compilation shards.
3. `WorkContext` is the versioned logical, timing, electrical, and eventually
   physical environment visible at one work-item boundary.
4. `RewriteDelta` is a complete, proof-carrying replacement produced from a
   private worker IR and committed transactionally into a new design revision.

The central rule is:

> **Design identity belongs to the database, decomposition belongs to the
> scheduler, and mutation authority belongs to one transaction.**

None of those domains may stand in for another. In particular:

- a stable entity ID does not encode a compilation shard;
- a compilation shard is not a semantic optimization boundary;
- a semantic decision group is not a storage allocation unit;
- provenance does not authorize mutation; and
- a worker never mutates the design revision from which its task was derived.

The global database uses a simple high-level cell and bit-net model. Dense SSA
remains available as a worker-private representation, where topological
renumbering and compaction are local implementation details. A semantic
`WorkItem` declares one writable core, a read-only halo, exact boundary bits,
and a `WorkContext`. A compilation shard batches independent work items for one
executor; the worker returns one keyed result per item. The
coordinator validates non-overlapping write sets, stable boundaries,
equivalence, and revision compatibility before publishing a new revision.

Parallel execution has three levels:

1. coarse compilation shards provide machine-level distribution and data
   locality;
2. fine compilation shards provide an oversubscribed task pool and work
   stealing; and
3. algorithm-level threading accelerates expensive work within one task when
   the fine-grain ready queue cannot occupy all workers.

This follows the useful public architectural properties of Cadence Genus: a
hierarchy-independent timing-driven partition, two distributed granularities,
algorithm-level threading, effort-aware load balancing, and global analytical
selection. It does not claim knowledge of proprietary Genus internals.

RFC 0011 remains authoritative for compile-once global choice synthesis. This
RFC supplies the missing execution and mutation substrate: decision groups own
semantic alternatives, analytical regions own global pricing context, and
compilation shards own bounded work only. A shard boundary may never remove a
candidate or prohibit a semantically valid optimization.

## Motivation and compatibility evidence

### The current failure is architectural

The current structural preparation path performs this sequence over one mutable
global `WordModule`:

```text
global normalization
provisional region partition
construct StructuralOwnershipProvenance
owner-confined FSM and dataflow rewrites
owner-confined sequential sharing
owner-confined target preparation
additional owner-confined dataflow rewrites
final partition constrained by provisional owners
verify that every live operation retained one frozen owner
```

`StructuralOwnershipProvenance` is a dense side column aligned with the mutable
operation arena:

```rust
owners: Vec<Option<RegionRowId>>
```

A transformation records the old arena length, appends operations, identifies
source operations, proves that their owner rows agree, and extends the owner
column over the appended suffix. The protocol assumes all of the following:

- the operation arena and owner column remain exactly synchronized;
- every transform identifies the complete source footprint;
- generated operations form a contiguous appended suffix;
- no transform compacts or reorders the arena without remapping the column;
- no cross-region candidate is admitted accidentally;
- unreachable operations are distinguished correctly from ownerless live
  operations; and
- the final partition never splits a surviving provisional owner atom.

These are not independent bugs. They are consequences of storing scheduling
membership as mutable semantic provenance beside an arena whose topology and
indices are intentionally changed by optimization.

`LocalOperationProvenance` exposes the same structural pressure at a smaller
scope. It maintains dense rows, inherits appended operations from SSA operands,
tracks replacement suffixes, and consumes compaction remaps. That protocol is
reasonable inside a private temporary IR. It is not a suitable authority for a
shared, long-lived design graph.

### Dense global SSA amplifies the ownership problem

The global Word representation currently uses position-addressed values and
operations. Transformations may append a replacement and redirect an earlier
consumer to it. Restoring the topological arena invariant then requires one
stable global remap, and every side database must consume exactly that remap.

This creates three unnecessary couplings:

1. logical identity is coupled to arena position;
2. connectivity edits are coupled to topological compaction; and
3. every derived fact is coupled to the lifetime of the mutable arena.

The canonical global representation does not need any of those couplings. A
netlist database can keep a boundary net stable while replacing its driver.
Consumers continue to reference the same net and do not require a whole-design
use rewrite. Dense SSA is still valuable for local algorithms, but its IDs
should never escape the private task that owns the arena.

### RFC 0007 chose isolation by freezing semantic reach

RFC 0007 correctly identified the serial pre-freeze pipeline and moved
expensive lowering and mapping into private regional modules. It also made
several choices that now block the required architecture:

- the region is the only optimization unit;
- structural candidates spanning provisional owners are unavailable;
- feedback never changes the partition;
- design-level architecture selection is rejected to preserve regional cache
  identity; and
- multi-machine distribution is out of scope.

Those rules make ownership load-bearing. Once a region is both the semantic
boundary and scheduling unit, every generated operation must retain exactly one
region identity and every transformation must preserve the partition.

The implementation experience invalidates that design. The owner protocol has
required repeated fixes across provenance, active-value reconstruction,
bit-exact publication, mapped-cell attribution, dead operations, and SSA
compaction. More importantly, freezing semantic reach at a scheduling boundary
places a permanent QoR ceiling on cross-boundary equivalence, sharing,
retiming, and datapath selection.

This RFC retains the valid parts of RFC 0007:

- stable source anchors and independent content revisions;
- exact bit-level boundary identity;
- private optimization IRs;
- deterministic task keys and ordered error selection;
- versioned MMMC boundary context;
- content-addressed reuse; and
- deterministic artifact assembly.

It removes only the assertion that those properties require a durable
per-operation owner.

### RFC 0011 supplies the semantic/global layer

RFC 0011 already separates three scopes:

| Scope | Authority |
| --- | --- |
| `ArchitectureDecisionGroup` | owns one coupled semantic choice and its sealed interface |
| `ArchitectureDecisionRegion` | owns read-only analytical timing and reconvergence context |
| `CompilationShard` | owns scheduling, storage locality, and bounded work only |

That distinction is correct and is adopted here. This RFC does not introduce a
second architecture-selection system. `WorkGraph` is the executable graph of
RFC 0011 compilation shards. `RewriteDelta` is the common mutation protocol by
which selected decision-group candidates, local optimization, bounded
multi-item repair, and mapped closure publish changes.

This RFC changes one RFC 0011 restriction: compilation-shard identity is not
durable semantic identity. Timing feedback and observed task cost may
deterministically split, merge, or regroup compilation shards within one
synthesis run. They may not silently change a decision group's sealed
interface, candidate set, or semantic identity.

### The required parallelism is data-model parallelism

Pass-level parallel loops over a flat mutable netlist are insufficient. They
eventually serialize on discovery, mutation, compaction, timing refresh, or
publication.

Cadence's public Genus technical brief describes the relevant architectural
properties:

- partitioning may slice across source hierarchy;
- the distribution is timing-driven;
- load is balanced by expected optimization effort rather than gate count;
- coarse clusters of roughly 100K or more instances distribute across
  machines;
- finer clusters of roughly 10K or more instances provide a second distributed
  level;
- algorithm-specific threading runs within each machine; and
- an adaptive scheduler selects the useful level of parallelism.

The particular thresholds are historical product guidance, not Opto ABI.
Opto's work units shall be controlled by measured cost, memory, cut quality,
and available execution resources. The architectural requirement is the
three-level decomposition, not a fixed instance count.

### Requirements

The replacement must satisfy all of the following simultaneously:

1. owner propagation failures must become unrepresentable;
2. expensive structural work must begin only after a small, parallelizable
   design-sealing phase;
3. scheduling decomposition must not remove semantic opportunities;
4. global timing and PPA coordination must remain possible;
5. task outputs must be deterministic across worker counts and completion
   orders;
6. commits must not introduce an O(number of cells) serial section;
7. exact bit connectivity and source-facing provenance must remain available;
8. incremental reuse must depend on real content and context, not shard
   geometry; and
9. the first implementation must run efficiently on one machine while keeping
   a serializable task boundary suitable for later distribution.

## Goals

1. Delete mutable structural ownership and every owner-inheritance API from the
   production synthesis path.
2. Replace the mutable global Word optimization stage with immutable design
   revisions and private task-local transformation.
3. Start fine-grained parallel work immediately after linking, alias sealing,
   root discovery, and structural atom formation.
4. Preserve the compile-once global candidate and analytical selector of RFC
   0011 independently of scheduling shards.
5. Support bounded multi-item structural work without global mutable state.
6. Provide hierarchical, effort-aware execution that can use multiple machines,
   multiple task workers per machine, and inner algorithm threads.
7. Keep the canonical domain model small: cell/net design, work decomposition,
   analysis context, and transaction.
8. Preserve bit-identical output across supported worker counts.
9. Bound resident memory by immutable shared pages, task-local working sets, and
   explicit candidate limits.
10. Make global coordination proportional to boundaries, decision summaries,
    or changed pages rather than total cell count.

## Non-goals

- Reconstructing or claiming the proprietary internal implementation of Genus.
- Retaining Top-K complete regional netlists or complete mapped plans.
- Building a whole-design e-graph.
- Making thread count, host count, or task completion order a synthesis input.
- Adding placement awareness before a real placement and interconnect model
  exists.
- Requiring a distributed executor in the first implementation phase.
- Allowing speculative workers to publish directly into shared mutable state.
- Preserving dense global Word IDs across revisions.
- Treating source hierarchy as a mandatory optimization or scheduling boundary.
- Guaranteeing that every multi-item candidate is profitable or selected.

## Detailed design and invariants

### Four architectural concepts

The production path has four conceptual objects. Additional packed tables may
exist as private representations, but they do not become independent authority
domains.

| Concept | Canonical contents | Lifetime |
| --- | --- | --- |
| `DesignRevision` | stable cell/net semantics and exact connectivity | immutable generation |
| `WorkGraph` | shard membership, adjacency, cost, and task dependencies | one synthesis epoch |
| `WorkContext` | logical halo and versioned analysis boundary | one analysis generation |
| `RewriteDelta` | read/write footprint, replacement fragment, bindings, proof | one transaction |

`ArchitectureDecisionGroup`, `ArchitectureDecisionRegion`, `ChoiceGraph`, and
`CompiledMapping` retain the contracts assigned by RFC 0011. They are semantic
or compiled-analysis structures layered over a `DesignRevision`; they are not
additional mutation authorities.

### Canonical design database

The canonical logical database is a stable high-level cell and bit-net graph.
Conceptually:

```rust
pub struct DesignRevision<L> {
    revision: DesignRevisionId,
    cells: PagedArena<CellId, Cell<L>>,
    nets: PagedArena<NetBitId, NetBit>,
    ports: PagedArena<PortId, Port>,
    state: PagedArena<StateId, StateElement>,
    memories: PagedArena<MemoryId, Memory>,
}

pub struct Cell<L> {
    id: CellId,
    kind: L,
    inputs: CompactNetBitRange,
    outputs: CompactNetBitRange,
    source: SourceOrigin,
}

pub struct NetBit {
    id: NetBitId,
    driver: Option<Endpoint>,
}
```

`L` distinguishes logical Word-level operations from mapped target cells. The
logical and mapped databases use the same revision and transaction principles;
they need not use one physical arena or one cell-kind enum.

The conceptual model is intentionally small:

- a cell consumes and produces exact net bits;
- a net bit has at most one logical driver after sealing;
- state and memory are explicit graph elements;
- constants and resolution behavior are explicit, canonical nodes or values;
- hierarchy occurrence and source origin are metadata, not containment; and
- source hierarchy may be crossed by `WorkGraph` formation.

Multi-driver procedural or tri-state semantics must be lowered into explicit
resolution structures before the revision is sealed. Alias equivalence is
resolved once into canonical net identity. These are design-construction
requirements, not synthesis optimizations.

#### Stable identity, packed storage

Stable identity does not require pointer-rich objects. A `CellId` or `NetBitId`
is an opaque typed handle independent of vector position and compilation shard.
The database may store records in immutable packed pages and resolve handles
through a compact page directory.

Page compaction may relocate a record without changing its entity ID. No public
or cross-stage identity is derived from a page number, dense row, address,
worker number, or shard ID.

Source entities derive identity from the stable hierarchy-occurrence and
syntax-path anchors already required by RFC 0007. Derived entities use a
deterministic semantic-recipe namespace and local role. One admissible form is:

```text
DerivedEntityId = H(
    entity-kind domain,
    sorted stable source entities and anchors,
    transformation ABI,
    semantic recipe identity,
    local result role
)
```

The exact encoding is implementation ABI. Its required properties are typed
separation, deterministic construction, collision checking, and independence
from unrelated design revisions, shard geometry, worker assignment, and task
completion order.

#### Global Word is retired as mutation authority

`WordModule` remains a supported private algorithm representation. A task may
import its core and halo into a compact, topologically ordered local Word
module, perform append-heavy SSA construction, compact it, and remap local side
tables.

No local `ValueId` or `OpId` may be stored in `DesignRevision`, `WorkGraph`, a
persistent cache key, another task, or a published artifact. The import binding
and exported boundary binding are the only bridges:

```rust
pub struct LocalImportBinding {
    source_cells: Box<[(CellId, word::OpId)]>,
    boundary_inputs: Box<[(NetBitId, word::ValueId)]>,
    boundary_outputs: Box<[(word::ValueId, NetBitId)]>,
}
```

Compaction remaps therefore remain local. They cannot corrupt global ownership,
identity, timing rows, or source publication.

### Identity and authority domains

Six typed domains are deliberately distinct:

| Domain | Meaning | Stability |
| --- | --- | --- |
| `EntityId` | one canonical logical or mapped entity | stable across storage movement |
| `DesignRevisionId` | one immutable complete generation | changes on accepted mutation |
| `ArchitectureDecisionGroupId` | one sealed coupled semantic choice | stable while its semantic closure is unchanged |
| `WorkItemId` | one explicit revision-local read/write or analysis scope | one compatible WorkGraph generation |
| `CompilationShardId` | one scheduling unit in a `WorkGraph` | epoch-local |
| `RewriteDeltaId` | one proposed transactional replacement | task-local until selection |

No implicit conversion exists between them. In particular, the following type
is forbidden:

```rust
struct CellId {
    shard: CompilationShardId,
    local: u32,
}
```

Encoding a shard in the entity ID would turn deterministic regrouping into
object migration, ID replacement, and another family of remap side tables.

### Design sealing

The pre-shard serial surface is deliberately small. Before the first
`WorkGraph`, Opto may perform only work needed to construct a valid immutable
graph:

1. parse and elaborate modules and processes;
2. resolve symbols and hierarchy occurrences;
3. canonicalize exact net and alias identity;
4. lower multi-driver and resolution semantics;
5. discover synthesis roots and reachability;
6. form indivisible combinational SCC, state, memory, and externally constrained
   atoms; and
7. validate the single-driver, type, width, and state-boundary invariants.

FSM recognition, resource sharing, sequential equivalence, arithmetic
architecture selection, target preparation, Boolean optimization, and mapping
are not sealing work.

Sealing itself shall exploit module/process parallel elaboration, segmented net
construction, parallel or segmented union-find, level-parallel reachability,
and parallel SCC algorithms. A small deterministic root publication is
permitted. A whole-design serial rewrite or compaction is not.

### WorkGraph

`WorkGraph` is a derived execution plan over one immutable design revision:

```rust
pub struct WorkGraph {
    design: DesignRevisionId,
    epoch: SynthesisEpochId,
    items: Box<[WorkItem]>,
    shards: Box<[CompilationShard]>,
    coarse_groups: PackedRows<CompilationShardId>,
    item_predecessors: PackedRows<WorkItemId>,
    item_successors: PackedRows<WorkItemId>,
}

pub struct WorkItem {
    id: WorkItemId,
    key: TaskKey,
    core: EntitySet,
    halo: EntitySet,
    context: WorkContextKey,
    kind: WorkItemKind,
    estimated_work: u64,
    estimated_memory: u64,
}

pub struct CompilationShard {
    id: CompilationShardId,
    key: TaskKey,
    items: CompactWorkItemRange,
    estimated_work: u64,
    estimated_memory: u64,
}
```

`WorkItem` is a typed execution row over an already defined semantic or
analytical scope. Its `kind` may refer to an RFC 0011 decision group, a fixed
logic rewrite scope, a proof bucket, a compile range, or a bounded repair
proposal. The complete read and replacement footprint is defined before the
item is assigned to a compilation shard.

A compilation shard is only a batch of work items chosen for data locality,
memory bounds, and execution overhead. Combining two items in one shard does
not authorize a joint rewrite. Splitting a batch does not split an item. If two
opportunities must be optimized jointly, they are one semantic decision group
or one explicitly admitted fusion work item before scheduling.

Reverse entity-to-item and item-to-shard columns are permitted inside an
immutable `WorkGraph` for fast lookup. They are derived, epoch-local, and never
propagated across mutations. They are not structural provenance.

When a worker creates local operations, nothing is appended to that reverse
column. The new operations exist only in the private task result. If the delta
is accepted, they enter a new `DesignRevision`; the next `WorkGraph` derives
membership from that revision normally.

#### Work-item core and halo

Every structural work item has two scopes:

- `core` is the complete write footprint authorized for that work item; and
- `halo` is a versioned read-only neighborhood used for pattern discovery,
  reconvergence, observability, and accurate local modeling.

The halo is not a copied semantic authority. Imported halo nodes retain exact
source entity and boundary net identities. A work-item delta may depend on them
but may not replace them.

A context containing only scalar arrival and required times is insufficient
for logic optimization. The halo preserves exact nearby structure; the
`WorkContext` supplies global analysis facts that cannot be reconstructed from
the halo.

#### Work-item formation and hierarchical batching

`WorkGraph` construction has two separate decisions.

First, transformation-specific analysis forms work items. Decision groups,
fixed-logic rewrite scopes, proof buckets, compile ranges, and repair proposals
use their own semantic rules. Item formation may cross hierarchy and may use
connectivity, timing criticality, reconvergence, shared support, state or memory
coupling, and real physical affinity. It may not use worker count, host count,
measured executor speed, or current shard batching. Equal semantic and analysis
inputs therefore form equal work items.

Second, the scheduler batches complete work items into two execution
granularities:

1. fine shards batch enough independent work items to amortize dispatch while
   remaining numerous enough to load-balance worker threads and bound private
   memory; and
2. coarse groups aggregate adjacent fine shards for machine placement, transfer
   locality, and distributed scheduling.

The scheduler should normally expose at least eight ready fine shards per
available worker on a sufficiently large design. This is a utilization target,
not an input to semantic work-item formation. Worker count may affect how work
items are batched and how shards are assigned to executors; it may not change
the candidate set, rewrite footprint, or design result.

Semantic work-item formation may consider:

- exact read and replacement closure;
- edge bit width;
- timing criticality across active MMMC lanes;
- reconvergence and shared-support affinity;
- state and memory coupling; and
- placement, congestion, and interconnect affinity when a real physical model
  exists.

Compilation-shard batching considers only execution properties:

- predicted optimization work;
- predicted peak task memory;
- immutable-page and target-context data locality;
- dependency-ready concurrency; and
- historical measured task cost keyed by compatible transformation ABI and
  target context.

One illustrative work estimate is:

```text
work =
    a * operation_count
  + b * active_bit_count
  + c * candidate_count_estimate
  + d * critical_arc_count * active_scenario_count
  + e * measured_compatible_runtime
```

The coefficients and feature set are scheduling-policy ABI. They may change
batching and executor assignment but not item membership or semantic output.
Gate or operation count alone is not sufficient because proof, cut, rewrite,
and mapping cost vary by orders of magnitude between equally sized cones.

#### Parallel deterministic formation

The current stable-order cone-claiming algorithm shall not become the serial
front of the new execution model. Any transformation that forms partition-like
fixed-logic work items uses bounded synchronous rounds:

1. atoms compute local weight and timing features in parallel;
2. seeds publish candidate labels;
3. each unassigned atom computes its preferred label from the previous round;
4. conflicting claims resolve by a stable tuple of criticality, cut gain, and
   entity identity;
5. disjoint deterministic matches coarsen in parallel; and
6. bounded refinement improves semantic cut quality.

After item formation, batching performs a separate deterministic parallel
multilevel grouping over complete items. It may optimize measured load balance,
memory, and locality without changing any item's core, halo, candidate set, or
result key.

All decisions in one round read the previous round and publish keyed rows. No
completion-order state is visible. Fixed deterministic tie-breaks preserve
bit-identical results across worker counts.

The exact item former and shard batcher are not fixed by this RFC. Any
implementation must prove bounded rounds, deterministic output, hierarchy
independence, work balance, memory bounds, semantic independence from batching,
and absence of an O(number of entities) serial claim loop.

#### Adaptive regrouping

Observed execution cost and current-run timing may change compilation-shard
batching without changing work items or semantic decision groups:

- a long-pole fine shard may split its independent work-item batch;
- adjacent shards may merge their batches if the memory bound permits;
- coarse groups may be reassigned to machines for load or data locality; and
- an oversized inner algorithm may receive additional threads instead of being
  repartitioned.

Timing or structural analysis may also nominate a new fusion work item. That is
a semantic proposal with its own stable footprint and admission rule; it is not
an authority acquired by merging compilation shards. Candidate nomination and
admission are deterministic functions of design and analysis revisions, not of
the current shard batching.

Regrouping produces a new `WorkGraph` epoch over the same `DesignRevision`, or
is folded into the `WorkGraph` constructed for the next revision. It does not
rename entities, mutate candidate catalogs, or invalidate a cache merely
because a shard range changed.

### WorkContext

`WorkContext` is analysis, not semantic truth. Exact connectivity remains in
the design revision.

```rust
pub struct WorkContext {
    design: DesignRevisionId,
    scenario_generation: ScenarioGeneration,
    target_fingerprint: TargetFingerprint,
    placement_generation: Option<PlacementGeneration>,
    inputs: Box<[BoundaryInputContext]>,
    outputs: Box<[BoundaryOutputContext]>,
    exceptions: Box<[BoundaryException]>,
    physical: Option<PhysicalClip>,
}
```

The context may contain, per relevant MMMC lane:

- arrival and required time;
- slew and capacitance limits;
- load and drive models;
- clock relation and uncertainty;
- false-path and multicycle exceptions;
- power-domain and reset information;
- boundary activity and power price;
- placement, floorplan, congestion, and interconnect estimates; and
- interface timing models returned by predecessor shards.

Every row is versioned by the exact design, scenario, target, and physical
generation on which it depends. A stale context may be used to generate a
speculative candidate, but that candidate cannot commit until repriced or
revalidated against the accepted generation.

Context extraction resembles the useful public idea behind Genus clips: a
subset is optimized with the timing and physical conditions of the complete
block. Opto initially supplies logical and MMMC timing context. Physical fields
remain absent until backed by a real placement and interconnect engine.

### Private task execution

An ordinary task performs the following steps:

```text
resolve DesignRevision + WorkItem row
materialize the item's writable core and read-only halo
import a private WordIR and exact boundary binding
run a complete local optimization bundle
produce zero or more compact candidates
select or price candidates under the supplied context
export one keyed RewriteDelta or immutable compiled artifact per work item
release all private IR and scratch memory
```

The task bundle is deliberately larger than one compiler pass. A barrier after
every canonicalization, rewrite, lowering, and mapping pass would preserve the
same synchronization structure as the current global pipeline. One task should
run all profitable local passes to a bounded local convergence point before
global coordination.

### RewriteDelta

```rust
pub struct RewriteDelta<L> {
    id: RewriteDeltaId,
    base: DesignRevisionId,
    task: TaskKey,
    reads: EntitySet,
    replaces: EntitySet,
    fragment: NetlistFragment<L>,
    boundary: BoundaryBinding,
    semantic: SemanticBinding,
    response: InterfaceResponse,
    proof: EquivalenceCertificate,
}
```

`reads` contains every entity whose structure or fact influenced the result.
`replaces` contains the complete mutation footprint. It must equal or be a
validated subset of the originating work item's core. A fusion or reduce item
declares its larger explicit footprint before shard batching and execution.

`BoundaryBinding` maps fragment endpoints to stable `NetBitId` values. External
consumers continue to reference those nets after commit, so a producer
replacement does not require rewriting the complete fanout.

`SemanticBinding` records public source roots, source results, observable
inputs, and any state mapping directly at the replacement interface. It is not
reconstructed later by selecting one source operation from a provenance set.
Helper nodes inside the fragment require no durable source-operation owner.

#### Validation

A delta is admissible only if:

1. its base revision is the accepted revision or an explicitly supported
   ancestor for which the complete read set is unchanged;
2. every read and replacement entity exists in that revision;
3. its replacement footprint is authorized for the task wave;
4. its external boundary net identities and types match the sealed interface;
5. all internal references resolve within the fragment or its declared
   boundary;
6. it introduces no illegal combinational cycle;
7. its interface response covers every active analysis lane;
8. its provenance and state mapping cover the public semantic interface; and
9. the required combinational or sequential equivalence proof succeeds.

Construction-by-equivalence may provide a compact certificate. General
combinational replacement uses boundary CEC. State encoding or cycle-changing
work requires an explicit state relation and sequential equivalence; it may not
hide behind ordinary combinational boundary equality.

#### Commit

Tasks in an ordinary wave have disjoint replacement footprints. Their deltas
therefore build new copy-on-write pages in parallel. The commit procedure is:

1. validate every delta against the same accepted generation;
2. select a deterministic non-conflicting subset where alternatives exist;
3. construct all replacement pages and boundary-index edits in parallel;
4. validate the provisional complete generation;
5. publish one new root revision; and
6. discard the provisional generation atomically on any failure.

No worker publishes directly into the accepted design. No accepted revision is
partially updated.

The only serial publication may be a bounded root-table installation and
revision seal. It must not visit every cell, net, cut, mapped instance, or task
result.

### Three task forms

#### LocalTask

A `LocalTask` executes one or more independent work items from a fine shard.
Each item writes only its own core, reads only its declared closure and context,
and returns a separately keyed result. It is the default form for:

- local canonicalization and observability reduction;
- bounded Boolean rewriting and functional reduction;
- local FSM and control optimization;
- operator lowering;
- cut enumeration and matching;
- local architecture candidate characterization; and
- local mapped repair.

#### FusionTask

A `FusionTask` executes one previously admitted fusion work item whose core
spans several ordinary item footprints. It supports work that is structurally
local but crosses their ordinary semantic scopes or current scheduling batches:

- cross-boundary CSE;
- resource sharing across mux branches;
- critical-path resynthesis;
- bounded retiming or state-cone optimization when separately authorized;
- FSM next-state/output co-optimization; and
- boundary buffering or load repair.

Fusion tasks are not serialized globally. Their entity-footprint overlap graph
is colored or matched deterministically into disjoint waves. Independent fusion
items may be batched into arbitrary compilation shards and run in parallel.

A fusion request is a proposal, not authority. It identifies the required
footprint, expected gain, cost, context, and proof regime. Admission occurs
before task execution or before expensive materialization, according to a
bounded deterministic policy.

#### ReduceTask

A `ReduceTask` implements globally discovered but distributable work as
map/shuffle/reduce rather than a flat pass over mutable state. Examples include:

- sequential-equivalence signatures;
- global structural or functional hashing;
- register-bank candidate discovery;
- high-fanout classification;
- multi-item candidate families; and
- global timing-price reductions.

For example, sequential equivalence may execute as:

```text
parallel local signature construction
deterministic shuffle by signature
parallel proof within independent buckets
deterministic conflict resolution
parallel RewriteDelta construction and commit
```

The reducer manipulates signatures, summaries, and conflict edges. It does not
become a serial mutation authority over the complete design.

### Global analytical selection

This RFC adopts RFC 0011 rather than introducing per-shard greedy QoR.

Critical `ArchitectureDecisionGroup` candidates expose compact recipes and
characterized interface responses. One design-wide read-only model selects
candidates under timing, area, power, electrical, and eventually physical
constraints. Candidate characterization is parallel; global propagation and
selection operate over decision summaries and the boundary timing graph.

Conceptually, selection minimizes an objective such as:

```text
sum(area(group, choice)) + power_price * sum(power(group, choice))
```

subject to design-wide arrival/required and electrical constraints. A bounded
Lagrangian or timing-price iteration may update boundary prices while each
group independently reprices its candidates. The RFC does not require one
specific numerical solver.

The following remain mandatory:

- a scheduling shard cannot split an indivisible decision group, Boolean
  choice class, or correlated multi-output alternative;
- a shard boundary cannot remove a candidate from the global selector;
- candidates are compact recipes and response rows, not cloned complete
  netlists;
- only selected or exceptionally reopened candidates are materialized;
- exact STA validates the selected implementation; and
- mapped closure repairs bounded physical/electrical violations rather than
  rediscovering architecture globally.

This division preserves both parallelism and global QoR: semantics and pricing
span shards, while expensive construction and compilation remain sharded.

### Hierarchical execution and elastic scheduling

The runtime exposes one logical task interface to local and distributed
executors:

```rust
pub trait SynthesisExecutor {
    fn execute(
        &self,
        packets: Box<[WorkPacket]>,
        policy: ExecutionPolicy,
    ) -> Result<Box<[WorkResult]>, RuntimeError>;
}
```

`WorkPacket` is self-contained and serializable. It names one compilation
shard's ordered work-item rows, their content-addressed design pages, target and
scenario fingerprints, contexts, algorithm ABI, deterministic item keys, work
estimates, and memory estimates. A remote executor transfers or resolves only
missing immutable pages and returns separately keyed item results.

#### Level 1: coarse distribution

Coarse groups amortize transport, target-catalog residency, and process-level
overhead. They are assigned to machines by work, memory, and data locality, not
source hierarchy or cell count alone.

#### Level 2: fine task scheduling

Fine shards populate a work-stealing queue within and, where supported, across
machines. Heavy tasks launch early, but result ordering and failure selection
remain keyed by stable `TaskKey`.

The number of ready fine tasks should normally exceed the worker count by a
substantial factor. Work stealing changes executor assignment only; it never
changes task semantics or selected output.

#### Level 3: algorithm threading

Tasks are moldable rather than assigned a static inner width. The scheduler
allocates cores according to the ready queue:

- when enough fine tasks are ready, tasks run mostly single-threaded;
- as the ready queue drains, expensive tasks receive additional inner workers;
- memory-heavy tasks may remain single-threaded even when cores are free; and
- nested parallel work uses the same resource budget and cannot oversubscribe
  the machine.

This replaces a fixed square-root split between outer and inner parallelism.
The scheduler continuously chooses the useful level from ready work, measured
cost, memory pressure, and algorithm scalability.

### Incremental reuse

Compilation-shard identity is not a persistent cache identity. A result is
reusable when its real dependency key matches, for example:

```text
subgraph revision
+ decision-group revision
+ boundary-value revisions
+ WorkContext revision
+ target/scenario fingerprint
+ transformation and mapping ABI
```

Changing a shard range does not invalidate a candidate whose structural and
analysis closure is unchanged. Conversely, retaining the same shard ID does not
authorize reuse after a real input changes.

The no-false-invalidation invariant applies to cached facts and artifacts:

> A cached result may be invalidated only by a dependency represented in its
> key or declared read closure.

It does not require scheduling geometry to remain fixed. An edit or measured
long pole may create a different `WorkGraph` while reusing every compatible
candidate, proof, cut, match, and response row.

### Determinism

Determinism is defined over semantic inputs, not schedule topology:

> Equal RTL, constraints, libraries, physical inputs, tool ABI, and synthesis
> policy produce identical output independently of supported worker count,
> machine assignment, task completion order, and work stealing.

The implementation enforces this through:

- stable typed entity and task identities;
- synchronous partition rounds;
- stable tie-breaks for claims, matches, candidate selection, and conflicts;
- immutable worker inputs and keyed worker outputs;
- deterministic reduction orders for floating-point values;
- target-time-quantized timing comparisons;
- disjoint commit waves;
- one deterministic selected failure; and
- output assembly from stable semantic order rather than completion order.

Adaptive shard regrouping is deterministic because it consumes versioned
current-run measurements and stable policy. Reproducibility does not require
keeping a poor scheduling partition forever.

### Provenance and publication

Source provenance is separated from scheduling authority.

Public implementation records are derived from the sealed semantic interface
of a decision group or rewrite delta:

- source result is an explicit semantic root;
- source inputs are resolved per semantic operand or exact boundary bit;
- generated helper cells have optional diagnostic origin but no durable
  structural owner;
- mapped artifacts carry an immutable `FragmentFootprint`; and
- publication uses exact stable boundary bits, never a net-wide ownership
  inference.

`FragmentFootprint` is containment metadata for one immutable artifact. It may
be used for invalidation, rollback, accounting, and replacement. It is not
propagated operation by operation during construction.

The current append-only mapped-slot and tombstoned-footprint model is compatible
with this RFC and should be generalized rather than discarded.

### Analysis and timing

Global timing remains a design-wide analysis. Partitioning does not make scalar
boundary context the only timing truth.

Each applicable work item or analytical decision region may return a compact
interface timing model. Compilation shards only batch those rows. The global
timing graph propagates arrivals, required times, slews, loads, and prices over
boundary levels. A committed delta invalidates only its affected internal
model, boundary rows, and global forward/backward cones.

Full timing construction and global propagation must themselves use parallel
levels, paged graph data, and incremental dirty worklists. If global STA scans
and updates every arc serially after every shard batch, the design has merely
moved the serial bottleneck.

### Memory and capacity

Immutable revisions use shared copy-on-write pages. A task materializes only:

- its writable core;
- its bounded halo;
- active context rows;
- bounded candidate and proof state; and
- its output fragment.

Multiple tasks do not clone the complete design. Distributed workers resolve
content-addressed pages lazily and may retain immutable target catalogs and
design pages across tasks.

Halo growth, fusion size, candidate count, proof count, cut count, and inner
parallel scratch all have explicit deterministic limits. A task that exceeds a
hard capacity reports a structured capacity error or is split by policy; it may
not silently fall back to an ownerless global pass.

### Required invariants

The implementation shall encode and test these invariants:

1. **Revision immutability.** No worker mutates its input design revision.
2. **Stable entity identity.** Entity identity is independent of dense storage,
   topology order, worker, and compilation shard.
3. **Shard non-authority.** WorkGraph membership never grants semantic identity
   or removes a candidate.
4. **Batching invariance.** A work item has the same inputs, candidates, result
   key, and accepted output regardless of which compilation shard batches it.
5. **Exact footprint.** Every delta declares complete read and replacement
   sets.
6. **Boundary preservation.** External stable net bits and their types are
   preserved or changed only through an explicitly larger semantic interface.
7. **Proof before publication.** A replacement provides the equivalence regime
   required by its semantic effect.
8. **Disjoint ordinary commits.** Intersecting replacement footprints never
   publish in one wave.
9. **No partial generation.** Failure before publication leaves the accepted
   revision unchanged.
10. **Context versioning.** Analysis rows name every generation that affects
   their validity.
11. **Deterministic selection.** Completion order, worker count, and host
    assignment do not influence accepted output.
12. **Bounded serialization.** The coordinator performs no serial work
    proportional to the total cell or proposal count on the hot path.
13. **Local-ID confinement.** Dense private Word, choice, cut, and mapping IDs
    never escape their declared immutable generation.

## Current-tree cutover

### Structural preparation

The current conceptual path:

```rust
let provisional = partition::build(module, policy)?;
let mut ownership = StructuralOwnershipProvenance::new(module, &provisional)?;

optimize_derived_fsms_in_regions(module, &mut ownership, ...)?;
optimize_owned_priority_dataflow(module, &mut ownership)?;
share_equivalent_sequential_values_by(module, ..., ownership.owners(), ...)?;
mapping.publish_owned_preparation(module, ..., &mut ownership)?;

let final_partition = partition::build_with_ownership(module, policy, &ownership)?;
ownership.verify_frozen(module, &final_partition)?;
```

becomes:

```rust
let design0 = DesignRevision::seal(linked_word)?;
let work0 = WorkGraph::build(&design0, &analysis0, policy)?;
let deltas = structural_epoch.execute(&design0, &work0, runtime)?;
let design1 = design0.commit(deltas)?;
let work1 = WorkGraph::build_or_update(&design1, &analysis1, policy)?;
```

The following production mechanisms are deleted:

- `StructuralOwnershipProvenance`;
- `claim_since` and `claim_range`;
- generated-operation owner inheritance;
- provisional-owner atoms as inputs to the final partition;
- `verify_frozen` as an ownership relation proof; and
- whole-module compaction used to resynchronize owner-indexed side tables.

Their correctness role moves to revision, footprint, boundary, and equivalence
validation.

### Region graph

The current `SynthesisRegionGraph` is not discarded immediately. Its useful
columns evolve into `WorkGraph` and exact boundary tables:

- region adjacency becomes shard adjacency;
- exact bit flows remain canonical boundary obligations;
- operation/memory reverse lookup becomes an epoch-local derived index;
- region timing estimates become work and context estimates;
- durable region identity is removed from scheduling membership; and
- semantic candidate identity moves to RFC 0011 decision groups.

`RegionRowId` may remain temporarily as a generation-stamped row ID during the
cutover. The final API shall use terminology that distinguishes semantic
decision groups, analytical regions, compilation shards, and published
implementation fragments.

### Runtime

The current runtime already provides valuable pieces:

- one deterministic worker pool;
- stable `TaskKey` ordering;
- cancellation;
- weighted task launch;
- nested worker limits; and
- deterministic result and error order.

The following changes are required:

1. replace fixed square-root outer/inner allocation with elastic moldable task
   scheduling;
2. expose task work and memory estimates to admission;
3. support explicit fine/coarse task graphs;
4. make large artifact installation parallel rather than using a serial commit
   callback per row;
5. define serializable `WorkPacket` and `WorkResult` formats; and
6. add a distributed executor behind the same interface only after the local
   backend satisfies determinism and capacity qualification.

### Regional mapping and mapped closure

The existing mapped generation already uses append-only mapped slots,
tombstoned regional footprints, immutable external nets, timing-feedback
epochs, and transactional mapped repair. Those mechanisms are compatible with
the new substrate.

The cutover replaces mapped structural owner inference with exact fragment
containment and boundary bindings. Global analysis remains allowed; mutations
publish through `RewriteDelta` or a mapped specialization with the same
revision and footprint invariants.

### RFC 0011 integration

RFC 0011 remains the only authority for:

- architecture decision groups and candidate catalogs;
- design-wide analytical selection;
- the shared choice graph;
- compile-once cuts, truth functions, and target matches;
- bounded exact-model correction; and
- final choice-aware mapping.

This RFC provides:

- the stable source design on which groups are discovered;
- hierarchy-independent compilation shards;
- private candidate characterization tasks;
- transactional materialization;
- elastic hierarchical scheduling; and
- exact dependency keys independent of shard geometry.

If the two RFCs conflict, this RFC controls structural ownership, design
revision, compilation-shard lifetime, and mutation. RFC 0011 controls semantic
candidate representation and global selection.

## Determinism, scalability, and QoR impact

### Determinism

The architecture strengthens determinism by removing completion-sensitive
shared mutation and dense-arena provenance repair. Every worker reads an
immutable generation and produces a keyed value. Partition rounds, proposal
admission, conflict resolution, numerical reductions, and final publication use
stable orders.

Dynamic load balancing is permitted because executor assignment is not a
semantic input. Adaptive shard regrouping is permitted because its policy and
measurements are versioned and deterministic.

### Scalability

The intended large-design critical path is:

```text
parallel seal
bounded parallel WorkGraph construction
coarse/fine/inner parallel task execution
parallel page construction
small revision publication
parallel incremental analysis
bounded global summary selection
```

The coordinator may perform work proportional to coarse groups, shard
boundaries, dirty pages, decision summaries, or timing frontiers. It may not
perform hot-path work proportional to every cell or every generated proposal.

Small designs skip unnecessary levels. If one fine shard is sufficient, the
same transaction path runs without distribution overhead. If many fine tasks
are ready, inner parallelism collapses to avoid oversubscription. If the ready
queue drains, remaining heavy tasks expand elastically.

### QoR

The architecture does not treat compilation shards as optimization boundaries.
QoR is preserved through four mechanisms:

1. decision groups capture coupled semantic alternatives before scheduling;
2. the design-wide analytical selector prices alternatives globally;
3. halos retain exact nearby structure for local discovery; and
4. fusion and reduce tasks recover bounded multi-item structural reach.

No claim is made that partitioning is QoR-neutral. Poor atoms, halos, fusion
limits, response models, or physical estimates can still hide profitable work
or misprice it. The difference is that these are explicit policy and modeling
errors, not a type-level prohibition caused by owner membership.

### Incrementality

Replacing durable region identity with dependency-keyed artifacts may reduce
coarse region-cache hits when an early implementation first lands. It also
prevents a scheduling change from invalidating semantically unchanged
candidates and allows reuse below the shard level.

Incremental reuse is measured at source entity, decision group, proof,
candidate, choice class, cut/match, interface response, and immutable page
granularity. Region-count stability is not an acceptance metric.

### Risks

#### DesignDB conversion cost

A Word-to-DesignDB adapter may initially add one O(number of entities) parallel
construction. It is accepted only as a cutover mechanism. The eventual frontend
shall seal directly into the stable database or stream into its page builders.

#### Halo explosion

Reconvergent control, high fanout, or wide datapaths can make a naive halo
approach duplicate substantial read-only structure. Halo policies require
explicit depth, support, bit, and resident-memory bounds. Shared immutable pages
should be referenced rather than copied where an algorithm can consume them
directly.

#### Fusion conflicts

Too many overlapping fusion proposals can reduce concurrency or create a large
conflict graph. Proposal count, footprint radius, selected degree, and waves are
bounded. Discovery and scoring remain parallel; the conflict reducer operates
over compact proposal edges.

#### Global timing becomes the new long pole

Exact global STA can dominate after local synthesis scales. Timing graph
construction, level propagation, lane execution, and dirty-cone updates are
therefore part of the qualification, not assumed background work.

#### Distributed transport dominates

Remote execution is useful only when coarse work amortizes immutable page and
target-context transfer. The local backend lands first. Remote qualification
must measure transfer, cache residency, serialization, retry, and worker-loss
cost separately.

#### Global selector coupling

A design-wide selector creates real global dependencies. An unrelated local
edit may legitimately change a global price and select a different candidate.
Characterization caches remain reusable; the cheap selector reruns. The design
does not preserve an old choice merely to claim a cache hit.

## Alternatives

### Continue repairing StructuralOwnershipProvenance

Rejected. More validation can detect desynchronization earlier but cannot
remove the requirement that every structural transform update one dense side
column correctly. It preserves the coupling between mutable arena order,
provenance, partition membership, and final publication.

### Encode the shard in every entity ID

Rejected. It makes scheduling decomposition durable identity. Splitting or
regrouping work then requires object migration, reference rewrites, cache-key
changes, and another complete remap protocol.

### Freeze one partition for the complete synthesis

Rejected. It simplifies authority but turns one early cost model into a
permanent QoR and load-balance decision. It cannot respond to measured long
poles, critical paths, or different algorithm granularities.

### Use one flat stable netlist with parallel passes

Rejected. Stable IDs improve correctness but do not create distributed
mutation, bounded working sets, hierarchical scheduling, or global/local
coordination. Whole-design pass barriers remain the scalability limit.

### Use a global mutable transaction arbiter

Rejected. A central arbiter that validates and applies every cell-level proposal
serially has work proportional to proposal count and becomes the long pole. This
RFC commits disjoint immutable fragments and page replacements in parallel.

### Prohibit all transformations spanning work items

Rejected. Scheduling boundaries are resource decisions. Cross-boundary
equivalence, sharing, critical-path restructuring, high-fanout repair, and
design-wide architecture selection are not all recoverable by a better initial
partition.

### Retain complete Top-K results per shard

Rejected consistently with RFC 0011. It multiplies lowering, Boolean, mapping,
timing, and resident memory. Compact recipes, equivalence classes, cuts,
matches, and response rows retain useful alternatives without cloned designs.

### Fully asynchronous conflicting commits

Deferred. Optimistic MVCC could increase utilization, but conflict retry and
floating analysis generations make deterministic behavior and bounded memory
harder to reason about. The first architecture uses immutable epochs and
disjoint deterministic commit waves. Later evidence may justify a bounded
asynchronous executor without changing the design database or delta contracts.

### Partition according to worker count

Rejected. Worker count may affect executor assignment and elastic inner width,
not semantic task results or persistent identity. The same policy input must
produce bit-identical output across supported worker counts.

## Validation and rollout

No phase adds a permanent production fallback or user-visible architecture
switch. During cutover, the displaced path may remain as a test-only oracle on
the development branch. A phase deletes the old production mechanism when its
acceptance criteria pass.

### Phase 0: baseline and observability

Record, per stage and task:

- wall and CPU time;
- active and idle worker time;
- ready-queue depth;
- predicted and measured task work;
- peak resident and scratch memory;
- partition and context construction time;
- commit and analysis propagation time;
- cache hits at every reusable granularity;
- WNS, TNS, area, power, cell count, and sequential count; and
- equivalence and diagnostic results.

The qualification suite includes at minimum OpenTitan HMAC, CSRNG, EDN, OTBN,
and representative interconnect blocks; Ibex; CVA6; PULP AXI; and at least one
design with one million or more logical operations after sealing.

*Accept:* reproducible one-, four-, and sixteen-worker baselines exist; stage
and task time sum coherently to end-to-end wall and CPU time; peak memory is
measured; current owner failures have permanent regression cases; and a test
can deliberately rebatch identical work items without changing their results.

### Phase 1: immutable revision and transaction substrate

Introduce the stable cell/net `DesignRevision`, Word import/export bindings,
copy-on-write page construction, `RewriteDelta`, exact boundary validation, and
atomic revision publication. Run the complete design as one task initially.

Move one representative combinational rewrite and one mapped repair through the
common delta protocol. Keep dense Word IDs strictly local.

*Accept:* the new revision reproduces the reference connectivity; both example
transformations pass CEC and rollback tests; failed publication leaves the
accepted revision byte-identical; no persistent table stores a local Word ID;
and the adapter's time and memory are measured separately.

### Phase 2: ownerless structural epochs

Build the first fine-grain `WorkGraph`. Move owner-confined FSM, priority
dataflow, sequential sharing, and target-preparation work into private tasks.
Each returns a `RewriteDelta`; no task mutates the input design.

Delete:

- `StructuralOwnershipProvenance`;
- `claim_since` and `claim_range`;
- provisional-owner inheritance;
- `build_with_ownership`; and
- `verify_frozen`.

*Accept:* all live structural work is represented by exact task footprints and
boundaries; owner-loss regressions are impossible through the production API;
OpenTitan, Ibex, CVA6, and PULP AXI pass equivalence; deterministic output holds
across worker counts and qualified shard rebatched layouts; and no heavy
whole-design optimization remains before the first WorkGraph.

### Phase 3: hierarchical local execution

Add deterministic parallel WorkGraph formation, coarse/fine task scheduling,
elastic inner parallelism, parallel page construction, and incremental analysis
updates. The distributed executor interface exists, but the only required
backend is local.

Initial qualification targets for a design with at least one million logical
operations are:

- at least 6x end-to-end synthesis speedup at sixteen workers over one worker;
- at least 70% average worker utilization during the main synthesis epochs;
- coordinator, partition-publication, and commit time below 15% of total wall
  time;
- at least eight ready fine tasks per worker during scalable phases unless the
  dependency graph exposes less parallelism; and
- peak resident memory no greater than 1.5x the qualified one-worker path.

These numbers are initial acceptance targets and may be revised only with
checked benchmark evidence and an RFC amendment, not silently weakened in
implementation.

### Phase 4: bounded multi-item reach

Implement fusion tasks, deterministic overlap coloring, and at least one reduce
workflow. The first required use cases are cross-boundary CSE or sequential
equivalence and critical-path fusion spanning two ordinary work items.

Integrate task-local candidate characterization with RFC 0011's global selector.
Compilation-shard boundaries may not change candidate discovery or selection.

*Accept:* selected multi-item cases demonstrate a measured QoR benefit;
independent fusion tasks execute concurrently; conflict order does not change
the result; proposal and memory bounds hold; and no global mutable structural
pass is reintroduced.

### Phase 5: compile-once global choice integration

Complete the RFC 0011 choice graph and compiled mapping cutover on the new
revision/work/delta substrate. Characterize candidates in parallel, run global
analytical selection over compact summaries, and materialize only selected or
exceptionally reopened candidates.

*Accept:* RFC 0011's runtime and QoR gates pass; scheduling shards cannot remove
an alternative; characterization caches survive compatible shard regrouping;
and exact STA validates the selected topology without regenerating Boolean or
mapping structure.

### Phase 6: distributed and physical execution

Add a remote executor using the same `WorkPacket` and `WorkResult` contracts.
Add physical context only with a real placement, congestion, parasitic, and
interconnect model.

*Accept:* worker loss and retry preserve deterministic output; transfer and
serialization are not the long pole; coarse distribution improves wall time on
qualified large designs; and logical/physical correlation is measured against
the accepted implementation flow.

### QoR gates

Every phase runs the same functional and QoR suite. Until stronger checked
targets exist, the initial regression limits against the accepted production
baseline are:

- zero functional or sequential-equivalence regressions;
- geometric-mean mapped area and critical delay regression no greater than 2%;
- no individual qualified design regresses more than 5% in either mapped area
  or critical delay without an accepted waiver and root-cause record;
- no new electrical violation; and
- no hidden loss of state, memory, clock, reset, or path-exception semantics.

RFC 0011 may define stronger design-specific QoR targets; the stronger target
controls.

### Architectural review checklist

Before each phase is accepted, review shall answer:

1. What immutable revision does every worker read?
2. What exact entities may it replace?
3. What is its complete read and boundary closure?
4. What equivalence or state relation authorizes publication?
5. Which facts are canonical and which are derived context?
6. Can task, worker, or shard ordering affect identity or selection?
7. What is the maximum private and shared resident memory?
8. Does any coordinator loop visit every cell or proposal serially?
9. Can a scheduling boundary remove a semantic candidate?
10. Which benchmark demonstrates both the runtime and QoR claim?

If those questions cannot be answered from types, invariants, and measured
evidence, the phase is not ready for the production path.

## References

- Cadence, [Genus Synthesis Solution Technical Brief](https://login.cadence.com/content/dam/cadence-www/global/en_US/documents/tools/digital-design-signoff/genus-synthesis-solution-tb.pdf).
- [RFC 0006: Region-parallel synthesis and deterministic mapping](0006-region-parallel-synthesis.md).
- [RFC 0007: Timing-driven partitioning and region-private optimization](0007-timing-driven-partitioning.md).
- [RFC 0011: Compile-once global choice synthesis](0011-compile-once-global-choice-synthesis.md).
