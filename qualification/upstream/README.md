<!-- SPDX-FileCopyrightText: 2026 Zhengyi Zhang -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# External qualification suites

Third-party RTL is checked out beside the repository and never copied into the
public tree. The Rust harness rejects a different Git revision, verifies source
manifests or inventory, and keeps generated reports outside the checkout.

| Source | Pinned revision | License handling | Coverage |
| --- | --- | --- | --- |
| CHIPS Alliance `sv-tests` | `2913f075dcd10e4d64b7d912fe7d4675dd0a1e29` | ISC; root license hash checked and all 1,027 HDL files must carry `SPDX-License-Identifier: ISC` | 137/137 reviewed positive ASIC frontend cases |
| Yosys RTL tests | `a0fbe6e13311d4909938c63eeb28b6c730467e6c` | ISC; root `COPYING` hash and the 549-file HDL inventory are checked | 172 synthesized designs; 119/119 mandatory observable targets formally equivalent |
| lowRISC Ibex | `c6edaa4060b1a3cd27fda928058db4f0ee3d24bd` | Apache-2.0 upstream checkout; every consumed RTL file is hash-pinned | ALU and complete core hierarchy |
| OpenHW CVA6 | `ed2efa51744387f510bfa4dcec29eb2e1f5697cf` | external checkout with upstream per-file licenses; every consumed RTL/config file is hash-pinned | two large core configurations |
| PULP AXI | `4da15979747f326bde2f9869c64e587ce599772c` | Solderpad Hardware License 0.51 external checkout; all consumed AXI and Bender dependency sources are hash-pinned | complete production source manifest and fourteen live mapped integration roots |

`sv-tests` contains 1,027 HDL files at the audited revision. Because its
metadata does not define an ASIC synthesis subset, the reviewed
`sv-tests/scope.toml` selects 137 positive synthesis-frontend requirements.
All 137 pass; cases with a top are elaborated and ported designs also run the
complete synthesis flow. The exact required set is committed as a count plus
SHA-256 in `sv-tests/baseline.toml`. Selection rules and the feature breakdown
are in `sv-tests/coverage.md`.

The pinned Yosys checkout contains 549 HDL test files. The qualification
adapter runs 136 cases that are independently meaningful without translating
a Yosys `.ys` driver. All 109 required compilation cases conform, with no
remaining capability gaps and 27 reviewed exclusions. The adapter synthesizes
172 designs, identifies 173 independently observable root designs, and proves
all 119 mandatory proof targets against Yosys using two complementary
four-state equivalence encodings. The remaining 53 targets are exact, hashed
deferrals: memory/state correspondence, undefined input domains, or an
unsupported reference-solver primitive. They are not counted as proofs.
Files driven by multi-file or self-checking `.ys` scripts stay audited but are
not mislabeled as independent circuits.

Run it with an official checkout at the pinned revision:

```sh
OPTO_SOURCE_SV_TESTS=/path/to/sv-tests \
OPTO_SV_TESTS_JOBS=8 \
OPTO_REGRESSION_OUTPUT=/tmp/opto-regression \
  cargo test -p opto --test qualification systemverilog_conformance \
  -- --exact --ignored --nocapture
```

Run the Yosys synthesis and formal qualification with both pinned source and a
Yosys executable:

```sh
OPTO_SOURCE_YOSYS_TESTS=/path/to/yosys-source \
OPTO_YOSYS=/path/to/yosys \
OPTO_YOSYS_TESTS_JOBS=8 \
OPTO_REGRESSION_OUTPUT=/tmp/opto-regression \
  cargo test -p opto --test qualification yosys_rtl_qualification \
  -- --exact --ignored --nocapture
```

Run the real-design gates similarly:

```sh
OPTO_SOURCE_IBEX=/path/to/ibex \
  cargo test -p opto --test qualification upstream_ibex \
  -- --exact --ignored --nocapture

OPTO_SOURCE_CVA6=/path/to/cva6 \
  cargo test -p opto --test qualification upstream_cva6 \
  -- --exact --ignored --nocapture

OPTO_SOURCE_PULP_AXI=/path/to/axi \
  cargo test -p opto --test qualification upstream_pulp_axi \
  --release --locked -- --exact --ignored --nocapture
```

Why external checkouts: ISC and Apache-2.0 material can be distributed under
their terms alongside GPLv3 code when notices are preserved, but not every
large hardware repository has one uniform license. Keeping the exact upstream
checkout external avoids stripping file-level notices and makes provenance
auditable. A new source must record its canonical URL, immutable revision,
license files, consumed-file hashes, and redistribution decision before it can
enter CI.

Do not import HDLBits, IWLS/ISCAS mirrors, scraped tutorial collections, or
other corpora whose test vectors or redistribution permission cannot be
traced to an authoritative license.
