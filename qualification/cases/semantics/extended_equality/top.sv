// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top(
  input  bit [3:0] a,
  input  bit [3:0] b,
  output logic     exact,
  output logic     different,
  output logic     masked,
  output logic     masked_different
);
  assign exact = a === b;
  assign different = a !== b;
  assign masked = a ==? 4'b1x0z;
  assign masked_different = a !=? 4'b0z1x;
endmodule
