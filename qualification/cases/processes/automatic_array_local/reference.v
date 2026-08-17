// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top(
  input        clk,
  input  [1:0] index,
  input  [7:0] a,
  input  [7:0] b,
  output reg [7:0] q
);
  always @(posedge clk) begin
    case (index)
      2'd0: q <= a;
      2'd1: q <= b;
      2'd2: q <= a ^ b;
      default: q <= a + b;
    endcase
  end
endmodule
