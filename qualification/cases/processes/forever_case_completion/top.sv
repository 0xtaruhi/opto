// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top(
  input  logic [1:0] mode,
  output logic [3:0] mask,
  output logic [2:0] count
);
  always_comb begin
    integer i;
    i = 0;
    mask = 4'b0000;
    forever begin
      case (mode)
        2'd0: if (i >= 1) break;
        2'd1: if (i >= 2) break;
        2'd2: if (i >= 3) break;
        default: if (i >= 4) break;
      endcase
      mask[i] = 1'b1;
      i++;
    end
    count = i[2:0];
  end
endmodule
