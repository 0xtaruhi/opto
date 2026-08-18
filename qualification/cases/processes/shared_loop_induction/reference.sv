// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top(
  input  logic       clock,
  input  logic [3:0] data,
  output logic [1:0] registered,
  output logic [3:0] combinational,
  output logic [3:0] fully_selected
);
  always_ff @(posedge clock) registered <= data[1:0];
  always_comb combinational = data;
  always_comb fully_selected = data;
endmodule
