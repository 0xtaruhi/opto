<!-- SPDX-FileCopyrightText: 2026 Zhengyi Zhang -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# Changelog

All notable changes to Opto are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- The production region-parallel synthesis engine: stable Word-region
  identity, hard typed boundaries, region-local Liberty mapping,
  bounded non-dominated plans, deterministic local-ID stitch, immutable
  contract epochs, reachable regional checkpoint reuse, and transient
  cross-boundary repair.
- Compact portable target-cover payloads whose selected cell, net,
  pin, topology, and implementation identities are validated again at stitch.
- Sparse per-scenario and per-timing-tag boundary response propagation with
  correlated early/min and late/max views, including first-class setup, hold,
  recovery, removal, pulse-width, and electrical checks.
- Reusable append-only Word and provenance revision checkpoints. Epoch
  candidates are audited against one persistent baseline, discarded by
  rollback, and only the best compact plan set is replayed for publication.
- A dedicated regional synthesis RFC and a cutover record covering ownership,
  invariants, implementation evidence, remaining qualification, and rejected
  designs.
- A centralized architecture conformance matrix that distinguishes
  implemented architecture from external industrial qualification.
- Machine-checked documentation invariants preventing obsolete synthesis
  ownership and completion claims from re-entering current architecture
  documents.
- A tiered public qualification contract for roughly hundred-thousand-,
  million- and ten-million-gate designs. It records reproducible QoR,
  end-to-end runtime, and peak-RSS gates using redistributable inputs.
- Initial public synthesis shell, typed design database, synthesis, timing,
  power, formal, report, qualification and public QoR infrastructure.
- Criterion runtime benchmarks for the public `SynthesisEngine::synthesize`
  entry point and machine-readable public QoR result schemas.

### Changed

- Public and extended QoR area, timing, cell-count, and cell-composition
  baselines now record the accepted generic priority-rebalance trade-offs from
  #3 instead of retaining pre-cutover values. The add256 peak-RSS gate now uses
  a 256 MiB absolute ceiling for the current embedded frontend and runtime.
- The statically embedded Tcl runtime and standard library were updated from
  8.6.11 to the unmodified upstream 8.6.18 source distribution.
- Checkpoints and canonical fingerprints use the validated `opto-archive`
  rkyv format with an explicit little-endian, 64-bit-pointer schema; the
  unmaintained bincode dependency and its legacy decoder have been removed.
- `docs/architecture.md` is the normative contract and its conformance matrix
  now reflects the single regional production path.
- RFC 0001 now owns only semantic root-artifact identity, exact dependency
  execution and deterministic publication. Regional synthesis identity and
  algorithms belong to RFC 0006.
- The procedural-IR RFC now separates its implemented Proc/Word contract from
  first-class regional memory selection and exact register-bank or macro
  materialization.
- Connectivity- and affinity-aware partitioning uses compact packed weighted
  adjacency and deterministic work-bounded regions instead of per-operation
  tree maps or hierarchy-derived scheduling buckets.
- Regional selection now commits one best measured `RegionCoverPlan` per
  stable region. Provider recipes remain inputs to that local search and are
  never treated as globally committed results.
- Public QoR qualification records area, timing, cell composition,
  wall/CPU/RSS resources, equivalence status and per-case diagnostics in
  machine-readable outputs. Generated artifacts remain outside the source
  tree.
- Process-local Tcl collection and singleton-member handles include a
  monotonically increasing generation, so registry replacement and checkpoint
  restore invalidate stale handles.

### Removed

- RFC 0004 and its hierarchy-derived regional execution claims. Still-valid
  canonical-root and source-provenance rules are retained by the main
  architecture contract.
- The obsolete platform-refactoring terminal-state narrative.
- The parallel GTECH backend, opaque generic operator cells, empty-library
  synthesis fallback, and their qualification models. Technology-independent
  Word/operator optimization remains the mandatory front half of Liberty mapping.
- The pre-lowering unique-recipe optimizer, full-root initial mapper, fake
  decision-only GTECH plans, and whole-module epoch candidate clones.
- Synthesis behavior switches including `OPTO_MUL_ARCH`,
  `OPTO_NO_REWRITE`, `OPTO_NO_ARITHMETIC_FUSION`,
  `OPTO_AREA_EVAL_BUDGET` and `OPTO_TIMING_EVAL_BUDGET`.
  Developer diagnostics remain explicit inputs and do not change the intended
  result.
- Predicted heap reservation and scheduler gating, decoded-heap accounting,
  retained-memory permits and checkpoint heap guards. Algorithms instead own
  structurally bounded working sets; measured peak RSS remains a qualification
  metric.

### Fixed

- FSM equivalent-state minimization resolves signal reads per bit when
  disjoint static connects drive one signal, preserving valid state merging.
- Liberty fixtures under `qualification/libraries` are no longer hidden by the
  repository ignore rules.
- Procedural normalization and current memory lowering report typed invariant
  errors instead of panicking on failed arena lookup.
- Technology-map exact recovery recomputes stale parallel viability slots
  deterministically instead of aborting a large timing-driven synthesis run.
- Incremental timing closure splices endpoint additions and removals and uses
  sparse topological propagation instead of repeated full-design scans.
- Closure endpoint evaluation runs on the shared worker pool.
- Timing-model instance resolution uses a persistent position index with
  bounded transactional rollback.
- Mapped edit validation checks newly introduced names locally; whole-netlist
  uniqueness remains a publication and checkpoint invariant.
- SPEF, selector exhaustiveness, joint binding and native FFI edge cases retain
  deterministic diagnostics and behavior.

[Unreleased]: https://github.com/0xtaruhi/opto/commits/main
