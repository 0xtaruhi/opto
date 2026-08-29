// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Structural checks performed before synthesis transforms begin.
//!
//! These checks reject malformed cross-references and ambiguous drivers at the
//! source RTL and Word IR boundaries. Later passes may therefore use typed IDs
//! directly without repeatedly defending against invalid source graphs.

use opto_ir::{proc, rtl::RtlModule, word};
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Port contract used to validate an instantiated design or target cell.
pub struct ReferencePort {
    /// Direction as observed from the referenced design.
    pub direction: word::PortDirection,
    /// Expected connection width.
    pub width: u32,
    /// Whether width must match exactly rather than being adapted by lowering.
    pub exact_width: bool,
}

/// Reference design name to port name and port contract.
pub type ReferencePortMap = BTreeMap<String, BTreeMap<String, ReferencePort>>;

#[derive(Debug, Error, PartialEq, Eq)]
/// Structural error found before synthesis.
pub enum CheckDesignError {
    /// The top-level design exposes no ports.
    #[error("design '{design}' has no ports")]
    NoPorts {
        /// Name of the portless design.
        design: String,
    },
    /// A port name is empty after trimming.
    #[error("design '{design}' has a port with empty name")]
    EmptyPortName {
        /// Name of the design containing the invalid port.
        design: String,
    },
    /// Two top-level ports have the same interned name.
    #[error("duplicate port '{port}'")]
    DuplicatePort {
        /// Duplicated port name.
        port: String,
    },
    /// A named signal has an empty name.
    #[error("design '{design}' has a signal with empty name")]
    EmptySignalName {
        /// Name of the design containing the invalid signal.
        design: String,
    },
    /// Two signals have the same interned name.
    #[error("duplicate signal '{signal}'")]
    DuplicateSignal {
        /// Duplicated signal name.
        signal: String,
    },
    /// An instance name is empty.
    #[error("design '{design}' has an instance with empty name")]
    EmptyInstanceName {
        /// Name of the design containing the invalid instance.
        design: String,
    },
    /// An instance has no referenced design name.
    #[error("instance '{instance}' has empty reference")]
    EmptyReference {
        /// Name of the instance lacking a referenced design.
        instance: String,
    },
    /// Two instances have the same name.
    #[error("duplicate instance '{instance}'")]
    DuplicateInstance {
        /// Duplicated instance name.
        instance: String,
    },
    /// An instance connection has no referenced port name.
    #[error("instance '{instance}' has connection with empty port")]
    EmptyConnectionPort {
        /// Name of the instance containing the invalid connection.
        instance: String,
    },
    /// One instance connects the same referenced port more than once.
    #[error("instance '{instance}' has duplicate connection port '{port}'")]
    DuplicateConnectionPort {
        /// Name of the instance containing both connections.
        instance: String,
        /// Duplicated referenced port name.
        port: String,
    },
    /// An instance connection contains an invalid Word IR value ID.
    #[error("instance '{instance}' port '{port}' references missing RTL value {value:?}")]
    MissingInstanceValue {
        /// Name of the instance containing the dangling value ID.
        instance: String,
        /// Referenced port receiving the invalid value.
        port: String,
        /// Value ID absent from the owning Word module.
        value: word::ValueId,
    },
    /// A connection names a port absent from the known reference contract.
    #[error("instance '{instance}' references unknown port '{reference}.{port}'")]
    UnknownInstancePort {
        /// Name of the instance containing the unknown port.
        instance: String,
        /// Referenced design or target-cell name.
        reference: String,
        /// Port name absent from the reference contract.
        port: String,
    },
    /// A connection width differs from an exact-width reference contract.
    #[error(
        "instance '{instance}' port '{reference}.{port}' expects width {expected}, got {actual}"
    )]
    InstancePortWidthMismatch {
        /// Name of the instance containing the mismatched connection.
        instance: String,
        /// Referenced design or target-cell name.
        reference: String,
        /// Referenced port whose width contract was violated.
        port: String,
        /// Width required by the reference contract.
        expected: u32,
        /// Width of the connected value.
        actual: u32,
    },
    /// An output connection cannot be resolved to writable signal bits.
    #[error("output connection '{instance}.{port}' is not a signal selection or concatenation")]
    InvalidOutputConnection {
        /// Name of the instance containing the output connection.
        instance: String,
        /// Referenced output port name.
        port: String,
    },
    /// A continuous assignment contains an invalid signal ID.
    #[error("continuous assign references missing RTL signal {signal:?}")]
    MissingConnectSignal {
        /// Signal ID absent from the owning Word module.
        signal: word::SignalId,
    },
    /// A continuous assignment contains an invalid value ID.
    #[error("continuous assign references missing RTL value {value:?}")]
    MissingConnectValue {
        /// Value ID absent from the owning Word module.
        value: word::ValueId,
    },
    /// A dynamic assignment contains an invalid offset value ID.
    #[error("dynamic continuous assign references missing offset value {value:?}")]
    MissingDynamicOffset {
        /// Offset value ID absent from the owning Word module.
        value: word::ValueId,
    },
    /// A dynamically indexed assignment targets an unsupported signal shape.
    #[error("dynamic continuous assignment target '{signal}' is not supported")]
    DynamicConnectTarget {
        /// User-visible name of the unsupported target signal.
        signal: String,
    },
    /// A driver resolves beyond the target signal width.
    #[error("signal '{signal}' bit {bit} is outside its width {width}")]
    DriverBitOutOfRange {
        /// User-visible name of the target signal.
        signal: String,
        /// Zero-based bit selected by the driver.
        bit: u32,
        /// Declared signal width.
        width: u32,
    },
    /// More than one source drives the same resolved signal bit.
    #[error("signal '{signal}' bit {bit} has multiple drivers")]
    MultipleDrivers {
        /// User-visible name of the multiply driven signal.
        signal: String,
        /// Multiply driven bit index.
        bit: u32,
    },
}

#[cfg(test)]
pub(crate) fn check_design(module: &word::WordModule) -> Result<(), CheckDesignError> {
    check_word_design_with_references(module, &BTreeMap::new())
}

#[must_use]
#[cfg(test)]
pub(crate) fn target_cell_reference_ports(cells: &opto_library::TargetCellSet) -> ReferencePortMap {
    let mut references = ReferencePortMap::new();
    for cell in cells.iter() {
        references
            .entry(cell.name().to_string())
            .or_insert_with(|| {
                cell.pins()
                    .filter_map(|pin| {
                        let direction = match pin.direction() {
                            opto_library::TargetPinDirection::Input => word::PortDirection::Input,
                            opto_library::TargetPinDirection::Output => word::PortDirection::Output,
                            opto_library::TargetPinDirection::Inout => word::PortDirection::Inout,
                            opto_library::TargetPinDirection::Internal => return None,
                        };
                        Some((
                            pin.name().to_string(),
                            ReferencePort {
                                direction,
                                width: 1,
                                exact_width: true,
                            },
                        ))
                    })
                    .collect()
            });
    }
    references
}

/// Validate a source RTL module against known design-unit and target-cell ports.
///
/// The check covers names, typed-ID references, exact-width instance ports,
/// output l-values, and structural/procedural single-driver ownership for
/// resolved signal bits.
///
/// # Errors
///
/// Returns the first structural inconsistency in deterministic arena order.
pub fn check_design_with_references(
    module: &RtlModule,
    reference_ports: &ReferencePortMap,
) -> Result<(), CheckDesignError> {
    check_module_with_references(
        module.word(),
        module.procedures().effects(),
        reference_ports,
        true,
    )
}

/// Validate a reachable source RTL definition against known design-unit and
/// target-cell ports.
///
/// Unlike [`check_design_with_references`], this permits an empty external
/// interface because a reachable child may exist only for hierarchy,
/// assertions removed by preprocessing, or other internal structure. All
/// remaining structural checks are identical.
///
/// # Errors
///
/// Returns the first structural inconsistency in deterministic arena order.
pub fn check_definition_with_references(
    module: &RtlModule,
    reference_ports: &ReferencePortMap,
) -> Result<(), CheckDesignError> {
    check_module_with_references(
        module.word(),
        module.procedures().effects(),
        reference_ports,
        false,
    )
}

/// Revalidate the normalized Word IR before it leaves the synthesis frontend.
pub(crate) fn check_word_design_with_references(
    module: &word::WordModule,
    reference_ports: &ReferencePortMap,
) -> Result<(), CheckDesignError> {
    check_module_with_references(module, &[], reference_ports, true)
}

fn check_module_with_references(
    module: &word::WordModule,
    procedural_effects: &[proc::Effect],
    reference_ports: &ReferencePortMap,
    require_ports: bool,
) -> Result<(), CheckDesignError> {
    if require_ports && module.ports().is_empty() {
        return Err(CheckDesignError::NoPorts {
            design: module.name().to_string(),
        });
    }
    let mut port_names = BTreeSet::new();
    for port in module.ports() {
        if module.name_str(port.name).trim().is_empty() {
            return Err(CheckDesignError::EmptyPortName {
                design: module.name().to_string(),
            });
        }
        if !port_names.insert(port.name) {
            return Err(CheckDesignError::DuplicatePort {
                port: module.name_str(port.name).to_string(),
            });
        }
    }

    let mut signal_names = BTreeSet::new();
    for signal in module.signals() {
        if let Some(name) = signal.name {
            if module.name_str(name).trim().is_empty() {
                return Err(CheckDesignError::EmptySignalName {
                    design: module.name().to_string(),
                });
            }
            if !signal_names.insert(name) {
                return Err(CheckDesignError::DuplicateSignal {
                    signal: module.name_str(name).to_string(),
                });
            }
        }
    }

    let mut driven_bits = module
        .signals()
        .iter()
        .map(|signal| vec![false; signal.ty.width() as usize])
        .collect::<Vec<_>>();
    // RtlModule validation has already proved that overlapping effects share
    // one procedure owner. Seed that owner before testing structural claims.
    for effect in procedural_effects {
        let proc::ProcTarget::Signal { signal, select } = effect.target else {
            continue;
        };
        let stored = module
            .signal(signal)
            .ok_or(CheckDesignError::MissingConnectSignal { signal })?;
        if stored.kind == word::SignalKind::ProcessLocal {
            continue;
        }
        let (lsb, width) = match select {
            proc::TargetSelect::Whole | proc::TargetSelect::Dynamic { .. } => {
                (0, stored.ty.width())
            }
            proc::TargetSelect::Static(range) => (range.lsb.min(range.msb), range.width()),
        };
        for offset in 0..width {
            mark_driven_bit(
                module,
                &mut driven_bits,
                signal,
                lsb.saturating_add(offset),
                false,
            )?;
        }
    }
    for port in module.ports().iter().filter(|port| {
        matches!(
            port.direction,
            word::PortDirection::Input | word::PortDirection::Inout
        )
    }) {
        let signal = module
            .signal(port.signal)
            .ok_or(CheckDesignError::MissingConnectSignal {
                signal: port.signal,
            })?;
        for bit in 0..signal.ty.width() {
            mark_driven_bit(module, &mut driven_bits, port.signal, bit, true)?;
        }
    }
    let mut instance_names = BTreeSet::new();
    for instance in module.instances() {
        if module.name_str(instance.name).trim().is_empty() {
            return Err(CheckDesignError::EmptyInstanceName {
                design: module.name().to_string(),
            });
        }
        if module.name_str(instance.module).trim().is_empty() {
            return Err(CheckDesignError::EmptyReference {
                instance: module.name_str(instance.name).to_string(),
            });
        }
        if !instance_names.insert(instance.name) {
            return Err(CheckDesignError::DuplicateInstance {
                instance: module.name_str(instance.name).to_string(),
            });
        }
        let mut connection_ports = BTreeSet::new();
        for connection in &instance.connections {
            if module.name_str(connection.port).trim().is_empty() {
                return Err(CheckDesignError::EmptyConnectionPort {
                    instance: module.name_str(instance.name).to_string(),
                });
            }
            if !connection_ports.insert(connection.port) {
                return Err(CheckDesignError::DuplicateConnectionPort {
                    instance: module.name_str(instance.name).to_string(),
                    port: module.name_str(connection.port).to_string(),
                });
            }
            let value = module.value(connection.value).ok_or_else(|| {
                CheckDesignError::MissingInstanceValue {
                    instance: module.name_str(instance.name).to_string(),
                    port: module.name_str(connection.port).to_string(),
                    value: connection.value,
                }
            })?;
            let reference = module.name_str(instance.module);
            let port = module.name_str(connection.port);
            let port_info = reference_ports
                .get(reference)
                .map(|ports| {
                    ports
                        .get(port)
                        .ok_or_else(|| CheckDesignError::UnknownInstancePort {
                            instance: module.name_str(instance.name).to_string(),
                            reference: reference.to_string(),
                            port: port.to_string(),
                        })
                })
                .transpose()?;
            if let Some(port_info) = port_info
                && port_info.exact_width
                && value.ty.width() != port_info.width
            {
                return Err(CheckDesignError::InstancePortWidthMismatch {
                    instance: module.name_str(instance.name).to_string(),
                    reference: reference.to_string(),
                    port: port.to_string(),
                    expected: port_info.width,
                    actual: value.ty.width(),
                });
            }
            let output = port_info.is_some_and(|port| {
                matches!(
                    port.direction,
                    word::PortDirection::Output | word::PortDirection::Inout
                )
            });
            if output {
                let fragments = module.signal_fragments(connection.value).map_err(|_| {
                    CheckDesignError::InvalidOutputConnection {
                        instance: module.name_str(instance.name).to_string(),
                        port: port.to_string(),
                    }
                })?;
                for fragment in fragments {
                    for offset in 0..fragment.reference.width() {
                        let bit = fragment.reference.lsb.checked_add(offset).ok_or_else(|| {
                            CheckDesignError::InvalidOutputConnection {
                                instance: module.name_str(instance.name).to_string(),
                                port: port.to_string(),
                            }
                        })?;
                        mark_driven_bit(
                            module,
                            &mut driven_bits,
                            fragment.reference.signal,
                            bit,
                            true,
                        )?;
                    }
                }
            }
        }
    }

    for connect in module.connects() {
        let signal =
            module
                .signal(connect.target.signal)
                .ok_or(CheckDesignError::MissingConnectSignal {
                    signal: connect.target.signal,
                })?;
        module
            .value(connect.value)
            .ok_or(CheckDesignError::MissingConnectValue {
                value: connect.value,
            })?;
        if let Some(dynamic) = connect.target.dynamic {
            module
                .value(dynamic.offset)
                .ok_or(CheckDesignError::MissingDynamicOffset {
                    value: dynamic.offset,
                })?;
            return Err(CheckDesignError::DynamicConnectTarget {
                signal: signal_name(module, signal),
            });
        }
        let (lsb, width) = connect
            .target
            .range
            .map_or((0, signal.ty.width()), |range| {
                (range.lsb.min(range.msb), range.width())
            });
        for offset in 0..width {
            let bit = lsb.saturating_add(offset);
            mark_driven_bit(module, &mut driven_bits, connect.target.signal, bit, true)?;
        }
    }
    Ok(())
}

fn mark_driven_bit(
    module: &word::WordModule,
    driven_bits: &mut [Vec<bool>],
    signal_id: word::SignalId,
    bit: u32,
    duplicate_is_error: bool,
) -> Result<(), CheckDesignError> {
    let signal = module
        .signal(signal_id)
        .ok_or(CheckDesignError::MissingConnectSignal { signal: signal_id })?;
    let driven = driven_bits
        .get_mut(signal_id.index())
        .ok_or(CheckDesignError::MissingConnectSignal { signal: signal_id })?;
    let width = signal.ty.width();
    let Some(driven) = usize::try_from(bit)
        .ok()
        .and_then(|bit| driven.get_mut(bit))
    else {
        return Err(CheckDesignError::DriverBitOutOfRange {
            signal: signal_name(module, signal),
            bit,
            width,
        });
    };
    if std::mem::replace(driven, true)
        && duplicate_is_error
        && signal.resolution == word::SignalResolution::SingleDriver
    {
        return Err(CheckDesignError::MultipleDrivers {
            signal: signal_name(module, signal),
            bit,
        });
    }
    Ok(())
}

fn signal_name(module: &word::WordModule, signal: &word::Signal) -> String {
    signal.name.map_or_else(
        || "<unnamed>".to_string(),
        |name| module.name_str(name).to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use opto_ir::proc::{AssignmentMode, ProcBuilder, ProcTarget, ProcedureKind, TargetSelect};

    fn bit() -> word::WordType {
        word::WordType::new(1, false, word::LogicStateKind::FourState).unwrap()
    }

    fn bits(width: u32) -> word::WordType {
        word::WordType::new(width, false, word::LogicStateKind::FourState).unwrap()
    }

    #[test]
    fn reports_semantic_error_without_tcl_command_prefix() {
        let module = word::WordModule::new("empty");

        let error = check_design(&module).unwrap_err();

        assert_eq!(
            error,
            CheckDesignError::NoPorts {
                design: "empty".to_string()
            }
        );
        assert_eq!(error.to_string(), "design 'empty' has no ports");
    }

    #[test]
    fn accepts_a_portless_reachable_definition() {
        let module = RtlModule::structural(word::WordModule::new("leaf")).unwrap();

        check_definition_with_references(&module, &ReferencePortMap::new()).unwrap();
    }

    #[test]
    fn rejects_overlapping_connect_drivers() {
        let mut module = word::WordModule::new("top");
        let a = module
            .add_port(
                "a",
                word::PortDirection::Input,
                bit(),
                word::SourceSpan::default(),
            )
            .unwrap();
        let b = module
            .add_port(
                "b",
                word::PortDirection::Input,
                bit(),
                word::SourceSpan::default(),
            )
            .unwrap();
        let y = module
            .add_port(
                "y",
                word::PortDirection::Output,
                bit(),
                word::SourceSpan::default(),
            )
            .unwrap();
        let a = module.port(a).unwrap().signal;
        let b = module.port(b).unwrap().signal;
        let a = module.read_signal(a, word::SourceSpan::default()).unwrap();
        let b = module.read_signal(b, word::SourceSpan::default()).unwrap();
        let y = module.port(y).unwrap().signal;
        module
            .connect(word::LValue::signal(y), a, word::SourceSpan::default())
            .unwrap();
        module
            .connect(word::LValue::signal(y), b, word::SourceSpan::default())
            .unwrap();

        let error = check_design(&module).unwrap_err();
        assert_eq!(
            error,
            CheckDesignError::MultipleDrivers {
                signal: "y".to_string(),
                bit: 0,
            }
        );
    }

    #[test]
    fn accepts_disjoint_connect_drivers() {
        let mut module = word::WordModule::new("top");
        let a = module
            .add_port(
                "a",
                word::PortDirection::Input,
                bit(),
                word::SourceSpan::default(),
            )
            .unwrap();
        let b = module
            .add_port(
                "b",
                word::PortDirection::Input,
                bit(),
                word::SourceSpan::default(),
            )
            .unwrap();
        let y = module
            .add_port(
                "y",
                word::PortDirection::Output,
                bits(2),
                word::SourceSpan::default(),
            )
            .unwrap();
        let a = module.port(a).unwrap().signal;
        let b = module.port(b).unwrap().signal;
        let a = module.read_signal(a, word::SourceSpan::default()).unwrap();
        let b = module.read_signal(b, word::SourceSpan::default()).unwrap();
        let y = module.port(y).unwrap().signal;
        module
            .connect(
                word::LValue::signal(y).with_range(word::BitRange { msb: 0, lsb: 0 }),
                a,
                word::SourceSpan::default(),
            )
            .unwrap();
        module
            .connect(
                word::LValue::signal(y).with_range(word::BitRange { msb: 1, lsb: 1 }),
                b,
                word::SourceSpan::default(),
            )
            .unwrap();

        check_design(&module).unwrap();
    }

    fn mixed_driver_module(procedural_bit: u32) -> RtlModule {
        let mut module = word::WordModule::new("top");
        let a = module
            .add_port(
                "a",
                word::PortDirection::Input,
                bit(),
                word::SourceSpan::default(),
            )
            .unwrap();
        let y = module
            .add_port(
                "y",
                word::PortDirection::Output,
                bits(2),
                word::SourceSpan::default(),
            )
            .unwrap();
        let a = module.port(a).unwrap().signal;
        let value = module.read_signal(a, word::SourceSpan::default()).unwrap();
        let y = module.port(y).unwrap().signal;
        module
            .connect(
                word::LValue::signal(y).with_range(word::BitRange { msb: 0, lsb: 0 }),
                value,
                word::SourceSpan::default(),
            )
            .unwrap();

        let mut procedures = ProcBuilder::new();
        let procedure = procedures
            .add_combinational_procedure(ProcedureKind::Combinational, word::SourceSpan::default())
            .unwrap();
        let block = procedures
            .add_block(procedure, word::SourceSpan::default())
            .unwrap();
        procedures
            .assign(
                block,
                AssignmentMode::Blocking,
                ProcTarget::signal(y).with_select(TargetSelect::Static(word::BitRange {
                    msb: procedural_bit,
                    lsb: procedural_bit,
                })),
                value,
                word::SourceSpan::default(),
            )
            .unwrap();
        procedures
            .terminate_return(block, word::SourceSpan::default())
            .unwrap();
        RtlModule::new(module, procedures.seal().unwrap()).unwrap()
    }

    #[test]
    fn rejects_overlapping_continuous_and_procedural_drivers() {
        let module = mixed_driver_module(0);

        let error = check_design_with_references(&module, &ReferencePortMap::new()).unwrap_err();

        assert_eq!(
            error,
            CheckDesignError::MultipleDrivers {
                signal: "y".to_string(),
                bit: 0,
            }
        );
    }

    #[test]
    fn accepts_disjoint_continuous_and_procedural_drivers() {
        let module = mixed_driver_module(1);

        check_design_with_references(&module, &ReferencePortMap::new()).unwrap();
    }
}
