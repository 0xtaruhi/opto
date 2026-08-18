// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top(
  input  logic a,
  input  logic b,
  input  logic select,
  output logic y
);
  tri driven;

  notif1 drive_a(driven, a, select);
  notif0 drive_b(driven, b, select);

  assign y = driven;
endmodule
