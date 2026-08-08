// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module child(input logic a, output logic y);
  assign y = ~a;
endmodule

module top(input logic [1:0] a, output logic [1:0] y);
  child left (.a(a[0]), .y(y[0]));
  child right (.a(a[1]), .y(y[1]));
endmodule
