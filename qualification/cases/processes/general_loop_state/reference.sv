// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top(
  input  logic [3:0] data,
  input  logic [3:0] keep,
  input  logic [3:0] remaining,
  input  logic [4:0] post_data,
  input  logic       bit_value,
  input  logic [3:0] hits,
  output logic [3:0] while_mask,
  output logic [2:0] while_count,
  output logic [2:0] do_count,
  output logic [2:0] for_count,
  output logic [2:0] forever_count,
  output logic [2:0] function_count,
  output logic [2:0] nested_count,
  output logic [4:0] post_overwrite,
  output logic [4:0] post_compound,
  output logic [4:0] post_partial,
  output logic       selected_data,
  output logic [2:0] match_position
);
  always_comb begin
    if (!keep[0]) begin
      while_count = 3'd0;
      while_mask = 4'b0000;
    end else if (!keep[1]) begin
      while_count = 3'd1;
      while_mask = data & 4'b0001;
    end else if (!keep[2]) begin
      while_count = 3'd2;
      while_mask = data & 4'b0011;
    end else if (!keep[3]) begin
      while_count = 3'd3;
      while_mask = data & 4'b0111;
    end else begin
      while_count = 3'd4;
      while_mask = data;
    end
    for_count = while_count;
    forever_count = while_count;
    nested_count = while_count;

    if (!keep[1]) do_count = 3'd1;
    else if (!keep[2]) do_count = 3'd2;
    else if (!keep[3]) do_count = 3'd3;
    else do_count = 3'd4;

    if (remaining >= 4) function_count = 3'd4;
    else function_count = remaining[2:0];

    post_overwrite = post_data;
    post_compound = 5'd4 + post_data;
    post_partial = bit_value ? 5'd5 : 5'd4;

    if (keep[0] && hits[0]) begin
      selected_data = data[0];
      match_position = 3'd0;
    end else if (keep[1] && hits[1]) begin
      selected_data = data[1];
      match_position = 3'd1;
    end else if (keep[2] && hits[2]) begin
      selected_data = data[2];
      match_position = 3'd2;
    end else if (keep[3] && hits[3]) begin
      selected_data = data[3];
      match_position = 3'd3;
    end else begin
      selected_data = bit_value;
      match_position = 3'd4;
    end
  end
endmodule
