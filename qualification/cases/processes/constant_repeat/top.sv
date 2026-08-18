// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top(
  input  logic [7:0] seed,
  input  logic       data,
  output logic [7:0] y
);
  logic [7:0] acc;

  always_comb begin
    acc = seed;
    repeat (4) begin
      acc = {acc[6:0], acc[7] ^ data};
    end
    y = acc;
  end
endmodule
