// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::TempEdge;
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
    operation_owner: &'a [Option<usize>],
    memory_signal_owner: &'a BTreeMap<word::SignalId, usize>,
    drivers: &'a SignalDriverIndex,
}

impl<'a> ConnectivityIndex<'a> {
    pub(super) fn new(
        module: &'a word::WordModule,
        value_keys: &'a [[u8; 32]],
        operation_owner: &'a [Option<usize>],
        memory_signal_owner: &'a BTreeMap<word::SignalId, usize>,
        drivers: &'a SignalDriverIndex,
    ) -> Self {
        Self {
            module,
            value_keys,
            operation_owner,
            memory_signal_owner,
            drivers,
        }
    }

    pub(super) fn append_input_edge(
        &self,
        value: word::ValueId,
        sink: usize,
        edges: &mut BTreeSet<TempEdge>,
    ) -> Result<(), crate::SynthError> {
        let stored = self.module.value(value).ok_or_else(|| {
            crate::SynthError::invariant("region input references an unknown value")
        })?;
        let source = self.value_region(value);
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

    pub(super) fn value_region(&self, value: word::ValueId) -> Option<usize> {
        let mut owner = None;
        let mut pending = vec![value];
        let mut visited = BTreeSet::new();
        while let Some(value) = pending.pop() {
            if !visited.insert(value) {
                continue;
            }
            match &self.module.value(value)?.kind {
                word::ValueKind::Operation(operation) => {
                    let candidate = self.operation_owner[operation.index()]?;
                    if owner
                        .replace(candidate)
                        .is_some_and(|current| current != candidate)
                    {
                        return None;
                    }
                }
                word::ValueKind::Signal(reference) => {
                    if let Some(candidate) =
                        self.memory_signal_owner.get(&reference.signal).copied()
                    {
                        if owner
                            .replace(candidate)
                            .is_some_and(|current| current != candidate)
                        {
                            return None;
                        }
                    } else {
                        pending.extend(self.drivers.reference_drivers(*reference)?);
                    }
                }
                word::ValueKind::Constant(_) => {}
            }
        }
        owner
    }
}
