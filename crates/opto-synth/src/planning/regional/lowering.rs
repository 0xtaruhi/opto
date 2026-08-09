// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Region-local Word dependency cones lowered before Boolean mapping.

use opto_ir::word;
use std::collections::{BTreeMap, BTreeSet};

mod importer;

pub(crate) struct RegionalWordCone {
    module: word::WordModule,
    source_to_local: BTreeMap<word::ValueId, word::ValueId>,
    boundary_bindings: Box<[(word::ValueId, word::ValueId)]>,
    operation_sources: Box<[Option<word::OpId>]>,
    memory_values: Box<[RegionalMemoryValueBinding]>,
    root_bindings: Box<[(word::ValueId, word::SignalId)]>,
}

pub(crate) struct RegionalWordConeRequest<'a> {
    pub(crate) source: &'a word::WordModule,
    pub(crate) operation_regions: &'a [Option<crate::RegionRowId>],
    pub(crate) region: crate::RegionRowId,
    pub(crate) memories: &'a [word::MemoryId],
    pub(crate) memory_implementations:
        &'a [crate::planning::regional::MemoryImplementationCandidate],
    pub(crate) target_cells: &'a opto_library::TargetCellSet,
    pub(crate) boundary_inputs: &'a [word::ValueId],
    pub(crate) roots: Vec<word::ValueId>,
}

pub(crate) struct RegionalWordConeParts {
    pub(crate) module: word::WordModule,
    pub(crate) source_to_local: BTreeMap<word::ValueId, word::ValueId>,
    pub(crate) boundary_bindings: Box<[(word::ValueId, word::ValueId)]>,
    pub(crate) operation_sources: Box<[Option<word::OpId>]>,
    pub(crate) memory_values: Box<[RegionalMemoryValueBinding]>,
    pub(crate) root_bindings: Box<[(word::ValueId, word::SignalId)]>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct RegionalMemoryValueBinding {
    pub(crate) local: word::ValueId,
    pub(crate) source_memory: word::MemoryId,
    pub(crate) kind: RegionalMemoryValueKind,
    pub(crate) ordinal: u32,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum RegionalMemoryValueKind {
    Operation,
    State,
}

impl RegionalWordCone {
    pub(crate) fn build(request: RegionalWordConeRequest<'_>) -> Result<Self, crate::SynthError> {
        let RegionalWordConeRequest {
            source,
            operation_regions,
            region,
            memories,
            memory_implementations,
            target_cells,
            boundary_inputs,
            roots,
        } = request;
        if operation_regions.len() != source.operations().len() {
            return Err(crate::SynthError::invariant(
                "region-local Word import has incomplete operation ownership",
            ));
        }
        if memories.len() != memory_implementations.len() {
            return Err(crate::SynthError::invariant(
                "region-local memory decisions do not align with region ownership",
            ));
        }
        let mut importer = RegionalWordImporter {
            source,
            signal_drivers: crate::word::signal_driver::SignalDriverIndex::new(source)?,
            operation_regions,
            region,
            module: word::WordModule::new(format!("{}$region{}", source.name(), region.raw())),
            source_to_local: BTreeMap::new(),
            boundary_bindings: Vec::new(),
            operation_sources: Vec::new(),
            visiting: BTreeSet::new(),
            import_path: Vec::new(),
            source_acyclic: false,
            recursive_boundaries: BTreeMap::new(),
            memory_signals: BTreeMap::new(),
            imported_bits: BTreeMap::new(),
            boundary_signals: BTreeMap::new(),
            boundary_port_signals: BTreeMap::new(),
            boundary_inputs: boundary_inputs.iter().copied().collect(),
        };
        importer.import_memories(memories)?;
        for &input in boundary_inputs {
            importer.import(input)?;
        }
        let mut observable_roots = BTreeMap::new();
        for root in roots.into_iter().collect::<std::collections::BTreeSet<_>>() {
            let local = importer.import(root)?;
            let source = importer.source.value(root).ok_or_else(|| {
                crate::SynthError::invariant(
                    "region-local observable root references an unknown source value",
                )
            })?;
            observable_roots
                .entry(local)
                .or_insert_with(|| (Vec::new(), source.ty, source.source.clone()))
                .0
                .push(root);
        }
        let mut root_bindings = Vec::new();
        for (index, (local, (roots, ty, source))) in observable_roots.into_iter().enumerate() {
            let port = importer
                .module
                .add_port(
                    format!("root${index}"),
                    word::PortDirection::Output,
                    ty,
                    source.clone(),
                )
                .map_err(crate::SynthError::from)?;
            let sink = importer
                .module
                .port(port)
                .ok_or_else(|| {
                    crate::SynthError::invariant("region-local root port is absent after creation")
                })?
                .signal;
            importer
                .module
                .connect(word::LValue::signal(sink), local, source)
                .map_err(crate::SynthError::from)?;
            root_bindings.extend(roots.into_iter().map(|root| (root, sink)));
        }
        let imported_operation_count = importer.module.operations().len();
        let memory_ownership = crate::planning::memory::lower_selected_memories(
            &mut importer.module,
            memory_implementations,
            target_cells,
        )?;
        importer
            .operation_sources
            .extend((imported_operation_count..importer.module.operations().len()).map(|_| None));
        let mut memory_values = Vec::new();
        for (local_memory_index, &source_memory) in memories.iter().enumerate() {
            let local_memory =
                word::MemoryId::from_index(local_memory_index).map_err(crate::SynthError::from)?;
            memory_values.extend(
                memory_ownership
                    .operations()
                    .filter_map(|(operation, owner)| {
                        (owner == local_memory)
                            .then_some(
                                importer
                                    .module
                                    .operation(operation)
                                    .map(|operation| operation.result),
                            )
                            .flatten()
                    })
                    .enumerate()
                    .map(|(ordinal, local)| {
                        Ok(RegionalMemoryValueBinding {
                            local,
                            source_memory,
                            kind: RegionalMemoryValueKind::Operation,
                            ordinal: u32::try_from(ordinal).map_err(|_| {
                                crate::SynthError::capacity("region-local memory operation ordinal")
                            })?,
                        })
                    })
                    .collect::<Result<Vec<_>, crate::SynthError>>()?,
            );
            memory_values.extend(
                memory_ownership
                    .state_values()
                    .filter_map(|(local, owner)| (owner == local_memory).then_some(local))
                    .enumerate()
                    .map(|(ordinal, local)| {
                        Ok(RegionalMemoryValueBinding {
                            local,
                            source_memory,
                            kind: RegionalMemoryValueKind::State,
                            ordinal: u32::try_from(ordinal).map_err(|_| {
                                crate::SynthError::capacity("region-local memory state ordinal")
                            })?,
                        })
                    })
                    .collect::<Result<Vec<_>, crate::SynthError>>()?,
            );
        }
        Ok(Self {
            module: importer.module,
            source_to_local: importer.source_to_local,
            boundary_bindings: importer.boundary_bindings.into_boxed_slice(),
            operation_sources: importer.operation_sources.into_boxed_slice(),
            memory_values: memory_values.into_boxed_slice(),
            root_bindings: root_bindings.into_boxed_slice(),
        })
    }

    pub(crate) fn into_parts(self) -> RegionalWordConeParts {
        RegionalWordConeParts {
            module: self.module,
            source_to_local: self.source_to_local,
            boundary_bindings: self.boundary_bindings,
            operation_sources: self.operation_sources,
            memory_values: self.memory_values,
            root_bindings: self.root_bindings,
        }
    }
}

struct RegionalWordImporter<'a> {
    source: &'a word::WordModule,
    signal_drivers: crate::word::signal_driver::SignalDriverIndex,
    operation_regions: &'a [Option<crate::RegionRowId>],
    region: crate::RegionRowId,
    module: word::WordModule,
    source_to_local: BTreeMap<word::ValueId, word::ValueId>,
    boundary_bindings: Vec<(word::ValueId, word::ValueId)>,
    operation_sources: Vec<Option<word::OpId>>,
    visiting: BTreeSet<word::ValueId>,
    import_path: Vec<word::ValueId>,
    source_acyclic: bool,
    recursive_boundaries: BTreeMap<word::ValueId, word::ValueId>,
    memory_signals: BTreeMap<word::SignalId, word::SignalId>,
    imported_bits: BTreeMap<(word::ValueId, u32), word::ValueId>,
    boundary_signals: BTreeMap<word::SignalRef, (word::WordType, word::ValueId)>,
    boundary_port_signals: BTreeMap<word::SignalId, word::SignalId>,
    boundary_inputs: std::collections::BTreeSet<word::ValueId>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use opto_core::DiagnosticSource;

    #[test]
    fn reports_combinational_feedback_with_hdl_locations() {
        let mut source = word::WordModule::new("feedback");
        let bit = word::WordType::bits(1).unwrap();
        let left = source
            .add_wire(
                "left",
                bit,
                word::SourceSpan::located("feedback.sv", Some(1), Some(7), "net"),
            )
            .unwrap();
        let right = source
            .add_wire(
                "right",
                bit,
                word::SourceSpan::located("feedback.sv", Some(1), Some(13), "net"),
            )
            .unwrap();
        let left_value = source
            .read_signal(
                left,
                word::SourceSpan::located("feedback.sv", Some(2), Some(17), "data assignment"),
            )
            .unwrap();
        let right_value = source
            .read_signal(
                right,
                word::SourceSpan::located("feedback.sv", Some(3), Some(18), "data assignment"),
            )
            .unwrap();
        source
            .connect(
                word::LValue::signal(left),
                right_value,
                word::SourceSpan::located("feedback.sv", Some(2), Some(3), "data assignment"),
            )
            .unwrap();
        source
            .connect(
                word::LValue::signal(right),
                left_value,
                word::SourceSpan::located("feedback.sv", Some(3), Some(3), "data assignment"),
            )
            .unwrap();
        let row = crate::RegionRowId::from_index(0).unwrap();

        let Err(error) = RegionalWordCone::build(RegionalWordConeRequest {
            source: &source,
            operation_regions: &[],
            region: row,
            memories: &[],
            memory_implementations: &[],
            target_cells: &opto_library::TargetCellSet::default(),
            boundary_inputs: &[],
            roots: vec![left_value],
        }) else {
            panic!("combinational feedback unexpectedly imported");
        };
        let crate::SynthError::CombinationalCycle(cycle) = &error else {
            panic!("unexpected error: {error:?}");
        };

        assert_eq!(
            error.to_string(),
            "combinational loop detected in module 'feedback'"
        );
        assert!(!error.to_string().contains("ValueId"));
        assert_eq!(cycle.debug_values(), &[left_value, right_value, left_value]);
        assert_eq!(cycle.nodes().len(), 2);
        let diagnostic = error.diagnostic().unwrap();
        assert_eq!(diagnostic.code(), "OPT-SYN-001");
        assert_eq!(diagnostic.primary().unwrap().location().line(), 2);
        assert_eq!(diagnostic.related()[0].location().line(), 3);
        assert!(
            diagnostic
                .primary()
                .unwrap()
                .message()
                .contains("signal 'left'")
        );
    }

    #[test]
    fn imports_only_the_owned_dependency_cone() {
        let mut source = word::WordModule::new("top");
        let ty = word::WordType::bits(1).unwrap();
        let left = source
            .add_port(
                "left",
                word::PortDirection::Input,
                ty,
                word::SourceSpan::default(),
            )
            .unwrap();
        let right = source
            .add_port(
                "right",
                word::PortDirection::Input,
                ty,
                word::SourceSpan::default(),
            )
            .unwrap();
        let left = source
            .read_signal(
                source.port(left).unwrap().signal,
                word::SourceSpan::default(),
            )
            .unwrap();
        let right = source
            .read_signal(
                source.port(right).unwrap().signal,
                word::SourceSpan::default(),
            )
            .unwrap();
        let first = source
            .unary(word::UnaryOp::BitNot, left, word::SourceSpan::default())
            .unwrap();
        let second = source
            .unary(word::UnaryOp::BitNot, right, word::SourceSpan::default())
            .unwrap();
        let first_row = crate::RegionRowId::from_index(0).unwrap();
        let second_row = crate::RegionRowId::from_index(1).unwrap();

        let cone = RegionalWordCone::build(RegionalWordConeRequest {
            source: &source,
            operation_regions: &[Some(first_row), Some(second_row)],
            region: first_row,
            memories: &[],
            memory_implementations: &[],
            target_cells: &opto_library::TargetCellSet::default(),
            boundary_inputs: &[],
            roots: vec![first],
        })
        .unwrap();
        let RegionalWordConeParts {
            module,
            source_to_local: values,
            operation_sources: operations,
            ..
        } = cone.into_parts();

        assert_eq!(module.operations().len(), 1);
        assert_eq!(
            operations.as_ref(),
            &[Some(word::OpId::from_index(0).unwrap())]
        );
        assert!(values.contains_key(&left));
        assert!(values.contains_key(&first));
        assert!(!values.contains_key(&right));
        assert!(!values.contains_key(&second));
    }

    #[test]
    fn represents_state_as_one_private_boundary() {
        let mut source = word::WordModule::new("feedback");
        let bit = word::WordType::bits(1).unwrap();
        let clock = source
            .add_port(
                "clock",
                word::PortDirection::Input,
                bit,
                word::SourceSpan::default(),
            )
            .unwrap();
        let clock = source
            .read_signal(
                source.port(clock).unwrap().signal,
                word::SourceSpan::default(),
            )
            .unwrap();
        let state = source
            .add_wire("state", bit, word::SourceSpan::default())
            .unwrap();
        let state_value = source
            .read_signal(state, word::SourceSpan::default())
            .unwrap();
        let next = source
            .unary(
                word::UnaryOp::BitNot,
                state_value,
                word::SourceSpan::default(),
            )
            .unwrap();
        let register = source
            .register(
                word::RegisterOp {
                    name: None,
                    d: next,
                    clock,
                    edge: word::Edge::Pos,
                    enable: None,
                    resets: Vec::new(),
                },
                word::SourceSpan::default(),
            )
            .unwrap();
        source
            .connect(
                word::LValue::signal(state),
                register,
                word::SourceSpan::default(),
            )
            .unwrap();
        let row = crate::RegionRowId::from_index(0).unwrap();

        let cone = RegionalWordCone::build(RegionalWordConeRequest {
            source: &source,
            operation_regions: &[Some(row), Some(row)],
            region: row,
            memories: &[],
            memory_implementations: &[],
            target_cells: &opto_library::TargetCellSet::default(),
            boundary_inputs: &[],
            roots: vec![register, next],
        })
        .unwrap();
        let RegionalWordConeParts {
            module,
            operation_sources: operations,
            ..
        } = cone.into_parts();

        assert_eq!(module.operations().len(), 1);
        assert_eq!(
            operations.as_ref(),
            &[Some(word::OpId::from_index(0).unwrap())]
        );
        assert_eq!(
            module
                .ports()
                .iter()
                .filter(|port| port.direction == word::PortDirection::Input)
                .count(),
            1
        );
    }

    #[test]
    fn follows_an_exact_intra_region_signal_driver_in_dependency_order() {
        let mut source = word::WordModule::new("top");
        let ty = word::WordType::bits(1).unwrap();
        let inputs = ["left", "right"].map(|name| {
            let port = source
                .add_port(
                    name,
                    word::PortDirection::Input,
                    ty,
                    word::SourceSpan::default(),
                )
                .unwrap();
            source
                .read_signal(
                    source.port(port).unwrap().signal,
                    word::SourceSpan::default(),
                )
                .unwrap()
        });
        let alias = source
            .add_wire("alias", ty, word::SourceSpan::default())
            .unwrap();
        let alias_value = source
            .read_signal(alias, word::SourceSpan::default())
            .unwrap();
        let consumer = source
            .unary(
                word::UnaryOp::BitNot,
                alias_value,
                word::SourceSpan::default(),
            )
            .unwrap();
        let driver = source
            .binary(
                word::BinaryOp::BitAnd,
                inputs[0],
                inputs[1],
                word::SourceSpan::default(),
            )
            .unwrap();
        source
            .connect(
                word::LValue::signal(alias),
                driver,
                word::SourceSpan::default(),
            )
            .unwrap();
        let row = crate::RegionRowId::from_index(0).unwrap();

        let cone = RegionalWordCone::build(RegionalWordConeRequest {
            source: &source,
            operation_regions: &[Some(row), Some(row)],
            region: row,
            memories: &[],
            memory_implementations: &[],
            target_cells: &opto_library::TargetCellSet::default(),
            boundary_inputs: &[],
            roots: vec![consumer],
        })
        .unwrap();
        let RegionalWordConeParts {
            module,
            operation_sources,
            ..
        } = cone.into_parts();

        assert_eq!(module.operations().len(), 2);
        assert_eq!(
            operation_sources[0],
            Some(word::OpId::from_index(1).unwrap())
        );
        assert_eq!(
            operation_sources[1],
            Some(word::OpId::from_index(0).unwrap())
        );
    }

    #[test]
    fn cuts_static_signal_slices_at_the_declared_region_boundary() {
        let mut source = word::WordModule::new("boundary_slice");
        let ty = word::WordType::bits(2).unwrap();
        let inputs = ["left", "right"].map(|name| {
            let port = source
                .add_port(
                    name,
                    word::PortDirection::Input,
                    ty,
                    word::SourceSpan::default(),
                )
                .unwrap();
            source
                .read_signal(
                    source.port(port).unwrap().signal,
                    word::SourceSpan::default(),
                )
                .unwrap()
        });
        let producer = source
            .binary(
                word::BinaryOp::BitXor,
                inputs[0],
                inputs[1],
                word::SourceSpan::default(),
            )
            .unwrap();
        let bus = source
            .add_wire("bus", ty, word::SourceSpan::default())
            .unwrap();
        source
            .connect(
                word::LValue::signal(bus),
                producer,
                word::SourceSpan::default(),
            )
            .unwrap();
        let boundary = source
            .read_signal_slice(bus, 0, 1, word::SourceSpan::default())
            .unwrap();
        let consumer = source
            .unary(word::UnaryOp::BitNot, boundary, word::SourceSpan::default())
            .unwrap();
        let producer_row = crate::RegionRowId::from_index(0).unwrap();
        let consumer_row = crate::RegionRowId::from_index(1).unwrap();

        let cone = RegionalWordCone::build(RegionalWordConeRequest {
            source: &source,
            operation_regions: &[Some(producer_row), Some(consumer_row)],
            region: consumer_row,
            memories: &[],
            memory_implementations: &[],
            target_cells: &opto_library::TargetCellSet::default(),
            boundary_inputs: &[boundary],
            roots: vec![consumer],
        })
        .unwrap();
        let RegionalWordConeParts {
            module,
            source_to_local,
            operation_sources,
            ..
        } = cone.into_parts();

        assert!(matches!(
            module.value(source_to_local[&boundary]).unwrap().kind,
            word::ValueKind::Signal(_)
        ));
        assert_eq!(
            operation_sources.as_ref(),
            &[Some(word::OpId::from_index(1).unwrap())]
        );
        assert!(module.operations().iter().all(|operation| !matches!(
            operation.kind,
            word::OpKind::Extract { .. } | word::OpKind::Concat { .. }
        )));
    }

    #[test]
    fn reconstructs_static_intra_region_signal_slices() {
        let mut source = word::WordModule::new("top");
        let ty = word::WordType::bits(4).unwrap();
        let inputs = ["left", "right"].map(|name| {
            let port = source
                .add_port(
                    name,
                    word::PortDirection::Input,
                    ty,
                    word::SourceSpan::default(),
                )
                .unwrap();
            source
                .read_signal(
                    source.port(port).unwrap().signal,
                    word::SourceSpan::default(),
                )
                .unwrap()
        });
        let driver = source
            .binary(
                word::BinaryOp::BitXor,
                inputs[0],
                inputs[1],
                word::SourceSpan::default(),
            )
            .unwrap();
        let alias = source
            .add_wire("alias", ty, word::SourceSpan::default())
            .unwrap();
        source
            .connect(
                word::LValue::signal(alias),
                driver,
                word::SourceSpan::default(),
            )
            .unwrap();
        let root = source
            .read_signal_slice(alias, 1, 2, word::SourceSpan::default())
            .unwrap();
        let row = crate::RegionRowId::from_index(0).unwrap();

        let cone = RegionalWordCone::build(RegionalWordConeRequest {
            source: &source,
            operation_regions: &[Some(row)],
            region: row,
            memories: &[],
            memory_implementations: &[],
            target_cells: &opto_library::TargetCellSet::default(),
            boundary_inputs: &[],
            roots: vec![root],
        })
        .unwrap();
        let RegionalWordConeParts {
            module,
            source_to_local,
            operation_sources,
            ..
        } = cone.into_parts();

        assert_eq!(
            module
                .ports()
                .iter()
                .filter(|port| port.direction == word::PortDirection::Input)
                .count(),
            2
        );
        assert_eq!(
            module
                .ports()
                .iter()
                .filter(|port| port.direction == word::PortDirection::Output)
                .count(),
            1
        );
        assert!(matches!(
            module.value(source_to_local[&root]).unwrap().kind,
            word::ValueKind::Operation(_)
        ));
        assert_eq!(
            operation_sources[0],
            Some(word::OpId::from_index(0).unwrap())
        );
        assert!(operation_sources[1..].iter().all(Option::is_none));
    }

    #[test]
    fn repeated_signal_reads_share_one_private_boundary() {
        let mut source = word::WordModule::new("read_aliases");
        let bit = word::WordType::bits(1).unwrap();
        let port = source
            .add_port(
                "a",
                word::PortDirection::Input,
                bit,
                word::SourceSpan::default(),
            )
            .unwrap();
        let signal = source.port(port).unwrap().signal;
        let first = source
            .read_signal(signal, word::SourceSpan::default())
            .unwrap();
        let second = source
            .read_signal(signal, word::SourceSpan::default())
            .unwrap();
        let root = source
            .binary(
                word::BinaryOp::BitXor,
                first,
                second,
                word::SourceSpan::default(),
            )
            .unwrap();
        let row = crate::RegionRowId::from_index(0).unwrap();

        let cone = RegionalWordCone::build(RegionalWordConeRequest {
            source: &source,
            operation_regions: &[Some(row)],
            region: row,
            memories: &[],
            memory_implementations: &[],
            target_cells: &opto_library::TargetCellSet::default(),
            boundary_inputs: &[first],
            roots: vec![root],
        })
        .unwrap();
        let RegionalWordConeParts {
            module,
            source_to_local,
            ..
        } = cone.into_parts();

        assert_eq!(module.ports().len(), 2);
        assert_eq!(source_to_local[&first], source_to_local[&second]);
    }

    #[test]
    fn overlapping_boundary_slices_share_one_backing_port() {
        let mut source = word::WordModule::new("overlapping_boundaries");
        let ty = word::WordType::bits(23).unwrap();
        let port = source
            .add_port(
                "input",
                word::PortDirection::Input,
                ty,
                word::SourceSpan::default(),
            )
            .unwrap();
        let full = source
            .read_signal(
                source.port(port).unwrap().signal,
                word::SourceSpan::default(),
            )
            .unwrap();
        let lower = source
            .read_signal_slice(
                source.port(port).unwrap().signal,
                0,
                22,
                word::SourceSpan::default(),
            )
            .unwrap();
        let row = crate::RegionRowId::from_index(0).unwrap();

        let cone = RegionalWordCone::build(RegionalWordConeRequest {
            source: &source,
            operation_regions: &[],
            region: row,
            memories: &[],
            memory_implementations: &[],
            target_cells: &opto_library::TargetCellSet::default(),
            boundary_inputs: &[full, lower],
            roots: vec![full, lower],
        })
        .unwrap();
        let RegionalWordConeParts {
            module,
            source_to_local,
            ..
        } = cone.into_parts();

        let word::ValueKind::Signal(full_reference) =
            module.value(source_to_local[&full]).unwrap().kind
        else {
            panic!("full boundary must remain a signal reference");
        };
        let word::ValueKind::Signal(lower_reference) =
            module.value(source_to_local[&lower]).unwrap().kind
        else {
            panic!("lower boundary must remain a signal reference");
        };
        assert_eq!(full_reference.signal, lower_reference.signal);
        assert_eq!(full_reference.lsb, 0);
        assert_eq!(lower_reference.lsb, 0);
        assert_eq!(full_reference.width(), 23);
        assert_eq!(lower_reference.width(), 22);
        assert_eq!(
            module
                .ports()
                .iter()
                .filter(|port| port.direction == word::PortDirection::Input)
                .count(),
            1
        );
    }

    #[test]
    fn follows_disjoint_packed_fields_without_a_whole_value_boundary() {
        let mut source = word::WordModule::new("packed_fields");
        let bit = word::WordType::bits(1).unwrap();
        let pair = word::WordType::bits(2).unwrap();
        let address_port = source
            .add_port(
                "address",
                word::PortDirection::Input,
                bit,
                word::SourceSpan::default(),
            )
            .unwrap();
        let request = source
            .add_wire("request", pair, word::SourceSpan::default())
            .unwrap();
        let old_address = source
            .read_signal_slice(request, 0, 1, word::SourceSpan::default())
            .unwrap();
        let flag = source
            .unary(
                word::UnaryOp::LogicalNot,
                old_address,
                word::SourceSpan::default(),
            )
            .unwrap();
        let new_address = source
            .read_signal(
                source.port(address_port).unwrap().signal,
                word::SourceSpan::default(),
            )
            .unwrap();
        let assembled = source
            .concat(vec![flag, new_address], word::SourceSpan::default())
            .unwrap();
        source
            .connect(
                word::LValue::signal(request),
                assembled,
                word::SourceSpan::default(),
            )
            .unwrap();
        let root = source
            .read_signal_slice(request, 1, 1, word::SourceSpan::default())
            .unwrap();
        let row = crate::RegionRowId::from_index(0).unwrap();

        crate::word::cycle::validate_combinational_acyclic(&source).unwrap();
        let cone = RegionalWordCone::build(RegionalWordConeRequest {
            source: &source,
            operation_regions: &[Some(row), Some(row)],
            region: row,
            memories: &[],
            memory_implementations: &[],
            target_cells: &opto_library::TargetCellSet::default(),
            boundary_inputs: &[],
            roots: vec![root],
        })
        .unwrap();
        let RegionalWordConeParts {
            source_to_local,
            boundary_bindings,
            ..
        } = cone.into_parts();

        assert!(source_to_local.contains_key(&root));
        assert!(
            boundary_bindings
                .iter()
                .all(|&(source, _)| source != assembled)
        );
    }
}
