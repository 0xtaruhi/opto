// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top (
    input  logic       clk,
    input  logic       reset,
    input  logic       select,
    output logic [2:0] phase
);
    localparam logic [7:0] IDLE = 8'h00;
    localparam logic [7:0] A0   = 8'h10;
    localparam logic [7:0] A1   = 8'h20;
    localparam logic [7:0] B0   = 8'h40;
    localparam logic [7:0] B1   = 8'h80;

    logic [7:0] state;

    always_ff @(posedge clk) begin
        if (reset) begin
            state <= IDLE;
        end else begin
            case (state)
                IDLE: state <= select ? A0 : A1;
                A0:   state <= B0;
                A1:   state <= B1;
                B0,
                B1:   state <= IDLE;
                default: state <= IDLE;
            endcase
        end
    end

    assign phase[2] = state == A0 || state == A1;
    assign phase[1] = state == B0 || state == B1;
    assign phase[0] = phase[1];
endmodule
