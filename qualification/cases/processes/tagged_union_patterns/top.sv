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
  typedef union tagged {
    void Empty;
    logic [3:0] Small;
    logic [7:0] Large;
  } unpacked_value_t;

  typedef union tagged packed {
    void Empty;
    logic [3:0] Small;
    logic [7:0] Large;
  } packed_value_t;

  unpacked_value_t unpacked_value;
  packed_value_t packed_value;

  always_comb begin
    case (kind)
      2'b00: unpacked_value = tagged Empty;
      2'b01: unpacked_value = tagged Small small_data;
      default: unpacked_value = tagged Large large_data;
    endcase
    packed_value = kind[0]
        ? tagged Large large_data
        : tagged Small small_data;

    unpacked_decoded = fallback;
    case (unpacked_value) matches
      tagged Empty: unpacked_decoded = 8'he0;
      tagged Small .small_value: unpacked_decoded = {4'h1, small_value};
      tagged Large .large_value: unpacked_decoded = large_value;
    endcase

    if (packed_value matches tagged Small .small_value &&& small_value[0]) begin
      packed_decoded = {4'h2, small_value};
    end else if (packed_value matches tagged Large .large_value) begin
      packed_decoded = large_value;
    end else begin
      packed_decoded = fallback;
    end

    conditional_decoded = unpacked_value matches tagged Small .small_value
        ? {4'h0, small_value}
        : fallback;
  end
endmodule
