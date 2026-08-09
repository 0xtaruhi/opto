// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top (
    input  logic [31:0] a,
    input  logic [31:0] b,
    input  logic [31:0] c,
    input  logic [31:0] d,
    input  logic [31:0] e,
    input  logic [31:0] f,
    input  logic [31:0] g,
    input  logic [31:0] h,
    output logic [31:0] y0,
    output logic [31:0] y1
);
    logic [31:0] shared;

    assign shared = a ^ b;
    assign y0 = shared & ~a & c & d & e & f & g;
    assign y1 = shared & ~b & d & e & f & g & h;
endmodule
