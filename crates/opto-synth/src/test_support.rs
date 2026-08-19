// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Explicit builders shared by cross-stage synthesis contract tests.
//!
//! Domain assertions do not live here. Each test remains under the
//! architecture domain that owns its primary observable behavior.

pub(crate) use super::*;
pub(crate) use opto_formats::{AreaCellKind, AreaReportContext, FormatError, report_qor};
pub(crate) use opto_ir::ConstBits;
pub(crate) use opto_ir::proc::{
    AssignmentMode, ProcBuilder, ProcModule, ProcTarget, ProcedureKind, SensitivityEvent,
};
pub(crate) use opto_ir::word::{
    self, BinaryOp, Edge, LValue, LogicStateKind, PortDirection, SourceSpan, UnaryOp, WordModule,
    WordType,
};
use std::ops::{Deref, DerefMut};

pub(crate) fn write_verilog(module: &WordModule) -> Result<String, FormatError> {
    let mut output = Vec::new();
    opto_formats::write_verilog(&mut output, module)?;
    Ok(String::from_utf8(output).expect("Verilog writer only emits UTF-8 text"))
}

pub(crate) fn bit() -> WordType {
    WordType::new(1, false, LogicStateKind::FourState).unwrap()
}

pub(crate) fn test_span() -> SourceSpan {
    SourceSpan::stable("test")
}

/// What one synthesized test design exposes.
///
/// Mapped cells live in the mapped netlist, not in the Word revision, so a test
/// that inspects the implementation reads `mapped`. `report` and the replaced
/// Word module remain for tests that assert on source-level structure.
#[derive(Debug)]
pub(crate) struct SynthesizedTest {
    pub(crate) report: SynthesisReport,
    pub(crate) mapped: opto_ir::mapped::MappedNetlist,
}

impl SynthesizedTest {
    /// Renders the mapped netlist the way a published artifact is written.
    pub(crate) fn mapped_verilog(&self) -> String {
        let mut output = Vec::new();
        opto_formats::write_mapped_verilog(&mut output, &self.mapped)
            .expect("mapped test netlist renders");
        String::from_utf8(output).expect("mapped Verilog is UTF-8")
    }
}

pub(crate) fn synthesize_test_module<M: TestSource>(
    module: &mut M,
    options: SynthesisOptions,
) -> Result<SynthesizedTest, crate::SynthError> {
    let rtl = module.rtl()?;
    let result = super::synthesize_rtl_module(rtl, options, test_runtime())?;
    let (synthesized, report, mapped) = result.into_module_and_report();
    module.replace_word(synthesized);
    Ok(SynthesizedTest { report, mapped })
}

pub(crate) trait TestSource {
    fn rtl(&self) -> Result<opto_ir::rtl::RtlModule, crate::SynthError>;
    fn replace_word(&mut self, word: WordModule);
}

impl TestSource for WordModule {
    fn rtl(&self) -> Result<opto_ir::rtl::RtlModule, crate::SynthError> {
        opto_ir::rtl::RtlModule::structural(self.clone())
            .map_err(|error| crate::SynthError::invalid(error.to_string()))
    }

    fn replace_word(&mut self, word: WordModule) {
        *self = word;
    }
}

pub(crate) struct TestModule {
    word: WordModule,
    pub(crate) procedures: ProcModule,
}

impl TestModule {
    fn new(word: WordModule, procedures: ProcBuilder) -> Self {
        Self {
            word,
            procedures: procedures.seal().unwrap(),
        }
    }
}

impl Deref for TestModule {
    type Target = WordModule;

    fn deref(&self) -> &Self::Target {
        &self.word
    }
}

impl DerefMut for TestModule {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.word
    }
}

impl TestSource for TestModule {
    fn rtl(&self) -> Result<opto_ir::rtl::RtlModule, crate::SynthError> {
        opto_ir::rtl::RtlModule::new(self.word.clone(), self.procedures.clone())
            .map_err(|error| crate::SynthError::invalid(error.to_string()))
    }

    fn replace_word(&mut self, word: WordModule) {
        self.word = word;
        self.procedures = ProcModule::default();
    }
}

pub(crate) fn read_port(
    module: &mut WordModule,
    port: opto_ir::word::PortId,
) -> opto_ir::word::ValueId {
    module
        .read_signal(module.port(port).unwrap().signal, test_span())
        .unwrap()
}

pub(crate) fn connect_port(
    module: &mut WordModule,
    port: opto_ir::word::PortId,
    value: opto_ir::word::ValueId,
) {
    module
        .connect(
            LValue::signal(module.port(port).unwrap().signal),
            value,
            test_span(),
        )
        .unwrap();
}

pub(crate) fn target_cell(
    name: &str,
    area: f64,
    pins: &[(&str, TargetPinDirection, Option<&str>)],
) -> TargetCell {
    TargetCell {
        dont_use: false,
        usage: opto_library::TargetCellUsage::default(),
        name: name.to_string(),
        area: Some(area),
        sequential: Vec::new(),
        pins: pins
            .iter()
            .map(|(name, direction, function)| TargetPin {
                name: (*name).to_string(),
                direction: *direction,
                function: function.map(|function| BooleanFunction::parse(function).unwrap()),
                three_state: None,
                capacitance: None,
                rise_capacitance: None,
                fall_capacitance: None,
                receiver_capacitance: None,
                fanout_load: None,
                next_state_type: None,
                timing_arcs: Vec::new(),
                clock_gate_role: None,
            })
            .collect(),
        clock_gate: None,
        memory: None,
    }
}

pub(crate) fn single_assignment(
    mut module: WordModule,
    kind: ProcedureKind,
    clock: Option<word::SignalId>,
    target: word::SignalId,
    value: word::ValueId,
    mode: AssignmentMode,
) -> TestModule {
    let mut cfg = ProcBuilder::new();
    let clock = clock.map(|signal| module.read_signal(signal, test_span()).unwrap());
    let procedure = match clock {
        Some(value) => cfg.add_clocked_procedure(
            [SensitivityEvent {
                value,
                edge: Edge::Pos,
                iff: None,
            }],
            test_span(),
        ),
        None => cfg.add_combinational_procedure(kind, test_span()),
    }
    .unwrap();
    let block = cfg.add_block(procedure, test_span()).unwrap();
    cfg.assign(block, mode, ProcTarget::signal(target), value, test_span())
        .unwrap();
    cfg.terminate_return(block, test_span()).unwrap();
    TestModule::new(module, cfg)
}

pub(crate) fn conditional_assignment(
    mut module: WordModule,
    kind: ProcedureKind,
    clock: Option<word::SignalId>,
    condition: word::ValueId,
    target: word::SignalId,
    value: word::ValueId,
    mode: AssignmentMode,
) -> TestModule {
    let mut cfg = ProcBuilder::new();
    let clock = clock.map(|signal| module.read_signal(signal, test_span()).unwrap());
    let procedure = match clock {
        Some(value) => cfg.add_clocked_procedure(
            [SensitivityEvent {
                value,
                edge: Edge::Pos,
                iff: None,
            }],
            test_span(),
        ),
        None => cfg.add_combinational_procedure(kind, test_span()),
    }
    .unwrap();
    let entry = cfg.add_block(procedure, test_span()).unwrap();
    let update = cfg.add_block(procedure, test_span()).unwrap();
    let hold = cfg.add_block(procedure, test_span()).unwrap();
    let exit = cfg.add_block(procedure, test_span()).unwrap();
    cfg.terminate_branch(entry, condition, update, hold, test_span())
        .unwrap();
    cfg.assign(update, mode, ProcTarget::signal(target), value, test_span())
        .unwrap();
    cfg.terminate_jump(update, exit, test_span()).unwrap();
    cfg.terminate_jump(hold, exit, test_span()).unwrap();
    cfg.terminate_return(exit, test_span()).unwrap();
    TestModule::new(module, cfg)
}

#[derive(Clone, Copy)]
struct ResetEnableFixture {
    kind: ProcedureKind,
    clock: Option<word::SignalId>,
    reset: word::ValueId,
    enable: Option<word::ValueId>,
    target: word::SignalId,
    reset_value: word::ValueId,
    data: word::ValueId,
}

fn reset_enable_module(mut module: WordModule, fixture: ResetEnableFixture) -> TestModule {
    let ResetEnableFixture {
        kind,
        clock,
        reset,
        enable,
        target,
        reset_value,
        data,
    } = fixture;
    let mut cfg = ProcBuilder::new();
    let clock = clock.map(|signal| module.read_signal(signal, test_span()).unwrap());
    let procedure = match clock {
        Some(value) => cfg.add_clocked_procedure(
            [SensitivityEvent {
                value,
                edge: Edge::Pos,
                iff: None,
            }],
            test_span(),
        ),
        None => cfg.add_combinational_procedure(kind, test_span()),
    }
    .unwrap();
    let entry = cfg.add_block(procedure, test_span()).unwrap();
    let reset_block = cfg.add_block(procedure, test_span()).unwrap();
    let data_entry = cfg.add_block(procedure, test_span()).unwrap();
    let data_block = cfg.add_block(procedure, test_span()).unwrap();
    let hold = enable.map(|_| cfg.add_block(procedure, test_span()).unwrap());
    let exit = cfg.add_block(procedure, test_span()).unwrap();
    cfg.terminate_branch(entry, reset, reset_block, data_entry, test_span())
        .unwrap();
    cfg.assign(
        reset_block,
        AssignmentMode::Nonblocking,
        ProcTarget::signal(target),
        reset_value,
        SourceSpan::stable("reset assignment"),
    )
    .unwrap();
    cfg.terminate_jump(reset_block, exit, test_span()).unwrap();
    if let Some(enable) = enable {
        cfg.terminate_branch(data_entry, enable, data_block, hold.unwrap(), test_span())
            .unwrap();
        cfg.terminate_jump(hold.unwrap(), exit, test_span())
            .unwrap();
    } else {
        cfg.terminate_jump(data_entry, data_block, test_span())
            .unwrap();
    }
    cfg.assign(
        data_block,
        AssignmentMode::Nonblocking,
        ProcTarget::signal(target),
        data,
        SourceSpan::stable("data assignment"),
    )
    .unwrap();
    cfg.terminate_jump(data_block, exit, test_span()).unwrap();
    cfg.terminate_return(exit, test_span()).unwrap();
    TestModule::new(module, cfg)
}

pub(crate) fn module_with_process(blocking: bool) -> TestModule {
    let mut module = WordModule::new("top");
    let a = module
        .add_port("a", PortDirection::Input, bit(), test_span())
        .unwrap();
    let y = module
        .add_port("y", PortDirection::Output, bit(), test_span())
        .unwrap();
    let a_signal = module.port(a).unwrap().signal;
    let y_signal = module.port(y).unwrap().signal;
    let value = module
        .read_signal(a_signal, SourceSpan::stable("signal read"))
        .unwrap();
    single_assignment(
        module,
        ProcedureKind::Combinational,
        None,
        y_signal,
        value,
        if blocking {
            AssignmentMode::Blocking
        } else {
            AssignmentMode::Nonblocking
        },
    )
}

pub(crate) fn module_with_schedule_sensitive_nonblocking_process() -> TestModule {
    let mut module = WordModule::new("top");
    let a = module
        .add_port("a", PortDirection::Input, bit(), test_span())
        .unwrap();
    let y = module
        .add_port("y", PortDirection::Output, bit(), test_span())
        .unwrap();
    let observed = module
        .add_port("observed", PortDirection::Output, bit(), test_span())
        .unwrap();
    let a_value = read_port(&mut module, a);
    let y_value = read_port(&mut module, y);
    let y_signal = module.port(y).unwrap().signal;
    let observed_signal = module.port(observed).unwrap().signal;
    let mut cfg = ProcBuilder::new();
    let procedure = cfg
        .add_combinational_procedure(ProcedureKind::Combinational, test_span())
        .unwrap();
    let block = cfg.add_block(procedure, test_span()).unwrap();
    cfg.assign(
        block,
        AssignmentMode::Nonblocking,
        ProcTarget::signal(y_signal),
        a_value,
        SourceSpan::stable("schedule-sensitive nonblocking assignment"),
    )
    .unwrap();
    cfg.assign(
        block,
        AssignmentMode::Blocking,
        ProcTarget::signal(observed_signal),
        y_value,
        test_span(),
    )
    .unwrap();
    cfg.terminate_return(block, test_span()).unwrap();
    TestModule::new(module, cfg)
}

pub(crate) fn module_with_flop_process() -> TestModule {
    let mut module = WordModule::new("top");
    let clk = module
        .add_port("clk", PortDirection::Input, bit(), test_span())
        .unwrap();
    let d = module
        .add_port("d", PortDirection::Input, bit(), test_span())
        .unwrap();
    let q = module
        .add_port("q", PortDirection::Output, bit(), test_span())
        .unwrap();
    let d_value = module
        .read_signal(module.port(d).unwrap().signal, test_span())
        .unwrap();
    let clock = module.port(clk).unwrap().signal;
    let target = module.port(q).unwrap().signal;
    single_assignment(
        module,
        ProcedureKind::FlipFlop,
        Some(clock),
        target,
        d_value,
        AssignmentMode::Nonblocking,
    )
}

pub(crate) fn module_with_latch_process() -> TestModule {
    let mut module = WordModule::new("top");
    let enable = module
        .add_port("en", PortDirection::Input, bit(), test_span())
        .unwrap();
    let data = module
        .add_port("d", PortDirection::Input, bit(), test_span())
        .unwrap();
    let q = module
        .add_port("q", PortDirection::Output, bit(), test_span())
        .unwrap();
    let enable = read_port(&mut module, enable);
    let data = read_port(&mut module, data);
    let target = module.port(q).unwrap().signal;
    conditional_assignment(
        module,
        ProcedureKind::Latch,
        None,
        enable,
        target,
        data,
        AssignmentMode::Nonblocking,
    )
}

pub(crate) fn module_with_reset_latch_process() -> TestModule {
    let mut module = WordModule::new("top");
    let reset = module
        .add_port("reset", PortDirection::Input, bit(), test_span())
        .unwrap();
    let enable = module
        .add_port("en", PortDirection::Input, bit(), test_span())
        .unwrap();
    let data = module
        .add_port("d", PortDirection::Input, bit(), test_span())
        .unwrap();
    let q = module
        .add_port("q", PortDirection::Output, bit(), test_span())
        .unwrap();
    let reset = read_port(&mut module, reset);
    let enable = read_port(&mut module, enable);
    let data = read_port(&mut module, data);
    let zero = module
        .constant(ConstBits::from_bin_str("0").unwrap(), bit(), test_span())
        .unwrap();
    let target = module.port(q).unwrap().signal;
    reset_enable_module(
        module,
        ResetEnableFixture {
            kind: ProcedureKind::Latch,
            clock: None,
            reset,
            enable: Some(enable),
            target,
            reset_value: zero,
            data,
        },
    )
}

pub(crate) fn module_with_enable_flop_process() -> TestModule {
    let mut module = WordModule::new("top");
    let clk = module
        .add_port("clk", PortDirection::Input, bit(), test_span())
        .unwrap();
    let enable = module
        .add_port("en", PortDirection::Input, bit(), test_span())
        .unwrap();
    let data = module
        .add_port("d", PortDirection::Input, bit(), test_span())
        .unwrap();
    let q = module
        .add_port("q", PortDirection::Output, bit(), test_span())
        .unwrap();
    let enable = read_port(&mut module, enable);
    let data = read_port(&mut module, data);
    let clock = module.port(clk).unwrap().signal;
    let target = module.port(q).unwrap().signal;
    conditional_assignment(
        module,
        ProcedureKind::FlipFlop,
        Some(clock),
        enable,
        target,
        data,
        AssignmentMode::Nonblocking,
    )
}

pub(crate) fn module_with_lowered_feedback_mux_flop() -> WordModule {
    let mut module = WordModule::new("top");
    let clk = module
        .add_port("clk", PortDirection::Input, bit(), test_span())
        .unwrap();
    let enable = module
        .add_port("en", PortDirection::Input, bit(), test_span())
        .unwrap();
    let data = module
        .add_port("d", PortDirection::Input, bit(), test_span())
        .unwrap();
    let q = module
        .add_port("q", PortDirection::Output, bit(), test_span())
        .unwrap();
    let clock = read_port(&mut module, clk);
    let enable = read_port(&mut module, enable);
    let data = read_port(&mut module, data);
    let q_signal = module.port(q).unwrap().signal;
    let feedback = module.read_signal(q_signal, test_span()).unwrap();
    let next = module.mux(enable, data, feedback, test_span()).unwrap();
    let register = module
        .register(
            word::RegisterOp {
                name: None,
                d: next,
                clock,
                edge: Edge::Pos,
                enable: None,
                resets: Vec::new(),
            },
            test_span(),
        )
        .unwrap();
    module
        .connect(LValue::signal(q_signal), register, test_span())
        .unwrap();
    module
}

pub(crate) fn module_with_sync_reset_flop_process() -> TestModule {
    module_with_sync_reset_control(false)
}

pub(crate) fn module_with_sync_reset_enable_flop_process() -> TestModule {
    module_with_sync_reset_control(true)
}

pub(crate) fn module_with_sync_reset_control(with_enable: bool) -> TestModule {
    let mut module = WordModule::new("top");
    let clk = module
        .add_port("clk", PortDirection::Input, bit(), test_span())
        .unwrap();
    let reset = module
        .add_port("reset", PortDirection::Input, bit(), test_span())
        .unwrap();
    let enable = with_enable
        .then(|| module.add_port("en", PortDirection::Input, bit(), test_span()))
        .transpose()
        .unwrap();
    let data = module
        .add_port("d", PortDirection::Input, bit(), test_span())
        .unwrap();
    let q = module
        .add_port("q", PortDirection::Output, bit(), test_span())
        .unwrap();
    let reset = read_port(&mut module, reset);
    let enable = enable.map(|enable| read_port(&mut module, enable));
    let data = read_port(&mut module, data);
    let zero = module
        .constant(ConstBits::from_bin_str("0").unwrap(), bit(), test_span())
        .unwrap();
    let q_signal = module.port(q).unwrap().signal;
    let clock = module.port(clk).unwrap().signal;
    reset_enable_module(
        module,
        ResetEnableFixture {
            kind: ProcedureKind::FlipFlop,
            clock: Some(clock),
            reset,
            enable,
            target: q_signal,
            reset_value: zero,
            data,
        },
    )
}

pub(crate) fn module_with_prioritized_constant_updates() -> TestModule {
    let mut module = WordModule::new("top");
    let clk = module
        .add_port("clk", PortDirection::Input, bit(), test_span())
        .unwrap();
    let first = module
        .add_port("first", PortDirection::Input, bit(), test_span())
        .unwrap();
    let second = module
        .add_port("second", PortDirection::Input, bit(), test_span())
        .unwrap();
    let q = module
        .add_port("q", PortDirection::Output, bit(), test_span())
        .unwrap();
    let first = read_port(&mut module, first);
    let second = read_port(&mut module, second);
    let zero = module
        .constant(ConstBits::from_bin_str("0").unwrap(), bit(), test_span())
        .unwrap();
    let one = module
        .constant(ConstBits::from_bin_str("1").unwrap(), bit(), test_span())
        .unwrap();
    let target = module.port(q).unwrap().signal;
    let clock = module.port(clk).unwrap().signal;
    let clock = module.read_signal(clock, test_span()).unwrap();

    let mut cfg = ProcBuilder::new();
    let procedure = cfg
        .add_clocked_procedure(
            [SensitivityEvent {
                value: clock,
                edge: Edge::Pos,
                iff: None,
            }],
            test_span(),
        )
        .unwrap();
    let entry = cfg.add_block(procedure, test_span()).unwrap();
    let first_update = cfg.add_block(procedure, test_span()).unwrap();
    let first_hold = cfg.add_block(procedure, test_span()).unwrap();
    let second_test = cfg.add_block(procedure, test_span()).unwrap();
    let second_update = cfg.add_block(procedure, test_span()).unwrap();
    let second_hold = cfg.add_block(procedure, test_span()).unwrap();
    let exit = cfg.add_block(procedure, test_span()).unwrap();

    cfg.terminate_branch(entry, first, first_update, first_hold, test_span())
        .unwrap();
    cfg.assign(
        first_update,
        AssignmentMode::Nonblocking,
        ProcTarget::signal(target),
        zero,
        test_span(),
    )
    .unwrap();
    cfg.terminate_jump(first_update, second_test, test_span())
        .unwrap();
    cfg.terminate_jump(first_hold, second_test, test_span())
        .unwrap();
    cfg.terminate_branch(second_test, second, second_update, second_hold, test_span())
        .unwrap();
    cfg.assign(
        second_update,
        AssignmentMode::Nonblocking,
        ProcTarget::signal(target),
        one,
        test_span(),
    )
    .unwrap();
    cfg.terminate_jump(second_update, exit, test_span())
        .unwrap();
    cfg.terminate_jump(second_hold, exit, test_span()).unwrap();
    cfg.terminate_return(exit, test_span()).unwrap();
    TestModule::new(module, cfg)
}

pub(crate) fn module_with_nested_async_controls() -> TestModule {
    let mut module = WordModule::new("top");
    let clk = module
        .add_port("clk", PortDirection::Input, bit(), test_span())
        .unwrap();
    let clear = module
        .add_port("clear", PortDirection::Input, bit(), test_span())
        .unwrap();
    let set = module
        .add_port("set", PortDirection::Input, bit(), test_span())
        .unwrap();
    let data = module
        .add_port("d", PortDirection::Input, bit(), test_span())
        .unwrap();
    let q = module
        .add_port("q", PortDirection::Output, bit(), test_span())
        .unwrap();
    let clock = read_port(&mut module, clk);
    let clear = read_port(&mut module, clear);
    let set = read_port(&mut module, set);
    let data = read_port(&mut module, data);
    let zero = module
        .constant(ConstBits::from_bin_str("0").unwrap(), bit(), test_span())
        .unwrap();
    let one = module
        .constant(ConstBits::from_bin_str("1").unwrap(), bit(), test_span())
        .unwrap();
    let target = module.port(q).unwrap().signal;
    let mut cfg = ProcBuilder::new();
    let procedure = cfg
        .add_clocked_procedure(
            [
                SensitivityEvent {
                    value: clock,
                    edge: Edge::Pos,
                    iff: None,
                },
                SensitivityEvent {
                    value: clear,
                    edge: Edge::Pos,
                    iff: None,
                },
                SensitivityEvent {
                    value: set,
                    edge: Edge::Pos,
                    iff: None,
                },
            ],
            test_span(),
        )
        .unwrap();
    let entry = cfg.add_block(procedure, test_span()).unwrap();
    let clear_update = cfg.add_block(procedure, test_span()).unwrap();
    let data_update = cfg.add_block(procedure, test_span()).unwrap();
    let set_test = cfg.add_block(procedure, test_span()).unwrap();
    let set_update = cfg.add_block(procedure, test_span()).unwrap();
    let exit = cfg.add_block(procedure, test_span()).unwrap();
    cfg.terminate_branch(entry, clear, clear_update, data_update, test_span())
        .unwrap();
    cfg.assign(
        clear_update,
        AssignmentMode::Nonblocking,
        ProcTarget::signal(target),
        zero,
        test_span(),
    )
    .unwrap();
    cfg.terminate_jump(clear_update, set_test, test_span())
        .unwrap();
    cfg.assign(
        data_update,
        AssignmentMode::Nonblocking,
        ProcTarget::signal(target),
        data,
        test_span(),
    )
    .unwrap();
    cfg.terminate_jump(data_update, set_test, test_span())
        .unwrap();
    cfg.terminate_branch(set_test, set, set_update, exit, test_span())
        .unwrap();
    cfg.assign(
        set_update,
        AssignmentMode::Nonblocking,
        ProcTarget::signal(target),
        one,
        test_span(),
    )
    .unwrap();
    cfg.terminate_jump(set_update, exit, test_span()).unwrap();
    cfg.terminate_return(exit, test_span()).unwrap();
    TestModule::new(module, cfg)
}

pub(crate) fn module_with_vector_flop_process(width: u32) -> TestModule {
    let mut module = WordModule::new("top");
    let ty = WordType::new(width, false, LogicStateKind::FourState).unwrap();
    let clk = module
        .add_port("clk", PortDirection::Input, bit(), test_span())
        .unwrap();
    let d = module
        .add_port("d", PortDirection::Input, ty, test_span())
        .unwrap();
    let q = module
        .add_port("q", PortDirection::Output, ty, test_span())
        .unwrap();
    let d_value = module
        .read_signal(module.port(d).unwrap().signal, test_span())
        .unwrap();
    let clock = module.port(clk).unwrap().signal;
    let target = module.port(q).unwrap().signal;
    single_assignment(
        module,
        ProcedureKind::FlipFlop,
        Some(clock),
        target,
        d_value,
        AssignmentMode::Nonblocking,
    )
}

pub(crate) fn module_with_inverted_flop_output() -> WordModule {
    let mut module = WordModule::new("top");
    let clock = module
        .add_port("clk", PortDirection::Input, bit(), test_span())
        .unwrap();
    let data = module
        .add_port("d", PortDirection::Input, bit(), test_span())
        .unwrap();
    let direct = module
        .add_port("q", PortDirection::Output, bit(), test_span())
        .unwrap();
    let inverted = module
        .add_port("y", PortDirection::Output, bit(), test_span())
        .unwrap();
    let data_value = read_port(&mut module, data);
    let clock_value = read_port(&mut module, clock);
    let register = module
        .register(
            word::RegisterOp {
                name: None,
                d: data_value,
                clock: clock_value,
                edge: Edge::Pos,
                enable: None,
                resets: Vec::new(),
            },
            test_span(),
        )
        .unwrap();
    connect_port(&mut module, direct, register);
    let direct_value = read_port(&mut module, direct);
    let inverted_value = module
        .unary(UnaryOp::BitNot, direct_value, test_span())
        .unwrap();
    connect_port(&mut module, inverted, inverted_value);
    module
}

pub(crate) fn simple_dff_target_cell() -> TargetCell {
    TargetCell {
        dont_use: false,
        usage: opto_library::TargetCellUsage::default(),
        name: "DFD1".to_string(),
        area: Some(2.142),
        pins: vec![
            TargetPin {
                name: "CP".to_string(),
                direction: TargetPinDirection::Input,
                function: None,
                three_state: None,
                capacitance: Some(0.4),
                rise_capacitance: None,
                fall_capacitance: None,
                receiver_capacitance: None,
                fanout_load: None,
                next_state_type: None,
                timing_arcs: Vec::new(),
                clock_gate_role: None,
            },
            TargetPin {
                name: "D".to_string(),
                direction: TargetPinDirection::Input,
                function: None,
                three_state: None,
                capacitance: Some(0.5),
                rise_capacitance: None,
                fall_capacitance: None,
                receiver_capacitance: None,
                fanout_load: None,
                next_state_type: None,
                timing_arcs: Vec::new(),
                clock_gate_role: None,
            },
            TargetPin {
                name: "Q".to_string(),
                direction: TargetPinDirection::Output,
                function: Some(BooleanFunction::parse("IQ").unwrap()),
                three_state: None,
                capacitance: None,
                rise_capacitance: None,
                fall_capacitance: None,
                receiver_capacitance: None,
                fanout_load: None,
                next_state_type: None,
                timing_arcs: Vec::new(),
                clock_gate_role: None,
            },
        ],
        sequential: vec![TargetSequential {
            kind: TargetSequentialKind::FlipFlop,
            state_variables: vec!["IQ".to_string(), "IQN".to_string()],
            clocked_on: Some(BooleanFunction::parse("CP").unwrap()),
            next_state: Some(BooleanFunction::parse("D").unwrap()),
            enable: None,
            clear: None,
            preset: None,
        }],
        clock_gate: None,
        memory: None,
    }
}

pub(crate) fn simple_latch_target_cell() -> TargetCell {
    TargetCell {
        dont_use: false,
        usage: opto_library::TargetCellUsage::default(),
        name: "LHQD1".to_string(),
        area: Some(1.8),
        pins: vec![
            TargetPin {
                name: "E".to_string(),
                direction: TargetPinDirection::Input,
                function: None,
                three_state: None,
                capacitance: Some(0.3),
                rise_capacitance: None,
                fall_capacitance: None,
                receiver_capacitance: None,
                fanout_load: None,
                next_state_type: None,
                timing_arcs: Vec::new(),
                clock_gate_role: None,
            },
            TargetPin {
                name: "D".to_string(),
                direction: TargetPinDirection::Input,
                function: None,
                three_state: None,
                capacitance: Some(0.4),
                rise_capacitance: None,
                fall_capacitance: None,
                receiver_capacitance: None,
                fanout_load: None,
                next_state_type: Some(TargetNextStateType::Data),
                timing_arcs: Vec::new(),
                clock_gate_role: None,
            },
            TargetPin {
                name: "Q".to_string(),
                direction: TargetPinDirection::Output,
                function: Some(BooleanFunction::parse("IQ").unwrap()),
                three_state: None,
                capacitance: None,
                rise_capacitance: None,
                fall_capacitance: None,
                receiver_capacitance: None,
                fanout_load: None,
                next_state_type: None,
                timing_arcs: Vec::new(),
                clock_gate_role: None,
            },
        ],
        sequential: vec![TargetSequential {
            kind: TargetSequentialKind::Latch,
            state_variables: vec!["IQ".to_string(), "IQN".to_string()],
            clocked_on: None,
            next_state: Some(BooleanFunction::parse("D").unwrap()),
            enable: Some(BooleanFunction::parse("E").unwrap()),
            clear: None,
            preset: None,
        }],
        clock_gate: None,
        memory: None,
    }
}

pub(crate) fn clear_latch_target_cell() -> TargetCell {
    let mut cell = simple_latch_target_cell();
    cell.name = "LHQD1R".to_string();
    cell.pins.insert(
        2,
        TargetPin {
            name: "R".to_string(),
            direction: TargetPinDirection::Input,
            function: None,
            three_state: None,
            capacitance: Some(0.2),
            rise_capacitance: None,
            fall_capacitance: None,
            receiver_capacitance: None,
            fanout_load: None,
            next_state_type: Some(TargetNextStateType::Clear),
            timing_arcs: Vec::new(),
            clock_gate_role: None,
        },
    );
    cell.sequential[0].clear = Some(BooleanFunction::parse("R").unwrap());
    cell
}

pub(crate) fn enable_dff_target_cell() -> TargetCell {
    TargetCell {
        dont_use: false,
        usage: opto_library::TargetCellUsage::default(),
        name: "EDFD1".to_string(),
        area: Some(2.8),
        pins: vec![
            TargetPin {
                name: "CP".to_string(),
                direction: TargetPinDirection::Input,
                function: None,
                three_state: None,
                capacitance: Some(0.4),
                rise_capacitance: None,
                fall_capacitance: None,
                receiver_capacitance: None,
                fanout_load: None,
                next_state_type: None,
                timing_arcs: Vec::new(),
                clock_gate_role: None,
            },
            TargetPin {
                name: "D".to_string(),
                direction: TargetPinDirection::Input,
                function: None,
                three_state: None,
                capacitance: Some(0.5),
                rise_capacitance: None,
                fall_capacitance: None,
                receiver_capacitance: None,
                fanout_load: None,
                next_state_type: None,
                timing_arcs: Vec::new(),
                clock_gate_role: None,
            },
            TargetPin {
                name: "DE".to_string(),
                direction: TargetPinDirection::Input,
                function: None,
                three_state: None,
                capacitance: Some(0.3),
                rise_capacitance: None,
                fall_capacitance: None,
                receiver_capacitance: None,
                fanout_load: None,
                next_state_type: None,
                timing_arcs: Vec::new(),
                clock_gate_role: None,
            },
            TargetPin {
                name: "Q".to_string(),
                direction: TargetPinDirection::Output,
                function: Some(BooleanFunction::parse("IQ").unwrap()),
                three_state: None,
                capacitance: None,
                rise_capacitance: None,
                fall_capacitance: None,
                receiver_capacitance: None,
                fanout_load: None,
                next_state_type: None,
                timing_arcs: Vec::new(),
                clock_gate_role: None,
            },
        ],
        sequential: vec![TargetSequential {
            kind: TargetSequentialKind::FlipFlop,
            state_variables: vec!["IQ".to_string(), "IQN".to_string()],
            clocked_on: Some(BooleanFunction::parse("CP").unwrap()),
            next_state: Some(BooleanFunction::parse("(D*DE)+(IQ*!DE)").unwrap()),
            enable: None,
            clear: None,
            preset: None,
        }],
        clock_gate: None,
        memory: None,
    }
}

pub(crate) fn dual_output_dff_target_cell() -> TargetCell {
    let mut cell = simple_dff_target_cell();
    cell.name = "DFDQN".to_string();
    cell.area = Some(2.3);
    cell.pins.push(TargetPin {
        name: "QN".to_string(),
        direction: TargetPinDirection::Output,
        function: Some(BooleanFunction::parse("IQN").unwrap()),
        three_state: None,
        capacitance: None,
        rise_capacitance: None,
        fall_capacitance: None,
        receiver_capacitance: None,
        fanout_load: None,
        next_state_type: None,
        timing_arcs: Vec::new(),
        clock_gate_role: None,
    });
    cell
}

pub(crate) fn module_with_if_process() -> TestModule {
    let mut module = WordModule::new("top");
    let sel = module
        .add_port("sel", PortDirection::Input, bit(), test_span())
        .unwrap();
    let a = module
        .add_port("a", PortDirection::Input, bit(), test_span())
        .unwrap();
    let b = module
        .add_port("b", PortDirection::Input, bit(), test_span())
        .unwrap();
    let y = module
        .add_port("y", PortDirection::Output, bit(), test_span())
        .unwrap();
    let sel_value = module
        .read_signal(module.port(sel).unwrap().signal, test_span())
        .unwrap();
    let a_value = module
        .read_signal(module.port(a).unwrap().signal, test_span())
        .unwrap();
    let b_value = module
        .read_signal(module.port(b).unwrap().signal, test_span())
        .unwrap();
    let target = module.port(y).unwrap().signal;
    let mut cfg = ProcBuilder::new();
    let procedure = cfg
        .add_combinational_procedure(ProcedureKind::Combinational, test_span())
        .unwrap();
    let entry = cfg.add_block(procedure, test_span()).unwrap();
    let then_block = cfg.add_block(procedure, test_span()).unwrap();
    let else_block = cfg.add_block(procedure, test_span()).unwrap();
    let exit = cfg.add_block(procedure, test_span()).unwrap();
    cfg.terminate_branch(entry, sel_value, then_block, else_block, test_span())
        .unwrap();
    for (block, value) in [(then_block, a_value), (else_block, b_value)] {
        cfg.assign(
            block,
            AssignmentMode::Blocking,
            ProcTarget::signal(target),
            value,
            test_span(),
        )
        .unwrap();
        cfg.terminate_jump(block, exit, test_span()).unwrap();
    }
    cfg.terminate_return(exit, test_span()).unwrap();
    TestModule::new(module, cfg)
}
