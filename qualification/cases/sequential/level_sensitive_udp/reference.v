// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top(
  input      d,
  input      enable,
  output reg q
);
  always @* begin
    if (enable)
      q = d;
  end
endmodule
