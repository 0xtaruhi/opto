// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top (
  input  logic signed [15:0] a,
  input  logic signed [7:0]  b,
  input  logic [2:0] shift,
  output logic signed [16:0] sum,
  output logic signed [15:0] shifted,
  output logic less,
  output logic [7:0] slice
);
  assign sum = a + $signed(b);
  assign shifted = a >>> shift;
  assign less = a < $signed({{8{b[7]}}, b});
  assign slice = a[shift +: 8];
endmodule
