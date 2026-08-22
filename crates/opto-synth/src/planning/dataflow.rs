// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

mod canonical;
mod priority;
mod sequential;

use canonical::canonicalize_values;
pub(crate) use sequential::{
    share_equivalent_sequential_values_by, shareable_sequential_operations,
};

use opto_core::PackedRows;
use opto_ir::word;
use std::collections::BTreeMap;

/// Reconstruct fully and uniquely driven wires as one canonical whole-signal
/// connection before region ownership is frozen.
///
/// A packed or flattened aggregate is commonly assembled by several static
/// slice assignments. Keeping those assignments separate makes a whole-signal
/// read span several producer values, which cannot be represented by one
/// regional boundary value. Canonicalizing only complete, forward, static,
/// non-overlapping wire drivers preserves the source semantics while giving
/// partitioning and publication one explicit dataflow value.
///
/// The rewrite no longer mutates the Word module in place. It returns one
/// [`WordFragment`] per candidate wire, keyed so publication reproduces the
/// dense-ID sequence the retired in-place pass produced; RFC 0013 Amendment 1
/// publication splices them and commits the changed revision cone.
pub(crate) fn static_wire_driver_fragments(
    module: &word::WordModule,
) -> Result<StaticWireCoalescing, crate::SynthError> {
    #[derive(Clone, Copy)]
    struct DrivenBit {
        value: word::ValueId,
        bit: u32,
    }

    let preserved = module
        .preserved_signals()
        .collect::<std::collections::BTreeSet<_>>();
    let mut candidates = BTreeMap::<word::SignalId, (Vec<DrivenBit>, word::SourceSpan)>::new();
    let mut candidate_connects = BTreeMap::<word::SignalId, Vec<usize>>::new();
    for (connect_index, connect) in module.connects().iter().enumerate() {
        candidate_connects
            .entry(connect.target.signal)
            .or_default()
            .push(connect_index);
    }
    for signal_index in 0..module.signals().len() {
        let signal_id =
            word::SignalId::from_index(signal_index).map_err(crate::SynthError::Word)?;
        let signal = &module.signals()[signal_index];
        if signal.kind != word::SignalKind::Wire
            || signal.ty.width() <= 1
            || preserved.contains(&signal_id)
        {
            continue;
        }
        let Some(drivers) = candidate_connects.get(&signal_id) else {
            continue;
        };
        if drivers.len() < 2 {
            continue;
        }
        let mut bits = vec![None; signal.ty.width() as usize];
        let mut valid = true;
        for &connect_index in drivers {
            let connect = &module.connects()[connect_index];
            if connect.target.dynamic.is_some() {
                valid = false;
                break;
            }
            let Some(value) = module.value(connect.value) else {
                return Err(crate::SynthError::invariant(
                    "static wire driver references an unknown value",
                ));
            };
            let (lsb, width) = match connect.target.range {
                Some(range) if range.msb >= range.lsb => (range.lsb, range.width()),
                None if value.ty.width() == signal.ty.width() => (0, signal.ty.width()),
                Some(_) | None => {
                    valid = false;
                    break;
                }
            };
            if value.ty.width() != width {
                valid = false;
                break;
            }
            for source_bit in 0..width {
                let target_bit = lsb
                    .checked_add(source_bit)
                    .ok_or_else(|| crate::SynthError::capacity("static wire target bit offset"))?;
                let Some(slot) = bits.get_mut(target_bit as usize) else {
                    valid = false;
                    break;
                };
                if slot
                    .replace(DrivenBit {
                        value: connect.value,
                        bit: source_bit,
                    })
                    .is_some()
                {
                    valid = false;
                    break;
                }
            }
            if !valid {
                break;
            }
        }
        if !valid || bits.iter().any(Option::is_none) {
            candidate_connects.remove(&signal_id);
            continue;
        }
        candidates.insert(
            signal_id,
            (
                bits.into_iter().map(Option::unwrap).collect(),
                module.connects()[drivers[0]].source.clone(),
            ),
        );
    }
    if candidates.is_empty() {
        return Ok(StaticWireCoalescing::default());
    }

    let mut wave = word::PublicationWave::new();
    let mut signals = Vec::with_capacity(candidates.len());
    for (&signal, (bits, source)) in &candidates {
        // The key orders fragments exactly like the retired in-place pass
        // appended its rows: ascending base signal identity.
        let key = coalesce_fragment_key(signal);
        let mut builder = word::WordFragmentBuilder::new(module);
        let mut runs = Vec::<(word::ValueId, u32, u32)>::new();
        for &bit in bits {
            if let Some((value, first, width)) = runs.last_mut()
                && *value == bit.value
                && first.checked_add(*width) == Some(bit.bit)
            {
                *width = width
                    .checked_add(1)
                    .ok_or_else(|| crate::SynthError::capacity("static wire driver run width"))?;
            } else {
                runs.push((bit.value, bit.bit, 1));
            }
        }
        let mut parts = Vec::with_capacity(runs.len());
        for (value, lsb, width) in runs {
            let value_width = module
                .value(value)
                .ok_or_else(|| {
                    crate::SynthError::invariant(
                        "coalesced wire driver references an unknown value",
                    )
                })?
                .ty
                .width();
            parts.push(if lsb == 0 && width == value_width {
                value
            } else {
                builder
                    .extract(value, lsb, width, source.clone())
                    .map_err(crate::SynthError::from)?
            });
        }
        parts.reverse();
        let mut value = if let [value] = parts.as_slice() {
            *value
        } else {
            builder
                .concat(parts, source.clone())
                .map_err(crate::SynthError::from)?
        };
        let signal_ty = module
            .signal(signal)
            .ok_or_else(|| crate::SynthError::invariant("coalesced wire disappeared"))?
            .ty;
        if builder.value_ty(value)? != signal_ty {
            value = builder
                .cast(
                    if signal_ty.is_signed() {
                        word::CastKind::SignExtend
                    } else {
                        word::CastKind::ZeroExtend
                    },
                    value,
                    signal_ty,
                    source.clone(),
                )
                .map_err(crate::SynthError::from)?;
        }
        for &connect_index in &candidate_connects[&signal] {
            builder.remove_connect(connect_index)?;
        }
        builder.connect(word::LValue::signal(signal), value, source.clone());
        let fragment = builder.build().map_err(crate::SynthError::from)?;
        signals.push((key, signal));
        wave.push(key, fragment);
    }
    Ok(StaticWireCoalescing {
        wave,
        signals: signals.into_boxed_slice(),
    })
}

fn coalesce_fragment_key(signal: word::SignalId) -> word::FragmentKey {
    // The key orders fragments exactly like the retired in-place pass appended
    // its rows: ascending base signal identity.
    let mut bytes = [0u8; 32];
    bytes[..8].copy_from_slice(&(signal.index() as u64).to_be_bytes());
    word::FragmentKey::from_bytes(bytes)
}

#[derive(Debug, Default)]
pub(crate) struct StaticWireCoalescing {
    wave: word::PublicationWave,
    /// Candidate wires ordered exactly like [`Self::wave`] entries after
    /// publication sorts them by key.
    signals: Box<[(word::FragmentKey, word::SignalId)]>,
}

impl StaticWireCoalescing {
    pub(crate) fn is_empty(&self) -> bool {
        self.signals.is_empty()
    }

    /// Splits into the publication wave and its candidate wires, which stay
    /// aligned with the wave entries after key-ordered publication.
    pub(crate) fn into_parts(
        self,
    ) -> (
        word::PublicationWave,
        Box<[(word::FragmentKey, word::SignalId)]>,
    ) {
        (self.wave, self.signals)
    }
}

pub(crate) struct DataflowChanges {
    representatives: Box<[word::ValueId]>,
    changed: bool,
}

impl DataflowChanges {
    fn identity(value_count: usize) -> Result<Self, crate::SynthError> {
        let representatives = (0..value_count)
            .map(|index| word::ValueId::from_index(index).map_err(crate::SynthError::Word))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            representatives: representatives.into_boxed_slice(),
            changed: false,
        })
    }

    fn from_aliases(
        value_count: usize,
        aliases: &[(word::ValueId, word::ValueId)],
    ) -> Result<Self, crate::SynthError> {
        let mut changes = Self::identity(value_count)?;
        for &(value, representative) in aliases {
            let slot = changes
                .representatives
                .get_mut(value.index())
                .ok_or_else(|| {
                    crate::SynthError::invariant("dataflow alias is outside the Word value arena")
                })?;
            *slot = representative;
        }
        changes.changed = !aliases.is_empty();
        Ok(changes)
    }

    pub(crate) fn representatives(&self) -> &[word::ValueId] {
        &self.representatives
    }

    /// Composes two committed rewrites into one entry-to-final remap.
    fn then(self, next: &Self) -> Result<Self, crate::SynthError> {
        let advance = |value: word::ValueId| {
            next.representatives
                .get(value.index())
                .copied()
                .ok_or_else(|| {
                    crate::SynthError::invariant(
                        "composed dataflow representative is outside the next value arena",
                    )
                })
        };
        let representatives = self
            .representatives
            .iter()
            .copied()
            .map(advance)
            .collect::<Result<_, _>>()?;
        Ok(Self {
            representatives,
            changed: self.changed || next.changed,
        })
    }

    #[cfg(test)]
    pub(crate) const fn has_equivalences(&self) -> bool {
        self.changed
    }
}

pub(crate) fn optimize_combinational_dataflow(
    module: &mut word::WordModule,
) -> Result<DataflowChanges, crate::SynthError> {
    let changes = canonicalize_combinational_dataflow(module)?;
    let priority = priority::rebalance_constant_priority_muxes(module)?;
    if priority.changed {
        let changes = changes.then(&priority)?;
        let rebalanced = canonicalize_combinational_dataflow(module)?;
        changes.then(&rebalanced)
    } else {
        Ok(changes)
    }
}

pub(crate) fn canonicalize_combinational_dataflow(
    module: &mut word::WordModule,
) -> Result<DataflowChanges, crate::SynthError> {
    optimize_combinational_dataflow_by(module, |_, _| true)
}

pub(crate) fn resolve_static_connect_aliases(
    module: &mut word::WordModule,
) -> Result<DataflowChanges, crate::SynthError> {
    let drivers = DriverIndex::build(module)?;
    let resolved_values = resolve_connect_aliases(module, &drivers)?;
    apply_representatives(module, &drivers, resolved_values, |_, _| true)
}

pub(crate) fn optimize_combinational_dataflow_by(
    module: &mut word::WordModule,
    permit_equivalence: impl FnMut(word::ValueId, word::ValueId) -> bool,
) -> Result<DataflowChanges, crate::SynthError> {
    let drivers = DriverIndex::build(module)?;
    let mut resolved_values = resolve_connect_aliases(module, &drivers)?;
    canonicalize_values(module, &mut resolved_values)?;
    apply_representatives(module, &drivers, resolved_values, permit_equivalence)
}

fn resolve_connect_aliases(
    module: &word::WordModule,
    drivers: &DriverIndex,
) -> Result<Vec<word::ValueId>, crate::SynthError> {
    let mut resolver = AliasResolver::new(module, drivers);
    (0..module.values().len())
        .map(|index| {
            let value = word::ValueId::from_index(index).map_err(crate::SynthError::Word)?;
            resolver.resolve(value)
        })
        .collect()
}

fn apply_representatives(
    module: &mut word::WordModule,
    drivers: &DriverIndex,
    mut resolved_values: Vec<word::ValueId>,
    mut permit_equivalence: impl FnMut(word::ValueId, word::ValueId) -> bool,
) -> Result<DataflowChanges, crate::SynthError> {
    // Permission is defined over the terminal representative. Checking an
    // intermediate alias can otherwise approve an equivalence whose transitive
    // target belongs to a different hard synthesis region.
    close_representatives(&mut resolved_values)?;
    for (index, canonical) in resolved_values.iter_mut().enumerate() {
        let original = word::ValueId::from_index(index).map_err(crate::SynthError::Word)?;
        if original != *canonical && !permit_equivalence(original, *canonical) {
            *canonical = original;
        }
    }
    close_representatives(&mut resolved_values)?;
    let read_bits = read_signal_bits(module, drivers, &resolved_values)?;
    let removable_connects = module
        .connects()
        .iter()
        .map(|connect| drivers.is_removable(module, connect, &read_bits))
        .collect::<Result<Vec<_>, crate::SynthError>>()?;
    commit_representatives(module, &resolved_values, &removable_connects)?;
    let changed = resolved_values.iter().enumerate().any(|(index, &value)| {
        word::ValueId::from_index(index).is_ok_and(|original| original != value)
    });
    Ok(DataflowChanges {
        representatives: resolved_values.into_boxed_slice(),
        changed,
    })
}

/// Substitutes the canonical representatives into every Word IR read, then
/// drops the connects the substitution left unreachable.
///
/// Both dataflow entry points must commit through this one function.
/// [`read_signal_bits`] charges a signal read to its representative, so a wire
/// whose readers were all substituted is judged removable. Rewriting only
/// operation operands and connects would honour that judgement for those two
/// readers while leaving instance connections and memory ports still naming the
/// wire, which is how a clock-gate enable ends up driven by nothing.
fn commit_representatives(
    module: &mut word::WordModule,
    representatives: &[word::ValueId],
    removable_connects: &[bool],
) -> Result<(), crate::SynthError> {
    module
        .rewrite_value_uses(representatives)
        .map_err(crate::SynthError::from)?;
    for (connect, &removable) in module.take_connects().into_iter().zip(removable_connects) {
        if removable {
            continue;
        }
        module
            .connect(connect.target, connect.value, connect.source)
            .map_err(crate::SynthError::from)?;
    }
    Ok(())
}

fn close_representatives(representatives: &mut [word::ValueId]) -> Result<(), crate::SynthError> {
    let mut state = vec![0u8; representatives.len()];
    let mut path = Vec::new();
    for start in 0..representatives.len() {
        if state[start] == 2 {
            continue;
        }
        path.clear();
        let mut current = start;
        loop {
            let representative = representatives.get(current).copied().ok_or_else(|| {
                crate::SynthError::invariant(
                    "dataflow canonical representative is outside the Word value arena",
                )
            })?;
            if representative.index() == current {
                state[current] = 2;
                break;
            }
            match state[current] {
                0 => {
                    state[current] = 1;
                    path.push(current);
                    current = representative.index();
                }
                1 => {
                    return Err(crate::SynthError::invariant(
                        "dataflow canonical representatives contain a cycle",
                    ));
                }
                2 => break,
                _ => {
                    return Err(crate::SynthError::invariant(
                        "dataflow canonical state is outside its three-state domain",
                    ));
                }
            }
        }
        let terminal = representatives.get(current).copied().ok_or_else(|| {
            crate::SynthError::invariant(
                "dataflow canonical representative is outside the Word value arena",
            )
        })?;
        for &value in path.iter().rev() {
            representatives[value] = terminal;
            state[value] = 2;
        }
    }
    Ok(())
}

pub(crate) fn rewrite_operation_inputs(
    kind: &mut word::OpKind,
    mut rewrite: impl FnMut(word::ValueId) -> Result<word::ValueId, crate::SynthError>,
) -> Result<(), crate::SynthError> {
    kind.try_for_each_input_mut(|value| {
        *value = rewrite(*value)?;
        Ok(())
    })
}

#[derive(Debug, Clone, Copy, Default)]
enum Driver {
    #[default]
    None,
    Unique(word::ValueId),
    Multiple,
}

/// Unique driver of every signal bit, in one packed row per signal.
///
/// `opto_core::PackedRows` owns the row layout, so the parallel per-bit sets in
/// this module share its flat coordinates instead of rebuilding an offsets
/// table.
#[derive(Debug)]
struct DriverIndex {
    by_bit: PackedRows<Driver>,
    connects_by_signal: PackedRows<usize>,
}

impl DriverIndex {
    fn build(module: &word::WordModule) -> Result<Self, crate::SynthError> {
        let mut rows = module
            .signals()
            .iter()
            .map(|signal| vec![Driver::None; signal.ty.width() as usize])
            .collect::<Vec<_>>();
        for connect in module.connects() {
            let value = module.value(connect.value).ok_or_else(|| {
                crate::SynthError::invariant(format!(
                    "connect references unknown value {:?}",
                    connect.value
                ))
            })?;
            let bits = static_target_bits(module, &connect.target)?;
            if value.ty.width() as usize != bits.len() {
                return Err(crate::SynthError::invariant(
                    "static connect driver width does not match its target",
                ));
            }
            for bit in bits {
                let slot = rows
                    .get_mut(connect.target.signal.index())
                    .and_then(|row| row.get_mut(bit as usize))
                    .ok_or_else(|| {
                        crate::SynthError::invariant(format!(
                            "connect target references out-of-range signal bit {:?}[{bit}]",
                            connect.target.signal
                        ))
                    })?;
                *slot = match *slot {
                    Driver::None => Driver::Unique(connect.value),
                    Driver::Unique(_) | Driver::Multiple => Driver::Multiple,
                };
            }
        }
        let by_bit = PackedRows::try_from_rows(rows)
            .map_err(|error| crate::SynthError::capacity(error.to_string()))?;
        let connects_by_signal = PackedRows::try_from_entries(
            module.signals().len(),
            module
                .connects()
                .iter()
                .enumerate()
                .map(|(index, connect)| (connect.target.signal.index(), index)),
        )
        .map_err(|error| crate::SynthError::capacity(error.to_string()))?;
        Ok(Self {
            by_bit,
            connects_by_signal,
        })
    }

    fn unique(&self, bit: SignalBit) -> Option<word::ValueId> {
        match self.by_bit.values().get(self.flat_bit(bit)?).copied()? {
            Driver::Unique(value) => Some(value),
            Driver::None | Driver::Multiple => None,
        }
    }

    fn bit_count(&self) -> usize {
        self.by_bit.value_count()
    }

    fn flat_bit(&self, bit: SignalBit) -> Option<usize> {
        let range = self.by_bit.row_range(bit.signal.index())?;
        range
            .start
            .checked_add(bit.bit as usize)
            .filter(|&flat| flat < range.end)
    }

    fn exact_static_driver(
        &self,
        module: &word::WordModule,
        reference: word::SignalRef,
    ) -> Option<word::ValueId> {
        let reference_end = reference.lsb.checked_add(reference.width())?;
        let mut driver = None;
        for &index in self
            .connects_by_signal
            .get(reference.signal.index())
            .unwrap_or_default()
        {
            let connect = module.connects().get(index)?;
            if connect.target.dynamic.is_some() {
                return None;
            }
            let signal_width = module.signal(reference.signal)?.ty.width();
            let (start, end, forward) =
                connect
                    .target
                    .range
                    .map_or((0, signal_width, true), |range| {
                        (
                            range.msb.min(range.lsb),
                            range.msb.max(range.lsb).saturating_add(1),
                            range.msb >= range.lsb,
                        )
                    });
            if start >= reference_end || reference.lsb >= end {
                continue;
            }
            if !forward || start != reference.lsb || end != reference_end || driver.is_some() {
                return None;
            }
            driver = Some(connect.value);
        }
        driver
    }

    fn is_removable(
        &self,
        module: &word::WordModule,
        connect: &word::Connect,
        read_bits: &ReadSignalBits,
    ) -> Result<bool, crate::SynthError> {
        let bits = static_target_bits(module, &connect.target)?;
        if bits.clone().any(|bit| {
            let bit = SignalBit {
                signal: connect.target.signal,
                bit,
            };
            self.unique(bit).is_none() || read_bits.contains(self, bit)
        }) {
            return Ok(false);
        }
        let signal = module.signal(connect.target.signal).ok_or_else(|| {
            crate::SynthError::invariant(format!(
                "connect targets unknown signal {:?}",
                connect.target.signal
            ))
        })?;
        if signal.kind != word::SignalKind::Wire {
            return Ok(false);
        }
        Ok(!is_register_value(module, connect.value)?)
    }
}

struct AliasResolver<'a> {
    module: &'a word::WordModule,
    drivers: &'a DriverIndex,
    preserved_signals: Vec<bool>,
    resolved: Vec<Option<word::ValueId>>,
    active_bits: Vec<bool>,
}

impl<'a> AliasResolver<'a> {
    fn new(module: &'a word::WordModule, drivers: &'a DriverIndex) -> Self {
        Self {
            module,
            drivers,
            preserved_signals: alias_boundary_signals(module),
            resolved: vec![None; module.values().len()],
            active_bits: vec![false; drivers.bit_count()],
        }
    }

    fn resolve(&mut self, value: word::ValueId) -> Result<word::ValueId, crate::SynthError> {
        if let Some(resolved) = self.resolved.get(value.index()).copied().flatten() {
            return Ok(resolved);
        }
        let model = self
            .module
            .value(value)
            .ok_or_else(|| crate::SynthError::invariant(format!("unknown value {value:?}")))?;
        let resolved = match model.kind {
            word::ValueKind::Signal(reference)
                if self
                    .preserved_signals
                    .get(reference.signal.index())
                    .copied()
                    .unwrap_or(false) =>
            {
                value
            }
            word::ValueKind::Signal(reference) => {
                let Some(driver) = self.drivers.exact_static_driver(self.module, reference) else {
                    self.resolved[value.index()] = Some(value);
                    return Ok(value);
                };
                let active = (0..reference.width())
                    .map(|offset| SignalBit {
                        signal: reference.signal,
                        bit: reference.lsb + offset,
                    })
                    .map(|bit| {
                        self.drivers.flat_bit(bit).ok_or_else(|| {
                            crate::SynthError::invariant(format!(
                                "signal value references out-of-range bit {:?}[{}]",
                                bit.signal, bit.bit
                            ))
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                if active.iter().any(|&flat| self.active_bits[flat]) {
                    return Err(crate::SynthError::invariant(format!(
                        "combinational signal-driver cycle at signal {:?}[{} +: {}]",
                        reference.signal,
                        reference.lsb,
                        reference.width()
                    )));
                }
                for &flat in &active {
                    self.active_bits[flat] = true;
                }
                let driver = if is_register_value(self.module, driver)? {
                    value
                } else {
                    self.resolve(driver)?
                };
                for flat in active {
                    self.active_bits[flat] = false;
                }
                driver
            }
            word::ValueKind::Constant(_) | word::ValueKind::Operation(_) => value,
        };
        self.resolved[value.index()] = Some(resolved);
        Ok(resolved)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct SignalBit {
    signal: word::SignalId,
    bit: u32,
}

fn static_target_bits(
    module: &word::WordModule,
    target: &word::LValue,
) -> Result<std::ops::Range<u32>, crate::SynthError> {
    if target.dynamic.is_some() {
        return Err(crate::SynthError::invariant(
            "dynamic connect target reached dataflow optimization",
        ));
    }
    let signal = module.signal(target.signal).ok_or_else(|| {
        crate::SynthError::invariant(format!(
            "connect targets unknown signal {:?}",
            target.signal
        ))
    })?;
    let bits = match target.range {
        None => 0..signal.ty.width(),
        Some(range) => {
            let start = range.msb.min(range.lsb);
            let end = range
                .msb
                .max(range.lsb)
                .checked_add(1)
                .ok_or_else(|| crate::SynthError::capacity("static connect target range"))?;
            if end > signal.ty.width() {
                return Err(crate::SynthError::invariant(
                    "static connect target range exceeds its signal",
                ));
            }
            start..end
        }
    };
    Ok(bits)
}

/// The signal bits some value still names once substitution has been applied.
///
/// Collapsing a read into its driver and deleting the wire's driving connect
/// are one decision seen from two sides: the connect may go exactly when no
/// read of it survives. Asking the substitution result directly is what makes
/// that a single decision. Approximating it instead — enumerating the reasons a
/// read might survive — leaves a wire with readers and no driver every time the
/// enumeration is incomplete.
#[derive(Debug)]
struct ReadSignalBits {
    bits: Vec<bool>,
    preserved_signals: Vec<bool>,
}

impl ReadSignalBits {
    fn contains(&self, drivers: &DriverIndex, bit: SignalBit) -> bool {
        self.preserved_signals
            .get(bit.signal.index())
            .copied()
            .unwrap_or(false)
            || drivers.flat_bit(bit).is_some_and(|flat| self.bits[flat])
    }
}

fn read_signal_bits(
    module: &word::WordModule,
    drivers: &DriverIndex,
    resolved_values: &[word::ValueId],
) -> Result<ReadSignalBits, crate::SynthError> {
    // `close_representatives` makes the mapping idempotent, so a value keeps a
    // use only when it resolves to itself. Counting through the mapping is
    // therefore exactly the post-substitution use count.
    let mut used = vec![false; module.values().len()];
    for value in crate::word::uses::direct_value_uses(module) {
        let representative = resolved_values.get(value.index()).copied().ok_or_else(|| {
            crate::SynthError::invariant(format!("dataflow use references unknown value {value:?}"))
        })?;
        *used.get_mut(representative.index()).ok_or_else(|| {
            crate::SynthError::invariant(
                "dataflow canonical representative is outside the Word value arena",
            )
        })? = true;
    }
    // A Word operation may only name values the arena defines before it, so a
    // read whose representative is defined later cannot be substituted at all.
    // Its wire therefore keeps the connect that drives it, whatever the use
    // counts say.
    for operation in module.operations() {
        for input in crate::word::operation_inputs(&operation.kind) {
            let representative = resolved_values.get(input.index()).copied().ok_or_else(|| {
                crate::SynthError::invariant("dataflow operation input is outside the value arena")
            })?;
            if representative.index() < operation.result.index() {
                continue;
            }
            *used.get_mut(input.index()).ok_or_else(|| {
                crate::SynthError::invariant("dataflow operation input is outside the value arena")
            })? = true;
        }
    }
    let mut bits = vec![false; drivers.bit_count()];
    for (index, value) in module.values().iter().enumerate() {
        let word::ValueKind::Signal(reference) = value.kind else {
            continue;
        };
        if !used[index] {
            continue;
        }
        for offset in 0..reference.width() {
            let bit = SignalBit {
                signal: reference.signal,
                bit: reference.lsb + offset,
            };
            if let Some(flat) = drivers.flat_bit(bit) {
                bits[flat] = true;
            }
        }
    }
    Ok(ReadSignalBits {
        bits,
        preserved_signals: alias_boundary_signals(module),
    })
}

/// Signals whose reads must not collapse onto a local connect driver.
///
/// Explicit preservation fixes signal identity. A tri-state net is also an
/// identity boundary: a read observes the physically resolved net, not merely
/// one local TBUF contribution. Treating either as an ordinary alias can feed
/// a `TriState` value into Boolean or sequential logic and delete the physical
/// driver connection that owns its resolution semantics.
fn alias_boundary_signals(module: &word::WordModule) -> Vec<bool> {
    let mut boundaries = module
        .signals()
        .iter()
        .map(|signal| signal.resolution == word::SignalResolution::TriState)
        .collect::<Vec<_>>();
    for signal in module.preserved_signals() {
        boundaries[signal.index()] = true;
    }
    boundaries
}

fn is_register_value(
    module: &word::WordModule,
    value: word::ValueId,
) -> Result<bool, crate::SynthError> {
    let value = module
        .value(value)
        .ok_or_else(|| crate::SynthError::invariant(format!("unknown value {value:?}")))?;
    let word::ValueKind::Operation(operation) = value.kind else {
        return Ok(false);
    };
    let operation = module
        .operation(operation)
        .ok_or_else(|| crate::SynthError::invariant(format!("unknown operation {operation:?}")))?;
    Ok(matches!(
        operation.kind,
        word::OpKind::Register(_) | word::OpKind::Latch(_)
    ))
}

#[cfg(test)]
mod tests;
