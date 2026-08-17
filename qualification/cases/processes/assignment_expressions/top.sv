// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top(
  input  logic       select,
  input  logic       enable,
  input  logic [1:0] index,
  input  logic [7:0] base [0:3],
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
  output logic [7:0] memory [0:3],
  output logic [1:0] next,
  output logic [7:0] assigned
);
  logic [7:0] first;
  logic [7:0] second;
  logic [7:0] concat_high;
  logic [7:0] concat_low;
  logic [7:0] working [0:3];
  logic [1:0] chosen;

  always_comb begin
    automatic logic [7:0] captured;

    first = p;
    second = q;
    sibling_sum = ((first = second) + (second = r));

    compound_state = p;
    compound_value = (compound_state += q);

    nested_compound_state = p;
    nested_compound_value =
      (nested_compound_state += (nested_compound_state = q));

    concat_high = p;
    concat_low = q;
    concat_value =
      ({concat_high, concat_low} += {8'b0, (concat_high = r)});
    concat_state = {concat_high, concat_low};

    capture_state = p;
    captured = capture_state;
    capture_state = q;
    capture_value = captured;

    update_state = p;
    update_value = (update_state++) + (++update_state);
    decrement_state = p;
    decrement_value = (--decrement_state) + (decrement_state--);

    branch_a = p;
    branch_b = r;
    branch_value = select ? (branch_a = q) : (branch_b = q);

    short_a = p;
    and_value = enable && (short_a = q);
    short_b = r;
    or_value = enable || (short_b = q);
    short_update_state = q;
    short_update_value = enable && (short_update_state++);

    working = base;
    chosen = index;
    assigned = (working[chosen] = (chosen = chosen + 2'd1));
    memory = working;
    next = chosen;
  end
endmodule
