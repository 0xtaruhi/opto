// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

typedef struct {
  logic [7:0] lanes [0:1];
  logic [3:0] tag;
} entry_t;

module top(
  input  logic       clk,
  input  logic       rst_n,
  input  logic       write_row,
  input  logic       write_column,
  input  logic       read_row,
  input  logic       read_column,
  input  logic [7:0] data,
  output logic [7:0] q,
  output logic [31:0] lanes
);
  entry_t state [0:1];

  always_ff @(posedge clk or negedge rst_n) begin
    if (!rst_n) begin
      state[0].lanes[0] <= '0;
      state[0].lanes[1] <= '0;
      state[1].lanes[0] <= '0;
      state[1].lanes[1] <= '0;
    end else begin
      state[write_row].lanes[write_column] <= data;
    end
  end

  assign q = state[read_row].lanes[read_column];
  assign lanes = {
    state[1].lanes[1],
    state[1].lanes[0],
    state[0].lanes[1],
    state[0].lanes[0]
  };
endmodule
