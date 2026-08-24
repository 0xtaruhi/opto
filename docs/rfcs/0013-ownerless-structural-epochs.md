<!-- SPDX-FileCopyrightText: 2026 Zhengyi Zhang -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# RFC 0013: Ownerless structural epochs

- Status: proposed
- Author: Zhengyi Zhang
- Date: 2026-08-21
- Revised: 2026-08-24
- Supersedes: RFC 0007's provisional structural-owner protocol
- Amends: RFC 0011's execution model; semantic choices are independent of
  compilation shards

## Decision

Word is Opto's only retained logical-design topology. A synthesis epoch seals
that immutable semantic snapshot into a compact revision identity and derives a
`WorkGraph` of explicit tasks. Workers read private core/halo fragments and
return proof-carrying results. Only the coordinator may publish a new Word
generation.

The governing rule is:

> Design identity belongs to the semantic database, decomposition belongs to
> the scheduler, and mutation authority belongs to one transaction.

Consequently:

- no operation owns or inherits a mutable region-owner label;
- stable entity identity does not encode a dense row, region, shard, worker, or
  completion order;
- a compilation shard is a batching decision, not an optimization boundary;
- provenance explains origin but never grants mutation authority;
- a worker cannot mutate its input generation; and
- there is one synthesis path for every worker count and effort setting.

This RFC defines implementation contracts. It makes no fixed speedup,
utilization, memory-ratio, or QoR guarantee. Measurements supporting a specific
claim remain subject to the repository benchmark policy.

## Why structural ownership is removed

The retired path kept a dense owner column beside a mutable Word operation
arena. Every rewrite had to keep arena growth, owner inheritance, compaction,
and final partitioning synchronized. That coupling had three architectural
costs:

1. an execution partition became semantic mutation authority;
2. valid cross-partition candidates were rejected or needed owner repair; and
3. correctness depended on every transformation maintaining unrelated side
   state.

The replacement makes authority explicit in task footprints and transactions.
Scheduling can then change without changing semantics, candidate discovery, or
identity.

## Canonical representations

### Word snapshot

`WordModule` remains the complete logical authority. The ownerless path does
not retain a second whole-design cell/net database. Dense Word IDs are local to
one module generation and never cross a work or persistence boundary as
semantic identity.

### Stable identities

`DesignRevisionId` hashes canonical Word operations, memories, connections,
types, and stable source identities. `CellId` and `NetBitId` are deterministic
recipes derived from those semantic anchors. They are not rows in a duplicate
global topology.

The same entity therefore has the same stable identity independent of:

- worker count and executor assignment;
- region geometry and compilation-shard batching;
- task completion and publication order; and
- generation-local Word row numbering.

### Exact compact entity sets

Each work item stores its writable core and read-only halo as canonical entity
groups with sorted `(lsb, width)` ranges. Adjacent ranges are coalesced and
set union/difference remain exact. Scalar net identities are expanded only at
an interface that requires them.

This preserves bit-exact conflict detection without allocating one 32-byte
identity for every bit of the complete design.

## WorkGraph

One sealed epoch owns:

- an immutable `WorkDesign` containing the semantic revision and stable state
  cells;
- the immutable semantic `SynthesisRegionGraph`;
- one `WorkContext` per semantic item;
- stable work-item rows with exact core, halo, and cost estimates;
- predecessor and successor rows; and
- epoch-local compilation shards and coarse groups.

`WorkContext` identifies every external fact visible to the task: timing
scenario generation, target generation, and boundary contracts. A result is
accepted only when its item, program, context, revision, read closure,
replacement footprint, and coordinator-recomputed proof all match the sealed
row.

Rebatching may change only which ordered items share a packet. It cannot change
an item ID, context, candidate set, selected output, or result order.

## Execution

### Local packets

`WorkPacket` schema version 2 carries:

- stable task, shard, item, and context identities;
- the program ABI;
- semantic and region content generations;
- ordered item descriptions; and
- bounded work and memory estimates.

The local executor schedules packets through the common runtime and returns one
`WorkResult` per item. Nested parallel work uses the same execution budget and
cannot create an independent thread pool.

### Remote packets

Remote execution transports the serialized `WorkPacket` and
`WorkPacketResult` payload in a keyed opaque envelope. Retry is deterministic:
the task key selects the first compatible worker, each worker is attempted at
most once, and a fatal failure stops retry. Results and selected failures are
ordered by stable task key rather than completion time.

The remote interface does not invent storage discovery, placement, or network
policy. Those concerns belong to a concrete deployment.

### Determinism and bounds

Every potentially large workflow has a structural bound:

- local work is bounded by exact task scope;
- shard size and oversubscription are explicit scheduler parameters;
- fusion has a fixed combined-work limit;
- global candidate pricing uses a fixed round count;
- remote retry is bounded by the compatible worker set; and
- candidate enumeration and mapped correction retain their existing limits.

Functional behavior may not depend on allocator telemetry, RSS, task timing,
hash-map iteration, or host scheduling.

## Transactional publication

Workers propose immutable Word fragments. Publication performs one atomic
sequence:

1. sort fragments by stable `FragmentKey`;
2. assign dense Word slots in that order;
3. splice all fragments under one undo journal;
4. validate the complete provisional Word module;
5. derive each fragment's stable semantic boundary and exact footprint;
6. validate proof, base revision, unique delta identity, and pairwise-disjoint
   replacement sets; and
7. seal and publish the next compact revision.

Any error restores appended rows, operation replacements, and connect edits to
the exact pre-publication state.

`RewriteDelta` deliberately contains only the transaction facts not already
owned by Word:

```rust
pub struct RewriteDelta {
    id: RewriteDeltaId,
    footprint: RevisionFootprint,
    semantic: SemanticBinding,
    proof: EquivalenceCertificate,
}
```

Word validation is the topology check. Re-encoding the same changed fragment
as a second temporary cell/net IR would add another source of truth without
strengthening the transaction.

The current production structural publisher is static-wire coalescing. Other
structural producers must adopt this transaction boundary before they may run
as mutating ownerless workers. Read-only analysis and compilation workers use
the same revision, context, footprint, and proof checks but return immutable
artifacts instead of rewrite deltas.

## Multi-item workflows

### Fusion

Every adjacent semantic pair may nominate one bounded fusion item. Its writable
core is the exact union of the member cores; its halo is the union of member
halos minus that core. Proposals above the fixed work bound are rejected.

The overlap graph is colored deterministically into disjoint waves. Items in a
wave may execute concurrently because their ordinary member sets do not
overlap. A fusion task is still a proposal: the coordinator owns selection and
publication.

### Map, shuffle, reduce

Global analysis maps immutable work rows to keyed summaries, sorts them by
summary key and stable work-item identity, then reduces independent groups in
parallel. Reducers receive summaries, never structural mutation authority.

The design-wide architecture selector uses this workflow to gather compact
candidate summaries. A scheduling boundary therefore cannot remove a candidate
or create a second selector.

## Compile-once global choice selection

RFC 0011 remains authoritative for candidate semantics. In the ownerless
execution model:

1. candidate characterization runs through WorkGraph packets;
2. map/reduce gathers ordered per-region summaries;
3. four synchronous price rounds propagate over semantic successor rows;
4. each group selects by stable score and recipe tie-break;
5. disjoint fusion waves may jointly improve adjacent selections;
6. one `CompiledChoiceDesign` compiles each choice scope once; and
7. exact timing and bounded mapped correction consume the selected topology.

Price propagation is synchronous and bounded, so legitimate sequential
feedback cycles are supported without a DAG assumption. The previous round is
the complete input to the next round; worker count cannot change the result.

## Physical context

Physical fields are absent until Opto has a real placement, congestion,
parasitic, and interconnect model. Fabricated coordinates or wire estimates
would create false authority and are forbidden. Adding such a model extends
`WorkContext`; it does not select a second synthesis architecture.

## Implementation status

| Contract | Status |
| --- | --- |
| Delete provisional structural ownership and owner inheritance | Implemented |
| Compact semantic revision without duplicate whole-design topology | Implemented |
| Exact WorkGraph core/halo/context and deterministic rebatching | Implemented |
| Atomic Word fragment publication with stable boundary delta | Implemented for static-wire coalescing |
| Coordinator proof recomputation for compiled artifacts | Implemented |
| Bounded fusion waves and summary-only reduce | Implemented |
| Design-wide selection and compile-once mapping handoff | Implemented |
| Shared local/remote packet-result ABI and bounded retry | Implemented |
| Real physical context | Conditional on a physical model |
| Common delta protocol for every remaining structural producer | Not yet implemented |

The table distinguishes implemented behavior from architectural direction. A
phase label or RFC text is not evidence that a production path exists.

## Compatibility and persistence

This RFC intentionally invalidates derived regional cache records whose graph
identity encoded the retired owner or per-bit layout. The region local-key and
graph-revision domains use version 2. There is no legacy decoder because these
records are reproducible cache state, not a public interchange format.

Public Tcl behavior and the single `opto` executable are unchanged.

## Rejected alternatives

### Keep owners as debug provenance

Rejected. A structural owner column still requires inheritance and repair and
will inevitably be consulted as authority. Source provenance and transaction
footprints already provide the useful diagnostics.

### Make a cell/net DesignRevision the semantic authority

Rejected. Word contains types, memories, dynamic lvalues, four-state behavior,
and procedural semantics that a synthesis cell graph would have to duplicate
or lose. Production-scale evidence also showed that a whole-design bit-level
copy violates the intended memory shape.

### Retain both old and new pipelines

Rejected. A fallback would double the state space, hide incomplete cutover,
and make worker-count behavior architecture-dependent.

### Treat shards as semantic regions

Rejected. It makes candidate discovery and QoR depend on batching and prevents
safe rebatching or distributed scheduling.

### Require fixed performance gates for architectural completion

Rejected. Runtime and QoR depend on inputs, target data, host configuration,
and implementation maturity. Reproducible experiments are evidence for a
specific claim, not part of the semantic contract.

## Review checklist

Every ownerless production path must answer:

1. Which immutable semantic revision does the worker read?
2. What exact entities may it replace and what is its complete read closure?
3. Which context generation and algorithm ABI produced the result?
4. What proof authorizes acceptance?
5. Can rebatching, worker count, or completion order change identity or output?
6. What deterministic bound limits private work, shared memory, and retry?
7. Does any helper retain a second representation of canonical topology?

## References

- [RFC 0006: Region-parallel synthesis and deterministic mapping](0006-region-parallel-synthesis.md)
- [RFC 0007: Timing-driven partitioning](0007-timing-driven-partitioning.md)
- [RFC 0011: Compile-once global choice synthesis](0011-compile-once-global-choice-synthesis.md)
