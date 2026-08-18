// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top(
  output wire high,
  output wire low
);
  pullup drive_high(high);
  pulldown drive_low(low);
endmodule
