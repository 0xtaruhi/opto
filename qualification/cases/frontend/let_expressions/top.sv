// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

interface helper_if;
    logic [5:0] data;
    let adjust(logic [5:0] value) = value ^ 6'b10_0101;

    function automatic logic [5:0] adjusted();
        adjusted = adjust(data);
    endfunction
endinterface

module top(
    input logic clk,
    input logic signed [3:0] a,
    input logic [5:0] b,
    input logic select,
    input logic enable,
    output logic [5:0] y,
    output logic [3:0] q
);
    helper_if bus();
    let step(logic signed [3:0] value) = value + 4'sd1;
    let nested = step(a) + 4'sd1;

    assign bus.data = b;

    always_comb begin
        let choose(logic pick, logic signed [5:0] first, logic [5:0] second) =
            pick ? first : second;
        y = choose(select, nested, bus.adjusted());
    end

    always_ff @(posedge clk) begin
        if (enable)
            q <= nested;
    end
endmodule
