<!-- SPDX-FileCopyrightText: 2026 Zhengyi Zhang -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# Contributing to Opto

Thank you for helping improve Opto. Discuss substantial architecture, command
compatibility, or synthesis-policy changes in an issue before investing in a
large patch. Small bug fixes may go directly to a pull request.

## Development setup

Clone with recursive submodules and use the latest stable Rust toolchain. Opto
does not maintain a fixed minimum Rust version. CMake 3.20+, a C++20 compiler,
Git, and `make` (or MSVC and `nmake` on Windows) are also required. Repository
policy scripts require Python 3.11 or newer.

```sh
git clone --recurse-submodules https://github.com/0xtaruhi/opto.git
cd opto
cargo build --locked
```

Set `OPTO_PYTHON` to an explicit Python 3.11-or-newer executable when CMake
cannot use the host's default Python. Opto's first-party native C++ bridge is
built with compiler warnings enabled and treated as errors; do not weaken that
gate to accept a change.

## Change titles

Every commit subject and pull-request title must use the form
`[prefix] Summary`, with exactly one of these lowercase prefixes:

- `[synth]`: HDL frontends, IR, synthesis, formal verification, mapping, timing,
  or QoR;
- `[db]`: design databases, persistence, session state, or runtime storage;
- `[cli]`: the executable, Tcl commands, reports, or other user interfaces;
- `[docs]`: documentation-only changes;
- `[test]`: tests, qualification, benchmarks, or fixtures without a product
  behavior change;
- `[build]`: build logic, CI, developer tooling, packaging, or releases;
- `[deps]`: dependency and pinned third-party updates;
- `[misc]`: repository maintenance that fits none of the categories above.

Choose the most specific prefix and use `[misc]` only as the fallback. Examples
include `[synth] Lower bounded procedural loops` and
`[docs] Explain memory inference limits`. Opto uses squash merges, so the
validated pull-request title also becomes the subject published on `main`.

## Required checks

Run these checks before submitting a pull request:

```sh
python3 tools/check_license_headers.py
python3 tools/check_public_repository.py
python3 tools/check_architecture.py
python3 tools/check_test_policy.py
cargo fmt --all -- --check
cargo check --workspace --all-targets --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets --all-features
```

Every fixed bug needs the smallest stable regression test that would have
caught it. Synthesis behavior changes also need equivalence coverage. QoR
changes need representative benchmark results and an intentional baseline
update; see [`benchmarks/qor/README.md`](benchmarks/qor/README.md) and
[`qualification/README.md`](qualification/README.md).

Tests have one primary owner at the lowest responsible layer. Higher-layer
tests must protect a distinct boundary, public contract, or independent oracle
instead of repeating the lower layer's matrix and internal assertions. Follow
the placement, fixture, ignored-test, consolidation, and review rules in
[`docs/testing.md`](docs/testing.md).

## Code documentation

Document contracts according to the knowledge they carry, not according to
whether an API is intended for a third party. Cross-crate APIs, restricted
interfaces between implementation modules, and private algorithms all require
documentation when correct maintenance depends on non-obvious ownership,
identity, phase, ordering, determinism, transaction, invalidation, unit, or
complexity rules. Put shared invariants on the owning module or type rather
than copying them onto every helper.

Do not add comments that only restate a name, signature, or the following line
of code. Simple accessors, direct constructors, representation conversions, and
narrow test helpers normally need no internal comment. See
[`docs/api.md`](docs/api.md) for the complete Rustdoc and implementation-comment
policy.

## Contributor licensing

Opto uses a contributor license agreement so that accepted contributions can
remain available in the public GPL project while the project owner can also
offer separately negotiated commercial licenses.

Before a contribution can be merged:

1. Read the
   [Opto Contributor License Agreement](CONTRIBUTOR-LICENSE-AGREEMENT.md).
2. Follow the CLA Assistant instructions on the pull request. First-time
   contributors must provide their full legal name and accept the current CLA
   version through the automated signing flow.
3. If an employer or another entity may own the contribution, obtain its
   written authorization. The maintainer may require a separate corporate
   agreement before merging the contribution.

The CLA Assistant check must pass before an individual contribution is
merged. Its versioned signing copy is published in the
[Opto CLA Gist](https://gist.github.com/0xtaruhi/fa297196c03f2756a7827c1a3061fcb8).
Corporate authorization is still reviewed manually; an automated individual
signature does not prove authority to bind an employer. If the signing service
is unavailable, wait for it to recover instead of bypassing the check.

You retain copyright in your contribution. The agreement grants the project
owner the rights needed for both public open-source distribution and
alternative commercial licensing. Do not submit material that you cannot make
public or license under those terms.

## Public interface and architecture

Opto defines its own public Tcl interface. Its command catalog, typed argument
grammar, object model, reports, tests, and architecture documentation are the
reviewable contract; another synthesis product is not the specification.
Verify behavior against public standards and reproducible examples before
changing it, and record intentional interface changes in
[`docs/architecture.md`](docs/architecture.md). Do not expose invented
commands, inert options, compatibility-only aliases, or deprecated names.

Keep the target contracts and dependency/ownership rules in
[`docs/architecture.md`](docs/architecture.md). Its conformance matrix is the
only architecture document allowed to claim what the current tree implements.
The regional design is specified by
[RFC 0006](docs/rfcs/0006-region-parallel-synthesis.md); do not describe its
missing stages as current behavior. Domain algorithms must not depend on
session state. Analysis may be parallel and read-only; commits must remain
deterministic.

## Public repository policy

Do not commit proprietary PDKs, non-redistributable assets, license
configuration, private regressions, or their raw outputs. Public benchmarks must use
redistributable or checksum-pinned inputs. See the repository data policy in
the README.

Accepted contributions are published under `GPL-3.0-only` unless a clearly
separated component states another license. All contributors must also follow
the code of conduct.
