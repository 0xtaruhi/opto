// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

primitive udp_majority(output out, input a, b, c);
  table
    0 0 ? : 0;
    0 ? 0 : 0;
    ? 0 0 : 0;
    1 1 ? : 1;
    1 ? 1 : 1;
    ? 1 1 : 1;
  endtable
endprimitive

module top(
  input  logic a,
  input  logic b,
  input  logic c,
  output logic y
);
  udp_majority u_majority(y, a, b, c);
endmodule
