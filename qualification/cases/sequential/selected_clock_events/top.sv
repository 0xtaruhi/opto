// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top(
  input  logic [3:0] clocks,
  input  logic [1:0] index,
  input  logic       static_enable,
  input  logic       dynamic_enable_a,
  input  logic       dynamic_enable_b,
  input  logic       static_data,
  input  logic       dynamic_data,
  output logic       static_value,
  output logic       dynamic_value
);
  always_ff @(posedge clocks[2] iff static_enable)
    static_value <= static_data;

  always_ff @(negedge clocks[index] iff dynamic_enable_a or
              negedge clocks[index] iff dynamic_enable_b)
    dynamic_value <= dynamic_data;
endmodule
