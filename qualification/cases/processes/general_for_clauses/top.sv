// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top(
  input  logic [3:0] data,
  input  logic [3:0] skip,
  input  logic [3:0] stop,
  input  logic [4:0] seed,
  output logic [3:0] mask,
  output logic [3:0] checksum,
  output logic [2:0] count,
  output logic [4:0] total
);
  always_comb begin
    integer i;
    logic [4:0] sum;
    i = 0;
    mask = 4'b0000;
    checksum = 4'b0000;
    for (; i < 4;) begin
      i++;
      if (skip[i - 1]) continue;
      mask[i - 1] = data[i - 1];
    end
    for (i = 0;; i++, checksum += i) begin
      if (stop[i] || i == 4) break;
    end
    count = i[2:0];
    for (i = 0, sum = seed; i < 4; i++) begin
      sum += data[i];
    end
    total = sum;
  end
endmodule
