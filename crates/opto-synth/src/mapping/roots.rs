// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::word;
use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct MappingRoot {
    pub(crate) value: word::ValueId,
    pub(crate) required_time: Option<f64>,
    pub(crate) output_load: Option<f64>,
    pub(crate) requires_combinational_cover: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CanonicalPublicationBit {
    Value { value: word::ValueId, bit: u32 },
    Constant(opto_ir::BitVal),
}

/// Full-Word publication proof frozen before any region-local simplification.
pub(crate) struct FullDomainRootSemantics<'a> {
    module: &'a word::WordModule,
    connectivity: crate::word::bit_connectivity::BitConnectivity<'a>,
}

impl<'a> FullDomainRootSemantics<'a> {
    pub(crate) fn new(module: &'a word::WordModule) -> Result<Self, crate::SynthError> {
        Ok(Self {
            module,
            connectivity: crate::word::bit_connectivity::BitConnectivity::new(module)?,
        })
    }

    pub(crate) fn requires_artifact(
        &self,
        value: word::ValueId,
    ) -> Result<bool, crate::SynthError> {
        self.prove(value, &mut BTreeSet::new(), &mut BTreeMap::new())
    }

    /// Return the frozen publication obligation for one bit of a Word value.
    ///
    /// Width-only operations preserve distinct bit identities, so their
    /// obligations cannot be represented by one aggregate flag before
    /// bitblasting. This proof follows only exact structural projections;
    /// ordinary combinational operations remain owned artifacts.
    pub(crate) fn bit_requires_artifact(
        &self,
        value: word::ValueId,
        bit: u32,
    ) -> Result<bool, crate::SynthError> {
        match self.connectivity.source(value, bit)? {
            crate::word::bit_connectivity::BitSource::Constant(_) => Ok(false),
            crate::word::bit_connectivity::BitSource::Value { value, .. } => {
                let stored = self.module.value(value).ok_or_else(|| {
                    crate::SynthError::invariant("publication bit source is unknown")
                })?;
                match stored.kind {
                    word::ValueKind::Signal(reference) => {
                        Ok(!self.signal_is_physical_boundary(reference))
                    }
                    word::ValueKind::Operation(operation) => {
                        let operation = self.module.operation(operation).ok_or_else(|| {
                            crate::SynthError::invariant("publication operation is unknown")
                        })?;
                        Ok(!matches!(
                            operation.kind,
                            word::OpKind::Register(_) | word::OpKind::Latch(_)
                        ))
                    }
                    word::ValueKind::Constant(_) => Err(crate::SynthError::invariant(
                        "constant publication bit did not resolve as a constant",
                    )),
                }
            }
        }
    }

    pub(crate) fn canonical_publication_bit(
        &self,
        value: word::ValueId,
        bit: u32,
    ) -> Result<CanonicalPublicationBit, crate::SynthError> {
        match self.connectivity.source(value, bit)? {
            crate::word::bit_connectivity::BitSource::Value { value, bit } => {
                Ok(CanonicalPublicationBit::Value { value, bit })
            }
            crate::word::bit_connectivity::BitSource::Constant(bit) => {
                let stored = self.module.value(value).ok_or_else(|| {
                    crate::SynthError::invariant("publication constant source is unknown")
                })?;
                Ok(CanonicalPublicationBit::Constant(
                    crate::boolean::resolve_publication_bit(
                        bit,
                        self.module.name(),
                        &stored.source,
                    )?,
                ))
            }
        }
    }

    fn signal_is_physical_boundary(&self, reference: word::SignalRef) -> bool {
        self.module.signal(reference.signal).is_some_and(|signal| {
            if signal.resolution == word::SignalResolution::TriState {
                return true;
            }
            let word::SignalKind::Port(port) = signal.kind else {
                return false;
            };
            self.module.port(port).is_some_and(|port| {
                matches!(
                    port.direction,
                    word::PortDirection::Input | word::PortDirection::Inout
                )
            })
        })
    }

    pub(crate) fn canonical_root(
        &self,
        value: word::ValueId,
    ) -> Result<word::ValueId, crate::SynthError> {
        let mut current = value;
        let mut visited = BTreeSet::new();
        loop {
            if !visited.insert(current) {
                return Err(crate::SynthError::invariant(
                    "publication identity contains an exact-alias cycle",
                ));
            }
            let stored = self.module.value(current).ok_or_else(|| {
                crate::SynthError::invariant(
                    "publication identity references an unknown source value",
                )
            })?;
            let next = match stored.kind {
                word::ValueKind::Signal(reference) => self
                    .connectivity
                    .exact_reference_driver(reference, stored.ty),
                word::ValueKind::Operation(operation) => self
                    .module
                    .operation(operation)
                    .and_then(|operation| scalar_projection_input(self.module, operation)),
                word::ValueKind::Constant(_) => None,
            };
            match next {
                Some(next) => current = next,
                None => return Ok(current),
            }
        }
    }

    fn prove(
        &self,
        value: word::ValueId,
        active: &mut BTreeSet<word::ValueId>,
        proven: &mut BTreeMap<word::ValueId, bool>,
    ) -> Result<bool, crate::SynthError> {
        if let Some(&result) = proven.get(&value) {
            return Ok(result);
        }
        if !active.insert(value) {
            return Err(crate::SynthError::invariant(
                "publication connectivity contains a cycle",
            ));
        }
        let stored = self.module.value(value).ok_or_else(|| {
            crate::SynthError::invariant(format!("unknown publication root {value:?}"))
        })?;
        let result = match stored.kind {
            word::ValueKind::Constant(_) => false,
            word::ValueKind::Signal(reference) => {
                match self.connectivity.reference_drivers(reference) {
                    Some(drivers) if !drivers.is_empty() => {
                        let mut required = false;
                        for driver in drivers {
                            required |= self.prove(driver, active, proven)?;
                        }
                        required
                    }
                    _ => !self.signal_is_physical_boundary(reference),
                }
            }
            word::ValueKind::Operation(operation) => {
                let operation = self.module.operation(operation).ok_or_else(|| {
                    crate::SynthError::invariant("publication root operation is unknown")
                })?;
                match &operation.kind {
                    word::OpKind::Register(_) | word::OpKind::Latch(_) => false,
                    word::OpKind::Cast { value, .. } | word::OpKind::Extract { value, .. } => {
                        self.prove(*value, active, proven)?
                    }
                    word::OpKind::Concat { parts } => {
                        let mut required = false;
                        for &part in parts {
                            required |= self.prove(part, active, proven)?;
                        }
                        required
                    }
                    word::OpKind::Unary { .. }
                    | word::OpKind::Binary { .. }
                    | word::OpKind::Mux { .. }
                    | word::OpKind::TriState { .. }
                    | word::OpKind::DynamicExtract { .. }
                    | word::OpKind::DynamicInsert { .. } => true,
                }
            }
        };
        active.remove(&value);
        proven.insert(value, result);
        Ok(result)
    }
}
/// Returns the input of a globally exact scalar pass-through operation.
pub(crate) fn scalar_projection_input(
    module: &word::WordModule,
    operation: &word::Operation,
) -> Option<word::ValueId> {
    let result = module.value(operation.result)?;
    if result.ty.width() != 1 {
        return None;
    }
    match &operation.kind {
        word::OpKind::Cast { value, .. }
            if module
                .value(*value)
                .is_some_and(|value| value.ty.width() == 1) =>
        {
            Some(*value)
        }
        word::OpKind::Extract { value, lsb, .. }
            if *lsb == 0
                && module
                    .value(*value)
                    .is_some_and(|value| value.ty.width() == 1) =>
        {
            Some(*value)
        }
        word::OpKind::Concat { parts } if parts.len() == 1 => Some(parts[0]),
        word::OpKind::Unary { .. }
        | word::OpKind::Binary { .. }
        | word::OpKind::Mux { .. }
        | word::OpKind::TriState { .. }
        | word::OpKind::Register(_)
        | word::OpKind::Latch(_)
        | word::OpKind::Extract { .. }
        | word::OpKind::DynamicExtract { .. }
        | word::OpKind::DynamicInsert { .. }
        | word::OpKind::Cast { .. }
        | word::OpKind::Concat { .. } => None,
    }
}

pub(crate) fn mapping_roots(
    module: &word::WordModule,
    timing: &opto_timing::TimingContext,
    port_bindings: &opto_timing::PortBindings,
    sequential_timing: Option<&super::sequential::SequentialTimingProjection>,
) -> Result<Vec<MappingRoot>, crate::SynthError> {
    let mut roots = Vec::new();
    let global_required = timing.minimum_synthesis_delay();
    let observability = crate::word::uses::netlist_observability(module)?;
    // State shells are semantic roots even when their results reach a signal
    // through Concat, Extract, or Cast rather than as its direct driver.
    for operation in module.operations() {
        if !observability.observes_value(operation.result)? {
            continue;
        }
        publish_state_roots(
            &mut roots,
            module,
            operation,
            timing,
            port_bindings,
            sequential_timing,
            global_required,
        );
    }
    for (connect_index, connect) in module.connects().iter().enumerate() {
        let is_live = observability.observes_connect(connect_index)?;
        let is_publication = observability.observes_publication_connect(connect_index)?;
        if !is_live {
            continue;
        }
        let endpoint = timing_port_for_signal(module, connect.target.signal, port_bindings);
        let endpoint_required = endpoint
            .and_then(|port| timing.minimum_max_delay_to(opto_timing::TimingEndpoint::Port(port)))
            .or(global_required);
        let output_load = endpoint.and_then(|port| timing.load_on(port));
        let value = module.value(connect.value).ok_or_else(|| {
            crate::SynthError::invariant(format!("unknown RTL value {:?}", connect.value))
        })?;
        let operation = match value.kind {
            word::ValueKind::Operation(operation_id) => {
                Some(module.operation(operation_id).ok_or_else(|| {
                    crate::SynthError::invariant(format!("unknown RTL operation {operation_id:?}"))
                })?)
            }
            word::ValueKind::Signal(_) | word::ValueKind::Constant(_) => None,
        };
        if let Some(operation) = operation
            && let word::OpKind::TriState { data, enable } = operation.kind
        {
            roots.extend(
                connect
                    .target
                    .dynamic
                    .map(|dynamic| timed_root(dynamic.offset, endpoint_required)),
            );
            roots.push(timed_root(data, endpoint_required));
            roots.push(timed_root(enable.value, endpoint_required));
            continue;
        }
        if !is_publication {
            continue;
        }
        if let Some(dynamic) = connect.target.dynamic {
            roots.push(timed_root(dynamic.offset, endpoint_required));
        }
        if operation.is_some_and(|operation| {
            matches!(
                operation.kind,
                word::OpKind::Register(_) | word::OpKind::Latch(_)
            )
        }) {
            continue;
        }
        roots.push(MappingRoot {
            value: connect.value,
            required_time: endpoint_required,
            output_load,
            requires_combinational_cover: false,
        });
    }
    for &value in observability.instance_root_values() {
        roots.extend(
            scalar_value_parts(module, value)?
                .into_iter()
                .map(unconstrained_root),
        );
    }
    Ok(merge_by_value(roots))
}

pub(crate) fn state_mapping_roots(
    module: &word::WordModule,
    operations: impl IntoIterator<Item = word::OpId>,
    timing: &opto_timing::TimingContext,
    port_bindings: &opto_timing::PortBindings,
    sequential_timing: Option<&super::sequential::SequentialTimingProjection>,
) -> Result<Vec<MappingRoot>, crate::SynthError> {
    let global_required = timing.minimum_synthesis_delay();
    let mut roots = Vec::new();
    for operation in operations {
        let operation = module.operation(operation).ok_or_else(|| {
            crate::SynthError::invariant("explicit state mapping root is not live")
        })?;
        if !publish_state_roots(
            &mut roots,
            module,
            operation,
            timing,
            port_bindings,
            sequential_timing,
            global_required,
        ) {
            return Err(crate::SynthError::invariant(
                "explicit state mapping root is combinational",
            ));
        }
    }
    Ok(merge_by_value(roots))
}

fn publish_state_roots(
    roots: &mut Vec<MappingRoot>,
    module: &word::WordModule,
    operation: &word::Operation,
    timing: &opto_timing::TimingContext,
    port_bindings: &opto_timing::PortBindings,
    sequential_timing: Option<&super::sequential::SequentialTimingProjection>,
    global_required: Option<f64>,
) -> bool {
    match &operation.kind {
        word::OpKind::Register(register) => {
            let mut required = timing_port_for_value(module, register.clock, port_bindings)
                .and_then(|port| timing.minimum_clock_period_on(port))
                .or(global_required);
            if let (Some(current), Some(setup)) = (
                required,
                sequential_timing.and_then(|projection| projection.setup(operation.result)),
            ) {
                required = Some(current - setup);
            }
            roots.push(MappingRoot {
                value: register.d,
                required_time: required,
                output_load: None,
                requires_combinational_cover: false,
            });
            roots.push(timed_root(register.clock, global_required));
            roots.extend(
                register
                    .enable
                    .map(|enable| timed_root(enable.value, global_required)),
            );
            for reset in &register.resets {
                roots.push(timed_root(reset.value, global_required));
                roots.push(timed_root(reset.reset_value, global_required));
            }
        }
        word::OpKind::Latch(latch) => {
            roots.push(MappingRoot {
                value: latch.d,
                required_time: global_required,
                output_load: None,
                requires_combinational_cover: false,
            });
            roots.push(timed_root(latch.enable.value, global_required));
            for reset in &latch.resets {
                roots.push(timed_root(reset.value, global_required));
                roots.push(timed_root(reset.reset_value, global_required));
            }
        }
        _ => return false,
    }
    true
}

/// Folds every sink constraint on a value into the single root that names it.
///
/// One value routinely reaches several sinks — a clock read by every register,
/// or the shared next-state cone of two equivalent FSM states. Each sink states
/// its own requirement, but the value is one mapping slot, so it must satisfy
/// the tightest required time and drive the total external load.
/// Discovery order is preserved: it is the order the cover visits roots in, so
/// merging must not silently reorder the mapping problem.
pub(crate) fn merge_by_value(roots: Vec<MappingRoot>) -> Vec<MappingRoot> {
    let mut slots = BTreeMap::<word::ValueId, usize>::new();
    let mut merged = Vec::with_capacity(roots.len());
    for root in roots {
        match slots.entry(root.value) {
            Entry::Vacant(slot) => {
                slot.insert(merged.len());
                merged.push(root);
            }
            Entry::Occupied(slot) => {
                let current: &mut MappingRoot = &mut merged[*slot.get()];
                current.required_time = match (current.required_time, root.required_time) {
                    (Some(current), Some(other)) => Some(current.min(other)),
                    (current, other) => current.or(other),
                };
                current.output_load = match (current.output_load, root.output_load) {
                    (Some(current), Some(other)) => Some(current + other),
                    (current, other) => current.or(other),
                };
                current.requires_combinational_cover |= root.requires_combinational_cover;
            }
        }
    }
    merged
}

fn unconstrained_root(value: word::ValueId) -> MappingRoot {
    timed_root(value, None)
}

fn timed_root(value: word::ValueId, required_time: Option<f64>) -> MappingRoot {
    MappingRoot {
        value,
        required_time,
        output_load: None,
        requires_combinational_cover: false,
    }
}

fn timing_port_for_value(
    module: &word::WordModule,
    value: word::ValueId,
    port_bindings: &opto_timing::PortBindings,
) -> Option<opto_timing::PortId> {
    let word::ValueKind::Signal(reference) = module.value(value)?.kind else {
        return None;
    };
    timing_port_for_signal(module, reference.signal, port_bindings)
}

fn timing_port_for_signal(
    module: &word::WordModule,
    signal: word::SignalId,
    port_bindings: &opto_timing::PortBindings,
) -> Option<opto_timing::PortId> {
    let word::SignalKind::Port(port) = module.signal(signal)?.kind else {
        return None;
    };
    port_bindings.get(port.index())
}

pub(crate) fn scalar_value_parts(
    module: &word::WordModule,
    value: word::ValueId,
) -> Result<Vec<word::ValueId>, crate::SynthError> {
    let stored = module.value(value).ok_or_else(|| {
        crate::SynthError::invariant(format!("unknown instance connection value {value:?}"))
    })?;
    if stored.ty.width() == 1 {
        return Ok(vec![value]);
    }
    let word::ValueKind::Operation(operation) = stored.kind else {
        return Err(crate::SynthError::invariant(
            "vector instance connection was not bitblasted",
        ));
    };
    let operation = module.operation(operation).ok_or_else(|| {
        crate::SynthError::invariant(format!(
            "unknown instance connection operation {operation:?}"
        ))
    })?;
    let word::OpKind::Concat { parts } = &operation.kind else {
        return Err(crate::SynthError::invariant(
            "vector instance connection is not a concatenation of scalar bits",
        ));
    };
    let mut bits = Vec::with_capacity(parts.len());
    for &part in parts.iter().rev() {
        bits.extend(scalar_value_parts(module, part)?);
    }
    Ok(bits)
}

#[cfg(test)]
mod tests {
    use super::*;
    use opto_ir::word::{LValue, PortDirection, SourceSpan, WordModule, WordType};
    use std::num::NonZeroU32;

    #[test]
    fn output_roots_keep_endpoint_specific_budget_and_load() {
        let mut module = WordModule::new("top");
        let input = module
            .add_port(
                "a",
                PortDirection::Input,
                WordType::bits(1).unwrap(),
                SourceSpan::default(),
            )
            .unwrap();
        let output = module
            .add_port(
                "y",
                PortDirection::Output,
                WordType::bits(1).unwrap(),
                SourceSpan::default(),
            )
            .unwrap();
        let value = module
            .read_signal(module.port(input).unwrap().signal, SourceSpan::default())
            .unwrap();
        module
            .connect(
                LValue::signal(module.port(output).unwrap().signal),
                value,
                SourceSpan::default(),
            )
            .unwrap();
        let dead = module
            .unary(word::UnaryOp::BitNot, value, SourceSpan::default())
            .unwrap();
        let dead_signal = module
            .add_wire("dead", WordType::bits(1).unwrap(), SourceSpan::default())
            .unwrap();
        let dead_connect = module.connects().len();
        module
            .connect(LValue::signal(dead_signal), dead, SourceSpan::default())
            .unwrap();

        let input_port = opto_timing::PortId::from_uid(opto_core::ObjectUid::from_raw(2).unwrap());
        let output_port = opto_timing::PortId::from_uid(opto_core::ObjectUid::from_raw(3).unwrap());
        let mut timing = opto_timing::TimingContext::new();
        timing
            .set_max_delay(
                0.8,
                Vec::new(),
                vec![opto_timing::TimingEndpoint::Port(output_port)],
            )
            .unwrap();
        timing.set_load(0.02, &[output_port]).unwrap();
        let port_bindings = opto_timing::PortBindings::new([input_port, output_port]);

        let roots = mapping_roots(&module, &timing, &port_bindings, None).unwrap();
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].value, value);
        assert_eq!(roots[0].required_time, Some(0.8));
        assert_eq!(roots[0].output_load, Some(0.02));

        let observability = crate::word::uses::netlist_observability(&module).unwrap();
        assert!(!observability.observes_value(dead).unwrap());
        assert!(!observability.observes_connect(dead_connect).unwrap());
    }

    #[test]
    fn live_physical_tri_state_shell_projects_data_and_enable_roots() {
        let mut module = WordModule::new("tri_state_roots");
        let bit = WordType::bits(1).unwrap();
        let inputs = ["data", "enable"].map(|name| {
            module
                .add_port(name, PortDirection::Input, bit, SourceSpan::default())
                .unwrap()
        });
        let [data, enable] = inputs.map(|port| {
            module
                .read_signal(module.port(port).unwrap().signal, SourceSpan::default())
                .unwrap()
        });
        let resolved = module
            .add_wire("resolved", bit, SourceSpan::default())
            .unwrap();
        module
            .set_signal_resolution(resolved, word::SignalResolution::TriState)
            .unwrap();
        let driver = module
            .tri_state(
                data,
                word::Enable {
                    value: enable,
                    active_high: true,
                },
                SourceSpan::default(),
            )
            .unwrap();
        module
            .connect(LValue::signal(resolved), driver, SourceSpan::default())
            .unwrap();
        let resolved = module.read_signal(resolved, SourceSpan::default()).unwrap();
        let output = module
            .add_port("y", PortDirection::Output, bit, SourceSpan::default())
            .unwrap();
        module
            .connect(
                LValue::signal(module.port(output).unwrap().signal),
                resolved,
                SourceSpan::default(),
            )
            .unwrap();

        let roots = mapping_roots(
            &module,
            &opto_timing::TimingContext::new(),
            &opto_timing::PortBindings::new([]),
            None,
        )
        .unwrap();

        assert!(roots.iter().any(|root| root.value == data));
        assert!(roots.iter().any(|root| root.value == enable));
    }

    #[test]
    fn live_dynamic_internal_connect_projects_publication_roots() {
        let mut module = WordModule::new("dynamic_connect_roots");
        let bit = WordType::bits(1).unwrap();
        let pair = WordType::bits(2).unwrap();
        let inputs = ["data", "offset"].map(|name| {
            module
                .add_port(name, PortDirection::Input, bit, SourceSpan::default())
                .unwrap()
        });
        let [data, offset] = inputs.map(|port| {
            module
                .read_signal(module.port(port).unwrap().signal, SourceSpan::default())
                .unwrap()
        });
        let driven = module
            .unary(word::UnaryOp::BitNot, data, SourceSpan::default())
            .unwrap();
        let internal = module
            .add_wire("internal", pair, SourceSpan::default())
            .unwrap();
        module
            .connect(
                LValue::signal(internal).with_dynamic_range(offset, NonZeroU32::new(1).unwrap()),
                driven,
                SourceSpan::default(),
            )
            .unwrap();
        let output = module
            .add_port("q", PortDirection::Output, pair, SourceSpan::default())
            .unwrap();
        let internal = module.read_signal(internal, SourceSpan::default()).unwrap();
        module
            .connect(
                LValue::signal(module.port(output).unwrap().signal),
                internal,
                SourceSpan::default(),
            )
            .unwrap();

        let roots = mapping_roots(
            &module,
            &opto_timing::TimingContext::new(),
            &opto_timing::PortBindings::new([]),
            None,
        )
        .unwrap();

        assert!(roots.iter().any(|root| root.value == driven));
        assert!(roots.iter().any(|root| root.value == offset));
        assert!(
            FullDomainRootSemantics::new(&module)
                .unwrap()
                .requires_artifact(driven)
                .unwrap()
        );
    }

    #[test]
    fn signal_wrapped_combinational_root_requires_artifact() {
        let mut module = WordModule::new("wrapped_mux");
        let bit = WordType::bits(1).unwrap();
        let inputs = ["select", "a", "b"].map(|name| {
            module
                .add_port(name, PortDirection::Input, bit, SourceSpan::default())
                .unwrap()
        });
        let [select, a, b] = inputs.map(|port| {
            module
                .read_signal(module.port(port).unwrap().signal, SourceSpan::default())
                .unwrap()
        });
        let selected = module.mux(select, a, b, SourceSpan::default()).unwrap();
        let internal = module
            .add_wire("selected", bit, SourceSpan::default())
            .unwrap();
        module
            .connect(LValue::signal(internal), selected, SourceSpan::default())
            .unwrap();
        let wrapped = module.read_signal(internal, SourceSpan::default()).unwrap();

        let full_domain = FullDomainRootSemantics::new(&module).unwrap();
        assert!(full_domain.requires_artifact(wrapped).unwrap());
        assert!(!full_domain.requires_artifact(a).unwrap());

        let undriven = module
            .add_wire("undriven", bit, SourceSpan::default())
            .unwrap();
        let undriven = module.read_signal(undriven, SourceSpan::default()).unwrap();
        let full_domain = FullDomainRootSemantics::new(&module).unwrap();
        assert!(full_domain.requires_artifact(undriven).unwrap());
    }

    #[test]
    fn publication_artifact_proof_memoizes_reconvergent_aliases() {
        let mut module = WordModule::new("reconvergent_aliases");
        let input = module
            .add_port(
                "input",
                PortDirection::Input,
                WordType::bits(1).unwrap(),
                SourceSpan::default(),
            )
            .unwrap();
        let mut value = module
            .read_signal(module.port(input).unwrap().signal, SourceSpan::default())
            .unwrap();
        for _ in 0..28 {
            value = module
                .concat(vec![value, value], SourceSpan::default())
                .unwrap();
        }

        let full_domain = FullDomainRootSemantics::new(&module).unwrap();
        assert!(!full_domain.requires_artifact(value).unwrap());
    }

    #[test]
    fn vector_projection_preserves_per_bit_publication_obligations() {
        let mut module = WordModule::new("mixed_projection");
        let bit = WordType::bits(1).unwrap();
        let input = module
            .add_port("input", PortDirection::Input, bit, SourceSpan::default())
            .unwrap();
        let input = module
            .read_signal(module.port(input).unwrap().signal, SourceSpan::default())
            .unwrap();
        let inverted = module
            .unary(word::UnaryOp::BitNot, input, SourceSpan::default())
            .unwrap();
        let packed = module
            .concat(vec![inverted, input], SourceSpan::default())
            .unwrap();

        let full_domain = FullDomainRootSemantics::new(&module).unwrap();
        assert!(!full_domain.bit_requires_artifact(packed, 0).unwrap());
        assert!(full_domain.bit_requires_artifact(packed, 1).unwrap());
    }

    #[test]
    fn memory_read_is_a_publication_obligation() {
        let mut module = WordModule::new("memory_root");
        let address_port = module
            .add_port(
                "address",
                PortDirection::Input,
                WordType::bits(1).unwrap(),
                SourceSpan::default(),
            )
            .unwrap();
        let address = module
            .read_signal(
                module.port(address_port).unwrap().signal,
                SourceSpan::default(),
            )
            .unwrap();
        let memory = module
            .add_memory(
                "memory",
                WordType::bits(1).unwrap(),
                NonZeroU32::new(2).unwrap(),
                SourceSpan::default(),
            )
            .unwrap();
        let data = module
            .add_wire(
                "read_data",
                WordType::bits(1).unwrap(),
                SourceSpan::default(),
            )
            .unwrap();
        module
            .add_memory_read_port(word::MemoryReadPort {
                memory,
                address,
                data,
                timing: word::MemoryReadTiming::Asynchronous,
                read_during_write: word::ReadDuringWrite::OldData,
                source: SourceSpan::default(),
            })
            .unwrap();
        let read = module.read_signal(data, SourceSpan::default()).unwrap();
        let packed = module
            .concat(vec![read, read], SourceSpan::default())
            .unwrap();

        let full_domain = FullDomainRootSemantics::new(&module).unwrap();
        assert!(full_domain.requires_artifact(read).unwrap());
        assert!(full_domain.requires_artifact(packed).unwrap());
    }

    #[test]
    fn involution_without_a_two_state_proof_is_not_a_global_alias() {
        let mut module = WordModule::new("involution");
        let input = module
            .add_port(
                "input",
                PortDirection::Input,
                WordType::bits(1).unwrap(),
                SourceSpan::default(),
            )
            .unwrap();
        let input = module
            .read_signal(module.port(input).unwrap().signal, SourceSpan::default())
            .unwrap();
        let first = module
            .unary(word::UnaryOp::BitNot, input, SourceSpan::default())
            .unwrap();
        let boundary = module
            .add_wire(
                "boundary",
                WordType::bits(1).unwrap(),
                SourceSpan::default(),
            )
            .unwrap();
        module
            .connect(LValue::signal(boundary), first, SourceSpan::default())
            .unwrap();
        let boundary = module.read_signal(boundary, SourceSpan::default()).unwrap();
        let second = module
            .unary(word::UnaryOp::BitNot, boundary, SourceSpan::default())
            .unwrap();

        let full_domain = FullDomainRootSemantics::new(&module).unwrap();
        assert_eq!(full_domain.canonical_root(second).unwrap(), second);
        assert!(full_domain.requires_artifact(second).unwrap());
    }
}
