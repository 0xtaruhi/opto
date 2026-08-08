// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top (
  input  logic [7:0] a,
  input  logic [7:0] b,
  output logic [8:0] y
);
  assign y = a + b;
endmodule
