// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top (
  input  logic        clock_a,
  input  logic        clock_b,
  input  logic        write_a,
  input  logic        write_b,
  input  logic [7:0]  data_a,
  input  logic [7:0]  data_b,
  output logic [15:0] state
);
  logic [7:0] memory [0:1];

  always_ff @(posedge clock_a)
    if (write_a)
      memory[0] <= data_a;

  always_ff @(posedge clock_b)
    if (write_b)
      memory[1] <= data_b;

  assign state = {memory[1], memory[0]};
endmodule
