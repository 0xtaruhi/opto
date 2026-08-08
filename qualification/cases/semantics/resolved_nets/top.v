// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top (
  input  [3:0] a,
  input  [3:0] b,
  input  [3:0] c,
  input  [3:0] d,
  output wand [3:0] and_result,
  output wor  [3:0] or_result
);
  assign and_result = a;
  assign and_result = b;
  assign or_result = c;
  assign or_result = d;
endmodule
