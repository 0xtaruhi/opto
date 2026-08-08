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

The production regional architecture additionally defines an external
hundred-thousand-, million- and ten-million-gate qualification contract in
[`architecture.md`](architecture.md). Its short-term same-host goal is
end-to-end geometric-mean throughput no slower than Genus with five-percent
area/timing bounds and peak RSS no higher than the reference. Ten-times
geometric-mean throughput is the long-term target. These gates have not yet
been demonstrated and are not performance claims about the current tree.
