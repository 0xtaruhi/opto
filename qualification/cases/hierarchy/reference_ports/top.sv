// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module transform(
  ref   logic [7:0] value,
  input logic [7:0] data,
  input logic       invert
);
  always_comb begin
    value = data;
    if (invert)
      value = ~value;
  end
endmodule

module store(
  ref   logic [7:0] value,
  input logic [7:0] data,
  input logic       clk
);
  always_ff @(posedge clk)
    value <= data;
endmodule

module top(
  input  logic [7:0] data,
  input  logic       invert,
  input  logic [1:0] index,
  input  logic       clk,
  output logic [7:0] y,
  output logic [7:0] dynamic_y,
  output logic [31:0] state
);
  logic [7:0] shared;
  logic [7:0] values [0:3];
  transform u_transform(.value(shared), .data(data), .invert(invert));
  store u_dynamic(.value(values[index]), .data(data), .clk(clk));
  assign y = shared;
  assign dynamic_y = values[index];
  assign state = {values[3], values[2], values[1], values[0]};
endmodule
