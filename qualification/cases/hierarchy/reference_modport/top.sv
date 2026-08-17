// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

interface bus_if;
  logic [7:0] data;
  modport alias_port(ref data);
endinterface

module transform(
  bus_if.alias_port bus,
  input logic [7:0] data,
  input logic       invert
);
  always_comb begin
    bus.data = data;
    if (invert)
      bus.data = ~bus.data;
  end
endmodule

module top(
  input  logic [7:0] data,
  input  logic       invert,
  output logic [7:0] y
);
  bus_if link();
  transform u_transform(.bus(link), .data(data), .invert(invert));
  assign y = link.data;
endmodule
