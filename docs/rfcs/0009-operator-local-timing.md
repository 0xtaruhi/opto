<!-- SPDX-FileCopyrightText: 2026 Zhengyi Zhang -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# RFC 0009: Operator-local timing and region-local architecture selection

- Status: proposed
- Author: Zhengyi Zhang
- Date: 2026-08-04

## Summary

Opto already has a substantial implementation-candidate pool. The arithmetic
provider alone declares 33 recipe IDs, including multi-operand
CSA/Wallace/Dadda implementations. Selection nevertheless receives one scalar
delay budget per region and applies it independently to every operator. Equal
operators in the same region therefore receive the same answer even when they
sit on materially different paths.

This RFC replaces scalar selection with a bounded architecture solve over the
complete private region:

1. a pin-and-arc timing graph preserves every scenario, check, corner, and
   transition lane, including sequential, latch, and memory endpoints;
2. target providers characterize candidate-specific word-pin timing, area, and
   electrical behavior at explicit slew/load operating points;
3. one regional problem selects operator, sequential, and memory
   implementations together;
4. a deterministic bounded solver proposes vectors, while a complete
   slew/load-coupled regional evaluation alone decides feasibility;
5. mapped timing remains the final QoR oracle.

The solve is region-private. It neither repartitions nor changes a boundary.
This is compatible with RFC 0007, which forbids a design-wide pre-freeze
architecture solve but permits bounded work within a frozen region.

The semantic operator representation remains technology-independent, but
`synthesis` requires a bound target library. Candidate selection and Liberty
mapping are therefore later stages of the same invocation, not a second backend
or a remappable generic checkpoint product.

## Motivation

### A region scalar discards the information selection needs

`RegionContractSet::delay_budget(row)` returns one `Option<f64>` for a region.
`select_for_budget` scores every operator against that value using structural
estimates. It has no interior arrivals or required times, no path
reconvergence, no slew/load state, no early timing, and no electrical
constraints. A candidate that misses the scalar is penalized rather than made
infeasible.

That model cannot answer the architectural question. Two adders of the same
width may need different implementations because one is on a setup-critical
path, one protects a hold path, and a third drives a large fanout. Selection is
a property of a complete vector in context, not a property of one operator and
one number.

### Private regions are not necessarily combinational

State regions contain registers, latches, and their coupled logic. Memory
regions contain access paths and target-dependent setup, hold, and access arcs.
Boundary contracts alone therefore do not seed every interior path. A solver
that models only boundary-input to boundary-output combinational paths cannot
replace the current selector on real designs.

### Early and late timing have opposite failure directions

Every lane is kept separate through propagation:

- setup, recovery, and max-delay checks constrain the maximum, late arrival;
- hold, removal, and min-delay checks constrain the minimum, early arrival.

A delay estimate biased upward may protect setup while hiding a hold violation.
A delay estimate biased downward has the opposite problem. This RFC never
collapses early and late behavior into one supposedly conservative scalar.

### Semantic identity is not implementation identity

`OperatorSignatureId` says what an operator computes. It intentionally omits
facts that do not change semantics, such as the exact value of a constant
operand or known-bit specialization. Those facts can radically change the
lowered structure. Characterization must consequently use a provider-owned
implementation shape identity derived from every fact the provider consumes.

## Decisions

### Semantic operators do not freeze recipes

The pre-mapping operator arena contains the complete semantic signature,
occurrence, operand/result bindings, source provenance, and enough
provider-independent facts to enumerate implementations. It contains neither a
selected candidate nor a selected recipe.

The split is strict:

- the semantic arena records operator instances before selection;
- target publication records the selected candidate, recipe, implementation,
  and mapped cells;
- an artifact whose semantic or provider ABI is incompatible is rejected. No
  legacy migration or structural-estimate fallback exists.

`OperatorManifestInstance` carries no recipe or mapped-cell binding. Selected
target data is represented separately by `PrivateArchitecturePublication` and
`ImplementationRegion`.

### Word pins are the initial characterization granularity

An operator timing arc connects an operand word pin to a result word pin. For
each input edge, output edge, and timing view, characterization reduces only
sensitizable bit arcs:

- the late estimate is the maximum observed bit-arc propagation;
- the early estimate is the minimum observed bit-arc propagation;
- logically independent bit pairs do not participate;
- timing sense determines which input/output edge pairs exist.

This is deliberately pessimistic for many carry structures. Bit-pin
characterization may be introduced later by changing `OperatorPinId`; the
problem and solver contracts do not otherwise change.

### Characterization is an estimate, not a signoff bound

Isolated characterization cannot be called a formal bound on contextual mapped
timing unless every legal mapping and operating point is covered by a proof.
This RFC makes no such claim. Candidate tables are directed, calibrated
estimates used by the regional model. Correlation against mapped timing is
measured separately, and mapped timing is the sole acceptance oracle for the
implemented design.

Queries outside a characterized grid are errors. They never extrapolate and
never fall back to structural estimates.

## Timing model

### Graph and lanes

```rust
pub(crate) struct RegionTimingGraph {
    points: Box<[TimingPoint]>,
    arcs: Box<[TimingArc]>,
    lanes: Box<[TimingLane]>,
}

pub(crate) enum TimingPointKind {
    BoundaryInput(BoundaryPortId),
    BoundaryOutput(BoundaryPortId),
    SequentialLaunch,
    SequentialCapture,
    LatchLaunch,
    LatchCapture,
    LatchRaceCheck,
    MemoryAccess,
    OperatorPin {
        operator: OperatorId,
        pin: OperatorPinId,
    },
    LogicPin,
}
```

A `TimingLane` is `(scenario, timing tag, check kind, timing view,
transition)`. Lanes are propagated independently. Splitting every sequential
element into launch and capture points keeps the data graph acyclic.

The initial latch model does not borrow time:

- `LatchLaunch` launches at the closing edge;
- `LatchCapture` requires data at the closing edge;
- transparent D-to-Q propagation is absent;
- `LatchRaceCheck` uses the opening edge for the minimum-delay race check.

This is an intentionally restrictive optimization model, not a signoff claim.
Multi-phase latch designs are rejected explicitly until a borrowing-aware
timing model is specified.

### Constraint clip and implementation timing

The constraint clip contains only design-side context:

```rust
pub struct RegionTimingConstraintClip {
    scenarios: Box<[ScenarioId]>,
    clocks: Box<[ClockContext]>,
    exceptions: Box<[ExceptionClass]>,
    internal_endpoints: Box<[EndpointIdentity]>,
    checks: Box<[CheckKind]>,
    timing_views: Box<[TimingViewFingerprint]>,
}
```

Clock-to-Q, access, setup, and hold tables for selected sequential cells and
memory macros are implementation-side facts. They are derived while evaluating
a complete decision vector:

```rust
pub struct RegionalImplementationTiming {
    sequential_arcs: Box<[SequentialArc]>,
    sequential_checks: Box<[SequentialCheck]>,
    memory_arcs: Box<[MemoryArc]>,
    memory_checks: Box<[MemoryCheck]>,
}
```

This value cannot enter the input context key: the context is built before the
decision vector, and implementation timing is derived from that vector.

### Candidate identity and single-view cache entries

```rust
pub(crate) struct ImplementationShapeId([u8; 32]);

pub(crate) struct CandidateCharacterizationKey {
    shape: ImplementationShapeId,
    characterization_abi: u32,
    timing_view: TimingViewFingerprint,
}

pub(crate) struct CandidateTimingModel {
    arcs: Box<[CandidateArcEstimate]>,
    area: FiniteValue,
}

pub(crate) struct CandidateArcEstimate {
    key: OperatorArcKey,
    timing_sense: TimingSense,
    estimate: DirectedArcEstimate,
    receiver_capacitance: EdgePairTables,
}

pub(crate) struct DirectedArcEstimate {
    early_delay: RiseFall<SlewLoadTable>,
    late_delay: RiseFall<SlewLoadTable>,
    output_transition: RiseFall<SlewLoadTable>,
}
```

Each cache entry contains exactly one timing view because the view is already
part of its key. `ImplementationShapeId` is emitted by the provider from:

- provider stable identity and provider ABI;
- recipe identity and `OperatorSignatureId`;
- exact constant operands and known-bit facts consumed by lowering;
- observable result-bit shape, dynamic bounds, alignment, and term shape;
- every provider-specific fact that can change lowering structure.

Provider-local recipe numbers never form a global identity.

Entries are immutable target-derived data owned by `TargetMappingContext`.
Each context has a byte budget and reports current and peak resident bytes.
Cache residency and eviction order are runtime state and need not be
deterministic under concurrent access; cache hits and misses must not change any
selected vector or artifact bytes. Concurrent duplicate builds may be discarded
after publishing one byte-identical entry.

### Canonical arc universe

The problem uses candidate-independent keys:

```rust
pub(crate) struct OperatorArcKey {
    input_pin: OperatorPinId,
    output_pin: OperatorPinId,
    input_edge: SignalEdge,
    output_edge: SignalEdge,
    timing_view: TimingViewFingerprint,
}

pub(crate) enum CandidateArc {
    Present(CandidateArcEstimate),
    Absent,
}
```

The universe is the stable sorted union of all candidate arc keys. `Absent`
means that no sensitizable dependency exists for that implementation; it is not
a zero-delay arc. Changing candidates may therefore change timing topology, and
every complete evaluation rebuilds the active graph accordingly.

Fixed surrounding Boolean arcs use the same directed-estimate representation.
The old `StructuralEstimateIndex` has no role in feasibility.

### Complete vector evaluation

`RegionalDecisionVector` is extended to contain operator, sequential, and
memory candidates. Evaluating one vector performs, for every lane:

1. instantiate candidate-specific arcs, checks, capacitances, and topology;
2. solve the vector's receiver loads and input/output transitions with a fixed,
   versioned sweep schedule;
3. propagate late arrivals with `max` and early arrivals with `min`;
4. evaluate setup, hold, recovery, removal, max-delay, min-delay, maximum
   transition, capacitance, and fanout constraints;
5. return the complete evaluated state and its implementation-timing identity.

The sweep schedule has a fixed iteration bound independent of design size. A
non-converged electrical state is an explicit infeasible result, not permission
to use stale or default values.

Solvers may freeze the operating point from an evaluated vector to create a
separable proposal subproblem. A proposal is never considered feasible until
the full coupled evaluator has checked it.

## Regional architecture problem

The problem contains:

```text
operators                 one finite candidate set per occurrence
sequential and memories   one finite candidate set per selectable resource
fixed topology            surrounding Boolean logic and fixed endpoints
timing constraints        sparse and lane-specific
electrical constraints    transition, capacitance, and fanout limits
objective                 stable lexicographic order over evaluated vectors
```

Evaluated vectors are ordered by:

1. electrical feasibility; violating vectors by maximum relative electrical
   violation, then total relative electrical violation;
2. timing feasibility; violating vectors by maximum positive residual in the
   target's canonical time unit, then total positive residual;
3. total area;
4. `(provider stable identity, recipe stable name, implementation shape)`
   vector, with occurrences ordered by `OperationAnchorId`.

Timing violations are not divided by a clock period. Hold, recovery, removal,
explicit min/max delay, and clockless paths do not share such a denominator.
Electrical and timing dimensions are never added together.

Every solver obeys these rules:

- return only after one final complete evaluation;
- retain the best evaluated feasible vector, not the last iterate;
- if none was observed, return the best evaluated vector with explicit
  violations and no global-minimality claim;
- use stable ordering for every tie;
- use fixed policy bounds and a versioned solver ABI.

### Region ownership invariants

- Candidate discovery, characterization requests, and vector selection start
  from the frozen private module. No design-wide `ArchitectureDecisions` pass
  remains on the production path.
- Multi-operand arithmetic, including CSA/Wallace/Dadda candidates, uses this
  same regional problem and provider interface. It is neither deleted as
  unreachable nor retained in a global compatibility layer.
- The solver cannot rebuild partition ownership, inspect a sibling private
  module, or change `RegionAnchorId`/`BoundaryPortId`.
- Cross-boundary knowledge enters only through sealed contracts, constraint
  clips, and predecessor summaries. Selected implementation details leave the
  worker only in its published regional plan.
- Selection introduces no whole-module compact/rewrite barrier. Characterized
  target data is immutable and shared read-only; every design mutation remains
  region-private until deterministic publication.

## Candidate solver designs

Both designs use the same graph, candidate tables, coupled evaluator, ordering,
and final validation. Phase 2 measures both. Phase 3 selects one and deletes the
other; production contains no fallback solver.

Their separable proposal subproblems keep the current candidate arc-presence
set fixed. Candidates that change `Present`/`Absent` topology use bounded
whole-vector proposal slots, sorted by the common vector ordering and capped by
a solver-ABI policy constant. They receive neither slack-proof credit nor a
duality-bound claim; only their complete coupled evaluation can retain them.

### Design A: feasible-seed slack distribution

For one frozen operating point and one lane, write `d(a)` for the relevant
directed delay estimate. For every constrained path `p`:

```text
late margin m(p)  = required_late(p) - sum(a in p) d_late(a)
early margin m(p) = sum(a in p) d_early(a) - required_early(p)
```

For each arc, choose a non-negative allocation weight `w(a)`. The initial
policy uses the magnitude of the current directed delay, floored at one target
time quantum. Define:

```text
margin(a) = min m(p) over constrained paths p containing a
cover(a)  = max sum(b in p) w(b) over constrained paths p containing a
share(a)  = max(0, margin(a)) * w(a) / cover(a)
```

`cover(a)` is a longest weighted path through the arc for **both** early and
late lanes. Early arrival itself still uses a shortest path. Using a shortest
path as the early allocation denominator reverses the required inequality and
can overspend hold slack.

For every path `p` containing `a`, `margin(a) <= margin(p)` and
`cover(a) >= sum(b in p) w(b)`. Therefore:

```text
sum(a in p) share(a) <= margin(p)
```

within the frozen, additive, same-topology subproblem. Late shares permit delay
increases; early shares permit delay decreases. The admissible interval for an
arc is consequently two-sided:

```text
d_early(candidate) >= d_early(current) - share_early(a)
d_late(candidate)  <= d_late(current)  + share_late(a)
```

Shares are computed per lane. The final early or late share for an arc is the
minimum across the applicable lanes, so satisfying the interval cannot spend
another lane's margin. Arcs on no constrained path receive no timing interval
restriction. The positive weight floor makes every constrained `cover(a)`
non-zero.

The proof does not cover a candidate that changes arc presence or changes the
slew/load operating point. Such candidates may still be proposed, but only a
complete coupled evaluation can accept them.

Design A uses a fixed deterministic seed set: minimum area, minimum weighted
late delay, and maximum weighted early delay. Seed scores use the reference
slew/load grid point sealed into the characterization ABI. It evaluates all
seeds and starts distribution from the best feasible seed. Each of two policy
rounds:

1. freezes that vector's operating points;
2. computes early and late shares without enumerating paths, using DAG
   forward/backward extrema;
3. chooses a stable minimum-area proposal satisfying every same-topology arc
  interval;
4. fully evaluates the proposal and retains the best feasible vector.

If no seed or proposal is feasible, the result is explicitly infeasible. The
fixed seed and round bounds are search policy, not correctness arguments.

### Design B: fixed-operating-point Lagrangian proposals

At a frozen operating point, the timing subproblem has late and early path
constraints:

```text
sum(a in p) d_late(a, c)  <= B_late(p)
sum(a in p) d_early(a, c) >= B_early(p)
```

For path multipliers `mu_p, nu_p >= 0`, its complete Lagrangian is:

```text
L(c, mu, nu) = area(c)
  + sum_p mu_p * (sum(a in p) d_late(a, c)  - B_late(p))
  - sum_p nu_p * (sum(a in p) d_early(a, c) - B_early(p))
```

Collecting multipliers by arc gives `M_a = sum(mu_p)` and
`N_a = sum(nu_p)` over paths containing `a`. With operating points fixed, the
candidate minimization separates per operator:

```text
argmin c_i of area(c_i)
  + sum(a in i) (M_a * d_late(a, c_i) - N_a * d_early(a, c_i))
```

The negative early term is required: min-delay pressure rewards a larger early
delay.

Paths are not enumerated. Each iteration fully evaluates its current vector and
builds a valid convex combination of active extremal paths:

- a late predecessor arc is active only when
  `arrival_late(u) + d_late(u,v) == arrival_late(v)`;
- an early predecessor arc is active only when
  `arrival_early(u) + d_early(u,v) == arrival_early(v)`;
- endpoint multipliers are injected at violated endpoints and propagated
  backward only through active arcs;
- recurrence operands are quantized to the target time quantum before `max` or
  `min`, so equality is exact;
- a tie selects the first active predecessor in stable arc order, constructing
  one valid extremal path per endpoint.

This construction conserves non-negative flow at every interior point. Sending
flow through merely near-critical arcs would no longer represent a subgradient
of the max/min recurrence and is forbidden.

Endpoint multipliers use the actual residuals:

```text
mu_t = max(0, mu_t + step * (arrival_late(t) - required_late(t)))
nu_t = max(0, nu_t + step * (required_early(t) - arrival_early(t)))
```

For the frozen same-topology subproblem, the implementation computes
`g(mu,nu) = min_c L(c,mu,nu)`, including the `B_late` and `B_early` constants.
The best observed `g` is a lower bound for that restricted problem, so the
reported duality gap is `best_feasible_area - best_dual_bound` for that problem
only. It is not an optimality bound for a topology-changing or nonlinear
slew/load-coupled problem. Electrical constraints are relaxed in the dual and
remain outer feasibility checks, which preserves the lower-bound direction.

The outer algorithm is a fixed-count successive approximation:

1. fully evaluate the current complete vector;
2. freeze its slew/load operating points;
3. perform one separable Lagrangian update and proposal;
4. fully re-evaluate the proposal under its own coupled operating point;
5. retain the best evaluated feasible vector.

The initial vector is the best evaluated member of Design A's deterministic
seed set. The multiplier schedule is `step(k) = STEP_0 / (k + 1)` after time
and area are converted to the target's canonical units. Phase 2 fixes `STEP_0`
and the iteration count before qualification, and both values enter the solver
ABI.

Discrete candidate sets and changing operating points remove any global
convexity claim. The iteration bound limits work; final evaluation establishes
feasibility.

### Selection of the production solver

Phase 2 records, for both designs, feasibility rate, area, mapped timing,
runtime, peak RSS, and characterization sensitivity. Design B additionally
records only the frozen same-topology subproblem duality gap defined above.
Phase 3 chooses by the complete benchmark table, records the decision in this
RFC, and removes the other implementation.

No runtime flag, environment variable, Tcl switch, or automatic fallback
selects between solvers.

## Identity and incremental reuse

Input context and output decision identity are distinct newtypes:

```rust
pub struct RegionContextKey([u8; 32]);
pub struct RegionalDecisionKey([u8; 32]);
pub struct RegionalPlanKey([u8; 32]);
```

`RegionContextKey` seals only information available before search:

- region revision and boundary-contract generations;
- constraint-clip generation;
- target and scenario fingerprints;
- synthesis effort, search ABI, characterization ABI, and solver ABI;
- predecessor summaries.

`RegionalDecisionKey` seals the selected operator, sequential, and memory
candidate vector plus the resulting implementation-timing identity.
`RegionalPlanKey` seals `(RegionContextKey, RegionalDecisionKey)` and names the
published plan payload.

The regional result cache is queried by context and stores the deterministic
result together with both output keys. A restored entry recomputes and checks
the decision and plan digests before use. This preserves unrelated-region reuse
without putting a value derived from the answer into the lookup key for that
answer.

Cutover bumps `SEARCH_ABI` once. Pre-RFC 0009 plans are incompatible and are
rejected; no compatibility path is retained.

## Determinism and scalability

- Lanes are sorted by `(scenario, tag, check kind, timing view, transition)`.
- Occurrences are sorted by `OperationAnchorId`; arc keys have a canonical
  total order.
- Every sweep, seed count, proposal round, and solver iteration is a policy
  constant independent of design size.
- Analysis is read-only and parallelizable. Publication is deterministic.
- Cache residency may vary with concurrency, but evaluated vectors and emitted
  artifacts may not.

Measurements distinguish cold characterization, warm-cache selection, coupled
evaluation, mapped validation, and peak resident bytes. Candidate cardinality
is reported by provider, implementation shape, timing view, and grid size; it
is never used as evidence of scalability by itself.

## Validation and QoR gates

There are three separate claims:

1. **Propagation correctness:** the regional evaluator agrees exactly with a
   reference evaluator on the same arc tables and topology.
2. **Model correlation:** directed candidate estimates are compared with direct
   isolated mapping and with contextual mapped timing; P50, P95, maximum signed
   error, and sign-error counts are reported per lane and check kind.
3. **Mapped QoR:** post-expansion mapped timing and electrical analysis decide
   whether the selected design is acceptable.

Model feasibility is not mapped feasibility. A characterization estimate is
never described as a formal mapped bound.

For every benchmark, the RFC records setup WNS/TNS, hold worst slack/TNS,
recovery/removal slack, max-delay/min-delay violations, transition,
capacitance, fanout, cell composition, total area, runtime, and peak RSS. Gates
are evaluated at the target report precision:

- no timing metric may degrade by more than one target time quantum;
- no electrical violation count or magnitude may increase;
- total area may not increase (`epsilon_area = 0`);
- runtime and RSS are recorded and reviewed, not silently omitted.

The baseline, target files, tool revision, commands, and report precision are
checked in before results are collected. Private PDK and commercial-tool data
remain outside the public repository.
## Rollout

### Phase 0: timing graph and complete evaluator

Implement `RegionTimingGraph`, `RegionTimingConstraintClip`, complete decision
vectors, implementation timing, coupled slew/load evaluation, every supported
check kind, and the new context/decision/plan identities. Selection still uses
the old scalar path, so mapped output is unchanged.

Accept when the propagation reference suite passes for reconvergence, multiple
outputs, register/latch/memory endpoints, early/late edges, exceptions, and all
supported views; non-converged electrical states and multi-phase latches fail
explicitly; identity and worker-count tests are deterministic.

### Phase 1: characterization and solver comparison

Implement provider-owned shape identity, single-view characterization, bounded
cache accounting, the canonical arc universe, and both solver designs behind a
benchmark-only comparison harness. Production selection remains unchanged.

Accept when cached and uncached models are byte-identical; provider-local
recipe IDs cannot collide; out-of-grid and missing-arc behavior is explicit;
model correlation and cache cardinality/cost are recorded; both solvers return
only fully evaluated vectors; Design B's reported bound is verified on small
enumerable frozen problems.

### Phase 2: choose and cut over

Select one solver from phase 1 evidence, record the decision and policy
constants here, delete the other solver and comparison harness, bump
`SEARCH_ABI`, and delete scalar selection and structural timing scoring.

Accept when same-width operators on materially different paths in one region
can select different recipes, while all infeasible regions report their exact
violations and no fallback path remains.

### Phase 3: qualification

Run the complete QoR table on ibex, CVA6, and synthetic chains,
reconvergence, hold-sensitive logic, and multi-operand CSA/Wallace/Dadda cases.
Record cold/warm runtime, peak RSS, and unrelated-region incremental reuse.

Accept only when the mapped QoR gates above pass, repeated runs and worker
counts produce identical artifacts, and every accepted plan restores with the
same context, decision, and plan identities.

Each phase adds a reproducible benchmark record under `benchmarks/rfc0009`.

## Alternatives

**Keep the region scalar.** Rejected because it cannot distinguish paths or
represent early/electrical constraints.

**Treat every private region as combinational.** Rejected because state and
memory regions contain internal endpoints.

**Use one conservative delay.** Rejected because early and late checks require
opposite directions.

**Use `OperatorSignatureId` as implementation identity.** Rejected because
semantically equal operators can lower to different structures.

**Put implementation timing in `RegionContextKey`.** Rejected because it is
derived from the decision vector selected using that context.

**Divide every timing violation by a clock period.** Rejected because several
check kinds have no meaningful period.

**Use the shortest early path as Design A's allocation denominator.** Rejected
because it cannot prove non-overspend; the denominator must dominate every path
through the arc.

**Make Design B separable under live slew/load coupling.** Rejected because a
candidate changes predecessor slew and successor load. Only a frozen operating
point is separable.

**Propagate multipliers through near-critical arcs.** Rejected because the flow
would not be a subgradient of the max/min timing recurrence.

**Claim isolated characterization bounds contextual mapping.** Rejected absent
a construction that proves the claim over all legal mappings and operating
points.

**Iterate until a fixpoint.** Rejected because discrete candidate selection need
not converge. Work is bounded and correctness comes from final evaluation.

**Keep both solvers in production.** Rejected because it creates a permanent
fallback and doubles the architecture surface.

**Model bit pins immediately.** Deferred because word pins bound initial model
size without freezing the interface.

**Model latch time borrowing immediately.** Deferred; unsupported multi-phase
latch designs fail explicitly rather than using an optimistic approximation.

## References

- [Genus Synthesis Solution Datasheet](https://www.cadence.com/en_US/home/resources/datasheets/genus-synthesis-solution-ds.html)
  — datapath microarchitecture alternatives, architecture-level PPA tradeoffs,
  ChipWare components, concurrent MMMC, and timing/physical context clips.
- [Genus Synthesis Solution Product Brief](https://www.cadence.com/en_US/home/resources/product-briefs/genus-synthesis-solution-pb.html)
  — global analytical microarchitecture optimization and the local-optimum
  limitations of traditional area-first refinement.

These sources establish that datapath architecture selection is an industrial
optimization concern. They do not establish an internal representation or
prove any Opto QoR result.
