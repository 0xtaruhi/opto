// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top (
  input  logic       clk,
  input  logic       enable,
  input  logic [7:0] data,
  output logic [7:0] value
);
  always_ff @(posedge clk) begin
    if (enable)
      value <= data;
  end
endmodule
