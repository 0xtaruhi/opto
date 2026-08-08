// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top (
  input  logic        clk,
  input  logic [63:0] left,
  input  logic [63:0] right,
  input  logic        carry,
  output logic [64:0] sum
);
  logic [63:0] left_q, right_q;
  logic        carry_q;
  logic [64:0] sum_q;

  always_ff @(posedge clk) begin
    left_q <= left;
    right_q <= right;
    carry_q <= carry;
    sum_q <= {1'b0, left_q} + {1'b0, right_q} + {64'b0, carry_q};
  end

  assign sum = sum_q;
endmodule
