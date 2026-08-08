// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top (
    input  wire [1:0] sel,
    input  wire       a,
    input  wire       b,
    input  wire       c,
    output reg        y
);
    always @* begin
        case (sel)
            2'b00: y = a;
            2'b01,
            2'b10: y = b;
            default: y = c;
        endcase
    end
endmodule
