<!-- SPDX-FileCopyrightText: 2026 Zhengyi Zhang -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# RFC 0003: Procedural CFG with ordered effects

- Status: accepted
- Implementation: core procedural migration and finite-state re-encoding complete
- Author: Opto project
- Date: 2026-07-22

RFC 0007 changes where derived FSM analysis and rewriting execute: both require
one provisional region owner, and a machine is re-encoded only when its
complete semantics are resident in that owner. The procedural CFG and
Word-state semantics in this RFC remain authoritative; its description of
whole-module FSM execution
is current-tree history, not a compatibility requirement.

## Summary

Language frontends produce one flat procedural CFG representation. Process
normalization consumes that representation and produces a process-free Word
IR with explicit register, latch, and memory operations. Memories are
first-class state; FSM information is derived analysis rather than a second
authoritative IR.

## Implemented scope

`RtlModule` is now the source artifact across the frontend, session,
checkpoint, fingerprint, object-index, linked elaboration, and synthesis
boundaries. It atomically pairs a structural, process-free `WordModule` with one
sealed `ProcModule`. The old recursive statement representation and lowerer have
been deleted.

`ProcModule` uses dense typed procedure, block, effect, edge, switch-arm, and
sensitivity-event arenas with explicit terminators. Each block owns one
contiguous source-ordered effect range; control-flow edges carry control only.
Expressions reference Word values rather than a duplicate expression IR.
Normalization consumes the CFG, distinguishes visible blocking state from
scheduled nonblocking state, merges joins deterministically, emits structural
register/latch/connect and first-class memory-port operations, and removes
process-local signals. Cyclic runtime CFGs are rejected explicitly.

Memory resource inference is a separate phase. The current transition
implementation atomically takes all memory identities and ports and
materializes the supported contracts as technology-independent register banks;
bit lowering rejects any resource left behind. RFC 0006 replaces that
unconditional downstream decision: the procedural contract remains unchanged,
while first-class memories survive into regional resource selection where
register-bank, ROM, and macro implementations compete. FSM recognition is
derived after process removal from resettable Word
register transitions whose next values form a finite constant/hold set. A
shared canonical Boolean DAG constructs observation and complete register-update
semantics for every finite state. It conservatively prunes reset-unreachable
states and refines observation/successor signatures to a fixed point; only
identical canonical literals are merged. The area plan selects compact binary
codes; a machine on a directly constrained source clock selects a narrower
one-hot code. Area plans compare and recode the next-state value; timing plans
rewrite the extracted finite transition DAG directly. The source-facing state
remains behind a decoder.

## Motivation and compatibility evidence

The replaced Word IR represented both frontend processes and normalized
structural dataflow. Recursive statement vectors crossed the native boundary,
were walked independently by validation, hashing, object indexing, linked elaboration,
and lowering, and forced assignment state to be cloned around nested control.
That made invalid phase states representable and scaled poorly on large control
structures.

Ordinary SSA is insufficient for HDL semantics. Blocking writes change values
visible to later statements, nonblocking writes schedule next state while
later reads still observe old state, uncovered paths mean no update or hold,
and partial, dynamic, and memory writes are ordered effects with last-write
priority.

## Detailed design and invariants

Frontend AST ownership remains inside slang or another frontend and is never
stored in Opto. A transient builder emits typed procedures and source-ordered
blocks, effects, edges, switch arms, events, terminators, and source spans, then
seals them into compact arenas. Source order inside a block is the complete
effect-order contract; no synthetic value or token duplicates that order.
Loops that the frontend can statically unroll become ordinary acyclic blocks;
unsupported runtime loops or a remaining CFG cycle are diagnosed explicitly.

The normalizer distinguishes current-visible values, scheduled next values,
no-update, and ordered writes. Target layout is partitioned by the static slice
boundaries actually written, while dynamic writes use explicit functional
insert semantics. Blocking memory writes are forwarded to later reads and
emitted ports preserve source priority.

`WordModule` contains no process, statement, phi, or sensitivity
representation. At the `RtlModule` boundary it may contain process-local
structural signals referenced by CFG expressions; normalization consumes and
removes them before the standalone Word phase. Word owns structural operations,
continuous connections, instances, registers, latches, and explicit memory
read/write ports with clock, enable, mask, priority, and collision contracts.
Normalization destroys the CFG after moving the module shell and emitted Word
arenas. The FSM catalog is snapshot-local derived data rather than stored IR;
encoding plans carry typed Word IDs. Rewrites remain stage-local until
structural eligibility, validation, and complete arena compaction succeed.

## Determinism, scalability, and QoR impact

IDs and flat arena ranges follow stable source order. Procedural normalization
is deterministic and iterative over a topological block order; compact
persistent state frames share unchanged assignments across branches. A memory
remains O(ports + metadata) through normalization. In the regional production
flow it stays compact until an explicit mapped implementation is selected.

Within a private region, FSM derivation may share use, driver, known-bit, and
canonical Boolean analyses across resident candidates. Candidates are bounded
to 64 states, 128 state bits, 4096 transition-cone values, and a bounded
symbolic node arena. Region tasks analyze immutable local Word data; plans and
materialization retain source order. Exhausting the symbolic bound preserves
the machine without state pruning or merging.

## Alternatives

A generic CFG that replaces the whole Word expression graph is rejected.
Keeping recursive statements alongside CFG is rejected as two authoritative
models. Modeling memories through unpacked arrays and whole-array registers is
rejected. Treating source CFG as an FSM is rejected because CFG describes one
process activation while an FSM describes state across clock events.

## Validation and rollout

The migration was atomic at the production boundary: frontend, session source,
checkpoints, hashing, linked elaboration, indexing, and normalization changed together,
then recursive statements and the old lowerer were deleted. Current regression
coverage includes arena and effect-order invariants, joins, deep flat control,
cycle rejection, blocking/nonblocking visibility, dynamic and partial targets,
latches, memory forwarding/priority, source locations, switch order, frontend
loop diagnostics, finite transition enumeration, unreachable-state removal,
generated semantic checks for the symbolic encoder, objective-specific
encoding, generated-state naming, and end-to-end sequential CEC.
