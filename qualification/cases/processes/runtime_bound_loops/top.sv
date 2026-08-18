// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top(
  input  logic        data,
  input  logic [3:0]  limit,
  input  logic signed [3:0] signed_limit,
  output logic [15:0] while_mask,
  output logic [15:0] do_mask,
  output logic [4:0]  while_count,
  output logic [4:0]  do_count,
  output logic [4:0]  for_count,
  output logic [4:0]  signed_count
);
  always_comb begin
    integer i;
    integer j;
    integer k;
    integer signed_index;
    i = 0;
    j = 0;
    k = 0;
    signed_index = 0;
    while_mask = '0;
    do_mask = '0;
    while (i < limit) begin
      while_mask[i] = data;
      i++;
    end
    do begin
      do_mask[j] = data;
      j++;
    end while (j < limit);
    for (k = 0; k < limit; k++) begin
    end
    while (signed_index < signed_limit)
      signed_index++;
    while_count = i[4:0];
    do_count = j[4:0];
    for_count = k[4:0];
    signed_count = signed_index[4:0];
  end
endmodule
