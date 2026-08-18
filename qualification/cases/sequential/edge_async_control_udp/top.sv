// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

primitive udp_dff_edge_reset(q, d, clk, reset);
  output reg q;
  input d, clk, reset;
  table
    ? ? r : ? : 0;
    ? r 1 : ? : 0;
    0 r 0 : ? : 0;
    1 r 0 : ? : 1;
  endtable
endprimitive

module top(
  input  logic d,
  input  logic clk,
  input  logic reset,
  output logic q
);
  udp_dff_edge_reset u_state(q, d, clk, reset);
endmodule
