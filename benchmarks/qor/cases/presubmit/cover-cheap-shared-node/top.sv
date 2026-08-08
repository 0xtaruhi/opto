// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top (
    input  logic [31:0] a,
    input  logic [31:0] b,
    input  logic [31:0] c,
    input  logic [31:0] d,
    input  logic [31:0] e,
    output logic [31:0] y0,
    output logic [31:0] y1
);
    logic [31:0] shared;

    assign shared = a & b & c;
    assign y0 = shared & d;
    assign y1 = shared & e;
endmodule
