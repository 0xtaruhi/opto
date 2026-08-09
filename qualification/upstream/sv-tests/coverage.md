<!-- SPDX-FileCopyrightText: 2026 Zhengyi Zhang -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# sv-tests ASIC synthesis contract

Audited revision: `2913f075dcd10e4d64b7d912fe7d4675dd0a1e29`.

The upstream tree contains 1,027 ISC-marked HDL files. `sv-tests` targets
general SystemVerilog tools, so the absence of its `unsynthesizable` metadata
does not mean that a test is ASIC synthesizable. The old 617-file adapter mixed
positive synthesis constructs with initial blocks, delays, randomization,
classes, assertions, system tasks, dynamic data structures and parser-negative
tests. Its partial conformance count was therefore neither a synthesis pass
rate nor a useful product acceptance gate.

The reviewed [`scope.toml`](scope.toml) instead defines 137 positive ASIC
frontend requirements. Every required case must pass: there are no expected
failures and no known-gap category. The baseline fixes both the count and the
SHA-256 of the sorted required path set, so dropping one case cannot preserve a
green result.

| Feature group | Required | Passing |
| --- | ---: | ---: |
| Numeric literals | 55 | 55 |
| Type declarations | 26 | 26 |
| Declarations and types | 25 | 25 |
| Lexical and preprocessing | 16 | 16 |
| Static arrays and memories | 5 | 5 |
| Event expressions | 3 | 3 |
| Design units and packages | 3 | 3 |
| Continuous assignments | 2 | 2 |
| Packed aggregate declarations | 2 | 2 |
| **Total** | **137** | **137** |

For each file, the adapter runs analysis and elaborates its selected top when
one exists. If the elaborated design exposes ports, it also runs the complete
`synthesis` flow. Declaration-only LRM examples intentionally stop at
elaboration: adding artificial ports or wrappers would no longer test the
pinned upstream source.

## Scope rules

A required case must be an upstream positive parsing, preprocessing or
elaboration test; it must not be tagged `simulation` or `unsynthesizable`.
Passing those metadata checks is necessary but not sufficient. Inclusion in
the committed scope is a reviewed assertion that the construct belongs in the
static ASIC frontend contract.

The remaining 890 files stay in the license and inventory audit, but are not
silently treated as synthesis failures or successes. They cover simulation,
verification and tool-language behavior, upstream negative tests, or constructs
outside Opto's synthesizable ASIC contract. In particular, declaration
initialization is executable time-zero behavior and is not admitted merely
because some FPGA flows synthesize it.

When changing the pinned revision or scope, review source semantics and license
metadata, update `scope.toml`, run the complete qualification, and then update
the count and required-set hash together. Never derive scope membership from
whether Opto currently accepts a file.
