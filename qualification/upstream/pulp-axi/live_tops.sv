// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

`include "axi/typedef.svh"

package opto_pulp_axi_types;
  typedef logic [31:0] addr_t;
  typedef logic [1:0] id_t;
  typedef logic user_t;
  typedef logic [31:0] data_t;
  typedef logic [3:0] strb_t;

  `AXI_TYPEDEF_AW_CHAN_T(aw_t, addr_t, id_t, user_t)
  `AXI_TYPEDEF_W_CHAN_T(w_t, data_t, strb_t, user_t)
  `AXI_TYPEDEF_B_CHAN_T(b_t, id_t, user_t)
  `AXI_TYPEDEF_AR_CHAN_T(ar_t, addr_t, id_t, user_t)
  `AXI_TYPEDEF_R_CHAN_T(r_t, data_t, id_t, user_t)
  `AXI_TYPEDEF_REQ_T(req_t, aw_t, w_t, ar_t)
  `AXI_TYPEDEF_RESP_T(resp_t, b_t, r_t)

  `AXI_LITE_TYPEDEF_AW_CHAN_T(lite_aw_t, addr_t)
  `AXI_LITE_TYPEDEF_W_CHAN_T(lite_w_t, data_t, strb_t)
  `AXI_LITE_TYPEDEF_B_CHAN_T(lite_b_t)
  `AXI_LITE_TYPEDEF_AR_CHAN_T(lite_ar_t, addr_t)
  `AXI_LITE_TYPEDEF_R_CHAN_T(lite_r_t, data_t)
  `AXI_LITE_TYPEDEF_REQ_T(lite_req_t, lite_aw_t, lite_w_t, lite_ar_t)
  `AXI_LITE_TYPEDEF_RESP_T(lite_resp_t, lite_b_t, lite_r_t)

  typedef logic [63:0] wide_data_t;
  typedef logic [7:0] wide_strb_t;
  `AXI_TYPEDEF_W_CHAN_T(wide_w_t, wide_data_t, wide_strb_t, user_t)
  `AXI_TYPEDEF_R_CHAN_T(wide_r_t, wide_data_t, id_t, user_t)
  `AXI_TYPEDEF_REQ_T(wide_req_t, aw_t, wide_w_t, ar_t)
  `AXI_TYPEDEF_RESP_T(wide_resp_t, b_t, wide_r_t)

  typedef logic [2:0] wide_id_t;
  `AXI_TYPEDEF_AW_CHAN_T(wide_id_aw_t, addr_t, wide_id_t, user_t)
  `AXI_TYPEDEF_B_CHAN_T(wide_id_b_t, wide_id_t, user_t)
  `AXI_TYPEDEF_AR_CHAN_T(wide_id_ar_t, addr_t, wide_id_t, user_t)
  `AXI_TYPEDEF_R_CHAN_T(wide_id_r_t, data_t, wide_id_t, user_t)
  `AXI_TYPEDEF_REQ_T(wide_id_req_t, wide_id_aw_t, w_t, wide_id_ar_t)
  `AXI_TYPEDEF_RESP_T(wide_id_resp_t, wide_id_b_t, wide_id_r_t)

  localparam int unsigned ReqBits = $bits(req_t);
  localparam int unsigned RespBits = $bits(resp_t);
  localparam int unsigned LiteReqBits = $bits(lite_req_t);
  localparam int unsigned LiteRespBits = $bits(lite_resp_t);
  localparam int unsigned WideReqBits = $bits(wide_req_t);
  localparam int unsigned WideRespBits = $bits(wide_resp_t);
  localparam int unsigned WideIdReqBits = $bits(wide_id_req_t);
  localparam int unsigned WideIdRespBits = $bits(wide_id_resp_t);

  typedef struct packed {
    addr_t          paddr;
    axi_pkg::prot_t pprot;
    logic           psel;
    logic           penable;
    logic           pwrite;
    data_t          pwdata;
    strb_t          pstrb;
  } apb_req_t;
  typedef struct packed {
    logic  pready;
    data_t prdata;
    logic  pslverr;
  } apb_resp_t;
  localparam int unsigned ApbReqBits = $bits(apb_req_t);
  localparam int unsigned ApbRespBits = $bits(apb_resp_t);
endpackage

module opto_axi_pipeline_live (
  input  logic                                      clk_i,
  input  logic                                      rst_ni,
  input  logic                                      isolate_i,
  input  logic [opto_pulp_axi_types::ReqBits-1:0]  slv_req_flat_i,
  input  logic [opto_pulp_axi_types::RespBits-1:0] mst_resp_flat_i,
  output logic [opto_pulp_axi_types::RespBits-1:0] slv_resp_flat_o,
  output logic [opto_pulp_axi_types::ReqBits-1:0]  mst_req_flat_o,
  output logic                                      isolated_o
);
  import opto_pulp_axi_types::*;
  req_t slv_req, cut_req, fifo_req, filter_req, serializer_req, mst_req;
  resp_t slv_resp, cut_resp, fifo_resp, filter_resp, serializer_resp, mst_resp;

  assign slv_req = slv_req_flat_i;
  assign mst_resp = mst_resp_flat_i;
  assign slv_resp_flat_o = slv_resp;
  assign mst_req_flat_o = mst_req;

  axi_cut #(
    .Bypass(1'b0), .aw_chan_t(aw_t), .w_chan_t(w_t), .b_chan_t(b_t),
    .ar_chan_t(ar_t), .r_chan_t(r_t), .axi_req_t(req_t), .axi_resp_t(resp_t)
  ) i_cut (
    .clk_i, .rst_ni, .slv_req_i(slv_req), .slv_resp_o(slv_resp),
    .mst_req_o(cut_req), .mst_resp_i(cut_resp)
  );

  axi_fifo #(
    .Depth(4), .FallThrough(1'b0), .aw_chan_t(aw_t), .w_chan_t(w_t),
    .b_chan_t(b_t), .ar_chan_t(ar_t), .r_chan_t(r_t), .axi_req_t(req_t),
    .axi_resp_t(resp_t)
  ) i_fifo (
    .clk_i, .rst_ni, .test_i(1'b0), .slv_req_i(cut_req), .slv_resp_o(cut_resp),
    .mst_req_o(fifo_req), .mst_resp_i(fifo_resp)
  );

  axi_atop_filter #(
    .AxiIdWidth(2), .AxiMaxWriteTxns(4), .axi_req_t(req_t), .axi_resp_t(resp_t)
  ) i_atop_filter (
    .clk_i, .rst_ni, .slv_req_i(fifo_req), .slv_resp_o(fifo_resp),
    .mst_req_o(filter_req), .mst_resp_i(filter_resp)
  );

  axi_serializer #(
    .MaxReadTxns(4), .MaxWriteTxns(4), .AxiIdWidth(2),
    .axi_req_t(req_t), .axi_resp_t(resp_t)
  ) i_serializer (
    .clk_i, .rst_ni, .slv_req_i(filter_req), .slv_resp_o(filter_resp),
    .mst_req_o(serializer_req), .mst_resp_i(serializer_resp)
  );

  axi_isolate #(
    .NumPending(4), .TerminateTransaction(1'b1), .AtopSupport(1'b1),
    .AxiAddrWidth(32), .AxiDataWidth(32), .AxiIdWidth(2), .AxiUserWidth(1),
    .axi_req_t(req_t), .axi_resp_t(resp_t)
  ) i_isolate (
    .clk_i, .rst_ni, .slv_req_i(serializer_req), .slv_resp_o(serializer_resp),
    .mst_req_o(mst_req), .mst_resp_i(mst_resp), .isolate_i, .isolated_o
  );
endmodule

module opto_axi_memory_endpoints_live (
  input  logic                                      clk_i,
  input  logic                                      rst_ni,
  input  logic                                      mem_req_i,
  input  logic [15:0]                               mem_addr_i,
  input  logic                                      mem_we_i,
  input  logic [31:0]                               mem_wdata_i,
  input  logic [3:0]                                mem_be_i,
  input  logic [3:0]                                slv_aw_cache_i,
  input  logic [3:0]                                slv_ar_cache_i,
  input  logic [opto_pulp_axi_types::RespBits-1:0] from_mem_axi_resp_flat_i,
  input  logic [opto_pulp_axi_types::ReqBits-1:0]  zero_mem_axi_req_flat_i,
  output logic                                      mem_gnt_o,
  output logic                                      mem_rsp_valid_o,
  output logic [31:0]                               mem_rsp_rdata_o,
  output logic                                      mem_rsp_error_o,
  output logic [opto_pulp_axi_types::ReqBits-1:0]  from_mem_axi_req_flat_o,
  output logic                                      zero_mem_busy_o,
  output logic [opto_pulp_axi_types::RespBits-1:0] zero_mem_axi_resp_flat_o
);
  import opto_pulp_axi_types::*;
  req_t from_mem_axi_req, zero_mem_axi_req;
  resp_t from_mem_axi_resp, zero_mem_axi_resp;
  assign from_mem_axi_resp = from_mem_axi_resp_flat_i;
  assign zero_mem_axi_req = zero_mem_axi_req_flat_i;
  assign from_mem_axi_req_flat_o = from_mem_axi_req;
  assign zero_mem_axi_resp_flat_o = zero_mem_axi_resp;

  axi_from_mem #(
    .MemAddrWidth(16), .AxiAddrWidth(32), .DataWidth(32), .MaxRequests(4),
    .axi_req_t(req_t), .axi_rsp_t(resp_t)
  ) i_from_mem (
    .clk_i, .rst_ni, .mem_req_i, .mem_addr_i, .mem_we_i, .mem_wdata_i,
    .mem_be_i, .mem_gnt_o, .mem_rsp_valid_o, .mem_rsp_rdata_o,
    .mem_rsp_error_o, .slv_aw_cache_i, .slv_ar_cache_i,
    .axi_req_o(from_mem_axi_req), .axi_rsp_i(from_mem_axi_resp)
  );

  axi_zero_mem #(
    .axi_req_t(req_t), .axi_resp_t(resp_t), .AddrWidth(16), .DataWidth(32),
    .IdWidth(2), .NumBanks(2), .BufDepth(2)
  ) i_zero_mem (
    .clk_i, .rst_ni, .busy_o(zero_mem_busy_o),
    .axi_req_i(zero_mem_axi_req), .axi_resp_o(zero_mem_axi_resp)
  );
endmodule

module opto_axi_cdc_live (
  input  logic                                      src_clk_i,
  input  logic                                      src_rst_ni,
  input  logic                                      dst_clk_i,
  input  logic                                      dst_rst_ni,
  input  logic [opto_pulp_axi_types::ReqBits-1:0]  src_req_flat_i,
  input  logic [opto_pulp_axi_types::RespBits-1:0] dst_resp_flat_i,
  output logic [opto_pulp_axi_types::RespBits-1:0] src_resp_flat_o,
  output logic [opto_pulp_axi_types::ReqBits-1:0]  dst_req_flat_o
);
  import opto_pulp_axi_types::*;
  req_t src_req, dst_req;
  resp_t src_resp, dst_resp;
  assign src_req = src_req_flat_i;
  assign dst_resp = dst_resp_flat_i;
  assign src_resp_flat_o = src_resp;
  assign dst_req_flat_o = dst_req;
  axi_cdc #(
    .aw_chan_t(aw_t), .w_chan_t(w_t), .b_chan_t(b_t), .ar_chan_t(ar_t),
    .r_chan_t(r_t), .axi_req_t(req_t), .axi_resp_t(resp_t),
    .LogDepth(2), .SyncStages(2)
  ) i_cdc (
    .src_clk_i, .src_rst_ni, .src_req_i(src_req), .src_resp_o(src_resp),
    .dst_clk_i, .dst_rst_ni, .dst_req_o(dst_req), .dst_resp_i(dst_resp)
  );
endmodule

module opto_axi_utilities_live (
  input  logic                                      clk_i,
  input  logic                                      rst_ni,
  input  logic                                      inval_enable_i,
  input  logic                                      inval_ready_i,
  input  logic [31:0]                               modified_aw_addr_i,
  input  logic [31:0]                               modified_ar_addr_i,
  input  logic [opto_pulp_axi_types::ReqBits-1:0]  slv_req_flat_i,
  input  logic [opto_pulp_axi_types::RespBits-1:0] mst_resp_flat_i,
  input  logic [opto_pulp_axi_types::ReqBits-1:0]  lfsr_req_flat_i,
  input  logic                                      w_ser_data_i,
  input  logic                                      w_ser_en_i,
  input  logic                                      r_ser_data_i,
  input  logic                                      r_ser_en_i,
  input  logic [opto_pulp_axi_types::ReqBits-1:0]  error_req_flat_i,
  output logic [opto_pulp_axi_types::RespBits-1:0] slv_resp_flat_o,
  output logic [opto_pulp_axi_types::ReqBits-1:0]  mst_req_flat_o,
  output logic [31:0]                               inval_addr_o,
  output logic                                      inval_valid_o,
  output logic [opto_pulp_axi_types::RespBits-1:0] lfsr_resp_flat_o,
  output logic                                      w_ser_data_o,
  output logic                                      r_ser_data_o,
  output logic [opto_pulp_axi_types::RespBits-1:0] error_resp_flat_o
);
  import opto_pulp_axi_types::*;
  req_t slv_req, modified_req, invalidated_req, read_req, write_req, mst_req;
  req_t lfsr_req, error_req;
  resp_t slv_resp, modified_resp, invalidated_resp, read_resp, write_resp, mst_resp;
  resp_t lfsr_resp, error_resp;
  assign slv_req = slv_req_flat_i;
  assign mst_resp = mst_resp_flat_i;
  assign lfsr_req = lfsr_req_flat_i;
  assign error_req = error_req_flat_i;
  assign slv_resp_flat_o = slv_resp;
  assign mst_req_flat_o = mst_req;
  assign lfsr_resp_flat_o = lfsr_resp;
  assign error_resp_flat_o = error_resp;

  axi_modify_address #(
    .slv_req_t(req_t), .mst_addr_t(addr_t), .mst_req_t(req_t),
    .axi_resp_t(resp_t)
  ) i_modify_address (
    .slv_req_i(slv_req), .slv_resp_o(slv_resp),
    .mst_aw_addr_i(modified_aw_addr_i), .mst_ar_addr_i(modified_ar_addr_i),
    .mst_req_o(modified_req), .mst_resp_i(modified_resp)
  );

  axi_inval_filter #(
    .MaxTxns(4), .AddrWidth(32), .L1LineWidth(64), .aw_chan_t(aw_t),
    .req_t(req_t), .resp_t(resp_t)
  ) i_inval_filter (
    .clk_i, .rst_ni, .en_i(inval_enable_i), .slv_req_i(modified_req),
    .slv_resp_o(modified_resp), .mst_req_o(invalidated_req),
    .mst_resp_i(invalidated_resp), .inval_addr_o, .inval_valid_o,
    .inval_ready_i
  );

  axi_rw_split #(.axi_req_t(req_t), .axi_resp_t(resp_t)) i_rw_split (
    .clk_i, .rst_ni, .slv_req_i(invalidated_req),
    .slv_resp_o(invalidated_resp), .mst_read_req_o(read_req),
    .mst_read_resp_i(read_resp), .mst_write_req_o(write_req),
    .mst_write_resp_i(write_resp)
  );

  axi_rw_join #(.axi_req_t(req_t), .axi_resp_t(resp_t)) i_rw_join (
    .clk_i, .rst_ni, .slv_read_req_i(read_req), .slv_read_resp_o(read_resp),
    .slv_write_req_i(write_req), .slv_write_resp_o(write_resp),
    .mst_req_o(mst_req), .mst_resp_i(mst_resp)
  );

  axi_lfsr #(
    .DataWidth(32), .AddrWidth(32), .IdWidth(2), .UserWidth(1),
    .axi_req_t(req_t), .axi_rsp_t(resp_t)
  ) i_lfsr (
    .clk_i, .rst_ni, .testmode_i(1'b0), .req_i(lfsr_req), .rsp_o(lfsr_resp),
    .w_ser_data_i, .w_ser_data_o, .w_ser_en_i,
    .r_ser_data_i, .r_ser_data_o, .r_ser_en_i
  );

  axi_err_slv #(
    .AxiIdWidth(2), .axi_req_t(req_t), .axi_resp_t(resp_t), .MaxTrans(4)
  ) i_error_slave (
    .clk_i, .rst_ni, .test_i(1'b0),
    .slv_req_i(error_req), .slv_resp_o(error_resp)
  );
endmodule

module opto_axi_compare_live (
  input  logic                                      clk_i,
  input  logic                                      rst_ni,
  input  logic [opto_pulp_axi_types::ReqBits-1:0]  axi_a_req_flat_i,
  input  logic [opto_pulp_axi_types::RespBits-1:0] axi_a_resp_flat_i,
  input  logic [opto_pulp_axi_types::ReqBits-1:0]  axi_b_req_flat_i,
  input  logic [opto_pulp_axi_types::RespBits-1:0] axi_b_resp_flat_i,
  output logic [opto_pulp_axi_types::RespBits-1:0] axi_a_resp_flat_o,
  output logic [opto_pulp_axi_types::ReqBits-1:0]  axi_a_req_flat_o,
  output logic [opto_pulp_axi_types::RespBits-1:0] axi_b_resp_flat_o,
  output logic [opto_pulp_axi_types::ReqBits-1:0]  axi_b_req_flat_o,
  output logic [3:0]                                aw_mismatch_o,
  output logic                                      w_mismatch_o,
  output logic [3:0]                                b_mismatch_o,
  output logic [3:0]                                ar_mismatch_o,
  output logic [3:0]                                r_mismatch_o,
  output logic                                      mismatch_o,
  output logic                                      busy_o
);
  import opto_pulp_axi_types::*;
  req_t axi_a_req_i, axi_a_req_o, axi_b_req_i, axi_b_req_o;
  resp_t axi_a_resp_i, axi_a_resp_o, axi_b_resp_i, axi_b_resp_o;
  assign axi_a_req_i = axi_a_req_flat_i;
  assign axi_a_resp_i = axi_a_resp_flat_i;
  assign axi_b_req_i = axi_b_req_flat_i;
  assign axi_b_resp_i = axi_b_resp_flat_i;
  assign axi_a_resp_flat_o = axi_a_resp_o;
  assign axi_a_req_flat_o = axi_a_req_o;
  assign axi_b_resp_flat_o = axi_b_resp_o;
  assign axi_b_req_flat_o = axi_b_req_o;

  axi_bus_compare #(
    .AxiIdWidth(2), .FifoDepth(4), .UseSize(1'b1), .DataWidth(32),
    .axi_aw_chan_t(aw_t), .axi_w_chan_t(w_t), .axi_b_chan_t(b_t),
    .axi_ar_chan_t(ar_t), .axi_r_chan_t(r_t), .axi_req_t(req_t),
    .axi_rsp_t(resp_t)
  ) i_bus_compare (
    .clk_i, .rst_ni, .testmode_i(1'b0),
    .axi_a_req_i, .axi_a_rsp_o(axi_a_resp_o),
    .axi_a_req_o, .axi_a_rsp_i(axi_a_resp_i),
    .axi_b_req_i, .axi_b_rsp_o(axi_b_resp_o),
    .axi_b_req_o, .axi_b_rsp_i(axi_b_resp_i),
    .aw_mismatch_o, .w_mismatch_o, .b_mismatch_o, .ar_mismatch_o,
    .r_mismatch_o, .mismatch_o, .busy_o
  );
endmodule

module opto_axi_dw_downsize_live (
  input  logic                                          clk_i,
  input  logic                                          rst_ni,
  input  logic [opto_pulp_axi_types::WideReqBits-1:0]  slv_req_flat_i,
  input  logic [opto_pulp_axi_types::RespBits-1:0]     mst_resp_flat_i,
  output logic [opto_pulp_axi_types::WideRespBits-1:0] slv_resp_flat_o,
  output logic [opto_pulp_axi_types::ReqBits-1:0]      mst_req_flat_o
);
  import opto_pulp_axi_types::*;
  wide_req_t slv_req;
  wide_resp_t slv_resp;
  req_t mst_req;
  resp_t mst_resp;
  assign slv_req = slv_req_flat_i;
  assign mst_resp = mst_resp_flat_i;
  assign slv_resp_flat_o = slv_resp;
  assign mst_req_flat_o = mst_req;
  axi_dw_converter #(
    .AxiMaxReads(4), .AxiSlvPortDataWidth(64), .AxiMstPortDataWidth(32),
    .AxiAddrWidth(32), .AxiIdWidth(2), .aw_chan_t(aw_t), .mst_w_chan_t(w_t),
    .slv_w_chan_t(wide_w_t), .b_chan_t(b_t), .ar_chan_t(ar_t),
    .mst_r_chan_t(r_t), .slv_r_chan_t(wide_r_t), .axi_mst_req_t(req_t),
    .axi_mst_resp_t(resp_t), .axi_slv_req_t(wide_req_t),
    .axi_slv_resp_t(wide_resp_t)
  ) i_dw_converter (
    .clk_i, .rst_ni, .slv_req_i(slv_req), .slv_resp_o(slv_resp),
    .mst_req_o(mst_req), .mst_resp_i(mst_resp)
  );
endmodule

module opto_axi_dw_upsize_live (
  input  logic                                          clk_i,
  input  logic                                          rst_ni,
  input  logic [opto_pulp_axi_types::ReqBits-1:0]      slv_req_flat_i,
  input  logic [opto_pulp_axi_types::WideRespBits-1:0] mst_resp_flat_i,
  output logic [opto_pulp_axi_types::RespBits-1:0]     slv_resp_flat_o,
  output logic [opto_pulp_axi_types::WideReqBits-1:0]  mst_req_flat_o
);
  import opto_pulp_axi_types::*;
  req_t slv_req;
  resp_t slv_resp;
  wide_req_t mst_req;
  wide_resp_t mst_resp;
  assign slv_req = slv_req_flat_i;
  assign mst_resp = mst_resp_flat_i;
  assign slv_resp_flat_o = slv_resp;
  assign mst_req_flat_o = mst_req;
  axi_dw_converter #(
    .AxiMaxReads(4), .AxiSlvPortDataWidth(32), .AxiMstPortDataWidth(64),
    .AxiAddrWidth(32), .AxiIdWidth(2), .aw_chan_t(aw_t),
    .mst_w_chan_t(wide_w_t), .slv_w_chan_t(w_t), .b_chan_t(b_t),
    .ar_chan_t(ar_t), .mst_r_chan_t(wide_r_t), .slv_r_chan_t(r_t),
    .axi_mst_req_t(wide_req_t), .axi_mst_resp_t(wide_resp_t),
    .axi_slv_req_t(req_t), .axi_slv_resp_t(resp_t)
  ) i_dw_converter (
    .clk_i, .rst_ni, .slv_req_i(slv_req), .slv_resp_o(slv_resp),
    .mst_req_o(mst_req), .mst_resp_i(mst_resp)
  );
endmodule

module opto_axi_id_serialize_live (
  input  logic                                            clk_i,
  input  logic                                            rst_ni,
  input  logic [opto_pulp_axi_types::WideIdReqBits-1:0]  slv_req_flat_i,
  input  logic [opto_pulp_axi_types::RespBits-1:0]       mst_resp_flat_i,
  output logic [opto_pulp_axi_types::WideIdRespBits-1:0] slv_resp_flat_o,
  output logic [opto_pulp_axi_types::ReqBits-1:0]        mst_req_flat_o
);
  import opto_pulp_axi_types::*;
  wide_id_req_t slv_req;
  wide_id_resp_t slv_resp;
  req_t mst_req;
  resp_t mst_resp;
  assign slv_req = slv_req_flat_i;
  assign mst_resp = mst_resp_flat_i;
  assign slv_resp_flat_o = slv_resp;
  assign mst_req_flat_o = mst_req;
  axi_iw_converter #(
    .AxiSlvPortIdWidth(3), .AxiMstPortIdWidth(2), .AxiSlvPortMaxUniqIds(8),
    .AxiSlvPortMaxTxnsPerId(4), .AxiSlvPortMaxTxns(8),
    .AxiMstPortMaxUniqIds(4), .AxiMstPortMaxTxnsPerId(4),
    .AxiAddrWidth(32), .AxiDataWidth(32), .AxiUserWidth(1),
    .slv_req_t(wide_id_req_t), .slv_resp_t(wide_id_resp_t),
    .mst_req_t(req_t), .mst_resp_t(resp_t)
  ) i_iw_converter (
    .clk_i, .rst_ni, .slv_req_i(slv_req), .slv_resp_o(slv_resp),
    .mst_req_o(mst_req), .mst_resp_i(mst_resp)
  );
endmodule

module opto_axi_to_mem_live (
  input  logic                                      clk_i,
  input  logic                                      rst_ni,
  input  logic [opto_pulp_axi_types::ReqBits-1:0]  axi_req_flat_i,
  output logic [opto_pulp_axi_types::RespBits-1:0] axi_resp_flat_o,
  output logic                                      busy_o,
  output logic [1:0]                                mem_req_o,
  input  logic [1:0]                                mem_gnt_i,
  output logic [1:0][15:0]                          mem_addr_o,
  output logic [1:0][15:0]                          mem_wdata_o,
  output logic [1:0][1:0]                           mem_strb_o,
  output logic [1:0][5:0]                           mem_atop_o,
  output logic [1:0]                                mem_we_o,
  input  logic [1:0]                                mem_rvalid_i,
  input  logic [1:0][15:0]                          mem_rdata_i
);
  import opto_pulp_axi_types::*;
  req_t axi_req;
  resp_t axi_resp;
  assign axi_req = axi_req_flat_i;
  assign axi_resp_flat_o = axi_resp;
  axi_to_mem #(
    .axi_req_t(req_t), .axi_resp_t(resp_t), .AddrWidth(16), .DataWidth(32),
    .IdWidth(2), .NumBanks(2), .BufDepth(2), .HideStrb(1'b1), .OutFifoDepth(2)
  ) i_to_mem (
    .clk_i, .rst_ni, .busy_o, .axi_req_i(axi_req), .axi_resp_o(axi_resp),
    .mem_req_o, .mem_gnt_i, .mem_addr_o, .mem_wdata_o, .mem_strb_o,
    .mem_atop_o, .mem_we_o, .mem_rvalid_i, .mem_rdata_i
  );
endmodule

module opto_axi_lite_xbar_live (
  input  logic                                               clk_i,
  input  logic                                               rst_ni,
  input  logic [1:0][opto_pulp_axi_types::LiteReqBits-1:0]  slv_req_flat_i,
  input  logic [1:0][opto_pulp_axi_types::LiteRespBits-1:0] mst_resp_flat_i,
  output logic [1:0][opto_pulp_axi_types::LiteRespBits-1:0] slv_resp_flat_o,
  output logic [1:0][opto_pulp_axi_types::LiteReqBits-1:0]  mst_req_flat_o
);
  import opto_pulp_axi_types::*;
  localparam axi_pkg::xbar_cfg_t XbarCfg = '{
    NoSlvPorts: 32'd2, NoMstPorts: 32'd2, MaxMstTrans: 32'd4,
    MaxSlvTrans: 32'd4, FallThrough: 1'b0, LatencyMode: axi_pkg::CUT_ALL_AX,
    AxiAddrWidth: 32'd32, AxiDataWidth: 32'd32, NoAddrRules: 32'd2,
    default: '0
  };
  localparam axi_pkg::xbar_rule_32_t [1:0] AddrMap = '{
    '{idx: 32'd1, start_addr: 32'h8000_0000, end_addr: 32'h0000_0000},
    '{idx: 32'd0, start_addr: 32'h0000_0000, end_addr: 32'h8000_0000}
  };
  lite_req_t [1:0] slv_req, mst_req;
  lite_resp_t [1:0] slv_resp, mst_resp;
  assign slv_req = slv_req_flat_i;
  assign mst_resp = mst_resp_flat_i;
  assign slv_resp_flat_o = slv_resp;
  assign mst_req_flat_o = mst_req;
  axi_lite_xbar #(
    .Cfg(XbarCfg), .aw_chan_t(lite_aw_t), .w_chan_t(lite_w_t),
    .b_chan_t(lite_b_t), .ar_chan_t(lite_ar_t), .r_chan_t(lite_r_t),
    .axi_req_t(lite_req_t), .axi_resp_t(lite_resp_t),
    .rule_t(axi_pkg::xbar_rule_32_t)
  ) i_xbar (
    .clk_i, .rst_ni, .test_i(1'b0), .slv_ports_req_i(slv_req),
    .slv_ports_resp_o(slv_resp), .mst_ports_req_o(mst_req),
    .mst_ports_resp_i(mst_resp), .addr_map_i(AddrMap),
    .en_default_mst_port_i('0), .default_mst_port_i('0)
  );
endmodule

module opto_axi_full_lite_bridge_live (
  input  logic                                      clk_i,
  input  logic                                      rst_ni,
  input  logic [opto_pulp_axi_types::ReqBits-1:0]  slv_req_flat_i,
  input  logic [opto_pulp_axi_types::RespBits-1:0] mst_resp_flat_i,
  output logic [opto_pulp_axi_types::RespBits-1:0] slv_resp_flat_o,
  output logic [opto_pulp_axi_types::ReqBits-1:0]  mst_req_flat_o
);
  import opto_pulp_axi_types::*;
  req_t slv_req, unwrap_req, mst_req;
  resp_t slv_resp, unwrap_resp, mst_resp;
  lite_req_t lite_req;
  lite_resp_t lite_resp;
  assign slv_req = slv_req_flat_i;
  assign mst_resp = mst_resp_flat_i;
  assign slv_resp_flat_o = slv_resp;
  assign mst_req_flat_o = mst_req;

  axi_burst_unwrap #(
    .MaxReadTxns(4), .MaxWriteTxns(4), .AddrWidth(32), .DataWidth(32),
    .IdWidth(2), .UserWidth(1), .axi_req_t(req_t), .axi_resp_t(resp_t)
  ) i_unwrap (
    .clk_i, .rst_ni, .slv_req_i(slv_req), .slv_resp_o(slv_resp),
    .mst_req_o(unwrap_req), .mst_resp_i(unwrap_resp)
  );

  axi_to_axi_lite #(
    .AxiAddrWidth(32), .AxiDataWidth(32), .AxiIdWidth(2), .AxiUserWidth(1),
    .AxiMaxWriteTxns(4), .AxiMaxReadTxns(4), .FullBW(1'b1),
    .FallThrough(1'b0), .full_req_t(req_t), .full_resp_t(resp_t),
    .lite_req_t(lite_req_t), .lite_resp_t(lite_resp_t)
  ) i_to_lite (
    .clk_i, .rst_ni, .test_i(1'b0), .slv_req_i(unwrap_req),
    .slv_resp_o(unwrap_resp), .mst_req_o(lite_req), .mst_resp_i(lite_resp)
  );

  axi_lite_to_axi #(
    .AxiDataWidth(32), .req_lite_t(lite_req_t), .resp_lite_t(lite_resp_t),
    .axi_req_t(req_t), .axi_resp_t(resp_t)
  ) i_from_lite (
    .slv_req_lite_i(lite_req), .slv_resp_lite_o(lite_resp),
    .slv_aw_cache_i('0), .slv_ar_cache_i('0),
    .mst_req_o(mst_req), .mst_resp_i(mst_resp)
  );
endmodule

module opto_axi_control_live (
  input  logic                                      clk_i,
  input  logic                                      rst_ni,
  input  logic [1:0]                                w_credit_i,
  input  logic [1:0]                                r_credit_i,
  input  logic [opto_pulp_axi_types::ReqBits-1:0]  slv_req_flat_i,
  input  logic [opto_pulp_axi_types::RespBits-1:0] mst_resp_flat_i,
  output logic [opto_pulp_axi_types::RespBits-1:0] slv_resp_flat_o,
  output logic [opto_pulp_axi_types::ReqBits-1:0]  mst_req_flat_o
);
  import opto_pulp_axi_types::*;
  req_t slv_req, delayed_req, throttled_req, mst_req;
  resp_t slv_resp, delayed_resp, throttled_resp, mst_resp;
  assign slv_req = slv_req_flat_i;
  assign mst_resp = mst_resp_flat_i;
  assign slv_resp_flat_o = slv_resp;
  assign mst_req_flat_o = mst_req;

  axi_delayer #(
    .aw_chan_t(aw_t), .w_chan_t(w_t), .b_chan_t(b_t), .ar_chan_t(ar_t),
    .r_chan_t(r_t), .axi_req_t(req_t), .axi_resp_t(resp_t),
    .StallRandomInput(1'b0), .StallRandomOutput(1'b0),
    .FixedDelayInput(2), .FixedDelayOutput(2)
  ) i_delay (
    .clk_i, .rst_ni, .slv_req_i(slv_req), .slv_resp_o(slv_resp),
    .mst_req_o(delayed_req), .mst_resp_i(delayed_resp)
  );

  axi_throttle #(
    .MaxNumAwPending(3), .MaxNumArPending(3),
    .axi_req_t(req_t), .axi_rsp_t(resp_t)
  ) i_throttle (
    .clk_i, .rst_ni, .req_i(delayed_req), .rsp_o(delayed_resp),
    .req_o(throttled_req), .rsp_i(throttled_resp), .w_credit_i, .r_credit_i
  );

  axi_multicut #(
    .NoCuts(2), .aw_chan_t(aw_t), .w_chan_t(w_t), .b_chan_t(b_t),
    .ar_chan_t(ar_t), .r_chan_t(r_t), .axi_req_t(req_t), .axi_resp_t(resp_t)
  ) i_multicut (
    .clk_i, .rst_ni, .slv_req_i(throttled_req), .slv_resp_o(throttled_resp),
    .mst_req_o(mst_req), .mst_resp_i(mst_resp)
  );
endmodule

module opto_axi_lite_peripherals_live (
  input  logic                                                clk_i,
  input  logic                                                rst_ni,
  input  logic [1:0][opto_pulp_axi_types::LiteReqBits-1:0]   mailbox_req_flat_i,
  input  logic [opto_pulp_axi_types::LiteReqBits-1:0]        regs_req_flat_i,
  input  logic [opto_pulp_axi_types::LiteReqBits-1:0]        apb_req_flat_i,
  input  logic [1:0][31:0]                                    mailbox_base_i,
  input  logic [7:0][7:0]                                     reg_d_i,
  input  logic [7:0]                                          reg_load_i,
  input  logic [1:0][opto_pulp_axi_types::ApbRespBits-1:0]   apb_resp_flat_i,
  output logic [1:0][opto_pulp_axi_types::LiteRespBits-1:0]  mailbox_resp_flat_o,
  output logic [opto_pulp_axi_types::LiteRespBits-1:0]       regs_resp_flat_o,
  output logic [opto_pulp_axi_types::LiteRespBits-1:0]       apb_axi_resp_flat_o,
  output logic [1:0]                                          mailbox_irq_o,
  output logic [7:0]                                          wr_active_o,
  output logic [7:0]                                          rd_active_o,
  output logic [7:0][7:0]                                     reg_q_o,
  output logic [1:0][opto_pulp_axi_types::ApbReqBits-1:0]    apb_req_flat_o
);
  import opto_pulp_axi_types::*;
  lite_req_t [1:0] mailbox_req;
  lite_resp_t [1:0] mailbox_resp;
  lite_req_t regs_req, apb_axi_req;
  lite_resp_t regs_resp, apb_axi_resp;
  apb_req_t [1:0] apb_req;
  apb_resp_t [1:0] apb_resp;
  localparam axi_pkg::xbar_rule_32_t [1:0] AddrMap = '{
    '{idx: 32'd1, start_addr: 32'h8000_0000, end_addr: 32'h0000_0000},
    '{idx: 32'd0, start_addr: 32'h0000_0000, end_addr: 32'h8000_0000}
  };
  assign mailbox_req = mailbox_req_flat_i;
  assign regs_req = regs_req_flat_i;
  assign apb_axi_req = apb_req_flat_i;
  assign apb_resp = apb_resp_flat_i;
  assign mailbox_resp_flat_o = mailbox_resp;
  assign regs_resp_flat_o = regs_resp;
  assign apb_axi_resp_flat_o = apb_axi_resp;
  assign apb_req_flat_o = apb_req;

  axi_lite_mailbox #(
    .MailboxDepth(8), .IrqEdgeTrig(1'b1), .IrqActHigh(1'b1),
    .AxiAddrWidth(32), .AxiDataWidth(32),
    .req_lite_t(lite_req_t), .resp_lite_t(lite_resp_t)
  ) i_mailbox (
    .clk_i, .rst_ni, .test_i(1'b0), .slv_reqs_i(mailbox_req),
    .slv_resps_o(mailbox_resp), .irq_o(mailbox_irq_o), .base_addr_i(mailbox_base_i)
  );

  axi_lite_regs #(
    .RegNumBytes(8), .AxiAddrWidth(32), .AxiDataWidth(32),
    .PrivProtOnly(1'b1), .SecuProtOnly(1'b0),
    .AxiReadOnly(8'b1000_0001), .req_lite_t(lite_req_t),
    .resp_lite_t(lite_resp_t)
  ) i_regs (
    .clk_i, .rst_ni, .axi_req_i(regs_req), .axi_resp_o(regs_resp),
    .wr_active_o, .rd_active_o, .reg_d_i, .reg_load_i, .reg_q_o
  );

  axi_lite_to_apb #(
    .NoApbSlaves(2), .NoRules(2), .AddrWidth(32), .DataWidth(32),
    .PipelineRequest(1'b1), .PipelineResponse(1'b1),
    .axi_lite_req_t(lite_req_t), .axi_lite_resp_t(lite_resp_t),
    .apb_req_t(apb_req_t), .apb_resp_t(apb_resp_t),
    .rule_t(axi_pkg::xbar_rule_32_t)
  ) i_to_apb (
    .clk_i, .rst_ni, .axi_lite_req_i(apb_axi_req),
    .axi_lite_resp_o(apb_axi_resp), .apb_req_o(apb_req),
    .apb_resp_i(apb_resp), .addr_map_i(AddrMap)
  );
endmodule

module opto_axi_full_xbar_live (
  input  logic                                                 clk_i,
  input  logic                                                 rst_ni,
  input  logic [1:0][opto_pulp_axi_types::ReqBits-1:0]        slv_req_flat_i,
  input  logic [1:0][opto_pulp_axi_types::WideIdRespBits-1:0] mst_resp_flat_i,
  output logic [1:0][opto_pulp_axi_types::RespBits-1:0]       slv_resp_flat_o,
  output logic [1:0][opto_pulp_axi_types::WideIdReqBits-1:0]  mst_req_flat_o
);
  import opto_pulp_axi_types::*;
  localparam axi_pkg::xbar_cfg_t XbarCfg = '{
    NoSlvPorts: 32'd2, NoMstPorts: 32'd2, MaxMstTrans: 32'd4,
    MaxSlvTrans: 32'd4, FallThrough: 1'b0, LatencyMode: axi_pkg::CUT_ALL_AX,
    AxiIdWidthSlvPorts: 32'd2, AxiIdUsedSlvPorts: 32'd2,
    AxiAddrWidth: 32'd32, AxiDataWidth: 32'd32, NoAddrRules: 32'd2,
    default: '0
  };
  localparam axi_pkg::xbar_rule_32_t [1:0] AddrMap = '{
    '{idx: 32'd1, start_addr: 32'h8000_0000, end_addr: 32'h0000_0000},
    '{idx: 32'd0, start_addr: 32'h0000_0000, end_addr: 32'h8000_0000}
  };
  req_t [1:0] slv_req;
  resp_t [1:0] slv_resp;
  wide_id_req_t [1:0] mst_req;
  wide_id_resp_t [1:0] mst_resp;
  assign slv_req = slv_req_flat_i;
  assign mst_resp = mst_resp_flat_i;
  assign slv_resp_flat_o = slv_resp;
  assign mst_req_flat_o = mst_req;

  axi_xbar #(
    .Cfg(XbarCfg), .ATOPs(1'b1), .slv_aw_chan_t(aw_t),
    .mst_aw_chan_t(wide_id_aw_t), .w_chan_t(w_t), .slv_b_chan_t(b_t),
    .mst_b_chan_t(wide_id_b_t), .slv_ar_chan_t(ar_t),
    .mst_ar_chan_t(wide_id_ar_t), .slv_r_chan_t(r_t),
    .mst_r_chan_t(wide_id_r_t), .slv_req_t(req_t), .slv_resp_t(resp_t),
    .mst_req_t(wide_id_req_t), .mst_resp_t(wide_id_resp_t),
    .rule_t(axi_pkg::xbar_rule_32_t)
  ) i_xbar (
    .clk_i, .rst_ni, .test_i(1'b0), .slv_ports_req_i(slv_req),
    .slv_ports_resp_o(slv_resp), .mst_ports_req_o(mst_req),
    .mst_ports_resp_i(mst_resp), .addr_map_i(AddrMap),
    .en_default_mst_port_i('0), .default_mst_port_i('0)
  );
endmodule
