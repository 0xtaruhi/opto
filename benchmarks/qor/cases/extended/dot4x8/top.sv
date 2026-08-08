// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top (
  input  logic signed [7:0] a,
  input  logic signed [7:0] b,
  input  logic signed [7:0] c,
  input  logic signed [7:0] d,
  input  logic signed [7:0] e,
  input  logic signed [7:0] f,
  input  logic signed [7:0] g,
  input  logic signed [7:0] h,
  output logic signed [16:0] result
);
  assign result = a * b + c * d + e * f + g * h;
endmodule
