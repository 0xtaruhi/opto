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
  wire dynamic_clock = clocks[index];

  always_ff @(posedge clocks[2]) begin
    if (static_enable)
      static_value <= static_data;
  end

  always_ff @(negedge dynamic_clock) begin
    if (dynamic_enable_a || dynamic_enable_b)
      dynamic_value <= dynamic_data;
  end
endmodule
