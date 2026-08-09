// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! The single stable-slot to dense-publication generation boundary.

use super::builder::append_pin;
use super::{
    CellId, ConnectionSignal, MappedError, MappedGenerationId, MappedNetlist, NetId, NetPins,
    PinId, PinLinks,
};

const REMOVED_SLOT: u32 = u32::MAX;

/// Cell-ID translation produced when the final mapped generation is repacked.
///
/// Stable slot IDs are retained throughout optimization. Repacking is a single
/// publication-boundary operation; live cells receive dense IDs and tombstones
/// have no destination.
#[derive(Debug)]
pub struct MappedCellRemap {
    source_generation: MappedGenerationId,
    target_generation: MappedGenerationId,
    cells: Box<[u32]>,
    cell_count: usize,
}

impl MappedCellRemap {
    /// Returns the stable-slot generation consumed by this translation.
    #[must_use]
    pub fn source_generation(&self) -> MappedGenerationId {
        self.source_generation
    }

    /// Returns the dense published generation produced by this translation.
    #[must_use]
    pub fn target_generation(&self) -> MappedGenerationId {
        self.target_generation
    }

    /// Translates a pre-publication cell ID into its dense published ID.
    #[must_use]
    pub fn cell(&self, cell: CellId) -> Option<CellId> {
        let index = *self.cells.get(cell.index())?;
        (index != REMOVED_SLOT).then(|| dense_cell_id(index))
    }

    /// Returns the old stable-slot domain covered by this translation.
    #[must_use]
    pub fn old_cell_slot_count(&self) -> usize {
        self.cells.len()
    }

    /// Returns the number of live cells in the repacked generation.
    #[must_use]
    pub fn cell_count(&self) -> usize {
        self.cell_count
    }
}

struct PublicationPlan {
    source_generation: MappedGenerationId,
    target_generation: MappedGenerationId,
    publication_revision: u64,
    nets: Vec<u32>,
    cells: Vec<u32>,
    pin_count: usize,
}

impl MappedNetlist {
    /// Replaces synthetic cell names with dense `<prefix><n>` publication names.
    ///
    /// Region materialization runs in parallel workers that cannot allocate
    /// global names, so a synthesized cell carries a region-scoped identifier
    /// while it is being optimized. That identifier is long and unstable by
    /// construction, and it must never reach a published netlist, a report, or
    /// a constraint that matches cells by name. Renaming happens here because
    /// publication is the one ordered, single-threaded pass over live cells.
    ///
    /// Names a caller does not classify as synthetic — source instances,
    /// sequential cells named after the state they implement — are preserved.
    /// Dense names continue past the highest `<prefix><n>` already in use, so a
    /// preserved name is never shadowed.
    ///
    /// # Errors
    ///
    /// Returns [`MappedError`] if the netlist is already published or if the
    /// name arena cannot represent the assigned names.
    pub fn assign_publication_names(
        &mut self,
        is_synthetic: impl Fn(&str) -> bool,
        prefix: &str,
    ) -> Result<(), MappedError> {
        if self.published {
            return Err(MappedError::invariant(
                "published mapped netlist cannot be renamed",
            ));
        }
        let mut synthetic = Vec::new();
        // Generated instance names are one-based, matching the convention every
        // other tool in this flow writes and reads.
        let mut next = 1u32;
        for (index, slot) in self.cells.iter().enumerate() {
            if !slot.live {
                continue;
            }
            let name = self.names.resolve(slot.cell.name).unwrap_or("");
            if is_synthetic(name) {
                synthetic.push(index);
                continue;
            }
            if let Some(ordinal) = name
                .strip_prefix(prefix)
                .and_then(|rest| rest.parse::<u32>().ok())
            {
                next = next.max(ordinal.saturating_add(1));
            }
        }
        for index in synthetic {
            let name = self.names.intern(&format!("{prefix}{next}")).map_err(|_| {
                MappedError::invariant("mapped publication name arena is exhausted")
            })?;
            self.cells[index].cell.name = name;
            next = next.checked_add(1).ok_or_else(|| {
                MappedError::invariant("mapped publication name space is exhausted")
            })?;
        }
        Ok(())
    }

    /// Gives every synthetic or unnamed net a dense `<prefix><n>` publication
    /// name.
    ///
    /// This is the net-side counterpart of [`Self::assign_publication_names`]:
    /// materialization either names an internal net after the operation that
    /// produced it or leaves it unnamed, and neither is a stable identity.
    /// Named source nets are preserved.
    ///
    /// # Errors
    ///
    /// Returns [`MappedError`] if the netlist is already published or if the
    /// name arena cannot represent the assigned names.
    pub fn assign_publication_net_names(
        &mut self,
        is_synthetic: impl Fn(&str) -> bool,
        prefix: &str,
    ) -> Result<(), MappedError> {
        if self.published {
            return Err(MappedError::invariant(
                "published mapped netlist cannot be renamed",
            ));
        }
        let mut synthetic = Vec::new();
        let mut next = 1u32;
        for (index, slot) in self.nets.iter().enumerate() {
            if !slot.live {
                continue;
            }
            // An unnamed net has no stable identity at all: every consumer
            // invents one, so two renders of the same artifact can disagree.
            // Publication is where it acquires the same dense name a synthetic
            // one gets.
            let Some(name) = slot.name.and_then(|name| self.names.resolve(name)) else {
                synthetic.push(index);
                continue;
            };
            if is_synthetic(name) {
                synthetic.push(index);
                continue;
            }
            if let Some(ordinal) = name
                .strip_prefix(prefix)
                .and_then(|rest| rest.parse::<u32>().ok())
            {
                next = next.max(ordinal.saturating_add(1));
            }
        }
        for index in synthetic {
            let name = self.names.intern(&format!("{prefix}{next}")).map_err(|_| {
                MappedError::invariant("mapped publication name arena is exhausted")
            })?;
            self.nets[index].name = Some(name);
            next = next.checked_add(1).ok_or_else(|| {
                MappedError::invariant("mapped publication name space is exhausted")
            })?;
        }
        Ok(())
    }

    /// Consumes, densely repacks, and seals the final mapped generation.
    ///
    /// This is intentionally separate from [`Self::compact`]: stable IDs are an
    /// optimization invariant and may be renumbered only once, after every
    /// incremental timing owner has been consumed and immediately before
    /// publication. The returned cell translation lets provenance cross that
    /// boundary with the netlist. No editable repacked generation is observable.
    ///
    /// Validation, compact-ID capacity checks, revision allocation, and
    /// generation allocation all finish before any topology arena is changed.
    /// The exclusive owner then reuses its existing arenas with read/write
    /// cursors; publication never reconstructs cells or names through owned
    /// string specifications.
    ///
    /// # Errors
    ///
    /// Returns [`MappedError`] if the netlist is already published or if its
    /// live topology cannot be published exactly.
    pub fn finalize_for_publication(mut self) -> Result<(Self, MappedCellRemap), MappedError> {
        let plan = self.prepare_publication()?;

        self.remap_boundary_nets(&plan.nets);
        self.compact_net_slots(&plan.nets);
        self.compact_cell_slots(&plan.cells, &plan.nets, plan.pin_count);

        self.generation = plan.target_generation;
        self.edit_revision = plan.publication_revision;
        self.published = true;
        self.compact();

        let remap = MappedCellRemap {
            source_generation: plan.source_generation,
            target_generation: plan.target_generation,
            cell_count: self.cells.len(),
            cells: plan.cells.into_boxed_slice(),
        };
        Ok((self, remap))
    }

    fn prepare_publication(&self) -> Result<PublicationPlan, MappedError> {
        if self.published {
            return Err(MappedError::invariant(
                "published mapped netlist cannot be repacked",
            ));
        }
        self.validate_live_counts()?;
        self.validate_references()?;
        let mut validation = self.validation_scratch();
        self.validate_external_net_index(&mut validation[..self.nets.len()])?;
        validation.fill(0);
        self.validate_connectivity(&mut validation[..self.connections.len()])?;
        validation.fill(0);
        self.validate_unique_names(&mut validation[..self.names.entry_count()])?;
        drop(validation);

        let publication_revision = self.edit_revision.checked_add(1).ok_or_else(|| {
            MappedError::invariant("mapped publication revision space is exhausted")
        })?;
        let nets = publication_remap(
            self.nets.iter().map(|slot| slot.live),
            self.nets.len(),
            NetId::from_index,
        )?;
        let cells = publication_remap(
            self.cells.iter().map(|slot| slot.live),
            self.cells.len(),
            CellId::from_index,
        )?;
        let pin_count =
            self.cells
                .iter()
                .filter(|slot| slot.live)
                .try_fold(0usize, |count, slot| {
                    count
                        .checked_add(
                            (slot.cell.connection_end - slot.cell.connection_start) as usize,
                        )
                        .ok_or_else(|| MappedError::capacity("pin connection arena"))
                })?;
        if pin_count != 0 {
            PinId::from_index(pin_count - 1)?;
        }

        let source_generation = self.generation;
        let target_generation = MappedGenerationId::fresh();
        debug_assert_ne!(source_generation, target_generation);
        Ok(PublicationPlan {
            source_generation,
            target_generation,
            publication_revision,
            nets,
            cells,
            pin_count,
        })
    }

    fn remap_boundary_nets(&mut self, nets: &[u32]) {
        for net in &mut self.port_nets {
            *net = remapped_net(nets, *net);
        }
        for signal in &mut self.design_connection_signals {
            *signal = remapped_signal(nets, *signal);
        }
        for (net, _) in &mut self.constant_drivers {
            *net = remapped_net(nets, *net);
        }
        for net in &mut self.external_nets {
            *net = remapped_net(nets, *net);
        }
    }

    fn compact_net_slots(&mut self, remap: &[u32]) {
        debug_assert_eq!(remap.len(), self.nets.len());
        let mut write = 0usize;
        for (read, &target) in remap.iter().enumerate() {
            if target == REMOVED_SLOT {
                continue;
            }
            let mut slot = self.nets[read];
            slot.live = true;
            slot.version = 0;
            self.nets[write] = slot;
            write += 1;
        }
        debug_assert_eq!(write, self.live_net_count);
        self.nets.truncate(write);
        self.net_pins.truncate(write);
        self.net_pins.fill(NetPins::default());
        self.live_net_count = write;
    }

    fn compact_cell_slots(&mut self, cells: &[u32], nets: &[u32], pin_count: usize) {
        debug_assert_eq!(cells.len(), self.cells.len());
        let mut write_cell = 0usize;
        let mut write_pin = 0usize;
        for (read_cell, &target) in cells.iter().enumerate() {
            if target == REMOVED_SLOT {
                continue;
            }
            let mut slot = self.cells[read_cell];
            let owner = dense_cell_id(target);
            let connection_start =
                u32::try_from(write_pin).expect("published pin count was capacity-checked");
            for read_pin in slot.cell.connection_start as usize..slot.cell.connection_end as usize {
                let mut connection = self.connections[read_pin];
                connection.signal = remapped_signal(nets, connection.signal);
                self.connections[write_pin] = connection;
                self.pin_owners[write_pin] = owner;
                self.pin_links[write_pin] = PinLinks::default();
                if let ConnectionSignal::Net(net) = connection.signal {
                    let pin = dense_pin_id(write_pin);
                    append_pin(
                        &mut self.net_pins,
                        &mut self.pin_links[..=write_pin],
                        net,
                        pin,
                    );
                }
                write_pin += 1;
            }
            slot.cell.connection_start = connection_start;
            slot.cell.connection_end =
                u32::try_from(write_pin).expect("published pin count was capacity-checked");
            slot.live = true;
            slot.version = 0;
            self.cells[write_cell] = slot;
            write_cell += 1;
        }
        debug_assert_eq!(write_cell, self.live_cell_count);
        debug_assert_eq!(write_pin, pin_count);
        self.cells.truncate(write_cell);
        self.connections.truncate(write_pin);
        self.pin_owners.truncate(write_pin);
        self.pin_links.truncate(write_pin);
        self.live_cell_count = write_cell;
    }
}

fn publication_remap<I, T>(
    live: I,
    slot_count: usize,
    validate_id: impl Fn(usize) -> Result<T, MappedError>,
) -> Result<Vec<u32>, MappedError>
where
    I: IntoIterator<Item = bool>,
{
    let mut remap = vec![REMOVED_SLOT; slot_count];
    let mut next = 0usize;
    for (slot, live) in live.into_iter().enumerate() {
        if !live {
            continue;
        }
        validate_id(next)?;
        remap[slot] =
            u32::try_from(next).map_err(|_| MappedError::capacity("publication remap"))?;
        next += 1;
    }
    Ok(remap)
}

fn dense_cell_id(index: u32) -> CellId {
    CellId::from_index(index as usize).expect("preflighted dense cell ID must fit")
}

fn dense_pin_id(index: usize) -> PinId {
    PinId::from_index(index).expect("preflighted dense pin ID must fit")
}

fn remapped_net(remap: &[u32], net: NetId) -> NetId {
    let index = remap[net.index()];
    debug_assert_ne!(index, REMOVED_SLOT);
    NetId::from_index(index as usize).expect("preflighted dense net ID must fit")
}

fn remapped_signal(remap: &[u32], signal: ConnectionSignal) -> ConnectionSignal {
    match signal {
        ConnectionSignal::Net(net) => ConnectionSignal::Net(remapped_net(remap, net)),
        ConnectionSignal::Constant(value) => ConnectionSignal::Constant(value),
    }
}
