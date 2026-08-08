// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top (
  input  logic       clk,
  input  logic       write_a,
  input  logic       write_b,
  input  logic [1:0] address_a,
  input  logic [1:0] address_b,
  input  logic [3:0] data_a,
  input  logic [3:0] data_b,
  output logic [3:0] read_a,
  output logic [3:0] read_b,
  output logic [15:0] state
);
  logic [3:0] memory [0:3];

  assign state = {memory[3], memory[2], memory[1], memory[0]};

  always_ff @(posedge clk) begin
    if (write_a)
      memory[address_a] <= data_a;
    read_a <= memory[address_a];
  end

  always_ff @(posedge clk) begin
    if (write_b && (!write_a || address_a != address_b))
      memory[address_b] <= data_b;
    read_b <= memory[address_b];
  end
endmodule
