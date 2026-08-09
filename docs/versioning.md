<!-- SPDX-FileCopyrightText: 2026 Zhengyi Zhang -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# Versioning and releases

Opto follows Semantic Versioning. Before 1.0, a minor release may deliberately
change Rust APIs, Tcl coverage, checkpoint formats, or netlist details; every
such change must be listed under `Unreleased` in `CHANGELOG.md`. Patch releases
contain compatible fixes and do not intentionally change synthesis results.

A release is cut from a clean `main` commit by updating the workspace version,
moving the changelog entries into a dated section, and pushing an annotated
`vMAJOR.MINOR.PATCH` tag. The release workflow validates the full workspace,
builds the single `opto` executable on Linux, macOS, and Windows, and attaches
the binaries and checksums to the GitHub release.

Changes to public Tcl names or semantics require compatibility evidence. Changes
to IR invariants, pass ordering, mapping objectives, or checkpoint formats use
the RFC process.
