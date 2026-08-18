// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top (
  input  logic [1:0] data,
  input  logic [1:0] fallback,
  input  logic [1:0] enable,
  output logic [1:0] observed
);
  wire [1:0] bus;

  assign bus[0] = enable[0] ? data[0] : 1'bz;
  assign bus[0] = enable[0] ? 1'bz : fallback[0];
  assign bus[1] = enable[1] ? 1'bz : data[1];
  assign bus[1] = enable[1] ? fallback[1] : 1'bz;
  assign observed = ~bus;
endmodule
