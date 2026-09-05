<!-- SPDX-FileCopyrightText: 2026 Zhengyi Zhang -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# Testing policy

Opto treats tests as executable ownership records, not as a collection whose
size is itself a quality metric. Every behavior has one primary test owner at
the lowest layer that can observe the behavior directly. A higher layer adds a
test only when it owns a distinct boundary, public contract, or independent
oracle.

This policy governs first-party Rust tests, qualification cases, generated
semantic suites, external differential suites, and QoR benchmarks. The
architectural invariants and evidence requirements in
[`architecture.md`](architecture.md) remain normative.

## Test layers and ownership

| Layer | Primary responsibility | Permitted evidence | Evidence that belongs elsewhere |
| --- | --- | --- | --- |
| Domain unit | One data structure, algorithm, or local invariant | In-memory typed fixtures, exact values, positive and negative cases, deterministic limits | Filesystem, process environment, Tcl routing, or public rendering |
| Crate boundary | One typed seam between domains | Success wiring, error propagation, transaction boundaries, serialization and generation identity | Repeating a domain's complete semantic or parameter matrix |
| Session service | Publication, lifecycle, object identity, checkpointing, and cross-domain coordination | Atomic publication, rollback, cache invalidation, persistent identity, and generation changes | Re-proving mapper, timing, or power algorithms through session state |
| Product CLI and Tcl | The public executable and command contract | Argument grammar, Tcl evaluation, exit status, diagnostics, streams, init files, and real filesystem boundaries | Large algorithmic matrices or internal implementation structure |
| Qualification and formal | End-to-end semantics with an independent or product-level oracle | Focused RTL cases, deterministic generators, CEC, pinned upstream corpora, and worker-count independence | Replacing the smallest responsible domain regression test |
| Benchmark and QoR | Representative quality and resource behavior | Area, timing, cell mix, runtime, memory, provenance, and equivalence | Tiny correctness smoke tests or unqualified performance claims |

A test in a higher layer must protect something the primary lower-layer test
cannot observe. Typical valid reasons are:

- conversion across a typed boundary;
- atomic publication or rollback;
- serialization, checkpoint ABI, or stable object identity;
- public error translation, formatting, exit status, or stream behavior;
- an independent oracle such as combinational or sequential equivalence;
- deterministic behavior across worker counts or supported platforms.

Testing the same input, code path, failure mode, and oracle at two layers is
duplication. Similar test names are not sufficient evidence of duplication:
for example, a timing-domain value check and a session test proving that SDC
state reaches the timing domain have different owners.

## Placement

Small tests that require private implementation access may live in a local
`tests` module. Larger domain suites use `src/<domain>/tests.rs` or
`src/<domain>/tests/` and begin with a short module comment naming the contract
they own.

Crates with broad algorithm suites keep a checked ownership inventory next to
their manifest. [`opto-timing`](../crates/opto-timing/test-owners.toml) assigns
all 114 tests to timing context, analysis, constraints, incremental engine,
model, parasitics, or scenario contracts. [`opto-synth`](../crates/opto-synth/test-owners.toml)
assigns the synthesis suite to the eleven synthesis domains in
[`architecture.md`](architecture.md). The policy checker rejects an unowned
test file, an overlapping owner, or a count that changes without an intentional
inventory update.

Product CLI tests and synthesis qualification are separate Cargo integration
targets:

```text
crates/opto/tests/
  cli/                    public process and command-line behavior
  qualification/          corpus runners, formal adapters, and QoR harnesses
  support/                support shared only by integration targets
```

Qualification inputs have distinct homes:

```text
qualification/cases/      focused first-party correctness cases
qualification/upstream/   pinned external inventories and adapters
benchmarks/qor/            representative quality measurements
benchmarks/real/           pinned medium and scale-oriented resource gates
```

Do not move a tiny regression into `benchmarks/` to make it appear
representative. Do not put an algorithm matrix behind the CLI merely to make
it end-to-end.

## Writing tests

Test names describe the condition and observable result. Names such as
`works`, `basic`, or `smoke` are not sufficient for a new regression. A test
should make its primary input and oracle visible without requiring readers to
reverse-engineer a large hidden fixture.

Use typed builders for repeated mechanical setup, but keep behavior-relevant
values at the call site. Avoid a universal fixture that silently creates an
entire design, library, constraint set, and expected result. Shared fixtures
must not become a second product model or bypass validation that production
inputs must pass.

Domain tests are in-memory by default. Tests that need temporary files,
environment variables, child processes, external executables, or platform
facilities belong in an integration target or a clearly owned boundary suite.
Temporary resources use scope-owned cleanup and collision-resistant paths;
manual cleanup at every return site is not an accepted pattern for new tests.

Prefer structured values and typed errors. String containment is appropriate
at the presentation or CLI boundary, where text is the contract. Snapshots are
reserved for reviewed public rendering and compact stable formats; they do not
replace meaningful domain assertions.

Loops and generators are appropriate when they exercise one invariant with one
oracle over a semantic space. Each generated point must have a stable label in
failure output. Do not copy the same width, signedness, or operator matrix into
unit, session, CLI, and qualification layers.

Never use sleeps, wall-clock timing, random hash iteration, allocator state,
or host scheduling as a functional oracle. Fixed-seed generators supplement
curated and independently authored cases; increasing seed count is not a
substitute for adding a missing semantic dimension.

## Regression and transformation evidence

Every bug fix adds the smallest stable regression at the primary responsible
layer. The pull request explains:

1. why existing coverage did not catch the bug;
2. which layer owns the corrected behavior;
3. what observation fails before the fix;
4. whether an upper-layer or independent oracle is also required.

Synthesis transformations require equivalence evidence in addition to
structural assertions. A mapped cell count or topology proves implementation
choice, not semantic correctness. Changes to deterministic identity,
serialization, reports, or worker-count behavior preserve exact assertions at
the responsible boundary.

## Ignored and scheduled tests

An ignored test names the external prerequisite in its `#[ignore = "..."]`
reason and is invoked by a checked-in CI workflow. A permanently ignored test
without a scheduled owner is dead coverage and is removed or given an active
lane.

The supported lanes are:

- pull request: repository policy, all domain tests, product CLI behavior,
  static qualification, and the non-CEC generated semantic matrix;
- nightly: pinned Yosys CEC, fixed-seed differential suites, formal
  cross-checks, frontend conformance, and synthesis qualification;
- weekly: pinned real designs and representative public QoR suites;
- release: the complete portable required gate.

Documentation must distinguish synthesis coverage from CEC-proved coverage and
must match the workflow that actually invokes each ignored test.

## Removing and consolidating tests

Test count and line coverage are not deletion criteria. A test may be removed
when one of these conditions is demonstrated:

- its production behavior or interface was deleted in the same cutover;
- a lower-layer test covers the same input, path, failure mode, and oracle;
- a deterministic matrix or generator covers the same semantic point with an
  equal or stronger oracle;
- a higher-layer test repeats internal details without owning an additional
  boundary;
- a snapshot duplicates complete structured assertions without protecting a
  distinct public format.

A consolidation change records the removed test, the surviving primary owner,
the oracle comparison, and any boundary evidence retained elsewhere. For
nontrivial algorithmic changes, targeted fault injection or mutation evidence
should show that the surviving test still detects the protected defect.

Keep tests that have a distinct oracle, platform boundary, persistent-format
contract, concurrency invariant, or historical regression dimension not
covered by the proposed replacement.

### Initial consolidation record

The policy cutover removed `smoke-add32`. Its three observable dimensions have
surviving primary owners with equal or stronger evidence:

| Removed dimension | Surviving owner | Oracle |
| --- | --- | --- |
| 32-bit unsigned addition | generated semantic matrix, `arithmetic-unsigned` width 32 | synthesis on pull requests and CEC nightly |
| Fixed-width addition truncation | `semantics-verilog-truncating-add` | end-to-end synthesis and nightly CEC |
| ANSI-style Verilog ports through the product | `semantics-resolved-nets` | end-to-end synthesis and nightly CEC |

The retained `semantics-verilog-truncating-add` also protects non-ANSI Verilog declarations, so the
two former smoke cases were not interchangeable merely because both used
addition. No other static case was removed without an equally explicit owner
and oracle comparison.

The synthesis-domain cutover moved three misplaced assertions without reducing
workspace coverage. Duplicate RTL instance rejection now belongs to
`opto-ir`; structural instance/assign rendering and sized constants now belong
to `opto-formats`. Their original `opto-synth` copies were removed, reducing
that crate's suite from 398 to 395 tests while retaining the same three
contracts at their lowest responsible layers.

The same audit removed `opto-synth`'s `reports_are_derived_from_rtl_module`:
the exact area-report schema and cell classification are already owned by the
`opto-formats` area snapshot, while active-library propagation is independently
owned by `opto-session::tests::libraries`. The vector port/net-bit assertion
was moved to `opto-formats`, its lowest responsible layer. This leaves 393
owned `opto-synth` tests and removes the obsolete root-wide fixture that only
the report duplicates required.

The qualification audit also removed `sequential-fsm-sparse`. The retained
`sequential-fsm-sparse-timing` uses the identical RTL and equivalence oracle,
and additionally proves the clock-constraint boundary through timing-aware
synthesis. The unconstrained duplicate had no independent input, path, or
oracle after that stronger case was established.

## Cost and feedback

Measure test cost by Cargo target and suite, not by the number of `#[test]`
functions. Record cold build time, warm command time, test execution time, peak
RSS, test-binary size, and incremental artifact size when changing profiles or
large harnesses. Hardware-dependent absolute times are observations, not
portable correctness gates.

Use the narrowest responsible command while iterating:

```sh
cargo test -p opto-timing --lib --locked
cargo test -p opto --test cli --locked
cargo test -p opto --test qualification presubmit_corpus --locked -- --exact
```

The pull-request gate runs every workspace library test plus the explicit
product and qualification targets. Nightly and weekly commands are listed in
[`../qualification/README.md`](../qualification/README.md) and the checked-in
workflows. `cargo test --workspace --all-targets --all-features --locked`
remains the complete local Rust gate when all pinned dependencies are present.

CI runs Linux checking/documentation/linting, tests, and release benchmark
compilation in three parallel lanes. The required `Rust quality gates (Linux)`
check succeeds only when all three lanes succeed. macOS and Windows retain their
portable Tcl and CLI checks. These jobs also run on `main` pushes so new pull
requests can restore default-branch caches; pull-request caches are otherwise
isolated from sibling branches. Cargo caches retain workspace build outputs,
and `SCCACHE_GHA_ENABLED` enables persistent compiler caching across runners.
Linux quality and Rust CodeQL jobs use Ninja for the native frontend build to
avoid Make's unlimited parallelism with an unnumbered `cmake --parallel`.

Rust CodeQL uses a persistent Cargo target directory for both workspace and fuzz
manifest extraction. A separate preparation step runs the real native build
scripts before analysis so their compilation time is visible. Extraction still
executes build scripts and procedural macros, enables the extractor's default
all-feature coverage, and scans both manifests. The Linux analysis explicitly
uses four workers and a 12 GiB CodeQL memory budget; it does not depend on the
runner-specific default allowance. Cache misses must remain correct, and a
missing or failed lane must never produce a successful required check.

The test profile retains line-level debugging while avoiding full debug-value
information in every large test binary. Any further profile change requires a
before-and-after measurement and a readable failure backtrace.

### Accepted test-profile measurement

The `debug = 1` test profile was accepted on 2026-08-12 after this focused
comparison. Both runs used `cargo test -p opto-timing --locked` with the
checked-in lockfile, Rust 1.97.1 (`8bab26f4f`, LLVM 22.1.6), x86-64 Linux
5.15, and an 80-logical-CPU container. The `target/` directory was absent
before each measured profile. Times are local observations rather than CI
limits.

| Observation | Full debug values | Line-level test profile |
| --- | ---: | ---: |
| Cold command elapsed | 78.48 s | 64.98 s |
| Warm cached command elapsed | not recorded | 0.57 s |
| Warm cached peak RSS | not recorded | 56,660 KiB |
| Peak RSS | 2,310,756 KiB | 1,470,280 KiB |
| `opto-timing` test binary | 196,224,808 bytes | 76,905,216 bytes |
| Two `opto-timing` incremental directories | 778 MiB | 455 MiB |
| Complete `target/` after the run | 2.3 GiB | 1.4 GiB |
| Test execution | 114 passed in 0.30 s | 114 passed in 0.31 s |

The profile is retained because it substantially reduces resident and disk
cost without changing coverage or removing line-level diagnostics. Repeat the
same focused comparison before changing its debug or incremental settings.
The warm observation used an unchanged artifact graph immediately after a
successful build; Cargo reported 0.16 seconds before execution, and the 114
tests themselves completed in 0.30 seconds.

## Review checklist

For every added or materially changed test, review:

- Is this the lowest responsible layer?
- What exact contract and oracle does it own?
- Does a higher or lower layer already test the same path and oracle?
- Does the fixture expose the values relevant to the behavior?
- Does it require filesystem, process, environment, platform, or external-tool
  isolation?
- Does a synthesis change also have equivalence evidence?
- Is the execution lane documented and actually invoked by CI?
- If another test is removed, is its replacement owner explicit?

Repository policy checks enforce structural parts of this contract. Review is
still responsible for semantic ownership and oracle quality.
