// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top(
  input  logic d,
  input  logic clk,
  input  logic reset,
  output logic q
);
  always_ff @(posedge clk or posedge reset) begin
    if (reset) q <= 1'b0;
    else q <= d;
  end
endmodule
