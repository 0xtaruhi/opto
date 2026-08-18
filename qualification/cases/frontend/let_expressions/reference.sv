// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top(
    input logic clk,
    input logic signed [3:0] a,
    input logic [5:0] b,
    input logic select,
    input logic enable,
    output logic [5:0] y,
    output logic [3:0] q
);
    logic signed [3:0] nested;

    always_comb begin
        nested = (a + 4'sd1) + 4'sd1;
        y = select ? {{2{nested[3]}}, nested} : (b ^ 6'b10_0101);
    end

    always_ff @(posedge clk) begin
        if (enable)
            q <= nested;
    end
endmodule
