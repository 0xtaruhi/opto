// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top(
  input  logic [1:0] kind,
  input  logic [3:0] small_data,
  input  logic [7:0] large_data,
  input  logic [7:0] fallback,
  output logic [7:0] unpacked_decoded,
  output logic [7:0] packed_decoded,
  output logic [7:0] conditional_decoded
);
  always_comb begin
    case (kind)
      2'b00: unpacked_decoded = 8'he0;
      2'b01: unpacked_decoded = {4'h1, small_data};
      default: unpacked_decoded = large_data;
    endcase

    if (!kind[0] && small_data[0]) begin
      packed_decoded = {4'h2, small_data};
    end else if (kind[0]) begin
      packed_decoded = large_data;
    end else begin
      packed_decoded = fallback;
    end

    conditional_decoded = kind == 2'b01
        ? {4'h0, small_data}
        : fallback;
  end
endmodule
