// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top(
  input  [7:0] data,
  input        invert,
  input  [1:0] index,
  input        clk,
  output [7:0] y,
  output [7:0] dynamic_y,
  output [31:0] state
);
  reg [31:0] values;

  always @(posedge clk) begin
    case (index)
      2'd0: values[7:0] <= data;
      2'd1: values[15:8] <= data;
      2'd2: values[23:16] <= data;
      default: values[31:24] <= data;
    endcase
  end

  assign y = invert ? ~data : data;
  assign dynamic_y = index == 2'd0 ? values[7:0] :
                     index == 2'd1 ? values[15:8] :
                     index == 2'd2 ? values[23:16] : values[31:24];
  assign state = values;
endmodule
