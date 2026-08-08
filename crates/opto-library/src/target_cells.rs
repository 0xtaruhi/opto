// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Canonical synthesis-facing cell, pin, and timing-arc records.
//!
//! These owned records are produced by Liberty parsing and packed into
//! [`TargetCellSet`] for read-heavy synthesis queries.

use crate::{
    ArcDelayModel, BooleanFunction, LookupTable, PinReceiverCapacitanceModel, TimingCheckKind,
    TimingEdge, TimingSense,
};
use serde::{Deserialize, Serialize};

mod arena;
pub use arena::{
    BooleanFunctionKind, BooleanFunctionRef, TargetCellRef, TargetCellSet, TargetPinRef,
    TargetSequentialRef, TargetTimingArcRef,
};

/// Returns a total-orderable area cost for library selection algorithms.
/// Missing, negative, or non-finite areas are not valid optimization choices.
#[must_use]
pub fn normalized_cell_area(area: Option<f64>) -> f64 {
    area.filter(|area| area.is_finite() && *area >= 0.0)
        .unwrap_or(f64::INFINITY)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
/// Owned synthesis view of one Liberty cell.
pub struct TargetCell {
    /// Library-unique cell name.
    pub name: String,
    /// Cell area in the library's area unit, when specified.
    pub area: Option<f64>,
    /// Whether synthesis must exclude the cell from mapping choices.
    pub dont_use: bool,
    /// Specialized usage restrictions parsed from Liberty attributes.
    pub usage: TargetCellUsage,
    /// Pins in source order.
    pub pins: Vec<TargetPin>,
    /// Sequential state descriptions in source order.
    pub sequential: Vec<TargetSequential>,
    /// Integrated clock-gating behavior, when the cell declares one.
    pub clock_gate: Option<TargetClockGateKind>,
    /// First-class characterized memory contract, when this cell is a memory macro.
    #[serde(default)]
    pub memory: Option<TargetMemory>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
/// Storage kind implemented by a characterized memory macro.
pub enum TargetMemoryKind {
    /// Read/write random-access memory.
    Ram,
    /// Read-only memory.
    Rom,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// Complete synthesis-facing behavior and pin binding of one memory macro.
///
/// Address, data, and mask pin vectors are least-significant-bit first. The
/// contract is exact: synthesis may bind a first-class memory only when shape,
/// port order, clocks, enables, masks, and collision behavior all match.
pub struct TargetMemory {
    /// Storage behavior.
    pub kind: TargetMemoryKind,
    /// Number of addressable words.
    pub depth: u32,
    /// Number of bits in each word.
    pub word_width: u32,
    /// Read-port contracts in declaration order.
    pub read_ports: Vec<TargetMemoryReadPort>,
    /// Write-port contracts in declaration order.
    pub write_ports: Vec<TargetMemoryWritePort>,
}

/// Active clock edge of a synchronous memory port.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TargetMemoryEdge {
    /// Rising clock edge.
    Rising,
    /// Falling clock edge.
    Falling,
}

/// Clock pin and active edge of a synchronous memory port.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TargetMemoryClock {
    /// Clock pin name.
    pub pin: String,
    /// Active clock edge.
    pub edge: TargetMemoryEdge,
}

/// Enable pin and polarity of a memory port.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TargetMemoryEnable {
    /// Enable pin name.
    pub pin: String,
    /// Whether logic high enables the port.
    pub active_high: bool,
}

/// Output behavior while a read port is disabled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TargetMemoryDisabledRead {
    /// Preserve the last observed output value.
    Hold,
    /// Produce an unspecified value.
    Undefined,
}

/// Value observed when a location is read while it is being written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TargetMemoryReadDuringWrite {
    /// Observe the value stored before the write.
    OldData,
    /// Observe the newly written value.
    NewData,
    /// Preserve the previous read output.
    NoChange,
    /// Produce an unspecified value.
    Undefined,
}

/// Pin binding and behavior of one memory read port.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TargetMemoryReadPort {
    /// Address pins from least- to most-significant bit.
    pub address_pins: Vec<String>,
    /// Read-data pins from least- to most-significant bit.
    pub data_pins: Vec<String>,
    /// Clocking contract, or `None` for an asynchronous read.
    pub clock: Option<TargetMemoryClock>,
    /// Optional read-enable contract.
    pub enable: Option<TargetMemoryEnable>,
    /// Behavior while the port is disabled.
    pub disabled: TargetMemoryDisabledRead,
    /// Same-address read/write collision behavior.
    pub read_during_write: TargetMemoryReadDuringWrite,
}

/// Pin binding and behavior of one memory write port.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TargetMemoryWritePort {
    /// Address pins from least- to most-significant bit.
    pub address_pins: Vec<String>,
    /// Write-data pins from least- to most-significant bit.
    pub data_pins: Vec<String>,
    /// Required write clock contract.
    pub clock: TargetMemoryClock,
    /// Optional write-enable contract.
    pub enable: Option<TargetMemoryEnable>,
    /// Mask pins in increasing word-slice order.
    pub mask_pins: Vec<String>,
    /// Number of data bits controlled by each mask pin.
    pub mask_granularity: u32,
    /// Whether logic high enables a masked write slice.
    pub mask_active_high: bool,
}

/// Sequential storage and active-edge form of an integrated clock gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TargetClockGateKind {
    /// Latch-based gate for a rising-edge clock.
    LatchPosedge,
    /// Latch-based gate for a falling-edge clock.
    LatchNegedge,
    /// Combinational gate for a rising-edge clock.
    NonePosedge,
    /// Combinational gate for a falling-edge clock.
    NoneNegedge,
}

impl TargetClockGateKind {
    #[must_use]
    /// Returns whether the gate drives rising-edge sequential elements.
    pub const fn gates_rising_edge(self) -> bool {
        matches!(self, Self::LatchPosedge | Self::NonePosedge)
    }

    #[must_use]
    /// Returns whether the enable is captured by an internal latch.
    pub const fn is_latch_based(self) -> bool {
        matches!(self, Self::LatchPosedge | Self::LatchNegedge)
    }

    #[must_use]
    /// Parses a Liberty `clock_gating_integrated_cell` value.
    pub fn parse(value: &str) -> Option<Self> {
        let base = value
            .strip_suffix("_precontrol")
            .or_else(|| value.strip_suffix("_postcontrol"))
            .unwrap_or(value);
        match base {
            "latch_posedge" => Some(Self::LatchPosedge),
            "latch_negedge" => Some(Self::LatchNegedge),
            "none_posedge" => Some(Self::NonePosedge),
            "none_negedge" => Some(Self::NoneNegedge),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[repr(transparent)]
/// Bit set of specialized cell usages that exclude general mapping.
pub struct TargetCellUsage(u8);

impl TargetCellUsage {
    /// Marks an isolation cell.
    pub const ISOLATION: Self = Self(1 << 0);
    /// Marks a level-shifter cell.
    pub const LEVEL_SHIFTER: Self = Self(1 << 1);
    /// Marks an integrated clock-gating cell.
    pub const INTEGRATED_CLOCK_GATING: Self = Self(1 << 2);
    /// Marks a cell that remains powered across switchable domains.
    pub const ALWAYS_ON: Self = Self(1 << 3);

    #[must_use]
    /// Returns whether the cell has no specialized usage restriction.
    pub const fn is_general_purpose(self) -> bool {
        self.0 == 0
    }

    pub(crate) fn insert(&mut self, usage: Self) {
        self.0 |= usage.0;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
/// Kind of state element represented by a sequential group.
pub enum TargetSequentialKind {
    /// Edge-triggered flip-flop.
    FlipFlop,
    /// Level-sensitive latch.
    Latch,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// Canonical Boolean behavior of one Liberty sequential group.
pub struct TargetSequential {
    /// State-element kind.
    pub kind: TargetSequentialKind,
    /// Liberty state-variable names.
    pub state_variables: Vec<String>,
    /// Clock or gate-control expression.
    pub clocked_on: Option<BooleanFunction>,
    /// Next-state data expression.
    pub next_state: Option<BooleanFunction>,
    /// Optional latch-enable expression.
    pub enable: Option<BooleanFunction>,
    /// Asynchronous clear expression.
    pub clear: Option<BooleanFunction>,
    /// Asynchronous preset expression.
    pub preset: Option<BooleanFunction>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
/// Owned synthesis and timing view of one Liberty pin.
pub struct TargetPin {
    /// Pin name within its cell.
    pub name: String,
    /// Signal-flow direction.
    pub direction: TargetPinDirection,
    /// Output logic function, when defined.
    pub function: Option<BooleanFunction>,
    /// Output high-impedance control, when defined.
    pub three_state: Option<BooleanFunction>,
    /// Default input capacitance.
    pub capacitance: Option<f64>,
    /// Rising input capacitance override.
    pub rise_capacitance: Option<f64>,
    /// Falling input capacitance override.
    pub fall_capacitance: Option<f64>,
    /// Slew-dependent receiver-capacitance model.
    pub receiver_capacitance: Option<PinReceiverCapacitanceModel>,
    /// Abstract fanout load used by fanout constraints.
    pub fanout_load: Option<f64>,
    /// Sequential role of an input pin.
    pub next_state_type: Option<TargetNextStateType>,
    /// Role of the pin within an integrated clock-gating cell.
    pub clock_gate_role: Option<TargetClockGateRole>,
    /// Timing arcs terminating at this pin.
    pub timing_arcs: Vec<TargetTimingArc>,
}

/// Functional role of a pin in an integrated clock-gating cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TargetClockGateRole {
    /// Ungated clock input.
    Clock,
    /// Functional gate-enable input.
    Enable,
    /// Gated clock output.
    Output,
    /// Test-mode gate-enable input.
    TestEnable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
/// Sequential next-state role assigned to a pin.
pub enum TargetNextStateType {
    /// Functional data input.
    Data,
    /// Asynchronous preset input.
    Preset,
    /// Asynchronous clear input.
    Clear,
    /// Load-control input.
    Load,
    /// Scan-chain data input.
    ScanIn,
    /// Scan-enable input.
    ScanEnable,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
/// Owned timing relationship from a related pin to the containing pin.
pub struct TargetTimingArc {
    /// Source or control pin name.
    pub related_pin: String,
    /// Semantic class of the timing relationship.
    pub timing_type: TargetTimingType,
    /// Output polarity relationship.
    pub timing_sense: TimingSense,
    /// Propagation delay and output-slew tables.
    pub delay_model: Option<ArcDelayModel>,
    /// Rising timing-check constraint table.
    pub rise_constraint: Option<LookupTable>,
    /// Falling timing-check constraint table.
    pub fall_constraint: Option<LookupTable>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
/// Canonical timing type of a Liberty timing group.
pub enum TargetTimingType {
    /// Combinational propagation arc.
    Combinational,
    /// Sequential clock-to-output arc for the given active edge.
    ClockToQ(TimingEdge),
    /// Setup or hold timing check against a clock edge.
    Check {
        /// Timing-check class.
        kind: TimingCheckKind,
        /// Active edge of the related clock.
        clock_edge: TimingEdge,
    },
    /// Asynchronous clear propagation arc.
    Clear,
    /// Asynchronous preset propagation arc.
    Preset,
    /// Recovery check against the given clock edge.
    Recovery(TimingEdge),
    /// Removal check against the given clock edge.
    Removal(TimingEdge),
    /// Minimum pulse-width check.
    MinPulseWidth,
    /// Nonsequential setup check.
    NonSequentialSetup(TimingEdge),
    /// Nonsequential hold check.
    NonSequentialHold(TimingEdge),
    /// Three-state enable propagation arc.
    ThreeStateEnable,
    /// Three-state disable propagation arc.
    ThreeStateDisable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
/// Signal-flow direction of a target pin.
pub enum TargetPinDirection {
    /// Input pin.
    Input,
    /// Output pin.
    Output,
    /// Bidirectional pin.
    Inout,
    /// Internal library node.
    Internal,
}

/// Tests whether a replacement preserves functional and timing interfaces.
///
/// Numeric characterization is intentionally ignored; the replacement may
/// trade area, delay, and power while retaining compatible pin semantics.
#[must_use]
pub fn cells_are_replacement_compatible(
    current: TargetCellRef<'_>,
    replacement: TargetCellRef<'_>,
) -> bool {
    current.memory() == replacement.memory()
        && current.sequential().len() == replacement.sequential().len()
        && current
            .sequential()
            .zip(replacement.sequential())
            .all(|(left, right)| {
                left.kind() == right.kind()
                    && left.state_variables().eq(right.state_variables())
                    && optional_functions_equal(left.clocked_on(), right.clocked_on())
                    && optional_functions_equal(left.next_state(), right.next_state())
                    && optional_functions_equal(left.enable(), right.enable())
                    && optional_functions_equal(left.clear(), right.clear())
                    && optional_functions_equal(left.preset(), right.preset())
            })
        && current.pins().len() == replacement.pins().len()
        && current.pins().zip(replacement.pins()).all(|(left, right)| {
            left.name() == right.name()
                && left.direction() == right.direction()
                && optional_functions_equal(left.function(), right.function())
                && optional_functions_equal(left.three_state(), right.three_state())
                && left.next_state_type() == right.next_state_type()
                && left.timing_arcs().len() == right.timing_arcs().len()
                && left
                    .timing_arcs()
                    .zip(right.timing_arcs())
                    .all(|(left, right)| {
                        left.related_pin() == right.related_pin()
                            && left.timing_type() == right.timing_type()
                            && left.timing_sense() == right.timing_sense()
                    })
        })
}

fn optional_functions_equal(
    left: Option<BooleanFunctionRef<'_>>,
    right: Option<BooleanFunctionRef<'_>>,
) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => left.semantic_eq(right),
        (None, None) => true,
        _ => false,
    }
}

/// Tests whether swapping two input pins preserves every output function.
///
/// Sequential cells and cells with more than twelve inputs are conservatively
/// rejected to keep exhaustive truth-table evaluation bounded.
#[must_use]
pub fn cell_input_pins_are_symmetric(cell: TargetCellRef<'_>, first: &str, second: &str) -> bool {
    const MAX_SYMMETRY_INPUTS: usize = 12;
    if first == second || cell.sequential().next().is_some() {
        return false;
    }
    let inputs = cell
        .pins()
        .filter(|pin| {
            matches!(
                pin.direction(),
                TargetPinDirection::Input | TargetPinDirection::Inout
            )
        })
        .map(TargetPinRef::name)
        .collect::<Vec<_>>();
    let (Some(first_index), Some(second_index)) = (
        inputs.iter().position(|name| *name == first),
        inputs.iter().position(|name| *name == second),
    ) else {
        return false;
    };
    if inputs.len() > MAX_SYMMETRY_INPUTS {
        return false;
    }
    let outputs = cell
        .pins()
        .filter(|pin| pin.direction() == TargetPinDirection::Output)
        .collect::<Vec<_>>();
    !outputs.is_empty()
        && outputs.iter().all(|pin| pin.function().is_some())
        && outputs
            .iter()
            .flat_map(|pin| [pin.function(), pin.three_state()])
            .flatten()
            .all(|function| {
                (0usize..(1usize << inputs.len())).all(|assignment| {
                    let evaluate = |swapped: bool| {
                        function.eval(&mut |name| {
                            let index = inputs.iter().position(|input| *input == name)?;
                            let index = if swapped && index == first_index {
                                second_index
                            } else if swapped && index == second_index {
                                first_index
                            } else {
                                index
                            };
                            Some(assignment & (1usize << index) != 0)
                        })
                    };
                    evaluate(false) == evaluate(true)
                })
            })
}

pub(crate) fn target_timing_type(value: Option<&str>) -> Option<TargetTimingType> {
    match value {
        None | Some("combinational" | "combinational_rise" | "combinational_fall") => {
            Some(TargetTimingType::Combinational)
        }
        Some("rising_edge") => Some(TargetTimingType::ClockToQ(TimingEdge::Rise)),
        Some("falling_edge") => Some(TargetTimingType::ClockToQ(TimingEdge::Fall)),
        Some("setup_rising") => Some(TargetTimingType::Check {
            kind: TimingCheckKind::Setup,
            clock_edge: TimingEdge::Rise,
        }),
        Some("setup_falling") => Some(TargetTimingType::Check {
            kind: TimingCheckKind::Setup,
            clock_edge: TimingEdge::Fall,
        }),
        Some("hold_rising") => Some(TargetTimingType::Check {
            kind: TimingCheckKind::Hold,
            clock_edge: TimingEdge::Rise,
        }),
        Some("hold_falling") => Some(TargetTimingType::Check {
            kind: TimingCheckKind::Hold,
            clock_edge: TimingEdge::Fall,
        }),
        Some("clear") => Some(TargetTimingType::Clear),
        Some("preset") => Some(TargetTimingType::Preset),
        Some("recovery_rising") => Some(TargetTimingType::Recovery(TimingEdge::Rise)),
        Some("recovery_falling") => Some(TargetTimingType::Recovery(TimingEdge::Fall)),
        Some("removal_rising") => Some(TargetTimingType::Removal(TimingEdge::Rise)),
        Some("removal_falling") => Some(TargetTimingType::Removal(TimingEdge::Fall)),
        Some("min_pulse_width") => Some(TargetTimingType::MinPulseWidth),
        Some("non_seq_setup_rising") => {
            Some(TargetTimingType::NonSequentialSetup(TimingEdge::Rise))
        }
        Some("non_seq_setup_falling") => {
            Some(TargetTimingType::NonSequentialSetup(TimingEdge::Fall))
        }
        Some("non_seq_hold_rising") => Some(TargetTimingType::NonSequentialHold(TimingEdge::Rise)),
        Some("non_seq_hold_falling") => Some(TargetTimingType::NonSequentialHold(TimingEdge::Fall)),
        Some("three_state_enable") => Some(TargetTimingType::ThreeStateEnable),
        Some("three_state_disable") => Some(TargetTimingType::ThreeStateDisable),
        Some(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proves_pin_symmetry_from_complete_output_functions() {
        let symmetric = TargetCellSet::from(vec![combinational_cell("A&B")]);
        let asymmetric = TargetCellSet::from(vec![combinational_cell("A&!B")]);

        assert!(cell_input_pins_are_symmetric(
            symmetric.get(0).unwrap(),
            "A",
            "B"
        ));
        assert!(!cell_input_pins_are_symmetric(
            asymmetric.get(0).unwrap(),
            "A",
            "B"
        ));
    }

    #[test]
    fn synthesis_view_excludes_policy_and_special_purpose_cells() {
        let general = combinational_cell("A&B");
        let mut dont_use = combinational_cell("A&B");
        dont_use.name = "DONT_USE".to_string();
        dont_use.dont_use = true;
        let mut isolation = combinational_cell("A&B");
        isolation.name = "ISO".to_string();
        isolation.usage = TargetCellUsage::ISOLATION;
        let cells: TargetCellSet = vec![general, dont_use, isolation].into();

        assert_eq!(
            cells
                .synthesis_cells()
                .map(|(index, cell)| (index, cell.name()))
                .collect::<Vec<_>>(),
            [(0, "CELL")]
        );
    }

    #[test]
    fn synthesis_validation_rejects_ambiguous_names() {
        let cells: TargetCellSet =
            vec![combinational_cell("A&B"), combinational_cell("A|B")].into();
        let error = cells.validate_for_synthesis().unwrap_err();
        assert!(error.to_string().contains("duplicate cell name 'CELL'"));

        let mut duplicate_pin = combinational_cell("A&B");
        duplicate_pin.pins[1].name = "A".to_string();
        let error = TargetCellSet::from(vec![duplicate_pin])
            .validate_for_synthesis()
            .unwrap_err();
        assert!(error.to_string().contains("duplicate pin name 'A'"));

        let mut unknown_function_name = combinational_cell("A&MISSING");
        unknown_function_name.name = "UNKNOWN_FUNCTION".to_string();
        let error = TargetCellSet::from(vec![unknown_function_name])
            .validate_for_synthesis()
            .unwrap_err();
        assert!(error.to_string().contains("unknown name 'MISSING'"));
    }

    fn combinational_cell(function: &str) -> TargetCell {
        let input = |name: &str| TargetPin {
            name: name.to_string(),
            direction: TargetPinDirection::Input,
            function: None,
            three_state: None,
            capacitance: None,
            rise_capacitance: None,
            fall_capacitance: None,
            receiver_capacitance: None,
            fanout_load: None,
            next_state_type: None,
            timing_arcs: Vec::new(),
            clock_gate_role: None,
        };
        TargetCell {
            dont_use: false,
            usage: TargetCellUsage::default(),
            name: "CELL".to_string(),
            area: None,
            pins: vec![
                input("A"),
                input("B"),
                TargetPin {
                    name: "Y".to_string(),
                    direction: TargetPinDirection::Output,
                    function: Some(BooleanFunction::parse(function).unwrap()),
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
            sequential: Vec::new(),
            clock_gate: None,
            memory: None,
        }
    }
}
