// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top(
  input  logic [7:0] a,
  output logic [7:0] y
);
  always_comb begin
    integer i;
    integer j;
    i = 0;
    j = 4;
    y = '0;
    while (i < 4) begin
      y[i] = a[i];
      i++;
    end
    do begin
      y[j] = a[j] ^ j[0];
      j++;
    end while (j < 8);
  end
endmodule
