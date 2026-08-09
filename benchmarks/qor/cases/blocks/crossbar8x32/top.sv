// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top (
  input  logic [255:0] inputs,
  input  logic [23:0]  select,
  output logic [255:0] outputs
);
  always_comb begin
    for (int output_index = 0; output_index < 8; output_index++) begin
      outputs[output_index*32 +: 32] =
        inputs[select[output_index*3 +: 3]*32 +: 32];
    end
  end
endmodule
