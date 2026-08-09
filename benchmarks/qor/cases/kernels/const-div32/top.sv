// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top (
    input  logic        [31:0] unsigned_value,
    input  logic signed [31:0] signed_value,
    output logic        [31:0] quotient,
    output logic        [31:0] remainder,
    output logic signed [31:0] signed_quotient,
    output logic signed [31:0] signed_remainder,
    output logic        [31:0] power_of_two_quotient
);
    assign quotient              = unsigned_value / 32'd12345;
    assign remainder             = unsigned_value % 32'd12345;
    assign signed_quotient       = signed_value / -32'sd12345;
    assign signed_remainder      = signed_value % -32'sd12345;
    assign power_of_two_quotient = unsigned_value / 32'd4096;
endmodule
