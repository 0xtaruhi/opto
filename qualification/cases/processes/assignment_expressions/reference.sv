// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top(
  input  logic       select,
  input  logic       enable,
  input  logic [1:0] index,
  input  logic [31:0] base,
  input  logic [7:0] p,
  input  logic [7:0] q,
  input  logic [7:0] r,
  output logic [7:0] sibling_sum,
  output logic [7:0] compound_state,
  output logic [7:0] compound_value,
  output logic [7:0] nested_compound_state,
  output logic [7:0] nested_compound_value,
  output logic [15:0] concat_state,
  output logic [15:0] concat_value,
  output logic [7:0] capture_state,
  output logic [7:0] capture_value,
  output logic [7:0] update_state,
  output logic [7:0] update_value,
  output logic [7:0] decrement_state,
  output logic [7:0] decrement_value,
  output logic [7:0] branch_a,
  output logic [7:0] branch_b,
  output logic [7:0] branch_value,
  output logic [7:0] short_a,
  output logic [7:0] short_b,
  output logic       and_value,
  output logic       or_value,
  output logic [7:0] short_update_state,
  output logic       short_update_value,
  output logic [31:0] memory,
  output logic [1:0] next,
  output logic [7:0] assigned
);
  always_comb begin
    sibling_sum = q + r;
    compound_state = p + q;
    compound_value = p + q;
    nested_compound_state = p + q;
    nested_compound_value = p + q;
    concat_state = {p, q} + {8'b0, r};
    concat_value = {p, q} + {8'b0, r};
    capture_state = q;
    capture_value = p;
    update_state = p + 8'd2;
    update_value = p + (p + 8'd2);
    decrement_state = p - 8'd2;
    decrement_value = (p - 8'd1) + (p - 8'd1);

    branch_a = select ? q : p;
    branch_b = select ? r : q;
    branch_value = q;

    short_a = enable ? q : p;
    and_value = enable && (|q);
    short_b = enable ? r : q;
    or_value = enable || (|q);
    short_update_state = enable ? q + 8'd1 : q;
    short_update_value = enable && (|q);

    memory = base;
    next = index + 2'd1;
    assigned = {6'b000000, next};
    case (index)
      2'd0: memory[7:0] = assigned;
      2'd1: memory[15:8] = assigned;
      2'd2: memory[23:16] = assigned;
      default: memory[31:24] = assigned;
    endcase
  end
endmodule
