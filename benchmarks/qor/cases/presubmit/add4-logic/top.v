// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top(a, b, mask, select, y);
    input [3:0] a;
    input [3:0] b;
    input [3:0] mask;
    input select;
    output [3:0] y;

    wire [3:0] sum = a + b;
    wire [3:0] logic_value = (a & mask) | (b ^ mask);
    assign y = select ? sum : logic_value;
endmodule
