// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top(
  input  [31:0] value,
  input  [1:0]  index,
  output reg [31:0] y,
  output reg [1:0]  next,
  output reg [3:0]  aliased
);
  always @* begin
    y = value;
    case (index)
      2'd0: y[7:0] = value[7:0] ^ 8'ha5;
      2'd1: y[15:8] = value[15:8] ^ 8'ha5;
      2'd2: y[23:16] = value[23:16] ^ 8'ha5;
      default: y[31:24] = value[31:24] ^ 8'ha5;
    endcase
    next = index + 2'd1;
    aliased = 4'hf;
  end
endmodule
