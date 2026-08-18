<!-- SPDX-FileCopyrightText: 2026 Zhengyi Zhang -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# opto-slang-sys native bridge

This directory contains the C++ adapter that links against the pinned
SystemVerilog slang source tree.

Rules:

- Do not expose slang C++ types to Rust callers.
- Export only the C ABI declared in `opto_slang_bridge.h`.
- Build from vendored source under `third_party/slang`; ordinary builds must not
  fetch slang from the network.
- Configure CMake with a compiler and standard library that implement C++20.
  Set `CXX` explicitly when the system default is older; for example, this
  development machine uses `CXX=/usr/bin/clang++` instead of GCC 8.
- Format this first-party bridge with clang-format 18 by running
  `python3 tools/check_cpp_format.py --fix` from the repository root. The
  corresponding command without `--fix` checks formatting without modifying
  files; vendored slang sources are outside its scope.
- Do not add a secondary Verilog parser here. If vendored slang is unavailable,
  the build must fail instead of leaving a runtime fallback path.

Current state:

- `CMakeLists.txt` builds a static `opto_slang_bridge` target and links
  `slang::slang` and vendored `fmt` with slang tools, tests, docs, install
  rules, Python bindings, and mimalloc disabled.
- `opto_slang_bridge.cpp` exports the C ABI, runs the slang driver through its
  normal option, parse, and elaboration flow, and freezes the elaborated module
  inventory. It does not retain per-module lowering scratch state.
- Lowering is compiled as independent support/naming, expression, process, and
  hierarchy translation units. Their deliberately private contract lives in
  `opto_slang_lower_internal.h`; only `opto_slang_bridge.h` is part of the C ABI.
  Function lowering stays with process lowering because both share the same CFG
  builder and procedural scope machinery.
- Each materialization creates one `ModuleLoweringContext`. That context
  encapsulates every mutable scope, counter, interning arena, and type cache
  used to lower exactly one module. Its module and body are non-null
  references, so helpers do not depend on ambient snapshot state or an
  "active module" protocol.
- `opto_slang_views.cpp` is the only lowered-data C ABI adapter. It fills one
  aggregate POD view per entity instead of exporting a getter for every field;
  C++ performs bounds checks for opaque collections, and no C++ container
  layout is exposed to Rust.
- Procedural lowering has one representation: each procedure owns a contiguous
  block arena, each block owns ordered assignment effects and exactly one flat
  return/jump/branch/switch terminator, and edges carry indexed targets. No
  recursive statement pointer crosses the ABI.
- Rust acquires a reference-counted native materialization lease and wraps it
  in `SlangMaterializedModule`. All ports, nets, instances, expressions,
  procedures, blocks, effects, and terminators borrow that guard. Dropping the final guard destroys the module's
  single native payload, releasing every container allocation and preserving
  streaming lowering's bounded peak memory without allowing a view to outlive
  its storage.
- Unsupported synthesis constructs must fail with explicit diagnostics at this
  boundary or in `opto-hdl`; unsupported module members must not be silently
  omitted. Do not add fallback parsing or compatibility paths.
