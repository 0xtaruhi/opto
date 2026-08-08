<!-- SPDX-FileCopyrightText: 2026 Zhengyi Zhang -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# RFC 0001: Artifact Identity And Dependency-Ready Execution

- Status: accepted
- Implementation: complete for root artifact identity and dependency execution; RFC 0006 implements regional publication, and RFC 0007 implements the replacement front half
- Author: Opto project
- Date: 2026-07-22
- Revised: 2026-07-29

## Summary

Persistent synthesis artifacts are identified by complete semantic content.
Chronological revisions reject stale publication but are not cache identity.
Parallel algorithms execute exact dependency plans over immutable inputs and
return outputs under stable typed keys. Physical execution order is never an
optimization input or publication order.

This RFC defines the generic identity and execution substrate. It does not
define synthesis-region boundaries, regional candidate search, timing
contracts, or mapped commits. RFC 0006 owns the implemented regional
publication contract; [RFC 0007](0007-timing-driven-partitioning.md) owns the
replacement partitioning, identity, and region-private front half.

## Decision

### Artifact Identity

`SynthesisKey` covers every input read by root synthesis:

- complete linked semantic RTL;
- resolved definition providers and interfaces;
- target, timing, and power library content relevant to synthesis;
- active scenarios, projected constraints, case analysis, and parasitics;
- synthesis configuration, effort policy, and algorithm/cache ABIs.

`ArtifactBinding` stores the semantic key separately from the publication
revision. A revision change with identical semantic inputs does not create a
different cache identity. Conversely, a semantic input change cannot reuse an
artifact merely because an object revision was not advanced.

Domain identities remain distinct. Source, region, library, constraint,
scenario, parasitic, mapped-topology, timing-generation, power-generation, and
rendered-report fingerprints use separate typed wrappers and domain-separated
hashes. There is no universal untyped cache key.

Definition and occurrence identity contribute linked content and provenance.
They do not create child mapped artifacts. One successful synthesis publishes one
canonical root implementation.

### Dependency Plans

An executable plan owns:

- dense typed task IDs;
- exact predecessor and successor CSR;
- task-to-output ownership;
- stable task keys;
- cancellation and failure state;
- output slots in contractual key order.

A task becomes ready when all declared predecessors complete successfully.
Independent ready tasks may start and finish in any physical order. The
coordinator drains concurrent failures safely and reports the stable
lowest-key error. Partial results from a failed unpublished artifact are
discarded.

Dependency rows use the same law. A worker receives an immutable input snapshot
and owns one typed output publication. Only the coordinator obtains mutable
access to the corresponding destination row, publishes it, activates changed
direct successors, and marks the task complete. Cross-row effects are returned
as typed records and reduced later in stable order.

### Deterministic Publication

Workers do not allocate final UIDs, object names, root revisions, artifact
bindings, or shared arena ranges. Algorithms that require final contiguous IDs
return local IDs and exact sizes; the coordinator computes stable prefixes and
relocates them deterministically.

Worker count, ready-set order, completion order, task stealing, and chunking
must not change:

- mapped connectivity or object IDs;
- names or provenance;
- optimization decisions or QoR;
- diagnostics or selected errors;
- serialized checkpoint content.

Wall-clock telemetry and RSS are measurements only. They never influence
search, pruning, task eligibility, or convergence.

## Regional Subdivision

RFC 0006 adds content-addressed `SynthesisRegion`s beneath the root artifact.
Regional keys subdivide computation and cache payloads but do not change the
root publication contract:

- a region is not a child design artifact;
- a region revision is not a Session revision;
- a region cache hit cannot publish independently;
- clean and rebuilt regions are reduced into one unpublished root;
- the root `SynthesisKey` remains the exact whole-artifact fast path.

Regional reuse records are owned by the root artifact's
`IncrementalSnapshot`. A new synthesis explicitly borrows one prior snapshot;
neither `SynthesisEngine` nor checkpoint installation owns a mutable regional
cache generation.

Generic execution supports region DAGs, timing rows, power rows, and bounded
proposal tasks without assigning semantic meaning to their task boundaries.
Each domain defines its own typed item and output contracts.

## Motivation

The superseded execution model coupled source hierarchy ordering to predicted
child publication revisions. That confused presentation hierarchy with real
data dependencies and prevented cross-boundary optimization and authoritative
global timing.

The accepted model separates three concerns:

1. semantic identity decides whether work is reusable;
2. exact dependencies decide when work may execute;
3. deterministic reduction decides how owned results become one publication.

This permits immediate scheduling of ready work without allowing thread timing
to alter the result.

## Rejected Alternatives

- Revision-only cache keys are rejected because they both over-invalidate and
  permit reuse when an untracked semantic input changes.
- Per-definition mapped artifacts are rejected because hierarchy is not a
  sufficient timing or optimization boundary.
- Global level barriers are rejected as the general scheduler because an
  unrelated slow branch delays ready work.
- Shared mutable worker publication is rejected because locks do not define a
  stable semantic conflict order.
- Global atomic ID allocation is rejected because completion order would leak
  into canonical IDs.
- Scheduler decisions based on predicted heap usage are rejected. Algorithms
  own bounded representations and explicit cache policy instead.
- Production shadow compilation is rejected. Cold differential compilation is
  a qualification oracle, not a fallback path.

## Validation

Required tests cover:

- identical semantic content across different revisions;
- invalidation by source, provider, library, scenario, constraint, parasitic,
  configuration, effort, and ABI changes;
- stale command input and stale source publication rejection;
- diamond and disconnected dependency graphs;
- immediate successor release after physical completion;
- stable failure selection under concurrent errors;
- cancellation and full drain;
- stable output order across worker counts and injected completion orders;
- local-ID prefix relocation;
- checkpoint root-artifact reuse;
- byte-equivalent warm and cold publication.

No test may infer a scheduler guarantee from wall-clock timing.
