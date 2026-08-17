// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top(
  input  a,
  input  b,
  input  c,
  output y
);
  assign y = (a & b) | (a & c) | (b & c);
endmodule
