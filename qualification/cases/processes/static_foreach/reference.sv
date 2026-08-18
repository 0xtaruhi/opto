// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top(
  input  logic [0:7] a,
  output logic [0:7] y
);
  always_comb begin
    y = '0;
    y[0] = a[0];
    y[1] = a[1] ^ 1'b1;
    y[2] = a[2];
    y[3] = a[3] ^ 1'b1;
    y[4] = a[4];
    y[5] = a[5] ^ 1'b1;
    y[6] = a[6];
    y[7] = a[7] ^ 1'b1;
  end
endmodule
