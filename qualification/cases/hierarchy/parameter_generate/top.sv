// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module reduce_lane #(parameter int WIDTH = 8) (
  input  logic [WIDTH-1:0] data,
  output logic             parity
);
  assign parity = ^data;
endmodule

module top #(parameter int LANES = 4, WIDTH = 8) (
  input  logic [LANES*WIDTH-1:0] data,
  output logic [LANES-1:0]       parity
);
  for (genvar lane = 0; lane < LANES; lane++) begin : lanes
    reduce_lane #(.WIDTH(WIDTH)) reducer (
      .data(data[lane*WIDTH +: WIDTH]),
      .parity(parity[lane])
    );
  end
endmodule
