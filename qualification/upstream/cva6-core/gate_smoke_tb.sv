// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

`timescale 1ns/1ps

module cva6_gate_smoke_tb;
    localparam logic [63:0] BOOT_ADDRESS = 64'h0000_0000_8000_0000;
    localparam logic [63:0] SIGNATURE_ADDRESS = 64'h0000_0000_1000_0100;
    localparam logic [31:0] EXPECTED_SIGNATURE = 32'd12;
    localparam int unsigned MAXIMUM_CYCLES = 20_000;

    typedef logic [3:0] axi_id_t;
    typedef logic [31:0] axi_user_t;

    typedef struct packed {
        axi_id_t id;
        logic [63:0] addr;
        logic [7:0] len;
        logic [2:0] size;
        logic [1:0] burst;
        logic lock;
        logic [3:0] cache;
        logic [2:0] prot;
        logic [3:0] qos;
        logic [3:0] region;
        logic [5:0] atop;
        axi_user_t user;
    } axi_aw_t;

    typedef struct packed {
        logic [63:0] data;
        logic [7:0] strb;
        logic last;
        axi_user_t user;
    } axi_w_t;

    typedef struct packed {
        axi_id_t id;
        logic [1:0] resp;
        axi_user_t user;
    } axi_b_t;

    typedef struct packed {
        axi_id_t id;
        logic [63:0] addr;
        logic [7:0] len;
        logic [2:0] size;
        logic [1:0] burst;
        logic lock;
        logic [3:0] cache;
        logic [2:0] prot;
        logic [3:0] qos;
        logic [3:0] region;
        axi_user_t user;
    } axi_ar_t;

    typedef struct packed {
        axi_id_t id;
        logic [63:0] data;
        logic [1:0] resp;
        logic last;
        axi_user_t user;
    } axi_r_t;

    typedef struct packed {
        axi_aw_t aw;
        logic aw_valid;
        axi_w_t w;
        logic w_valid;
        logic b_ready;
        axi_ar_t ar;
        logic ar_valid;
        logic r_ready;
    } axi_req_t;

    typedef struct packed {
        logic aw_ready;
        logic ar_ready;
        logic w_ready;
        logic b_valid;
        axi_b_t b;
        logic r_valid;
        axi_r_t r;
    } axi_resp_t;

    logic clk_i = 1'b0;
    logic rst_ni = 1'b0;
    logic [4195:0] rvfi_probes;
    logic [256:0] cvxif_req;
    logic [373:0] noc_req_bits;
    logic [145:0] noc_resp_bits;
    axi_req_t noc_req;
    axi_resp_t noc_resp;

    logic read_active;
    logic [63:0] read_address;
    logic [7:0] read_beats_remaining;
    axi_id_t read_id;

    logic address_active;
    logic [63:0] write_address;
    axi_id_t write_id;
    logic data_active;
    logic [63:0] write_data;
    logic [7:0] write_strobe;
    logic response_active;
    axi_id_t response_id;
    int unsigned cycles;

    assign noc_req = axi_req_t'(noc_req_bits);
    assign noc_resp_bits = noc_resp;

    cva6 dut (
        .clk_i(clk_i),
        .rst_ni(rst_ni),
        .boot_addr_i(BOOT_ADDRESS[31:0]),
        .hart_id_i(32'b0),
        .irq_i(2'b0),
        .ipi_i(1'b0),
        .time_irq_i(1'b0),
        .debug_req_i(1'b0),
        .rvfi_probes_o(rvfi_probes),
        .cvxif_req_o(cvxif_req),
        .cvxif_resp_i(114'b0),
        .noc_req_o(noc_req_bits),
        .noc_resp_i(noc_resp_bits)
    );

    always #5 clk_i = ~clk_i;

    initial begin
        repeat (8) @(posedge clk_i);
        rst_ni = 1'b1;
    end

    function automatic logic [31:0] instruction_word(input logic [63:0] address);
        case (address)
            BOOT_ADDRESS + 0: instruction_word = 32'h0050_0093; // addi x1, x0, 5
            BOOT_ADDRESS + 4: instruction_word = 32'h0070_0113; // addi x2, x0, 7
            BOOT_ADDRESS + 8: instruction_word = 32'h0020_81b3; // add  x3, x1, x2
            BOOT_ADDRESS + 12: instruction_word = 32'h1000_0237; // lui  x4, 0x10000
            BOOT_ADDRESS + 16: instruction_word = 32'h1032_2023; // sw   x3, 256(x4)
            BOOT_ADDRESS + 20: instruction_word = 32'h0000_006f; // jal  x0, 0
            default: instruction_word = 32'h0000_0013; // addi x0, x0, 0
        endcase
    endfunction

    function automatic logic [63:0] memory_beat(input logic [63:0] address);
        logic [63:0] aligned_address;
        begin
            aligned_address = {address[63:3], 3'b000};
            memory_beat = {
                instruction_word(aligned_address + 4),
                instruction_word(aligned_address)
            };
        end
    endfunction

    always_comb begin
        noc_resp = '0;
        noc_resp.ar_ready = !read_active;
        noc_resp.r_valid = read_active;
        noc_resp.r.id = read_id;
        noc_resp.r.data = memory_beat(read_address);
        noc_resp.r.resp = 2'b00;
        noc_resp.r.last = read_beats_remaining == 0;
        noc_resp.aw_ready = !address_active && !response_active;
        noc_resp.w_ready = !data_active && !response_active;
        noc_resp.b_valid = response_active;
        noc_resp.b.id = response_id;
        noc_resp.b.resp = 2'b00;
    end

    always_ff @(posedge clk_i or negedge rst_ni) begin
        if (!rst_ni) begin
            read_active <= 1'b0;
            read_address <= '0;
            read_beats_remaining <= '0;
            read_id <= '0;
        end else if (!read_active && noc_req.ar_valid) begin
            read_active <= 1'b1;
            read_address <= noc_req.ar.addr;
            read_beats_remaining <= noc_req.ar.len;
            read_id <= noc_req.ar.id;
        end else if (read_active && noc_req.r_ready) begin
            if (read_beats_remaining == 0) begin
                read_active <= 1'b0;
            end else begin
                read_address <= read_address + 8;
                read_beats_remaining <= read_beats_remaining - 1'b1;
            end
        end
    end

    always_ff @(posedge clk_i or negedge rst_ni) begin
        logic accepting_address;
        logic accepting_data;
        logic [63:0] completed_address;
        logic [63:0] completed_data;
        logic [7:0] completed_strobe;

        if (!rst_ni) begin
            address_active <= 1'b0;
            write_address <= '0;
            write_id <= '0;
            data_active <= 1'b0;
            write_data <= '0;
            write_strobe <= '0;
            response_active <= 1'b0;
            response_id <= '0;
        end else begin
            accepting_address = noc_req.aw_valid && noc_resp.aw_ready;
            accepting_data = noc_req.w_valid && noc_resp.w_ready;

            if (accepting_address) begin
                address_active <= 1'b1;
                write_address <= noc_req.aw.addr;
                write_id <= noc_req.aw.id;
            end
            if (accepting_data) begin
                data_active <= 1'b1;
                write_data <= noc_req.w.data;
                write_strobe <= noc_req.w.strb;
            end

            if (!response_active && (address_active || accepting_address)
                    && (data_active || accepting_data)) begin
                completed_address = accepting_address ? noc_req.aw.addr : write_address;
                completed_data = accepting_data ? noc_req.w.data : write_data;
                completed_strobe = accepting_data ? noc_req.w.strb : write_strobe;
                address_active <= 1'b0;
                data_active <= 1'b0;
                response_active <= 1'b1;
                response_id <= accepting_address ? noc_req.aw.id : write_id;

                if (completed_address == SIGNATURE_ADDRESS) begin
                    if (completed_strobe[3:0] != 4'b1111
                            || completed_data[31:0] != EXPECTED_SIGNATURE) begin
                        $fatal(1, "bad CVA6 signature: strobe=%02x data=%016x",
                               completed_strobe, completed_data);
                    end
                    $display("PASS: CVA6 wrote signature %0d to %016x",
                             completed_data[31:0], completed_address);
                    $finish;
                end
            end else if (response_active && noc_req.b_ready) begin
                response_active <= 1'b0;
            end
        end
    end

    always_ff @(posedge clk_i) begin
        if (!rst_ni) begin
            cycles <= 0;
        end else if (cycles == MAXIMUM_CYCLES) begin
            $fatal(1, "CVA6 did not produce the expected signature in %0d cycles", cycles);
        end else begin
            cycles <= cycles + 1;
        end
    end
endmodule
