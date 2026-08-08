// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top (
  input  logic clk,
  input  logic preset,
  input  logic clear,
  input  logic enable,
  input  logic data,
  output logic q
);
  always_ff @(posedge clk or posedge preset or posedge clear) begin
    if (preset)
      q <= 1'b1;
    else if (clear)
      q <= 1'b0;
    else if (enable)
      q <= data;
  end
endmodule
