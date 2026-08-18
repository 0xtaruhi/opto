// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top(
  input  logic clock,
  input  logic pos_enable,
  input  logic neg_enable,
  input  logic data,
  output logic q
);
  always @(posedge clock iff pos_enable or negedge clock iff neg_enable)
    q <= data;
endmodule
