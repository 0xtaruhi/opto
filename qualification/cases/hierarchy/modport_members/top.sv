// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

interface data_if;
  logic [3:0] hi;
  logic [3:0] lo;
  logic       mask;

  function automatic logic [7:0] apply_mask(input logic [7:0] value);
    return value ^ {8{mask}};
  endfunction

  function automatic logic [7:0] transform(input logic [7:0] value);
    return apply_mask(value);
  endfunction

  modport source(input .payload({hi, lo}), import transform);
  modport sink(output .payload({hi, lo}));
endinterface

module pipeline(data_if.source source, data_if.sink sink);
  assign sink.payload = source.transform(source.payload);
endmodule

interface callback_if(input logic [7:0] source);
  extern task automatic transform(
    input  logic [7:0] value,
    output logic [7:0] transformed
  );
  logic [7:0] result;
  always_comb transform(source, result);
  modport implementation(input source, output result, export transform);
endinterface

module callback_provider(callback_if.implementation callback);
  task automatic callback.transform(
    input  logic [7:0] value,
    output logic [7:0] transformed
  );
    transformed = ~value;
  endtask
endmodule

interface pad_if;
  wire pad;
  modport device(inout .pin(pad));
endinterface

module transceiver(
  pad_if.device pad,
  input  logic drive,
  output logic observed
);
  assign pad.pin = drive;
  assign observed = pad.pin;
endmodule

module top(
  input  logic [7:0] data,
  input  logic       mask,
  input  logic       drive,
  output logic [7:0] imported_result,
  output logic [7:0] exported_result,
  output logic       inout_observed
);
  data_if source();
  data_if sink();
  callback_if callback(data);
  pad_if pad();

  assign source.hi = data[7:4];
  assign source.lo = data[3:0];
  assign source.mask = mask;
  assign imported_result = {sink.hi, sink.lo};
  assign exported_result = callback.result;

  pipeline u_pipeline(.source(source), .sink(sink));
  callback_provider u_callback(.callback(callback));
  transceiver u_transceiver(
    .pad(pad),
    .drive(drive),
    .observed(inout_observed)
  );
endmodule
