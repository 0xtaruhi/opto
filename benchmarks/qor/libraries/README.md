<!-- SPDX-FileCopyrightText: 2026 Zhengyi Zhang -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# Library inputs

`cover_test.lib` is an authored, area-only micro-library for deterministic
cover-search regression. It is not a PDK model and must not be used for timing
claims. Its deliberately nonmonotonic gate areas make sharing and duplication
choices observable.

No PDK or foundry standard-cell library is committed here.
`fetch_sky130.sh` downloads one Liberty view from an immutable
OpenROAD-flow-scripts revision and rejects any content whose SHA-256 is not
`ec0e1067a35c8bf20b11e58d1e8ac53326067e4dac84a125cc1b917a3518d0d9`.
Checksum failures are retried as bounded transport faults; the file is
published only after the pinned digest matches, so a retry cannot change the
benchmark library or its baseline.

Provenance:

- OpenROAD-flow-scripts revision:
  `a5ff7ef7dac4338e6e5fad7710b85fc6c8f3503c` (BSD-3-Clause repository;
  platform inputs retain their own licenses).
- SkyWater SKY130 PDK and the `sky130_fd_sc_hd` standard-cell source:
  Apache-2.0.
- The downloaded Liberty remains an external test input; it is neither
  modified nor redistributed by Opto.

When the revision changes, re-audit the upstream platform license and cell
library provenance before updating the URL and checksum together.
