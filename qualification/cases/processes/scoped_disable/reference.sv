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
  always_comb begin
    block_value = 8'd3;
    if (stop_inner) begin
      block_value = block_value + 8'd16;
    end else begin
      block_value = block_value + 8'd4;
      if (!stop_outer) begin
        block_value = block_value + 8'd8;
        block_value = block_value + 8'd16;
      end
    end
    block_value = block_value + 8'd32;
  end

  always_comb begin
    loop_value = 8'd4;
    if (!stop_loop) begin
      loop_value = loop_value + 8'd2;
      loop_value = loop_value + 8'd3;
      loop_value = loop_value + 8'd3;
      loop_value = loop_value + 8'd16;
    end
    loop_value = loop_value + 8'd32;
  end

  always_comb begin
    task_value = 8'd1;
    if (!stop_task) task_value = task_value + 8'd2;
    task_value = task_value + 8'd4;
  end
endmodule
