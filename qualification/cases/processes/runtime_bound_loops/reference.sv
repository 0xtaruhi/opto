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
  logic [4:0] effective_do_count;

  always_comb begin
    effective_do_count = limit == 0 ? 5'd1 : {1'b0, limit};
    while_count = {1'b0, limit};
    do_count = effective_do_count;
    for_count = {1'b0, limit};
    signed_count = signed_limit > 0 ? {{1{1'b0}}, signed_limit} : 5'd0;
    while_mask = data ? ((16'h0001 << limit) - 1'b1) : 16'd0;
    do_mask = data ? ((16'h0001 << effective_do_count) - 1'b1) : 16'd0;
  end
endmodule
