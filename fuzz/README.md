<!-- SPDX-FileCopyrightText: 2026 Zhengyi Zhang -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# Input fuzzing

Install `cargo-fuzz`, then run one of the input targets with a nightly
toolchain:

```console
cargo +nightly fuzz run liberty
cargo +nightly fuzz run spef
cargo +nightly fuzz run sdc
cargo +nightly fuzz run checkpoint
```

The nightly workflow performs bounded smoke runs for every target. Longer local
runs keep the generated corpus under `fuzz/corpus/`, which is intentionally not
tracked.
