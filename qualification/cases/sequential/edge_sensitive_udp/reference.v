// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top(
  input      clk,
  input      reset_n,
  input      d,
  output reg q
);
  always @(posedge clk or negedge reset_n) begin
    if (!reset_n)
      q <= 1'b0;
    else
      q <= d;
  end
endmodule
