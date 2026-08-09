// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top (
    input wire [255:0] a,
    input wire [255:0] b,
    output wire [255:0] y
);
    assign y = a + b;
endmodule
