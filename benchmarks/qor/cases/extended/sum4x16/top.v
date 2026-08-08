// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top(a, b, c, d, sum);
    input [15:0] a;
    input [15:0] b;
    input [15:0] c;
    input [15:0] d;
    output [17:0] sum;

    assign sum = {2'b0, a} + {2'b0, b} + {2'b0, c} + {2'b0, d};
endmodule
