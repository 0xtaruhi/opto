// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top(
  input  logic       clk,
  input  logic [1:0] index,
  input  logic [7:0] a,
  input  logic [7:0] b,
  output logic [7:0] q
);
  always_ff @(posedge clk) begin
    automatic logic [7:0] temporary [0:3];
    temporary[0] = a;
    temporary[1] = b;
    temporary[2] = a ^ b;
    temporary[3] = a + b;
    q <= temporary[index];
  end
endmodule
