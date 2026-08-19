<!-- SPDX-FileCopyrightText: 2026 Zhengyi Zhang -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# PULP AXI qualification

This case uses the official [`pulp-platform/axi`](https://github.com/pulp-platform/axi)
checkout at `4da15979747f326bde2f9869c64e587ce599772c`. The upstream RTL is
Solderpad Hardware License 0.51; the audited root `LICENSE` SHA-256 is
`6527a46225891b976fa94f634f6ee0cd22e1c7129d6f11c5c98c997627625fc7`.
No upstream source is copied into this repository.

`manifest.tsv` pins all 60 production AXI source files and 111 required source
files from the Bender checkouts. The checkout used to establish the manifest
has these dependency revisions:

- `common_cells`: `9ca8a7655f741e7dd5736669a20a301325194c28`
- `tech_cells_generic`: `7968dd6e6180df2c644636bc6d2908a49f2190cf`

The suite parses the complete manifest for every run, then elaborates and maps
the live-I/O roots in `designs.tsv`. Keeping all request and response channels
at top-level boundaries prevents an empty or mostly constant benchmark from
passing merely because dead hierarchy was removed.

| Root | Principal coverage |
| --- | --- |
| `pipeline` | cut, FIFO, ATOP filter, serializer and isolation |
| `cdc` | multi-clock AXI CDC and common-cell CDC FIFOs |
| `data-width` | 64-to-32-bit full-AXI downsizing |
| `data-width-up` | 32-to-64-bit full-AXI upsizing |
| `id-width` | ID serialization through the width converter |
| `memory-bridge` | two-bank AXI-to-memory request and response paths |
| `lite-crossbar` | two-by-two AXI4-Lite crossbar routing |
| `full-lite-bridge` | burst unwrap, full-to-lite and lite-to-full conversion |
| `control` | fixed delay, throttling and multiple register cuts |
| `lite-peripherals` | mailbox, register bank and pipelined APB bridge |
| `full-crossbar` | two-by-two full AXI crossbar, demux and ID-widening mux |
| `memory-endpoints` | memory-to-AXI source and live zero-memory terminator |
| `utilities` | address modification, invalidation, read/write split-join, LFSR and error target |
| `compare` | dual full-AXI traffic and response comparator with per-ID FIFOs |

Run the suite with the pinned checkout after initializing its Bender
dependencies:

```sh
cd /path/to/axi
bender checkout

cd /path/to/opto
OPTO_SOURCE_PULP_AXI=/path/to/axi \
OPTO_REGRESSION_OUTPUT=/tmp/opto-regression \
  cargo test -p opto --test qualification upstream_pulp_axi \
  --release --locked -- --exact --ignored --nocapture
```

A successful synthesis and nontrivial port/net threshold prove that each named
root remains live and mappable. They do not by themselves prove protocol
compliance. Gate-level structural checks and bounded RTL/gate differential
simulation are recorded separately when synthesis behavior changes.

`axi_fifo_delay_dyn` is intentionally absent from the ASIC roots: upstream
executes `$fatal("Delay unit is not made for synthesis")` whenever `SYNTHESIS`
is defined without `TARGET_XILINX`. Simulation-only modules (`axi_dumper`,
`axi_sim_mem`, and `axi_test`) are likewise not claimed as synthesis coverage.
