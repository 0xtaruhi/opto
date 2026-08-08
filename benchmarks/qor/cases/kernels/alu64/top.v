// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top (
    input wire [63:0] lhs,
    input wire [63:0] rhs,
    input wire [3:0] operation,
    output reg [63:0] result,
    output wire zero,
    output wire negative,
    output reg carry,
    output reg overflow
);
    wire [64:0] add_result = {1'b0, lhs} + {1'b0, rhs};
    wire [64:0] sub_result = {1'b0, lhs} - {1'b0, rhs};
    wire [5:0] shift_amount = rhs[5:0];
    wire [5:0] inverse_shift_amount = 6'd0 - shift_amount;
    wire signed [63:0] signed_lhs = lhs;
    wire signed [63:0] signed_rhs = rhs;

    wire [63:0] shift_left = lhs << shift_amount;
    wire [63:0] shift_right = lhs >> shift_amount;
    wire [63:0] shift_arithmetic = signed_lhs >>> shift_amount;
    wire [63:0] rotate_left =
        (lhs << shift_amount) | (lhs >> inverse_shift_amount);
    wire [63:0] rotate_right =
        (lhs >> shift_amount) | (lhs << inverse_shift_amount);

    assign zero = ~|result;
    assign negative = result[63];

    always @* begin
        result = 64'd0;
        carry = 1'b0;
        overflow = 1'b0;

        case (operation)
            4'd0: begin
                result = add_result[63:0];
                carry = add_result[64];
                overflow = (~(lhs[63] ^ rhs[63]))
                    & (lhs[63] ^ add_result[63]);
            end
            4'd1: begin
                result = sub_result[63:0];
                carry = ~sub_result[64];
                overflow = (lhs[63] ^ rhs[63])
                    & (lhs[63] ^ sub_result[63]);
            end
            4'd2: result = lhs & rhs;
            4'd3: result = lhs | rhs;
            4'd4: result = lhs ^ rhs;
            4'd5: result = ~(lhs | rhs);
            4'd6: result = shift_left;
            4'd7: result = shift_right;
            4'd8: result = shift_arithmetic;
            4'd9: result = {63'd0, signed_lhs < signed_rhs};
            4'd10: result = {63'd0, lhs < rhs};
            4'd11: result = rotate_left;
            4'd12: result = rotate_right;
            4'd13: result = signed_lhs < signed_rhs ? lhs : rhs;
            4'd14: result = signed_lhs < signed_rhs ? rhs : lhs;
            4'd15: result = {
                lhs[7:0], lhs[15:8], lhs[23:16], lhs[31:24],
                lhs[39:32], lhs[47:40], lhs[55:48], lhs[63:56]
            };
            default: result = 64'd0;
        endcase
    end
endmodule
