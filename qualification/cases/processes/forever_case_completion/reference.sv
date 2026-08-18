// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top(
  input  logic [1:0] mode,
  output logic [3:0] mask,
  output logic [2:0] count
);
  always_comb begin
    case (mode)
      2'd0: begin
        mask = 4'b0001;
        count = 3'd1;
      end
      2'd1: begin
        mask = 4'b0011;
        count = 3'd2;
      end
      2'd2: begin
        mask = 4'b0111;
        count = 3'd3;
      end
      default: begin
        mask = 4'b1111;
        count = 3'd4;
      end
    endcase
  end
endmodule
