// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top (
  input  [3:0] a,
  input  [3:0] b,
  input  [3:0] c,
  input  [3:0] d,
  input        enable,
  output [3:0] and_result,
  output [3:0] or_result,
  output [3:0] and_tristate,
  output [3:0] or_tristate
);
  assign and_result = a & b;
  assign or_result = c | d;
  assign and_tristate = a & (b | {4{~enable}});
  assign or_tristate = c | (d & {4{~enable}});
endmodule
