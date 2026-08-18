// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top(
  input  logic       select,
  input  logic [3:0] data,
  output logic [3:0] forever_value,
  output logic [3:0] while_value,
  output logic [3:0] disable_value
);
  always_comb begin
    forever_value = select ? data : ~data;
    while_value = select ? data + 4'd1 : data - 4'd1;
    disable_value = data ^ 4'h3;
  end
endmodule
