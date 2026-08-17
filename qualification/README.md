<!-- SPDX-FileCopyrightText: 2026 Zhengyi Zhang -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# Verification corpus

Opto uses a test pyramid instead of treating a few hand-written circuits as a
qualification suite. A change is trustworthy only when the relevant layers
all pass.

| Tier | Corpus | Current scale | Gate |
| --- | --- | ---: | --- |
| Unit | Rust tests beside each domain | domain-local cases and invariants | every pull request |
| Static integration | focused RTL, Tcl shell behavior, SDC, hierarchy, memory and sequential cases | 60 cases | every pull request |
| Generated semantics | operator × width × signedness × context | 238 synthesis points on pull requests; the same points are CEC-proved nightly | pull request and nightly |
| Generated differential | deterministic RTL generator | 512 fixed-seed, CEC-proved designs as a regression sentinel | nightly |
| Mapping-fixture determinism | presubmit mapping cases without Yosys | 5 zero-tolerance local-mechanism baselines | every Linux pull request |
| Language conformance | CHIPS Alliance `sv-tests` | 1,027 audited HDL files; 137/137 reviewed positive ASIC cases | nightly |
| Synthesis qualification | Yosys RTL tests | 549 files audited; 172 designs synthesized; 119/119 mandatory observable designs proved | nightly |
| Real designs | pinned Ibex and CVA6 configurations | complete checked manifests | weekly |
| QoR | representative kernels and blocks | area, timing, cell mix, runtime, memory and CEC | weekly |

The 60 static cases are seeds, not a claim of broad coverage. Every fixed bug
must add the smallest stable regression case that would have caught it. Broad
semantic spaces belong in a generator; real integration behavior belongs in a
pinned upstream design.

Every static case declares its unique dimension in the checked `covers` field:

| Case | Unique dimension | Pull-request oracle |
| --- | --- | --- |
| `frontend-packed-generate` | packed-struct arrays across generated parameterized instances | elaboration inventory |
| `frontend-dynamic-continuous-lvalue` | runtime-indexed continuous net lvalue rejected during language legality | expected process failure and diagnostic text |
| `frontend-syntax-error` | malformed SystemVerilog port-list diagnostic | expected process failure and diagnostic text |
| `hierarchy-parameter-generate` | indexed part-selects through generated child hierarchy | mapped synthesis; CEC nightly |
| `hierarchy-reference-ports` | module `ref` ports, including runtime-selected unpacked-array actuals, eliminated as exact aliases of parent variables | mapped synthesis against an explicitly flattened reference; CEC nightly |
| `hierarchy-reference-modport` | `ref` modport members flattened as exact interface-variable aliases | mapped synthesis against an explicitly flattened reference; CEC nightly |
| `memories-dual-port-register-bank` | two clocked ports with explicit same-address write priority | sequential mapped synthesis; CEC nightly |
| `memories-multi-clock-partitioned-bank` | disjoint memory words owned by independent write clocks | sequential mapped synthesis; CEC nightly |
| `memories-multi-clock-macro` | overlapping dual-clock writes bind an exact two-write-port Liberty RAM | mapped synthesis; macro boundary is not flattened for CEC |
| `memories-same-clock-exclusive-macro` | same-clock logical writes with provably disjoint enables bind separate ports of a two-write-port Liberty RAM | mapped synthesis; macro boundary is not flattened for CEC |
| `memories-noncontiguous-unpacked-state` | unpacked dimensions separated by aggregate fields use canonical flattened register state | sequential mapped synthesis against a flattened reference; CEC nightly |
| `memories-async-reset-register-bank` | a bounded asynchronous reset over every unpacked-array element lowers to resettable register-bank state | sequential mapped synthesis against a flattened reference; CEC nightly |
| `sequential-dual-edge-state` | one state variable updates on both edges of the same clock without losing conditional holds | sequential mapped synthesis against an explicit phase-bank reference; CEC nightly |
| `sequential-dual-edge-iff` | opposite edges of one clock carry independent `iff` qualifiers | sequential mapped synthesis against an explicit qualified phase-bank reference; CEC nightly |
| `sequential-level-sensitive-udp` | current-state and hold rows in a level-sensitive user-defined primitive | latch synthesis against an explicit procedural reference; CEC nightly |
| `sequential-edge-sensitive-udp` | binary-reachable transition rows, hold edges, and one level-sensitive asynchronous control in a user-defined primitive | resettable flip-flop synthesis against an explicit edge-triggered reference; CEC nightly |
| `sequential-edge-async-control-udp` | distinct data-clock and edge-coded constant asynchronous-control transition inputs in a sequential UDP | resettable flip-flop synthesis against an explicit edge-triggered reference; CEC nightly |
| `memories-sync-ram` | typed 64-by-32 synchronous-read memory retention | elaboration inventory |
| `processes-priority-case` | `priority casez` wildcard order under an outer enable | mapped synthesis; CEC nightly |
| `processes-grouped-case-items` | Verilog `always @*`, grouped case items, and default | mapped synthesis; CEC nightly |
| `processes-ascending-part-select` | procedural slices on ascending packed declarations | mapped synthesis; CEC nightly |
| `processes-constant-repeat` | bounded constant expansion over successive blocking values | mapped synthesis; CEC nightly |
| `processes-static-foreach` | declared-order expansion of an ascending packed range | mapped synthesis; CEC nightly |
| `processes-static-condition-loops` | typed local induction and termination proof for `while` and `do-while` | mapped synthesis; CEC nightly |
| `processes-bounded-runtime-condition-loops` | statically bounded `for`, `while`, and `do-while` loops with runtime early-exit predicates | mapped synthesis against explicitly nested conditions; CEC nightly |
| `processes-runtime-bound-loops` | signed and unsigned runtime integral bounds that conservatively cap `for`, `while`, and `do-while` expansion | mapped synthesis against direct count and mask arithmetic; CEC nightly |
| `processes-bounded-runtime-repeat` | entry-snapshotted runtime repeat counts whose signed or unsigned type domain fits the deterministic expansion bound | mapped synthesis against direct count arithmetic; CEC nightly |
| `processes-general-for-clauses` | finite `for` loops with omitted clauses, body-owned induction transitions, runtime initializer effects, continue-to-step control, and ordered step side effects | mapped synthesis against an explicit finite expansion; CEC nightly |
| `processes-general-loop-state` | module-scope, multi-variable, and nested outer induction state, independently bounded runtime function arguments, and post-loop runtime overwrite or partial update across all static loop forms | mapped synthesis against explicit finite control flow; CEC nightly |
| `processes-bounded-forever-loop` | `forever` expansion whose local induction state proves a finite `break`, while runtime `break` and `continue` can exit or skip earlier iterations | mapped synthesis against explicitly nested conditions; CEC nightly |
| `processes-forever-case-completion` | `forever` termination proof merges every runtime `case` arm and its explicit default without adding a CFG backedge | mapped synthesis against an explicit finite result table; CEC nightly |
| `processes-activation-loop-exit` | activation-scoped `return` and enclosing lexical `disable` complete unbounded or runtime-entry loops without crossing nested activation boundaries | mapped synthesis against explicit finite control flow; CEC nightly |
| `processes-scoped-disable` | lexical named-block exit, outer-scope exit from an unrolled loop, and current task activation exit with output copy-out | mapped synthesis against explicit structured conditions; CEC nightly |
| `processes-assignment-expressions` | blocking, compound, increment, and decrement expressions with frozen values and lvalue addresses, branch-local ternary effects, and logical short-circuit effects | mapped synthesis against an explicit side-effect-free expansion; CEC nightly |
| `processes-pattern-matching` | structure patterns, match-time variable snapshots, short-circuited condition-list effects, conditional-expression bindings, and filtered pattern-case priority | mapped synthesis against an explicit pattern-free expansion; CEC nightly |
| `processes-tagged-union-patterns` | canonical discriminant and payload storage for packed and unpacked tagged unions across runtime construction and pattern binding | mapped synthesis against an explicit tag-free behavioral expansion; CEC nightly |
| `processes-replicated-assignment-patterns` | replicated packed-struct and unpacked-array patterns with side-effecting elements evaluated in source order | mapped synthesis against an explicit scalar expansion; CEC nightly |
| `processes-ref-arguments` | exact subroutine aliases, same-actual visibility, and call-entry snapshots for dynamic unpacked-array elements | mapped synthesis against an explicit expansion; CEC nightly |
| `processes-nested-dynamic-lvalue` | composed runtime unpacked-element and packed-bit procedural target | mapped synthesis; CEC nightly |
| `processes-automatic-array-local` | fully assigned automatic unpacked-array temporary in an edge-triggered process | sequential mapped synthesis against a flattened reference; CEC nightly |
| `sdc-max-delay` | Tcl-composed collections carrying electrical and path constraints | constrained synthesis and reports |
| `semantics-division` | guarded signed/unsigned quotient and remainder composition | mapped synthesis; CEC nightly |
| `semantics-runtime-bit-functions` | bounded runtime power, population count, and one-hot predicates | mapped synthesis; CEC nightly |
| `semantics-extended-equality` | lossless two-state case equality and constant wildcard masks | mapped synthesis; CEC nightly |
| `semantics-combinational-udp` | wildcard rows in a user-defined combinational primitive table | mapped synthesis against a Boolean reference; CEC nightly |
| `semantics-multilevel-boolean-cover` | shared five-input two-level AND-OR product cover | mapped synthesis; CEC nightly |
| `semantics-resolved-nets` | multiple-driver `wand` and `wor` resolution | mapped synthesis; CEC nightly |
| `semantics-inout-tristate` | target-cell-backed top-level inout drive and internal readback | mapped synthesis; Yosys SAT does not model the external tri-state environment |
| `semantics-tri-state-bus` | complementary active-high/active-low drivers on an internal resolved bus | mapped synthesis; CEC nightly after `tribuf -formal` normalization |
| `semantics-notif-tristate` | complementary inverting tri-state primitives preserve both data inversion and enable polarity | mapped synthesis; CEC nightly |
| `semantics-pull-primitives` | built-in pullup and pulldown primitives become exact constant network drivers | mapped synthesis against explicit constant assignments; CEC nightly |
| `semantics-signed-arithmetic` | mixed-width sign extension, arithmetic shift, compare, and dynamic slice | mapped synthesis; CEC nightly |
| `semantics-verilog-truncating-add` | non-ANSI Verilog declarations and fixed-width addition truncation | mapped synthesis; CEC nightly |
| `sequential-async-set-clear` | preset-over-clear priority composed with enable | sequential mapped synthesis; CEC nightly |
| `sequential-dff-enable` | vector inferred clock-enable register | sequential mapped synthesis; CEC nightly |
| `sequential-event-iff-enable` | runtime clock `iff` qualification composes with canonicalized post-edge-true reset and constant-false discarded events | sequential mapped synthesis against explicit reset and enable control; CEC nightly |
| `sequential-fsm-equivalent-states` | equivalent-state merge from reviewed zero initial state | sequential mapped synthesis; CEC nightly |
| `sequential-fsm-sparse-timing` | sparse FSM under an explicit timing clock | timing-aware synthesis; CEC nightly |
| `tcl-collections` | collection length/filtering plus redirect and reports | script-owned exact assertions |

The policy checker rejects a static case without a specific `covers` entry.
Renames replace vague smoke identifiers rather than preserving aliases. Case
deletions and their stronger surviving owners are recorded in
[`../docs/testing.md`](../docs/testing.md).

Generated differential testing is deliberately capped at 512 fixed seeds. It
is useful for inexpensive mutation coverage inside already modeled semantic
families, but repeating the same grammar distribution is not a substitute for
new language constructs or independently authored RTL. Coverage growth should
prefer curated upstream cases and proof closure over larger random seed counts.

The target region-parallel cutover has an additional public scale
gate at approximately one hundred thousand, one million and ten million mapped
gates. It covers control, arithmetic, fanout, pipelines, memory, multi-clock
and explicit sparse MMMC behavior. The reproducible corpus and regression
contract are specified in [`../docs/architecture.md`](../docs/architecture.md).
This target tier is not part of the current coverage counts above.

## Layout

```text
qualification/
  cases/                 focused first-party regression cases
  libraries/             small synthetic, redistributable test inputs
  suites/                typed suite manifests
  upstream/              adapters, manifests, hashes and license audit data
crates/opto/tests/
  cli/                    public executable and command-line behavior
  qualification/          typed corpus runners, formal adapters and reports
  support/                focused integration-test support
benchmarks/qor/           performance and quality measurements, not correctness smoke tests
```

Case and suite manifests are parsed with `serde(deny_unknown_fields)`. A typo
therefore fails loudly instead of silently dropping coverage. The runner emits
machine-readable `results.json`, a compact `summary.tsv`, per-case logs on
failure, tool hashes and equivalence status.

When an independent oracle cannot parse the syntax under test, an equivalence
case may declare a separately reviewed `equivalence_sources` list containing a
portable semantic expansion. Opto still compiles only the primary `sources`;
the alternate source is used only for the golden side of the proof and both
input sets are hashed into the result record.

Sequential cases that change state representation declare
`equivalence_initial_state = "zero"` only when zero is the reviewed
state-relation boundary on both sides, normally the post-reset state. The
runner then proves a shared-input miter by temporal induction from that state;
it does not treat unrelated arbitrary register contents as equivalent.

## Local gates

The normal workspace test command runs the domain tests, static corpus, and
the non-CEC form of the 238-point generated matrix:

```sh
cargo test --workspace --all-targets --all-features
```

CEC requires an explicit Yosys executable:

```sh
OPTO_YOSYS=/path/to/yosys \
  cargo test -p opto --test qualification presubmit_equivalence \
  -- --exact --ignored --nocapture

OPTO_YOSYS=/path/to/yosys \
  cargo test -p opto --test qualification generated_semantic_equivalence \
  -- --exact --ignored --nocapture
```

Filter a static suite without editing its manifest:

```sh
OPTO_REGRESSION_CASE=sequential-dff-enable \
  cargo test -p opto --test qualification presubmit_corpus -- --exact --nocapture
```

External suites are documented in [`upstream/README.md`](upstream/README.md).
Generated outputs belong outside the repository; set `OPTO_REGRESSION_OUTPUT`
to retain them.

## Coverage policy

A case count alone is not a coverage metric. Reviews must identify which of
these dimensions changed: syntax/preprocessing, type and width semantics,
process lowering, hierarchy/parameters, sequential behavior, memories,
constraints, Tcl collections, mapping, reports, errors, determinism, or
resource limits. New constructs need positive and negative tests; synthesis
changes need CEC; QoR changes need a representative nontrivial benchmark.
Test placement, primary ownership, fixture, ignored-test, and consolidation
rules are defined in [`../docs/testing.md`](../docs/testing.md).

The nightly `sv-tests` baseline hashes the exact required case IDs. The Yosys
baseline independently hashes synthesized designs, observable roots, mandatory
proof targets, and deferred proof targets. Its 53 deferrals are exact
path/design pairs with reviewed reasons in `upstream/yosys/proof.toml`; a new
or renamed deferral fails instead of silently reducing proof coverage. Neither
baseline can be satisfied by losing one old case while gaining an unrelated
new one. One required rejection and 27 non-hardware exclusions are also
enumerated with reasons in `upstream/yosys/audit.toml`; there is no remaining
required-compilation capability gap. Updating a hash or classification requires
reviewing the mismatch report and the pinned upstream license/inventory again.
