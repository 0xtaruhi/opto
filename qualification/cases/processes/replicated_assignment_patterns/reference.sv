// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top(
  input  logic [3:0] a,
  input  logic [3:0] b,
  output logic [3:0] value0,
  output logic [3:0] value1,
  output logic [3:0] value2,
  output logic [3:0] value3,
  output logic [15:0] packed_bits,
  output logic [3:0] final_state
);
  always_comb begin
    value0 = 4'd0;
    value1 = 4'd1;
    value2 = 4'd2;
    value3 = 4'd3;
    packed_bits = {a, b, a, b};
    final_state = 4'd4;
  end
endmodule
