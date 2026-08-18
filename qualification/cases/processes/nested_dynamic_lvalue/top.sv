// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top(
  input  logic [1:0] row,
  input  logic [1:0] bit_index,
  input  logic       data,
  output logic [15:0] y
);
  logic [3:0] foo [0:3];

  always_comb begin
    foo[0] = '0;
    foo[1] = '0;
    foo[2] = '0;
    foo[3] = '0;
    foo[row][bit_index] = data;
    y = {foo[0], foo[1], foo[2], foo[3]};
  end
endmodule
