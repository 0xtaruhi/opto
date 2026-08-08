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
