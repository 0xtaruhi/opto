// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top (
  input  logic [63:0] request,
  output logic [5:0]  index,
  output logic        valid
);
  always_comb begin
    index = 6'd0;
    valid = 1'b0;
    for (int bit_index = 0; bit_index < 64; bit_index++) begin
      if (request[bit_index]) begin
        index = bit_index[5:0];
        valid = 1'b1;
      end
    end
  end
endmodule
