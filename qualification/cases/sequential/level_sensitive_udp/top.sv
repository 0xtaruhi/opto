// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

primitive udp_latch(q, d, enable);
  output reg q;
  input d, enable;
  table
    ? 0 : ? : -;
    0 1 : ? : 0;
    1 1 : ? : 1;
  endtable
endprimitive

module top(
  input  logic d,
  input  logic enable,
  output logic q
);
  udp_latch u_latch(q, d, enable);
endmodule
