<!-- SPDX-FileCopyrightText: 2026 Zhengyi Zhang -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# Rust API and documentation policy

Opto is one product assembled from narrowly scoped crates. The generated API
reference is published with this guide at
[API reference](api/opto/index.html). Start with the product shell only when
embedding Tcl; synthesis and analysis integrations should enter through the
domain crate that owns the relevant state.

| Crate | Responsibility | Primary entry point |
| --- | --- | --- |
| `opto` | Tcl product shell and presentation | `Shell` |
| `opto-session` | Transactional product state and shell use cases | `Session` |
| `opto-hdl` | SystemVerilog analysis and elaboration | `Frontend` |
| `opto-synth` | RTL-to-mapped synthesis pipeline | `SynthesisEngine` |
| `opto-timing` | Full and incremental static timing analysis | `TimingEngine` |
| `opto-power` | Activity-aware power analysis | `PowerEngine` |
| `opto-formal` | Test and qualification equivalence | proof functions |
| `opto-library` | Liberty import and immutable library revisions | `LibraryStore` |
| `opto-ir` | Procedural, word, logic, RTL, and mapped IRs | phase modules |
| `opto-db` | Persistent object identity, hierarchy, and collections | `ObjectRegistry` |
| `opto-runtime` | Deterministic parallel execution and commit | `ExecutionContext` |
| `opto-formats` | Structural formats and deterministic reports | format functions |
| `opto-core` | Compact IDs, revisions, rows, and interned names | storage primitives |
| `opto-slang-sys` | Safe ownership boundary around the C++ frontend | `SlangCompilation` |
| `opto-tcl-sys` | Embedded Tcl interpreter and its raw FFI | `Interpreter` |

Regional synthesis types are internal implementation contracts rather than a
supported public integration surface. `SynthesisEngine` Rustdoc must describe
the production tree truthfully and distinguish implemented RFC 0007 identities
from future proposals. Internal identities such as `OperationAnchorId` are not
therefore promoted into a supported public integration API.

## What belongs in Rustdoc

Rustdoc describes contracts a caller cannot safely infer from a type signature.
The relevant caller is not limited to a downstream user: a `pub(crate)`,
`pub(super)`, or `pub(in ...)` item is an interface between implementation
modules and needs documentation when correct use depends on non-obvious
knowledge. Visibility controls where an item can be called; it does not decide
whether its contract is important.

Document public and internal interfaces that establish any of the following:

- ownership and lifetime boundaries;
- the arena or revision in which an ID is valid;
- whether a result is sealed, transactional, or speculative;
- determinism and ordering guarantees;
- required preconditions and the meaning of error cases;
- units for timing, power, area, and memory values;
- invalidation, rollback, publication, or partial-update behavior;
- whole-design scans, non-obvious complexity bounds, or bounded search policy;
- examples when several APIs must be called in a particular order.

Core domain types and phase entry points should explain their role, invariants,
and relationship to adjacent types. Rustdoc links should point to those
adjacent types instead of repeating their definitions. Put a shared contract on
the owning module or type when that is clearer than duplicating it on every
helper.

Simple accessors, direct constructors, representation conversions, and narrow
test helpers do not need internal Rustdoc merely because they use restricted
visibility. If such an item is externally reachable, `missing_docs` still
requires a concise description. A comment that only restates the item name or
signature is not acceptable documentation.

Crate roots and public modules use `//!` when callers need an overview that is
not evident from the exported items. Private implementation and test files do
not need a module summary merely to restate their path or contents. They do
need one when several items share a phase invariant, ordering rule, identity
domain, or mutation protocol that would otherwise be duplicated or left
implicit.

## What belongs in implementation comments

Implementation comments explain *why* code is shaped a particular way. Their
need is independent of visibility: a private loop may require a stronger
explanation than a public getter. Good comments record an algorithmic
invariant, a compatibility constraint, a non-obvious complexity bound, or the
reason an apparently simpler alternative is unsound. Comments that merely
translate the next statement into English should be removed.

Reviews of synthesis, timing, persistence, and parallel-runtime code must look
specifically for undocumented assumptions about semantic ownership, stable
versus dense identities, generation compatibility, deterministic reduction,
transaction atomicity, cache invalidation, units, and bounded work. A change
that introduces one of these assumptions must document it at the narrowest
interface or implementation site that owns the knowledge.

Unsafe code additionally requires a `SAFETY:` explanation covering the exact
preconditions established at that site. Temporary history, disabled code, and
future-work placeholders belong in issues or RFCs rather than source comments.

## Documentation gate

The documentation build treats Rustdoc warnings as errors:

```console
python3 tools/check_rust_documentation.py
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps --locked --document-private-items
```

This rejects broken or private intra-doc links, malformed code-block
attributes, and bare URLs. Building private items also exercises links and
formatting in internal algorithm documentation. The source checker rejects item
documentation accidentally split around attributes. `missing_docs` protects
the externally reachable API floor, and `unreachable_pub` prevents a private
implementation from using an accidentally broad public visibility. Neither can
recognize an undocumented semantic invariant, so internal coverage and accuracy
remain code-review responsibilities. The normal format, check, Clippy, and test
gates remain required because examples and documentation must describe code
that actually builds.
