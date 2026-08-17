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
  typedef struct packed {
    logic [1:0] opcode;
    logic [3:0] payload;
  } packet_t;

  packet_t packet;
  packet_t expr_packet;
  packet_t case_packet;

  always_comb begin
    packet = '{opcode: opcode, payload: payload};
    expr_packet = '{opcode: expr_opcode, payload: expr_payload};
    case_packet = '{opcode: case_opcode, payload: case_payload};

    if_value = fallback;
    if (packet matches '{opcode: 2'b01, payload: .captured}
        &&& ((packet.payload = mutate) == mutate)
        &&& captured[3]) begin
      if_value = captured;
    end
    post_payload = packet.payload;

    expr_value = expr_packet matches '{opcode: 2'b10, payload: .value}
        &&& value[0] ? value : fallback;

    case (case_packet) matches
      '{opcode: 2'b00, payload: .value}
          &&& ((case_packet.payload = mutate) == mutate) && value[3]:
        case_value = value;
      '{opcode: 2'b00, payload: .*}: case_value = 4'ha;
      '{opcode: 2'b01, payload: .value}: case_value = value ^ 4'hf;
      default: case_value = 4'h5;
    endcase
    case_post_payload = case_packet.payload;
  end
endmodule
