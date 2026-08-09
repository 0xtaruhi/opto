// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

package packed_generate_pkg;
  typedef struct packed {
    logic [7:0] payload;
    logic       valid;
  } lane_t;
endpackage

module lane_module #(parameter int LANE_VALUE = 0) (
  input  packed_generate_pkg::lane_t input_lane,
  output packed_generate_pkg::lane_t output_lane
);
  assign output_lane.payload = input_lane.payload ^ LANE_VALUE[7:0];
  assign output_lane.valid = input_lane.valid;
endmodule

module top (
  input  packed_generate_pkg::lane_t inputs [4],
  output packed_generate_pkg::lane_t outputs [4]
);
  for (genvar lane_index = 0; lane_index < 4; lane_index++) begin : generated_lanes
    lane_module #(.LANE_VALUE(lane_index)) u_lane (
      .input_lane(inputs[lane_index]),
      .output_lane(outputs[lane_index])
    );
  end
endmodule
