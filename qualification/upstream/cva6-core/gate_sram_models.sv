// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module cva6_gate_sram_1rw (
    input  logic        clk,
    input  logic        cs,
    input  logic        we,
    input  logic [7:0]  addr,
    input  logic [25:0] wdata,
    output logic [25:0] rdata
);
    logic [25:0] memory [256];

    always_ff @(posedge clk) begin
        if (cs) begin
            if (we) begin
                memory[addr] <= wdata;
            end else begin
                rdata <= memory[addr];
            end
        end
    end
endmodule

module cva6_gate_sram_wbyteenable_1rw (
    input  logic         clk,
    input  logic         cs,
    input  logic         we,
    input  logic [8:0]   addr,
    input  logic [127:0] wdata,
    input  logic [15:0]  wbyteenable,
    output logic [127:0] rdata
);
    logic [127:0] memory [512];

    always_ff @(posedge clk) begin
        if (cs) begin
            if (we) begin
                for (int byte_index = 0; byte_index < 16; byte_index++) begin
                    if (wbyteenable[byte_index]) begin
                        memory[addr][byte_index*8 +: 8] <= wdata[byte_index*8 +: 8];
                    end
                end
            end else begin
                rdata <= memory[addr];
            end
        end
    end
endmodule

bind hpdcache_sram_1rw cva6_gate_sram_1rw gate_model (
    .clk,
    .cs,
    .we,
    .addr,
    .wdata,
    .rdata
);

bind hpdcache_sram_wbyteenable_1rw cva6_gate_sram_wbyteenable_1rw gate_model (
    .clk,
    .cs,
    .we,
    .addr,
    .wdata,
    .wbyteenable,
    .rdata
);
