// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! End-to-end command-line and synthesis qualification suite.

mod cli;
mod qualification;
mod qualification_support;
#[path = "../support/tcl.rs"]
mod test_tcl;
