// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top (
  input  logic        clk,
  input  logic [31:0] left,
  input  logic [31:0] right,
  output logic [32:0] sum
);
  always_ff @(posedge clk)
    sum <= left + right;
endmodule
