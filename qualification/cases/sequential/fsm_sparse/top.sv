// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top (
    input  logic       clk,
    input  logic       reset,
    input  logic       start,
    input  logic       loaded,
    input  logic       done,
    output logic       active,
    output logic [7:0] state_o
);
    localparam logic [7:0] IDLE = 8'h00;
    localparam logic [7:0] LOAD = 8'h10;
    localparam logic [7:0] RUN  = 8'h20;
    localparam logic [7:0] DONE = 8'h40;

    logic [7:0] state;

    always_ff @(posedge clk) begin
        if (reset) begin
            state <= IDLE;
        end else begin
            case (state)
                IDLE: if (start)  state <= LOAD;
                LOAD: if (loaded) state <= RUN;
                RUN:  if (done)   state <= DONE;
                DONE:             state <= IDLE;
                default:          state <= IDLE;
            endcase
        end
    end

    assign active = state == RUN;
    assign state_o = state;
endmodule
