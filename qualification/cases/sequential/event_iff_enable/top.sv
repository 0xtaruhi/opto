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
  always_ff @(posedge clk iff enable or negedge reset_n iff !reset_n or posedge ignored iff 1'b0) begin
    if (!reset_n)
      value <= 1'b0;
    else
      value <= data;
  end
endmodule
