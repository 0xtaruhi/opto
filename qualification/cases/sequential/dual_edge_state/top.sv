// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top (
  input  logic clock,
  input  logic reset,
  input  logic enable,
  input  logic data,
  output logic q
);
  always @(posedge clock or negedge clock) begin
    if (reset)
      q <= 1'b0;
    else if (enable)
      q <= data;
  end
endmodule
