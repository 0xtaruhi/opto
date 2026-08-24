<!-- SPDX-FileCopyrightText: 2026 Zhengyi Zhang -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# RFC process

Use an RFC for changes that alter architectural invariants, public Tcl
semantics, persistent formats, pass ordering, or synthesis objectives. Copy the
template, assign the next four-digit number, and submit it with the motivating
implementation or as an earlier focused pull request.

An accepted RFC records the decision and tradeoffs; it is not a compatibility
shell. If implementation evidence invalidates the design, update or supersede
the RFC explicitly.

`Status` records the design decision. `Implementation` records what the current
tree actually provides and names any accepted work that is still pending;
“accepted” alone never means “fully implemented.”

- [RFC template](0000-template.md)
- [RFC 0001: Artifact identity and dependency-ready execution](0001-artifact-execution.md)
- [RFC 0003: Procedural CFG with ordered effects](0003-procedural-ir.md)
- [RFC 0005: Path exceptions and constraint arbitration](0005-path-exceptions.md)
- [RFC 0006: Region-parallel synthesis and deterministic mapping](0006-region-parallel-synthesis.md)
- [RFC 0007: Timing-driven partitioning and region-private optimization](0007-timing-driven-partitioning.md)
- [RFC 0009: Operator-local timing and region-local architecture selection](0009-operator-local-timing.md)
- [RFC 0011: Compile-once global choice synthesis](0011-compile-once-global-choice-synthesis.md)
- [RFC 0012: Versioned synthesizable SystemVerilog profile](0012-synthesizable-systemverilog-profile.md)
- [RFC 0013 (withdrawn): Ownerless structural epochs and hierarchical compilation shards](0013-ownerless-structural-epochs.md)

RFC 0004 was removed after its hierarchy-derived regional model was replaced
by RFC 0006. Still-valid canonical-root and provenance-overlay rules now live
in the main architecture contract; removed RFC text is not an implementation
or compatibility surface.

RFC 0007 supplies the implemented replacement front half for RFC 0006. RFC
0006 remains the record of the unaffected post-freeze ownership, publication,
feedback, and post-map contracts; its former pre-freeze global optimization
and canonical-lowering text is historical only.

RFC 0013 was withdrawn after PR 109's implementation evidence showed that its
ownerless WorkGraph cutover increased complexity and broadly regressed QoR,
runtime, and memory. It remains indexed only as a historical design record and
does not supersede RFC 0007 or amend RFC 0011.
