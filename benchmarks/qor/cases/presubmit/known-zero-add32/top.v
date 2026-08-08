// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top(a, b, sum);
    input [11:0] a;
    input [11:0] b;
    output [31:0] sum;

    wire [31:0] wide_a = {20'b0, a};
    wire [31:0] wide_b = {20'b0, b};

    assign sum = wide_a + wide_b;
endmodule
