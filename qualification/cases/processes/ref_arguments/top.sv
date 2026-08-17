// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top(
  input  logic [31:0] value,
  input  logic [1:0]  index,
  output logic [31:0] y,
  output logic [1:0]  next,
  output logic [3:0]  aliased
);
  logic [7:0] working [0:3];
  logic [1:0] chosen;
  logic [3:0] alias_value;

  task automatic update(
    ref logic [7:0] item,
    ref logic [1:0] selector
  );
    selector = selector + 2'd1;
    item = item ^ 8'ha5;
  endtask

  task automatic alias_pair(
    ref logic [3:0] first,
    ref logic [3:0] second
  );
    first = 4'hc;
    second = first ^ 4'h3;
  endtask

  always_comb begin
    working[0] = value[7:0];
    working[1] = value[15:8];
    working[2] = value[23:16];
    working[3] = value[31:24];
    chosen = index;
    update(working[chosen], chosen);

    alias_value = value[3:0];
    alias_pair(alias_value, alias_value);

    y = {working[3], working[2], working[1], working[0]};
    next = chosen;
    aliased = alias_value;
  end
endmodule
