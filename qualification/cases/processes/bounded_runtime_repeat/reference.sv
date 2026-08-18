// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top(
  input  logic [3:0]        count,
  input  logic              seed,
  input  logic signed [4:0] signed_count,
  output logic [4:0]        iterations,
  output logic              parity,
  output logic [3:0]        signed_iterations
);
  always_comb begin
    iterations = {1'b0, count};
    parity = seed ^ count[0];
    signed_iterations = signed_count > 0 ? signed_count[3:0] : 4'd0;
  end
endmodule
