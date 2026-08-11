// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

`timescale 1ns/1ps

module ibex_smoke_tb;
    localparam logic [31:0] SIGNATURE_ADDRESS = 32'h0000_0104;
    localparam logic [31:0] EXPECTED_SIGNATURE = 32'd16;
    localparam int unsigned MAXIMUM_CYCLES = 500;

    logic clk_i = 1'b0;
    logic rst_ni = 1'b0;
    logic instr_req_o;
    logic instr_gnt_i;
    logic instr_rvalid_i;
    logic [31:0] instr_addr_o;
    logic [31:0] instr_rdata_i;
    logic data_req_o;
    logic data_gnt_i;
    logic data_rvalid_i;
    logic data_we_o;
    logic [3:0] data_be_o;
    logic [31:0] data_addr_o;
    logic [31:0] data_wdata_o;
    logic [31:0] data_rdata_i;
    logic [4:0] rf_raddr_a_o;
    logic [4:0] rf_raddr_b_o;
    logic [4:0] rf_waddr_wb_o;
    logic rf_we_wb_o;
    logic [31:0] rf_wdata_wb_ecc_o;
    logic [31:0] rf_rdata_a_ecc_i;
    logic [31:0] rf_rdata_b_ecc_i;
    logic [31:0] register_file [0:31];
    logic [31:0] data_memory_word;
    int unsigned cycles;
    integer register_index;

    assign instr_gnt_i = instr_req_o;
    assign data_gnt_i = data_req_o;
    assign rf_rdata_a_ecc_i = rf_raddr_a_o == 0 ? 32'b0 : register_file[rf_raddr_a_o];
    assign rf_rdata_b_ecc_i = rf_raddr_b_o == 0 ? 32'b0 : register_file[rf_raddr_b_o];

    ibex_core dut (
        .clk_i(clk_i),
        .rst_ni(rst_ni),
        .hart_id_i(32'b0),
        .boot_addr_i(32'b0),
        .instr_req_o(instr_req_o),
        .instr_gnt_i(instr_gnt_i),
        .instr_rvalid_i(instr_rvalid_i),
        .instr_addr_o(instr_addr_o),
        .instr_rdata_i(instr_rdata_i),
        .instr_err_i(1'b0),
        .data_req_o(data_req_o),
        .data_gnt_i(data_gnt_i),
        .data_rvalid_i(data_rvalid_i),
        .data_we_o(data_we_o),
        .data_be_o(data_be_o),
        .data_addr_o(data_addr_o),
        .data_wdata_o(data_wdata_o),
        .data_rdata_i(data_rdata_i),
        .data_err_i(1'b0),
        .rf_raddr_a_o(rf_raddr_a_o),
        .rf_raddr_b_o(rf_raddr_b_o),
        .rf_waddr_wb_o(rf_waddr_wb_o),
        .rf_we_wb_o(rf_we_wb_o),
        .rf_wdata_wb_ecc_o(rf_wdata_wb_ecc_o),
        .rf_rdata_a_ecc_i(rf_rdata_a_ecc_i),
        .rf_rdata_b_ecc_i(rf_rdata_b_ecc_i),
        .ic_tag_rdata_i('{default:'0}),
        .ic_data_rdata_i('{default:'0}),
        .ic_scr_key_valid_i(1'b0),
        .irq_software_i(1'b0),
        .irq_timer_i(1'b0),
        .irq_external_i(1'b0),
        .irq_fast_i(15'b0),
        .irq_nm_i(1'b0),
        .debug_req_i(1'b0),
        .fetch_enable_i(4'b0101),
        .mcounteren_writable_i(4'b0101)
    );

    always #5 clk_i = ~clk_i;

    initial begin
        repeat (8) @(posedge clk_i);
        rst_ni = 1'b1;
    end

    function automatic logic [31:0] instruction_word(input logic [31:0] address);
        case (address)
            32'h0000_0080: instruction_word = 32'h0050_0093; // addi x1, x0, 5
            32'h0000_0084: instruction_word = 32'h0070_0113; // addi x2, x0, 7
            32'h0000_0088: instruction_word = 32'h0020_81b3; // add  x3, x1, x2
            32'h0000_008c: instruction_word = 32'h1030_2023; // sw   x3, 256(x0)
            32'h0000_0090: instruction_word = 32'h1000_2203; // lw   x4, 256(x0)
            32'h0000_0094: instruction_word = 32'h0032_0213; // addi x4, x4, 3
            32'h0000_0098: instruction_word = 32'h0012_42b3; // xor  x5, x4, x1
            32'h0000_009c: instruction_word = 32'h0022_9463; // bne  x5, x2, +8
            32'h0000_00a0: instruction_word = 32'h0010_0313; // skipped on success
            32'h0000_00a4: instruction_word = 32'h0022_8313; // addi x6, x5, 2
            32'h0000_00a8: instruction_word = 32'h0213_43b3; // div  x7, x6, x1
            32'h0000_00ac: instruction_word = 32'h0213_6433; // rem  x8, x6, x1
            32'h0000_00b0: instruction_word = 32'h0083_84b3; // add  x9, x7, x8
            32'h0000_00b4: instruction_word = 32'h0093_0533; // add  x10, x6, x9
            32'h0000_00b8: instruction_word = 32'h10a0_2223; // sw   x10, 260(x0)
            32'h0000_00bc: instruction_word = 32'h0000_006f; // jal  x0, 0
            default: instruction_word = 32'h0000_0013; // addi x0, x0, 0
        endcase
    endfunction

    function automatic logic [31:0] memory_read(input logic [31:0] address);
        case (address)
            32'h0000_0100: memory_read = data_memory_word;
            default: memory_read = 32'b0;
        endcase
    endfunction

    always_ff @(posedge clk_i or negedge rst_ni) begin
        if (!rst_ni) begin
            instr_rvalid_i <= 1'b0;
            instr_rdata_i <= 32'b0;
        end else begin
            instr_rvalid_i <= instr_req_o;
            if (instr_req_o) instr_rdata_i <= instruction_word(instr_addr_o);
        end
    end

    always_ff @(posedge clk_i or negedge rst_ni) begin
        if (!rst_ni) begin
            data_rvalid_i <= 1'b0;
            data_rdata_i <= 32'b0;
            data_memory_word <= 32'b0;
        end else begin
            data_rvalid_i <= data_req_o;
            if (data_req_o && !data_we_o) data_rdata_i <= memory_read(data_addr_o);
            if (data_req_o && data_we_o) begin
                if (data_addr_o == 32'h0000_0100) begin
                    if (data_be_o[0]) data_memory_word[7:0] <= data_wdata_o[7:0];
                    if (data_be_o[1]) data_memory_word[15:8] <= data_wdata_o[15:8];
                    if (data_be_o[2]) data_memory_word[23:16] <= data_wdata_o[23:16];
                    if (data_be_o[3]) data_memory_word[31:24] <= data_wdata_o[31:24];
                end
                if (data_addr_o == SIGNATURE_ADDRESS) begin
                    if (data_be_o != 4'b1111 || data_wdata_o != EXPECTED_SIGNATURE) begin
                        $fatal(1, "bad Ibex signature: be=%x data=%08x", data_be_o, data_wdata_o);
                    end
                    $display("PASS: Ibex wrote signature %0d to %08x after %0d cycles",
                             data_wdata_o, data_addr_o, cycles);
                    $finish;
                end
            end
        end
    end

    always_ff @(posedge clk_i or negedge rst_ni) begin
        if (!rst_ni) begin
            for (register_index = 0; register_index < 32; register_index = register_index + 1) begin
                register_file[register_index] <= 32'b0;
            end
        end else if (rf_we_wb_o && rf_waddr_wb_o != 0) begin
            register_file[rf_waddr_wb_o] <= rf_wdata_wb_ecc_o;
        end
    end

    always_ff @(posedge clk_i or negedge rst_ni) begin
        if (!rst_ni) begin
            cycles <= 0;
        end else if (cycles >= MAXIMUM_CYCLES) begin
            $fatal(1, "Ibex did not produce the expected signature in %0d cycles", cycles);
        end else begin
            cycles <= cycles + 1;
        end
    end
endmodule
