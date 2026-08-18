// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top(
  input  logic [3:0] data,
  input  logic [3:0] keep,
  input  logic [3:0] remaining,
  input  logic [4:0] post_data,
  input  logic       bit_value,
  output logic [3:0] while_mask,
  output logic [2:0] while_count,
  output logic [2:0] do_count,
  output logic [2:0] for_count,
  output logic [2:0] forever_count,
  output logic [2:0] function_count,
  output logic [2:0] nested_count,
  output logic [4:0] post_overwrite,
  output logic [4:0] post_compound,
  output logic [4:0] post_partial
);
  integer while_index;
  integer while_limit;
  integer do_index;
  integer do_remaining;
  integer for_index;
  integer for_limit;
  integer forever_index;
  integer forever_limit;
  integer nested_outer;
  integer nested_inner;
  integer overwrite_index;
  integer compound_index;
  integer partial_index;

  function automatic logic [2:0] bounded_count(
    input logic [3:0] left
  );
    integer index = 0;
    while (index < 4 && left != 0) begin
      index++;
      left--;
    end
    return index[2:0];
  endfunction

  always_comb begin
    while_index = 0;
    while_limit = 4;
    while_mask = 4'b0000;
    while (while_index < while_limit && keep[while_index]) begin
      while_mask[while_index] = data[while_index];
      while_index++;
    end
    while_count = while_index[2:0];
  end

  always_comb begin
    do_index = 0;
    do_remaining = 4;
    do begin
      do_index++;
      do_remaining--;
    end while (do_remaining > 0 && keep[do_index]);
    do_count = do_index[2:0];
  end

  always_comb begin
    for_index = 0;
    for_limit = 4;
    for (; for_index < for_limit; for_index++) begin
      if (!keep[for_index]) break;
    end
    for_count = for_index[2:0];
  end

  always_comb begin
    forever_index = 0;
    forever_limit = 4;
    forever begin
      if (forever_index == forever_limit || !keep[forever_index]) break;
      forever_index++;
    end
    forever_count = forever_index[2:0];
  end

  always_comb function_count = bounded_count(remaining);

  always_comb begin
    nested_outer = 0;
    while (nested_outer < 4 && keep[nested_outer]) begin
      nested_inner = 0;
      while (nested_inner < 1) begin
        nested_outer++;
        nested_inner++;
      end
    end
    nested_count = nested_outer[2:0];
  end

  always_comb begin
    overwrite_index = 0;
    while (overwrite_index < 4) overwrite_index++;
    overwrite_index = post_data;
    post_overwrite = overwrite_index[4:0];
  end

  always_comb begin
    compound_index = 0;
    while (compound_index < 4) compound_index++;
    compound_index += post_data;
    post_compound = compound_index[4:0];
  end

  always_comb begin
    partial_index = 0;
    while (partial_index < 4) partial_index++;
    partial_index[0] = bit_value;
    post_partial = partial_index[4:0];
  end
endmodule
