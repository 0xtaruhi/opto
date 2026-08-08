// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Deterministic source fingerprints and incremental-change accounting.
//!
//! Fingerprints describe semantic IR content, not allocation addresses or hash
//! iteration order. A [`SourceSnapshot`] is deliberately smaller than the IR:
//! it retains just enough semantic identity to reject incompatible checkpoints
//! and report which portions changed on the next synthesis.

use opto_core::resident;

mod fingerprint_types;
mod regional_cache;
mod snapshot;

pub use fingerprint_types::*;
use opto_ir::{proc, rtl::RtlModule, word};
pub(crate) use regional_cache::RegionalCacheRecord;
use serde::{Deserialize, Serialize};
pub use snapshot::*;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicUsize, Ordering};

mod fingerprint;

use fingerprint::Fingerprint;

const SOURCE_FINGERPRINT_DOMAIN: &[u8] = b"opto/source-semantic/v6\0";
const HIERARCHY_FINGERPRINT_DOMAIN: &[u8] = b"opto/hierarchy-semantic/v5\0";
const INTERFACE_FINGERPRINT_DOMAIN: &[u8] = b"opto/interface/v1\0";

/// Compact structural fingerprints used to report source changes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceSnapshot {
    effort: crate::SynthesisEffort,
    semantic_fingerprint: SourceFingerprint,
    value_fingerprints: Box<[u64]>,
    operation_fingerprints: Box<[u64]>,
    boundary_fingerprints: Box<[u64]>,
    region_fingerprints: Box<[u64]>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct IncrementalReuseMetrics {
    pub(crate) boolean_recipe_hits: usize,
    pub(crate) boolean_recipe_misses: usize,
    pub(crate) regional_decision_hits: usize,
    pub(crate) regional_decision_misses: usize,
}

impl SourceSnapshot {
    pub(crate) fn capture(rtl: &RtlModule, effort: crate::SynthesisEffort) -> Self {
        let captured = capture_source(rtl);
        Self {
            effort,
            semantic_fingerprint: captured.semantic_fingerprint,
            value_fingerprints: captured.value_fingerprints,
            operation_fingerprints: captured.operation_fingerprints,
            boundary_fingerprints: captured.boundary_fingerprints,
            region_fingerprints: captured.region_fingerprints,
        }
    }

    pub(crate) fn changes_from(&self, previous: Option<&Self>) -> SourceChangeMetrics {
        let Some(previous) = previous.filter(|previous| previous.effort == self.effort) else {
            return SourceChangeMetrics {
                values: self.value_fingerprints.len(),
                changed_values: self.value_fingerprints.len(),
                operations: self.operation_fingerprints.len(),
                changed_operations: self.operation_fingerprints.len(),
                boundaries: self.boundary_fingerprints.len(),
                changed_boundaries: self.boundary_fingerprints.len(),
                regions: self.region_fingerprints.len(),
                rebuilt_regions: self.region_fingerprints.len(),
                ..SourceChangeMetrics::default()
            };
        };
        let (changed_values, removed_values) =
            changed_multiset(&self.value_fingerprints, &previous.value_fingerprints);
        let (changed_operations, removed_operations) = changed_multiset(
            &self.operation_fingerprints,
            &previous.operation_fingerprints,
        );
        let (changed_boundaries, removed_boundaries) =
            changed_multiset(&self.boundary_fingerprints, &previous.boundary_fingerprints);
        let reused_regions =
            matched_entries(&self.region_fingerprints, &previous.region_fingerprints);
        SourceChangeMetrics {
            values: self.value_fingerprints.len(),
            changed_values,
            removed_values,
            operations: self.operation_fingerprints.len(),
            changed_operations,
            removed_operations,
            boundaries: self.boundary_fingerprints.len(),
            changed_boundaries,
            removed_boundaries,
            regions: self.region_fingerprints.len(),
            rebuilt_regions: self
                .region_fingerprints
                .len()
                .saturating_sub(reused_regions),
            reused_regions,
        }
    }

    #[must_use]
    /// Return the digest of the complete semantic source.
    pub fn semantic_fingerprint(&self) -> SourceFingerprint {
        self.semantic_fingerprint
    }

    #[must_use]
    /// Return the synthesis effort under which this snapshot was captured.
    ///
    /// Snapshots from different efforts are not compared incrementally because
    /// their enabled pass sets differ.
    pub const fn effort(&self) -> crate::SynthesisEffort {
        self.effort
    }

    pub(crate) fn owned_memory_bytes(&self) -> usize {
        // Snapshot arenas are boxed slices at construction and decode time, so
        // their logical lengths are already the compact payload definition.
        resident::slice_bytes::<u64>(self.value_fingerprints.len())
            .saturating_add(resident::slice_bytes::<u64>(
                self.operation_fingerprints.len(),
            ))
            .saturating_add(resident::slice_bytes::<u64>(
                self.boundary_fingerprints.len(),
            ))
            .saturating_add(resident::slice_bytes::<u64>(self.region_fingerprints.len()))
    }

    /// Validate invariants required before using a restored snapshot.
    ///
    /// # Errors
    ///
    /// Returns an invariant error when the snapshot lacks its mandatory module
    /// boundary fingerprint.
    pub fn validate_checkpoint(&self) -> Result<(), crate::SynthError> {
        if self.boundary_fingerprints.is_empty() {
            return Err(crate::SynthError::invariant(
                "source snapshot is missing its module boundary fingerprint",
            ));
        }
        if !self.region_fingerprints.is_sorted() {
            return Err(crate::SynthError::invariant(
                "source snapshot region fingerprints are not canonical",
            ));
        }
        if !self.value_fingerprints.is_sorted()
            || !self.operation_fingerprints.is_sorted()
            || !self.boundary_fingerprints.is_sorted()
        {
            return Err(crate::SynthError::invariant(
                "source snapshot semantic fingerprint multisets are not canonical",
            ));
        }
        Ok(())
    }
}

fn canonical_len(length: usize) -> u64 {
    u64::try_from(length).expect("semantic fingerprint input length exceeds 64-bit capacity")
}

fn matched_entries(current: &[u64], previous: &[u64]) -> usize {
    debug_assert!(current.is_sorted());
    debug_assert!(previous.is_sorted());
    let (mut left, mut right, mut matched) = (0usize, 0usize, 0usize);
    while left < current.len() && right < previous.len() {
        match current[left].cmp(&previous[right]) {
            std::cmp::Ordering::Less => left += 1,
            std::cmp::Ordering::Greater => right += 1,
            std::cmp::Ordering::Equal => {
                matched += 1;
                left += 1;
                right += 1;
            }
        }
    }
    matched
}

struct CapturedSource {
    semantic_fingerprint: SourceFingerprint,
    value_fingerprints: Box<[u64]>,
    operation_fingerprints: Box<[u64]>,
    boundary_fingerprints: Box<[u64]>,
    region_fingerprints: Box<[u64]>,
}

fn capture_source(rtl: &RtlModule) -> CapturedSource {
    let module = rtl.word();
    let mut semantic_values = Vec::with_capacity(module.values().len());
    for value in module.values() {
        semantic_values.push(semantic_value_fingerprint(module, value, &semantic_values).finish());
    }
    let mut value_fingerprints = semantic_values.clone();

    let mut operation_fingerprints = Vec::with_capacity(module.operations().len());
    let mut region_fingerprints = Vec::with_capacity(module.operations().len());
    for operation in module.operations() {
        operation_fingerprints.push(
            semantic_values
                .get(operation.result.index())
                .copied()
                .unwrap_or_else(|| {
                    let mut fingerprint = Fingerprint::new();
                    fingerprint.tag(255);
                    fingerprint.id(operation.result.raw());
                    fingerprint.finish()
                }),
        );
        let mut fingerprint = Fingerprint::new();
        fingerprint.tag(254);
        module
            .value(operation.result)
            .map(|value| value.ty)
            .hash(&mut fingerprint);
        hash_operation(module, &semantic_values, &operation.kind, &mut fingerprint);
        region_fingerprints.push(fingerprint.finish());
    }

    let mut boundary_fingerprints = Vec::with_capacity(boundary_count(rtl));
    visit_boundary_fingerprints(rtl, &semantic_values, |fingerprint| {
        boundary_fingerprints.push(fingerprint.finish());
    });

    value_fingerprints.sort_unstable();
    operation_fingerprints.sort_unstable();
    boundary_fingerprints.sort_unstable();
    region_fingerprints.sort_unstable();

    let mut digest = blake3::Hasher::new();
    digest.update(SOURCE_FINGERPRINT_DOMAIN);
    digest.update(&canonical_len(value_fingerprints.len()).to_le_bytes());
    for fingerprint in &value_fingerprints {
        digest.update(&fingerprint.to_le_bytes());
    }
    digest.update(&canonical_len(region_fingerprints.len()).to_le_bytes());
    for fingerprint in &region_fingerprints {
        digest.update(&fingerprint.to_le_bytes());
    }
    digest.update(&canonical_len(boundary_fingerprints.len()).to_le_bytes());
    for fingerprint in &boundary_fingerprints {
        digest.update(&fingerprint.to_le_bytes());
    }
    CapturedSource {
        semantic_fingerprint: SourceFingerprint(*digest.finalize().as_bytes()),
        value_fingerprints: value_fingerprints.into_boxed_slice(),
        operation_fingerprints: operation_fingerprints.into_boxed_slice(),
        boundary_fingerprints: boundary_fingerprints.into_boxed_slice(),
        region_fingerprints: region_fingerprints.into_boxed_slice(),
    }
}

fn semantic_value_fingerprint(
    module: &word::WordModule,
    value: &word::Value,
    values: &[u64],
) -> Fingerprint {
    let mut fingerprint = Fingerprint::new();
    value.ty.hash(&mut fingerprint);
    match &value.kind {
        word::ValueKind::Signal(reference) => {
            fingerprint.tag(0);
            if let Some(signal) = module.signal(reference.signal) {
                signal
                    .name
                    .map(|name| module.name_str(name))
                    .hash(&mut fingerprint);
                hash_signal_kind(signal.kind, &mut fingerprint);
                signal.ty.hash(&mut fingerprint);
                fingerprint.tag(signal.resolution as u8);
            }
            reference.lsb.hash(&mut fingerprint);
            reference.width().hash(&mut fingerprint);
        }
        word::ValueKind::Constant(bits) => {
            fingerprint.tag(1);
            bits.hash(&mut fingerprint);
        }
        word::ValueKind::Operation(operation) => {
            fingerprint.tag(2);
            if let Some(operation) = module.operation(*operation) {
                hash_operation(module, values, &operation.kind, &mut fingerprint);
            }
        }
    }
    fingerprint
}

fn changed_multiset(current: &[u64], previous: &[u64]) -> (usize, usize) {
    let matched = matched_entries(current, previous);
    (
        current.len().saturating_sub(matched),
        previous.len().saturating_sub(matched),
    )
}

fn hash_operation(
    module: &word::WordModule,
    values: &[u64],
    operation: &word::OpKind,
    fingerprint: &mut Fingerprint,
) {
    match operation {
        word::OpKind::Unary { op, arg } => {
            fingerprint.tag(0);
            fingerprint.tag(*op as u8);
            hash_value(*arg, values, fingerprint);
        }
        word::OpKind::Binary { op, left, right } => {
            fingerprint.tag(1);
            fingerprint.tag(*op as u8);
            hash_value(*left, values, fingerprint);
            hash_value(*right, values, fingerprint);
        }
        word::OpKind::Mux {
            cond,
            then_value,
            else_value,
        } => {
            fingerprint.tag(2);
            hash_value(*cond, values, fingerprint);
            hash_value(*then_value, values, fingerprint);
            hash_value(*else_value, values, fingerprint);
        }
        word::OpKind::Concat { parts } => {
            fingerprint.tag(3);
            parts.len().hash(fingerprint);
            for part in parts {
                hash_value(*part, values, fingerprint);
            }
        }
        word::OpKind::Extract { value, lsb, width } => {
            fingerprint.tag(4);
            hash_value(*value, values, fingerprint);
            lsb.hash(fingerprint);
            width.hash(fingerprint);
        }
        word::OpKind::DynamicExtract {
            value,
            offset,
            width,
        } => {
            fingerprint.tag(5);
            hash_value(*value, values, fingerprint);
            hash_value(*offset, values, fingerprint);
            width.hash(fingerprint);
        }
        word::OpKind::DynamicInsert {
            value,
            offset,
            replacement,
        } => {
            fingerprint.tag(6);
            hash_value(*value, values, fingerprint);
            hash_value(*offset, values, fingerprint);
            hash_value(*replacement, values, fingerprint);
        }
        word::OpKind::Cast {
            kind,
            value,
            target,
        } => {
            fingerprint.tag(7);
            fingerprint.tag(*kind as u8);
            hash_value(*value, values, fingerprint);
            target.hash(fingerprint);
        }
        word::OpKind::Register(register) => {
            fingerprint.tag(8);
            register
                .name
                .map(|name| module.name_str(name))
                .hash(fingerprint);
            hash_value(register.d, values, fingerprint);
            hash_value(register.clock, values, fingerprint);
            fingerprint.tag(register.edge as u8);
            register.enable.is_some().hash(fingerprint);
            if let Some(enable) = register.enable {
                hash_value(enable.value, values, fingerprint);
                enable.active_high.hash(fingerprint);
            }
            register.resets.len().hash(fingerprint);
            for reset in &register.resets {
                fingerprint.tag(reset.kind as u8);
                hash_value(reset.value, values, fingerprint);
                reset.active_high.hash(fingerprint);
                hash_value(reset.reset_value, values, fingerprint);
            }
        }
        word::OpKind::Latch(latch) => {
            fingerprint.tag(9);
            latch
                .name
                .map(|name| module.name_str(name))
                .hash(fingerprint);
            hash_value(latch.d, values, fingerprint);
            hash_value(latch.enable.value, values, fingerprint);
            latch.enable.active_high.hash(fingerprint);
            latch.resets.len().hash(fingerprint);
            for reset in &latch.resets {
                fingerprint.tag(reset.kind as u8);
                hash_value(reset.value, values, fingerprint);
                reset.active_high.hash(fingerprint);
                hash_value(reset.reset_value, values, fingerprint);
            }
        }
    }
}

fn boundary_count(rtl: &RtlModule) -> usize {
    let module = rtl.word();
    1usize
        .saturating_add(module.ports().len())
        .saturating_add(module.signals().len())
        .saturating_add(module.connects().len())
        .saturating_add(module.instances().len())
        .saturating_add(module.memories().len())
        .saturating_add(module.memory_read_ports().len())
        .saturating_add(module.memory_write_ports().len())
        .saturating_add(rtl.procedures().procedures().len())
        .saturating_add(rtl.procedures().blocks().len())
}

fn visit_boundary_fingerprints(rtl: &RtlModule, values: &[u64], mut emit: impl FnMut(Fingerprint)) {
    let module = rtl.word();
    let procedures = rtl.procedures();
    emit(entry_fingerprint(|fingerprint| {
        fingerprint.tag(255);
        module.name().hash(fingerprint);
        fingerprint.tag(module.definition_kind() as u8);
        module.annotations().len().hash(fingerprint);
        for annotation in module.annotations() {
            hash_annotation(module, annotation, fingerprint);
        }
        module.synthesis_directives().len().hash(fingerprint);
        for directive in module.synthesis_directives() {
            hash_synthesis_directive(directive, fingerprint);
        }
    }));
    for port in module.ports() {
        emit(entry_fingerprint(|fingerprint| {
            fingerprint.tag(0);
            module.name_str(port.name).hash(fingerprint);
            fingerprint.tag(port.direction as u8);
            fingerprint.id(port.signal.raw());
            port.ty.hash(fingerprint);
        }));
    }
    for (index, signal) in module.signals().iter().enumerate() {
        emit(entry_fingerprint(|fingerprint| {
            fingerprint.tag(1);
            signal
                .name
                .map(|name| module.name_str(name))
                .hash(fingerprint);
            hash_signal_kind(signal.kind, fingerprint);
            signal.ty.hash(fingerprint);
            fingerprint.tag(signal.resolution as u8);
            let signal = word::SignalId::from_index(index)
                .expect("sealed RTL arena contains valid signal IDs");
            hash_signal_type_layout(module, signal, fingerprint);
        }));
    }
    for connect in module.connects() {
        emit(entry_fingerprint(|fingerprint| {
            fingerprint.tag(2);
            hash_lvalue(&connect.target, values, fingerprint);
            hash_value(connect.value, values, fingerprint);
        }));
    }
    for instance in module.instances() {
        emit(entry_fingerprint(|fingerprint| {
            fingerprint.tag(3);
            module.name_str(instance.name).hash(fingerprint);
            module.name_str(instance.module).hash(fingerprint);
            instance.connections.len().hash(fingerprint);
            for connection in &instance.connections {
                module.name_str(connection.port).hash(fingerprint);
                hash_value(connection.value, values, fingerprint);
            }
        }));
    }
    for memory in module.memories() {
        emit(entry_fingerprint(|fingerprint| {
            fingerprint.tag(4);
            module.name_str(memory.name).hash(fingerprint);
            memory.element_type.hash(fingerprint);
            memory.depth.get().hash(fingerprint);
        }));
    }
    for read in module.memory_read_ports() {
        emit(entry_fingerprint(|fingerprint| {
            fingerprint.tag(5);
            fingerprint.id(read.memory.raw());
            hash_value(read.address, values, fingerprint);
            fingerprint.id(read.data.raw());
            hash_memory_read_timing(read.timing, values, fingerprint);
            fingerprint.tag(read.read_during_write as u8);
        }));
    }
    for write in module.memory_write_ports() {
        emit(entry_fingerprint(|fingerprint| {
            fingerprint.tag(6);
            fingerprint.id(write.memory.raw());
            hash_value(write.address, values, fingerprint);
            hash_value(write.data, values, fingerprint);
            hash_memory_clock(write.clock, values, fingerprint);
            hash_enable(write.enable, values, fingerprint);
            match write.mask {
                Some(mask) => {
                    fingerprint.tag(1);
                    hash_value(mask.value, values, fingerprint);
                    mask.granularity.get().hash(fingerprint);
                    mask.active_high.hash(fingerprint);
                }
                None => fingerprint.tag(0),
            }
            write.priority.hash(fingerprint);
        }));
    }
    for (index, procedure) in procedures.procedures().iter().enumerate() {
        let id = proc::ProcedureId::from_index(index)
            .expect("sealed procedural arena contains valid procedure IDs");
        emit(entry_fingerprint(|fingerprint| {
            fingerprint.tag(7);
            fingerprint.tag(procedure.kind as u8);
            fingerprint.id(procedure.entry.raw());
            procedure.block_count().hash(fingerprint);
            match procedure.sensitivity {
                proc::Sensitivity::Implicit => fingerprint.tag(0),
                proc::Sensitivity::Edges(_) => {
                    fingerprint.tag(1);
                    let events = procedures
                        .sensitivity_events(id)
                        .expect("sealed edge-sensitive procedure owns its events");
                    events.len().hash(fingerprint);
                    for (_, event) in events {
                        fingerprint.id(event.signal.raw());
                        fingerprint.tag(event.edge as u8);
                    }
                }
            }
        }));
    }
    for (index, block) in procedures.blocks().iter().enumerate() {
        let id = proc::BlockId::from_index(index)
            .expect("sealed procedural arena contains valid block IDs");
        emit(entry_fingerprint(|fingerprint| {
            fingerprint.tag(8);
            fingerprint.id(block.procedure.raw());
            block.effect_count().hash(fingerprint);
            for (_, effect) in procedures
                .block_effects(id)
                .expect("sealed block owns its effects")
            {
                hash_effect(effect, values, fingerprint);
            }
            hash_terminator(procedures, id, &block.terminator.kind, values, fingerprint);
        }));
    }
}

fn hash_annotation(
    module: &word::WordModule,
    annotation: &word::Annotation,
    fingerprint: &mut Fingerprint,
) {
    hash_annotation_target(annotation.target, fingerprint);
    module.name_str(annotation.name).hash(fingerprint);
    match &annotation.value {
        word::AnnotationValue::Integer {
            bits,
            width,
            signed,
        } => {
            fingerprint.tag(0);
            signed.hash(fingerprint);
            width.hash(fingerprint);
            module.name_str(*bits).hash(fingerprint);
        }
        word::AnnotationValue::String(value) => {
            fingerprint.tag(1);
            module.name_str(*value).hash(fingerprint);
        }
        word::AnnotationValue::Other(value) => {
            fingerprint.tag(2);
            module.name_str(*value).hash(fingerprint);
        }
    }
}

fn hash_synthesis_directive(directive: &word::SynthesisDirective, fingerprint: &mut Fingerprint) {
    hash_annotation_target(directive.target, fingerprint);
    fingerprint.tag(directive.kind as u8);
    directive.enabled.hash(fingerprint);
}

fn hash_annotation_target(target: word::AnnotationTarget, fingerprint: &mut Fingerprint) {
    match target {
        word::AnnotationTarget::Module => fingerprint.tag(0),
        word::AnnotationTarget::Port(id) => {
            fingerprint.tag(1);
            fingerprint.id(id.raw());
        }
        word::AnnotationTarget::Signal(id) => {
            fingerprint.tag(2);
            fingerprint.id(id.raw());
        }
        word::AnnotationTarget::Memory(id) => {
            fingerprint.tag(3);
            fingerprint.id(id.raw());
        }
        word::AnnotationTarget::MemoryReadPort(id) => {
            fingerprint.tag(4);
            fingerprint.id(id.raw());
        }
        word::AnnotationTarget::MemoryWritePort(id) => {
            fingerprint.tag(5);
            fingerprint.id(id.raw());
        }
        word::AnnotationTarget::Value(id) => {
            fingerprint.tag(6);
            fingerprint.id(id.raw());
        }
        word::AnnotationTarget::Operation(id) => {
            fingerprint.tag(7);
            fingerprint.id(id.raw());
        }
        word::AnnotationTarget::Instance(id) => {
            fingerprint.tag(8);
            fingerprint.id(id.raw());
        }
    }
}

fn hash_signal_type_layout(
    module: &word::WordModule,
    signal: word::SignalId,
    fingerprint: &mut Fingerprint,
) {
    let traversal = module.visit_signal_type_layout(signal, |event| match event {
        word::TypeLayoutEvent::Scalar => fingerprint.tag(0),
        word::TypeLayoutEvent::Array { kind, range } => {
            fingerprint.tag(1);
            fingerprint.tag(kind as u8);
            range.left.hash(fingerprint);
            range.right.hash(fingerprint);
        }
        word::TypeLayoutEvent::Struct { field_count } => {
            fingerprint.tag(2);
            field_count.hash(fingerprint);
        }
        word::TypeLayoutEvent::Field { name, bit_offset } => {
            fingerprint.tag(3);
            name.hash(fingerprint);
            bit_offset.hash(fingerprint);
        }
    });
    fingerprint.tag(traversal as u8);
}

fn hash_signal_kind(kind: word::SignalKind, fingerprint: &mut Fingerprint) {
    match kind {
        word::SignalKind::Wire => fingerprint.tag(0),
        word::SignalKind::Register => fingerprint.tag(1),
        word::SignalKind::ProcessLocal => fingerprint.tag(2),
        word::SignalKind::Port(port) => {
            fingerprint.tag(3);
            fingerprint.id(port.raw());
        }
    }
}

fn hash_lvalue(lvalue: &word::LValue, values: &[u64], fingerprint: &mut Fingerprint) {
    fingerprint.id(lvalue.signal.raw());
    lvalue.range.is_some().hash(fingerprint);
    if let Some(range) = lvalue.range {
        range.msb.hash(fingerprint);
        range.lsb.hash(fingerprint);
    }
    lvalue.dynamic.is_some().hash(fingerprint);
    if let Some(dynamic) = lvalue.dynamic {
        hash_value(dynamic.offset, values, fingerprint);
        dynamic.width.hash(fingerprint);
    }
}

fn hash_memory_clock(clock: word::MemoryClock, values: &[u64], fingerprint: &mut Fingerprint) {
    hash_value(clock.value, values, fingerprint);
    fingerprint.tag(clock.edge as u8);
}

fn hash_enable(enable: Option<word::Enable>, values: &[u64], fingerprint: &mut Fingerprint) {
    match enable {
        Some(enable) => {
            fingerprint.tag(1);
            hash_value(enable.value, values, fingerprint);
            enable.active_high.hash(fingerprint);
        }
        None => fingerprint.tag(0),
    }
}

fn hash_memory_read_timing(
    timing: word::MemoryReadTiming,
    values: &[u64],
    fingerprint: &mut Fingerprint,
) {
    match timing {
        word::MemoryReadTiming::Asynchronous => fingerprint.tag(0),
        word::MemoryReadTiming::Synchronous {
            clock,
            enable,
            disabled,
        } => {
            fingerprint.tag(1);
            hash_memory_clock(clock, values, fingerprint);
            hash_enable(enable, values, fingerprint);
            fingerprint.tag(disabled as u8);
        }
    }
}

fn hash_effect(effect: &proc::Effect, values: &[u64], fingerprint: &mut Fingerprint) {
    fingerprint.tag(effect.mode as u8);
    match effect.target {
        proc::ProcTarget::Signal { signal, select } => {
            fingerprint.tag(0);
            fingerprint.id(signal.raw());
            hash_target_select(select, values, fingerprint);
        }
        proc::ProcTarget::Memory {
            memory,
            address,
            select,
        } => {
            fingerprint.tag(1);
            fingerprint.id(memory.raw());
            hash_value(address, values, fingerprint);
            hash_target_select(select, values, fingerprint);
        }
    }
    hash_value(effect.value, values, fingerprint);
}

fn hash_target_select(select: proc::TargetSelect, values: &[u64], fingerprint: &mut Fingerprint) {
    match select {
        proc::TargetSelect::Whole => fingerprint.tag(0),
        proc::TargetSelect::Static(range) => {
            fingerprint.tag(1);
            range.msb.hash(fingerprint);
            range.lsb.hash(fingerprint);
        }
        proc::TargetSelect::Dynamic { offset, width } => {
            fingerprint.tag(2);
            hash_value(offset, values, fingerprint);
            width.get().hash(fingerprint);
        }
    }
}

fn hash_terminator(
    procedures: &proc::ProcModule,
    block: proc::BlockId,
    terminator: &proc::TerminatorKind,
    values: &[u64],
    fingerprint: &mut Fingerprint,
) {
    match terminator {
        proc::TerminatorKind::Return => fingerprint.tag(0),
        proc::TerminatorKind::Jump { edge } => {
            fingerprint.tag(1);
            hash_proc_edge(procedures, *edge, fingerprint);
        }
        proc::TerminatorKind::Branch {
            condition,
            then_edge,
            else_edge,
        } => {
            fingerprint.tag(2);
            hash_value(*condition, values, fingerprint);
            hash_proc_edge(procedures, *then_edge, fingerprint);
            hash_proc_edge(procedures, *else_edge, fingerprint);
        }
        proc::TerminatorKind::Switch {
            selector, default, ..
        } => {
            fingerprint.tag(3);
            hash_value(*selector, values, fingerprint);
            let arms = procedures
                .switch_arms(block)
                .expect("sealed switch terminator owns its arms");
            arms.len().hash(fingerprint);
            for (_, arm) in arms {
                hash_value(arm.pattern, values, fingerprint);
                hash_proc_edge(procedures, arm.edge, fingerprint);
            }
            hash_proc_edge(procedures, *default, fingerprint);
        }
    }
}

fn hash_proc_edge(
    procedures: &proc::ProcModule,
    edge: proc::EdgeId,
    fingerprint: &mut Fingerprint,
) {
    fingerprint.id(edge.raw());
    let edge = procedures
        .edge(edge)
        .expect("sealed terminator references a valid edge");
    fingerprint.id(edge.from.raw());
    fingerprint.id(edge.target.raw());
}

fn hash_value(value: word::ValueId, values: &[u64], fingerprint: &mut Fingerprint) {
    fingerprint.id(value.raw());
    values.get(value.index()).copied().hash(fingerprint);
}

fn entry_fingerprint(action: impl FnOnce(&mut Fingerprint)) -> Fingerprint {
    let mut fingerprint = Fingerprint::new();
    action(&mut fingerprint);
    fingerprint
}

#[cfg(test)]
mod tests;
