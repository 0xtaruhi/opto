<!-- SPDX-FileCopyrightText: 2026 Zhengyi Zhang -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# Public QoR trends

The [weekly QoR dashboard](qor/) records area, timing, cell count, and runtime
from checksum-pinned public-library benchmarks. Each point identifies the exact
Opto commit, and the workflow retains the structured result and diagnostics as
build artifacts.

Area and cell comparisons use identical RTL and Liberty inputs. Comparative
slack and critical delay remain unpublished until the same independent STA
contract evaluates every mapped netlist.

The production regional architecture additionally defines a public
hundred-thousand-, million- and ten-million-gate qualification contract in
[`architecture.md`](architecture.md). Candidate commits are compared with the
last accepted Opto baseline on the same host and worker count, with versioned
area, timing, throughput, and peak-RSS bounds. These large-scale gates have not
yet been demonstrated and are not performance claims about the current tree.
