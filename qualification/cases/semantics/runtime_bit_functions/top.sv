// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top(
  input  logic [7:0]  value,
  output logic [7:0]  cube,
  output logic [31:0] count,
  output logic [31:0] zero_count,
  output logic        exactly_one,
  output logic        at_most_one
);
  assign cube = value ** 3;
  assign count = $countones(value);
  assign zero_count = $countbits(value, 1'b0);
  assign exactly_one = $onehot(value);
  assign at_most_one = $onehot0(value);
endmodule
