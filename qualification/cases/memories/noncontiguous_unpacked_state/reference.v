// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top(
  input        clk,
  input        rst_n,
  input        write_row,
  input        write_column,
  input        read_row,
  input        read_column,
  input  [7:0] data,
  output [7:0] q,
  output [31:0] lanes
);
  reg [39:0] state;

  always @(posedge clk or negedge rst_n) begin
    if (!rst_n) begin
      state[19:4] <= 16'b0;
      state[39:24] <= 16'b0;
    end else begin
      case ({write_row, write_column})
        2'b00: state[11:4] <= data;
        2'b01: state[19:12] <= data;
        2'b10: state[31:24] <= data;
        default: state[39:32] <= data;
      endcase
    end
  end

  assign q = read_row
    ? (read_column ? state[39:32] : state[31:24])
    : (read_column ? state[19:12] : state[11:4]);
  assign lanes = {state[39:32], state[31:24], state[19:12], state[11:4]};
endmodule
