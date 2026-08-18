<!-- SPDX-FileCopyrightText: 2026 Zhengyi Zhang -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# Contributor Guide for Coding Agents

This file applies to the entire repository. It is written for automated coding
agents and community contributors who prepare changes for review. Follow it
together with `CONTRIBUTING.md`, `CODE_OF_CONDUCT.md`, `SECURITY.md`, and the
normative contracts in `docs/architecture.md`. When instructions disagree,
the more specific repository document or maintainer direction takes priority.

## Communication and repository language

- Use English for first-party production code, comments, documentation,
  diagnostics, commit messages, and pull-request text.
- Prefix every commit subject and pull-request title with exactly one category
  defined in `CONTRIBUTING.md`, such as `[synth]`, `[db]`, `[docs]`, or `[misc]`.
- Chinese text is acceptable in tests and fixtures when it directly exercises
  Unicode decoding, source locations, terminal width, rendering, or another
  language-sensitive behavior. Keep such samples minimal and explain their
  purpose through an English test name or comment.
- Do not rewrite vendored tests or documentation merely to change their human
  language. Treat upstream text as part of the vendored source unless a focused
  local patch is technically necessary.
- Keep public explanations factual and reproducible. Distinguish implemented
  behavior from proposals, targets, and known limitations.

## Start with the public contract

- Read `README.md` and `CONTRIBUTING.md` before making a broad change.
- Treat `docs/architecture.md` as the normative architecture and current-state
  conformance record. RFCs explain decisions and proposals, but an RFC alone
  does not prove that a feature is implemented.
- Discuss substantial changes to architecture, Tcl compatibility, synthesis
  policy, persistence formats, or benchmark policy in an issue before writing
  a large patch. Focused bug fixes may go directly to a pull request.
- Keep each change coherent. Avoid unrelated cleanup unless it is required to
  make the requested change correct.

## Toolchain and build environment

- Use the latest stable Rust toolchain selected by `rust-toolchain.toml`. Opto
  does not define a fixed minimum supported Rust version. Do not downgrade code
  or dependencies to support an older compiler.
- Clone and update all pinned submodules:

  ```sh
  git submodule update --init --recursive
  ```

- The native frontend requires CMake 3.20 or newer, a C++20 compiler, and Git.
  Unix builds require `make`; Windows builds require MSVC and `nmake`.
- Repository policy scripts require Python 3.11 or newer.
- Prefer locked dependency resolution for reproducible validation:

  ```sh
  cargo build --locked
  ```

## Product boundaries

- Preserve one user-facing executable: `opto`.
- Do not introduce a project manager, a manifest-driven build model, alternate
  production pipelines, or additional user-entry binaries.
- Use `clap` for new or changed command-line arguments. Define one canonical
  spelling per option; do not add a pre-`clap` normalization or alias layer.
- Unsupported functionality must return a clear error. Do not silently ignore
  commands, flags, constraints, or malformed data.
- Do not add fallback implementations, legacy compatibility branches,
  deprecated aliases, migration shells, or speculative code kept for possible
  future use. Remove obsolete code, tests, commands, and documentation.
- Breaking an internal API is acceptable when it produces a cleaner long-term
  architecture. Public behavior and persistent formats still require an
  explicit design decision, tests, and documentation.

## Tcl and synthesis behavior

- Treat Opto's documented command catalog, argument grammar, object model, and
  report schemas as the public contract. Do not describe another product as
  the specification or compatibility target.
- Verify command names, option behavior, collection semantics, and report
  fields against Opto's tests, architecture documents, public standards, and
  reproducible examples before changing them. Never expose commands that Opto
  does not implement.
- Record intentional public-interface changes and their rationale in
  `docs/architecture.md`. Prefer a small, coherent Opto interface over aliases
  added solely to resemble another tool.
- Preserve the single synthesis path. Effort settings may control bounded
  search policy, but must not select a separate implementation architecture.

## Architecture and implementation quality

- Respect crate ownership and dependency boundaries enforced by
  `tools/check_architecture.py`. Domain algorithms must not depend on session or
  presentation state.
- Design IR, databases, and algorithms for large inputs. Prefer typed compact
  IDs, contiguous arenas or structure-of-arrays layouts, string interning,
  sparse overlays, and batched traversal.
- Avoid object-per-allocation designs, repeated owned strings, full-design
  cloning, hidden global locks, and unbounded recursive or combinatorial work.
- Analysis may use read-only parallelism. Mutation, publication, diagnostics,
  checkpoints, netlists, and reports must remain deterministic across worker
  counts.
- Bound work with deterministic structural limits. Do not make functional
  behavior depend on allocator telemetry, current RSS, thread timing, hash-map
  iteration order, or host-specific scheduling.
- Keep persisted data portable and explicitly validated before publication.
  Format changes must update their schema or ABI marker and must not retain an
  old decoder unless maintainers explicitly approve a compatibility contract.
- Use structured errors with enough context to diagnose the input and failing
  stage. Do not convert a recoverable user error into a panic.
- Document non-obvious contracts wherever they live, including restricted
  cross-module interfaces and private algorithms. Record ownership, identity,
  phase, ordering, determinism, transaction, invalidation, unit, and bounded-
  work assumptions at the narrowest site that owns them. Do not add comments
  that merely restate a name, signature, or following statement.
- The workspace denies warnings, missing documentation, unsafe Rust, and
  undocumented unsafe blocks. Do not weaken repository lint policy to make a
  patch pass.

## Tests and evidence

- Every bug fix needs the smallest stable regression test that would have
  caught the bug.
- Changes to parsing, elaboration, optimization, mapping, timing, or reports
  need tests at the narrowest responsible layer. Add integration or
  differential coverage when behavior crosses crate boundaries.
- Synthesis transformations need equivalence evidence. Do not accept a QoR
  improvement that changes function or relies only on one handcrafted test.
- Preserve deterministic assertions where output order, IDs, serialization,
  diagnostics, or worker-count independence are part of the contract.
- Do not replace meaningful tests with snapshots that merely accept new
  output. Review and explain intentional baseline changes.

## Performance and QoR work

- Performance and QoR claims require reproducible benchmarks. Record the
  command, toolchain/profile, input revision or checksum, thread count, and
  relevant host information.
- Synthesis QoR changes should report area, timing, cell composition, and
  failure diagnostics on representative cases. Follow
  `benchmarks/qor/README.md` and `qualification/README.md`.
- Use `--release` for published runtime or QoR results. A faster development
  profile is acceptable for iteration when it is identified as such.
- Do not tune production heuristics solely to repository fixtures or recognize
  benchmark names and structures as special cases.

## Dependencies and third-party code

- Add a dependency only when its maintenance status, license, source, feature
  set, and impact on the dependency graph are acceptable under `deny.toml`.
- Disable unnecessary default features and keep workspace dependency versions
  centralized in the root `Cargo.toml`.
- Update `Cargo.lock` and `fuzz/Cargo.lock` when their graphs are affected.
- Group related routine dependency updates into one branch and pull request.
  Do not create one branch or pull request per crate unless separate review is
  necessary for a concrete compatibility or security reason.
- Keep GitHub Actions pinned to full commit hashes.
- Treat `third_party/` as vendored upstream code. Make only necessary,
  auditable changes there, preserve upstream license notices, and document any
  substantial local patch in `third_party/README.md`.

## Public repository and licensing rules

- Never commit proprietary PDKs, non-redistributable binaries or scripts, license
  configuration, credentials, private RTL, internal regressions, private QoR
  baselines, or non-redistributable results.
- Public benchmark inputs must be redistributable or fetched from a pinned
  revision and verified by checksum.
- Do not commit generated build directories, local editor state, temporary
  diagnostics, core dumps, or machine-specific configuration.
- Preserve the SPDX header style already used by the file type. New
  first-party files must use `GPL-3.0-only` unless maintainers approve another
  license for a clearly separated component.
- Do not merge an external contribution until the CLA Assistant check records
  acceptance of `CONTRIBUTOR-LICENSE-AGREEMENT.md`. Do not bypass a failed or
  unavailable check. Contributions whose rights may be owned by an employer
  or another entity require separately reviewed written authorization; an
  automated individual signature is not sufficient corporate authority.
- Direct requests for proprietary redistribution, closed-source embedding, or
  contractual commercial terms to `COMMERCIAL-LICENSING.md`. Do not describe
  GPL-compliant commercial use as requiring a separate commercial license.
- Report suspected vulnerabilities privately as described in `SECURITY.md`.

## Required validation

Run the checks relevant to the changed platform and, before handing off a Rust
change, run the complete Linux quality gate when available:

```sh
python3 tools/check_license_headers.py
python3 tools/check_public_repository.py
python3 tools/check_architecture.py
python3 tools/check_test_policy.py
python3 tools/check_rust_documentation.py
python3 tools/check_real_benchmarks.py benchmarks/real/medium.toml
python3 tools/check_real_benchmarks.py benchmarks/real/gate.toml
cargo fmt --all -- --check
cargo fmt --manifest-path fuzz/Cargo.toml --all --check
cargo check --workspace --all-targets --all-features --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-targets --all-features --locked
cargo bench --workspace --benches --no-run --locked
```

Also run focused tests while iterating. If a required check cannot run because
of a missing external tool or platform, state exactly which check was skipped
and why. Do not describe a change as complete when a known failure remains.

## Pull-request handoff

- Explain the user-visible behavior, architectural impact, and compatibility
  implications before listing implementation details.
- List the exact validation commands run and disclose ignored or unavailable
  external suites.
- Call out persistent-format, dependency, benchmark-baseline, vendored-source,
  and documentation changes explicitly.
- Keep reviewable commits free of unrelated generated churn and temporary
  debugging code. Maintainers decide the final merge and release-history shape.
