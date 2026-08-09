<!-- SPDX-FileCopyrightText: 2026 Zhengyi Zhang -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# third_party

Pinned external source trees live here.

The SystemVerilog frontend uses pinned Git submodules:

```text
third_party/slang/  https://github.com/MikePopoloski/slang.git
third_party/fmt/    https://github.com/fmtlib/fmt.git
```

Their upstream license texts are retained as `third_party/slang/LICENSE` and
`third_party/fmt/LICENSE`. The official Tcl 8.6.18 source distribution is
vendored without local patches under `third_party/tcl`; its license is retained
as `third_party/tcl/license.terms`. The imported release asset is
`tcl8.6.18-src.tar.gz` from `tcltk/tcl` with SHA-256
`14f9af32b1767ff718477a8f974ad03c34341097e6b43f4ce54644ee974e268e`.

Rules:

- Do not fetch dependencies from the network during a normal build.
- Keep every submodule revision pinned by `.gitmodules` and the repository
  gitlinks.
- Do not replace slang with a local hand-written SystemVerilog parser.
