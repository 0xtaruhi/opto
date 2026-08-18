<!-- SPDX-FileCopyrightText: 2026 Zhengyi Zhang -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# RFC 0012: Versioned synthesizable SystemVerilog profile

- Status: proposed
- Implementation: transient cyclic Proc lowering for every procedural loop form, side-effecting procedural expressions, canonical writable ranges, scalar selected-clock expressions with event-local `iff` identity, reviewed operators, exact subroutine and hierarchical reference aliases, combinational, level-sensitive, and single-update-input edge-sensitive UDP tables, target-backed tri-state/inout mapping, and exact independent-clock or mutually-exclusive same-clock memory-macro binding implemented; profile inventory and remaining capability work pending
- Author: Opto project
- Date: 2026-08-17

## Summary

Opto defines a versioned ASIC synthesizable SystemVerilog profile instead of
claiming an unspecified "synthesizable subset" of the complete language. The
profile classifies each reviewed construct by source semantics and by the
stage that can reject it. It distinguishes language support from whether a
particular Liberty target contains a compatible implementation.

Every positive profile entry must elaborate through the pinned slang
frontend, normalize into the one Proc/Word pipeline, synthesize through the
one production flow, and carry independent semantic evidence. Unsupported
profile entries fail with a structured diagnostic. Simulation-only behavior,
invalid source, intentionally excluded ASIC policy, and missing target-library
capabilities are not reported as frontend capability gaps.

## Motivation and compatibility evidence

SystemVerilog does not specify one normative synthesis subset. Implementations
disagree on constructs such as initialization, runtime four-state tests,
multi-event sequential behavior, multi-clock memories, internal tri-state
resolution, UDPs, and tool-specific language extensions. A claim that Opto
supports "all synthesizable SystemVerilog" would therefore have no stable
denominator and could not be reproduced.

The pinned `sv-tests` and Yosys corpora remain evidence rather than the
language specification. Their file counts also do not form a support
percentage: many files are parser tests, self-checking testbenches, invalid
inputs, tool extensions, or components driven only by a multi-file script.
Qualification must identify independently observable design units and the
profile features they exercise before a case contributes to a support claim.

The first rollout targets the reviewed gaps in static procedural loops,
canonical writable selections, memories, event controls, resolved nets,
operators and system functions, interfaces, reference arguments, primitives,
and UDPs. `casex` remains intentionally rejected because its wildcard-X
behavior is unsafe for deterministic ASIC intent. Initial blocks, static-
lifetime declaration initialization, and memory-file initialization remain
outside the profile while Opto's product contract requires explicit reset
hardware. An initializer on an automatic procedural variable is instead an
ordinary per-activation assignment and remains supported.

## Detailed design and invariants

### Classification

Every reviewed feature or upstream design unit has exactly one classification:

- `supported_and_proved`: accepted by the public flow and protected by an
  independent semantic oracle;
- `supported_target_dependent`: source semantics and technology-independent IR
  are supported, but publication requires an exactly compatible target cell or
  macro;
- `intentional_reject`: valid language behavior deliberately excluded from the
  Opto ASIC profile;
- `invalid_or_non_hardware`: invalid source, a testbench, a parser-only input,
  or another unit without independently observable hardware;
- `known_gap`: behavior included in the profile but not yet implemented or
  proved.

An upstream baseline hashes the exact design-unit and feature inventory for
each classification. A count may not be updated to exchange one missing
requirement for an unrelated passing case. `known_gap = 0` is meaningful only
for the complete reviewed profile inventory, not for a selected directory or
file subset.

### Acceptance stages

Diagnostics identify the earliest authoritative rejection stage:

1. the native slang adapter owns parsing, name lookup, constant evaluation,
   language legality, and the source-semantic projection of statements,
   patterns, aliases, and activation scopes into typed owned views;
2. `opto-hdl` owns importing and validating those views, elaboration
   flattening, source-index translation, canonical writable places, interface
   projection, and source locations; language-independent cyclic Proc proof,
   state analysis, and graph transformation are owned by `opto-ir` in Rust;
3. `opto-synth::frontend` owns acyclic procedural normalization, event-aware
   state extraction, resolved-driver normalization, and validated Word IR;
4. planning owns technology-independent resource and architecture eligibility;
5. mapping owns exact compatibility with the active Liberty target.

A source construct accepted through stage three is language-supported. A
stage-four or stage-five failure caused solely by absent target capabilities is
`supported_target_dependent`; it is not relabeled as an unsupported syntax
error. No stage may silently ignore an unsupported clause, selector, event,
driver, memory port, or primitive table entry.

### Canonical writable places

Source-facing array ranges and aggregate layouts retain declaration indices and
orientation. Writable expressions are flattened at the HDL boundary into
ordered signal or memory fragments with canonical storage offsets. Static and
procedural dynamic selections share one ordering rule: replacement bit zero
drives the least-significant bit of the selected value, even when that order
walks toward decreasing storage offsets.

Procedural dynamic writes use functional insert semantics over the latest
visible or scheduled value. A runtime-indexed continuous-assignment net lvalue,
such as `assign y[index] = data`, is rejected by the pinned standards frontend
during language legality and is classified as a tool-specific extension rather
than a profile gap. Continuous assignments that reach Opto use statically
identified driver fragments. Driver overlap and resolved-net semantics remain
owned by the frontend driver-normalization boundary.

### Statically expanded control flow

`for`, constant or type-domain-bounded runtime `repeat`, static `foreach`,
provably finite `while`/`do-while`,
and `forever` with a provably finite exit share one transient-CFG pipeline. The
native adapter records owned procedural expressions, activation-scoped locals,
ordinary CFG edges, and parent-before-child `LoopRegion` metadata. Every source
body is lowered once. Source syntax only determines whether the condition is
tested in the header or latch and how entry state is initialized. `break`,
`continue`, current-activation return, and lexical disable remain ordinary
control transfers; no Boolean `_broken` or `_continued` net simulates them.

The Rust IR layer is the sole owner of boundedness proof and elimination. Its
opaque certificate borrows the exact transient graph and referenced Word
module, so safe Rust cannot consume it against a different graph or mutate
either input between proof and elimination. No textual serialization or
runtime fingerprint is part of this authority contract. The eliminator
processes nested regions innermost-first, clones the certified finite trace,
and removes only the final unreachable latch-to-header edge. The final
persistent `ProcModule` remains acyclic and contains neither procedural locals
nor loop metadata.

The proof portfolio first applies monotone induction to relational pre-test and
post-test loops. Its sparse affine domain follows blocking temporary values
across blocks and merges the delta range from every backedge path. A proof
requires a loop-invariant bound, exact entry and bound extrema, strict progress
in one comparison direction, and a fixed-width no-wrap argument. This proves
wide induction variables and path-dependent steps such as `+1` or `+2` without
enumerating every reachable value. Exact finite-state enumeration remains the
fallback and models SystemVerilog width, sign, truncation, cast, selection, and
blocking-assignment semantics. Conservative known-bit extrema additionally
decide comparisons that hold for every value of a runtime integral operand.
Unknown module inputs otherwise fork reachable control unless short-circuit
Boolean facts decide the path. A repeated header state in the fallback domain,
a backedge path without proven progress, an internal cycle other than the
declared backedge, or a runtime-only exit with no finite local bound is
rejected.

The source-profile size boundary is the number of transient blocks after
structural expansion, currently 1,048,576 for one module. The maximum permitted
header visits is derived from that block budget and the natural-loop body size;
it is not a second solver-defined language limit. Exact-state count and
transfer-step guards protect the implementation from excessive analysis work.
Exhausting either analysis guard is reported as a boundedness-analysis
capability gap and contributes `known_gap` evidence for an otherwise in-profile
case; it does not redefine the loop syntax or structural profile boundary.
All limits are deterministic and independent of RSS, elapsed time, and worker
scheduling.

Runtime `repeat` snapshots the count once before entry, intersects its predicate
with the complete finite type-domain bound, and treats negative signed counts as
zero iterations. `foreach` carries an ordinal local and translates it through
the source declaration ranges. A finite `for` may omit any clause; initializer
and step expressions remain ordered effects, and `continue` reaches the common
step path. A conditionless `for` or `forever` must still have a provably finite
exit.

Classic module-scope Verilog induction variables are promoted by the Rust CFG
pass into fresh activation locals after a unique reaching initializer. They are
copied back only when
later procedural code, another process or continuous expression, an instance
connection, or an output/inout port observes the final value. Nested loops bind
outer induction locals explicitly, so inner updates participate in the outer
proof without turning automatic state into module-level hardware.

After elimination, activation locals are published as explicitly typed
process-local signals. Procedure normalization versions them jointly with
module signals, constructs predicate muxes at acyclic joins, folds exact
expressions, removes value-unreachable control, and canonicalizes dynamic
target offsets. The synthesis phase boundary then requires every process-local
signal to be dead and removes it. This ordering preserves source-time captures
across intervening blocking assignments as well as reset, memory, and
event-template recognition after structural loop expansion.

Lexical `disable` shares this completion machinery rather than introducing a
second procedural IR or a CFG backedge. Every active named sequential block and
inlined task activation that can be disabled owns a fresh flag. Later
statements in that scope and the cyclic loop continuation are guarded by the
inactive predicate. Exiting a task does not bypass its output and inout
copy-out. Hierarchical or cross-process block targets and a task target other
than the current activation are outside this profile and fail explicitly.

### Side-effecting procedural expressions

Blocking and compound assignment expressions use the same acyclic CFG builder
as statements and inlined functions. The lowering freezes a runtime-selected
lvalue address before evaluating the right-hand side, stores the converted
right-hand side in a process-local value, writes the lvalue from that value,
and returns it. Nested assignments consequently retain source evaluation order
even when later operands update the same signal.

Prefix and postfix increment and decrement expressions share this prelude.
Prefix forms return the materialized updated value; postfix forms snapshot and
return the old value before writing the update. Both forms freeze a dynamic
lvalue address before reading it.

Lazy expression regions are control flow rather than an unconditional list of
effects. The two arms of `?:` receive distinct prelude fragments, while the
right operand of `&&` or `||` is reachable only when the left operand does not
decide the result. Nonblocking and timing-controlled assignments do not produce
values in this profile.

Assignment patterns, including replicated patterns, evaluate their elements in
source order. The resulting values are then placed into the canonical packed
or unpacked storage layout. A replicated element with side effects is evaluated
once per replication; it is not evaluated once and copied. Static assignment-
pattern expansion has its own deterministic 65,536-element representation
limit; procedural loops instead use the expanded transient-block profile
boundary described above.

### Pattern conditions and pattern case

Pattern lowering accepts fixed-size structures and tagged unions with constant,
wildcard, variable, and recursively nested fields. Structure fields are
selected by canonical storage offsets. Packed and unpacked tagged unions use
the same synthesis representation: the member discriminant occupies the upper
bits, the active payload occupies the low bits, and any width difference
between members is care-free padding. Nested tagged unions recursively include
their own discriminant. Each `.name` variable captures its matched field into a
scoped process-local value at pattern-evaluation time; later conditions may
modify the original expression without changing that binding. Bindings are
visible only to following `&&&` conditions and the true conditional branch or
owning pattern-case item.

Condition lists lower left to right. A prelude for a later condition is placed
behind the preceding predicate, and a pattern-case filter is similarly guarded
by its pattern. Pattern-case dispatch snapshots the selector once and evaluates
items in source priority order, so a filter cannot change the value seen by a
later item. These operations produce only ordinary acyclic Proc control,
process-local values, slices, comparisons, and Boolean predicates.

Normal constant patterns require exact binary constants. `casez ... matches`
may use `Z` bits as compile-time mask positions; `casex`, exact runtime X/Z
observation, and values without a fixed synthesis representation remain
explicit gaps.

### Subroutine reference aliases

Synthesizable `ref` arguments on tasks, void functions, and value-returning
functions are eliminated while the subroutine body is inlined. A formal reads
and writes the actual writable place directly; it is not lowered as an
`inout`-style copy-in/copy-out temporary. Two formals bound to the same actual
therefore observe each other's blocking writes immediately.

Automatic subroutine bodies may contain Slang formal-symbol clones. Alias
lookup uses direct symbol identity as its fast path and originating syntax
identity as the stable clone relation, under a scoped binding stack for nested
and recursive calls. It does not use a hierarchical-name lookup or retain a
source-level alias in Proc or Word IR.

The language permits a runtime-selected element of an unpacked array as a
`ref` actual. Its canonical flattened offset is evaluated into a process-local
selector at call entry, before any subroutine effect; subsequent writes to an
index variable cannot redirect the alias. Packed bit and part selects are not
legal `ref` actuals and remain owned by Slang's language-legality diagnostic.

Module `ref` ports and `ref` modport members use a separate hierarchy alias
contract. Definition-local Word IR retains a typed `Ref` direction until linked
RTL elaboration binds each occurrence to its enclosing variable actual. Whole
variables and static flattened members reuse the exact parent signal range;
runtime-selected unpacked-array elements compose their dynamic offset into
child reads and writes. The linked root cannot retain a reference port, and a
preserved hierarchy boundary around one is rejected rather than approximated
as an `inout` net.

### User-defined primitives

A combinational UDP is eliminated at structural lowering from Slang's
validated table representation. Each binary-reachable `0` or `1` row becomes
a conjunction over its exact inputs, with `?` and `b` treated as binary
wildcards. The rows feed a deterministic mux network in source priority order.
An uncovered binary combination or a row whose output is `x` remains a
care-free one-bit Word value; an input symbol `x` matches only a runtime unknown
and therefore does not become a binary predicate.

Level-sensitive sequential UDP rows additionally match the current output
state. Each binary-reachable row whose next state is not `-` becomes a guarded
blocking Proc assignment; unmatched and `-` rows are hold paths. The ordinary
`CombinationalOrLatch` normalization therefore decides whether the table is a
latch, combinational function, constant, or invalid feedback without a second
state representation.

UDP input count and table-row count have deterministic structural limits.
For edge-sensitive tables, binary-reachable `r`, `p`, and `(01)` transitions
map to a positive-edge event; `f`, `n`, and `(10)` map to a negative-edge
event; and `*` maps to both. Transitions that require an X endpoint are absent
from the binary synthesis domain. Rows whose next state is `-` remain holds
and do not add events. A single transition input yields one nonblocking flop
procedure with current-level and current-state predicates. With distinct
transition inputs, exactly one port must have reachable rows that produce both
binary states; it is the data clock. Every other transition port must produce
one fixed binary state, and its post-edge active level is retained so ordinary
reset-template analysis can prove an asynchronous control. Ambiguous clocks
and multiple data-update event ports remain explicit gaps rather than being
merged into a synthetic clock. A level-sensitive update row in the same table
can add an asynchronous event when it is state-independent, writes binary zero
or one, and constrains exactly one input to its active level; the ordinary
reset-template analysis then proves the combined procedure. Other mixed level-
sensitive update rows remain explicit gaps until they have an exact
synthesizable event template. UDP initialization remains outside the explicit-
reset ASIC profile.

### Four-state and resolved behavior

Runtime X/Z observability is supported only where it has a realizable profile
contract. X remains a care-free Word bit under the existing architecture and
cannot be reinterpreted as a runtime unknown detector merely to accept
`===`, `!==`, or a system function. Masked equality with constant wildcard
positions is realizable; X-sensitive simulation tests remain outside the
positive hardware profile unless a later explicit encoded-state contract
supersedes this decision.

The implemented comparison subset lowers `===` / `!==` when both operands have
intrinsic two-state types. It lowers `==?` / `!=?` for a two-state left operand
and either a two-state right operand or a constant X/Z wildcard mask. A
four-state left operand remains an explicit rejection because the result would
observe runtime unknown state that Word IR intentionally represents as care-
free.

Runtime `$countones`, binary-control `$countbits`, `$onehot`, and `$onehot0`
lower to bounded combinational networks. `$isunknown` remains in the same
intentional X/Z-observation rejection class. Integral runtime-base power with a
known nonnegative exponent lowers to a fixed-width multiplication chain under
a deterministic structural limit; a runtime exponent is accepted only for the
existing power-of-two shift form.

Tri-state drivers remain data-plus-enable contributions until a typed
resolution boundary. Internal resolution may lower to ordinary logic when the
profile semantics are realizable. A top-level `inout` retains its public port
and requires a compatible three-state Liberty cell, IO pad, or an explicitly
retained black-box boundary. Drive strengths, delays, and charge storage do not
become Boolean approximations.

The first implementation slice represents `bufif0` / `bufif1` and conditional
high-impedance assignments as a typed Word `TriState` operation. `wand` and
`wor` consume disabled drivers through their exact one and zero identities.
`notif0` / `notif1` use the same typed contribution with explicit data
inversion and their declared enable polarity. Ordinary built-in logic gates
lower to the corresponding structural operations, while `pullup` and
`pulldown` become exact one-bit constant network drivers.
Ordinary multi-driver `wire` / `tri` resolution and public `inout` publication
now preserve one scalar data/enable contribution per target bit and materialize
polarity-compatible Liberty three-state cells on a shared mapped net. Missing
target cells are reported as target-dependent mapping failures. Strength,
charge-storage, and unconstrained contention semantics remain outside this
implemented row rather than being approximated as Boolean logic.
Switch-level `cmos`, `nmos`, `pmos`, their resistive variants, and the `tran`
families are intentionally excluded: their transistor-strength or
bidirectional pass-network semantics do not have an exact target-independent
Boolean synthesis contract.

### Memories and sequential events

First-class memory IR accepts the profile's logical ports independently of the
chosen implementation. A register-bank candidate is eligible only when its
clocking and reset contract is physically representable. Multi-clock and
special-reset memories remain language-supported when their IR is complete,
but mapping succeeds only with an exact macro unless another canonical
implementation is defined.

Register-bank lowering now assigns clocks per scalarized word rather than per
memory declaration. Conservative unsigned address bounds may therefore prove
that independent write clocks own disjoint words; those banks lower exactly to
separate register clock domains. If two distinct clocks may update the same
word, the register-bank candidate remains ineligible instead of constructing a
synthetic clock. General overlapping multi-clock ports still require an exact
multi-port macro contract. The implemented macro check binds every logical read
and write port to a Liberty memory port with matching width, clock edge, enable,
mask, and read-timing semantics. Distinct-clock write ports may overlap only
through such a macro. Multiple logical write ports on the same clock may bind
separate physical ports when conservative Boolean facts prove their effective
enables mutually exclusive. Other same-clock ports remain ineligible unless a
future library contract can express their procedural priority and
simultaneous-write collision behavior exactly.

Memory addresses are canonicalized to the physical address width only when
unsigned range analysis proves that every discarded high bit is zero. This
lets safe widened source arithmetic match an exact macro without changing
out-of-range behavior. Liberty timing declared on a bus is expanded onto each
physical output pin, so memory boundaries participate in ordinary timing
analysis rather than receiving invented or missing scalar arcs.

Only a contiguous leading prefix of fixed unpacked dimensions becomes a
first-class memory shape. Automatic unpacked arrays inside a procedure are
temporary values and remain process-local flattened signals. Module state with
unpacked dimensions separated by an unpacked struct or another aggregate
boundary remains canonical flattened register state. Unpacked-struct fields
receive deterministic bitstream offsets in declaration order, and all nested
static or dynamic member and element selections compose through the same
signal extract/insert path. This fallback preserves legal source semantics
without inventing a rectangular memory or rejecting an irregular aggregate.
Likewise, an unpacked array written by a multi-event procedure remains
flattened register state. A statically bounded asynchronous reset loop over all
elements and a dynamic address write in the clocked data phase therefore share
the ordinary per-bit register reset and dynamic-insert semantics. Such state is
not admitted as a reset-free first-class memory candidate.

The two opposite edges of one scalar clock form a distinct canonical template,
not a clock-plus-reset pair. Such state lowers to positive- and negative-edge
phase banks driven by the same complete next-state expression, including
synchronous reset priority and old-state feedback for conditional holds. A
clock-level mux selects the bank written by the most recent edge. Named bank
signals keep both state operations in the unique regional sequential shell.
This is exact without a synthetic clock and without a dedicated dual-edge
Liberty primitive.

Sequential normalization never combines unrelated event signals into a
synthetic clock. An event pattern must reduce losslessly to a typed clock,
ordered reset/enable behavior, and per-bit state transition, or match an exact
target-dependent sequential primitive contract. Otherwise it remains a
reviewed profile gap or intentional rejection.

A single positive- or negative-edge event with an `iff` qualifier lowers to
the same clock and conditional hold as an explicit entry `if`, producing an
ordinary register enable. One qualified clock may coexist with unqualified
asynchronous-control events: their active levels bypass the entry guard, and
the ordinary reset-template analysis must prove that the extra executions are
idempotent reset actions. Compile-time true qualifiers are removed before this
analysis, while compile-time false events are discarded. The same
canonicalization uses the guaranteed post-edge signal level, so `posedge s iff
s` and `negedge s iff !s` become unqualified and their opposite-polarity forms
are discarded; eliminating every event is an explicit error. Every event-list
member retains a typed event identity, scalar expression, edge, and optional
runtime qualifier through the Proc normalization boundary. Static and runtime
bit-selected clocks therefore reach the sequential operation as their exact
Word value. Duplicate events for the same value and edge OR their qualifiers,
and the two opposite edges of one scalar value use its post-edge level to
select the matching qualifier in the dual-edge phase-bank template. Independent
unrelated clocks remain distinct; they are not approximated by a common guard
or synthetic clock and require an exact sequential implementation contract to
be accepted.

## Determinism, scalability, and QoR impact

Expansion order follows source order, declared range order, and stable arena
identity. Feature behavior cannot depend on hash iteration, thread completion,
allocator state, or host scheduling. Static loops, exponentiation networks,
selector networks, flattened arrays, UDP tables, and register-bank
materialization use deterministic structural bounds where their work can grow
combinatorially.

Canonical selectors avoid copying complete designs and permit nested static
selections to collapse before Word publication. Dynamic selectors use bounded
barrel networks already owned by Word lowering. Memories stay O(ports plus
metadata) until regional implementation selection. New syntax is not allowed
to select a second synthesis pipeline or retain an alternate full-design IR.

QoR changes caused by new lowering forms require representative area, timing,
cell-composition, runtime, and memory measurements. Passing a syntax corpus is
not evidence that a new lowering is suitable for production-sized RTL.

## Alternatives

Treating Yosys behavior as the synthesis specification is rejected. Yosys
remains a valuable independent oracle, but its tests include extensions,
testbenches, and driver scripts that do not define Opto's public contract.

Using the complete IEEE grammar as the positive profile is rejected because
simulation scheduling, delays, strengths, force/release, process control, and
other constructs do not describe the targeted static ASIC hardware model.

Accepting syntax and silently discarding unsupported semantics is rejected.
Keeping a fallback frontend or alternate synthesis flow for difficult
constructs is also rejected by the one-pipeline architecture.

## Validation and rollout

The rollout is incremental and keeps every merge independently reviewable:

1. publish the classification schema and feature/design-unit inventory;
2. complete canonical static, nested, and dynamic writable selections;
3. migrate every procedural loop form to the owned cyclic Proc proof pipeline;
4. add exact Word lowering for the reviewed operator and system-function set;
5. extend memory and sequential-event contracts before mapper support;
6. introduce typed tri-state contributions and target-dependent inout mapping;
7. eliminate supported ref/modport aliases during elaboration and lower the
   supported primitive and UDP tables;
8. close profile-related formal deferrals and expand pinned real-design gates.

Every behavior has one primary test at its lowest owning layer, a negative
boundary test, and higher-layer evidence only for a distinct seam or
independent oracle. Synthesis transformations receive combinational or
sequential equivalence evidence. The qualification baseline changes only
after the exact new required set, proof set, exclusions, and remaining gaps
have been reviewed and hashed.
