// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::{TempBitFlow, TempEdge};
use crate::word::bit_connectivity::{BitConnectivity, BitSource};
use crate::word::signal_driver::SignalDriverIndex;
use opto_ir::word;
use std::collections::{BTreeMap, BTreeSet};

pub(super) struct InputOperations<'a> {
    module: &'a word::WordModule,
    drivers: &'a SignalDriverIndex,
    visited: Vec<u32>,
    epoch: u32,
    pending: Vec<word::ValueId>,
    operations: Vec<usize>,
}

impl<'a> InputOperations<'a> {
    pub(super) fn new(module: &'a word::WordModule, drivers: &'a SignalDriverIndex) -> Self {
        Self {
            module,
            drivers,
            visited: vec![0; module.values().len()],
            epoch: 0,
            pending: Vec::new(),
            operations: Vec::new(),
        }
    }

    /// Finds the combinational producer operations behind a Word value.
    ///
    /// Signal reads may cross any number of connect aliases before reaching
    /// their producer. Partition planning and final boundary construction must
    /// therefore use the same transitive dependency relation; stopping after
    /// one signal hop can make an acyclic unit plan become cyclic after regions
    /// are sealed.
    pub(super) fn resolve(&mut self, value: word::ValueId) -> &[usize] {
        self.begin_query();
        self.pending.push(value);
        while let Some(value) = self.pending.pop() {
            let Some(mark) = self.visited.get_mut(value.index()) else {
                continue;
            };
            if *mark == self.epoch {
                continue;
            }
            *mark = self.epoch;
            let Some(stored) = self.module.value(value) else {
                continue;
            };
            match stored.kind {
                word::ValueKind::Operation(operation) => {
                    self.operations.push(operation.index());
                }
                word::ValueKind::Signal(reference) => {
                    if let Some(drivers) = self.drivers.reference_drivers(reference) {
                        self.pending.extend(drivers);
                    } else {
                        self.pending.extend(self.drivers.values(reference.signal));
                    }
                }
                word::ValueKind::Constant(_) => {}
            }
        }
        self.operations.sort_unstable();
        self.operations.dedup();
        &self.operations
    }

    fn begin_query(&mut self) {
        self.pending.clear();
        self.operations.clear();
        self.epoch = self.epoch.wrapping_add(1);
        if self.epoch == 0 {
            self.visited.fill(0);
            self.epoch = 1;
        }
    }
}

pub(super) struct ConnectivityIndex<'a> {
    module: &'a word::WordModule,
    value_keys: &'a [[u8; 32]],
    operation_region: &'a [Option<usize>],
    memory_signal_region: &'a BTreeMap<word::SignalId, usize>,
    instance_boundary_signals: BTreeSet<word::SignalId>,
    bit_connectivity: BitConnectivity<'a>,
}

impl<'a> ConnectivityIndex<'a> {
    pub(super) fn new(
        module: &'a word::WordModule,
        value_keys: &'a [[u8; 32]],
        operation_region: &'a [Option<usize>],
        memory_signal_region: &'a BTreeMap<word::SignalId, usize>,
    ) -> Result<Self, crate::SynthError> {
        let mut instance_boundary_signals = BTreeSet::new();
        for connection in module
            .instances()
            .iter()
            .flat_map(|instance| &instance.connections)
        {
            collect_projection_signal_leaves(
                module,
                connection.value,
                &mut instance_boundary_signals,
            )?;
        }
        Ok(Self {
            module,
            value_keys,
            operation_region,
            memory_signal_region,
            instance_boundary_signals,
            bit_connectivity: BitConnectivity::new(module)?,
        })
    }

    pub(super) fn append_input_edge(
        &self,
        value: word::ValueId,
        sink: usize,
        edges: &mut BTreeSet<TempEdge>,
        bit_flows: &mut BTreeSet<TempBitFlow>,
    ) -> Result<(), crate::SynthError> {
        let stored = self.module.value(value).ok_or_else(|| {
            crate::SynthError::invariant("region input references an unknown value")
        })?;
        self.append_bit_flows(value, Some(sink), bit_flows)?;
        let source = self.value_region(value)?;
        if source == Some(sink) || matches!(stored.kind, word::ValueKind::Constant(_)) {
            return Ok(());
        }
        edges.insert(TempEdge {
            source,
            sink: Some(sink),
            value,
            endpoint: super::temp_endpoint(self.module, value)?,
            ty: stored.ty,
            semantic_key: self.value_keys[value.index()],
            value_revision: self.value_keys[value.index()],
        });
        Ok(())
    }

    pub(super) fn append_bit_flows(
        &self,
        value: word::ValueId,
        sink: Option<usize>,
        bit_flows: &mut BTreeSet<TempBitFlow>,
    ) -> Result<(), crate::SynthError> {
        let width = self
            .module
            .value(value)
            .ok_or_else(|| crate::SynthError::invariant("bit flow references an unknown value"))?
            .ty
            .width();
        for bit in 0..width {
            let BitSource::Value {
                value: source,
                bit: source_bit,
            } = self.bit_connectivity.source(value, bit)?
            else {
                continue;
            };
            let Some(producer) = self.endpoint_region(source)? else {
                continue;
            };
            if Some(producer) != sink {
                bit_flows.insert(TempBitFlow {
                    source: producer,
                    sink,
                    value: source,
                    bit: source_bit,
                });
            }
        }
        Ok(())
    }

    pub(super) fn value_region(
        &self,
        value: word::ValueId,
    ) -> Result<Option<usize>, crate::SynthError> {
        let width = self
            .module
            .value(value)
            .ok_or_else(|| crate::SynthError::invariant("region value is unknown"))?
            .ty
            .width();
        let mut region = None;
        for bit in 0..width {
            let BitSource::Value { value, .. } = self.bit_connectivity.source(value, bit)? else {
                continue;
            };
            let Some(candidate) = self.endpoint_region(value)? else {
                continue;
            };
            if region
                .replace(candidate)
                .is_some_and(|current| current != candidate)
            {
                return Ok(None);
            }
        }
        Ok(region)
    }

    fn endpoint_region(&self, value: word::ValueId) -> Result<Option<usize>, crate::SynthError> {
        let stored = self.module.value(value).ok_or_else(|| {
            crate::SynthError::invariant("bit producer references an unknown value")
        })?;
        match stored.kind {
            word::ValueKind::Operation(operation) => {
                if let Some(region) = self
                    .operation_region
                    .get(operation.index())
                    .copied()
                    .flatten()
                {
                    return Ok(Some(region));
                }
                if self.module.operation(operation).is_some_and(|operation| {
                    matches!(operation.kind, word::OpKind::TriState { .. })
                }) {
                    // The resolved-net shell contains the physical driver cell;
                    // only its data and enable cones receive region placement.
                    Ok(None)
                } else {
                    Err(crate::SynthError::invariant(format!(
                        "live bit producer operation {operation:?} has no region placement"
                    )))
                }
            }
            word::ValueKind::Signal(reference) => {
                if let Some(region) = self.memory_signal_region.get(&reference.signal).copied() {
                    return Ok(Some(region));
                }
                let signal = self.module.signal(reference.signal).ok_or_else(|| {
                    crate::SynthError::invariant("bit producer signal is unknown")
                })?;
                if signal.resolution == word::SignalResolution::TriState
                    || self.instance_boundary_signals.contains(&reference.signal)
                    || matches!(signal.kind, word::SignalKind::Port(port) if self.module.port(port).is_some_and(|port| matches!(port.direction, word::PortDirection::Input | word::PortDirection::Inout)))
                {
                    Ok(None)
                } else {
                    let name = signal
                        .name
                        .and_then(|name| self.module.resolve_name(name))
                        .unwrap_or("<unnamed>");
                    let connects = self
                        .module
                        .connects()
                        .iter()
                        .filter(|connect| connect.target.signal == reference.signal)
                        .count();
                    Err(crate::SynthError::invariant(format!(
                        "live internal signal '{name}' slice {:?}[{}:{}] has no semantic producer ({connects} structural drivers on the signal)",
                        reference.signal,
                        reference.lsb + reference.width() - 1,
                        reference.lsb
                    )))
                }
            }
            word::ValueKind::Constant(_) => Ok(None),
        }
    }
}

fn collect_projection_signal_leaves(
    module: &word::WordModule,
    root: word::ValueId,
    signals: &mut BTreeSet<word::SignalId>,
) -> Result<(), crate::SynthError> {
    let mut pending = vec![root];
    let mut visited = BTreeSet::new();
    while let Some(value) = pending.pop() {
        if !visited.insert(value) {
            continue;
        }
        let stored = module.value(value).ok_or_else(|| {
            crate::SynthError::invariant("instance connection references an unknown Word value")
        })?;
        match &stored.kind {
            word::ValueKind::Signal(reference) => {
                signals.insert(reference.signal);
            }
            word::ValueKind::Operation(operation) => {
                let operation = module.operation(*operation).ok_or_else(|| {
                    crate::SynthError::invariant(
                        "instance connection projection references an unknown operation",
                    )
                })?;
                match &operation.kind {
                    word::OpKind::Extract { value, .. } | word::OpKind::Cast { value, .. } => {
                        pending.push(*value);
                    }
                    word::OpKind::Concat { parts } => pending.extend(parts.iter().copied()),
                    _ => {}
                }
            }
            word::ValueKind::Constant(_) => {}
        }
    }
    Ok(())
}
