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
  wire [7:0] predecessor = value - 8'd1;

  assign cube = value * value * value;
  assign count = {31'd0, value[0]} +
                 {31'd0, value[1]} +
                 {31'd0, value[2]} +
                 {31'd0, value[3]} +
                 {31'd0, value[4]} +
                 {31'd0, value[5]} +
                 {31'd0, value[6]} +
                 {31'd0, value[7]};
  assign zero_count = 32'd8 - count;
  assign at_most_one = (value & predecessor) == 8'd0;
  assign exactly_one = (value != 8'd0) && at_most_one;
endmodule
