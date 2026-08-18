// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top(
    input [7:0] data,
    input select,
    output [7:0] y
);
    wire [3:0] high = data[7:4];
    wire [3:0] low = data[3:0];

    assign y = {select ? low : high, high};
endmodule
