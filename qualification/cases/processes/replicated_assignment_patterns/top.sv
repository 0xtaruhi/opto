// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top(
  input  logic [3:0] a,
  input  logic [3:0] b,
  output logic [3:0] value0,
  output logic [3:0] value1,
  output logic [3:0] value2,
  output logic [3:0] value3,
  output logic [15:0] packed_bits,
  output logic [3:0] final_state
);
  typedef struct packed {
    logic [3:0] first;
    logic [3:0] second;
    logic [3:0] third;
    logic [3:0] fourth;
  } packed_t;

  logic [3:0] values [0:3];
  packed_t packed_value;
  integer state;

  always_comb begin
    state = 0;
    values = '{2{state++, state++}};
    packed_value = '{2{a, b}};

    value0 = values[0];
    value1 = values[1];
    value2 = values[2];
    value3 = values[3];
    packed_bits = packed_value;
    final_state = state;
  end
endmodule
