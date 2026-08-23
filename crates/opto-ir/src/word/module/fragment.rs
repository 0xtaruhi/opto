// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Transactional Word fragments and deterministic multi-fragment publication.
//!
//! A [`WordFragment`] is a complete replacement proposed against an immutable
//! base module generation. It carries appended constants and operations,
//! retargeted base operations, and connect edits. References inside the
//! fragment resolve either against the base generation or against the
//! fragment's own future rows through provisional dense IDs numbered beyond
//! the base arena lengths.
//!
//! [`WordModule::publish_fragments`] implements RFC 0013 Amendment 1 steps one
//! and two. Slot assignment walks the wave in ascending [`FragmentKey`] order
//! and assigns dense arena IDs, so published numbering never depends on task
//! completion order. Splicing applies every fragment inside one undo journal:
//! any failure restores the module byte-exactly, including connect removals
//! and operation retargets that speculation checkpoints cannot undo.

use super::{
    CastKind, Connect, ConstBits, LValue, LogicStateKind, OpId, OpKind, SourceSpan, ValueId,
    ValueKind, WordError, WordModule, WordType,
};
use crate::value::BitVal;

/// Deterministic sort key ordering one fragment inside a publication wave.
///
/// Keys must be stable functions of semantic inputs such as stable anchors or
/// base arena identities. They may never encode worker count, executor
/// assignment, or completion order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FragmentKey([u8; 32]);

impl FragmentKey {
    /// Constructs a key from its canonical digest.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Returns the canonical digest of this key.
    #[must_use]
    pub const fn bytes(self) -> [u8; 32] {
        self.0
    }
}

#[derive(Debug, Clone)]
enum FragmentValueRow {
    Constant {
        bits: ConstBits,
        ty: WordType,
        source: SourceSpan,
    },
    Operation {
        row: usize,
        ty: WordType,
        source: SourceSpan,
    },
}

#[derive(Debug, Clone)]
struct FragmentOperationRow {
    kind: OpKind,
}

#[derive(Debug, Clone)]
struct FragmentConnectRow {
    target: LValue,
    value: ValueId,
    source: SourceSpan,
}

#[derive(Debug, Clone)]
/// Complete transactional replacement proposed against one module generation.
pub struct WordFragment {
    base_values: usize,
    base_operations: usize,
    base_connects: usize,
    values: Vec<FragmentValueRow>,
    operations: Vec<FragmentOperationRow>,
    replaced_operations: Vec<(OpId, OpKind)>,
    removed_connects: Vec<usize>,
    added_connects: Vec<FragmentConnectRow>,
}

/// Collects one [`WordFragment`] against an immutable module generation.
///
/// Builder methods replicate the checked construction rules of the ordinary
/// module builders, so publishing a fragment produces the same typed graph as
/// building those rows directly through [`WordModule`]. Operand references to
/// the fragment's own future rows use dense IDs numbered from the base value
/// arena length.
#[derive(Debug)]
pub struct WordFragmentBuilder<'a> {
    module: &'a WordModule,
    fragment: WordFragment,
}

impl<'a> WordFragmentBuilder<'a> {
    /// Starts a fragment against the caller's immutable module generation.
    ///
    /// The module must not change while the builder lives; publication
    /// validates the recorded base lengths again.
    #[must_use]
    pub fn new(module: &'a WordModule) -> Self {
        Self {
            fragment: WordFragment {
                base_values: module.values.len(),
                base_operations: module.operations.len(),
                base_connects: module.connects.len(),
                values: Vec::new(),
                operations: Vec::new(),
                replaced_operations: Vec::new(),
                removed_connects: Vec::new(),
                added_connects: Vec::new(),
            },
            module,
        }
    }

    /// Resolves the result type of a base value or an already-appended
    /// provisional fragment row.
    ///
    /// # Errors
    ///
    /// Returns [`WordError`] when the reference names neither.
    pub fn value_ty(&self, value: ValueId) -> Result<WordType, WordError> {
        let index = value.index();
        if index < self.fragment.base_values {
            return self
                .module
                .value(value)
                .map(|stored| stored.ty)
                .ok_or_else(|| {
                    WordError::new(format!("fragment references unknown value {value:?}"))
                });
        }
        let row = self
            .fragment
            .values
            .get(index - self.fragment.base_values)
            .ok_or_else(|| {
                WordError::new(format!(
                    "fragment references unappended provisional value {value:?}"
                ))
            })?;
        Ok(match row {
            FragmentValueRow::Constant { ty, .. } | FragmentValueRow::Operation { ty, .. } => *ty,
        })
    }

    fn push_operation_row(
        &mut self,
        kind: OpKind,
        ty: WordType,
        source: SourceSpan,
    ) -> Result<ValueId, WordError> {
        let id = ValueId::from_index(self.fragment.base_values + self.fragment.values.len())?;
        let row = self.fragment.operations.len();
        self.fragment.operations.push(FragmentOperationRow { kind });
        self.fragment
            .values
            .push(FragmentValueRow::Operation { row, ty, source });
        Ok(id)
    }

    /// Appends a typed four-state constant row.
    ///
    /// # Errors
    ///
    /// Returns [`WordError`] when widths differ, a two-state type receives an
    /// unknown or high-impedance bit, or the provisional ID exceeds capacity.
    pub fn constant(
        &mut self,
        bits: ConstBits,
        ty: WordType,
        source: SourceSpan,
    ) -> Result<ValueId, WordError> {
        if bits.width() != ty.width() {
            return Err(WordError::new(format!(
                "constant width {} does not match type width {}",
                bits.width(),
                ty.width()
            )));
        }
        if ty.state() == LogicStateKind::TwoState
            && bits
                .as_slice()
                .iter()
                .any(|bit| matches!(bit, BitVal::X | BitVal::Z))
        {
            return Err(WordError::new(
                "two-state constant cannot contain x or z bits",
            ));
        }
        let id = ValueId::from_index(self.fragment.base_values + self.fragment.values.len())?;
        self.fragment
            .values
            .push(FragmentValueRow::Constant { bits, ty, source });
        Ok(id)
    }

    /// Appends a static contiguous bit extraction row.
    ///
    /// # Errors
    ///
    /// Returns [`WordError`] for an unresolved operand, zero width, an
    /// out-of-range selection, arithmetic overflow, or capacity failure.
    pub fn extract(
        &mut self,
        value: ValueId,
        lsb: u32,
        width: u32,
        source: SourceSpan,
    ) -> Result<ValueId, WordError> {
        let Some(width) = std::num::NonZeroU32::new(width) else {
            return Err(WordError::new("extract width must be non-zero"));
        };
        let value_ty = self.value_ty(value)?;
        let end = lsb
            .checked_add(width.get())
            .ok_or_else(|| WordError::new("extract range exceeds 32-bit capacity"))?;
        if end > value_ty.width() {
            return Err(WordError::new(format!(
                "extract [{lsb} +: {}] exceeds value width {}",
                width.get(),
                value_ty.width()
            )));
        }
        let ty = value_ty.with_width(width.get())?;
        self.push_operation_row(OpKind::Extract { value, lsb, width }, ty, source)
    }

    /// Appends a most-significant-first concatenation row.
    ///
    /// # Errors
    ///
    /// Returns [`WordError`] for an empty list, an unresolved operand, width
    /// overflow, or capacity failure.
    pub fn concat(
        &mut self,
        parts: Vec<ValueId>,
        source: SourceSpan,
    ) -> Result<ValueId, WordError> {
        if parts.is_empty() {
            return Err(WordError::new("concat requires at least one part"));
        }
        let mut width = 0u32;
        let mut state = LogicStateKind::TwoState;
        for &part in &parts {
            let part_ty = self.value_ty(part)?;
            width = width
                .checked_add(part_ty.width())
                .ok_or_else(|| WordError::new("concat width exceeds 32-bit capacity"))?;
            state = state.merge(part_ty.state());
        }
        let ty = WordType::new(width, false, state)?;
        self.push_operation_row(OpKind::Concat { parts }, ty, source)
    }

    /// Appends an explicit extension or truncation row.
    ///
    /// # Errors
    ///
    /// Returns [`WordError`] when an extension shrinks, a truncation widens,
    /// the operand reference is invalid, or capacity fails.
    pub fn cast(
        &mut self,
        kind: CastKind,
        value: ValueId,
        target: WordType,
        source: SourceSpan,
    ) -> Result<ValueId, WordError> {
        let value_ty = self.value_ty(value)?;
        match kind {
            CastKind::ZeroExtend | CastKind::SignExtend if target.width() < value_ty.width() => {
                return Err(WordError::new("extend cast cannot shrink a value"));
            }
            CastKind::Truncate if target.width() > value_ty.width() => {
                return Err(WordError::new("truncate cast cannot widen a value"));
            }
            _ => {}
        }
        self.push_operation_row(
            OpKind::Cast {
                kind,
                value,
                target,
            },
            target,
            source,
        )
    }

    /// Removes one base continuous assignment.
    ///
    /// Indices refer to the base generation and must be requested in strictly
    /// ascending order so the splice can apply them deterministically.
    ///
    /// # Errors
    ///
    /// Returns [`WordError`] when the index is out of range or out of order.
    pub fn remove_connect(&mut self, index: usize) -> Result<(), WordError> {
        if index >= self.fragment.base_connects {
            return Err(WordError::new(format!(
                "fragment removes connect {index} outside the base generation"
            )));
        }
        if self
            .fragment
            .removed_connects
            .last()
            .is_some_and(|&last| last >= index)
        {
            return Err(WordError::new(
                "fragment connect removals must be strictly ascending",
            ));
        }
        self.fragment.removed_connects.push(index);
        Ok(())
    }

    /// Appends one continuous assignment row.
    ///
    /// Type checking against the target runs at publication time through the
    /// ordinary [`WordModule::connect`] rules.
    pub fn connect(&mut self, target: LValue, value: ValueId, source: SourceSpan) {
        self.fragment.added_connects.push(FragmentConnectRow {
            target,
            value,
            source,
        });
    }

    /// Finishes the fragment.
    ///
    /// # Errors
    ///
    /// Returns [`WordError`] when a recorded connect addition references a
    /// dynamic offset outside the base generation or this fragment.
    pub fn build(self) -> Result<WordFragment, WordError> {
        for row in &self.fragment.added_connects {
            self.value_ty(row.value)?;
            if let Some(dynamic) = &row.target.dynamic {
                self.value_ty(dynamic.offset)?;
            }
        }
        Ok(self.fragment)
    }
}

/// Deterministic publication wave over disjoint fragments.
#[derive(Debug, Default)]
pub struct PublicationWave {
    entries: Vec<(FragmentKey, WordFragment)>,
}

impl PublicationWave {
    /// Creates an empty wave.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds one fragment under a deterministic key.
    ///
    /// Duplicate keys are rejected at publication time.
    pub fn push(&mut self, key: FragmentKey, fragment: WordFragment) {
        self.entries.push((key, fragment));
    }

    /// Returns whether the wave would leave the module unchanged.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// One published fragment and the dense IDs its rows received.
#[derive(Debug, Clone)]
pub struct PublishedFragment {
    key: FragmentKey,
    values: Box<[ValueId]>,
    operations: Box<[OpId]>,
}

impl PublishedFragment {
    /// Returns the fragment's publication key.
    #[must_use]
    pub const fn key(&self) -> FragmentKey {
        self.key
    }

    /// Returns the dense ID assigned to each appended value row.
    #[must_use]
    pub fn values(&self) -> &[ValueId] {
        &self.values
    }

    /// Returns the dense ID assigned to each appended operation row.
    #[must_use]
    pub fn operations(&self) -> &[OpId] {
        &self.operations
    }
}

#[derive(Debug, Clone)]
/// Result of one successful publication wave.
pub struct PublishedWave {
    entries: Box<[PublishedFragment]>,
}

impl PublishedWave {
    /// Returns published fragments in ascending key order.
    #[must_use]
    pub fn entries(&self) -> &[PublishedFragment] {
        &self.entries
    }
}

#[derive(Default)]
struct FragmentUndo {
    base_values: usize,
    base_operations: usize,
    connects_after_removals: usize,
    removals: Vec<(usize, Connect)>,
    replaced: Vec<(usize, OpKind)>,
}

impl WordModule {
    /// Publishes one deterministic wave of fragments (RFC 0013 Amendment 1,
    /// steps one and two).
    ///
    /// Step one assigns dense value and operation slots to every fragment's
    /// appended rows in ascending [`FragmentKey`] order, making the resulting
    /// numbering independent of completion order. Step two splices the
    /// fragments into this module under a single undo journal; any failure
    /// leaves this module byte-identical to its state before the call.
    ///
    /// Every fragment must name this module's current arena lengths as its
    /// base generation; a stale fragment aborts the wave without mutation.
    ///
    /// # Errors
    ///
    /// Returns [`WordError`] for duplicate keys, a stale base generation,
    /// overlapping connect removals, a failed connect insertion, or failed
    /// whole-module validation. The module is rolled back atomically on every
    /// error.
    pub fn publish_fragments(&mut self, wave: PublicationWave) -> Result<PublishedWave, WordError> {
        self.publish_fragments_checked(wave, |_, _| Ok::<_, WordError>(()))
            .map(|(published, ())| published)
    }

    /// Publishes one fragment wave and accepts it only after a caller-owned
    /// validation succeeds against the complete spliced module.
    ///
    /// This is the transaction boundary between Word publication and an
    /// external identity, proof, or analysis authority. The callback observes
    /// the fully validated provisional module and its deterministic slot
    /// assignment. If it rejects the wave, every append, operation retarget,
    /// connect removal, and connect addition is rolled back before the error is
    /// returned.
    ///
    /// # Errors
    ///
    /// Returns the caller's error type for either Word publication failure or
    /// callback rejection. `self` is restored exactly on every error.
    pub fn publish_fragments_checked<T, E>(
        &mut self,
        mut wave: PublicationWave,
        accept: impl FnOnce(&Self, &PublishedWave) -> Result<T, E>,
    ) -> Result<(PublishedWave, T), E>
    where
        E: From<WordError>,
    {
        if wave.entries.is_empty() {
            let published = PublishedWave {
                entries: Box::new([]),
            };
            let accepted = accept(self, &published)?;
            return Ok((published, accepted));
        }
        wave.entries.sort_by_key(|(key, _)| key.0);
        if let Some(&(duplicate, _)) = wave
            .entries
            .windows(2)
            .find_map(|pair| (pair[0].0 == pair[1].0).then_some(&pair[0]))
        {
            return Err(WordError::new(format!(
                "publication wave repeats fragment key {:02x?}",
                duplicate.bytes()
            ))
            .into());
        }

        let base_values = self.values.len();
        let base_operations = self.operations.len();
        let base_connects = self.connects.len();
        for (key, fragment) in &wave.entries {
            if fragment.base_values != base_values
                || fragment.base_operations != base_operations
                || fragment.base_connects != base_connects
            {
                return Err(WordError::new(format!(
                    "fragment {:02x?} was built against a different base generation",
                    key.bytes()
                ))
                .into());
            }
        }

        // Step one: deterministic slot assignment in sorted-key order.
        let mut slots = Vec::<(Vec<ValueId>, Vec<OpId>)>::with_capacity(wave.entries.len());
        let mut next_value = base_values;
        let mut next_operation = base_operations;
        for (_, fragment) in &wave.entries {
            let mut value_ids = Vec::with_capacity(fragment.values.len());
            for offset in 0..fragment.values.len() {
                value_ids.push(ValueId::from_index(next_value + offset)?);
            }
            next_value += fragment.values.len();
            let mut operation_ids = Vec::with_capacity(fragment.operations.len());
            for offset in 0..fragment.operations.len() {
                operation_ids.push(OpId::from_index(next_operation + offset)?);
            }
            next_operation += fragment.operations.len();
            slots.push((value_ids, operation_ids));
        }

        let mut undo = FragmentUndo {
            base_values,
            base_operations,
            connects_after_removals: base_connects,
            removals: Vec::new(),
            replaced: Vec::new(),
        };
        if let Err(error) = self.apply_wave(&wave.entries, &slots, &mut undo) {
            self.rollback_fragments(undo);
            return Err(E::from(error));
        }
        if let Err(error) = self.validate() {
            self.rollback_fragments(undo);
            return Err(E::from(error));
        }

        let entries = wave
            .entries
            .into_iter()
            .zip(slots)
            .map(|((key, _), (values, operations))| PublishedFragment {
                key,
                values: values.into_boxed_slice(),
                operations: operations.into_boxed_slice(),
            })
            .collect::<Box<[_]>>();
        let published = PublishedWave { entries };
        match accept(self, &published) {
            Ok(accepted) => Ok((published, accepted)),
            Err(error) => {
                self.rollback_fragments(undo);
                Err(error)
            }
        }
    }

    fn apply_wave(
        &mut self,
        entries: &[(FragmentKey, WordFragment)],
        slots: &[(Vec<ValueId>, Vec<OpId>)],
        undo: &mut FragmentUndo,
    ) -> Result<(), WordError> {
        // Operation retargets first: they only touch the operation arena and
        // record their previous payloads for rollback.
        for ((_, fragment), (value_slots, _)) in entries.iter().zip(slots) {
            for (op, kind) in &fragment.replaced_operations {
                let mut kind = kind.clone();
                remap_kind(&mut kind, fragment.base_values, value_slots);
                let stored = self.operations.get_mut(op.index()).ok_or_else(|| {
                    WordError::new(format!("fragment replaces unknown operation {op:?}"))
                })?;
                undo.replaced
                    .push((op.index(), std::mem::replace(&mut stored.kind, kind)));
            }
        }

        // Connect removals are collected across the wave against base
        // numbering, checked for overlap, and applied highest-index first so
        // recorded indices stay valid.
        let mut removals = Vec::new();
        for (_, fragment) in entries {
            for &index in &fragment.removed_connects {
                if removals
                    .iter()
                    .any(|(recorded, _): &(usize, Connect)| *recorded == index)
                {
                    return Err(WordError::new(format!(
                        "publication wave removes connect {index} twice"
                    )));
                }
                removals.push((index, self.connects[index].clone()));
            }
        }
        removals.sort_unstable_by_key(|(index, _)| *index);
        for &(index, _) in removals.iter().rev() {
            if index >= self.connects.len() {
                return Err(WordError::new(
                    "connect removal raced with the module generation",
                ));
            }
            self.connects.remove(index);
        }
        undo.removals = removals;
        undo.connects_after_removals = self.connects.len();

        // Appends run per entry in assigned-slot order; provisional local
        // references remap onto their published dense IDs.
        for ((_, fragment), (value_slots, _)) in entries.iter().zip(slots) {
            for (offset, row) in fragment.values.iter().enumerate() {
                let expected = value_slots[offset];
                match row {
                    FragmentValueRow::Constant { bits, ty, source } => {
                        let id = self.push_value(
                            ValueKind::Constant(bits.clone()),
                            *ty,
                            source.clone(),
                        )?;
                        debug_assert_eq!(id, expected);
                    }
                    FragmentValueRow::Operation { row, ty, source } => {
                        let mut kind = fragment.operations[*row].kind.clone();
                        remap_kind(&mut kind, fragment.base_values, value_slots);
                        let id = self.push_operation(kind, *ty, source.clone())?;
                        debug_assert_eq!(id, expected);
                    }
                }
            }
            for connect_row in &fragment.added_connects {
                let mut target = connect_row.target.clone();
                if let Some(dynamic) = &mut target.dynamic
                    && dynamic.offset.index() >= fragment.base_values
                {
                    dynamic.offset = value_slots[dynamic.offset.index() - fragment.base_values];
                }
                let value = if connect_row.value.index() >= fragment.base_values {
                    value_slots[connect_row.value.index() - fragment.base_values]
                } else {
                    connect_row.value
                };
                self.connect(target, value, connect_row.source.clone())?;
            }
        }
        Ok(())
    }

    /// Restores the exact pre-publication module state.
    fn rollback_fragments(&mut self, undo: FragmentUndo) {
        self.values.truncate(undo.base_values);
        self.operations.truncate(undo.base_operations);
        for (row, kind) in undo.replaced {
            if let Some(stored) = self.operations.get_mut(row) {
                stored.kind = kind;
            }
        }
        self.connects.truncate(undo.connects_after_removals);
        for (index, connect) in undo.removals {
            self.connects.insert(index, connect);
        }
    }
}

/// Rewrites a payload's provisional references onto their published slots.
fn remap_kind(kind: &mut OpKind, base_values: usize, slots: &[ValueId]) {
    kind.for_each_input_mut(|value| {
        let index = value.index();
        if index >= base_values
            && let Some(slot) = slots.get(index - base_values)
        {
            *value = *slot;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::word::{self, PortDirection, SignalKind};

    fn source(name: &str) -> SourceSpan {
        SourceSpan::stable(format!("fragment test {name}"))
    }

    struct Fixture {
        module: WordModule,
        wide: word::SignalId,
    }

    /// One module with four byte inputs sliced into a 32-bit wire.
    fn fixture() -> Fixture {
        let mut module = WordModule::new("fragments");
        let byte = WordType::bits(8).unwrap();
        let inputs = (0..4)
            .map(|index| {
                let port = module
                    .add_port(
                        format!("a{index}"),
                        PortDirection::Input,
                        byte,
                        source("input"),
                    )
                    .unwrap();
                module
                    .read_signal(module.port(port).unwrap().signal, source("read"))
                    .unwrap()
            })
            .collect::<Vec<_>>();
        let wide = module
            .add_wire("wide", WordType::bits(32).unwrap(), source("wire"))
            .unwrap();
        for (index, &value) in inputs.iter().enumerate() {
            let lsb = u32::try_from(index).unwrap() * 8;
            module
                .connect(
                    LValue::signal(wide).with_range(word::BitRange { msb: lsb + 7, lsb }),
                    value,
                    source("slice"),
                )
                .unwrap();
        }
        Fixture { module, wide }
    }

    /// One fragment per candidate wire: concat of its drivers replacing every
    /// slice connect, keyed by base signal identity.
    fn coalesce_fragments(fixture: &Fixture) -> Vec<(FragmentKey, WordFragment)> {
        let mut connects_by_signal = std::collections::BTreeMap::new();
        for (index, connect) in fixture.module.connects().iter().enumerate() {
            connects_by_signal
                .entry(connect.target.signal)
                .or_insert_with(Vec::new)
                .push(index);
        }
        let mut fragments = Vec::new();
        for (&signal, indices) in &connects_by_signal {
            if indices.len() < 2 {
                continue;
            }
            let ty = fixture.module.signal(signal).unwrap().ty;
            let mut builder = WordFragmentBuilder::new(&fixture.module);
            // Every fixture slice assigns a full-width driver value, so each
            // coalesced part is that value itself.
            let mut parts = indices
                .iter()
                .map(|&connect_index| fixture.module.connects()[connect_index].value)
                .collect::<Vec<_>>();
            parts.reverse();
            let value = builder.concat(parts, source("concat")).unwrap();
            assert_eq!(builder.value_ty(value).unwrap(), ty);
            for &connect_index in indices {
                builder.remove_connect(connect_index).unwrap();
            }
            builder.connect(LValue::signal(signal), value, source("whole"));
            let key = signal_key(signal);
            fragments.push((key, builder.build().unwrap()));
        }
        fragments
    }

    fn signal_key(signal: word::SignalId) -> FragmentKey {
        let mut bytes = [0u8; 32];
        bytes[..8].copy_from_slice(&(signal.index() as u64).to_be_bytes());
        FragmentKey::from_bytes(bytes)
    }

    #[test]
    fn slot_assignment_is_independent_of_wave_insertion_order() {
        let mut first = fixture();
        let mut second = fixture();
        let mut wave_first = PublicationWave::new();
        for (key, fragment) in coalesce_fragments(&first) {
            wave_first.push(key, fragment);
        }
        let mut wave_second = PublicationWave::new();
        let fragments = coalesce_fragments(&second);
        // Push in exactly the opposite order; keys must normalize it.
        for (key, fragment) in fragments.into_iter().rev() {
            wave_second.push(key, fragment);
        }
        let published_first = first.module.publish_fragments(wave_first).unwrap();
        let published_second = second.module.publish_fragments(wave_second).unwrap();

        assert_eq!(
            serde_json::to_string(&first.module).unwrap(),
            serde_json::to_string(&second.module).unwrap(),
        );
        let summarize = |wave: &PublishedWave| {
            wave.entries()
                .iter()
                .map(|entry| {
                    (
                        entry.key(),
                        entry
                            .values()
                            .iter()
                            .map(|id| id.index())
                            .collect::<Vec<_>>(),
                        entry
                            .operations()
                            .iter()
                            .map(|id| id.index())
                            .collect::<Vec<_>>(),
                    )
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(summarize(&published_first), summarize(&published_second));
    }

    #[test]
    fn failed_publication_restores_the_module_exactly() {
        let mut fixture = fixture();
        let before = serde_json::to_string(&fixture.module).unwrap();

        let mut wave = PublicationWave::new();
        // Low key publishes first: its splice removes slice connects and
        // appends the replacement chain before the conflicting fragment fails.
        for (key, fragment) in coalesce_fragments(&fixture) {
            wave.push(key, fragment);
        }
        let mut conflicting = WordFragmentBuilder::new(&fixture.module);
        let mismatched = conflicting
            .constant(
                ConstBits::from_bits(vec![BitVal::One; 7]).unwrap(),
                WordType::bits(7).unwrap(),
                source("mismatch"),
            )
            .unwrap();
        conflicting.connect(LValue::signal(fixture.wide), mismatched, source("bad"));
        wave.push(
            FragmentKey::from_bytes([u8::MAX; 32]),
            conflicting.build().unwrap(),
        );

        assert!(fixture.module.publish_fragments(wave).is_err());
        let after = serde_json::to_string(&fixture.module).unwrap();
        assert_eq!(before, after, "failed publication must restore every arena");
        assert_eq!(
            fixture.module.signals()[fixture.wide.index()].kind,
            SignalKind::Wire
        );
    }

    #[test]
    fn rejected_cross_authority_validation_restores_the_module_exactly() {
        let mut fixture = fixture();
        let before = fixture.module.clone();
        let mut wave = PublicationWave::new();
        for (key, fragment) in coalesce_fragments(&fixture) {
            wave.push(key, fragment);
        }

        let error = fixture
            .module
            .publish_fragments_checked(wave, |module, published| {
                assert!(!published.entries().is_empty());
                assert_ne!(before, *module);
                Err::<(), _>(WordError::new("revision authority rejected the wave"))
            })
            .unwrap_err();

        assert!(error.to_string().contains("revision authority rejected"));
        assert_eq!(before, fixture.module);
    }

    #[test]
    fn duplicate_and_stale_fragments_are_rejected_without_mutation() {
        let mut fixture = fixture();
        let before = serde_json::to_string(&fixture.module).unwrap();
        for (key, fragment) in coalesce_fragments(&fixture) {
            let duplicate = fragment.clone();
            let mut wave = PublicationWave::new();
            wave.push(key, fragment);
            wave.push(key, duplicate);
            assert!(fixture.module.publish_fragments(wave).is_err());
        }
        assert_eq!(before, serde_json::to_string(&fixture.module).unwrap());

        let stale = WordFragment {
            base_values: fixture.module.values.len() + 1,
            base_operations: fixture.module.operations.len(),
            base_connects: fixture.module.connects().len(),
            values: Vec::new(),
            operations: Vec::new(),
            replaced_operations: Vec::new(),
            removed_connects: Vec::new(),
            added_connects: Vec::new(),
        };
        let mut wave = PublicationWave::new();
        wave.push(FragmentKey::from_bytes([1; 32]), stale);
        assert!(fixture.module.publish_fragments(wave).is_err());
        assert_eq!(before, serde_json::to_string(&fixture.module).unwrap());
    }
}
