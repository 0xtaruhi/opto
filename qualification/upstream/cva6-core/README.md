<!-- SPDX-FileCopyrightText: 2026 Zhengyi Zhang -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# CVA6 qualification

This case pins CVA6 at `ed2efa51744387f510bfa4dcec29eb2e1f5697cf` and covers the
RV32IMAC/Sv32 and RV64IMAFDC/Sv39 configurations. The source inventory is
checksum-pinned in `manifest.tsv`; `core/cvfpu` and
`core/cache_subsystem/hpdcache` must be initialized from the pinned checkout.

The normal upstream suite performs analysis, elaboration, linking, structural
checks, and an area report for both configurations. Full Liberty mapping is
deliberately opt-in because the large flat mapped netlist has substantially
higher runtime and memory requirements than frontend qualification.

To run full synthesis without writing the large netlist:

```sh
CVA6_COMPILE=1 \
OPTO_SOURCE_CVA6=/path/to/cva6 \
OPTO_REGRESSION_OUTPUT=/path/to/results \
cargo test -p opto --test qualification upstream_cva6 \
  --locked -- --exact --ignored --nocapture
```

Set `CVA6_NETLIST_DIRECTORY=/path/to/netlists` when gate-level simulation
artifacts are required. Each configuration is written separately, using the
configuration package basename as the filename. The smoke test boots a
six-instruction RV32 program through an AXI memory model, computes `5 + 7`, and
requires a 32-bit signature value of `12` at address `0x10000100`. The memory
model handles AXI read bursts, independent write-address and write-data
handshakes, byte strobes, and write responses.

Run the stimulus against the RTL with:

```sh
CVA6_ROOT=/path/to/cva6 \
CXX=/path/to/a/C++20/compiler \
qualification/upstream/cva6-core/run_smoke.sh rtl /path/to/rtl-results
```

The same stimulus accepts a generated netlist mapped to `opto_test.lib`:

```sh
CXX=/path/to/a/C++20/compiler \
qualification/upstream/cva6-core/run_smoke.sh gate /path/to/gate-results \
  /path/to/cva6.v
```

The flat gate build needs substantially more memory than synthesis. Verilator
5.048 exceeded 43 GiB RSS while still growing on the measured RV32 netlist, so
gate simulation should run on a host with a larger memory budget.
