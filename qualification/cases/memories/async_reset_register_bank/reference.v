// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top (
  input        clk,
  input        rst_n,
  input        write_enable,
  input  [1:0] address,
  input  [7:0] data,
  output [7:0] q,
  output [31:0] state
);
  reg [31:0] memory;

  always @(posedge clk or negedge rst_n) begin
    if (!rst_n)
      memory <= 32'b0;
    else if (write_enable) begin
      case (address)
        2'd0: memory[7:0] <= data;
        2'd1: memory[15:8] <= data;
        2'd2: memory[23:16] <= data;
        2'd3: memory[31:24] <= data;
      endcase
    end
  end

  assign q = address == 2'd0 ? memory[7:0] :
             address == 2'd1 ? memory[15:8] :
             address == 2'd2 ? memory[23:16] : memory[31:24];
  assign state = memory;
endmodule
