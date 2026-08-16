// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Realization of first-class memories as inferred or characterized macros.

use opto_ir::{BitVal, ConstBits, word};
use opto_library::{
    TargetCellRef, TargetCellSet, TargetMemoryDisabledRead, TargetMemoryEdge,
    TargetMemoryReadDuringWrite,
};
use std::collections::BTreeMap;

mod macro_binding;

#[derive(Debug)]
struct Bank {
    words: Vec<word::ValueId>,
    next: Vec<word::ValueId>,
}

#[derive(Debug, Default)]
struct MemoryOwnership {
    operations: Vec<word::OpId>,
    state_values: Vec<word::ValueId>,
}

#[derive(Debug, Default)]
pub(crate) struct MemoryLoweringOwnership {
    memories: Vec<MemoryOwnership>,
}

impl MemoryLoweringOwnership {
    pub(crate) fn operations(&self) -> impl Iterator<Item = (word::OpId, word::MemoryId)> + '_ {
        self.memories
            .iter()
            .enumerate()
            .flat_map(|(index, ownership)| {
                let memory = word::MemoryId::from_index(index)
                    .expect("memory ownership rows use valid dense IDs");
                ownership
                    .operations
                    .iter()
                    .copied()
                    .map(move |operation| (operation, memory))
            })
    }

    pub(crate) fn operation(&self, memory: word::MemoryId, ordinal: u32) -> Option<word::OpId> {
        self.memories
            .get(memory.index())?
            .operations
            .get(ordinal as usize)
            .copied()
    }

    pub(crate) fn state_values(
        &self,
    ) -> impl Iterator<Item = (word::ValueId, word::MemoryId)> + '_ {
        self.memories
            .iter()
            .enumerate()
            .flat_map(|(index, ownership)| {
                let memory = word::MemoryId::from_index(index)
                    .expect("memory ownership rows use valid dense IDs");
                ownership
                    .state_values
                    .iter()
                    .copied()
                    .map(move |value| (value, memory))
            })
    }

    pub(crate) fn state_value(
        &self,
        memory: word::MemoryId,
        ordinal: u32,
    ) -> Option<word::ValueId> {
        self.memories
            .get(memory.index())?
            .state_values
            .get(ordinal as usize)
            .copied()
    }
}

fn unique_name(
    base: &str,
    mut reserve: impl FnMut(&str) -> bool,
    exhausted: &'static str,
) -> Result<String, crate::SynthError> {
    if reserve(base) {
        return Ok(base.to_string());
    }
    for suffix in 1..=u64::MAX {
        let name = format!("{base}${suffix}");
        if reserve(&name) {
            return Ok(name);
        }
    }
    Err(crate::SynthError::capacity(exhausted))
}

/// Atomically materializes every selected first-class memory implementation.
/// Register banks are scalarized by word; characterized macros become exact
/// pin-bound target instances and leave no aggregate memory resource behind.
pub(crate) fn lower_selected_memories(
    module: &mut word::WordModule,
    implementations: &[crate::planning::regional::MemoryImplementationCandidate],
    target_cells: &TargetCellSet,
) -> Result<MemoryLoweringOwnership, crate::SynthError> {
    module
        .validate_memories()
        .map_err(crate::SynthError::from)?;
    if implementations.len() != module.memories().len() {
        return Err(crate::SynthError::invariant(
            "selected memory implementations do not align with the memory arena",
        ));
    }
    preflight(module, implementations, target_cells)?;
    let resources = module.take_memory_resources();
    if resources.is_empty() {
        return Ok(MemoryLoweringOwnership::default());
    }

    let mut reads = vec![Vec::new(); resources.memories.len()];
    let mut writes = vec![Vec::new(); resources.memories.len()];
    for read in resources.reads {
        reads[read.memory.index()].push(read);
    }
    for write in resources.writes {
        writes[write.memory.index()].push(write);
    }
    for ports in &mut writes {
        ports.sort_by_key(|port| port.priority);
    }

    let mut ownership = MemoryLoweringOwnership {
        memories: (0..resources.memories.len())
            .map(|_| MemoryOwnership::default())
            .collect(),
    };
    for (index, memory) in resources.memories.iter().enumerate() {
        let first_operation = module.operations().len();
        match implementations[index] {
            crate::planning::regional::MemoryImplementationCandidate::RegisterBank => {
                let bank = materialize_storage(module, memory, &writes[index])?;
                ownership.memories[index]
                    .state_values
                    .extend(bank.words.iter().copied());
                for port in std::mem::take(&mut reads[index]) {
                    materialize_read(module, &bank, &writes[index], port)?;
                }
            }
            crate::planning::regional::MemoryImplementationCandidate::Macro(cell_index) => {
                let cell = target_cells.get(cell_index as usize).ok_or_else(|| {
                    crate::SynthError::invariant(
                        "selected memory macro index is outside the target library",
                    )
                })?;
                materialize_macro(module, memory, &reads[index], &writes[index], cell)?;
            }
        }
        for operation in first_operation..module.operations().len() {
            let operation = word::OpId::from_index(operation).map_err(crate::SynthError::from)?;
            ownership.memories[index].operations.push(operation);
        }
    }
    if !module.memories().is_empty()
        || !module.memory_read_ports().is_empty()
        || !module.memory_write_ports().is_empty()
    {
        return Err(crate::SynthError::invariant(
            "memory resource inference left first-class resources behind",
        ));
    }
    module
        .validate_memories()
        .map_err(crate::SynthError::from)?;
    Ok(ownership)
}

#[cfg(test)]
pub(crate) fn lower_memories_to_register_banks(
    module: &mut word::WordModule,
) -> Result<MemoryLoweringOwnership, crate::SynthError> {
    let implementations = vec![
            crate::planning::regional::MemoryImplementationCandidate::RegisterBank;
            module.memories().len()
        ];
    lower_selected_memories(module, &implementations, &TargetCellSet::default())
}

fn preflight(
    module: &word::WordModule,
    implementations: &[crate::planning::regional::MemoryImplementationCandidate],
    target_cells: &TargetCellSet,
) -> Result<(), crate::SynthError> {
    let mut clocks = BTreeMap::<word::MemoryId, word::MemoryClock>::new();
    for port in module.memory_write_ports() {
        if implementations[port.memory.index()]
            != crate::planning::regional::MemoryImplementationCandidate::RegisterBank
        {
            continue;
        }
        if let Some(clock) = clocks.insert(port.memory, port.clock)
            && !same_clock(module, clock, port.clock)
        {
            let name = module
                .memory(port.memory)
                .map_or("<unknown>", |memory| module.name_str(memory.name));
            return Err(crate::SynthError::unsupported(format!(
                "register-bank memory '{name}' has multiple write clocks"
            )));
        }
    }
    for (index, memory) in module.memories().iter().enumerate() {
        let id = word::MemoryId::from_index(index)
            .map_err(|error| crate::SynthError::capacity(error.to_string()))?;
        match implementations[index] {
            crate::planning::regional::MemoryImplementationCandidate::RegisterBank => {
                if !module
                    .memory_write_ports()
                    .iter()
                    .any(|port| port.memory == id)
                {
                    return Err(crate::SynthError::unsupported(format!(
                        "memory '{}' has no writable storage implementation",
                        module.name_str(memory.name)
                    )));
                }
            }
            crate::planning::regional::MemoryImplementationCandidate::Macro(cell_index) => {
                let cell = target_cells.get(cell_index as usize).ok_or_else(|| {
                    crate::SynthError::invariant(
                        "selected memory macro index is outside the target library",
                    )
                })?;
                if !memory_macro_is_compatible(module, id, cell)? {
                    return Err(crate::SynthError::invariant(format!(
                        "selected memory macro '{}' is incompatible with memory '{}'",
                        cell.name(),
                        module.name_str(memory.name)
                    )));
                }
            }
        }
    }
    Ok(())
}

pub(crate) fn compatible_memory_macros(
    module: &word::WordModule,
    memory: word::MemoryId,
    target_cells: &TargetCellSet,
) -> Result<Vec<u32>, crate::SynthError> {
    target_cells
        .synthesis_cells()
        .filter(|(_, cell)| cell.memory().is_some())
        .filter_map(
            |(index, cell)| match memory_macro_is_compatible(module, memory, cell) {
                Ok(true) => Some(u32::try_from(index).map_err(|_| {
                    crate::SynthError::capacity(
                        "memory macro target-cell index exceeds 32-bit capacity",
                    )
                })),
                Ok(false) => None,
                Err(error) => Some(Err(error)),
            },
        )
        .collect()
}

pub(crate) fn register_bank_is_supported(
    module: &word::WordModule,
    memory: word::MemoryId,
) -> bool {
    let mut writes = module
        .memory_write_ports()
        .iter()
        .filter(|port| port.memory == memory);
    let Some(first) = writes.next() else {
        return false;
    };
    writes.all(|write| same_clock(module, first.clock, write.clock))
}

fn memory_macro_is_compatible(
    module: &word::WordModule,
    memory_id: word::MemoryId,
    cell: TargetCellRef<'_>,
) -> Result<bool, crate::SynthError> {
    let Some(target) = cell.memory() else {
        return Ok(false);
    };
    let memory = module.memory(memory_id).ok_or_else(|| {
        crate::SynthError::invariant("memory-macro compatibility references an unknown memory")
    })?;
    let reads = module
        .memory_read_ports()
        .iter()
        .filter(|port| port.memory == memory_id);
    let writes = module
        .memory_write_ports()
        .iter()
        .filter(|port| port.memory == memory_id);
    let read_count = reads.clone().count();
    let write_count = writes.clone().count();
    if target.depth != memory.depth.get()
        || target.word_width != memory.element_type.width()
        || target.read_ports.len() != read_count
        || target.write_ports.len() != write_count
        || (target.kind == opto_library::TargetMemoryKind::Rom && write_count != 0)
        || write_count > 1
    {
        return Ok(false);
    }
    for (source, target) in reads.clone().zip(&target.read_ports) {
        if module
            .value(source.address)
            .is_none_or(|value| value.ty.width() as usize != target.address_pins.len())
            || target.data_pins.len() != memory.element_type.width() as usize
            || !read_timing_matches(source, target)
            || source.read_during_write != target_read_during_write(target.read_during_write)
        {
            return Ok(false);
        }
    }
    for (source, target) in writes.clone().zip(&target.write_ports) {
        if module
            .value(source.address)
            .is_none_or(|value| value.ty.width() as usize != target.address_pins.len())
            || module
                .value(source.data)
                .is_none_or(|value| value.ty.width() as usize != target.data_pins.len())
            || !clock_matches(source.clock, &target.clock)
            || !enable_matches(source.enable, target.enable.as_ref())
            || !mask_matches(module, source.mask, target)
        {
            return Ok(false);
        }
    }
    Ok(macro_binding::bindings_are_consistent(
        module, reads, writes, target,
    ))
}

fn read_timing_matches(
    source: &word::MemoryReadPort,
    target: &opto_library::TargetMemoryReadPort,
) -> bool {
    match (source.timing, target.clock.as_ref()) {
        (word::MemoryReadTiming::Asynchronous, None) => target.enable.is_none(),
        (
            word::MemoryReadTiming::Synchronous {
                clock,
                enable,
                disabled,
            },
            Some(target_clock),
        ) => {
            clock_matches(clock, target_clock)
                && enable_matches(enable, target.enable.as_ref())
                && disabled == target_disabled_read(target.disabled)
        }
        (word::MemoryReadTiming::Asynchronous, Some(_))
        | (word::MemoryReadTiming::Synchronous { .. }, None) => false,
    }
}

fn clock_matches(source: word::MemoryClock, target: &opto_library::TargetMemoryClock) -> bool {
    matches!(
        (source.edge, target.edge),
        (word::Edge::Pos, TargetMemoryEdge::Rising) | (word::Edge::Neg, TargetMemoryEdge::Falling)
    )
}

fn enable_matches(
    source: Option<word::Enable>,
    target: Option<&opto_library::TargetMemoryEnable>,
) -> bool {
    match (source, target) {
        (None, None) => true,
        (Some(source), Some(target)) => source.active_high == target.active_high,
        (None, Some(_)) | (Some(_), None) => false,
    }
}

fn mask_matches(
    module: &word::WordModule,
    source: Option<word::MemoryWriteMask>,
    target: &opto_library::TargetMemoryWritePort,
) -> bool {
    match source {
        None => target.mask_pins.is_empty() && target.mask_granularity == 0,
        Some(source) => {
            source.granularity.get() == target.mask_granularity
                && source.active_high == target.mask_active_high
                && module
                    .value(source.value)
                    .is_some_and(|value| value.ty.width() as usize == target.mask_pins.len())
        }
    }
}

const fn target_disabled_read(value: TargetMemoryDisabledRead) -> word::DisabledRead {
    match value {
        TargetMemoryDisabledRead::Hold => word::DisabledRead::Hold,
        TargetMemoryDisabledRead::Undefined => word::DisabledRead::Undefined,
    }
}

const fn target_read_during_write(value: TargetMemoryReadDuringWrite) -> word::ReadDuringWrite {
    match value {
        TargetMemoryReadDuringWrite::OldData => word::ReadDuringWrite::OldData,
        TargetMemoryReadDuringWrite::NewData => word::ReadDuringWrite::NewData,
        TargetMemoryReadDuringWrite::NoChange => word::ReadDuringWrite::NoChange,
        TargetMemoryReadDuringWrite::Undefined => word::ReadDuringWrite::Undefined,
    }
}

fn materialize_macro(
    module: &mut word::WordModule,
    memory: &word::Memory,
    reads: &[word::MemoryReadPort],
    writes: &[word::MemoryWritePort],
    cell: TargetCellRef<'_>,
) -> Result<(), crate::SynthError> {
    let target = cell.memory().ok_or_else(|| {
        crate::SynthError::invariant("selected memory macro has no memory contract")
    })?;
    let mut connections = BTreeMap::<String, (word::ValueId, word::SourceSpan)>::new();
    for (source, target) in reads.iter().zip(&target.read_ports) {
        bind_value_bits(
            module,
            &mut connections,
            &target.address_pins,
            source.address,
            &source.source,
        )?;
        for (bit, pin) in target.data_pins.iter().enumerate() {
            let bit = u32::try_from(bit)
                .map_err(|_| crate::SynthError::capacity("memory macro data width"))?;
            let value = module
                .read_signal_slice(source.data, bit, 1, source.source.clone())
                .map_err(crate::SynthError::from)?;
            bind_macro_pin(module, &mut connections, pin, value, &source.source)?;
        }
        if let (word::MemoryReadTiming::Synchronous { clock, enable, .. }, Some(target_clock)) =
            (source.timing, target.clock.as_ref())
        {
            bind_macro_pin(
                module,
                &mut connections,
                &target_clock.pin,
                clock.value,
                &source.source,
            )?;
            if let (Some(enable), Some(target_enable)) = (enable, target.enable.as_ref()) {
                bind_macro_pin(
                    module,
                    &mut connections,
                    &target_enable.pin,
                    enable.value,
                    &source.source,
                )?;
            }
        }
    }
    for (source, target) in writes.iter().zip(&target.write_ports) {
        bind_value_bits(
            module,
            &mut connections,
            &target.address_pins,
            source.address,
            &source.source,
        )?;
        bind_value_bits(
            module,
            &mut connections,
            &target.data_pins,
            source.data,
            &source.source,
        )?;
        bind_macro_pin(
            module,
            &mut connections,
            &target.clock.pin,
            source.clock.value,
            &source.source,
        )?;
        if let (Some(enable), Some(target_enable)) = (source.enable, target.enable.as_ref()) {
            bind_macro_pin(
                module,
                &mut connections,
                &target_enable.pin,
                enable.value,
                &source.source,
            )?;
        }
        if let (Some(mask), false) = (source.mask, target.mask_pins.is_empty()) {
            bind_value_bits(
                module,
                &mut connections,
                &target.mask_pins,
                mask.value,
                &source.source,
            )?;
        }
    }
    let base = module.name_str(memory.name).to_string();
    let name = unique_name(
        &base,
        |candidate| module.instance_id(candidate).is_none(),
        "memory instance name suffix space is exhausted",
    )?;
    module
        .add_instance(
            name,
            cell.name(),
            connections
                .into_iter()
                .map(|(pin, (value, source))| (pin, value, source))
                .collect(),
            memory.source.clone(),
        )
        .map_err(crate::SynthError::from)?;
    Ok(())
}

fn bind_value_bits(
    module: &mut word::WordModule,
    connections: &mut BTreeMap<String, (word::ValueId, word::SourceSpan)>,
    pins: &[String],
    value: word::ValueId,
    source: &word::SourceSpan,
) -> Result<(), crate::SynthError> {
    for (bit, pin) in pins.iter().enumerate() {
        let bit = u32::try_from(bit)
            .map_err(|_| crate::SynthError::capacity("memory macro binding width"))?;
        if connections.get(pin).is_some_and(|(existing, _)| {
            extracted_bit_source(module, *existing) == Some((value, bit))
        }) {
            continue;
        }
        let value = module
            .extract(value, bit, 1, source.clone())
            .map_err(crate::SynthError::from)?;
        bind_macro_pin(module, connections, pin, value, source)?;
    }
    Ok(())
}

fn extracted_bit_source(
    module: &word::WordModule,
    value: word::ValueId,
) -> Option<(word::ValueId, u32)> {
    let word::ValueKind::Operation(operation) = module.value(value)?.kind else {
        return None;
    };
    let word::OpKind::Extract { value, lsb, width } = module.operation(operation)?.kind else {
        return None;
    };
    (width.get() == 1).then_some((value, lsb))
}

fn bind_macro_pin(
    module: &word::WordModule,
    connections: &mut BTreeMap<String, (word::ValueId, word::SourceSpan)>,
    pin: &str,
    value: word::ValueId,
    source: &word::SourceSpan,
) -> Result<(), crate::SynthError> {
    if let Some((previous, _)) = connections.get(pin) {
        if *previous == value
            || module
                .value(*previous)
                .zip(module.value(value))
                .is_some_and(|(left, right)| left.ty == right.ty && left.kind == right.kind)
        {
            return Ok(());
        }
        return Err(crate::SynthError::invariant(format!(
            "memory macro pin '{pin}' is bound to conflicting source values"
        )));
    }
    connections.insert(pin.to_string(), (value, source.clone()));
    Ok(())
}

fn same_clock(
    module: &word::WordModule,
    left: word::MemoryClock,
    right: word::MemoryClock,
) -> bool {
    left.edge == right.edge
        && (left.value == right.value
            || module
                .value(left.value)
                .zip(module.value(right.value))
                .is_some_and(|(left, right)| left.ty == right.ty && left.kind == right.kind))
}

fn materialize_storage(
    module: &mut word::WordModule,
    memory: &word::Memory,
    writes: &[word::MemoryWritePort],
) -> Result<Bank, crate::SynthError> {
    let clock = writes
        .first()
        .expect("preflight requires a memory write port")
        .clock;
    let name = module.name_str(memory.name).to_string();
    let depth = memory.depth.get() as usize;
    let mut signals = Vec::with_capacity(depth);
    let mut words = Vec::with_capacity(depth);
    for index in 0..depth {
        let name = unique_name(
            &format!("{name}${index}"),
            |candidate| module.signal_id(candidate).is_none(),
            "memory signal name suffix space is exhausted",
        )?;
        let signal = module
            .add_register_signal(name, memory.element_type, memory.source.clone())
            .map_err(crate::SynthError::from)?;
        let value = module
            .read_signal(signal, memory.source.clone())
            .map_err(crate::SynthError::from)?;
        signals.push(signal);
        words.push(value);
    }

    let mut next = Vec::with_capacity(depth);
    for (index, (&signal, &old)) in signals.iter().zip(&words).enumerate() {
        let mut value = old;
        for port in writes {
            let selected = address_match(module, port.address, index, &port.source)?;
            let enabled = port
                .enable
                .map(|enable| normalize_enable(module, enable, &port.source))
                .transpose()?;
            let condition = and(module, selected, enabled, &port.source)?;
            let data = apply_mask(module, value, port.data, port.mask, &port.source)?;
            value = module
                .mux(condition, data, value, port.source.clone())
                .map_err(crate::SynthError::from)?;
        }
        let q = module
            .register(
                word::RegisterOp {
                    name: module.signal(signal).and_then(|signal| signal.name),
                    d: value,
                    clock: clock.value,
                    edge: clock.edge,
                    enable: None,
                    resets: Vec::new(),
                },
                memory.source.clone(),
            )
            .map_err(crate::SynthError::from)?;
        module
            .connect(word::LValue::signal(signal), q, memory.source.clone())
            .map_err(crate::SynthError::from)?;
        next.push(value);
    }
    Ok(Bank { words, next })
}

fn materialize_read(
    module: &mut word::WordModule,
    bank: &Bank,
    writes: &[word::MemoryWritePort],
    read: word::MemoryReadPort,
) -> Result<(), crate::SynthError> {
    let value = match read.timing {
        word::MemoryReadTiming::Asynchronous => {
            select_word(module, read.address, &bank.words, &read.source)?
        }
        word::MemoryReadTiming::Synchronous {
            clock,
            enable,
            disabled,
        } => {
            let same_clock_writes = writes
                .first()
                .is_some_and(|write| same_clock(module, write.clock, clock));
            let words =
                if read.read_during_write == word::ReadDuringWrite::NewData && same_clock_writes {
                    &bank.next
                } else {
                    &bank.words
                };
            let read_enable = enable
                .map(|enable| normalize_enable(module, enable, &read.source))
                .transpose()?;
            let mut d = select_word(module, read.address, words, &read.source)?;
            let mut effective_enable = match disabled {
                word::DisabledRead::Hold => enable,
                word::DisabledRead::Undefined => {
                    if let Some(enable) = read_enable {
                        let unknown = unknown_value(module, value_type(module, d)?, &read.source)?;
                        d = module
                            .mux(enable, d, unknown, read.source.clone())
                            .map_err(crate::SynthError::from)?;
                    }
                    None
                }
            };
            if same_clock_writes
                && matches!(
                    read.read_during_write,
                    word::ReadDuringWrite::NoChange | word::ReadDuringWrite::Undefined
                )
                && let Some(mut collision) = read_collision(module, &read, writes)?
            {
                collision = and(module, collision, read_enable, &read.source)?;
                match read.read_during_write {
                    word::ReadDuringWrite::NoChange => {
                        let no_collision = module
                            .unary(word::UnaryOp::LogicalNot, collision, read.source.clone())
                            .map_err(crate::SynthError::from)?;
                        effective_enable = Some(match effective_enable {
                            Some(enable) => {
                                let enable = normalize_enable(module, enable, &read.source)?;
                                word::Enable {
                                    value: and(module, enable, Some(no_collision), &read.source)?,
                                    active_high: true,
                                }
                            }
                            None => word::Enable {
                                value: no_collision,
                                active_high: true,
                            },
                        });
                    }
                    word::ReadDuringWrite::Undefined => {
                        let unknown = unknown_value(module, value_type(module, d)?, &read.source)?;
                        d = module
                            .mux(collision, unknown, d, read.source.clone())
                            .map_err(crate::SynthError::from)?;
                    }
                    word::ReadDuringWrite::OldData | word::ReadDuringWrite::NewData => {
                        unreachable!("read-during-write mode was filtered above")
                    }
                }
            }
            module
                .register(
                    word::RegisterOp {
                        name: module.signal(read.data).and_then(|signal| signal.name),
                        d,
                        clock: clock.value,
                        edge: clock.edge,
                        enable: effective_enable,
                        resets: Vec::new(),
                    },
                    read.source.clone(),
                )
                .map_err(crate::SynthError::from)?
        }
    };
    module
        .connect(word::LValue::signal(read.data), value, read.source)
        .map_err(crate::SynthError::from)
}

fn read_collision(
    module: &mut word::WordModule,
    read: &word::MemoryReadPort,
    writes: &[word::MemoryWritePort],
) -> Result<Option<word::ValueId>, crate::SynthError> {
    let mut collision = None;
    for write in writes {
        let selected = module
            .binary(
                word::BinaryOp::Eq,
                read.address,
                write.address,
                read.source.clone(),
            )
            .map_err(crate::SynthError::from)?;
        let enabled = write
            .enable
            .map(|enable| normalize_enable(module, enable, &write.source))
            .transpose()?;
        let selected = and(module, selected, enabled, &read.source)?;
        let mask = write
            .mask
            .map(|mask| mask_active(module, mask, &write.source))
            .transpose()?;
        let selected = and(module, selected, mask, &read.source)?;
        collision = Some(or(module, collision, selected, &read.source)?);
    }
    Ok(collision)
}

fn mask_active(
    module: &mut word::WordModule,
    mask: word::MemoryWriteMask,
    source: &word::SourceSpan,
) -> Result<word::ValueId, crate::SynthError> {
    let ty = value_type(module, mask.value)?;
    let inactive = if mask.active_high {
        BitVal::Zero
    } else {
        BitVal::One
    };
    let bits = vec![inactive; ty.width() as usize];
    let inactive = module
        .constant(constant_bits(bits)?, ty, source.clone())
        .map_err(crate::SynthError::from)?;
    module
        .binary(word::BinaryOp::Ne, mask.value, inactive, source.clone())
        .map_err(crate::SynthError::from)
}

fn select_word(
    module: &mut word::WordModule,
    address: word::ValueId,
    words: &[word::ValueId],
    source: &word::SourceSpan,
) -> Result<word::ValueId, crate::SynthError> {
    let ty = module
        .value(words[0])
        .ok_or_else(|| crate::SynthError::invariant("memory word value disappeared"))?
        .ty;
    let mut selected = unknown_value(module, ty, source)?;
    for (index, &word) in words.iter().enumerate() {
        let condition = address_match(module, address, index, source)?;
        selected = module
            .mux(condition, word, selected, source.clone())
            .map_err(crate::SynthError::from)?;
    }
    Ok(selected)
}

fn unknown_value(
    module: &mut word::WordModule,
    ty: word::WordType,
    source: &word::SourceSpan,
) -> Result<word::ValueId, crate::SynthError> {
    module
        .constant(
            ConstBits::from_bits(vec![BitVal::X; ty.width() as usize])
                .map_err(crate::SynthError::from)?,
            ty,
            source.clone(),
        )
        .map_err(crate::SynthError::from)
}

fn address_match(
    module: &mut word::WordModule,
    address: word::ValueId,
    index: usize,
    source: &word::SourceSpan,
) -> Result<word::ValueId, crate::SynthError> {
    let ty = module
        .value(address)
        .ok_or_else(|| crate::SynthError::invariant("memory address value disappeared"))?
        .ty;
    let mut bits = Vec::with_capacity(ty.width() as usize);
    for bit in (0..ty.width()).rev() {
        bits.push(if index.checked_shr(bit).unwrap_or(0) & 1 == 0 {
            BitVal::Zero
        } else {
            BitVal::One
        });
    }
    let constant = module
        .constant(constant_bits(bits)?, ty, source.clone())
        .map_err(crate::SynthError::from)?;
    module
        .binary(word::BinaryOp::Eq, address, constant, source.clone())
        .map_err(crate::SynthError::from)
}

fn normalize_enable(
    module: &mut word::WordModule,
    enable: word::Enable,
    source: &word::SourceSpan,
) -> Result<word::ValueId, crate::SynthError> {
    if enable.active_high {
        Ok(enable.value)
    } else {
        module
            .unary(word::UnaryOp::LogicalNot, enable.value, source.clone())
            .map_err(crate::SynthError::from)
    }
}

fn and(
    module: &mut word::WordModule,
    left: word::ValueId,
    right: Option<word::ValueId>,
    source: &word::SourceSpan,
) -> Result<word::ValueId, crate::SynthError> {
    match right {
        Some(right) => module
            .binary(word::BinaryOp::LogicalAnd, left, right, source.clone())
            .map_err(crate::SynthError::from),
        None => Ok(left),
    }
}

fn or(
    module: &mut word::WordModule,
    left: Option<word::ValueId>,
    right: word::ValueId,
    source: &word::SourceSpan,
) -> Result<word::ValueId, crate::SynthError> {
    match left {
        Some(left) => module
            .binary(word::BinaryOp::LogicalOr, left, right, source.clone())
            .map_err(crate::SynthError::from),
        None => Ok(right),
    }
}

fn apply_mask(
    module: &mut word::WordModule,
    old: word::ValueId,
    data: word::ValueId,
    mask: Option<word::MemoryWriteMask>,
    source: &word::SourceSpan,
) -> Result<word::ValueId, crate::SynthError> {
    let Some(mask) = mask else {
        return Ok(data);
    };
    let width = value_type(module, data)?.width();
    let mut bits = Vec::with_capacity(width as usize);
    for bit in (0..width).rev() {
        let select = module
            .extract(mask.value, bit / mask.granularity.get(), 1, source.clone())
            .map_err(crate::SynthError::from)?;
        let select = if mask.active_high {
            select
        } else {
            module
                .unary(word::UnaryOp::LogicalNot, select, source.clone())
                .map_err(crate::SynthError::from)?
        };
        let data = module
            .extract(data, bit, 1, source.clone())
            .map_err(crate::SynthError::from)?;
        let old = module
            .extract(old, bit, 1, source.clone())
            .map_err(crate::SynthError::from)?;
        bits.push(
            module
                .mux(select, data, old, source.clone())
                .map_err(crate::SynthError::from)?,
        );
    }
    let value = module
        .concat(bits, source.clone())
        .map_err(crate::SynthError::from)?;
    let target = value_type(module, data)?;
    if value_type(module, value)? == target {
        Ok(value)
    } else {
        module
            .cast(word::CastKind::ZeroExtend, value, target, source.clone())
            .map_err(crate::SynthError::from)
    }
}

fn value_type(
    module: &word::WordModule,
    value: word::ValueId,
) -> Result<word::WordType, crate::SynthError> {
    Ok(module
        .value(value)
        .ok_or_else(|| crate::SynthError::invariant("value is not in the module arena"))?
        .ty)
}

fn constant_bits(bits: Vec<BitVal>) -> Result<ConstBits, crate::SynthError> {
    ConstBits::from_bits(bits).map_err(|error| crate::SynthError::capacity(error.to_string()))
}

#[cfg(test)]
mod tests;
