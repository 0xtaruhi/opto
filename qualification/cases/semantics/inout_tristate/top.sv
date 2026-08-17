// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top (
  input  logic [1:0] data,
  input  logic [1:0] enable,
  inout  wire  [1:0] pad,
  output logic [1:0] observed
);
  assign pad[0] = enable[0] ? data[0] : 1'bz;
  assign pad[1] = enable[1] ? 1'bz : data[1];
  assign observed = pad;
endmodule
