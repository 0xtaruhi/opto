// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top(
  input  logic clk,
  input  logic reset_n,
  input  logic ignored,
  input  logic enable,
  input  logic data,
  output logic value
);
  always_ff @(posedge clk or negedge reset_n) begin
    if (!reset_n)
      value <= 1'b0;
    else if (enable)
      value <= data;
  end
endmodule
