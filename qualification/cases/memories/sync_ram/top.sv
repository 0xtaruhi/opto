// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top (
  input  logic        clk,
  input  logic        write_enable,
  input  logic [5:0]  address,
  input  logic [31:0] write_data,
  output logic [31:0] read_data
);
  logic [31:0] memory [64];

  always_ff @(posedge clk) begin
    if (write_enable)
      memory[address] <= write_data;
    read_data <= memory[address];
  end
endmodule
