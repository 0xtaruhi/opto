<!-- SPDX-FileCopyrightText: 2026 Zhengyi Zhang -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# RFC 0011: Compile-once global choice synthesis

- Status: proposed
- Author: Zhengyi Zhang
- Date: 2026-08-14
- Revised: 2026-08-14, after the measured QoR decomposition recorded under
  Motivation
- Implementation: Phase 1a (functional reduction) and Phase 1b (sequential
  excess) are implemented and measured, and the unconditional post-map area MFS
  sweep is removed. Every other phase is unimplemented.

## Summary

Opto shall replace repeated Boolean construction, independently mapped regional
proposals, and area-first post-map recovery with one compile-once synthesis
architecture:

1. discover bounded architecture decision groups whose candidates may replace
   several coupled operations, and evaluate them from target-characterized
   response tables;
2. select one candidate per group with one design-wide analytical timing/PPA
   model;
3. lower only the selected candidate of each group into one compact Boolean
   subject that retains equivalent structures as choices;
4. reduce that subject once by bounded simulation-guided SAT sweeping, so
   functionally equivalent nodes collapse to one node and the structurally
   different survivors become choices rather than duplicated logic;
5. enumerate bounded cuts, truth functions, and target matches once over that
   choice graph;
6. perform timing-feasible mapping and area recovery by changing compact choice,
   cut, and match IDs, without rebuilding Boolean logic; and
7. expose the selected compact implementation directly to exact MMMC timing
   and bounded closure, then seal that same topology as the published mapped
   netlist.

The central rule is:

> **Compile structure once; choose implementations many times.**

Normal timing refinement may reprice an existing candidate, cut, or cell match.
It may not restart Word optimization, bit lowering, Boolean rewriting, cut
enumeration, truth computation, or library matching. One exact-timing failure
may exceptionally select and compile a different already-discovered candidate
for a bounded set of decision groups; it never regenerates candidates or
recompiles the design.

This RFC does not retain several complete mapped plans per region. A
microarchitecture candidate is a compact semantic recipe plus a characterized
response. A Boolean alternative is an equivalence edge in one shared choice
graph. A mapping alternative is a packed cut/match record. Only the selected
topology becomes an implementation.

Acceptance of this RFC supersedes every existing Opto decision that restricts
architecture selection to one region, forbids a design-wide read-only selector,
or requires independent lowering and cover for every structural proposal.
There is no compatibility mode. The cutover removes the displaced production
path and updates the architecture contract in the same series.

## Motivation

### The current runtime is architectural, not a missing micro-optimization

A release profile of the public Ibex SKY130 flow motivates this RFC. The
measurements below are a local profile snapshot, not yet a checked-in benchmark
result:

| Work | Approximate wall time | Share of 17.1 s | Observation |
| --- | ---: | ---: | --- |
| Word/Boolean preparation and alternative construction | 9.77 s | 57% | repeated proposal lowering, rewriting, and cover preparation |
| Initial Liberty mapping | 0.74-0.80 s | 4-5% | not the dominant cost |
| Post-map area MFS | 3.70 s | 22% | recovered about 0.35% area |
| All remaining work | about 2.9 s | 17% | frontend, publication, timing, and reporting |

The profile says that making one rewrite lookup a few nanoseconds faster cannot
reach five seconds. Even deleting the principal Boolean optimization caused a
large area regression and still did not establish a five-second flow. The
required change is to stop compiling equivalent implementations independently
and to stop using a full-netlist post-map search to recover area that mapping
should have selected initially.

Small experiments reinforce that conclusion:

- a raw 65,536-entry truth lookup did not materially reduce end-to-end time;
- runtime four-input NPN canonicalization made the flow slower;
- a shared runtime recipe cache added synchronization cost;
- reducing MUX rewrite rounds saved little time or lost roughly 2-2.5% area;
- disabling the main Boolean optimization made the result roughly 18% larger.

These experiments do not reject lookup tables, NPN classes, or structural
rewriting. They reject adding them to an architecture that repeats the expensive
surrounding work.

### Measured QoR evidence separates the runtime thesis from the quality thesis

The runtime profile above says nothing about quality, and the first revision of
this RFC assumed without measurement that retained choices were the dominant
quality mechanism. They are not. The measurements below are local single-run
snapshots, not checked-in benchmark results, and they were produced from:

- Opto `9dc2f6d`, `cargo build --release --locked --bin opto`, `--threads 8`,
  `synth_effort high`;
- Ibex `c6edaa4060b1a3cd27fda928058db4f0ee3d24bd`, top `ibex_core`;
- Liberty `sky130_fd_sc_hd__tt_025C_1v80.lib`, SHA-256
  `ec0e1067a35c8bf20b11e58d1e8ac53326067e4dac84a125cc1b917a3518d0d9`;
- Yosys 0.67 (`2d1509d1b`) with its bundled ABC;
- one host, no constraints, area-unconstrained objective.

End-to-end mapped area:

| Flow | Total | Combinational | Sequential | Cells | Sequential cells |
| --- | ---: | ---: | ---: | ---: | ---: |
| Opto | 80,846.3 | 54,210.7 | 26,635.5 | 8,956 | 1,006 |
| Yosys + ABC | 76,789.9 | 51,037.7 | 25,752.2 | 9,636 | 972 |
| Opto excess | +4,056.4 | +3,173.0 | +883.3 | | +34 |

The combinational gap was then decomposed by an ablation that holds the design,
the mapper, and the library fixed. Every row consumes one identical pre-ABC
netlist written by `synth -top ibex_core -flatten -noabc; dfflibmap`, and every
row maps it with the same `&nf`, so the only variable is the
technology-independent optimization in front of the mapper. Sequential area is
25,752.2 in every row because `dfflibmap` runs before the ablation:

| Technology-independent script before `&nf` | Combinational area |
| --- | ---: |
| none | 64,081.5 |
| `&fraig -x` | 60,640.7 |
| `dc2` | 54,194.5 |
| `resyn2` | 53,872.9 |
| `&fraig -x; dc2` | 52,263.9 |
| `&fraig -x; dc2; &dch -f` (the Yosys default) | 51,672.1 |
| *Opto, for reference (its own subject and its own mapper)* | *54,210.7* |

Three conclusions follow, and they redirect this RFC:

1. **Opto's existing rewriting is already at `dc2` quality.** 54,210.7 against
   54,194.5 is a 0.03% difference. A better rewrite atlas, more NPN classes, or
   a wider cut policy cannot be the source of the remaining gap.
2. **Functional reduction is the largest single missing mechanism.** Adding
   `&fraig -x` in front of `dc2` removes 1,930.6, or 3.6% of combinational
   area, which is 61% of Opto's total combinational excess. Opto has no
   simulation-guided SAT sweeping, no functional node merging, and no
   equivalence-class construction anywhere in the synthesis path; its only
   structural sharing is the constructor-level hash consing in `LogicGraph`.
3. **Structural choices are worth 1.1%, not the headline.** Adding `&dch -f`
   removes a further 591.8. Choices remain worth building, both for quality and
   because they are the representation that makes functional reduction
   non-destructive, but they cannot be the primary quality justification for the
   compile-once cutover.

Two supporting measurements bound the alternative explanations:

- Re-strashing Opto's own mapped netlist and remapping it with the complete ABC
  script produces 54,533.6 combinational area against Opto's 54,210.7, while
  reimplementing the register enables that Opto had absorbed into `edfxtp_1`
  cells. Opto's cut enumeration, matching, and exact-area recovery are therefore
  not the deficit; the subject handed to them is.
- Raising `MAX_CUTS_PER_NODE` from 32 to 64 moved the same flow from 80,846.3 to
  81,057.7. Cut capacity is not the binding constraint, so the cut-count policy
  in this RFC is a runtime and memory decision rather than a quality decision.

The sequential excess has a separate and simpler cause, recorded under
Sequential cell selection and register identity.

### Area-first architecture repair is the wrong main loop

Cadence publicly describes a traditional approach that first favors area and
then incrementally changes timing-critical architectures as runtime-inefficient
and susceptible to local optima. Its published Genus model instead considers
multiple datapath microarchitectures and solves an analytical model across the
critical datapath regions of the design. This RFC adopts that separation of
responsibility, not any claim about Genus internals:

- semantic microarchitecture selection is design-wide and analytical;
- Boolean and cell selection retain alternatives until mapping;
- exact STA validates the selected implementation;
- mapped closure performs bounded physical/electrical repair, not architecture
  discovery.

The Genus material is motivation and prior art. Opto's types, algorithms,
determinism rules, and benchmark results remain its own contract.

### ABC demonstrates the useful granularity of retained alternatives

ABC does not obtain its speed by retaining several complete netlists and
running a full mapper on each. Its published work instead combines compact AIG
storage, structural choices, bounded priority cuts, precomputed rewrite data,
and mapping-time area recovery. Equivalent intermediate networks contribute
choices to one mapping problem; a small fixed cut set controls work and memory.

The lesson for Opto is precise:

- merge functionally equivalent nodes before retaining anything, because a
  duplicate is not an alternative;
- retain equivalent nodes, not cloned designs;
- retain cut and match records, not cloned mapped regions;
- precompute design-independent facts;
- make mapping select across structural alternatives; and
- recover area with mapping reference counts while timing constraints are still
  explicit.

The first item is the one the measurements above rank highest, and it is the
one the first revision of this RFC omitted. ABC's `&fraig` and the fraiging
inside `&dch` are the same machinery viewed twice: proving two nodes equal
either deletes one of them or records them as one equivalence class. Opto needs
both readings, and it needs the proving engine before it needs the class
representation.

Supergates are an optional target-catalog technique. They are not the first
milestone and are not required by this RFC.

## Goals

1. Reduce the reference Ibex SKY130 end-to-end median to at most 5.0 seconds in
   the default release flow on its recorded reference host.
2. Improve the accepted public-suite geometric-mean runtime by at least 3x,
   rather than tuning only Ibex.
3. Preserve or improve exact mapped area, setup/hold timing, electrical
   legality, and equivalence against the accepted baseline.
4. Close the measured external combinational-area gap on the reference Ibex
   SKY130 case. The accepted target is combinational area no worse than
   51,700 with the same inputs and library, which is the measured
   `&fraig -x; dc2; &dch -f` point and a 4.6% reduction from the current
   54,210.7. Each contributing mechanism reports its own measured share, so a
   phase that lands its representation without landing its area is not
   accepted.
5. Reduce the sequential excess to at most 100 area units and zero surplus
   registers on the same case. The mechanism is not yet known; see Phase 1b for
   the measurements that ruled out register duplication and sequential cell
   form.
6. Make every irreversible semantic or structural choice timing-aware, retain
   it as an alternative until timing-aware mapping, or prove it cannot worsen
   the objective.
7. Bound runtime and memory linearly in the selected subject size, the number
   of active timing lanes, and fixed candidate/cut limits.
8. Preserve bit-identical output across supported worker counts.
9. Preserve decision-group- and shard-local incremental compilation even though
   final selection uses design-wide read-only analysis.
10. Keep semantic decision scope independent of scheduling and storage shards so
    parallel decomposition cannot remove an optimization alternative.

## Non-goals

- Claiming a globally optimal discrete solution. The analytical selector is a
  bounded deterministic heuristic over a non-convex problem.
- Building an e-graph or alternative network for the whole RTL design.
- Retaining Top-K complete region covers or mapped netlists.
- Running exact full-design STA for every candidate.
- Reconstructing logic from a mapped critical cone.
- Repartitioning from timing feedback.
- Adding a second mapper, a fallback synthesis path, or user-visible algorithm
  switches.
- Adding placement awareness without a real placement and interconnect model.
- Cycle-changing retiming, state re-encoding, new clock-gating topology, and
  physical multi-bit-flop grouping. The initial decision-group interface keeps
  the sequential boundary and cycle latency fixed. Later RFCs may extend that
  interface instead of hiding those transformations in a combinational recipe.

## Design overview

```text
validated Word IR + constraints + target
                  |
                  v
       architecture decision-group discovery
                  |
                  v
     target-characterized response lookup
                  |
                  v
       design-wide analytical selection
     area + power + early/late timing + load
                  |
                  v
       lower selected group candidates once
                  |
                  v
   bounded simulation-guided SAT sweeping
   merge proved-equal nodes, class the rest
                  |
                  v
       one shard-partitioned choice graph
                  |
                  v
  compile cuts + truth + cell matches once
                  |
                  v
 global timing prices + choice-aware mapping
       feasibility, area-flow, recovery
                  |
                  v
       selected compact implementation
                  |
                  v
          exact incremental MMMC STA
                  |
                  v
       bounded exact-timing correction
       1. reselect compiled cut/match records
       2. if structurally required, compile one
          bounded decision-group repair batch
                  |
                  v
         exact MMMC accepted/best-known
                  |
                  v
 bounded size/buffer/clone/pin-swap closure
                  |
                  v
         seal the same topology once
                  |
                  v
               reports
```

There are two different choice domains:

| Domain | Alternative representation | Selection point |
| --- | --- | --- |
| Architecture decision group | semantic recipe graph plus characterized response | design-wide analytical selector |
| Boolean structure and cells | equivalence classes plus packed cut/match records | timing-driven mapper |

They must not be conflated. A carry-lookahead versus ripple implementation is a
semantic architecture decision with a word-pin response model. Two equivalent
factorizations of a Boolean cone are structural choices selected by cut-based
mapping. The first is chosen before bit lowering; the second survives through
mapping.

### Decision groups, decision regions, and compilation shards

Three scopes have different authority and must never share an identity domain:

| Scope | Purpose | May change semantics or candidate freedom? |
| --- | --- | --- |
| `ArchitectureDecisionGroup` | one coupled discrete architecture decision | yes, within its sealed interface |
| `ArchitectureDecisionRegion` | timing/reconvergence context for jointly priced groups | no mutation; owns analytical context |
| `CompilationShard` | parallel scheduling, storage, and cache locality | no |

A decision group is an indivisible semantic atom. A decision region is formed
from complete groups and intervening fixed logic using path criticality,
reconvergence, boundary width, fanout coupling, and sequential endpoints; it may
cross source hierarchy. A compilation shard may combine several decision
regions or split unrelated work inside one large region, but it may not split a
decision group, a Boolean choice class, or a correlated multi-output
alternative.

The design-wide timing graph spans every region and shard. Region boundaries
therefore reduce storage and parallel work but never turn an imported timing
summary into the only evidence available to the global selector. Shard creation
uses a deterministic bounded partition policy and cannot depend on worker count.

Timing feedback never repartitions the design. If exact timing requires
structural repair, it reopens only already-sealed decision groups and preserves
their region and shard identities. A repeatedly bad boundary is benchmark
evidence for changing the next partition-policy ABI, not permission to mutate
the current run's ownership.

These scopes are semantic contracts, not a requirement to allocate one Rust
object for every noun in this RFC. A decision group is a first-class semantic
row because candidates and invalidation refer to it directly. A decision region
may be a derived range over group IDs plus analytical adjacency, and a
compilation shard may be a deterministic range table over existing arenas. The
implementation shall use distinct typed IDs wherever accidental interchange is
invalid, but it shall not build parallel object hierarchies merely to mirror the
terminology above.

## Representation and ownership

### Representation minimality

The compile-once architecture has three long-lived authoritative owners:

| Owner | Canonical contents |
| --- | --- |
| candidate catalog | decision groups, candidate recipes, sealed interfaces, and characterized responses |
| choice graph | Boolean nodes, equivalence classes, roots, ownership, and shard ranges |
| compiled mapping | bounded cuts, truth identities, target matches, and response arcs |

Selection uses transient indexed arrays over those owners. Publication seals
the selected topology directly into the synthesis artifact. There is no
parallel family of `Plan`, `Row`, `Record`, `JournalRecord`, and `Outcome`
objects carrying the same fields through successive stages.

A named Rust type is justified only when it enforces at least one of these
properties:

- an identity, unit, generation, or state that must not be mixed with another;
- independent ownership, mutation authority, lifetime, or rollback scope;
- a versioned persistence or ABI boundary; or
- a compact representation whose layout changes asymptotic work or memory.

Grouping constructor arguments, restating one pipeline phase, or naming an RFC
noun is not sufficient. Derived decision regions, compilation shards, frontier
views, score vectors, and measurement batches remain ranges, indexed tables, or
method-local workspaces unless they acquire one of the contracts above. Packed
rows may use private implementation structs, but those rows do not become a
second domain model and are not copied between owners.

Each fact has one canonical home. Runtime-only measurements live in side tables
keyed by stable IDs rather than in a near-duplicate persistent record. A
checkpoint encoder reads the canonical arenas and emits its versioned schema at
the persistence boundary; synthesis does not propagate both runtime and
checkpoint representations. The implementation review shall enumerate every
full-field conversion between long-lived representations. A conversion that
only renames or repackages unchanged fields is rejected.

### Architecture decision groups and candidate catalog

Providers publish immutable candidate descriptions for complete coupled
decisions rather than pretending every architecture choice belongs to one
operation. The following is a logical arena layout, not a requirement for a
Rust object per row:

```text
CandidateCatalog
  groups[group_id]
    footprint range, interface_id, candidate range
  candidates[candidate_id]
    owner group_id, recipe_graph_id, shape_id, response_id
  responses[response_id]
    area, leakage, activity-power range, arc range, capacitance range
```

The production representation should use contiguous columns and packed ranges
when profiling confirms their benefit. `ArchitectureDecisionGroupId`,
`ArchitectureCandidateId`, and `CandidateResponseId` remain distinct because
mixing their index domains is invalid; this does not require independently
allocated group, candidate, or response objects.

The footprint is the exact union of source operations a candidate may consume,
replace, duplicate, share, or use to derive generated structure. Live groups
have disjoint footprints. A group may represent one adder, a fused operator
chain, a share-versus-duplicate decision, or a coupled mux/operator/demux
structure. Candidate discovery rejects a partial or overlapping footprint
instead of resolving it by proposal order.

Providers first emit stable candidate families with complete footprints. The
planner builds their overlap graph in stable family order. A connected
component becomes one decision group only when its total structural work fits a
fixed group-policy bound. When an overlap component exceeds that bound, the
planner retains the baseline families and a deterministic Pareto set of bounded
families, rejects every remaining cross-bound family with a recorded reason,
and rebuilds disjoint components. It never clips a candidate footprint. Group
size, rejected overlap work, and lost candidate classes are QoR metrics; the
bound is versioned policy and never allocator- or host-dependent.

Every candidate in one group has the same `ArchitectureInterface`: typed input
and output values, observable functions, sequential endpoints, cycle latency,
clock domain, and state contract. The initial RFC permits internal topology and
resource-count changes but not an interface or latency change. That makes a
candidate replaceable without retiming or reinterpreting the surrounding Word
graph. Cycle-changing retiming and state re-encoding remain explicit non-goals.

`CandidateResponse` is target-derived, immutable, and independent of one
design's arrival and required times. Each directed arc preserves timing sense,
early/late behavior, input transition, and output load. Its power surfaces are
queried with the synthesis scenario's activity context; missing activity is an
explicit unknown, not zero power. A power-optimizing objective requires a
complete activity context for every compared group or fails explicitly; an
area/timing objective may report power as unavailable. A query outside a
characterized electrical domain is an explicit error and is never silently
extrapolated.

Candidate topology may change which interface arcs are sensitizable. Each group
therefore owns a stable canonical arc universe; a candidate marks every arc as
present with a directed response or absent. Absent is not a zero-delay arc.

Candidate generation is bounded by provider policy. Dominated candidates are
removed only when dominance holds at every characterized operating point and
for every supported timing and activity lane. A candidate cannot be removed
merely because it is worse at one nominal slew/load point.

### Functional reduction

Functional reduction is the first pass over the lowered subject and the highest
measured quality mechanism in this RFC. It answers one question for pairs of
nodes: are these two literals the same Boolean function of the subject inputs?
A proved pair is either merged, when both nodes are ordinary logic, or recorded
as one equivalence class, when the two structures are worth retaining as
choices for mapping. Merging and classing are the same proof with two different
dispositions; they are not two passes and not two owners.

The pass has three bounded stages:

```text
1. bit-parallel random simulation refines candidate classes
2. one incremental SAT instance proves or refutes candidate pairs
3. refuted pairs return their counterexample to the simulation vectors
```

Stage 1 assigns every node a simulation signature over a fixed vector count and
partitions nodes by signature and polarity. Two nodes in different partitions
are definitely different; two nodes in one partition are candidates. Stage 2
proves candidates against a class representative in one incremental solver
under bounded conflict, representative, and pair budgets. Stage 3 folds every
counterexample back into the vector set so one refutation splits every class it
touches, rather than being rediscovered by a later pair.

`opto-formal::prove_logic_literal_partitions` already implements stage 2 with
exactly this contract, including the representative and pair budgets, and is
currently reachable only from its own tests. This RFC does not add a second
proof engine. It adds the simulation front end, the counterexample feedback,
the merge and class dispositions, and the production call site.

Determinism is a design constraint, not a solver property:

- simulation vectors come from a fixed seed and a fixed generator that depend
  only on subject input count and vector count, never on wall time, address
  values, worker count, or hash iteration order;
- candidate classes and their representatives use the stable node order, so the
  representative is the lowest-ID member and not the first prover to finish;
- conflict, representative, and pair budgets are versioned policy constants,
  and exhausting a budget leaves the affected class unmerged rather than
  merged on an unproved guess;
- a solver timeout is a bounded, reported non-merge, never a fallback path.

The pass never merges on simulation agreement alone. Equal signatures are a
filter; only an unsatisfiable miter authorizes a merge or a class edge. This is
the same rule the existing `ChoiceGraph` text states as an equivalence
certificate, applied one stage earlier.

Functional reduction has a crate-boundary consequence that must be decided
explicitly rather than discovered during implementation. `opto-formal` is
currently a dev-dependency of `opto-synth`, and `tools/check_architecture.py`
does not list it among `opto-synth`'s allowed dependencies. Accepting this RFC
accepts adding `opto-formal` to `opto-synth`'s production dependencies and to
that allow list. The direction stays acyclic because `opto-formal` depends only
on `opto-ir`. No SAT solver is introduced into `opto-core`, `opto-ir`, or any
timing or library crate.

The measured target for this pass alone is a 3.6% combinational-area reduction
on the reference Ibex SKY130 case, from 54,210.7 toward 52,300. Its runtime
budget is included in the one Boolean choice compilation line of the
performance contract and is not permitted to consume that line by itself. The
pass reports its own simulation, solve, merge, and class time, its proved,
refuted, and budget-exhausted pair counts, and the node count it removed.

### Choice graph

The selected decision-group candidates lower into one compact, shard-partitioned
Boolean graph owned by one arena:

```rust
pub(crate) struct ChoiceGraph {
    nodes: Box<[ChoiceNode]>,
    class_representatives: Box<[ChoiceNodeId]>,
    class_alternatives: Box<[CompactNodeRange]>,
    roots: Box<[ChoiceLiteral]>,
    shards: Box<[ChoiceShardRange]>,
}
```

Nodes use typed 32-bit IDs, complemented edges, contiguous arenas, and packed
fanins. An equivalence class may contain structurally different nodes with the
same complete Boolean function and polarity relation. It does not own a mapped
cell, a cloned region, or a second root set.

Every alternative carries an equivalence certificate produced by construction
or checked before installation. A choice belongs to exactly one decision group
or fixed-logic owner, and a compilation shard may not split its class. The
design-wide selector reads choices and responses but does not mutate ownership
or connectivity.

The choice graph is not a general e-graph. Rewrites are directed, support is
bounded, equivalence classes have fixed total order, and the retained node count
has a deterministic per-original-node limit.

That limit is not an area-first truncation. Admission first removes exact
duplicates, then removes alternatives dominated over the complete structural
signature:

```text
late depth by observable and critical input
early depth by observable and critical input
estimated area
fanout exposure
duplication and sharing cost
support size
```

If the Pareto set still exceeds the limit, the policy reserves every dimension
extreme and fills the remaining slots by a versioned deterministic Pareto-
crowding order. A zero-cost or depth-improving alternative cannot disappear
because a smaller-area structure happened to be inserted first. Candidate
admission, rejection reason, and retained structural signatures are benchmark
metrics.

### Compiled mapping arena

Cut enumeration, truth computation, and target matching populate one immutable
structure-of-arrays owner. The fields below describe the canonical columns;
they do not prescribe public row wrappers:

```rust
pub(crate) struct CompiledMapping {
    node_cuts: Box<[CompactCutRange]>,
    node_representatives: Box<[CutId]>,
    cut_leaves: Box<[PackedLeaves]>,
    cut_truths: Box<[TruthId]>,
    cut_matches: Box<[CompactMatchRange]>,
    match_cells: Box<[LibraryCellId]>,
    match_phases: Box<[MatchPhase]>,
    match_pin_permutations: Box<[PackedPermutation]>,
    match_areas: Box<[FiniteValue]>,
    match_arcs: Box<[CompactArcRange]>,
}
```

`NodeId`, `CutId`, `TruthId`, and `MatchId` remain different index domains.
Private packed row views may be returned briefly by accessors, but they borrow
these columns and never own or duplicate them.

The default policy stores at most a fixed number `C` of non-trivial priority
cuts per node and one representative cut. Phase qualification selects `C`; the
initial design point is 8 for ordinary logic and a separately bounded value for
recognized macro-cell matching. Raising `C` is an effort-policy change within
the same architecture, not a different mapper.

Cuts from every member of a choice class compete in the same bounded priority
set. Truth and target matching are computed once per unique `(leaves, truth)`
record and referenced by ID. Mapping passes may change ranking and the
representative record; they may not reconstruct the records.

The full priority set need only be resident on the active mapping frontier.
Nodes behind the frontier retain their representative plus any record required
by a live choice. The implementation shall measure whether frontier recycling
or retaining all bounded records is faster on Opto's target workloads before
choosing one production policy.

### Selected implementation is the timing and publication substrate

The mapper produces one compact selected topology whose cells, nets, arcs, and
boundary bindings are sufficient for exact timing. The timing engine borrows
that topology directly. It does not require an expanded named netlist.

If a bounded correction replaces a selected cut or match, one transaction
updates only the affected selected cone and its timing dependency closure. Once
accepted, publication seals the same arenas and adds final names, provenance,
and session identities without reconstructing cell connectivity.

There is therefore one topology construction and one topology owner, not a
temporary mapped netlist followed by a copied published netlist.

### Sequential cell selection and register identity

The measured sequential excess on the reference case is 883.3 area units and 34
registers. It has two independent causes, and neither is a Boolean or mapping
problem.

**Sequential cell form is chosen structurally, not by cost.**
`recover_feedback_enables` rewrites a register whose data input is a feedback
mux into an enabled register whenever the target library merely *has* an enable
cell for that edge and reset shape. On the reference case that produces 290
`edfxtp_1` at 30.03 where the alternative is `dfxtp_1` at 20.02 plus enable
logic that the mapper can absorb into surrounding cells. The rewrite is not
wrong; deciding it without comparing the two costs is.

This RFC requires the sequential form decision to be a costed decision in the
same units as every other selection in the flow:

- both forms are priced from the target: the enabled cell's own area, leakage,
  and characterized arcs against the plain cell's plus the estimated mapped
  cost of the enable structure it replaces;
- the enable structure is priced as logic the mapper will cover, not as a
  standalone mux, because a standalone-mux price systematically overstates it;
- with finite required times the comparison respects the same lexicographic
  order as candidate selection: electrical feasibility, then timing
  feasibility, then area, then power, then stable identity;
- absent characterization for either form is an explicit unavailable result,
  not an assumed zero and not a silent preference for the structural rewrite.

The decision is a decision-group candidate choice in the terms of this RFC. It
is not a new selector, a new pass, or a post-map repair.

**The surplus registers are not duplicates.** Opto retained 1,006 registers
where the same design mapped to 972 through Yosys. The obvious explanation is
that Opto lacks register merging, and `docs/architecture.md` does record that
absence deliberately: full-domain state equivalence sharing stays out until
arbitrary initial state, reset, enable, and clock semantics can be proved
rather than inferred from locally equal data inputs.

That explanation is wrong for this case. The mapped netlist contained no
register pair sharing a clock, reset, enable, and `D` connection, and at most
four registers whose output was unobserved or whose `D` was constant. An
exact-identity merge would have fired on nothing. The excess was hardwired and
reserved CSR fields whose reachable value is one constant, and Phase 1b records
how they are proved and removed.

The costed sequential form remains the right contract even though it did not
recover this gap, because deciding a cell form without pricing it is wrong
independently of what it happens to cost on one design. It is not urgent: on
the reference case the structural preference is a 264-unit net win.

## Compile-time and target-time lookup data

### Boolean rewrite atlas

A build-time generator may emit a versioned `RewriteAtlas` containing:

- canonical four-input NPN class identities and transforms;
- a compact shared DAG of useful non-redundant implementations;
- ordered rewrite recipes with exact support and polarity metadata; and
- a generator digest and atlas ABI.

The generated asset is ordinary checked source or a reproducibly generated
binary included in the build. Runtime never enumerates the atlas. A lookup
returns recipe IDs and transforms; materialization into the choice graph
remains decision-group- and shard-local.

The atlas is deliberately not the performance thesis of this RFC. The earlier
raw truth-table experiment failed because it left repeated lowering, rewriting,
and covering intact. The atlas becomes useful only after one lookup can feed a
shared choice graph and one mapping compilation.

### Target match catalog

Liberty-dependent matching cannot be compiled into the Opto executable. It is
built once per exact target fingerprint before parallel design work:

```text
Liberty fingerprint + mapping ABI
  -> canonical cell functions and legal phases
  -> pin permutations and timing senses
  -> optional bounded supergate matches
  -> immutable MatchCatalog
```

Workers read the catalog without locks. Construction finishes before hot shard
compilation begins. Concurrent lazy insertion into a shared hash table is
forbidden on the mapping hot path.

The first implementation matches single library cells. Supergates may be added
only if a separate benchmark shows material QoR benefit within runtime and
memory budgets.

### Characterization catalog

Architecture candidates use a target-characterization catalog keyed by the
exact `TargetFingerprint`, provider identity, recipe ABI, implementation shape,
timing view, operating grid, and characterization ABI. The target fingerprint
covers the canonical Liberty content and every target option that changes
matching, area, power, or timing. A response from another target namespace is
never a cache hit even if every remaining key field agrees. This catalog is the
information source for the analytical microarchitecture selector. It is
separate from Boolean rewrite recipes and cell matches because their identities,
invalidation, and consumers differ.

An arbitrary valid Liberty target cannot depend on a pre-warmed proprietary
database. Providers publish parameterized structural response templates. For a
fresh target, Opto projects only the implementation shapes present in the
current decision groups onto the immutable MatchCatalog, builds their bounded
response grids in parallel, and freezes the result before global selection.
The cold construction cost is part of the end-to-end five-second measurement.

A process-resident target may reuse a fingerprint-identical frozen catalog, but
warm reuse is reported separately and never substitutes for the cold gate. No
candidate may use a structural placeholder because cold characterization was
slow. A missing, non-converged, or out-of-domain response is a contextual
synthesis error naming the group, candidate, view, and operating point.

## Design-wide analytical microarchitecture selection

### Problem definition

The selector sees the complete design timing graph but mutates no design graph.
It chooses one candidate ID for every architecture decision group:

```text
decision variables       one finite candidate set per disjoint decision group
fixed structure          Word-level connectivity and endpoints
timing lanes             scenario, tag, check, view, transition
electrical state         receiver load and output transition
objective                feasibility, then PPA, then stable identity
```

Lanes remain correlated. The implementation may prune inactive lanes, but it
may not replace several scenarios with a scalar assembled from the maximum
arrival of one scenario and minimum required time of another.

Complete analytical evaluation of a decision vector performs a deterministic
bounded load/slew solve, early and late timing propagation, electrical checks,
and power/area accumulation. It is an estimate used for selection, not signoff.
Non-selectable operators and surrounding control logic contribute calibrated
fixed response arcs in the same model, so a datapath candidate is never scored
as though the rest of its path had zero delay or load.

Vectors are ordered lexicographically:

1. electrical feasibility, then maximum and total relative violation;
2. timing feasibility, then maximum and total violation in target time units;
3. total area;
4. leakage and dynamic power according to the declared synthesis objective;
5. the stable candidate-ID vector.

No weighted sum may allow area to purchase an electrical or timing violation.

### Bounded Lagrangian proposal engine

The production proposal engine is a design-wide, fixed-count Lagrangian search.
At each iteration it:

1. completely evaluates the current vector in the analytical model;
2. propagates non-negative prices backward from violated or near-critical
   endpoints through stable active predecessor arcs;
3. freezes the current slew/load operating points;
4. independently selects the minimum priced candidate for each disjoint group;
5. completely evaluates that proposal under its own coupled operating point;
6. retains the best completely evaluated vector.

For late arcs the priced candidate cost contains a positive delay term. For
early arcs it contains the opposite sign, so hold pressure rewards additional
minimum delay rather than accidentally removing it. Area and power retain their
canonical target units before multiplication by prices.

The step schedule, seed vectors, iteration count, quantization, and tie orders
are versioned policy. Phase qualification fixes them before production cutover.
The search stops at its bound, never at a data-dependent fixpoint. It reports no
global-optimality or duality-gap claim for the coupled discrete problem.

This is global selection without global mutation. A candidate change invalidates
the selected vector and downstream scores, not decision-region identity,
unrelated candidate compilations, or unrelated choice records.

## Choice-aware timing-driven mapping

### Timing seeds

The selected decision-group candidate vector and global analytical graph
provide every Boolean root and boundary with lane-specific:

- early and late arrival;
- early and late required time;
- input transition;
- output load;
- electrical limits; and
- timing price.

The mapper preserves lanes through feasibility tests. A compact scalar rank may
order already-feasible records, but it cannot decide feasibility.

### Mapping schedule

Mapping uses one fixed bounded schedule over the compiled records:

1. **feasibility pass** selects minimum late delay while respecting early and
   electrical constraints;
2. **area-flow pass** reduces estimated shared area without violating the
   required-time envelope;
3. **exact-area recovery passes** use selected-reference counts and local cover
   replacement to reduce actual area while preserving every active constraint;
4. **final timing-price pass** resolves remaining globally coupled boundary
   choices and stable ties.

Every pass reuses the same choice, cut, truth, and match records. The selected
reference-count overlay is sparse and reset in bulk. A pass changes IDs and
scores, not graph structure.

Shard compilation and local scoring run in parallel. Global arrival/required
and price propagation run over the decision-region DAG in stable topological
levels. Boundary summaries are compact lane-specific rows. A fixed number of
global selection rounds is permitted; there is no region-by-region STA/remap
loop.

### Exact-model correction

Exact MMMC timing runs on the selected compact topology before publication.
Correction is triggered by either an exact timing/electrical violation or a
model error beyond a versioned correlation tolerance. A clean exact result does
not enter correction merely because another legal mapping has lower estimated
cost.

The first correction level never recompiles structure. One fixed round may:

1. update timing prices from exact residuals;
2. rescore already compiled match records;
3. replace selected records in the affected timing closure; and
4. rerun incremental exact timing.

This level may not change an architecture candidate, generate a new Boolean
choice, enumerate a new cut, compute a new truth function, or build a new
library match.

If a structural violation remains after compiled-record selection, one
exceptional structural round may select another already-discovered and already-
characterized candidate for a stable bounded batch of mutually disjoint
decision groups. Admission requires both a structural violation classification
and a predicted improvement to the exact violation order. The round:

1. selects groups in exact violation contribution and stable group-ID order;
2. chooses from their existing candidate sets using the updated global prices;
3. lowers and compiles only newly selected `(group, candidate)` pairs;
4. replaces their selected topology in one checked transaction; and
5. reruns incremental exact timing over the complete affected closure.

The maximum group count and the single round are versioned policy constants.
The round does not rediscover candidates, change a decision-group footprint,
repartition, recompile a clean group, or restart full-design Word/Boolean work.
It is part of the one synthesis architecture, not a fallback mapper.

Structural repair is a pre-publication transaction over immutable accepted
generations. Newly lowered choices and compiled rows are appended to
transaction-owned provisional arenas; accepted `ChoiceGraph` and
`CompiledMapping` generations are never edited in place. Provisional IDs carry
the transaction generation and cannot be dereferenced through the accepted
generation. Before commit, validation proves that every new ID, cut leaf, match,
arc range, ownership range, and shard range belongs to the provisional or
accepted generation declared by the transaction; the candidate retains its
sealed interface and exact decision-group footprint; and its
`CandidateCompilationKey` contains the current group revision, candidate,
rewrite-atlas ABI, exact target fingerprint, and mapping ABI.

A successful commit first derives a new `CandidateCompilationSetId` from the
stable ordered set of accepted compilation keys, then derives the corresponding
`SelectionContextKey`, and finally performs one atomic root replacement that
publishes the new arena generations, selected topology, compilation set, and
exact timing generation together. Until that root replacement, no checkpoint,
report, closure pass, or session object can observe a provisional ID. Failure
at lowering, compilation, validation, exact timing, or objective comparison
discards the provisional arenas and timing overlay as one unit; the accepted
generation, selected topology, cache identities, and publication state remain
byte-identical.

If a violation remains, ordinary mapped closure receives the best structurally
legal topology and the exact violation state. Synthesis reports any final
unmet timing constraint; an unresolved electrical legality requirement is an
explicit failure rather than a nominally successful mapped result.

Model mismatch statistics are qualification output. Persistent systematic
mismatch or frequent structural-round activation is fixed in characterization,
candidate generation, or the analytical model, not by increasing the online
correction count.

## Post-map closure

Post-map closure begins from the best structurally legal, area-recovered result
and its exact violation state; that result may still miss timing. Closure owns
only transformations whose decisive information does not exist during choice
mapping:

- electrical buffering and high-fanout topology;
- legal driver cloning;
- drive-strength and threshold selection under exact load/slew;
- pin swapping under exact arcs; and
- narrowly scoped care-set resynthesis for a measured critical or violating
  cone.

An unconditional full-netlist area MFS sweep is removed from the default flow.
The mapper's exact-area recovery is responsible for ordinary combinational area.
Mapped MFS runs only on a measured dirty cone and only when its required care or
physical context was unavailable to mapping.

Closure evaluates forests or batches, not one cell followed by one full STA.
Every effort level has a fixed transaction and exact-timing budget. Rejected
transactions roll back atomically.

## Incremental reuse

Compilation identity and timing context are deliberately separate:

```rust
pub(crate) struct CandidateCompilationKey {
    group_revision: ArchitectureDecisionGroupRevision,
    candidate: ArchitectureCandidateId,
    rewrite_atlas_abi: u32,
    target_fingerprint: TargetFingerprint,
    target_match_fingerprint: TargetMatchFingerprint,
    mapping_abi: u32,
}

pub(crate) struct SelectionContextKey {
    candidate_compilations: CandidateCompilationSetId,
    constraint_generation: ConstraintGeneration,
    scenario_fingerprint: ScenarioFingerprint,
    interconnect_fingerprint: InterconnectFingerprint,
    selection_abi: u32,
}
```

These key types are intentional strong types rather than ceremonial wrappers.
They define different invalidation proofs: compilation identity excludes timing
context, while selection identity includes it. Accepting one where the other is
required could silently reuse invalid work.

A timing-only edit reuses every candidate compilation whose selected group
candidate does not change. If global selection chooses a different candidate,
Opto restores its fingerprint-identical compilation or compiles that candidate
once; it does not pretend that a different recipe has the same Boolean subject.
A local semantic edit invalidates only groups whose exact footprints or
interfaces intersect the real structural dependency closure. A target-library
change invalidates matches and characterization but may retain
target-independent Boolean choices and truth records.

The successful synthesis artifact owns immutable per-candidate compilation
records for candidates reached by the accepted search and bounded structural
round. It does not eagerly compile every architecture candidate. There is no
process-global mutable design cache and no cache eviction decision that changes
output.

## Determinism

- Decision groups, candidates, decision regions, compilation shards, timing
  lanes, choice classes, cuts, and matches have explicit stable total orders;
  a total order does not require every ordered item to be a standalone object.
- Parallel workers produce keyed immutable rows. Completion order never enters a
  score or ID.
- Floating-point inputs reject NaN, use canonical units, and are reduced in
  stable order. Timing comparisons quantize to the target time quantum.
- Search rounds and correction counts are fixed policy, not wall-time or RSS
  limits.
- Hash equality is followed by exact structural or truth comparison wherever a
  collision could change semantics.
- The MatchCatalog is immutable before parallel mapping. No hot-path shared
  lock or first-writer-wins cache affects selection.

Identical complete inputs and effort must produce identical mapped topology,
names, provenance, reports, and checkpoint bytes for every supported worker
count.

## Resource bounds

For `N` Boolean nodes, `C` priority cuts per node, `M` retained matches per cut,
`A` architecture candidates, `G` decision groups, and `L` active timing lanes,
the intended bounds are:

- choice graph: `O(N)` under a fixed alternatives-per-node policy;
- cut/match compilation: `O(N * C * M)` with policy constants `C` and `M`;
- analytical selection state: `O(G + A + N + L * E_timing)`;
- selected topology: `O(N)`; and
- correction journal: proportional to the affected selected cone and the fixed
  structural-repair group cap.

The design never owns `P` complete implementations for a portfolio size `P`.
Candidate recipes, response rows, choice nodes, and match records share their
canonical arenas. Peak RSS and construction scratch are reported separately.

## Performance contract

The five-second goal is an acceptance condition, not a forecast. The reference
Ibex run starts a fresh process and includes input and Liberty parsing, target-
catalog construction, elaboration, synthesis, exact timing, publication, and
the requested reports. It uses a pinned public RTL revision, public SKY130
library checksum, constraints, exact command, release binary digest, worker
count, and host description checked into the benchmark record. No prior process
state or pre-warmed target catalog contributes to the primary result.

The initial end-to-end budget is:

| Stage | Ibex target |
| --- | ---: |
| input, Liberty, normalization, ownership, and setup | <= 1.2 s |
| cold target catalog and global selection | <= 0.6 s |
| one Boolean choice compilation | <= 1.4 s |
| mapping, global recovery, and exact correction | <= 1.0 s |
| exact timing, closure, publication, and reports | <= 0.8 s |
| **total** | **<= 5.0 s** |

These sub-budgets guide profiling; only the total and QoR gates accept the
architecture. If a stage exceeds its budget, optimization targets its dominant
algorithm or data movement. The budget is never enforced by skipping valid
work at runtime.

Phase 0 freezes the comparison contract in a checked benchmark manifest. It
records the baseline Opto commit and release-binary SHA-256, the exact
`cargo build --release --locked --bin opto` build, synthesis command, Rust
toolchain, worker count, host image, RTL and Liberty SHA-256 values, constraints,
case membership, timeout, and every threshold below. Phase 7 compares the RFC
implementation against that artifact, not against a moving branch or a
developer build. Baseline and candidate runs are interleaved on the same idle
host image and use identical inputs and worker limits.

The product-level gate requires all of the following:

- the reference Ibex median from at least five serial fresh-process,
  cold-catalog runs is `<= 5.0 s`;
- every suite case has at least five successful interleaved baseline/candidate
  pairs, and each case is represented by its median paired runtime ratio;
- the geometric mean runtime ratio is `<= 1/3` for the complete suite and
  separately for each declared size tier, while no individual case ratio may
  exceed `1.05`;
- peak `peak_rss_kib` is no worse in geometric mean and no individual case may
  exceed `1.05` times baseline; the million- and ten-million-gate tier maxima
  may not increase at all;
- area ratios are `<= 1.00` in geometric mean and `<= 1.05` for every
  individual case; `critical_delay` uses the same aggregate and per-case bounds
  over timing-constrained cases with complete schema timing tuples;
- `worst_slack` may not cross from non-negative to negative,
  `total_negative_slack` may not decrease, and `violating_paths` may not
  increase; setup, hold, transition, capacitance, or fanout diagnostics newly
  introduced by the candidate fail the case;
- every requested combinational or sequential equivalence result is exactly
  `pass`; equivalence has no numeric tolerance; and
- mapped-netlist, report, and checkpoint SHA-256 values are byte-identical
  across supported worker counts after normalizing only manifest-declared
  absolute output paths. No names, IDs, numeric fields, diagnostics, or record
  ordering are normalized.

A timeout, crash, missing result, missing timing tuple, unavailable requested
equivalence result, or non-finite metric fails the gate before aggregation; it
is never dropped or replaced with the last successful run. No statistical
outlier is discarded. If the median absolute deviation of paired runtime ratios
for any case exceeds five percent of its median, the host run is unstable and
the complete baseline/candidate set is repeated; two unstable attempts fail
qualification rather than relaxing the bound. Warm-catalog, resident-session,
and incremental results are reported separately and never enter the cold gate.

Ibex alone cannot accept the RFC. The suite includes control-heavy, arithmetic,
reconvergent, high-fanout, memory, and larger production-shaped open designs.

## Validation

### Correctness

- Exhaustively prove each generated four-input rewrite recipe against its truth
  class and transform.
- Check every installed choice edge by construction proof or SAT equivalence.
- Verify that every decision-group candidate has the same sealed functional and
  sequential interface and that live group footprints are disjoint.
- Verify oversized overlap components retain only whole candidate families,
  report every rejection, and form identical groups across worker counts.
- Verify that no compilation shard splits a decision group, choice class, or
  correlated multi-output alternative.
- Differentially compare compiled cut truth against direct simulation.
- Verify every cell match against the canonical Liberty function and pin
  permutation.
- Run combinational or sequential equivalence, as applicable, from validated
  Word input through the final mapped result.
- Test early/late, rise/fall, unate/non-unate, setup/hold, recovery/removal,
  max/min delay, and electrical lanes independently.

### Functional reduction

- Prove that no merge or class edge is installed without an unsatisfiable
  miter; a signature match alone must never authorize either disposition.
- Fuzz the pass against exhaustive truth-table comparison on small subjects,
  including inverted, constant, and input-projection equalities.
- Verify that an injected wrong merge is caught by the end-to-end equivalence
  check, so the pass is covered by the existing proof and not only by its own
  assertions.
- Verify identical merges, identical representatives, and identical resulting
  node IDs across supported worker counts and across two runs of one worker
  count.
- Verify that exhausting the conflict, representative, or pair budget leaves
  the class unmerged, reports the exhaustion, and does not change the result of
  any other class.
- Verify that a refutation counterexample is folded back into the simulation
  vectors, by showing the same refuted pair is not re-proposed.
- Record proved, refuted, and budget-exhausted pair counts, removed node count,
  and the area delta attributable to the pass alone.

### Sequential selection

- Verify that the enabled and plain sequential forms are both priced and that
  the cheaper legal form is selected, on cases constructed to make each form
  win.
- Verify that missing characterization for either form produces an explicit
  unavailable result rather than a default preference.
- Verify that register merging fires only on exact structural identity, and
  construct near-miss cases differing only in reset value, enable polarity,
  initial value, or clock edge that must not merge.
- Run sequential equivalence on every merged case.

### Solver

- Enumerate small candidate graphs completely and compare the bounded selector's
  retained vector with the true optimum; record quality gaps without claiming
  the heuristic is exact.
- Verify late and early multiplier signs on synthetic setup- and hold-sensitive
  cases.
- Verify that no scalar envelope mixes scenario identities.
- Verify that cut/match correction changes only selected IDs, while structural
  correction creates only the explicitly journaled bounded candidate
  compilations.
- Verify that an exact violation triggers correction even when analytical and
  exact timing agree on its magnitude.
- Verify timing-only edits reuse unchanged candidate compilations and compile a
  newly selected candidate under a distinct key.

### Performance and QoR

Every benchmark records:

- source, library, constraints, binary, host, profile, and worker identities;
- schema-defined `wall_seconds`, `user_seconds`, `system_seconds`,
  `cpu_seconds`, and `peak_rss_kib`, plus separately named stage durations in
  seconds whose sum is checked against `wall_seconds` within the trace overhead
  declared by the manifest;
- node, choice, cut, match, frontier, and selected-cell counts;
- lookup hit counts without using them as a QoR decision;
- analytical versus exact timing error distributions;
- decision-group sizes, candidate counts, rejected overlap work, lost candidate
  classes, partition cut metrics, compilation-reuse counts, structural-repair
  activation, and repair runtime tails;
- schema-defined `area`, `cells`, `cell_histogram`, `clock_period`,
  `critical_delay`, `worst_slack`, `total_negative_slack`, and
  `violating_paths`; and
- a versioned qualification sidecar containing mapped-netlist, report, and
  checkpoint SHA-256 values per worker count, plus named setup, hold,
  transition, capacitance, and fanout violation counts.

The current result schema has no power or artifact-fingerprint fields. Before
power becomes an acceptance metric, the rollout must version that schema with
total, leakage, internal, and switching power in watts, their scenario and
activity fingerprint, and explicit missing-data semantics. For a power-aware
objective, total-power and leakage-power ratios must then be `<= 1.00` in
geometric mean and `<= 1.05` per case; missing requested activity or power data
fails the case. Until that schema revision, power is reported outside the
normalized gate and is never fabricated as zero. The qualification sidecar is
likewise versioned before Phase 7; it is not inserted into the current
`additionalProperties: false` result object.

Primary median runtime uses at least five serial fresh-process runs, each with a
cold target catalog. A separately reported resident process measures warm target
reuse and incremental synthesis. Concurrent independent-design throughput is
also separate and cannot substitute for single-design latency.

## Rollout

Each phase is a cutover to one production representation. Temporary comparison
harnesses are test/benchmark-only and are deleted when their decision is made.

The phase order below is the revised order. It front-loads the two mechanisms
whose quality contribution is measured and whose implementations are small,
before the large representation cutover whose measured quality contribution is
1.1%. The reason is not that the compile-once architecture is optional; it is
that a representation change validated only by "the netlist did not get worse"
gives no signal, whereas a mechanism with a predicted area delta either lands
that delta or is wrong. Phases 1a and 1b are independent of each other and of
the rest, so a stall in one does not block the others.

Phases 1a and 1b apply to the current per-region AXM subject and are re-run
unchanged on the choice graph once Phase 3 lands. Neither is a temporary
harness and neither is deleted.

### Phase 0: checked benchmark and stage accounting

Check in the exact Ibex and public-suite benchmark manifests. Make the stage
trace close numerically to total wall time and record Boolean node/pass counts,
cut/truth work, catalog construction, mapped MFS work, exact timing calls, and
peak resident/scratch bytes.

Accept when repeated release runs identify the same dominant work and all
benchmark metadata is reproducible. The primary five-second result is a fresh-
process cold-catalog median; warm resident results use a separate table.

### Phase 1a: functional reduction — implemented

Add bit-parallel simulation signatures, counterexample feedback, and the
production call site for `prove_logic_literal_partitions` on the existing AXM
subject. Merge proved-equal ordinary nodes. Move `opto-formal` into
`opto-synth`'s production dependencies and into the `check_architecture.py`
allow list in the same change. Do not introduce the choice graph yet; retained
alternatives arrive with Phase 3.

Accept when end-to-end equivalence holds, the reference Ibex SKY130 case shows
a combinational-area reduction of at least 3.0% attributable to this pass, the
merge set is identical across supported worker counts and repeated runs, budget
exhaustion is reported rather than silently merged, and the pass fits its
declared share of the Boolean stage budget.

Measured on the reference case, same inputs and host as the motivation section:

| Metric | Before | After |
| --- | ---: | ---: |
| lowered AXM subject nodes | 17,817 | 13,382 |
| combinational area | 54,210.7 | 51,157.8 |
| sequential area | 26,635.5 | 26,635.5 |
| total area | 80,846.3 | 77,793.4 |
| mapped cells | 8,956 | 8,475 |
| Boolean stage | 21.0 s | 17.6 s |

The pass proves 2,880 substitutions from 6,889 nominations across eight
refinement rounds and costs 2.7 s of that stage. It reduces the Boolean stage
rather than adding to it, because the 25% smaller subject makes the following
rewrite and cover passes cheaper than the sweep costs. Combinational area is
now 0.2% above the measured Yosys+ABC point of 51,037.7, so goal 4 is met by
this phase alone; the remaining external gap is sequential.

Class shards are proved in parallel with one solver per shard. The serial
formulation cost 15.1 s for the same result, which is a 4.4x reduction with a
0.008% area difference and no change to determinism: 1, 4, and 8 workers
produce byte-identical mapped netlists.

### Phase 1b: sequential excess — implemented, but not by the named mechanisms

The two mechanisms this phase originally named were measured against the
reference case and neither explains the sequential excess:

- **Exact-identity register merging finds nothing.** The mapped netlist has
  zero register pairs sharing a clock, reset, enable, and `D` connection, and
  at most four registers whose output is unobserved or whose `D` is constant.
  Whatever produces 34 surplus registers is not duplication.
- **The enable-cell preference is a small net win, not a loss.** Disabling
  `recover_feedback_enables` on the reference case converts only 10 of the 290
  enabled registers, and costs 264 area units overall: it saves 100 units of
  sequential area and pays 364 units of combinational area for the enable
  structure the mapper then has to cover. The remaining 280 enabled registers
  come from an RTL enable, not from feedback recovery, and Yosys selects 283
  enabled registers on the same design while still finishing ahead.

Matching the two register sets by RTL path found the real cause. The excess is
concentrated in hardwired and reserved CSR fields: 23 bits of `dcsr_q`, 8 of
`mtvec_q`, 6 of `cpuctrlsts_part_q`, and a long tail of ones and twos. Every one
of them is a register whose reachable value is a single constant, reached
through a write-enable gate rather than through a constant pin. Opto's
`constant_register_candidate` already asked the right question, substituting the
reset value into the register's own next-state function, but it asked it only of
the register's own pins, so a next state of `write ? 0 : Q` read as unknown.
Opto is also ahead of Yosys on the opposite side: it re-encodes four FSMs into
17 fewer registers than Yosys keeps.

The implemented change folds a bounded combinational cone behind the register's
input pins, following only nets the register's own outputs can still reach and
enumerating everything else as a leaf. The influence restriction is what makes
the fold both bounded and meaningful: a net the register cannot affect is an
unconstrained input to the proof, and following its cone would enumerate logic
that answers nobody's question. Independent removals commit as one transaction,
because each post-map transaction pays one incremental-STA update and paying it
per register cost 5.3 s for 39 removals against 0.24 s for the batch.

Measured on the reference case, on top of Phase 1a and the MFS scoping:

| Metric | Before | After |
| --- | ---: | ---: |
| registers | 1,006 | 967 |
| sequential area | 26,635.5 | 25,657.1 |
| combinational area | 51,446.8 | 51,205.4 |
| total area | 78,082.4 | 76,862.5 |

Opto now keeps five fewer registers than Yosys on the same design, and total
area is within 0.09% of the measured Yosys+ABC point of 76,789.9. Goal 5 is met.
The costed sequential-form contract stays in this RFC as the correct rule for
deciding a cell form, but it is no longer on the critical path for this case.

### Phase 2: immutable generated catalogs

Implement the reproducible RewriteAtlas generator, target MatchCatalog,
parameterized response templates, and on-demand CandidateResponse projection
for the shapes present in a design. Build and freeze target catalogs before
parallel design work. Do not change production selection yet.

Accept when generated assets are reproducible, all recipes and matches pass
exhaustive checks, cold/warm cost is measured, and catalog lookup has no hot
shared lock.

This phase is a runtime and infrastructure phase. The measurements in this RFC
show Opto's existing rewriting already at `dc2` quality, so no area improvement
is claimed for the atlas and none is required to accept the phase.

### Phase 3: decision groups, partition scopes, and choice graph

Introduce complete disjoint decision-group footprints and sealed interfaces.
Give decision groups, analytical regions, and compilation shards distinct index
domains without requiring parallel object trees, and prove that no shard cuts a
group or correlated choice. Replace independently optimized AXM implementations
with one `ChoiceGraph` and one `CompiledMapping`; compile bounded cuts, truth,
and matches across its equivalence classes once. Retain the existing selected
result only as a test oracle, not a production fallback.

Accept when equivalence holds, Boolean preparation drops below its stage budget,
memory remains within the declared bound, group/shard invariants pass on
reconvergent and coupled multi-output cases, and the compiled arenas can
reproduce or improve the accepted mapped cover. The implementation review must
also identify the three authoritative owners, show that derived scopes are
views or ranges, and find no full-field `Plan`/`Record` conversion in the hot
path. The retained equivalence classes must additionally show at least a 1.0%
combinational-area reduction beyond the Phase 1a result on the reference case,
which is the measured `&dch -f` contribution; a choice graph that reproduces
the Phase 1a area is a representation change without its stated benefit and
does not pass.

### Phase 4: analytical semantic selection

Introduce lane-preserving CandidateResponse evaluation and the design-wide
bounded proposal engine. Delete scalar or independently regional semantic
selection when the global selector cuts over.

Accept when timing-sensitive groups may select different microarchitectures,
share-versus-duplicate cases are jointly evaluated, small exhaustive problems
quantify heuristic gap, analytical/exact correlation is recorded, and the stage
meets its runtime budget.

### Phase 5: choice-aware mapping and integrated area recovery

Make required times, loads, slews, and timing prices drive the compiled-record
selector. Add exact-area recovery over mapping reference counts. Delete
independent proposal cover runs and unconditional default full-netlist area MFS.

Accept when the complete public QoR gate passes and mapping plus recovery meets
its stage budget.

### Phase 6: zero-copy exact timing handoff and bounded correction

Make exact MMMC consume the selected compact topology directly. Add one bounded
compiled-record correction, one exceptional bounded decision-group structural
round, and seal the accepted topology without rebuilding clean connectivity.

Accept when exact and published topology identities agree, rollback is atomic,
systematic model error and structural-repair activation are reported, timing-
only cache reuse follows candidate identity, unresolved electrical illegality
fails explicitly, and timing/publication meets its stage budget.

### Phase 7: production qualification and deletion

Run the complete performance, QoR, equivalence, determinism, checkpoint, and
incremental suites. Delete displaced plan portfolios, duplicate lowering/cover
paths, obsolete cache fields, old objectives, old tests, and conflicting
architecture text.

Accept only when the reference Ibex median is at most 5.0 seconds, the suite-level
3x and QoR gates pass, and no fallback or hidden selector remains.

## Alternatives

### Treat retained choices as the primary quality mechanism

Rejected by measurement, and this is the change from the first revision. On the
reference case `&dch -f` is worth 1.1% of combinational area while `&fraig -x`
is worth 3.6%. Choices stay in the design because they are the correct
representation for the survivors of functional reduction and because 1.1% is
real, but a rollout that lands the choice graph first spends its largest
implementation effort on its smallest measured return and produces no
intermediate quality signal.

### Add functional reduction later, on top of the choice graph

Rejected. The proof engine is the shared prerequisite: merging a proved-equal
pair and recording it as a class edge are two dispositions of one result. Doing
the representation first means building the class machinery with no engine to
populate it beyond directed rewrites, which is what the current portfolio
already does at whole-network granularity. Doing the engine first makes the
class representation a small extension rather than a precondition, and it
delivers the larger measured share against the existing subject.

### Rely on simulation equivalence without SAT proof

Rejected. Signature agreement over any finite vector set is a filter, not a
proof, and a wrong merge is a functional bug that the end-to-end equivalence
check would report as a synthesis failure rather than an area result. The cost
of the proof is bounded by explicit conflict, representative, and pair budgets;
the cost of an unproved merge is unbounded.

### Solve the sequential excess with post-map repair

Rejected. Both causes are decisions, not damage. The sequential form is chosen
before mapping and can be priced there; register identity is visible in the
Word graph. Recovering either after mapping would rebuild the area-first repair
loop this RFC rejects for combinational logic.

### Widen the cut policy to close the gap

Rejected by measurement. Raising `MAX_CUTS_PER_NODE` from 32 to 64 on the
reference case moved total area from 80,846.3 to 81,057.7. Cut capacity is a
runtime and memory parameter here, not a quality parameter.

### Keep multiple complete regional cover plans

Rejected. It multiplies the dominant lowering, Boolean optimization, cut, truth,
and cover work and retains too much topology. Compact semantic responses,
choice nodes, and cut/match records preserve the useful alternatives.

### Area-first mapping followed by critical-cone restructuring

Rejected as the main architecture. It pays exact timing and reconstruction
after committing structural decisions, exposes only local repair moves, and can
oscillate or converge to a poor local result.

### Timing-driven mapping only

Rejected. Mapping cannot recover a microarchitecture that semantic selection or
bit lowering already discarded. Timing must guide every irreversible upstream
choice or that choice must remain represented through mapping.

### Full exact STA inside candidate selection

Rejected. Exact mapped timing requires a selected cell topology and would turn
candidate evaluation into repeated materialization. Characterized analytical
responses select coarse architectures; exact timing validates the compact
implementation and is rerun only for the two explicitly bounded correction
levels.

### Treat every operation as an independent architecture variable

Rejected. Sharing, duplication, fusion, and coupled mux/datapath structures
replace several operations together. Complete disjoint decision-group
footprints make that coupling explicit and prevent proposal-order ownership.

### Use compilation shards as semantic regions

Rejected. Scheduling and storage decomposition would then remove alternatives
and make worker policy a QoR decision. Decision groups and analytical regions
own semantics; shards only own bounded work.

### Precompile every architecture candidate

Rejected. It multiplies the dominant Boolean work. A synthesis compiles the
selected candidate of each group and at most the candidates reached by one
bounded structural round; immutable per-candidate records provide later reuse.

### Runtime NPN search or a larger raw truth table as the main optimization

Rejected by profile evidence. It attacks lookup cost while leaving repeated
compilation intact. Generated lookup data is retained only as infrastructure for
the compile-once flow.

### Supergate-first mapping

Deferred. Structural choices, bounded priority cuts, single-cell Boolean
matching, and integrated area recovery have higher leverage and lower catalog
cost. Supergates require separate evidence.

### Whole-design e-graph

Rejected. It has an unbounded alternative and memory surface, weak incremental
ownership, and no demonstrated path to the latency target.

### Unbounded exact-timing correction

Rejected. It converts model error into unpredictable runtime and recreates the
progressive repair loop. One compiled-record round plus one bounded decision-
group structural round are the complete correction budget; persistent error is
fixed at its source.

## Prior art and references

- Cadence, [Genus Synthesis Solution product brief](https://www.cadence.com/en_US/home/resources/product-briefs/genus-synthesis-solution-pb.html),
  especially the public description of global analytical architecture
  optimization.
- Cadence, [Genus Synthesis Solution datasheet](https://www.cadence.com/en_US/home/resources/datasheets/genus-synthesis-solution-ds.html),
  for public claims about critical datapath regions, microarchitecture choices,
  timing-driven partitioning, and concurrent MMMC optimization.
- Cadence, [Genus and Innovus: Compus and iSpatial](https://community.cadence.com/cadence_blogs_8/b/breakfast-bytes/posts/cdnlivegenus2),
  for the public distinction between early TNS-guided microarchitecture choice
  and bounded post-placement critical-region restructuring.
- A. Mishchenko et al., [Combinational and Sequential Mapping with Priority
  Cuts](https://people.eecs.berkeley.edu/~alanmi/publications/2007/iccad07_map.pdf),
  ICCAD 2007.
- S. Chatterjee et al., [Reducing Structural Bias in Technology Mapping](https://people.eecs.berkeley.edu/~alanmi/publications/2005/tcad05_map.pdf),
  IEEE TCAD 2006.
- A. Mishchenko et al., [DAG-Aware AIG Rewriting: A Fresh Look at Combinational
  Logic Synthesis](https://people.eecs.berkeley.edu/~alanmi/courses/2007_290N/papers/synthesis_berkeley_dac06.pdf),
  DAC 2006.
- A. Mishchenko et al., [Improvements to Combinational Equivalence
  Checking](https://people.eecs.berkeley.edu/~alanmi/publications/2006/iccad06_cec.pdf),
  ICCAD 2006, for the simulation-and-SAT fraiging loop this RFC adopts for
  functional reduction.
- S. Chatterjee et al., [FRAIGs: A Unifying Representation for Logic
  Synthesis and Verification](https://people.eecs.berkeley.edu/~alanmi/publications/2005/tech05_fraigs.pdf),
  ERL technical report, 2005, for the merge-versus-class disposition of one
  equivalence proof.

External descriptions motivate the representation and search strategy; they do
not establish an Opto performance or QoR result.
