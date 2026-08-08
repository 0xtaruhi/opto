// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top (
    input  logic [63:0] a,
    input  logic [63:0] b,
    output logic [63:0] y,
    output logic [63:0] yn
);
    logic [63:0] shared;

    assign shared = a & b;
    assign y = shared;
    assign yn = ~shared;
endmodule
