// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top(
  input  logic [3:0] keep,
  input  logic [3:0] skip,
  output logic [2:0] while_count,
  output logic [2:0] do_count,
  output logic [2:0] for_count,
  output logic [3:0] while_mask,
  output logic [3:0] do_mask,
  output logic [3:0] for_mask
);
  always_comb begin
    while_count = 3'd0;
    while_mask = 4'b0000;
    if (keep[0]) begin
      while_count = 3'd1;
      while_mask[0] = 1'b1;
      if (keep[1]) begin
        while_count = 3'd2;
        while_mask[1] = 1'b1;
        if (keep[2]) begin
          while_count = 3'd3;
          while_mask[2] = 1'b1;
          if (keep[3]) begin
            while_count = 3'd4;
            while_mask[3] = 1'b1;
          end
        end
      end
    end

    do_count = 3'd1;
    do_mask = 4'b0001;
    if (keep[1]) begin
      do_count = 3'd2;
      do_mask[1] = 1'b1;
      if (keep[2]) begin
        do_count = 3'd3;
        do_mask[2] = 1'b1;
        if (keep[3]) begin
          do_count = 3'd4;
          do_mask[3] = 1'b1;
        end
      end
    end

    for_count = 3'd0;
    for_mask = 4'b0000;
    if (keep[0]) begin
      for_count = 3'd1;
      for_mask[0] = ~skip[0];
      if (keep[1]) begin
        for_count = 3'd2;
        for_mask[1] = ~skip[1];
        if (keep[2]) begin
          for_count = 3'd3;
          for_mask[2] = ~skip[2];
          if (keep[3]) begin
            for_count = 3'd4;
            for_mask[3] = ~skip[3];
          end
        end
      end
    end
  end
endmodule
