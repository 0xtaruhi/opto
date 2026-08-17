// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top(
  input  logic       stop_inner,
  input  logic       stop_outer,
  input  logic       stop_loop,
  input  logic       stop_task,
  output logic [7:0] block_value,
  output logic [7:0] loop_value,
  output logic [7:0] task_value
);
  task automatic leave(output logic [7:0] value, input logic stop);
    value = 8'd1;
    if (stop) disable leave;
    value = value + 8'd2;
  endtask

  always_comb begin
    block_value = 8'd0;
    begin : outer
      block_value = block_value + 8'd1;
      begin : inner
        block_value = block_value + 8'd2;
        if (stop_inner) disable inner;
        block_value = block_value + 8'd4;
        if (stop_outer) disable outer;
        block_value = block_value + 8'd8;
      end
      block_value = block_value + 8'd16;
    end
    block_value = block_value + 8'd32;
  end

  always_comb begin
    loop_value = 8'd0;
    begin : loop_scope
      for (int i = 0; i < 4; i++) begin
        loop_value = loop_value + 8'd1;
        if (stop_loop && i == 1) disable loop_scope;
        loop_value = loop_value + 8'd2;
      end
      loop_value = loop_value + 8'd16;
    end
    loop_value = loop_value + 8'd32;
  end

  always_comb begin
    leave(task_value, stop_task);
    task_value = task_value + 8'd4;
  end
endmodule
