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
