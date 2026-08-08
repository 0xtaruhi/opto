// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top (
  input  logic        valid,
  input  logic [3:0]  unsigned_dividend,
  input  logic [3:0]  unsigned_divisor,
  input  logic signed [3:0] signed_dividend,
  input  logic signed [3:0] signed_divisor,
  output logic [3:0]  unsigned_quotient,
  output logic [3:0]  unsigned_remainder,
  output logic signed [3:0] signed_quotient,
  output logic signed [3:0] signed_remainder
);
  always_comb begin
    unsigned_quotient = '0;
    unsigned_remainder = '0;
    signed_quotient = '0;
    signed_remainder = '0;
    if (valid && unsigned_divisor != 0 && signed_divisor != 0) begin
      unsigned_quotient = unsigned_dividend / unsigned_divisor;
      unsigned_remainder = unsigned_dividend % unsigned_divisor;
      signed_quotient = signed_dividend / signed_divisor;
      signed_remainder = signed_dividend % signed_divisor;
    end
  end
endmodule
