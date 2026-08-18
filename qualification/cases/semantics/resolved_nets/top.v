// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top (
  input  [3:0] a,
  input  [3:0] b,
  input  [3:0] c,
  input  [3:0] d,
  input        enable,
  output wand [3:0] and_result,
  output wor  [3:0] or_result,
  output wand [3:0] and_tristate,
  output wor  [3:0] or_tristate
);
  assign and_result = a;
  assign and_result = b;
  assign or_result = c;
  assign or_result = d;
  assign and_tristate = a;
  assign or_tristate = c;
  genvar bit_index;
  generate
    for (bit_index = 0; bit_index < 4; bit_index = bit_index + 1) begin : gen_tristate
      bufif1 (and_tristate[bit_index], b[bit_index], enable);
      bufif0 (or_tristate[bit_index], d[bit_index], enable);
    end
  endgenerate
endmodule
