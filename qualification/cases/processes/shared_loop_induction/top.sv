// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top(
  input  logic       clock,
  input  logic [3:0] data,
  output logic [1:0] registered,
  output logic [3:0] combinational,
  output logic [3:0] fully_selected
);
  integer index;

  always_ff @(posedge clock) begin
    for (index = 0; index < 2; index++) begin
      registered[index] <= data[index];
    end
  end

  always_comb begin
    combinational = '0;
    for (index = 0; index < 4; index++) begin
      combinational[index] = data[index];
    end
  end

  always_comb begin
    for (index = 0; index < 4; index++) begin
      fully_selected[index] = data[index];
    end
  end
endmodule
