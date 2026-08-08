// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top (
  input  logic [3:0] request,
  input  logic       enable,
  output logic [1:0] index,
  output logic       valid
);
  always_comb begin
    index = 2'b00;
    valid = 1'b0;
    if (enable) begin
      priority casez (request)
        4'b1???: begin index = 2'd3; valid = 1'b1; end
        4'b01??: begin index = 2'd2; valid = 1'b1; end
        4'b001?: begin index = 2'd1; valid = 1'b1; end
        4'b0001: begin index = 2'd0; valid = 1'b1; end
        default: begin index = 2'd0; valid = 1'b0; end
      endcase
    end
  end
endmodule
