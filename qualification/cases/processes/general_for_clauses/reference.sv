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
    mask = data & ~skip;
    total = seed + data[0] + data[1] + data[2] + data[3];
    if (stop[0]) begin
      checksum = 4'd0;
      count = 3'd0;
    end else if (stop[1]) begin
      checksum = 4'd1;
      count = 3'd1;
    end else if (stop[2]) begin
      checksum = 4'd3;
      count = 3'd2;
    end else if (stop[3]) begin
      checksum = 4'd6;
      count = 3'd3;
    end else begin
      checksum = 4'd10;
      count = 3'd4;
    end
  end
endmodule
