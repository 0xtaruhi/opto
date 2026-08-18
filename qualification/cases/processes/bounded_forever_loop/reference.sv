// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top(
  input  logic [3:0] stop,
  input  logic [3:0] skip,
  output logic [3:0] mask,
  output logic [2:0] count
);
  always_comb begin
    count = 3'd0;
    mask = 4'b0000;
    if (!stop[0]) begin
      count = 3'd1;
      mask[0] = ~skip[0];
      if (!stop[1]) begin
        count = 3'd2;
        mask[1] = ~skip[1];
        if (!stop[2]) begin
          count = 3'd3;
          mask[2] = ~skip[2];
          if (!stop[3]) begin
            count = 3'd4;
            mask[3] = ~skip[3];
          end
        end
      end
    end
  end
endmodule
