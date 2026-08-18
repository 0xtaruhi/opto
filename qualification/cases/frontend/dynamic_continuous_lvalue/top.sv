// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top(
  input  logic [1:0] index,
  input  logic       data,
  output wire  [3:0] y
);
  assign y[index] = data;
endmodule
