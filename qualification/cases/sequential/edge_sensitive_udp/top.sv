// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

primitive udp_dff(q, d, clk, reset_n);
  output reg q;
  input d, clk, reset_n;
  table
    ? ?    0 : ? : 0;
    0 p    1 : ? : 0;
    1 p    1 : ? : 1;
    ? (10) ? : ? : -;
    * ?    ? : ? : -;
  endtable
endprimitive

module top(
  input  logic clk,
  input  logic reset_n,
  input  logic d,
  output logic q
);
  udp_dff u_dff(q, d, clk, reset_n);
endmodule
