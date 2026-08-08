// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top(a, b, y);
    input [3:0] a;
    input [3:0] b;
    output [3:0] y;

    assign y = a + b;
endmodule
