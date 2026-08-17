// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top(
  input  logic [0:31] base,
  input  logic [7:0]  patch,
  output logic [0:31] y
);
  always_comb begin
    y = base;
    y[0:7] = patch ^ base[16:23];
  end
endmodule
