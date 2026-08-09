// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module BUF_X1(input A, output Y);
  assign Y = A;
endmodule

module INV_X1(input A, output Y);
  assign Y = ~A;
endmodule

module AND2_X1(input A, B, output Y);
  assign Y = A & B;
endmodule

module OR2_X1(input A, B, output Y);
  assign Y = A | B;
endmodule

module NAND2_X1(input A, B, output Y);
  assign Y = ~(A & B);
endmodule

module NOR2_X1(input A, B, output Y);
  assign Y = ~(A | B);
endmodule

module XOR2_X1(input A, B, output Y);
  assign Y = A ^ B;
endmodule

module XNOR2_X1(input A, B, output Y);
  assign Y = ~(A ^ B);
endmodule

module MUX2_X1(input S, A, B, output Y);
  assign Y = S ? B : A;
endmodule

module DFF_X1(input CLK, D, output reg Q);
  always @(posedge CLK)
    Q <= D;
endmodule

module DFFSR_X1(input CLK, D, CLR, PRE, output reg Q);
  always @(posedge CLK or posedge CLR or posedge PRE)
    if (CLR)
      Q <= 1'b0;
    else if (PRE)
      Q <= 1'b1;
    else
      Q <= D;
endmodule

module QUAL_DFFN_X1(input CLK, D, output reg Q);
  always @(negedge CLK)
    Q <= D;
endmodule

module QUAL_DFFRN_X1(input CLK, D, RESET, output reg Q);
  always @(posedge CLK or negedge RESET)
    if (!RESET)
      Q <= 1'b0;
    else
      Q <= D;
endmodule

module QUAL_DFFR_X1(input CLK, D, RESET, output reg Q);
  always @(posedge CLK or posedge RESET)
    if (RESET)
      Q <= 1'b0;
    else
      Q <= D;
endmodule

module QUAL_DFFP_X1(input CLK, D, RESET, output reg Q);
  always @(posedge CLK or posedge RESET)
    if (RESET)
      Q <= 1'b1;
    else
      Q <= D;
endmodule

module QUAL_DFFPN_X1(input CLK, D, RESET, output reg Q);
  always @(posedge CLK or negedge RESET)
    if (!RESET)
      Q <= 1'b1;
    else
      Q <= D;
endmodule

module QUAL_DFFE_X1(input CLK, D, EN, output reg Q);
  always @(posedge CLK)
    if (EN)
      Q <= D;
endmodule

module QUAL_DFFER_X1(input CLK, D, EN, RESET, output reg Q);
  always @(posedge CLK or posedge RESET)
    if (RESET)
      Q <= 1'b0;
    else if (EN)
      Q <= D;
endmodule

module QUAL_DFFERN_X1(input CLK, D, EN, RESET, output reg Q);
  always @(posedge CLK or negedge RESET)
    if (!RESET)
      Q <= 1'b0;
    else if (EN)
      Q <= D;
endmodule

module QUAL_DFFEP_X1(input CLK, D, EN, RESET, output reg Q);
  always @(posedge CLK or posedge RESET)
    if (RESET)
      Q <= 1'b1;
    else if (EN)
      Q <= D;
endmodule

module QUAL_DFFEPN_X1(input CLK, D, EN, RESET, output reg Q);
  always @(posedge CLK or negedge RESET)
    if (!RESET)
      Q <= 1'b1;
    else if (EN)
      Q <= D;
endmodule

module QUAL_DFFNP_X1(input CLK, D, RESET, output reg Q);
  always @(negedge CLK or posedge RESET)
    if (RESET)
      Q <= 1'b1;
    else
      Q <= D;
endmodule

module QUAL_DFFNRN_X1(input CLK, D, RESET, output reg Q);
  always @(negedge CLK or negedge RESET)
    if (!RESET)
      Q <= 1'b0;
    else
      Q <= D;
endmodule

module QUAL_LATCH_X1(input D, EN, output reg Q);
  always @(*)
    if (EN)
      Q = D;
endmodule

module QUAL_LATCHR_X1(input D, EN, RESET, output reg Q);
  always @(*)
    if (RESET)
      Q = 1'b0;
    else if (EN)
      Q = D;
endmodule

module QUAL_LATCHRN_X1(input D, EN, RESET, output reg Q);
  always @(*)
    if (!RESET)
      Q = 1'b0;
    else if (EN)
      Q = D;
endmodule
