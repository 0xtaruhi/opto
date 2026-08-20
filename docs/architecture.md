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

### HDL support claims

SystemVerilog support is stated against a versioned Opto ASIC synthesis
profile, never as an unqualified percentage of the complete language grammar
or an upstream repository's file count. A reviewed feature or independently
observable design unit is classified as supported and proved, supported but
target-dependent, intentionally rejected, invalid or non-hardware, or a known
capability gap. The exact required inventory and its evidence are hashed by the
qualification baseline. [RFC 0012](rfcs/0012-synthesizable-systemverilog-profile.md)
records the proposed profile design and rollout; its proposed status does not
claim that the pending feature inventory is implemented.

Language acceptance and target realization are separate contracts. The HDL
and synthesis frontend may publish a valid technology-independent memory,
sequential, resolved-net, or bidirectional interface whose implementation
requires an exactly compatible Liberty macro or cell. Absence of that target
resource produces an explicit planning or mapping diagnostic rather than being
reported as a syntax failure or triggering an alternate implementation path.

Fixed-shape streaming concatenations include constant and runtime-base `with`
selections over fixed unpacked arrays. Indexed selections retain declared range
orientation, stream direction, slice ordering, left-aligned conversion fill,
and per-element out-of-range defaults. A dynamic simple range, dynamically
sized operand or element, or aggregate without Opto's canonical flattened
bitstream layout is outside the synthesis profile and fails with a structured,
source-located diagnostic. Lowering deterministically caps selected elements and
flattened bitstream parts at 65,536 each.

The frontend removes source constructs that become semantically empty after
preprocessing and elaboration. In particular, an empty `initial` block is not
published as a procedure. This does not admit time-zero initialization into the
ASIC synthesis profile: any reachable assignment, declaration initializer,
memory preload, system task, timing control, or other executable initial
behavior remains an explicit unsupported-profile error.

Structural validation requires the selected root design to expose an external
port interface. A reachable child definition may have no ports; it remains
subject to the same name, reference, connection, driver, and typed-ID checks as
every other definition.

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
records many-to-many source-operation provenance independently from occurrence
identity. `OperatorOccurrenceId` hashes the semantic signature, the complete
sorted set of stable source-operation anchors, and a deterministic same-key
ordinal. Source spans are diagnostics only and never infer ownership or
provenance. This keeps one explicit provenance relation while allowing one
source operation to contribute to multiple generated semantic operators. The
global and region-private publication paths derive their source results and
inputs through the same boundary rule: results are every internal value not
consumed by the source-operation set, and inputs are every external value
entering that set. Results are sorted, while inputs retain semantic operand
order before deterministic structural completion; publication never invents a
representative when shared logic has multiple source exits. The
operator-occurrence identity change increments the synthesis-cache ABI, so a
checkpoint produced with the former anchor semantics is rejected rather than
reinterpreted.
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
Slang procedural tree
  -> native semantic adapter
  -> owned transient Proc graph
       (procedural expressions + activation locals + ordinary CFG backedges)
  -> structural validation and LoopRegion validation
  -> CFG loop-state promotion and live-out placement
  -> deterministic boundedness proof certificate
  -> certified loop elimination
  -> path-sensitive exact-local specialization and dead-local-effect removal
  -> activation-local publication as typed process-local signals
  -> sealed acyclic ProcModule
  -> CFG canonicalization
  -> joint source-order state propagation for module and process-local signals
  -> typed event-aware reset/enable extraction
  -> canonical Predicate DAG
  -> mandatory process-local removal
  -> Word IR
  -> validated Word IR
```

The final `ProcModule` contract is acyclic. Its validator owns that invariant;
downstream synthesis does not serve as the first cycle check. The construction
core is shared with a separate, non-persistent transient graph whose value
domain is `ProcExprId` and whose assignment domain includes `ProcLocalId`.
Transient local reads are place reads at their CFG use sites rather than
referentially transparent module values. Loop syntax remains metadata:
ordinary edges carry control semantics, while validated `LoopRegion` records
header, body entry, latch, exit, lexical parent, and condition placement.

The Rust boundedness analyzer is the only constructor of an opaque proof
certificate. A certificate borrows its exact semantic graph and referenced
Word module and records the region, proof method, explored-state count, and
maximum header visits. Ownership and lifetimes prevent consumption against a
different graph without a debug-format or serialization fingerprint. The
eliminator requires innermost-first order, clones the natural region, and
redirects only the final certified-unreachable backedge. After loop analysis,
activation locals are published as explicitly typed `SignalKind::ProcessLocal`
signals. The existing procedure normalizer then versions those values jointly
with module signals, so a local capture cannot move across an intervening
blocking assignment. The process-local signals are mandatory transient state
and are removed before normalized Word IR leaves the synthesis frontend.
The proof portfolio first applies monotone induction to relational pre-test and
post-test loops. It tracks an induction local as an affine delta, follows
blocking temporary assignments across CFG blocks, intersects facts at joins,
and merges the minimum and maximum delta from every reachable backedge path.
A certificate is issued only when every path makes progress in the comparison
direction, the bound is loop invariant, entry and bound extrema are known, and
the complete fixed-width update range cannot wrap. The resulting arithmetic
header-visit bound is checked directly against the expanded-block profile
limit, without enumerating each induction value. Exact-state enumeration is the
conservative fallback for transition relations outside that domain;
module-level runtime values fork control conservatively, repeated reachable
local states reject that proof, and all work uses deterministic limits. State
and transfer-step guards are implementation protection and their exhaustion is
an analysis capability gap rather than a language-profile boundary. Before
either traversal, the analyzer removes the declared latch-to-header edge and
requires the remaining natural region to be acyclic; CFG joins can therefore
be merged without being confused with an undeclared internal cycle.

Persistent signal-backed variables enter the transient graph without an AST-
side induction-variable classification. Before proof, Rust intersects blocking
signal writes with signal reads in each top-level natural region, promotes the
resulting recurrence state to `ProcLocalId`, and rewrites nested regions to use
the same local. Copy-in values come from unique CFG reaching definitions when
available, otherwise from the visible signal value. No synthetic copy-back is
emitted. Live-out writes keep their original signal target and bit selection
alongside the promoted local update; later loop expansion therefore
specializes dynamic selections without inventing a whole-signal owner. This
pass owns recurrence discovery, entry-value flow, and live-out policy; the
Slang adapter retains only lexical local allocation for `for` declarations and
`foreach` indices.

When distinct procedures use the same otherwise unobserved module-scope
variable solely as classic loop induction state, each procedure receives an
independent activation local. Reads after that procedure's loop retain its
local final value, while no persistent multi-driver copy-back is created.

Transient procedures own explicit block-ID lists rather than the final IR's
contiguous block ranges. Loop elimination can therefore retain iteration zero,
append only additional natural-loop copies, and update only the affected
procedure and loop-region table. Unrelated procedures and block IDs remain
stable. The complete graph is validated once when the combined loop pipeline
finishes; an independently exposed proof operation remains read-only and
cannot trigger a whole-module clone.

After the final backedge is removed, a bounded exact-state traversal
specializes local-dependent expressions separately at each reachable CFG
occurrence. Static loop addresses and conditions therefore become constants
without folding a runtime-dependent branch. Local assignments with no
remaining reachable read are removed before process-local publication;
exact-infeasible clones do not keep otherwise dead activation state alive.
Final publication materializes only the expression closure referenced by
exact-reachable blocks. This is required for memory reads: an unreachable
owned read must not create a physical read port merely because its old
expression remains in the transient arena after specialization.

The Slang adapter publishes every nontrivial `for`, `repeat`, `foreach`,
`while`, `do-while`, and `forever` loop through this transient cyclic path.
The source body is lowered exactly once. Syntax determines only graph shape:
pre-test loops branch in the header, post-test loops branch in the latch,
`repeat` snapshots its count at entry, and `foreach` carries an ordinal local
that is translated to declared indices. `break` targets the region exit,
`continue` targets the common continue funnel, and return and lexical-disable
predicates are part of the loop continuation semantics. Nested regions retain
their lexical parent. There is no native iteration interpreter, trace
materializer, `_broken`/`_continued` flag protocol, or second boundedness
implementation.

The native procedure builder owns a temporary block, edge, terminator, local,
expression, and region arena and does not write the published FFI
representation while source lowering is in progress. It may remove wholly
unreachable source fragments, but a reachable loop whose structural exit is
unreachable is rejected rather than silently losing its region record. The
Rust HDL boundary imports the resulting graph without interpreting loop
syntax, proves and eliminates innermost regions before their parents, and then
requires an acyclic graph before local substitution.

Monotone induction models fixed-width signed and unsigned comparison semantics,
path-dependent positive or negative affine steps, loop-invariant runtime-bound
extrema, and pre-test versus post-test condition placement. It refuses a proof
when an update path can stall, reverse direction, or wrap. Exact enumeration
models the remaining SystemVerilog widths, signedness, truncation, casts,
blocking local updates, and branch forks. Known-bit extrema conservatively
prove relational predicates that hold across the complete finite type domain,
including termination at the maximum of a runtime signed or unsigned bound.
Other runtime module values remain unknown unless Boolean short-circuit
information decides a path. A finite proof may therefore use an arithmetic
induction invariant or exact local state while allowing runtime inputs to cause
an earlier exit. A state that reaches the header twice in the fallback domain,
a backedge path with no proven progress, an undeclared internal cycle, or a
runtime-only exit without a finite local bound fails explicitly. Runtime
`repeat` uses both its captured count and the finite type-domain bound; negative
signed counts enter no iteration. The source-profile boundary is at most
1,048,576 transient blocks after expansion, so the permitted header visits are
derived from the actual natural-loop size. Analysis guards are reported
separately as capability gaps. Neither category depends on host memory, time,
or scheduling.

`for` may omit initializer, condition, or step clauses. Initializers and steps
remain ordered procedural effects, and `continue` reaches the step path.
Conditionless `for` and `forever` require an exit that the same graph analysis
can prove. Classic module-scope induction variables are promoted to fresh
activation locals from their CFG reaching definitions. Copy-back occurs only
when the value is live later in the process, in another non-peer process or
continuous expression, at an instance connection, or through an output/inout
port. Independent classic induction peers use the isolation rule above.
Nested loops explicitly bind outer induction state so inner updates participate
in the outer proof.

After elimination, exact procedural expressions are folded before Word
materialization. Constant branches are converted to jumps and newly
unreachable blocks are omitted. Constant dynamic target offsets become static
bit ranges. These canonicalizations are semantically required for downstream
event/reset and memory recognition; for example, a reset loop over a flattened
register bank publishes disjoint static reset slices while retaining a dynamic
runtime write port.
A lexical `disable` of an active named sequential block
or of the current inlined task activation uses the same acyclic completion
model: one activation-local predicate guards the remainder of that scope and,
when an outer scope is exited from a loop body, the loop continuation
predicate. Task argument copy-out remains after the task scope exit.
Hierarchical, cross-process, and cross-activation task disable are diagnosed
explicitly instead of being approximated. State
propagation is per target: an effect that changes another signal cannot
manufacture a mux or guard for the target being lowered. Sensitivity events
remain typed controls instead of being rediscovered from an arbitrary Boolean
expression. Each persistent event row has its own `EventId`, scalar Word
`ValueId`, edge, and optional scalar `iff` value. Consequently a static bit
selection remains a one-bit `SignalRef`, a runtime bit selection remains a
`DynamicExtract`, and an event-local qualifier cannot be confused with a
qualifier on another sensitivity-list member.

A valued function return stores its result and transfers directly to the
activation exit. A task or void function `return;` transfers to that same exit
without a synthetic returned flag. The exit precedes output/inout copy-out, so
returns from nested conditionals or cyclic regions cannot bypass caller-visible
writeback. Valued returns in tasks or void functions and valueless returns in
value functions remain source-located errors.

A scalar `iff` qualifier on a positive- or negative-edge clock becomes a
conditional hold after event/reset classification and therefore an ordinary
register or memory-write enable. With one qualified clock and unqualified
events, the active levels of the unqualified events bypass that enable;
reset-template validation must prove that those events are exact asynchronous
controls. Compile-time true qualifiers canonicalize to unqualified events and
compile-time false events are removed before CFG construction. The guaranteed
post-edge signal level similarly proves direct or inverted self-qualifiers true
or false; an empty resulting event list is rejected. Duplicate members for the
same value and edge combine their independent qualifiers with OR, while an
unqualified duplicate dominates. Opposite edges of the same value retain
independent qualifiers and use the post-edge clock level in the exact dual-edge
phase-bank template. Unrelated clocks are never combined into a synthetic
clock; a multi-clock state pattern without an exact sequential implementation
contract is rejected after preserving its event identities for diagnosis.
Nested asynchronous clear/set branches are admitted only when normalization
can produce one data clock and one ordered reset list whose constant result is
defined for every combination of asserted controls. List order preserves
source priority. Conflicting path stacks, nonconstant asynchronous updates, and
unrelated data clocks remain structured rejections.

Subroutine `ref` arguments are also eliminated before Proc publication. An
inlined formal is a scoped binding to the actual canonical writable place, so
formals that share one actual observe blocking writes immediately. Dynamic
unpacked-array element aliases snapshot their flattened selector into a
process-local value at call entry. Automatic-body formal clones resolve through
their common originating syntax identity; no name-based alias lookup, copy-out
approximation, or source-level reference node survives into Proc or Word IR.
This contract applies to tasks and void or value-returning functions.

Static SystemVerilog net aliases are eliminated as flattened-bit equivalence
classes. Equal-width whole nets, static slices, and unambiguous concatenations
union their bits transitively and select the lexicographically smallest
`(flattened name, bit)` representative. Reads and lvalues are rewritten to that
representative, so all members share one resolution domain instead of becoming
directional assignments. Dynamic alias selections, incompatible resolution
kinds, and mappings without one contiguous exact representation are rejected.

Module `ref` ports and `ref` modport members retain a typed `Ref` direction
only in definition-local Word IR. They are not resolved `inout` nets. Linked
RTL elaboration binds the child port signal directly to the parent variable
actual before cloning structural values or Proc effects. Whole variables and
static flattened members reuse the exact parent signal range; runtime-selected
unpacked-array actuals retain their canonical dynamic offset, so child reads
become extracts and child writes compose into the same dynamic target. The
occurrence remap is applied to both Word values and Proc targets before
synthesis, and no reference port survives in the linked root. A root `ref`
port or a hierarchy-preservation directive around a reference-port instance is
rejected because it has no enclosing variable binding at that boundary.

Explicit modport ports are lowered from Slang's elaborated connection rather
than reconstructed from source syntax. The flattened child port therefore
retains the modport member's source identity, direction, width, signedness, and
aggregate layout, while the parent connection retains the complete typed
expression. Input expressions may be arbitrary synthesizable values. Output
expressions must pass Slang's lvalue checks and lower through the ordinary
structural lvalue path. An `inout` member must resolve to one contiguous net
alias; linked elaboration substitutes that alias before cloning the child and
merges its physical resolution policy into the parent net. Discontiguous
bidirectional expressions are rejected instead of approximated as directed
connections.

Imported and exported modport functions and tasks use the same static
subroutine-inlining contract as direct calls. Lowering follows the unique
elaborated implementation, recursively discovers interface storage used by
helper calls, and surfaces those dependencies as typed flattened input or
exact-reference ports. Interface behavioral processes and scalar input/output
constructor connections are materialized in the enclosing module so an
exported callback executes at its declared interface site. Missing or
ambiguous extern implementations, virtual or DPI dispatch, writes to captured
nets, bidirectional constructor aliases, and other non-synthesizable method
members produce structured profile failures; no dynamic callback object enters
Proc or Word IR.

Side-effecting procedural expressions lower through an expression prelude that
is part of the enclosing acyclic Proc CFG. A blocking or compound assignment
expression first snapshots any dynamic target address, evaluates and converts
its right-hand side into a process-local result, writes the target from that
result, and returns the same result. Prefix and postfix `++` / `--` use the same
address snapshot and separately materialize the specified new or old result.
This prevents a later sibling expression from changing the earlier
expression's value. Conditional-expression branches
own separate prelude fragments, and the right operand of `&&` or `||` is placed
only on the reachable CFG edge; side effects are therefore neither hoisted out
of `?:` nor executed across a short-circuit boundary. Timing-controlled and
nonblocking assignments remain statements rather than value-producing
expressions.
An `always_comb` nonblocking statement is accepted only when its target signal
is not read anywhere during that procedure activation, including data and
control expressions, dynamic target offsets, and memory addressing. Under this
condition end-of-step publication and the generated combinational connection
are equivalent. A same-activation read is schedule-sensitive and is diagnosed
before assignment-mode normalization.

Simple, structured, and replicated assignment-pattern elements are evaluated
in source order into ordinary expression values. Only after evaluation does
lowering reorder those values into the canonical unpacked-array storage order;
packed structures retain declaration order. Replication repeats evaluation as
specified rather than cloning one previously evaluated value. Expansion is
deterministically limited to 65,536 elements.

Pattern conditions use that same prelude-backed CFG contract. A structure
pattern recursively slices the canonical synthesis aggregate and builds one
predicate from constant, wildcard, variable, structure, and tagged-union
fields. Packed and unpacked tagged unions share one storage contract: an upper
discriminant, care-free padding, and the active payload in the low bits. The
discriminant width is the minimum number of bits required for every declared
member, and nested tagged payloads recursively use the same layout. A pattern
variable is a scoped binding to a process-local snapshot taken when its pattern
is evaluated; it is not an alias of the source expression. Subsequent `&&&`
conditions and a pattern-case item filter are reachable only after the earlier
predicate succeeds. Pattern-case selectors are also snapshotted once before
source-ordered priority dispatch, so filter side effects cannot redirect later
items. No pattern object, tagged value, or binding survives into Proc or Word
IR. Exact runtime X/Z matching remains an explicit profile gap.

Validated UDP tables are likewise eliminated before Word publication.
Binary-reachable combinational rows become bounded predicates in source order;
wildcard rows share the ordinary Boolean network, while uncovered and `x`
output rows remain care-free. Level-sensitive sequential rows become guarded
`CombinationalOrLatch` assignments, with `-` and unmatched rows retaining
state. For edge-sensitive tables, binary-reachable `r`, `p`, and `(01)`
transitions become positive-edge events; `f`, `n`, and `(10)` become
negative-edge events; and `*` can request both. Hold-only transitions do not
create hardware events. One transition input may be proved as the unique data
clock because its reachable rows produce both binary states. Distinct
transition inputs are then admitted only when every such input writes one
fixed binary state; their active post-edge levels remain explicit predicates,
and ordinary reset-template analysis must prove them as exact asynchronous
controls. A table with no unique data clock or with multiple data-update event
inputs remains explicitly unsupported instead of being approximated. A mixed
level row becomes an asynchronous control only when its binary predicate
constrains exactly one input level, is independent of current state, and writes
a constant state; other mixed level-sensitive updates remain unsupported.

Linked elaboration completes the SSA value domain before regional ownership is
established. An omitted input of an expanded definition receives a type-correct
completion value (`X` for a four-state input and zero for a two-state input), and
that value propagates through the ordinary Word graph. Four-state `X` remains
care-free; two-state zero is the language-domain value. An instance retained by
`dont_touch`, `keep_hierarchy`, or black-box status instead preserves the absent
binding as part of its structural interface. After procedures and resolved nets
are lowered, otherwise-undefined bits of source-observable output ports are
sealed with the same type-correct completion rule. The same sealing boundary
completes holes in a partially driven internal single-driver aggregate, so a
whole-value SSA read cannot make unused packed-layout padding look like a
missing producer. A wholly undriven internal that remains source-observable is
likewise completed as a source-level unspecified value; an unobservable one is
left dead. Signals with a dynamic target, resolved nets, ports, and generated
logic are never completed by this rule. This sealing happens only at the
source-to-Word boundary, so a producer lost by a later transformation remains
an internal consistency failure rather than being hidden as a don't-care.

Built-in `and`, `or`, `xor`, `nand`, `nor`, `xnor`, `buf`, and `not` instances
lower to ordinary structural Word operations. `pullup` and `pulldown` lower to
one-bit constant network drivers; an explicit strength annotation is still
rejected. Verilog switch-level primitives (`cmos`, `nmos`, `pmos`, their
resistive variants, and the `tran` families) remain outside the synthesis
profile because transistor strength and bidirectional pass-network semantics
do not have an exact target-independent Boolean lowering.

High-impedance source drivers enter Word IR as an explicit `TriState` operation
containing data and a polarized scalar enable; `Z` is not smuggled through an
ordinary mux. Wired-AND and wired-OR normalization consume that operation by
substituting their exact disabled-driver identity before Boolean planning.
`triand` and `trior` use those same wired identities. `tri0` and `tri1` retain a
default physical pull contribution enabled only when every ordinary driver on
the bit is disabled, so no `Z` reaches Boolean planning. Undriven `supply0` and
`supply1` become typed constant drivers at this boundary; an explicit supply-
net driver is rejected because it requires out-of-profile strength resolution.
These policies append explicit `SignalResolution` variants while preserving the
existing serialized variant tags; the generic archive envelope version is
unchanged, and an older reader rejects a new variant rather than decoding it as
an older resolution policy.
Ordinary resolved `wire` / `tri` nets and public `inout` ports retain the typed
resolution boundary. Frontend normalization scalarizes each contribution into
one `TriState(data, enable)` operation per target bit. The region graph owns the
data and enable cones but deliberately excludes the physical driver shell, so
ordinary Boolean lowering cannot reinterpret `Z`. Global materialization keeps
the resolved target as a shared mapped net and binds every contribution to a
polarity-compatible Liberty three-state cell. Selection excludes `dont_use`,
special-purpose, sequential, memory, and structurally incompatible cells, then
orders compatible choices by normalized area, cell name, and library index.
Missing active-high or active-low target support is a mapping diagnostic rather
than a frontend syntax failure.

Observable outputs normally require one physical driver. An output sharing a
mapped net with an `inout` boundary or a Liberty three-state output instead uses
resolved connectivity: it must have at least one potential driver and may have
multiple physical contributors. Top-level `inout` qualification exercises the
published boundary directly. Resolved-driver CEC uses Yosys `tribuf -formal`
normalization on both designs, which replaces every `$tribuf` with Boolean
logic and asserts the binary-domain requirement that at most one contribution
is enabled. This includes resolved top-level outputs without pretending to
model the external electrical environment of an `inout` port.

Four-state `X` is the Word representation of a care-free SSA bit, not an early
choice of Boolean zero. Fixed combinational dataflow supplies value facts in
topological order; regional observability and cover propagate care from frozen
roots over the reverse dependencies. A register, latch, or memory is not itself
an observability root: storage outside the port, preserved-object,
retained-instance, explicit-value, and observable-memory-read closure is dead
and does not enter regional ownership. The `word::uses` observability closure
is the sole definition of global SSA liveness. It classifies root signals,
externally rooted and region-publication connections, retained-instance values,
observable memories and their port controls, and the complete reverse
dependency closure in one snapshot.
Regional reachability, FSM discovery, operator demand and sharing, sequential
selection, mapping publication, and global bit lowering consume projections of
that snapshot; those stages may add a local phase boundary or project a
physical shell such as tri-state data and enable, but they must not rescan
ports, connections, instances, memories, or state to seed another global root
set. In particular, FSM discovery rejects dead state through this snapshot
before bounded transition extraction or symbolic analysis begins. An internal
dynamic connection reached by the closure is a publication connection because
its target cannot be represented as one static driver edge. An internal memory
becomes observable only when one of its read-data signals is reached; dead
memories have no region owner or implementation decision and are erased when
aggregate memory resources are lowered.
Before global bit lowering changes state-boundary identities, the final region
graph freezes each live sequential operation together with its region row. Bit
lowering resolves that record once to the emitted scalar state operations;
substrate observation and sequential-cell materialization consume the resolved
records and never rediscover liveness or ownership from the mutated Word graph.
Rewrites use sparse dependency worklists when a changed fact can affect another
result. Registers, latches, and memories are explicit boundaries, and source
combinational cycles are diagnosed instead of being treated as a fixed-point
optimization problem. Deterministic zero is chosen for a remaining care-free
bit only when the final physical netlist is published.

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
- exact member operations and observable first-class memories;
- typed input/output boundary ports with identities separate from value
  revisions;
- predecessor/successor packed rows;
- architecture-independent delay, logic, and wiring estimates;
- local and contextual fingerprints.

`StructuralOwnershipProvenance` is the write authority during structural
preparation. Initial operations carry their frozen owner atom; every transform
must claim each generated operation from an exact source set with one common
atom. The final graph consumes the shared observability closure, rejects an
unowned live operation, and verifies that every surviving
frozen atom remains whole. Final partitioning may merge whole atoms but may not
split one. Ownership is not a preparation-side lookup that later partitioning
may discard: it survives operation replacement, final partition, private-IR
construction, plan binding, and provenance. It is placement and write
authority only; it is not a connectivity classifier and never decides whether
a bit is external, live, or publishable.
Published objects use exactly three owner classes: global substrate, one
region, or one directed boundary edge.

The same freeze first resolves full-domain connectivity per bit, through exact
signal connects and width-only projections, without consulting placement. It
then records every live crossing as an immutable
`(producer value, producer bit, producer region, consumer region/root)` row.
An operation reached by that table without a placement owner is an invariant
failure; `None` is reserved for a real physical boundary or constant, never an
unknown-owner fallback. Aggregate typed ports remain boundary contract
identities, while bit-flow rows are the sole source of producer publication.
Global
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
When several private cones reconstruct the same source operation bit, only
that operation's frozen owner may publish it. A proven constant operation that
has no live regional owner remains a substrate constant instead of entering
regional implementation planning.

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
explicitly. Register-bank feasibility is checked per word: conservative
address bounds may assign disjoint words to different write clocks, while any
word reachable from multiple clock domains requires an exact memory macro and
is never implemented by combining clocks. Exact multi-write-port macro binding
matches every address, data, enable, mask, edge, and read-timing contract.
Write clocks may be distinct, or same-clock logical ports may bind separate
physical ports when their effective enables are conservatively proven mutually
exclusive. Other same-clock ports are rejected unless the target format can
represent their priority and collision semantics.
Widened addresses are narrowed to a macro's physical address width only after
unsigned range analysis proves the discarded high bits are zero. Timing arcs
declared on Liberty buses are expanded to their scalar pins before regional
timing analysis.

First-class memory inference consumes only contiguous leading fixed unpacked
dimensions. Fully assigned automatic unpacked arrays remain process-local
temporary signals. Stateful layouts whose unpacked dimensions are separated
by aggregate fields lower to flattened register state, with unpacked-struct
fields assigned deterministic declaration-order bitstream offsets. Nested
member and element selections then use the ordinary canonical signal
extract/insert path, including composed runtime selectors; no synthetic
rectangular memory shape or alternate aggregate IR is introduced.
An unpacked array written by a multi-event process also remains flattened
register state. This preserves statically bounded whole-array asynchronous
reset loops together with dynamic clocked element writes: ordinary register
reset semantics apply to every flattened lane, while dynamic selection uses
the same extract/insert path. It does not pretend that a resettable register
bank is a reset-free first-class RAM macro.

A procedure sensitive to both edges of one scalar clock is not classified as
an asynchronous-reset template. Its state lowers to positive- and
negative-edge phase banks whose data input is the complete next-state
function, including synchronous resets and explicit old-state feedback for a
conditional hold. The current clock level selects the bank written by the most
recent edge. Each bank has a named internal state boundary, so both operations
remain owned by the ordinary sequential shell through regional mapping and
publication. This construction neither ORs edges into a synthetic clock nor
assumes that a disabled bank already contains the other phase's current value.

### 4. Optimize And Map Private Regions

Every worker imports only its owned dependency cone and explicit boundary
values into a private `WordModule`. Owned registers and latches remain in the
unique sequential shell: `Q` is imported as a typed boundary input, and `D`
plus controls are private observable roots. No placeholder or backpatch is
used. Packed signal reconstruction follows the validated bit-dependency graph
when a coarse whole-value edge appears recursive. A source operation rebuilt
on that path is shared once, and variable extracts use one memoized barrel DAG;
known selector bits remove unreachable branches before they can import false
feedback. Work is bounded by value width times selector width rather than
result width times every legal offset. The worker then performs, in that
module:

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

Boundary slices of the same physical source signal share one region-local
backing port. Internally driven signals are reconstructed from their canonical
bit producers; an aggregate port marker cannot turn them into imported data.
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
Before private optimization, each bit-flow producer and observable root
receives a full-domain publication obligation from the source Word graph. If
that graph does not
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
their bindings cross lowering together. Exact frozen bit-flow rows select the
producer bits that those plans must publish; neither owner lookup nor alias
membership may invent or erase that set. No epoch repartitions the shell or
attempts to rediscover private logic from its endpoints. Plan inputs may
resolve to frozen substrate nets; plan outputs are explicit bit-flow write
obligations. Input membership never implies that the substrate owns a physical
producer. If an output obligation and an input resolve to the same mapped bit,
the output keeps its producer claim; the sealed artifact then proves from the
exact Liberty pin functions that the combined bit graph remains acyclic.
Deleting a producer or moving it to an unobservable local net is not a valid
way to repair an alias overlap.
Every per-bit regional write obligation carries its exact `RegionRowId`.
Coordinator aggregation is a set operation over `(source value, bit, region)`:
duplicate claims from the same region are idempotent, while claims from
different regions fail independently of discovery order. Regional claims only
validate the full-domain contract; they can never replace its constant or
connectivity proof.
Final mapped validation applies the same distinction physically: every net
consumed by a cell input or observable output has exactly one driver, except an
explicit resolved net, which must have at least one potential driver.
Memory lowering applies the same single-owner rule before regional binding:
every generated register or latch result receives one `MemoryStateBit`
identity, while only non-state operations enter the independently ordinalled
`MemoryLogicBit` sequence. A state value never competes with a logic binding.

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

Every plan uses artifact-local cell and net identities. A sealed artifact has
one compact net table: each cell connection refers only to an `ArtifactNetId`,
and the table records exactly once whether that bit binds an existing mapped
net or a transaction-local net. External output claims live in the same table.
Before a transaction, sealing requires one producer for every local bit,
requires every external output pin to match one claim, and rejects physical
combinational cycles at bit granularity by following the concrete pins named in
each Liberty output function. The coordinator builds
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

Mapped resynthesis has one candidate catalog, one effort gate, and one bounded
pass over the committed topology. It does not classify the search as area or
timing work. Equivalent library implementations use one stable representative;
the sizing pass that follows owns drive-strength selection. Candidate generation
admits a bounded set of structural alternatives, including region-owned cells,
and the shared transaction objective decides whether design rules, timing,
power, and area justify each edit.

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
Before synthesis, `report_qor` runs timing only for a structurally pre-mapped
source with no remaining RTL operations, whose instance types resolve uniquely
to selected library cells, and whose named connections resolve to cell pins.
Ordinary RTL hierarchy remains an area-only report until synthesis publishes a
timing-compatible artifact.

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
| One closure-ranked mapped-resynthesis regime without area/timing search modes | Implemented |
| Constant-register removal proved through a bounded influence cone | Implemented; one batched transaction per round |
| Weighted outer/inner worker allocation | Implemented |
| Direct transactional region artifact commit | Implemented |
| Single-atom mapped ownership and edge-owned boundary repair | Implemented |
| Sparse boundary measurement and bounded feedback | Implemented |
| Selected sequential clock-to-Q/setup projection plus exact mapped checkpoint timing | Implemented |
| One shared sparse MMMC owner service | Implemented |
| Transactional mapped optimization and exact STA | Implemented |
| Structured source diagnostics and successful frontend warnings | Implemented across CLI/session, HDL, Liberty, formats, timing, power, and synthesis domains |
| Legacy external port expressions and SystemVerilog `let` declarations | Implemented for statically shaped named input/output projections and hygienically elaborated non-recursive value lets; overlapping projections and non-exact inout/ref mappings are structured errors |
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

## Speculative Construction Rewinds

Feedback-enable recovery has to build an expression before it can prove whether
to keep it: the enable and data it extracts are only checkable once they exist
as Word values. It used to build them in the production module and then walk
away on a failed proof, leaving the reads, the expressions, the reconstructed
mux, and their ownership entries resident. They were unreachable, so
materialization dropped them, but every Word pass, ownership traversal, and
partition build between then and there still walked them.

The construction now takes a module checkpoint first and rewinds to it on every
declining path. A rewind rejects any change to another arena and checks every
retained annotation, directive, memory port, value, operation,
connection, and instance for references into the suffix, then discards the
suffix and rewinds interned names. Every rejection happens before mutation, so
the public rollback operation is atomic without requiring the intermediate
module to satisfy unrelated publication invariants.

## The Initial-State Contract

Constant-register removal proves a register constant by induction over that one
register: the base case is that it holds its reset value, and the step is that
its next state is that same value. Only registers with an asynchronous clear or
preset are considered and only the reset value is ever folded, so the base case
is exactly one assumption, stated here rather than derived: every such register
is reset before the design is observed.

The cone the proof folds carries a second obligation. It rewrites a net into a
Boolean function of its inputs, so exactly one non-boundary cell must drive that
net unconditionally. A second output, an explicit constant driver, an external
design boundary, an `Inout` pin, a three-state output, or an unresolved driver
makes the net an unknown leaf instead.

Nothing in a mapped netlist can establish the initial-state assumption, so the
pass enforces what it can. A register whose own reset the netlist holds inactive
is declined because the assumption cannot reach it. The qualification harness applies
the assumption rather than relying on it: an asynchronous reset is a falling
edge, so a reset that is merely low at time zero never fires one and every
asynchronously reset flop keeps its simulator initial value. The harness now
drives reset high, low, and back, and releases on a falling clock edge.

The case that makes the contract visible is a register whose next state is its
own output. It satisfies the induction, and in hardware it holds its power-up
value forever; folding it to the reset value is correct exactly when the
contract holds. A test pins that behaviour so the assumption appears in the
suite instead of being implied by the proof.

## One Definition of a Propagation Dependency

The propagation plan orders a net's arrival after every incoming arc's source
and after the enable of every latch-data arc. Region editing decides whether the
retained plan still describes the graph, and it now asks the same question from
the same definition. Deciding it from `from`/`to` adjacency instead was wrong in
one specific way: a latch enable reaches the graph inside the data arc rather
than as an edge, so replacing a latch with the same data and output nets but a
different enable changed the plan's dependency set without changing adjacency at
all. The plan was retained, and a later edit confined to the new enable's cone
never rescheduled the latch output.

A related limit is worth recording: the topological order itself is computed
from arcs only, so it does not order a latch enable before the latch output.
The plan then declares a dependency the order does not provide, and building the
model fails with a dependency-ordering error. A design whose latch enable cone
is longer than its data cone reaches that today.

## Functional Reduction Merges As It Proves

A refinement round proves a batch of candidate equivalences, learns the
counterexamples the refuted ones produced, and simulates again. The proved
equivalences are now merged into the subject before the next round, which is
what makes the next round cheap: two cones that differ only by an equivalence an
earlier round proved become one node under structural hashing, so the solver is
never asked about them. Proving every round against the original subject made
each round re-derive every earlier proof. Signatures are carried onto the merged
node space rather than re-simulated, because a merged node computes the function
of every node that mapped onto it.

A round's proof budget is partitioned across shards exactly, quotient and
remainder, so the quotas sum to the budget. Handing every shard the rounded-up
share overshot by up to one proof per shard, and once the remaining budget fell
below the shard count it gave every shard a quota of one: a round with a single
proof left could still launch one per shard. The caller's subtraction is exact
for the same reason, where it used to clamp and absorb the overshoot.

One limit is worth stating plainly. The sweep bounds how many proofs it
attempts, per round and in total, but nothing bounds how hard a single proof may
be. On Ibex SKY130 one miter takes about a second, roughly half the pass, and it
is not distinguishable by cone depth or cone size from instances that finish in
microseconds. A deterministic per-proof budget, in conflicts rather than in wall
clock, is what would bound it; the pinned solver exposes neither a conflict
limit nor an interrupt, and a wall-clock bound would make the netlist depend on
machine speed.

## Mapped Resynthesis Rounds

Mapped resynthesis derives candidates in stable cell order from one committed
generation. Accepted edits refresh the driver index, and another round runs
only while at least one edit committed and the shared QoR budget remains. The
search does not choose an area or timing regime: every proposal crosses the same
transaction gate, and compatible sizing follows on the resulting topology.

## The Support Index Is Its Own Key Index

Rewriting looks up which nodes have a given cut as their support. The entries
are sorted by that key, so equal keys are already contiguous and a range lookup
is a binary search over them. Carrying a second map from key to range meant
hashing a whole cut once per distinct key on every rewrite pass, which cost more
than every other part of building the index put together. The negative filter
stays: it answers "no such key" without touching the entries at all.

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

Longer-horizon product sequencing is recorded in the
[industrial synthesis product roadmap](roadmap.md). That roadmap is
non-normative and must not be cited as evidence that a capability is
implemented; this document's conformance matrix remains the current-state
authority.

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
