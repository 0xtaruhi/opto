// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top(
  input  logic [0:7] a,
  output logic [0:7] y
);
  always_comb begin
    y = '0;
    foreach (a[i]) begin
      y[i] = a[i] ^ i[0];
    end
  end
endmodule
