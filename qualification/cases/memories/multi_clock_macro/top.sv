// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top (
  input  logic       clock_a,
  input  logic       clock_b,
  input  logic       write_a,
  input  logic       write_b_n,
  input  logic [1:0] address_a,
  input  logic [1:0] address_b,
  input  logic [1:0] read_address,
  input  logic [7:0] data_a,
  input  logic [7:0] data_b,
  output logic [7:0] result
);
  logic [7:0] memory [0:3];

  always_ff @(posedge clock_a)
    if (write_a)
      memory[address_a] <= data_a;

  always_ff @(posedge clock_b)
    if (!write_b_n)
      memory[address_b] <= data_b;

  assign result = memory[read_address];
endmodule
