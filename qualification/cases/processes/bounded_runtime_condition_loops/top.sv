// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top(
  input  logic [3:0] keep,
  input  logic [3:0] skip,
  output logic [2:0] while_count,
  output logic [2:0] do_count,
  output logic [2:0] for_count,
  output logic [3:0] while_mask,
  output logic [3:0] do_mask,
  output logic [3:0] for_mask
);
  always_comb begin
    integer i;
    integer j;
    integer k;
    i = 0;
    j = 0;
    k = 0;
    while_mask = '0;
    do_mask = '0;
    for_mask = '0;
    while (i < 4 && keep[i]) begin
      while_mask[i] = 1'b1;
      i++;
    end
    do begin
      do_mask[j] = 1'b1;
      j++;
    end while (j < 4 && keep[j]);
    for (k = 0; k < 4 && keep[k]; k++) begin
      if (skip[k]) continue;
      for_mask[k] = 1'b1;
    end
    while_count = i[2:0];
    do_count = j[2:0];
    for_count = k[2:0];
  end
endmodule
