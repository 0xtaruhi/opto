// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top(
  input  logic       select,
  input  logic [3:0] data,
  output logic [3:0] forever_value,
  output logic [3:0] while_value,
  output logic [3:0] disable_value
);
  function automatic logic [3:0] choose_forever(
    input logic       pick,
    input logic [3:0] value
  );
    forever begin
      if (pick) return value;
      else return ~value;
    end
  endfunction

  function automatic logic [3:0] choose_while(
    input logic       pick,
    input logic [3:0] value
  );
    while (pick) return value + 4'd1;
    return value - 4'd1;
  endfunction

  always_comb begin
    forever_value = choose_forever(select, data);
    while_value = choose_while(select, data);
  end

  always_comb begin
    disable_value = 4'd0;
    begin : outer
      forever begin
        disable_value = data;
        if (select) disable outer;
        else disable outer;
      end
      disable_value = 4'hf;
    end
    disable_value = disable_value ^ 4'h3;
  end
endmodule
