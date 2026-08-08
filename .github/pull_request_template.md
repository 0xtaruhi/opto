<!-- SPDX-FileCopyrightText: 2026 Zhengyi Zhang -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

## Summary

Describe the problem and the resulting behavior.

## Verification

- [ ] The CLA Assistant check passes, or the maintainer has confirmed that an
      existing corporate agreement covers this contribution.
- [ ] SPDX, public-repository, and architecture checks pass.
- [ ] `cargo fmt --all`, `cargo check`, Clippy with warnings denied, and tests pass.
- [ ] A regression test covers each fixed bug.
- [ ] Synthesis changes include equivalence evidence.
- [ ] QoR-affecting changes include representative results and baseline updates.
- [ ] No proprietary inputs, reports, paths, or license configuration are included.

## Prior art and design

Document architectural tradeoffs when applicable. When a change follows DC or
Genus behavior, cite the verified behavior; when it departs from both, record
the departure and its reason in `docs/architecture.md`.

## Contributor license agreement

Read [the Opto Contributor License Agreement](../CONTRIBUTOR-LICENSE-AGREEMENT.md).
First-time individual contributors will receive automated signing instructions
from CLA Assistant. If an employer or another entity may own the contribution,
obtain written authorization as described in the agreement and wait for
maintainer confirmation. Do not place private corporate agreements in the pull
request.
