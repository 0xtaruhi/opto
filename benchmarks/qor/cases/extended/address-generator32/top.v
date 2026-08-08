// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top (
  input  [31:0] base,
  input  [31:0] index,
  input  [31:0] displacement,
  input  [31:0] correction,
  output [31:0] address
);
  assign address = base + index * 32'd12 + displacement - correction;
endmodule
