// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top(
  input  [7:0] data,
  input        invert,
  output [7:0] y
);
  assign y = invert ? ~data : data;
endmodule
