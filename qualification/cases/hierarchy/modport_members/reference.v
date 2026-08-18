// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top(
  input  [7:0] data,
  input        mask,
  input        drive,
  output [7:0] imported_result,
  output [7:0] exported_result,
  output       inout_observed
);
  assign imported_result = data ^ {8{mask}};
  assign exported_result = ~data;
  assign inout_observed = drive;
endmodule
