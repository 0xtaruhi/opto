// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top(
  input  clock,
  input  pos_enable,
  input  neg_enable,
  input  data,
  output q
);
  reg rising_state;
  reg falling_state;
  wire selected_enable = clock ? pos_enable : neg_enable;
  wire next_state = selected_enable ? data : q;

  always @(posedge clock)
    rising_state <= next_state;

  always @(negedge clock)
    falling_state <= next_state;

  assign q = clock ? rising_state : falling_state;
endmodule
