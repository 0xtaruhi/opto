// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

module top(a, b, c, d, e, y);
    input a;
    input b;
    input c;
    input d;
    input e;
    output y;

    assign y = (a & b) | (c & d) | e;
endmodule
