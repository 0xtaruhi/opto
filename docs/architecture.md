<!-- SPDX-FileCopyrightText: 2026 Zhengyi Zhang -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# Opto Architecture

This document is the normative architecture for Opto. It describes the
long-term production model and identifies what the current tree implements.
Source code is not architectural precedent merely because it exists; code that
violates this contract is removed rather than preserved behind a compatibility
path.

[RFC 0007](rfcs/0007-timing-driven-partitioning.md) replaced the global
synthesis front half with timing-driven ownership and region-private mapping.
The coordinator validates, establishes provisional ownership, commits only
owner-confined structural transformations, seals the final region graph,
propagates contracts, and publishes private artifacts. No semantic candidate
may span owners.

[RFC 0010](rfcs/0010-command-surface.md) defines the public command design: a
flat Tcl surface with a coherent typed database model and one
canonical `read_hdl` -> `elaborate` -> `synth` lifecycle.

Opto targets industrial logical synthesis from small blocks through
multi-million-gate designs. Performance objectives are versioned against
public, checksum-pinned suites and the last accepted Opto baseline; one small
design never establishes a product-level claim. Runtime, peak RSS, QoR,
deterministic output, and useful diagnostics are all correctness properties.

## Product Boundary

Opto has one executable, `opto`, and one production synthesis flow. The public
surface is a flat Tcl shell with a typed `get_db` / `set_db` model; process
options are parsed by `clap`. There is no project manager, manifest-driven
product mode, alternate mapper, or backend-specific executable.

Opto's command catalog, typed argument grammar, database schema, report
schemas, and tests define the public contract. Flat report names and standard
SDC commands are retained where they are clear. Interface decisions are
recorded in [Public interface policy](#public-interface-policy).
Internal concepts such as regions, construction vectors, Boolean subjects,
contracts, and epochs are not exposed as invented Tcl controls. Unsupported
behavior reports an explicit error.

`synth` is the single public synthesis operation. Typed effort properties may
change deterministic budgets, but they do not select a different
implementation architecture.

## Non-Negotiable Rules

- There is no fallback, legacy, shadow, or environment-selected synthesis path.
- A semantic operation has one authoritative owner at every stage.
- Stable identities and dense row identities are different types.
- Cross-region dataflow is represented by explicit typed ports.
- Global connectivity, boundary identity, and publication obligations freeze
  before region-local Boolean optimization or cover.
- A regional artifact may read the global substrate but may never merge,
  redirect, or drive an equivalence class that it imports.
- Analysis is read-only parallel work; mutation and publication are
  deterministic.
- Workers never allocate final global IDs, names, or user-visible UIDs.
- Worker count and task completion order cannot change the mapped result.
- Search bounds are deterministic policy, never an RSS admission controller.
- Valid work is not rejected because a predicted heap cost crosses a threshold.
- The target flow retains one canonical Boolean subject, not a full-design
  alternative graph.
- A region has one construction vector. There is no regional “winner,” Top-K
  implementation set, or Pareto portfolio.
- Structural estimates may choose a construction, allocate work, and guide
  cover search. Exact Liberty mapping and global timing remain authoritative.
- Final mapped topology, timing, power, and provenance are sealed generations;
  consumers reject mixed generations.
- `TimingObjectBindings` resolve persistent objects by stable flat names, not
  generation-local mapped or timing arena IDs. Repacking may change dense IDs
  but must preserve those names exactly. Pin bindings store the instance and
  pin-name components as interned IDs, so hierarchical instance names remain
  exact without retaining one allocated `instance/pin` string per pin.
- Dead commands, code, tests, cache fields, and documentation are deleted in
  the same cutover that supersedes them.

## Ownership

```text
opto                 executable, clap, Tcl lifecycle, commands, diagnostics
opto-session         persistent state, atomic publication, object identity

opto-core            typed identities, diagnostics, packed rows
opto-db              designs, collections, linked user-visible objects
opto-ir              RTL/Proc, Word, and mapped storage invariants
opto-library         canonical Liberty target, timing, and power data
opto-runtime         one worker pool, cancellation, ordered parallel execution

opto-hdl             frontend orchestration
opto-slang-sys       opaque slang boundary
opto-tcl-sys         opaque vendored Tcl boundary

opto-synth           synthesis policy and transformations
opto-timing          constraints, timing graph, full/incremental STA
opto-power           activity propagation and power evaluation
opto-formal          independent proof and qualification problems, plus the
                     equivalence engine that AXM functional reduction calls
opto-formats         Verilog and report rendering
```

`opto-session` is the only component that coordinates frontend, synthesis,
timing, power, persistence, and user-visible identity. Algorithm crates receive
typed immutable inputs; they do not receive `Session`.

### Diagnostic contract

`opto-core::Diagnostic` is the presentation-independent user-facing contract.
It carries a severity, stable product code, title, optional primary and related
source labels, notes, and remediation help. Owning domains implement
`DiagnosticSource` where their dependency boundary permits it; the
`opto-session` coordinator supplies adapters for lower-level HDL and format
errors, preserves typed diagnostics, and adds command context. `opto` alone
renders them for a terminal. The
stable code domains are `OPT-CLI`, `OPT-SES`, `OPT-HDL`, `OPT-LIB`, `OPT-FMT`,
`OPT-TIM`, `OPT-PWR`, and `OPT-SYN`. Internal and capacity failures use the
reserved high-numbered codes in their owning domain instead of falling back to
an unclassified string.

The Slang bridge captures effective severity, numeric diagnostic identity,
warning-control name, formatted message, source path, line, column, and range
directly from Slang's diagnostic API. Rust never recovers frontend locations by
parsing rendered terminal text. Errors abort the operation; warnings remain
attached to successful analysis or compilation results. `opto-session` queues
successful-operation warnings only after atomic state publication, and the Tcl
dispatcher drains and renders them exactly once without changing command
success or result values.

Command invocation validation also participates in the structured contract.
Usage errors carry `OPT-CLI-001`, point users to `help <command>`, and suggest a
nearby declared option for small spelling mistakes. Per-command help is derived
from the same declarative schema as validation and includes a summary, usage,
typed arguments and options, preconditions, and an example. Unsupported options
remain listed explicitly and fail when used.

### `opto-synth` Internal Domains

`opto-synth` has eleven top-level domains. Top-level modules are architectural
boundaries, not a flat inventory of individual passes:

| Domain | Owns | Must not own |
| --- | --- | --- |
| `api` | library-facing requests, results, errors, and diagnostics | stage ordering, rendering DTOs, or synthesis algorithms |
| `engine` | synthesis stage ordering, immutable stage states, regional mapping epochs, and final publication | pass-local transformation algorithms |
| `frontend` | procedural RTL normalization and the single validated Word-IR entry point | architecture selection or mapped-netlist mutation |
| `word` | reusable Word-IR dependency, driver, cycle, use, and instance analyses | transformation policy or mapped artifacts |
| `planning` | region-owned Word optimization, semantic recognition, mapping cost policy, resource choices, and regional architecture recipes | cross-owner semantic optimization, target-cell cover, or post-map repair |
| `boolean` | bit lowering, canonical Boolean subject construction, rewriting, and graph analysis | target-library policy or mapped closure |
| `regional` | stable region graph identities, boundary contracts, portable plans, and deterministic convergence policy | incremental persistence or MMMC measurement |
| `incremental` | source/Word fingerprints, recipe caches, metrics, and portable regional cache records | architecture selection or live mapped state |
| `mapping` | target catalogs, cover selection, bindings, sequential mapping, clock-gating preparation, and deterministic materialization | epoch scheduling, frontend normalization, or post-map closure |
| `closure` | MMMC analysis, mapped boundary measurement, power objectives, and transactional post-map repair | source semantics, regional identities, or technology mapping |
| `artifact` | durable implementation ownership and source provenance | implementation selection algorithms |

The crate root is only the external Rust facade. Internal code uses a root name
only when that name is part of the public `opto-synth` contract; private
implementation dependencies use their owning domain path. `engine` is the only
domain allowed to order the complete flow. The frontend exposes one sealed
entry point, and downstream domains never call its procedural lowering
internals. Planning and Boolean lowering exchange explicit architecture recipe
types; regional planning and mapping exchange explicit contracts, bindings,
and cover plans. Mapping produces one generation for the engine-owned epoch
driver; closure measures that generation without reaching back into cover
selection. These typed seams are intentional and must not be replaced by access
to mutable orchestration state.

Adding another top-level domain requires updating this ownership table. A new
pass normally belongs under the domain that owns its input and output
invariants; it does not receive a peer module at the crate root merely because
its implementation is large.

One successful synthesis run publishes one canonical mapped netlist,
implementation provenance, synthesis report, timing summary, and compact incremental records. It
does not retain every explored builder or a mutable copy of each intermediate
design.

The mapped netlist is also the canonical session object backend. A borrowed
`DesignView` selects either the source `DesignIndex` or
`SynthesisResult::mapped()`; mapped publication never expands cells, pins,
connections, signals, or names into a second `DesignIndex`. The only mapped
session sidecar is `MappedObjectIndex`: generation-bound, name-ordered `u32`
slot rows for deterministic registry replay and exact lookup. Anonymous mapped
net names (`_n<slot>`) are formatted into reusable boundary scratch and become
owned only by the persistent object registry or an actual command result, not
by another full-netlist name arena.

Mapped provenance and mapped ownership are independent relations.
`ImplementationDb` assigns every live cell exactly one compact owner atom:
the static global substrate, one `RegionAnchorId`, or one
`BoundaryEdgeId(driver_region -> sink_region)`. Source/link instances, memory
macros, black boxes, ports, and clock infrastructure belong to the global
substrate unless lowering gives a generated cell an explicit region owner.
HFNS and cloning split a multi-region sink set into one segment per sink-region
endpoint; `BoundaryEdgeId -> [CellId]` is the stable reverse footprint used by
transactions, checkpoint validation, and publication remapping. Semantic
operator provenance follows the cells that implement the operator and never
acquires sink provenance merely because a repair crosses a boundary.

Semantic operators also have a distinct durable database representation.
Region-private recognition interns complete, versioned semantic signatures and
records each occurrence by `OperationAnchorId` and source-operation provenance.
The technology-independent Word and operator representations remain the normal
input to architecture selection; target lowering expands the chosen recipe and
Liberty covering produces the only publishable mapped netlist.
`SynthesisResult::operator_manifest()` keeps the semantic signature table and
stable occurrences for provenance and incremental analysis. It is not a second
generic mapped representation and carries no opaque mapped-cell binding.

Origin sets have one canonical packed operator CSR. Exact reverse lookup stores
only a deterministic 64-bit hash and inline `OriginSetId` collision candidates;
it compares candidate CSR rows before accepting a match. The reverse index
therefore neither duplicates each operator row into a boxed tree key nor treats
hash equality as identity.

An accepted cross-region repair is represented once in the live implementation
database as an exact `(driver region, sink region)` edge owner with a stable
reverse cell footprint. Physical repair topology is not copied into the
regional decision cache: every synthesis run reconstructs it through the same
canonical post-map transaction flow. Missing, same-region, global, and
multi-region endpoints are never silently converted into a boundary owner;
non-crossing root segments take an explicit region/global lineage at their call
site.

## Production Workflow

The production flow has one region-private synthesis path and requires a bound,
non-empty Liberty target library:

```text
linked RTL + constraints + libraries
  |
  v
process normalization and design validation
  |
  v
stable structural anchoring and root-closure partitioning
  |
  v
owner-confined FSM / sharing / sequential preparation
  |
  v
final region-graph rebuild and identity seal
  |
  v
immutable SynthesisRegionGraph + monotone boundary contracts
  |
  v
parallel region-private Word optimize / plan / lower / cover
  |
portable regional plans + explicit source/boundary bindings
  |
  v
deterministic regional artifact publication
                            |
                            v
          one mapped generation + shared MMMC owners
                            |
                            v
        incremental boundary measurement/dirty replacement
                            |
                            v
          canonical mapped netlist + provenance
                            |
                            v
           transactional post-map optimization
                            |
                            v
               sealed timing/power/report state
```

### 1. Normalize And Validate

The frontend publishes one `RtlModule` containing structural Word IR and sealed
Proc IR. Each procedure follows one explicit lowering pipeline:

```text
Proc CFG
  -> CFG canonicalization
  -> per-target sparse state propagation
  -> typed event-aware reset/enable extraction
  -> canonical Predicate DAG
  -> Word IR
  -> validated Word IR
```

CFG canonicalization removes unreachable blocks, folds constant decisions,
threads empty jumps, coalesces equal successors, creates a virtual common exit,
and computes topological, dominator, and post-dominator structure. Unsupported
procedural loops are diagnosed explicitly. State propagation is per target: an
effect that changes another signal cannot manufacture a mux or guard for the
target being lowered. Sensitivity events remain typed controls instead of being
rediscovered from an arbitrary Boolean expression.

Linked elaboration completes the SSA value domain before regional ownership is
established. An omitted input of an expanded definition receives a type-correct
completion value (`X` for a four-state input and zero for a two-state input), and
that value propagates through the ordinary Word graph. Four-state `X` remains
care-free; two-state zero is the language-domain value. An instance retained by
`dont_touch`, `keep_hierarchy`, or black-box status instead preserves the absent
binding as part of its structural interface. After procedures and resolved nets
are lowered, otherwise-undefined bits of source-observable output ports are
sealed with the same type-correct completion rule. This step does not add
drivers to missing internal or generated logic.

Four-state `X` is the Word representation of a care-free SSA bit, not an early
choice of Boolean zero. Fixed combinational dataflow supplies value facts in
topological order; regional observability and cover propagate care from frozen
roots over the reverse dependencies. Rewrites use sparse dependency worklists
when a changed fact can affect another result. Registers, latches, and memories
are explicit boundaries, and source combinational cycles are diagnosed instead
of being treated as a fixed-point optimization problem. Deterministic zero is
chosen for a remaining care-free bit only when the final physical netlist is
published.

Failures carry the structured diagnostic contract defined above, including the
Tcl invocation that triggered the work when available. A raw failure string
without a stable owning-domain code is not the diagnostic contract.

### 2. Establish Ownership And Freeze The Region Graph

The coordinator first partitions the validated synthesis-root closure to give
every live operation one provisional owner. FSM optimization, equivalent
sequential sharing, and target sequential preparation may then analyze and
rewrite only candidates whose complete footprint has one owner. Generated
operations inherit that owner through stable source provenance, so the stages
compose without renumbering the arena or rebuilding the partition between
passes. One final build seals the post-rewrite graph.

No opportunity discovered in this stage influences the initial ownership
partition, and no candidate may span owners. Combinational canonicalization,
muxed-arithmetic sharing, architecture selection, operator fusion, and cover do
not run here. After the owned structural stage,
`SynthesisRegionGraph` is rebuilt and sealed with:

- stable operation, region-anchor, and region-revision identities;
- dense revision-local `RegionRowId`;
- exact member operations and first-class memories;
- typed input/output boundary ports with identities separate from value
  revisions;
- predecessor/successor packed rows;
- architecture-independent delay, logic, and wiring estimates;
- local and contextual fingerprints.

`StructuralOwnershipProvenance` is the write authority during structural
preparation. Initial operations carry their frozen owner atom; every transform
must claim each generated operation from an exact source set with one common
atom. The final graph independently recomputes the complete synthesis-root
closure, rejects an unowned live operation, and verifies that every surviving
frozen atom remains whole. Final partitioning may merge whole atoms but may not
split one. Ownership is not a preparation-side lookup that later partitioning
may discard: it survives operation replacement, final partition, private-IR
construction, plan binding, artifact publication, and provenance.
Published objects use exactly three owner classes: global substrate, one
region, or one directed boundary edge.

The same freeze seals full-domain connectivity and boundary identity. Global
substrate equivalence may come only from an explicit unique static connect, a
globally exact pass-through, or a constant proved over the complete Word
domain. A care set, truth-table reduction, or region-local rewrite is not
connectivity evidence and cannot enter the substrate alias classes.
An involution such as two logical or bitwise inversions is not an alias without
an explicit two-state domain proof: `Z` may become `X` and cannot be recovered.
When publication needs a physical identity and the target library has no
buffer, the cover retains a two-inverter artifact instead of merging nets.
Full-domain per-bit constant facts are captured before the regional shell
mutates the Word graph. Proven bits enter substrate constant classes directly;
unknown bits receive publication endpoints. A private cover may omit an
artifact for one of those frozen constants, but a private pass-through never
creates a substrate alias or weakens a source-level publication obligation.

State and output roots claim their fan-in cones. Size truncation promotes a
frontier operation to a seed; shared fan-in receives one owner. Coarsening
scores adjacent cones by the accumulated criticality and bit width of the
boundary that a merge removes. Below-minimum fragments propose to a feasible
established neighbour, which may absorb several independent fragments in one
round; otherwise a deterministic maximal matching merges disjoint highest-gain
pairs. A fixed maximum round count bounds propagation, and coarsening stops
early when no admissible adjacent merge remains.

Partition activation, minimum useful work, target work, and maximum merge work
are separate deterministic limits. When the complete reachable operation
closure fits the activation limit, the initial cone partitioner emits exactly
one operation region before seed construction; this preserves all region-local
sharing and amortizes regional scheduling and publication for small blocks.
The final structural-owner path groups its already-frozen owner atoms directly
and does not evaluate the activation limit. Larger initial closures use the
bounded cone partitioner and never merge disconnected work merely to reduce
region count. There is no global bin packing,
resource-affinity proposer, cross-owner candidate analysis, or architecture
candidate analysis before the final freeze.

The final freeze contracts each provisional `StructuralOwnershipProvenance`
owner into an indivisible atom before coarsening. Generated operations inherit
both the owner and its stable partition anchor. The final partition may merge
whole atoms but never repartitions individual operations and then repairs a
split owner after the fact; therefore capacity accounting observes the actual
atomic input to the final partition.

Partition size does not depend on thread count. This preserves incremental
identity and makes one-thread and many-thread output equivalent.

### 3. Allocate Boundary Contracts

Sparse scenarios bind explicit early/late libraries, constraints, interconnect
views, activity, and enabled checks. `BoundaryContract` carries only active
scenario/tag rows, including arrival, transition, required time, load, and
electrical limits. It is not a second dataflow representation: constants,
liveness, aliases, and region ports are derived from the frozen Word graph.
Absolute structural delay drives arrival/required propagation without a
design-wide normalization factor.

For memories, compatible characterized macros and an exact register-bank
implementation are normal candidates. There is no memory admission gate. An
unsupported port, mask, collision, clock, or reset contract is reported
explicitly.

### 4. Optimize And Map Private Regions

Every worker imports only its owned dependency cone and explicit boundary
values into a private `WordModule`. Owned registers and latches remain in the
unique sequential shell: `Q` is imported as a typed boundary input, and `D`
plus controls are private observable roots. No placeholder or backpatch is
used. The worker then performs, in that module:

1. dataflow canonicalization, constant/known-bit/liveness optimization, and
   priority-mux rebalancing;
2. semantic operator discovery and architecture choice;
3. selected-recipe bit lowering and Liberty Boolean cover.

Selected recipes emit canonical AXM literals through the shared bit-lowering
algorithms. Region lowering retains only scalar Word identities needed for
typed input, root, ownership, provenance, and publication bindings; it does not
materialize an intermediate scalar-Word Boolean network and rebuild AXM from
that network. The global sequential shell uses the same algorithms with a Word
backend because its state cells and structural connections remain publication
objects.

The sequential shell is selected before Boolean cover establishes its timing
coordinates. Cover guidance and substrate materialization call one deterministic
register-cell selector: the selected flip-flop's characterized clock-to-Q delay
and output transition seed the `Q` boundary input, while its setup check reduces
the corresponding `D` root requirement. Missing characterization remains absent
rather than becoming an invented zero. Latch transparency remains the global
timing engine's responsibility and is not approximated as a flip-flop boundary.

The canonical Boolean subject is an **AXM graph**:

```text
AND2 | XOR2 | MUX3 + complemented edges
```

There is no inverter node. AND and XOR operands are ordered, constants and
idempotence are folded before interning, XOR phase is carried by the result
edge, and an inverted MUX selector is normalized by swapping its arms. MUX
forms that are semantically AND, XOR, or a literal reduce to those canonical
forms; MUX remains first-class for genuine selection and Shannon
decomposition. This redundancy is intentional semantic structure, not three
independent design representations. Liberty cover evaluates the actual target
cells and may choose NAND/NOR, AOI/OAI, XOR/XNOR, MUX, or another exact cut; an
AXM node is never assumed to correspond one-for-one with a physical cell.

The freshly lowered subject is first reduced by one bounded functional-
reduction pass. Structural hash consing merges only syntactically identical
nodes, so bit lowering, operator recipes, and separately rewritten cones leave
behind nodes that compute the same function through different structure. The
pass nominates candidates by bit-parallel simulation, proves or refutes each
candidate against a class representative with `opto-formal`, folds every
refutation's boundary assignment back into the stimulus, and repeats for a fixed
round budget. A node is replaced only by a literal whose miter was proved
unsatisfiable; equal simulation signatures nominate, they never authorize. The
stimulus depends on boundary origin and word index alone, classes are emitted in
ascending node order, each class elects its lowest-ID member, and independent
class shards are proved in parallel and reassembled in shard order, so the
substitution set is identical across worker counts. Constants and inputs are
nominated alongside gates because a cone proved constant or proved equal to one
input removes its whole support. Reduction precedes the optimization portfolio
so every retained implementation is built from one duplicate-free subject.

MUX expansion is part of the one canonical optimization path, not a competing
implementation. Genuine MUX nodes are expanded into equivalent AND/inverter
structure, while XOR remains first-class, and the ordinary local normalizer and
structural balancer run after each of at most two expansion rounds. A second
round is attempted only when normalization creates another MUX. This exposes
NAND/NOR sharing to Liberty cover without recognizing an RTL pattern or
creating another synthesis pipeline. Cover still selects MUX cells, because it
matches cut truth tables against the target library rather than AXM node kinds.

Optimizing an un-expanded implementation beside the expanded one is not part of
the flow. It doubled every rewrite, cut, truth, and cover pass to produce an
alternative that mapping then discarded, and the retained subject arena carried
both. One path is the architecture: alternatives are justified by what mapping
selects, not by what the optimizer could have produced.

Small-support multi-output logic adds one bounded functional normalization:
complete truth evaluation, shared-cube factoring, root-to-root resubstitution,
then local AXM normalization. Global sharing census remains part of the ordinary
baseline; repeating it after global functional factoring would duplicate pass
ownership. The ordinary rewrite and functional result are
installed once into a shared hash-consed subject with common cut and truth
analysis. They remain equivalent implementations until Liberty covering;
primitive-weight estimates never decide which structure survives.

This functional path is selected solely when the complete region truth space
fits the fixed `u128` representation (at most seven inputs and at least two
roots). The bound is a representation invariant, not a QoR threshold. Regions
beyond it retain ordinary local rewrite. Within the bounded state space,
output ordering is derived from functional sharing, and the search budget is
divided among remaining roots so an early combinatorial search cannot starve
later roots.
Functional dependency is proved by bit-parallel separation before a projected
truth table is constructed, so rejected feature sets do not repeat the exact
projection work. No RTL operator or benchmark pattern is recognized. With
finite required times, Liberty constraint violation is compared first;
equivalent feasible implementations then minimize area-delay product, followed
by area, delay, and cell count as deterministic ties. Without a finite required
time, each bounded proposal receives exact reference recovery and the portfolio
minimizes mapped area before delay and cell count. Post-map MFS remains local
cell/MFFC cleanup and incrementally refines care-set partitions while visiting
larger input sets; it does not reconstruct a second global factoring engine.

Divisor collection enumerates the leaf subsets of a cut and asks the support
index which nodes realize each subset. Almost all of those subsets are the
support of no node, so the index carries an exact negative filter keyed by an
order-independent fingerprint of the subset: a clear bit is a proof of absence
and skips building and hashing the full key, while a set bit still requires the
probe. The filter changes cost only; the divisors found are identical.

AXM optimization is scheduled statically by a typed pass pipeline. Destructive
passes return a `TransformProduct`; `TransformState` alone composes node maps,
checks that active roots survive, and carries typed reusable analyses. Optional
passes return equivalent proposals. Iterative representation proposals use a
static specification that names the transform, its fixed round budget, and its
optimization policy; proposal-specific booleans and open-coded retry chains are
not part of the scheduler. The pipeline installs retained proposals once into
the shared graph. Independent implementation-cover recoveries
run as keyed composite tasks on limited views of the same worker pool; results
return in implementation order before the ordinary deterministic ranking step. The mapper covers the generic
implementation list with real Liberty cells. Timing-driven portfolios use
bounded flow ranking before exact recovery of the selected implementation;
unconstrained portfolios compare exact mapped area because that is their stated
objective. Pass names, remap
composition, analysis
invalidation, and proposal handling do not leak into candidate enumeration.
Dispatch remains monomorphic, with no per-node pass objects or virtual calls.

Boundary slices of the same source signal share one region-local backing port.
Overlapping views therefore produce the same AXM input IDs and structural
hashing can preserve cross-output sharing. A true cross-region value remains a
hard boundary; backing-port coalescing never follows a driver into another
region.

Multi-operand additive regions retain carry-save reduction and independently
choose Wallace or Dadda schedules according to the local objective. Fused
multiply-accumulate and related arithmetic recipes follow the same private
catalog path. The worker returns one portable `RegionCoverPlan` plus explicit
input and owner-output bindings; it never writes another region's arena.
Boundary observations and publication roots are distinct frozen identities.
An observation preserves the contract endpoint in the private Word import but
never enters the cover-input binding namespace. Its exact global producer is
the publication root, so a local simplification cannot reinterpret an owned
output as an imported copy of itself.
Before private optimization, each observable root receives a full-domain
publication obligation from the source Word graph. If that graph does not
prove the root to be a constant, an imported projection, or an already-owned
state artifact, the plan must retain a combinational artifact even when its
local care set reduces the function to a constant or pass-through. Local cover
may simplify the implementation, but it cannot weaken this frozen obligation.
### 5. Publish Deterministically

The coordinator reconstructs region artifacts in stable region order. The
scalar shell preserves ports, state, memories, source provenance, and boundary
wiring. Sequential endpoint reconstruction uses the same normalization rules
but includes region ownership in clock-gating bank keys, so publication cannot
create a cross-region sharing opportunity.

The scalar shell is substrate, not a second cover input. Private plans and
their bindings cross lowering together and are the only regional publication
source. No epoch repartitions the shell or attempts to rediscover private logic
from its endpoints. Plan inputs may resolve to frozen substrate nets; plan
outputs are owner write obligations. If an implementation output resolves to
the same substrate class as one of its inputs, publication keeps the required
cell on an artifact-local net and does not write that imported class. This is a
write-permission rule, not an alias-conflict repair heuristic.

### 6. Allocate Workers Without Oversubscription

Outer region parallelism is primary. The sole `ExecutionContext` schedules
keyed region tasks. Estimated work is converted to per-region worker limits by
deterministic Hamilton apportionment:

- many comparable regions each receive one lane and run concurrently;
- when only a few regions dominate the design, idle lanes are assigned to
  those regions for inner cut/rewrite work;
- nested work uses a limited view of the same pool, never a private pool.

One worker owns one mutable output row or analysis builder. Shared inputs are
immutable. Results are returned in keyed order.

### 7. Commit And Measure

Every plan uses artifact-local cell and net identities. The coordinator builds
ports, retained instances, clock/memory infrastructure, lowered-value bindings,
and static boundary aliases once. It then:

1. orders plans by stable region row/identity;
2. validates plan, decision, revision, and contract generations;
3. prepares independent sequential and regional mapped artifacts;
4. appends the first generation in one checked `RegionDelta`;
5. records one stable footprint and explicit region/provenance owner per artifact;
6. freezes observable port-net identity and retained source-pin direction for
   the complete mapped generation;
7. creates one sparse MMMC owner service over the complete generation.

No worker allocates final IDs with atomics. A dirty region is replaced through a
new delta that tombstones only its previous footprint and appends new slots;
surviving IDs never move while timing is live. Connectivity and serialization
are therefore independent of worker completion order. A footprint retains both
internal and external nets, and replacement snapshots take the union of the old
and new footprint. The transaction core rejects any existing cell or net outside
that explicit read/write set.

The committed generation is evaluated by authoritative global timing and power.
Every epoch derives checkpoint WNS, TNS, and violating-endpoint count from the
complete mapped MMMC owners, including the materialized sequential shell.
Projected regional timing guides the first cover but never decides which mapped
checkpoint is best and is never reported as committed-candidate slack.
Measured boundary responses may reallocate contracts and mark dirty regions.
An epoch updates the dirty plans' explicit contract and context rows, then
replaces only those plans' owned footprints. It does not reopen private cover,
repartition the scalar shell, rebuild binding identity, or change the frozen
topology. Clean plans and footprints remain untouched. A future incremental
re-cover feature must retain the complete frozen private IR and consume the
same explicit ownership and binding provenance; reconstructing it from the
global shell is forbidden.

One incremental region edit reuses the retained topological order and its
dependency plan whenever the edit adds no dependency edge and no net. Removing
an arc cannot move a net earlier than a live predecessor, and a plan that still
lists a removed edge is conservative rather than wrong: the traversal counts
only dependencies it actually scheduled, so an extra edge recomputes a sink from
its live predecessors instead of deadlocking on a dead one. Appending a net does
force a rebuild, because the plan's position arena no longer covers the graph.
Rebuilding is `O(nets + arcs)` per edit per timing view, so making it conditional
is what keeps post-map candidate evaluation proportional to the edit rather than
to the design. Cell resizing and constant-register removal both reuse the plan.

There is one MMMC fact source, but acceptance authority is deliberately scoped
to its decision domain. Initial mapping has one total `MappedObjective` used
only to retain or restore an epoch checkpoint. Its timing order comes from the
complete mapped MMMC quality; boundary-contract metrics identify local response
and break exact global timing ties rather than replacing full-design STA.
Post-map has one transaction gate that first rejects any edit that removes a
frozen boundary net or changes the unique driver of an affected observable
output, then compares full-design STA/DRC and physical metrics before commit or
rollback. The connectivity check is incremental over the transaction's exact
affected-net set; the complete frozen contract is revalidated at publication.
These are not competing global
objectives: boundary legality is not full-design DRC, managed implementation
cost is not the count of every live substrate cell, and an epoch tie requires a
stable key while a no-change post-map candidate is rejected. Sharing a nominal
objective type would erase those domains and change acceptance semantics; both
authorities consume the same `MmmcTiming` owners instead.

Each effort has a deterministic epoch bound. The best structurally legal
plan/binding checkpoint is retained by a total order. If the last epoch is not
best, only changed footprints are restored with region deltas. Wall time and
observed RSS do not affect convergence.

After post-map consumes the shared timing service, the publication barrier
consumes the sole mutable mapped owner, completes every fallible validation,
capacity, revision, and generation step, then repacks live cell/net slots
exactly once with forward read/write cursors over the existing typed arenas.
Before repacking, every observable output bit must have exactly one physical
driver derived from a top-level input, explicit constant, target-library output
pin, or retained-instance output contract. Missing and multiply driven outputs
are invariant failures, so incomplete regional publication cannot escape as a
successful gate netlist.
Port, retained-design, constant-driver, external-net, cell, pin-owner, and
intrusive-adjacency references cross the same compact `u32` translation, and
`ImplementationDb` receives the matching cell remap. Publication neither
reconstructs `CellSpec`/`String` payloads nor freezes or copies the name table:
the published owner is not cloneable, all downstream name access is borrowed,
and the publication barrier prevents further interning, so its existing
frozen-plus-delta stores are already an immutable zero-copy name view. Unused
stable `NameId` entries may remain until a later name-storage design explicitly
supports ID-preserving GC. Tombstoned topology itself never enters the
checkpoint artifact.

### 8. Build And Optimize The Mapped Netlist

The direct regional commits form one flat `MappedNetlist` with
typed cells, nets, ports, pins, target bindings, and interned names.
`ImplementationDb` stores many-to-many provenance separately.

The mapped-generation handoff also builds one compact, generation-stamped
fanout/load profile. Each multi-sink mapped net has its complete sink count,
abstract fanout load, and summed mapped-pin capacitance. This is a property of
the committed topology, not something inferred later from whichever path happens
to be worst.

Post-map is one large stage, but its internal topology order is mandatory:

1. initial MMMC STA classifies path increments as cell arc, interconnect, or
   boundary contribution;
2. whole-net HFNS takes the union of every negative-slack mapped net from every
   enabled view and every mapped net with an explicit transition,
   capacitance, or fanout violation, then consumes each net's complete sink
   set and actual early/late mapped-pin loads;
3. read-only workers plan balanced trees per net and stable reduction folds all
   eligible trees into one atomic fanout forest; every active view must provide
   complete cell-arc and wire evidence, leaf groups are load-balanced, and
   branching search follows distinct topology depths instead of scanning every
   possible fanout;
4. residual electrical violations are planned once per violating source net
   and committed as generation-wide forests until exact STA reaches a fixed
   point or no legal improvement exists;
5. global STA is rerun on the legalized topology;
6. only residual critical branches may use driver cloning, and those branches
   are committed as one atomic clone forest;
7. bounded MFS, compatible sizing, and pin swapping run on that topology;
   each sizing frontier is one atomic replacement forest and critical pin
   permutations are one atomic pin-swap forest, not per-cell STA searches.

All seven steps consume the same frozen observable-connectivity contract.
Boundary nets remain stable identities: MFS may replace the cell driving an
output net, but a constant/wire reduction cannot rewire internal consumers and
discard that output net. Candidate generation prunes that form, while the
shared transaction gate remains the authoritative defense for every post-map
pass. MFS starts from immutable top-level, retained-design, constant-driver,
and external nets, then adds every net whose incident cells have different
exact implementation owners. Cell names and hierarchy strings never infer
ownership or optimization permission.

This topology order runs whenever an MMMC timing owner exists, whether or not
the scenario has explicit optimization constraints. Constraints add
feasibility measurements to the shared transaction objective; they do not
select a second post-map flow. Physical recovery follows legalization and
timing preparation and preserves any measured closure through the same commit
gate.

Mapped resynthesis seeds only from a measured dirty cone: the cells this
closure has already edited, and the retained non-region instances cover never
costed. A region-owned cell was selected by cover under the same care set and
the same library, with exact-area recovery already applied, so re-deriving it
after mapping repeats a decision that has not changed. Sweeping the whole clean
netlist is not the default; it spends the largest post-map budget on cells whose
context never moved.

A register whose reachable value is one constant is removed before that
resynthesis. The proof substitutes the register's own outputs with their reset
value, folds the bounded combinational cone behind its input pins, and requires
the next state to stay at that value for every assignment of the nets the fold
could not resolve. The fold follows only nets the register's own value can
still reach, which is what bounds it and what makes the answer meaningful;
every other net is one enumerated leaf. This is constant folding over structure, not
inferred state equivalence, so it stays inside the rule that keeps full-domain
state sharing out of the tree. Independent removals commit as one transaction
because each transaction pays one incremental-STA update; a round repeats only
over the cells the previous round reached.

Cloning or sizing before whole-net HFNS is forbidden: removing a few sinks from
a thousand-sink net merely moves the worst path to another sink. Forest
evaluation normally performs one STA for the complete violating-net set. If a
forest is rejected, stable net order is bisected deterministically; a net's
sink set and tree are never split. Residual cloning follows the same
whole-batch-then-bisect rule instead of rediscovering one branch after every
STA. A rejected sizing forest advances the semantic frontier instead of
enumerating binary subsets until the global evaluation budget is exhausted.
Pin swapping likewise evaluates one stable permutation per eligible critical
cell in a single transaction. Every accepted transaction updates mapped
topology, timing, power, and provenance together.

Topology synthesis and electrical legalization finish their finite forests
independently of the deterministic QoR-search evaluation budget. That budget
applies only after electrical topology is established; effort cannot truncate
legality work or change its ownership.

### 9. Seal Timing And Reports

Timing consumes the sealed mapped generation and exact target timing arcs.
`report_timing` is a real mapped-path report: it resolves `-from`/`-to`
collections, supports max/min delay selection and a bounded global worst-path
count through `-max_paths`, and prints launch/capture objects, pin-by-pin typed
increments, requirements, slack, and
unconstrained-path status. Interconnect steps retain fanout, load, resistance,
wire delay, annotated parasitic delay, and derate, so a QoR failure can be
assigned to a cell arc, wire model, or boundary model before optimization is
changed. Unsupported report modes fail explicitly instead of changing meaning.

## Canonical Representations

```text
RtlModule
  = structural WordModule + sealed ProcModule

SynthesisRegionGraph
  = stable semantic identities + dense rows + typed boundary CSR

regional cache records
  = one context-keyed construction decision and optional compact plan per region

canonical Boolean subject
  = one compact mixed-node graph for the selected target construction

AnalyzedRegionCover
  = task-owned cuts, bindings, and selected cover for one immutable slice

RegionCoverPlan
  = compact portable selected topology + boundary response + stable keys

MappedNetlist
  = the sole published implementation topology
```

Hot structures use typed 32-bit IDs, contiguous arenas, SoA/packed rows,
interned strings, bit masks, and bulk traversal. Object-per-node heap
allocation, duplicate strings, pointer-rich ownership, and global locks are
not acceptable scale foundations.

The sealed timing graph stores its base net names in one compact `NameTable`
behind a shared immutable allocation.
`TimingNetId` remains insertion-stable, exact external lookup uses the
interner's hash index, and sibling MMMC views share the same frozen byte arena.
An uncommitted region owns only its appended names and a compact u32 ordering;
rollback truncates that append layer and compaction seals it into the next
shared base. A parallel `Vec<String>` or name-sorted full-net index is not part
of the resident timing topology.

The retained timing design follows the same boundary. Public
`TimingDesign`/`TimingInstance` values are owned construction and regional
delta records; a sealed model consumes them into one shared `NameTable`, a
contiguous `CompactInstanceRow` arena, and one contiguous pin/net connection
arena. Sibling views share that base, while a region owns only sparse changed
rows. Exact instance lookup probes the name table and its compact position
rows, then checks only the sparse overlay, so SPEF pin binding never scans the
complete cell arena. `TimingDesignView::to_owned` is the explicit full
materialization boundary.

Validated parasitics use one `NameTable` and contiguous net/node/resistor/
connection arenas per immutable store. Import sorts `RcNetwork` values first
and streams each analyzed net directly into that store; it never retains a
full `ComputedNet` generation or builds copied `String` set/vector/map layers.
Incremental reads keep the original store as a shared base and append immutable
sorted runs. Runs are geometrically size-tiered by their exact compact-store
weight, so only comparable tail runs merge and no sequence of small updates
rebuilds the cumulative delta. A heap performs deterministic multiway logical
traversal with newest-run precedence; once the runs cover every base net they
are promoted to a single base. Exact lookups compare resolved text, and
`instance/pin` queries binary-search borrowed byte parts without joining a
temporary string.

Closure endpoint adjacency uses the same packed-row representation for both
net and stable-instance lookups. Region edits materialize only touched rows and
retain stable typed endpoint IDs across rollback.

Each timing view keeps propagated arrival and required state in
`ArrivalSlotStore` and `RequiredSlotStore`. A checked u32 slot identifies one
net edge; the common first state lives in dense SoA tag/origin/value columns,
transition presence uses a bitmap, and only genuinely multi-tag rows allocate
ordered sparse overflow. Scalar optimization views allocate neither a path-ID
column nor a predecessor arena. Owned row values exist only at worker,
publication, and rollback-journal boundaries, and dependency publication writes
the column stores directly instead of restoring a resident object-per-net
container. Path compaction and deferred required-time synchronization reject a
live region edit, so neither remapped path IDs nor a recomputed backward
frontier can cross its rollback journal. Resident accounting charges actual
column and overflow capacities, including optional tracked-path storage.

Session object reconciliation follows the same rule. A replayable design
inventory keeps only canonical `NameId`/u32 order vectors and derives pin full
names through one reused scratch string. Registry planning marks retained
objects only within participating designs and represents removals as sorted
u32 live slots. The fallible preflight interns names, verifies an exact source
digest, freezes only compact `ObjectKey` additions, and validates every UID and
arena bound. Timing, power, and collections prepare sparse edits by scanning
the smaller of the removal set and their reverse index. Once those tokens and
the registry token exist, commit performs no source callback or recoverable
operation: dependent owners consume their sparse edits first and the registry
publishes the prevalidated removals/additions last.

## Cache And Incremental Reuse

Process-scoped engine caches store only immutable content-addressed recipes and
target-derived catalogs:

- Word rewrite recipes;
- Boolean rewrite recipes;
- target-derived mapping catalogs.

Regional construction decisions, compact plans, and boundary responses instead
belong to one `IncrementalSnapshot`. A successful `SynthesisResult` owns that
snapshot; when the mapped artifact is invalidated, the session moves the same
snapshot into its `DesignRecord`. `SynthesisRequest` may borrow exactly one prior
snapshot, and regional search reads its strictly context-sorted records by
binary search. It never installs them into `SynthesisEngine` or another
process-global container.

Keys include semantic identity, boundary schema, target/scenario generations,
effort, and relevant predecessor summaries. Raw arena IDs, pointers, complete
design clones, and whole-generation containers are not cache payloads.
Checkpoint decode and the synthesis boundary validate the source identity,
canonical record order, and typed payloads before reuse.
Portable plan records order boundary metadata and measured responses by
semantic port key, order each sparse response strictly by `(scenario,
timing_tag)`, and restore only when both sequences exactly match the rebuilt
contracts.

Plans, epoch journals, and regional cache records share immutable topology,
measurement, implementation-census, and decision slices by `Arc`.
Checkpoint publication and reconstruction clone only those owners, never their
payload bytes or per-cell records. Resident accounting charges each reachable
allocation identity once even when several explored contexts share it, and
treats an `Arc`'s two reference-count words plus inline payload as one
allocation before applying allocator overhead. Physical boundary repair remains
part of the published mapped artifact and its implementation ownership, not a
second portable topology reconstructed by the next synthesis run.

The checkpoint wire stores design owners, not their derived query indexes. A
`DesignRecord` keeps its `DesignIndex` while resident, but serialization omits
that complete duplicate of the source or mapped artifact. Decode first
validates every source, synthesis or incremental owner exactly once, compacts
the validated synthesis artifacts, then linearly rebuilds and validates the
mapped index when the record selects its synthesized view, or the source index
otherwise. The rebuilt store is sealed behind a checkpoint-local
`ValidatedDesignStore` with no mutable access. Preparation consumes that proof
while checking only relationships to session revisions, the object registry,
libraries, timing, parasitics, and power before atomic installation. This is a
schema boundary: older payloads are rejected rather than decoded through a
compatibility/default path.

The physical checkpoint and canonical-fingerprint encoding is owned by
`opto-archive`. Existing domain-specific Serde views first lower into a flat
post-order node arena; rkyv archives that arena with bytecheck, little-endian
scalars, 64-bit relative pointers, and unaligned file access selected
explicitly as format features. Decode validates rkyv pointers and byte ranges,
then verifies the archived arena is a single bounded-depth tree whose children
strictly precede their parents, before allocating the owned Serde model. The
session subsequently performs its domain validation and atomic publication as
described above. This separates stable domain projections from the physical
archive implementation without retaining the former bincode format or an old
checkpoint compatibility path.

Exact object lookup is likewise derived from the resident `DesignIndex`, not
from repeated arena scans. Each source-ordered port, net, cell, and used-signal
arena owns a compact `NameId`-sorted row index built on first exact lookup;
mutable arena access invalidates only that derived index, cloning drops it, and
checkpoint serde remains a transparent row sequence. Session queries use these
typed indexes to establish semantic existence and borrowed object-registry
lookups to obtain durable identity. This distinction matters for source nets:
their registry identity may still be interned lazily, but not-found and
port-versus-net ambiguity never depend on which query happened first.

The checkpoint-only RTL view also preserves source-origin ownership. Within
one RTL record the first `(file, construct)` pair defines a dense origin ID and
later spans encode only that ID; restore rejects forward, sparse, or duplicate
definitions and reuses the same `Arc` for every reference. The scope is owned
by the synchronous checkpoint field wrapper and rejects nested streams, while
ordinary `WordModule`, `ProcModule`, and `RtlModule` serde remains
self-contained and unchanged.

A target `RegionCoverPlan` stores only its portable cell topology. Input and
owner-output bindings are a separate frozen object carried with the plan
through global lowering and publication. A cache record is accepted only after
the current private source semantics reconstruct the same topology and binding
obligations; cached topology never supplies connectivity or ownership proof.

Scheduling chunks are not cache identities. An unrelated edit preserves clean
regions; a local edit replaces only the affected connected region set.
A successful synthesis run publishes only its current base contexts plus
contexts explored by its current epoch journal. A failed synthesis run mutates no prior
artifact or engine state. Checkpoint installation likewise installs persistent
artifacts without a cache-restoration side effect. Regional cache memory is
therefore bounded by artifact/session reachability rather than an arbitrary
process-wide cap or eviction policy.

## Determinism

For identical complete inputs and effort, every supported worker count must
produce identical:

- mapped cells, nets, pins, connectivity, IDs, and names;
- construction decisions and plan keys;
- implementation provenance;
- remaining violations and diagnostics;
- timing, area, power, and cell-composition reports;
- reachable checkpoint records.

All reductions use total orders. Floating-point values reject NaN, use
canonical units, and are folded in stable order. Hash equality is followed by
identity reconstruction where collision would violate ownership.

## Resource Bounds

Peak memory is bounded by ownership, not by refusing work:

- one compact Word revision;
- one canonical Boolean subject for the selected construction;
- packed cuts and truth rows partitioned by region;
- region-local sorted Word-value bindings and scratch proportional to values
  touched by active region tasks;
- one compact plan and binding per region;
- one retained best plan/binding checkpoint;
- one prior artifact-owned incremental snapshot borrowed read-only while
  building its replacement.

Opto does not retain an EHM, a full-design alternative graph, Top-K mapped
designs, Pareto portfolios, cloned full mapped candidates, or a process-global
regional cache. A replacement artifact does not copy unreachable historical
contexts forward. No cache admission, entry cap, or victim policy can alter
semantic validity.

MMMC timing is a synthesis-time service, not a published artifact owner. Its
resident bytes therefore do not contribute to `SynthesisResult` resident
memory. `SynthesisMetrics` instead checkpoints its resident footprint,
concurrent construction-scratch high-water, and total construction high-water;
artifacts without a timing summary must record zero for all three values.

Timing views are grouped by `TimingTopologySchema`, whose fingerprint only
narrows candidates and whose canonical structural bytes provide exact
equality. Each group builds one leader and then forks followers from its
`PreparedTimingTopology`; both phases use the same deterministic two-task
limit and publish in analysis-view order. The nested serial context applies
only while one view is being constructed; each completed incremental owner
retains the caller's execution context for later dirty-cone propagation.
Immutable design records, graph columns, packed adjacency, and dependency-plan
allocations are shared, while per-view arc values, parasitics, loads, and edit
state stay independent. Variable graph, closure, and instance-net rows are
split into immutable 4096-row CSR pages behind a dense page table. A regional
edit clones only a touched row into that page's sorted override vector; forks
continue sharing every untouched page. The fallible pre-publication commit
barrier first builds every replacement for each dirty arena, then installs only
those pages and clears their overrides. Rollback restores rows through the same
page-local path and seals them again, so overlays never accumulate across
transactions. Streaming page builders avoid a retained `Vec<Vec<_>>` generation,
and a page's local u32 offsets do not impose a whole-design value-count limit.
Resident accounting emits one identity per immutable page, so sharing is
deduplicated even when a regional edit detaches only one page. Construction metrics
record explicit model/graph build scratch together with topology-group, task,
and ordered-result scheduler storage. They take the largest separately checked
grouping, leader, follower, and final-resident phase bound; these are
deterministic logical upper bounds, not allocator telemetry.

Fixed-width stable-ID columns use the sibling `PagedCowVec` representation:
mapping directions, instance positions, instance-to-library-cell links, and
the bit-packed instance-net liveness words are split into always-shareable
4096-value `Arc` pages. A fork clones only the page table. The first write to a
page shared with another view fallibly clones that single page; an already
exclusive page is updated in place, including repeated tail appends. These
columns have no sparse tree overlay and no commit-time whole-column rebuild.
Shared-memory accounting likewise emits one identity per value page.

Initial mapped timing construction scans live mapped nets once and seals both
dense mapping directions directly into page-shared dense columns. The full
generation never materializes a mapped-ID `BTreeMap` or allocates names for
already named nets; sparse maps remain restricted to regional edit deltas.
Mapped cells stream directly into the compact interned instance/pin arena and
the stable-ID instance-net CSR; the product path never first materializes an
owned `TimingDesign`. Connection rows retain pin identity only, while graph CSR
rows and one shared net-name table are the canonical connectivity source.
External instance and parasitic sink resolution reuse the resident exact-name
index, including stable first-row behavior for duplicate instance names, so
validation has no all-instance sorting or copied-name phase.

Sequential substrate naming probes the Word module's existing instance-name
index directly. Generated names are injective in `(operation ID, role)`, so
materialization never copies every source instance name into a second owned
set merely to avoid collisions.

Checkpoint name tables preserve their sequence wire format but deserialize
directly into the final `NameStore`: each incoming `String` is validated and
interned before the next element is read, so restore never retains a duplicate
full `Vec<String>` beside the compact UTF-8 arena.

Object-registry records likewise deserialize in stable UID order directly into
the final slot arena and its UID, locator, and design indexes. Semantic errors
discard that partial owner while the bounded decoder consumes the remaining
wire; no complete `SnapshotRecord` array coexists with the rebuilt registry.
Cross-owner validation uses a one-byte registry-slot marker and borrowed names,
not expected/actual trees of owned object locators.
Exact endpoint resolution probes the current design's typed locator keys and
the global clock key directly; it never scans the registry or materializes
unrelated locator strings.

## Public Interface Policy

The public Tcl surface keeps flat action commands and a coherent typed object
and property model. Opto's own documentation is authoritative; another
product's lifecycle or aliases do not define compatibility requirements. An
undocumented divergence between implementation, tests, and this contract is a
defect.

- **No separately publishable generic netlist.** Technology-independent
  optimization and semantic architecture work are mandatory stages before
  target mapping, never a fallback selected by an empty library. Word-level
  normalization runs before regional planning and region-private restructuring
  runs immediately before Liberty covering. A `synth` without a target library
  is therefore an explicit error.
- **No inert compatibility options.** Opto does not accept flags that
  cannot affect behavior, including report pagination switches, switching-
  activity verbosity, and parasitic database write-back controls. Implemented
  options have observable semantics; unavailable behavior is rejected instead
  of being silently acknowledged.
- **One database model and one synthesis operation.** Opto uses typed `get_db`
  / `set_db`, `read_hdl`, and `elaborate`, and exposes one `synth` operation.
  Reports remain flat. Database writes are schema-declared,
  transactional mutations rather than unrestricted path-based assignment.
  One sorted root-property catalog declares each canonical name, value type,
  readability, writability, and help text. One sorted object-query catalog
  declares each class and whether related-object and filter queries are
  supported; `get_db` and the flat `get_*` commands dispatch through the same
  typed query kind.
  RFC 0010 is the normative command-design policy.
- **One HDL frontend lifecycle.** Rust and Tcl callers ingest source units and
  then elaborate a named top. Test fixtures use that same two-stage path; no
  one-shot parse-and-publish frontend remains alongside it.
- **Command failures remain failures.** `read_sdc` rolls back the constraint
  checkpoint and raises a Tcl error when any command in the file fails. The
  caller cannot accidentally continue after a malformed or unsupported
  constraint file by ignoring a Boolean result.
- **Typed command variants.** Standard sibling commands may share an argument
  schema, but their semantic operation is bound as a typed variant when the
  catalog is generated. Command handlers do not infer their operation from a
  command-name string. Scalar options are rejected on their second occurrence;
  only fields declared as repeated accept multiple occurrences.
- **One complete command schema.** The generated catalog owns ordered and
  independently named positional fields, their numeric or textual lexeme,
  option identity and repetition, conditional positional arity, validation
  behavior, and user-facing help. Generic parsing and SDC validation execute
  those typed policies and contain no command-name exceptions. Public commands
  must declare an explicit summary and lifecycle requirements; every argument
  has nonempty field help. Commands with positional arguments or more than one
  option must declare an executable example, and variants override summary,
  requirements, and example independently where their public wording differs.
- **Unambiguous option termination.** Before `--`, an unknown hyphen-prefixed
  word remains an unsupported option and receives spelling help. A negative
  numeric word is accepted only where the next declared positional field is
  numeric. `--` terminates option parsing so textual object names and paths
  beginning with a hyphen remain representable.
- **One activity-target resolver.** Stored switching activity uses persistent
  typed port or net identities. Every report and synthesis-scenario path binds
  those identities through the timing generation's compact object index;
  ports expand through the model's port-net relation. A target removed by
  optimization contributes no timing net in every caller, while conflicting
  live annotations are rejected after expansion. No caller scans all mapped
  nets or interprets generated net-name spellings.

## Rejected Architectures

- **Global EHM/e-graph ownership:** alternatives and mutation scope grow across
  the whole design and defeat predictable regional memory ownership.
- **Uncontracted independent region-local target lowering:** cloning arbitrary
  cones without stable anchors, boundary contracts, private ownership, and
  deterministic assembly duplicates canonicalization and makes aliases or QoR
  depend on commit order. RFC 0007's region-private IR is the contracted
  replacement and is not this rejected design.
- **Top-K/Pareto regional portfolios:** multiply lowering and cover work and
  turn region mapping into speculative whole-implementation search.
- **A “winner” chosen after workers race:** leaks scheduling into semantics and
  has no coherent region-level optimization meaning.
- **Runtime memory admission:** rejects valid semantics based on a noisy
  estimate and cannot serve as an industrial capacity model.
- **Thread-count-dependent partitioning:** destroys stable identity and
  reproducibility.
- **Shared mutable target graph:** requires global locking or racy atomics and
  makes rollback ambiguous.
- **Source hierarchy as a hard wall:** loses cross-module optimization without
  providing a reliable timing boundary.
- **Ad-hoc mapper extraction:** reintroduces semantic discovery after ownership
  and construction should already be frozen.
- **Fixed-size repair super-regions:** make an unrelated scheduling chunk look
  like a synthesis semantic boundary and can duplicate multi-region cells.
- **Sink-count-only HFNS:** ignores MMMC pin loads and Liberty characterization
  domains, so a nominally balanced tree need not be electrically balanced.

## Current Conformance

| Contract | Current tree |
|---|---|
| Single `opto` production entry | Implemented |
| Every semantic opportunity has one explicit region owner | Implemented |
| Stable typed region graph over the synthesis-root closure | Implemented |
| Timing-driven cone claiming and fixed-round local matching | Implemented |
| Architecture-independent partition and budget estimates | Implemented |
| Separate region anchors/revisions and boundary identities/revisions | Implemented |
| Word graph as the sole pre-cover connectivity and dataflow authority | Implemented |
| Absolute locally dependent timing budgets | Implemented |
| Region-private Word optimization and architecture selection | Implemented |
| Private muxed arithmetic, CSA, Wallace/Dadda, and fused MAC; owner-confined FSM and sequential sharing | Implemented |
| No memory admission mechanism | Implemented |
| Parallel private technology-independent optimization and Liberty lowering/cover | Implemented |
| Proof-backed AXM functional reduction before the optimization portfolio | Implemented; shard-parallel, deterministic across worker counts |
| Unobservable mapped logic removed before closure evaluates it | Implemented |
| Feedback-enable recovery guarded by a value-level equivalence proof | Implemented; reset registers are declined, see Known Architectural Gaps |
| Clock gating enabled by default | Implemented |
| Mapped resynthesis scoped to a measured dirty cone instead of the whole netlist | Implemented |
| Constant-register removal proved through a bounded influence cone | Implemented; one batched transaction per round |
| Weighted outer/inner worker allocation | Implemented |
| Direct transactional region artifact commit | Implemented |
| Single-atom mapped ownership and edge-owned boundary repair | Implemented |
| Sparse boundary measurement and bounded feedback | Implemented |
| Selected sequential clock-to-Q/setup projection plus exact mapped checkpoint timing | Implemented |
| One shared sparse MMMC owner service | Implemented |
| Transactional mapped optimization and exact STA | Implemented |
| Structured source diagnostics and successful frontend warnings | Implemented across CLI/session, HDL, Liberty, formats, timing, power, and synthesis domains |
| Opto `report_timing` core path report | Implemented; unsupported advanced report modes are explicit errors |
| Flat Opto command policy | Registered parsing, help, validation behavior, and current root/object database catalogs use generated typed schemas; scenario and structured-report completion remains pending |
| Same-host real medium-scale regression guard | Implemented for 14 executable 353–10,225-cell cases selected from a pinned 30-case public pool |
| Multi-million-gate runtime/RSS/QoR qualification | Not yet demonstrated |
| Versioned public scale-suite performance targets | Target; not yet demonstrated |

## Register Control Ownership

Each register control has exactly one owner. The frontend knows each register's
enable exactly, because `always_ff` lowering emits
`RegisterOp { d, enable, resets }` with the enable taken straight from the branch
condition. Control lowering normalizes resets and composes a synchronous reset
into that enable but never consumes it. Clock gating and enabled-cell selection
consume exact enables. `expand_unsupported_enables` is the single, last site that
turns a remaining enable into a next-state mux, so a register's held value is
read through a wire with exactly one driver and denotes the register's output
rather than whichever assignment ran last.

The earlier pipeline discarded the exact enable in control lowering and then had
feedback-enable recovery pattern-match it back out of the next state, with the
expansion implemented twice. Recovery decided that a path holds by comparing it
against a read of the register's target signal, and equated two reads of one
signal even though a clocked process that assigns on a reset branch and on an
enable branch produces reads that denote different values. On the Ibex SKY130
case that inferred an enable narrower than the design's and co-simulation
diverged from cycle 116. The value-level equivalence proof that guards recovery
does not close that hole either: the CNF encoder gives one variable per signal
bit for every read of that signal, so it cannot distinguish reads taken at
different program points. Recovery is now reachable only for a register whose RTL
writes its enable as a mux rather than as a branch.

Letting exact enables reach clock gating exposed a second defect, in owned
combinational dataflow rather than in sequential mapping. `read_signal_bits`
charges a signal read to its canonical representative, so a wire whose readers
were all substituted is judged removable, but the substitution loop rewrote only
operation operands and connects. Instance connections and memory ports kept
naming the wire whose driver had just been dropped. Only an enable read solely by
an integrated clock gate could expose it: on Ibex three load-store-unit gates
took `ctrl_update`, `rdata_update`, and `addr_update` from undriven nets, so
those banks never updated. Both dataflow entry points now commit through one
`commit_representatives`, which substitutes through `rewrite_value_uses` — the
single definition of every Word IR value read — before dropping connects.

With both fixed, clock gating is on by default and gates 24 register banks on
Ibex SKY130 instead of 6.

## One Boolean Implementation

Technology mapping covers one AXM implementation. The subject used to carry a
portfolio: a PLA-based multi-output resynthesis proposed an alternative, the
pipeline installed every proposal into one hash-consed graph, and the cover
selector covered each implementation and ranked the results. The alternative
only ever existed for a region whose whole subject had at most seven primary
inputs, which no real design reaches, so the portfolio ranked one member and the
machinery cost more to carry than it could ever return. RFC 11 keeps a choice
graph on the roadmap; when it lands it will nominate choices inside one subject
rather than cover whole implementations against each other.

## Window Care Sets and Exact Recovery

Two analyses decide what they need before they compute it, because doing so
bounds the work rather than trimming it.

A window care set projects each of a node's cuts onto the largest cut that is
not the node itself. A cut the window does not contain is projected as fully
cared, so its leaves are never read back. Building the truth tables first meant
observing those leaves anyway, and observing a leaf the window does not reach
expands its whole cone, past the window's inputs and on toward the primary
inputs. Coverage is therefore decided first, in cut order and short circuiting,
so the traversal budget is spent on the same leaves either way; only the cones
outside the window go unevaluated.

Exact area recovery scores every viable candidate of a slot. It used to score
one by installing the choice and immediately removing it again, which walked the
newly activated cone twice and wrote every reference count on the way. A slot is
charged exactly when it is unreferenced and the trial has not reached it yet,
which visited marks decide without touching the cover. The trial walks and sorts
its frontier exactly as the committing update does, so the two sum areas in the
same order and agree bit for bit.

## Known Architectural Gaps

The remaining unproven product targets are multi-million-gate runtime/RSS and
public-suite QoR/runtime comparison. Those require the benchmark evidence
below, not compatibility paths or additional ownership models. Full-domain
state equivalence sharing is intentionally absent until arbitrary initial
state, reset, enable, and clock semantics can be proved rather than inferred
from locally equal data inputs.

## Qualification Contract

Every QoR or performance change must use repeatable inputs and record:

- exact source revision, top, defines, libraries, constraints, and thread
  count;
- wall time, CPU time, and peak RSS by major stage;
- mapped area, cell count, and cell composition;
- WNS, TNS, violating endpoints, transition/capacitance/fanout violations;
- deterministic netlist and report fingerprints;
- failure diagnostic quality.

Required scale tiers are:

1. unit and semantic microcases;
2. small open cores for rapid regression;
3. a production-shaped core large enough to expose partition and memory
   behavior;
4. hundred-thousand-, million-, and ten-million-gate stress designs.

Commit-to-commit synthesis changes use the pinned public medium-scale gate in
`benchmarks/real/gate.toml`. The gate permits local trade-offs but rejects both
an aggregate geometric-mean area or critical-delay regression and a per-case
tail beyond the declared limit. Each version runs once per case, and independent
cases use CPU-budgeted parallelism. Concurrent wall time and RSS are diagnostic
only; repeated serial performance measurement belongs to the dedicated runtime
benchmark. Tiny semantic tests never contribute to this quality decision, and
policy thresholds live in the manifest rather than in synthesis code.

Published comparisons use identical RTL, Tcl intent, Liberty, SDC, wire/RC
inputs, machine class, and usable thread count. Every published result records
the exact command, Rust toolchain, `--release` build profile, binary SHA-256,
input revisions or SHA-256 checksums, worker count, and host information.
Development-profile results are not accepted for publication. Inputs must be
redistributable or fetched from a public checksum-pinned source. A performance
target is accepted only when geometric-mean end-to-end runtime improves without
a material QoR cliff and no individual scale tier violates its versioned RSS
ceiling. A fast frontend on a small non-production circuit is not evidence for
the target.

## Implementation Policy

New work extends these owners and invariants. If an optimization needs a new
representation, it must define:

- semantic owner and lifetime;
- typed identity and generation;
- region boundary behavior;
- parallel read and deterministic commit model;
- peak-memory bound;
- exact benchmark and diagnostic contract.

If those cannot be stated, the optimization is not ready for the production
flow.

These contracts must also be recorded at the code boundary that enforces them.
That boundary may be a crate API, a restricted interface between internal
modules, a core type, or a private algorithm. Rust visibility is not a proxy
for architectural importance: ownership, identity, generation, deterministic
publication, rollback, invalidation, unit, and bounded-work assumptions remain
documentation obligations even when their implementation is crate-private.
