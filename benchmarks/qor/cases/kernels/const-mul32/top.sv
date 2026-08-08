// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top (
    input  logic        [31:0] unsigned_value,
    input  logic signed [31:0] signed_value,
    output logic        [31:0] sparse_product,
    output logic        [31:0] dense_product,
    output logic signed [31:0] signed_product,
    output logic        [31:0] power_of_two_product
);
    assign sparse_product       = unsigned_value * 32'd12345;
    assign dense_product        = unsigned_value * 32'h6db6db6d;
    assign signed_product       = signed_value * -32'sd12345;
    assign power_of_two_product = unsigned_value * 32'd4096;
endmodule
