// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Packed, immutable storage for synthesis-facing target cells.
//!
//! Cell records retain strings and repeated characterization in shared arenas.
//! Public access is through lifetime-bound views in the `access` submodule;
//! compact IDs never cross the arena boundary.

use super::{
    TargetCell, TargetCellUsage, TargetClockGateKind, TargetClockGateRole, TargetMemory,
    TargetNextStateType, TargetPinDirection, TargetSequentialKind, TargetTimingType,
};
use crate::{
    ArcDelayModel, LibraryError, LibraryFingerprint, LookupTable, PinReceiverCapacitanceModel,
    TimingEdge, fingerprint_serializable,
};
use opto_core::DenseId;
use serde::Serialize;
use std::collections::BTreeSet;
use std::fmt;
use std::sync::{Arc, OnceLock};

mod access;
mod builder;
mod fingerprint;

pub use access::{
    BooleanFunctionKind, BooleanFunctionRef, TargetCellRef, TargetPinRef, TargetSequentialRef,
    TargetTimingArcRef,
};
use builder::ArenaBuilder;
use fingerprint::{FingerprintCells, topology_schema_bytes};

macro_rules! dense_id {
    ($visibility:vis $name:ident, $tag:ident, $kind:literal) => {
        enum $tag {}

        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
        #[repr(transparent)]
        $visibility struct $name(DenseId<$tag>);

        impl $name {
            fn new(index: usize) -> Result<Self, LibraryError> {
                DenseId::from_index(index)
                    .map(Self)
                    .map_err(|_| LibraryError::ArenaCapacity { arena: $kind })
            }

            fn slot(self) -> usize {
                self.0.index()
            }
        }
    };
}

dense_id!(TargetPinId, PinTag, "target pins");
dense_id!(TargetTimingArcId, TimingArcTag, "target timing arcs");
dense_id!(
    TargetSequentialId,
    SequentialTag,
    "target sequential declarations"
);
dense_id!(TargetNameId, NameTag, "target-library names");
dense_id!(
    TargetFunctionId,
    FunctionTag,
    "target-library Boolean functions"
);
dense_id!(TargetTableId, TableTag, "target-library lookup tables");
dense_id!(
    TargetDelayModelId,
    DelayModelTag,
    "target-library delay models"
);

enum LocalCellTag {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
struct LocalCellId(DenseId<LocalCellTag>);

impl LocalCellId {
    fn new(index: usize) -> Result<Self, LibraryError> {
        DenseId::from_index(index)
            .map(Self)
            .map_err(|_| LibraryError::ArenaCapacity {
                arena: "target cells",
            })
    }

    fn index(self) -> usize {
        self.0.index()
    }
}
dense_id!(
    TargetReceiverModelId,
    ReceiverModelTag,
    "target-library receiver models"
);

#[derive(Debug, Clone, Copy)]
struct ArenaRange {
    start: u32,
    len: u32,
}

impl ArenaRange {
    fn append<T>(
        arena: &mut Vec<T>,
        values: impl IntoIterator<Item = T>,
    ) -> Result<Self, LibraryError> {
        let start = u32::try_from(arena.len())
            .map_err(|_| LibraryError::ArenaCapacity { arena: "range" })?;
        arena.extend(values);
        let end = u32::try_from(arena.len())
            .map_err(|_| LibraryError::ArenaCapacity { arena: "range" })?;
        Ok(Self {
            start,
            len: end - start,
        })
    }

    fn indices(self) -> std::ops::Range<usize> {
        self.start as usize..(self.start + self.len) as usize
    }
}

#[derive(Debug, Clone, Copy)]
struct TextRange {
    start: u32,
    len: u32,
}

#[derive(Debug)]
struct CellRecord {
    name: TargetNameId,
    area: Option<f64>,
    dont_use: bool,
    usage: TargetCellUsage,
    clock_gate: Option<TargetClockGateKind>,
    pins: ArenaRange,
    sequential: ArenaRange,
    memory: Option<u32>,
}

#[derive(Debug)]
struct PinRecord {
    name: TargetNameId,
    direction: TargetPinDirection,
    function: Option<TargetFunctionId>,
    three_state: Option<TargetFunctionId>,
    capacitance: Option<f64>,
    rise_capacitance: Option<f64>,
    fall_capacitance: Option<f64>,
    receiver_capacitance: Option<TargetReceiverModelId>,
    fanout_load: Option<f64>,
    next_state_type: Option<TargetNextStateType>,
    clock_gate_role: Option<TargetClockGateRole>,
    timing_arcs: ArenaRange,
}

#[derive(Debug)]
struct TimingArcRecord {
    related_pin: TargetNameId,
    timing_type: TargetTimingType,
    timing_sense: crate::TimingSense,
    delay_model: Option<TargetDelayModelId>,
    rise_constraint: Option<TargetTableId>,
    fall_constraint: Option<TargetTableId>,
}

#[derive(Debug)]
struct SequentialRecord {
    kind: TargetSequentialKind,
    state_variables: ArenaRange,
    clocked_on: Option<TargetFunctionId>,
    next_state: Option<TargetFunctionId>,
    enable: Option<TargetFunctionId>,
    clear: Option<TargetFunctionId>,
    preset: Option<TargetFunctionId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum FunctionNode {
    Const(bool),
    Pin(TargetNameId),
    Not(TargetFunctionId),
    And(TargetFunctionId, TargetFunctionId),
    Or(TargetFunctionId, TargetFunctionId),
    Xor(TargetFunctionId, TargetFunctionId),
    Imp(TargetFunctionId, TargetFunctionId),
    Iff(TargetFunctionId, TargetFunctionId),
    Cond(TargetFunctionId, TargetFunctionId, TargetFunctionId),
}

#[derive(Debug)]
struct TargetCellArena {
    text: Box<str>,
    names: Box<[TextRange]>,
    cells: Box<[CellRecord]>,
    pins: Box<[PinRecord]>,
    timing_arcs: Box<[TimingArcRecord]>,
    sequential: Box<[SequentialRecord]>,
    state_variables: Box<[TargetNameId]>,
    functions: Box<[FunctionNode]>,
    tables: Box<[LookupTable]>,
    delay_models: Box<[ArcDelayModel]>,
    receiver_models: Box<[PinReceiverCapacitanceModel]>,
    memories: Box<[TargetMemory]>,
}

impl TargetCellArena {
    fn name(&self, id: TargetNameId) -> &str {
        let range = self.names[id.slot()];
        &self.text[range.start as usize..(range.start + range.len) as usize]
    }

    fn resident_memory_bytes(&self) -> usize {
        let dynamic_models =
            crate::serialized_size(&(&self.tables, &self.delay_models, &self.receiver_models));
        let memory_payload = crate::serialized_size(&self.memories);
        opto_core::resident::allocation_bytes(self.text.len())
            .saturating_add(opto_core::resident::slice_bytes::<TextRange>(
                self.names.len(),
            ))
            .saturating_add(opto_core::resident::slice_bytes::<CellRecord>(
                self.cells.len(),
            ))
            .saturating_add(opto_core::resident::slice_bytes::<PinRecord>(
                self.pins.len(),
            ))
            .saturating_add(opto_core::resident::slice_bytes::<TimingArcRecord>(
                self.timing_arcs.len(),
            ))
            .saturating_add(opto_core::resident::slice_bytes::<SequentialRecord>(
                self.sequential.len(),
            ))
            .saturating_add(opto_core::resident::slice_bytes::<TargetNameId>(
                self.state_variables.len(),
            ))
            .saturating_add(opto_core::resident::slice_bytes::<FunctionNode>(
                self.functions.len(),
            ))
            .saturating_add(opto_core::resident::slice_bytes::<LookupTable>(
                self.tables.len(),
            ))
            .saturating_add(opto_core::resident::slice_bytes::<ArcDelayModel>(
                self.delay_models.len(),
            ))
            .saturating_add(opto_core::resident::slice_bytes::<
                PinReceiverCapacitanceModel,
            >(self.receiver_models.len()))
            .saturating_add(opto_core::resident::slice_bytes::<TargetMemory>(
                self.memories.len(),
            ))
            // The encoded model payload conservatively covers every nested
            // Arc-backed table axis, waveform, and value allocation.
            .saturating_add(opto_core::resident::allocation_bytes(dynamic_models))
            .saturating_add(opto_core::resident::allocation_bytes(memory_payload))
    }
}

#[derive(Debug, Clone)]
struct TargetCellGroup {
    arena: Arc<TargetCellArena>,
    selected: Option<Arc<[LocalCellId]>>,
    dont_use: Arc<[LocalCellId]>,
}

impl TargetCellGroup {
    fn len(&self) -> usize {
        self.selected
            .as_deref()
            .map_or(self.arena.cells.len(), <[LocalCellId]>::len)
    }

    fn local(&self, index: usize) -> Option<LocalCellId> {
        match &self.selected {
            Some(selected) => selected.get(index).copied(),
            None => LocalCellId::new(index).ok(),
        }
    }

    fn is_dont_use(&self, local: LocalCellId) -> bool {
        self.dont_use.binary_search(&local).is_ok()
    }
}

/// A sealed, shareable target-library view.
///
/// Construction DTOs are flattened once; retained cells, pins, arcs,
/// sequential declarations, names, functions, and table/model headers live in
/// dense immutable arenas.
#[derive(Debug, Clone)]
pub struct TargetCellSet {
    groups: Arc<[TargetCellGroup]>,
    offsets: Arc<[u32]>,
    fingerprint: Arc<OnceLock<LibraryFingerprint>>,
    topology_schema: Arc<OnceLock<Arc<[u8]>>>,
    synthesis_validation: Arc<OnceLock<Result<(), String>>>,
}

impl Default for TargetCellSet {
    fn default() -> Self {
        Self {
            groups: Arc::from([]),
            offsets: Arc::from([]),
            fingerprint: Arc::new(OnceLock::new()),
            topology_schema: Arc::new(OnceLock::new()),
            synthesis_validation: Arc::new(OnceLock::new()),
        }
    }
}

impl TargetCellSet {
    pub(crate) fn canonical_resident_memory_bytes(&self) -> usize {
        let mut arenas = BTreeSet::new();
        let canonical = self.groups.iter().fold(0usize, |bytes, group| {
            let address = Arc::as_ptr(&group.arena) as usize;
            if arenas.insert(address) {
                bytes.saturating_add(group.arena.resident_memory_bytes())
            } else {
                bytes
            }
        });
        canonical.saturating_add(self.retained_view_memory_bytes())
    }

    pub(crate) fn retained_view_memory_bytes(&self) -> usize {
        let overlays = self.groups.iter().fold(0usize, |bytes, group| {
            bytes
                .saturating_add(group.selected.as_ref().map_or(0, |selected| {
                    opto_core::resident::slice_bytes::<LocalCellId>(selected.len())
                }))
                .saturating_add(opto_core::resident::slice_bytes::<LocalCellId>(
                    group.dont_use.len(),
                ))
        });
        let validation_error = self
            .synthesis_validation
            .get()
            .and_then(|result| result.as_ref().err())
            .map_or(0, |error| {
                opto_core::resident::allocation_bytes(error.len())
            });
        opto_core::resident::slice_bytes::<TargetCellGroup>(self.groups.len())
            .saturating_add(opto_core::resident::slice_bytes::<u32>(self.offsets.len()))
            .saturating_add(opto_core::resident::allocation_bytes(std::mem::size_of::<
                OnceLock<LibraryFingerprint>,
            >()))
            .saturating_add(opto_core::resident::allocation_bytes(std::mem::size_of::<
                OnceLock<Result<(), String>>,
            >()))
            .saturating_add(overlays)
            .saturating_add(validation_error)
    }

    pub(crate) fn first_by_name(sets: impl IntoIterator<Item = Self>) -> Self {
        let mut names = BTreeSet::new();
        let mut groups = Vec::new();
        for set in sets {
            for group in set.groups.iter() {
                let selected = (0..group.len())
                    .filter_map(|index| {
                        let local = group.local(index)?;
                        names
                            .insert(
                                group
                                    .arena
                                    .name(group.arena.cells[local.index()].name)
                                    .to_string(),
                            )
                            .then_some(local)
                    })
                    .collect::<Vec<_>>();
                if selected.is_empty() {
                    continue;
                }
                if selected.len() == group.len() {
                    groups.push(group.clone());
                    continue;
                }
                let dont_use = group
                    .dont_use
                    .iter()
                    .copied()
                    .filter(|local| selected.binary_search(local).is_ok())
                    .collect::<Vec<_>>()
                    .into();
                groups.push(TargetCellGroup {
                    arena: Arc::clone(&group.arena),
                    selected: Some(selected.into()),
                    dont_use,
                });
            }
        }
        Self::from_group_list(groups).expect("selected target cells retain valid global IDs")
    }

    pub(crate) fn with_dont_use(
        &self,
        mut predicate: impl FnMut(&str) -> bool,
    ) -> Result<(Self, usize, usize), LibraryError> {
        let mut changed = 0usize;
        let mut matched = 0usize;
        let mut groups = Vec::with_capacity(self.groups.len());
        for group in self.groups.iter() {
            let mut dont_use = None;
            for index in 0..group.len() {
                let local = group
                    .local(index)
                    .expect("sealed target-cell group index is valid");
                let record = &group.arena.cells[local.index()];
                if !predicate(group.arena.name(record.name)) {
                    continue;
                }
                matched += 1;
                if record.dont_use || group.is_dont_use(local) {
                    continue;
                }
                let dont_use = dont_use.get_or_insert_with(|| group.dont_use.to_vec());
                let position = dont_use
                    .binary_search(&local)
                    .expect_err("new dont-use cell is absent from the overlay");
                dont_use.insert(position, local);
                changed += 1;
            }
            groups.push(dont_use.map_or_else(
                || group.clone(),
                |dont_use| TargetCellGroup {
                    arena: Arc::clone(&group.arena),
                    selected: group.selected.clone(),
                    dont_use: dont_use.into(),
                },
            ));
        }
        if changed == 0 {
            return Ok((self.clone(), matched, 0));
        }
        let mut updated = Self::from_group_list(groups)?;
        updated.synthesis_validation = Arc::clone(&self.synthesis_validation);
        Ok((updated, matched, changed))
    }

    fn from_group_list(groups: Vec<TargetCellGroup>) -> Result<Self, LibraryError> {
        let mut total = 0usize;
        let mut offsets = Vec::with_capacity(groups.len());
        for group in &groups {
            total = total
                .checked_add(group.len())
                .ok_or(LibraryError::ArenaCapacity {
                    arena: "target cells",
                })?;
            offsets.push(
                u32::try_from(total).map_err(|_| LibraryError::ArenaCapacity {
                    arena: "target cells",
                })?,
            );
        }
        Ok(Self {
            groups: groups.into(),
            offsets: offsets.into(),
            fingerprint: Arc::new(OnceLock::new()),
            topology_schema: Arc::new(OnceLock::new()),
            synthesis_validation: Arc::new(OnceLock::new()),
        })
    }

    pub(crate) fn try_from_cells(cells: Vec<TargetCell>) -> Result<Self, LibraryError> {
        if cells.is_empty() {
            return Ok(Self::default());
        }
        let arena = Arc::new(ArenaBuilder::default().seal(cells)?);
        Self::from_group_list(vec![TargetCellGroup {
            arena,
            selected: None,
            dont_use: Arc::from([]),
        }])
    }

    /// Iterates over all target cells in logical library order.
    ///
    /// # Panics
    ///
    /// Panics only if the set's private group offsets disagree with the sealed
    /// arenas; all constructors build and validate those offsets together.
    #[must_use]
    pub fn iter(&self) -> impl Clone + ExactSizeIterator<Item = TargetCellRef<'_>> {
        (0..self.len()).map(|index| {
            self.get(index)
                .expect("target cell iteration stays within the sealed arena")
        })
    }

    /// Iterate the only cells that synthesis is allowed to introduce.
    pub fn synthesis_cells(&self) -> impl Clone + Iterator<Item = (usize, TargetCellRef<'_>)> {
        self.iter()
            .enumerate()
            .filter(|(_, cell)| cell.is_synthesis_eligible())
    }

    #[must_use]
    /// Returns the target cell at a logical sequence index.
    pub fn get(&self, index: usize) -> Option<TargetCellRef<'_>> {
        let group_index = self.offsets.partition_point(|&end| end as usize <= index);
        let group = self.groups.get(group_index)?;
        let base = group_index
            .checked_sub(1)
            .map_or(0, |previous| self.offsets[previous] as usize);
        let local = group.local(index.checked_sub(base)?)?;
        group.arena.cells.get(local.index())?;
        Some(TargetCellRef {
            arena: &group.arena,
            local,
            dont_use: group.is_dont_use(local),
        })
    }

    #[must_use]
    /// Returns `true` when the target set contains no cells.
    pub fn is_empty(&self) -> bool {
        self.offsets.last().copied().unwrap_or(0) == 0
    }

    #[must_use]
    /// Returns the number of target cells across all arena groups.
    pub fn len(&self) -> usize {
        self.offsets.last().copied().unwrap_or(0) as usize
    }

    #[must_use]
    /// Returns a cached semantic fingerprint independent of arena grouping.
    pub fn content_fingerprint(&self) -> LibraryFingerprint {
        *self
            .fingerprint
            .get_or_init(|| fingerprint_serializable(&FingerprintCells(self)))
    }

    pub(crate) fn timing_topology_schema(&self) -> Arc<[u8]> {
        Arc::clone(
            self.topology_schema
                .get_or_init(|| topology_schema_bytes(self)),
        )
    }

    /// Validate the name and index invariants required by every synthesis consumer.
    ///
    /// # Errors
    ///
    /// Returns [`LibraryError::InvalidSynthesisLibrary`] for duplicate/empty
    /// names, invalid function references, malformed sequential/timing ranges,
    /// or an inconsistent memory contract.
    pub fn validate_for_synthesis(&self) -> Result<(), LibraryError> {
        self.synthesis_validation
            .get_or_init(|| self.validate_synthesis_cells())
            .as_ref()
            .map_err(|detail| LibraryError::InvalidSynthesisLibrary {
                detail: detail.clone(),
            })
            .copied()
    }

    fn validate_synthesis_cells(&self) -> Result<(), String> {
        let mut cell_names = BTreeSet::new();
        for cell in self.iter() {
            if cell.name().is_empty() {
                return Err("cell names must not be empty".to_string());
            }
            if !cell_names.insert(cell.name()) {
                return Err(format!("duplicate cell name '{}'", cell.name()));
            }
            if cell.pins().len() > u16::MAX as usize {
                return Err(format!(
                    "cell '{}' exceeds the 16-bit mapped-library pin capacity",
                    cell.name()
                ));
            }
            let mut pin_names = BTreeSet::new();
            for pin in cell.pins() {
                if pin.name().is_empty() {
                    return Err(format!("cell '{}' contains an empty pin name", cell.name()));
                }
                if !pin_names.insert(pin.name()) {
                    return Err(format!(
                        "cell '{}' contains duplicate pin name '{}'",
                        cell.name(),
                        pin.name()
                    ));
                }
            }
            for pin in cell.pins() {
                for arc in pin.timing_arcs() {
                    if !arc.related_pin().is_empty() && !pin_names.contains(arc.related_pin()) {
                        return Err(format!(
                            "cell '{}' pin '{}' timing arc references unknown pin '{}'",
                            cell.name(),
                            pin.name(),
                            arc.related_pin()
                        ));
                    }
                }
            }
            let mut function_names = pin_names;
            for sequential in cell.sequential() {
                function_names.extend(sequential.state_variables());
            }
            for pin in cell.pins() {
                for (field, function) in [
                    ("function", pin.function()),
                    ("three_state", pin.three_state()),
                ] {
                    if let Some(unknown) =
                        function.and_then(|function| function.first_unknown(&function_names))
                    {
                        return Err(format!(
                            "cell '{}' pin '{}' {field} references unknown name '{unknown}'",
                            cell.name(),
                            pin.name()
                        ));
                    }
                }
            }
            for sequential in cell.sequential() {
                for (field, function) in [
                    ("clocked_on", sequential.clocked_on()),
                    ("next_state", sequential.next_state()),
                    ("enable", sequential.enable()),
                    ("clear", sequential.clear()),
                    ("preset", sequential.preset()),
                ] {
                    if let Some(unknown) =
                        function.and_then(|function| function.first_unknown(&function_names))
                    {
                        return Err(format!(
                            "cell '{}' sequential {field} references unknown name '{unknown}'",
                            cell.name()
                        ));
                    }
                }
            }
            validate_memory_contract(cell)?;
        }
        Ok(())
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "the memory contract is a single cross-index invariant over ports, clocks, and masks"
)]
fn validate_memory_contract(cell: TargetCellRef<'_>) -> Result<(), String> {
    let Some(memory) = cell.memory() else {
        return Ok(());
    };
    if memory.depth == 0 || memory.word_width == 0 {
        return Err(format!(
            "cell '{}' has a zero-sized memory contract",
            cell.name()
        ));
    }
    if memory.kind == super::TargetMemoryKind::Rom && !memory.write_ports.is_empty() {
        return Err(format!("ROM cell '{}' declares write ports", cell.name()));
    }
    let address_width = if memory.depth <= 1 {
        0
    } else {
        u32::BITS - (memory.depth - 1).leading_zeros()
    } as usize;
    let pin_direction = |name: &str| {
        cell.pins()
            .find(|pin| pin.name() == name)
            .map(TargetPinRef::direction)
    };
    let check_pins = |names: &[String], direction: TargetPinDirection, role: &str| {
        let mut unique = BTreeSet::new();
        for name in names {
            if !unique.insert(name.as_str()) {
                return Err(format!(
                    "memory cell '{}' repeats {role} pin '{name}'",
                    cell.name()
                ));
            }
            if pin_direction(name) != Some(direction) {
                return Err(format!(
                    "memory cell '{}' {role} pin '{name}' is absent or has the wrong direction",
                    cell.name()
                ));
            }
        }
        Ok(())
    };
    let check_control = |name: &str, role: &str| {
        if pin_direction(name) != Some(TargetPinDirection::Input) {
            return Err(format!(
                "memory cell '{}' {role} pin '{name}' is absent or is not an input",
                cell.name()
            ));
        }
        Ok(())
    };
    for (index, port) in memory.read_ports.iter().enumerate() {
        if port.address_pins.len() != address_width
            || port.data_pins.len() != memory.word_width as usize
        {
            return Err(format!(
                "memory cell '{}' read port {index} does not match its declared shape",
                cell.name()
            ));
        }
        check_pins(
            &port.address_pins,
            TargetPinDirection::Input,
            "read-address",
        )?;
        check_pins(&port.data_pins, TargetPinDirection::Output, "read-data")?;
        if let Some(clock) = &port.clock {
            check_control(&clock.pin, "read-clock")?;
        }
        if let Some(enable) = &port.enable {
            check_control(&enable.pin, "read-enable")?;
        }
        if port.clock.is_none() && port.enable.is_some() {
            return Err(format!(
                "asynchronous memory cell '{}' read port {index} has a synchronous enable",
                cell.name()
            ));
        }
    }
    for (index, port) in memory.write_ports.iter().enumerate() {
        if port.address_pins.len() != address_width
            || port.data_pins.len() != memory.word_width as usize
        {
            return Err(format!(
                "memory cell '{}' write port {index} does not match its declared shape",
                cell.name()
            ));
        }
        check_pins(
            &port.address_pins,
            TargetPinDirection::Input,
            "write-address",
        )?;
        check_pins(&port.data_pins, TargetPinDirection::Input, "write-data")?;
        check_control(&port.clock.pin, "write-clock")?;
        if let Some(enable) = &port.enable {
            check_control(&enable.pin, "write-enable")?;
        }
        let expected_masks = if port.mask_pins.is_empty() {
            if port.mask_granularity != 0 {
                return Err(format!(
                    "unmasked memory cell '{}' write port {index} has mask granularity",
                    cell.name()
                ));
            }
            0
        } else {
            if port.mask_granularity == 0
                || !memory.word_width.is_multiple_of(port.mask_granularity)
            {
                return Err(format!(
                    "memory cell '{}' write port {index} has an invalid mask granularity",
                    cell.name()
                ));
            }
            (memory.word_width / port.mask_granularity) as usize
        };
        if port.mask_pins.len() != expected_masks {
            return Err(format!(
                "memory cell '{}' write port {index} has the wrong mask width",
                cell.name()
            ));
        }
        check_pins(&port.mask_pins, TargetPinDirection::Input, "write-mask")?;
    }
    Ok(())
}

impl From<Vec<TargetCell>> for TargetCellSet {
    fn from(cells: Vec<TargetCell>) -> Self {
        Self::try_from_cells(cells)
            .expect("allocated target cell DTOs fit the dense library arenas")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell(name: &str, area: f64, dont_use: bool) -> TargetCell {
        TargetCell {
            name: name.to_string(),
            area: Some(area),
            dont_use,
            usage: TargetCellUsage::default(),
            pins: Vec::new(),
            sequential: Vec::new(),
            clock_gate: None,
            memory: None,
        }
    }

    #[test]
    fn fingerprint_only_covers_the_effective_selection() {
        let primary = TargetCellSet::from(vec![cell("A", 1.0, false)]);
        let selected = |shadow_area| {
            TargetCellSet::first_by_name([
                primary.clone(),
                TargetCellSet::from(vec![cell("A", shadow_area, false), cell("B", 2.0, false)]),
            ])
        };

        assert_eq!(
            selected(3.0).content_fingerprint(),
            selected(300.0).content_fingerprint()
        );

        let changed = TargetCellSet::first_by_name([
            primary.clone(),
            TargetCellSet::from(vec![cell("A", 3.0, false), cell("B", 4.0, false)]),
        ]);
        assert_ne!(
            selected(3.0).content_fingerprint(),
            changed.content_fingerprint()
        );
    }

    #[test]
    fn fingerprint_is_independent_of_groups_and_overlay_representation() {
        let contiguous = TargetCellSet::from(vec![cell("A", 1.0, false), cell("B", 2.0, false)]);
        let grouped = TargetCellSet::first_by_name([
            TargetCellSet::from(vec![cell("A", 1.0, false)]),
            TargetCellSet::from(vec![cell("B", 2.0, false)]),
        ]);
        assert_eq!(
            contiguous.content_fingerprint(),
            grouped.content_fingerprint()
        );

        let encoded = TargetCellSet::from(vec![cell("A", 1.0, true)]);
        let (overlaid, matched, changed) = TargetCellSet::from(vec![cell("A", 1.0, false)])
            .with_dont_use(|_| true)
            .unwrap();
        assert_eq!((matched, changed), (1, 1));
        assert_eq!(
            encoded.content_fingerprint(),
            overlaid.content_fingerprint()
        );
    }
}
