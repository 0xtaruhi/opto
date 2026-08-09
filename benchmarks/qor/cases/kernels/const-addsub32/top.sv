// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top (
    input  logic [31:0] a,
    output logic [31:0] sum,
    output logic [31:0] difference,
    output logic [31:0] reverse_difference
);
    assign sum                = a + 32'd12345;
    assign difference         = a - 32'd12345;
    assign reverse_difference = 32'd12345 - a;
endmodule
