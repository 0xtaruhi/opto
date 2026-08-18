// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top(
  input  logic [1:0] opcode,
  input  logic [3:0] payload,
  input  logic [3:0] mutate,
  input  logic [3:0] fallback,
  input  logic [1:0] expr_opcode,
  input  logic [3:0] expr_payload,
  input  logic [1:0] case_opcode,
  input  logic [3:0] case_payload,
  output logic [3:0] if_value,
  output logic [3:0] post_payload,
  output logic [3:0] expr_value,
  output logic [3:0] case_value,
  output logic [3:0] case_post_payload
);
  always_comb begin
    if_value = fallback;
    if (opcode == 2'b01 && payload[3]) begin
      if_value = payload;
    end
    post_payload = opcode == 2'b01 ? mutate : payload;

    expr_value = expr_opcode == 2'b10 && expr_payload[0]
        ? expr_payload
        : fallback;

    if (case_opcode == 2'b00 && case_payload[3]) begin
      case_value = case_payload;
    end else if (case_opcode == 2'b00) begin
      case_value = 4'ha;
    end else if (case_opcode == 2'b01) begin
      case_value = case_payload ^ 4'hf;
    end else begin
      case_value = 4'h5;
    end
    case_post_payload = case_opcode == 2'b00 ? mutate : case_payload;
  end
endmodule
