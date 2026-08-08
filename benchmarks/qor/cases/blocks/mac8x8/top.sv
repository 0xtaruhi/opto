// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top (
  input  logic signed [7:0]  multiplicand,
  input  logic signed [7:0]  multiplier,
  input  logic signed [15:0] addend,
  output logic signed [16:0] result
);
  assign result = multiplicand * multiplier + addend;
endmodule
