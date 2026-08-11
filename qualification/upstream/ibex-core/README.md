<!-- SPDX-FileCopyrightText: 2026 Zhengyi Zhang -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# Ibex core integration gate

The case pins the official Ibex repository at commit
`c6edaa4060b1a3cd27fda928058db4f0ee3d24bd`. `manifest.tsv` records the exact
source list and SHA-256 of every consumed file before Opto analyzes,
elaborates, links and checks the complete core.

```sh
OPTO_SOURCE_IBEX=/path/to/ibex \
OPTO_REGRESSION_OUTPUT=/tmp/opto-regression \
  cargo test -p opto --test integration qualification::upstream_ibex \
  -- --exact --ignored --nocapture
```

The upstream RTL, local libraries, and generated artifacts remain outside the
repository.

Set `IBEX_SYNTHESIS=1` to map the complete core with the qualification library.
When `IBEX_NETLIST_DIRECTORY` is also set, the flow writes `ibex_core.v` there.
The same self-checking program can then run against the pinned RTL and mapped
gate netlist:

```sh
IBEX_ROOT=/path/to/ibex \
  qualification/upstream/ibex-core/run_smoke.sh rtl /tmp/ibex-rtl

qualification/upstream/ibex-core/run_smoke.sh gate /tmp/ibex-gate \
  /tmp/ibex-netlist/ibex_core.v
```

Both simulations execute arithmetic, branch, load/store, `DIV`, and `REM`
instructions and must write the value `16` to signature address `0x104`.
