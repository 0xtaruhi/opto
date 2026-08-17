// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top (
  input  logic        clk,
  input  logic        rst_n,
  input  logic        write_enable,
  input  logic [1:0]  address,
  input  logic [7:0]  data,
  output logic [7:0]  q,
  output logic [31:0] state
);
  logic [7:0] memory [0:3];
  integer index;

  always_ff @(posedge clk or negedge rst_n) begin
    if (!rst_n) begin
      for (index = 0; index < 4; index = index + 1)
        memory[index] <= '0;
    end else if (write_enable) begin
      memory[address] <= data;
    end else begin
      for (index = 0; index < 4; index++)
        memory[index] <= memory[index];
    end
  end

  assign q = memory[address];
  assign state = {memory[3], memory[2], memory[1], memory[0]};
endmodule
