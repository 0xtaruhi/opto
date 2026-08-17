// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top(
  input  logic [3:0] stop,
  input  logic [3:0] skip,
  output logic [3:0] mask,
  output logic [2:0] count
);
  always_comb begin
    integer i;
    i = 0;
    mask = 4'b0000;
    forever begin
      if (stop[i] || i == 4) break;
      i++;
      if (skip[i - 1]) continue;
      mask[i - 1] = 1'b1;
    end
    count = i[2:0];
  end
endmodule
