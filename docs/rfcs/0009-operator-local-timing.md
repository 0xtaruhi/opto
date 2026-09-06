<!-- SPDX-FileCopyrightText: 2026 Zhengyi Zhang -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# RFC 0009: Operator-local timing and region-local architecture selection

- Status: accepted, bounded structural guidance implemented
- Author: Zhengyi Zhang
- Revised: 2026-09-06
- Implementation: per-result-bit path budgets, bounded private AXM probes,
  critical-path rewriting, and shared-budget mapped search

## Decision

Operators in the same frozen region may require different implementations.
Their widths and the region's external requirement do not identify their
positions on a path. Selection therefore uses a region-private timing sweep
that includes intervening Boolean logic and sequential endpoints.

The former proposal for a complete provider-characterized nonlinear regional
solver is retired. It was not implemented, and retaining its planned APIs as
current requirements obscured the actual contract. Future characterization or
choice-graph work requires its own bounded design and reproducible evidence;
RFC 0011 records the remaining compile-once proposal. The normative current
architecture remains `docs/architecture.md`.

## Implemented selection

Selection starts from the minimum-cost recipe vector. At most four probes
lower that vector into the ordinary region-private AXM graph. Each probe:

1. retains actual bit correspondence while propagating input arrivals and
   endpoint requirements through the complete local subject;
2. derives each operator's contextual budget from the selected recipe's
   estimated depth and the minimum slack of its actual result-bit paths;
3. ranks equivalent provider recipes against those contextual budgets; and
4. retains the best evaluated vector by worst structural violation, total
   structural violation, then live gate count.

A late operand bit and a tight result bit must not be combined unless they
are connected by an actual path. This matters for chained carry propagation,
where different bit positions overlap in time. Unconstrained operators retain
area-oriented selection. Negative budgets still distinguish faster recipes.

Probes roll back temporary Word bindings on success and failure. Only compact
recipe decisions survive. Region ownership, boundaries, durable identities,
and deterministic publication remain unchanged. Liberty cover and mapped MMMC
analysis provide the subsequent electrical acceptance checks; structural
stage counts are guidance, not a claim of timing closure.

## Rewriting and search coverage

Critical Boolean rewriting traces the actual maximum-violation paths and may
spend bounded local area to reduce their deficit. Both primitive weight and
gate growth are limited to twice the removed cone. Feasible rewrites may save
area within the real requirement. Exact care-set equivalence remains required.

Multi-operand sums can select ripple, Brent-Kung, or Kogge-Stone final carry
networks. A row supported only at bit zero enters the final adder as carry-in
instead of requiring a redundant compression layer. Prefix carry-in is folded
into the first generate term when it arrives early enough; a late carry
enters after the prefix scan.

Mapped search shares one deterministic evaluation budget. Cloning, both
sizing frontiers, candidate ranks, and symmetric pin assignments receive
bounded opportunities through the same transactional MMMC evaluator. Feasible
equal-cost candidates can improve data-path arrival without spending area;
clock pulse-width checks do not stand in for data delay. Primary setup, hold,
other timing checks, design rules, and physical objectives still take priority.

## Limits and evidence

The implementation does not provide provider-characterized nonlinear timing
surfaces, a joint sequential/memory architecture solver, or a global choice
graph. It does not promise global optimality or parity with another synthesis
tool. Search ABI changes invalidate prior cached decisions rather than keeping
a second decoder or pipeline.

Regression tests cover different path contexts for equal-width operators,
bit-correlated chained arithmetic, exhausted budgets, bounded critical-cone
growth, later sizing and pin candidates, and transactional rollback. Arithmetic
recipes receive exhaustive small-width equivalence tests; end-to-end semantic
CEC and the unchanged real-design QoR gate qualify the integrated pipeline.
