<!-- SPDX-FileCopyrightText: 2026 Zhengyi Zhang -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# Verification corpus

Opto uses a test pyramid instead of treating a few hand-written circuits as a
qualification suite. A change is trustworthy only when the relevant layers
all pass.

| Tier | Corpus | Current scale | Gate |
| --- | --- | ---: | --- |
| Unit | Rust tests beside each domain | domain-local cases and invariants | every push |
| Static integration | focused RTL, Tcl shell behavior, SDC, hierarchy, memory and sequential cases | 20 cases | every push |
| Generated semantics | operator × width × signedness × context | 238 CEC-proved points | every push |
| Generated differential | deterministic RTL generator | 512 fixed-seed designs as a low-cost regression sentinel | every push |
| Mapping-fixture determinism | presubmit mapping cases without Yosys | 5 zero-tolerance local-mechanism baselines | every push, every platform |
| Language conformance | CHIPS Alliance `sv-tests` | 1,027 audited HDL files; 137/137 reviewed positive ASIC cases | nightly |
| Synthesis qualification | Yosys RTL tests | 549 files audited; 210 designs synthesized; 126/126 selected observable designs proved | nightly |
| Real designs | pinned Ibex and CVA6 configurations | complete checked manifests | weekly |
| QoR | representative kernels and blocks | area, timing, cell mix, runtime, memory and CEC | weekly |

The 20 static cases are seeds, not a claim of broad coverage. Every fixed bug
must add the smallest stable regression case that would have caught it. Broad
semantic spaces belong in a generator; real integration behavior belongs in a
pinned upstream design.

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
  integration.rs         the single Cargo integration-test entry point
  integration/           CLI and qualification modules, typed runners and reports
benchmarks/qor/           performance and quality measurements, not correctness smoke tests
```

Case and suite manifests are parsed with `serde(deny_unknown_fields)`. A typo
therefore fails loudly instead of silently dropping coverage. The runner emits
machine-readable `results.json`, a compact `summary.tsv`, per-case logs on
failure, tool hashes and equivalence status.

Sequential cases that change state representation declare
`equivalence_initial_state = "zero"` only when zero is the reviewed
state-relation boundary on both sides, normally the post-reset state. The
runner then proves a shared-input miter by temporal induction from that state;
it does not treat unrelated arbitrary register contents as equivalent.

## Local gates

The normal workspace test command runs the static corpus and the 238-point
generated matrix:

```sh
cargo test --workspace --all-targets --all-features
```

CEC requires an explicit Yosys executable:

```sh
OPTO_YOSYS=/path/to/yosys \
  cargo test -p opto --test integration qualification::presubmit_equivalence \
  -- --exact --ignored --nocapture

OPTO_YOSYS=/path/to/yosys \
  cargo test -p opto --test integration qualification::generated_semantic_equivalence \
  -- --exact --ignored --nocapture
```

Filter a static suite without editing its manifest:

```sh
OPTO_REGRESSION_CASE=sequential-dff-enable \
  cargo test -p opto --test integration qualification::presubmit_corpus -- --exact --nocapture
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

The nightly `sv-tests` baseline hashes the exact required case IDs. The Yosys
baseline independently hashes synthesized designs, observable roots, mandatory
proof targets, and deferred proof targets. Its 46 deferrals are exact
path/design pairs with reviewed reasons in `upstream/yosys/proof.toml`; a new
or renamed deferral fails instead of silently reducing proof coverage. Neither
baseline can be satisfied by losing one old case while gaining an unrelated
new one. Updating a hash requires reviewing the mismatch report and the pinned
upstream license/inventory again.
