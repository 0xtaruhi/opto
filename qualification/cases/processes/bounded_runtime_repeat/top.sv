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
  logic [3:0] mutable_count;

  always_comb begin
    mutable_count = count;
    iterations = 0;
    parity = seed;
    repeat (mutable_count) begin
      iterations++;
      parity = ~parity;
      mutable_count = 0;
    end
  end

  always_comb begin
    signed_iterations = 0;
    repeat (signed_count) signed_iterations++;
  end
endmodule
