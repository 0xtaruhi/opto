// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top(
    .data({high, low}),
    .select(select),
    .y({upper[0:3], lower[7:4]})
);
    input [3:0] high;
    input [0:3] low;
    input select;
    output [0:3] upper;
    output [7:0] lower;

    assign upper = select ? low : high;
    assign lower = {high, low};
endmodule
