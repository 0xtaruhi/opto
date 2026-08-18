// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top(
  input  a,
  input  b,
  input  select,
  output y
);
  assign y = ~(select ? a : b);
endmodule
