<!-- SPDX-FileCopyrightText: 2026 Zhengyi Zhang -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# RFC 0005: Path exceptions and constraint arbitration

- Status: accepted
- Implementation: complete; `set_false_path`, `set_max_delay`, `set_min_delay`,
  and `set_multicycle_path` share the unified exception arena, ordered-through
  tag propagation, endpoint arbitration, and reporting. Every type rank,
  specificity bit, corner term, and tightness rule is pinned by an isolated
  arbitration test; the setup and hold endpoint equations are pinned by an
  analysis test
- Author: Opto project
- Date: 2026-07-25

## Summary

Path exceptions become one arena of `PathException` records covering false
paths, multicycle paths, and maximum/minimum delay. Tagged arrival propagation
generalizes from a startpoint-matched max-delay signature to a candidate set
carried with per-exception `-through` progress. Exactly one exception wins at
each endpoint, selected by an integer priority rather than by taking the
tightest value. Arbitration is a separate, table-driven, independently tested
component whose ranks are checked against the reference environment rather than
restated from the SDC text, and whose table is pinned in the public tree by
isolated unit tests.

The implementation also lands the four public Tcl commands against this
mechanism. Their documented point classes, edge-qualified forms,
setup/hold selection, `-start`/`-end`, `-reset_path`, comments, and
clock-latency option are therefore part of this RFC's completed surface.

## Motivation and compatibility evidence

Constraint completion gates every claim about real designs. Timing-driven
optimization currently observes a design whose critical paths include structural
false paths, so its objective is measurably wrong on any design whose intent is
expressed with exceptions. Adding those commands without a decided arbitration
model is the failure mode this RFC exists to prevent: exception precedence is
invisible at runtime, produces plausible slack rather than a diagnosable error,
and cannot be retrofitted once several commands assume different rules.

The existing mechanism is the right shape and the wrong generality.
`analysis/state.rs` interns `TagKey { launch_domain, max_delay_exceptions }`
into a `TagArena`, so arrival times already split by launch clock, launch edge,
and applicable exception set. `analysis/support.rs` computes the exception
signature once at the startpoint by matching `from`. `analysis/required.rs`
consumes it at output ports, filtering by `to` and combining survivors with
`min`. Three properties of that code are specific to max delay and must not be
generalized by accident:

- **Startpoint-time matching.** A signature computed from `from` alone cannot
  express `-through`, which is a predicate over the path rather than over its
  ends.
- **Value combination.** Taking the minimum of several matching delays is
  sound only because all candidates are max-delay constraints. Under a general
  model the most specific exception wins even when a laxer one is tighter.
- **Endpoint coverage.** `seed_check_required` derives required time purely
  from launch and capture clock edges and never reads the tag's exception set,
  so exceptions terminating at a register check are not honored today.

Before this RFC, `TimingEndpoint` admitted `Port` and `Clock`. Real exception objects also include
pins, cells, and nets, and `-through` accepts pins and nets that are neither.
Widening that enumeration is a prerequisite for this design and is in scope.

Precedence rules were not safe to derive from the SDC text. Secondary sources
disagree on the order of multicycle paths against maximum-delay constraints, and
an early draft of this RFC recorded that order backwards. Every rank and mask
bit below was checked against the reference environment named by AGENTS.md,
which stays outside this repository along with its results.

That environment is not reproducible from the public tree, so it cannot be the
artifact that defends the table. The isolated arbitration tests are. Each rule
below is pinned by a test that fails if the rank, the mask bit, the tightness
rule, or the multicycle equation changes, so a public reader can see what the
implementation claims and a regression names the rule it broke. A rule with no
such test is not implemented.

## Detailed design and invariants

### Exception records

One `OrderedArena<PathException>` replaces the max-delay arena and follows the
established constraint-storage pattern: a context field, a checkpoint vector, an
undo journal entry, and a row-edit list in `PreparedTimingObjectRemoval`.

```text
PathException {
    kind:    FalsePath
           | MultiCycle { cycles, use_end_clock }
           | MaxDelay { delay }
           | MinDelay { delay },
    from:    ExceptionFilter,
    through: Box<[ExceptionFilter]>,
    to:      ExceptionFilter,
    edges:   EdgeQualifier,
    corner:  Both | Setup | Hold,
    ignore_clock_latency: bool,
    comment: String,
}
```

`ExceptionFilter` is a canonical ordered set of `TimingEndpoint` values plus an empty
"unrestricted" case; empty `from`, `through`, and `to` reproduce today's
"unrestricted" semantics. `corner` distinguishes `-setup` from `-hold`, while
`use_end_clock` implements `-start` versus `-end`. `EdgeQualifier` carries the
rise/fall qualification of every path point.

An unrestricted false path or multicycle path is rejected at the command
boundary. A global maximum/minimum delay remains valid behavior. Every
explicit `through` filter must be nonempty, and its edge qualifier occupies the
same index.

### Tag content

`TagKey` becomes:

```text
TagKey {
    launch_domain: LaunchDomain,
    candidates:    SmallVec<[ExceptionCandidate; 1]>,
}

ExceptionCandidate { slot: ExceptionSlot, through_progress: u16 }
```

`candidates` holds every exception whose `from` matched the startpoint,
ordered by slot so the key is canonical. `through_progress` counts the
`through` filters already satisfied on this path; an exception with no
`through` list has progress zero and is immediately eligible.

Invariants:

- A candidate enters the tag only at a startpoint. `-through` never revives an
  exception whose `from` did not match.
- Traversing an arc whose head object satisfies candidate `c`'s next unmatched
  `through` filter yields a tag identical to the source tag except that `c`'s
  progress advances by one. Progress never decreases.
- A candidate is *eligible* at an endpoint when its progress equals its
  `through` length and the endpoint satisfies its `to` filter and its edge
  qualification.
- Tags are interned. Two paths with the same launch domain and the same
  candidate progress vector share one tag regardless of how they reached it.

### Arbitration

Arbitration is one function over the eligible candidate set, and it lives in its
own module with no dependency on propagation:

```text
resolve(eligible: &[ExceptionSlot], timing: &TimingContext, corner: MinMax)
    -> ExceptionSlot
```

Each exception carries one integer priority. The winner is the maximum. The
integer is the sum of a type rank, a specificity mask, and a corner term, which
makes the comparison a single flat integer rather than a lexicographic tuple.

Type ranks. The integers are Opto's encoding; what is verified is their
order:

| Exception | Rank |
| --- | ---: |
| False path | 4000 |
| Path delay (`set_max_delay` / `set_min_delay`) | 3000 |
| Multicycle path | 2000 |
| Filter path | 1000 |
| Group path | 0 |

Path delay outranks multicycle. This is the rule an early draft of this RFC
recorded backwards, and it is the reason the ranks are cited rather than
restated from memory.

Specificity, from `ExceptionPath::fromThruToPriority` in
`sdc/ExceptionPath.cc`, is a bit mask rather than an ordering over cases:

| Condition | Bit |
| --- | ---: |
| `from` names pins or instances | 6 |
| `to` names pins or instances | 5 |
| `through` list is non-empty | 4 |
| `from` names clocks | 3 |
| `to` names clocks | 2 |
| corner term | 1..0 |

A mask is not a tuple comparison: `-from` on a pin outweighs any combination of
the lower bits, so a pin-qualified `-from` alone beats a clock-qualified
`-from -through -to`. Opto reproduces the mask rather than a ranking that
merely agrees with it on the common cases.

The corner term occupies the low two bits and is supplied by the exception kind.
Multicycle uses it to prefer the constraint written for the corner under
analysis, per `MultiCyclePath::priority`: `+2` when the exception names this
corner explicitly, `+1` when it names both, `+0` when it names the other. A
multicycle written `-setup` therefore outranks one written for both corners
during maximum-delay analysis and is outranked by it during minimum-delay
analysis.

Equal priorities do not change semantics by definition order. Tightness
decides: a maximum delay chooses the smaller value, a
minimum delay chooses the larger value, and a multicycle chooses the smaller
multiplier. Equal-effect rows use stable slot order only to identify the
reported winner; their timing result is identical.

### Endpoint semantics

`resolve` runs at both endpoint classes, which unifies the asymmetry noted
above.

- **False path.** No required time is seeded for that tag. The arrival
  contributes to no slack and to no reported path.
- **Multicycle.** The check equation applies the reference cycle adjustment.
  `seed_check_required` currently computes setup as
  `capture.next_edge_after(check.clock_edge, launch_edge_time) - constraint`
  and hold as `capture.edge_at_or_after(...) + constraint`. Setup adds
  `(cycles - 1) * period`. Hold uses
  `(setup_cycles - 1) * setup_period - hold_cycles * hold_period`, with either
  term omitted when that exception is absent.

  Two sub-semantics are recorded rather than assumed. `-start` versus `-end`
  decides whether the multiplier counts edges of the launch clock or the
  capture clock. The hold
  relationship implied by a setup multicycle is the second, and it is the most
  common source of silently wrong hold analysis. Both are evidence items with
  their own hand-derived cases.
- **Max/min delay.** Required time comes from the exception value, as today,
  but from the winner rather than from the minimum of all matches.
- **No eligible candidate.** The clock relationship applies unchanged. This is
  the current behavior for untagged arrivals and stays the default.

### Reporting

`TimingAnalysis` carries the winning exception, its kind, and its specificity rank.
`report_timing` prints them. An exception model whose arbitration is invisible
cannot be debugged by the person writing the constraints, so this is part of the
feature rather than a follow-on.

## Determinism, scalability, and QoR impact

Determinism follows from interning plus a total order. Candidate vectors are
slot-ordered, progress families are pre-interned before read-only parallel
propagation, and arbitration applies priority plus the evidenced tightness
rule. Analysis output does not depend on traversal order or thread count.

The scalability risk is tag population. `k` exceptions whose `from` matches one
startpoint admit up to `2^k` progress vectors along a reconvergent cone in the
worst case. Three bounds apply, in order:

1. Only exceptions with a non-empty `through` list can multiply; the rest have
   a fixed progress of zero and contribute one candidate each.
2. Progress is monotonic, so the reachable set along any path is a chain rather
   than a power set; the blow-up requires reconvergence over distinct
   `through` points.
3. The tag arena is bounded. Exceeding the bound is a diagnostic error naming
   the exceptions involved, not a silent switch to a conservative analysis.
   A stale or unrepresentable analysis is discarded, never replaced by a hidden
   second result.

The implementation pre-interns at most `1 << 20` progress variants. This is a
deterministic resource guard rather than a semantic fallback: a larger family
is rejected before propagation with an explicit analysis-capacity diagnostic.

Incremental analysis keeps the conservative rule. `analysis.rs` compares the
previous and current path-exception arenas and dirties every net when they
differ. Exception
edits invalidate the tag arena wholesale because slot identity is embedded in
every key. Finer invalidation is possible and deliberately out of scope.

QoR impact is intended and must be measured rather than assumed. Honoring false
paths removes paths from the timing-driven objective, which should improve
achieved slack on real designs and may increase area where optimization
previously spent it on unreachable paths. Every command landing against this
RFC extends the QoR suite with a case whose constraint intent is the thing under
test.

## Alternatives

**Keep startpoint-time signatures and reject `-through`.** Cheapest, and
sufficient for a large fraction of real constraint files. Rejected because
`-through` is not a corner case in the designs Opto targets, and because the
tag layout is the one decision that cannot be revised later without rewriting
propagation. Paying for it once is the point of this RFC.

**Combine matching exceptions by value instead of arbitrating.** This is what
the tree does today, and it is why the current code is correct only for a set of
max-delay constraints. It gives the wrong answer as soon as a false path and a
multicycle path overlap, and the wrongness is silent.

**Resolve exceptions while building the timing graph, by pruning arcs.**
Attractive for false paths and unworkable for the rest: a graph edge is shared
by many paths, and an exception qualified by `from` cannot be applied to an arc
without knowing the path that reached it. It also destroys the arc structure
that incremental analysis and reporting depend on.

**Adopt a precedence table from the SDC text now and correct it later.**
Rejected on the repository's own rule. A wrong precedence rule does not fail; it
produces confident slack. The hard-error-until-evidenced design costs one
mechanical pass per verified rule and removes an entire class of silent
divergence.

## Validation and rollout

Semantic correctness of timing constraints cannot be established by equivalence
checking, which is the tree's usual instrument. Three layers replace it.

- **Hand-derived cases.** Small designs whose expected slack is derived by hand
  from the clock waveform and the exception are checked into the timing and Tcl
  regression suites. These pin precedence, endpoint equations, ordered-through
  propagation, and command storage.
- **Invariant-oriented cases.** The regressions verify that a winning false
  path removes the path, setup and hold multicycle adjustments use their
  respective equations, reversed `through` sequences do not match, and path
  delays choose the documented tighter value after priority arbitration.
- **Arbitration table pinned by isolated tests.** The ranks, the specificity
  mask, the tightness rules, and the multicycle equations each have a unit test
  in `constraints/arbitration.rs` that fails on any change. These tests are the
  public statement of the table. No external analyzer is on the required test
  path, and no reference-environment artifact enters this repository.

  The tests cover exception selection, path membership, and the endpoint
  equation. They do not cover absolute slack, which differs between any two
  delay calculators and is not a property of this design. Checking a rule
  against the reference environment happens outside the tree; what lands here
  is the test that encodes the outcome.

The completed rollout was:

1. Widen `TimingObject` and `TimingEndpoint` to the object classes exceptions
   accept, with no behavior change.
2. Land the `PathException` arena, storage, checkpoint, undo, removal, and
   memory accounting, migrating `set_max_delay` onto it with the QoR baselines
   unchanged.
3. Land tag generalization and arbitration with a table containing only the
   max-delay rows, still with baselines unchanged. This step proves the
   mechanism against known-good numbers before any new semantics exist.
4. Check the exception table and endpoint equations against the reference
   environment, and land one isolated arbitration test per verified rule.
5. Land `set_false_path`, then `set_min_delay`, then `set_multicycle_path`,
   each with its evidence, its hand-derived cases, and its QoR case.

The generalized max-delay mechanism reproduced the existing baselines before
the new commands were enabled. The final tests cover tighter-value arbitration,
false-path precedence, ordered and reversed `through` sequences, max/min output
requirements, setup/hold multicycle accounting, typed Tcl collections,
checkpoint/reset behavior, and report metadata.
