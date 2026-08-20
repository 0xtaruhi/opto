// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Sealed timing topology, stable identities, and incremental region deltas.
//!
//! The model separates stable instance IDs from vector positions and seals a
//! generation hash over topology, library, and parasitics. Results carry that
//! generation so callers can reject stale analysis after structural edits.

use crate::{
    DesignId, MappedNetId, Parasitics, PortId, TargetCellRef, TimingLibrary, TimingTopologySchema,
    analysis,
};
use opto_ir::mapped::MappedGenerationId;
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::mem::size_of;
use std::num::NonZeroU32;
use std::sync::Arc;

mod access;
mod bindings;
mod design;
mod mapped;
mod region;

pub use access::{TimingConnectionRef, TimingConnections, TimingInstanceRef};
pub use bindings::{TimingObjectBindings, TimingObjectBindingsBuilder};
pub use design::TimingDesignView;
pub(crate) use design::{OwnedTimingInstance, SharedTimingDesign, TimingInstanceView};
pub(crate) use region::InstanceRegionModelEdit;

/// Semantic identity of one sealed timing topology, before analysis inputs
/// such as Liberty and parasitics are bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct TimingTopologyFingerprint([u8; 32]);

impl TimingTopologyFingerprint {
    fn seal(state: &TimingTopologyState) -> Self {
        let mut hash = blake3::Hasher::new();
        hash.update(b"opto.timing.incremental-topology.v2\0");
        hash.update(&state.fixed);
        hash.update(&state.instances.count.to_le_bytes());
        hash.update(&state.instances.sum.bytes());
        hash.update(&state.mapped_bindings.count.to_le_bytes());
        hash.update(&state.mapped_bindings.sum.bytes());
        Self(*hash.finalize().as_bytes())
    }
}

/// A compact, reversible digest of independently keyed topology records.
///
/// Instance and mapped-net identities are unique within a model. Summing
/// their domain-separated BLAKE3 digests therefore gives an order-independent
/// accumulator that can be updated and rolled back in O(changed records),
/// without retaining a second copy of the flat topology.
#[derive(Debug, Clone, Copy, Default)]
struct TopologyDigestSum([u64; 4]);

impl TopologyDigestSum {
    fn add(&mut self, digest: [u8; 32]) {
        for (sum, bytes) in self.0.iter_mut().zip(digest.as_chunks::<8>().0) {
            *sum = sum.wrapping_add(u64::from_le_bytes(*bytes));
        }
    }

    fn remove(&mut self, digest: [u8; 32]) {
        for (sum, bytes) in self.0.iter_mut().zip(digest.as_chunks::<8>().0) {
            *sum = sum.wrapping_sub(u64::from_le_bytes(*bytes));
        }
    }

    fn bytes(self) -> [u8; 32] {
        let mut bytes = [0; 32];
        for (chunk, value) in bytes.as_chunks_mut::<8>().0.iter_mut().zip(self.0) {
            *chunk = value.to_le_bytes();
        }
        bytes
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct TopologyRecordDigest {
    count: u64,
    sum: TopologyDigestSum,
}

impl TopologyRecordDigest {
    fn insert(&mut self, digest: [u8; 32]) {
        self.count += 1;
        self.sum.add(digest);
    }

    fn remove(&mut self, digest: [u8; 32]) {
        debug_assert_ne!(self.count, 0);
        self.count -= 1;
        self.sum.remove(digest);
    }
}

/// Incrementally maintained semantic topology seal.
///
/// The design header and ports are immutable after model construction.
/// Mutable instances and mapped-net bindings are accumulated by stable typed
/// identity, making the seal independent of `swap_remove` positions and of
/// append-only graph-net tombstones.
#[derive(Debug, Clone, Copy)]
struct TimingTopologyState {
    fixed: [u8; 32],
    instances: TopologyRecordDigest,
    mapped_bindings: TopologyRecordDigest,
}

impl TimingTopologyState {
    fn from_source(
        design: &SharedTimingDesign,
        topology: &SealedTopology,
    ) -> Result<Self, crate::TimingError> {
        let mut fixed = blake3::Hasher::new();
        fixed.update(b"opto.timing.fixed-topology.v2\0");
        fixed.update(&design.id().uid().get().get().to_le_bytes());
        hash_text(&mut fixed, design.name());
        fixed.update(&(design.ports().len() as u64).to_le_bytes());
        for port in design.ports() {
            fixed.update(&port.id.uid().get().get().to_le_bytes());
            hash_text(&mut fixed, &port.name);
            fixed.update(&[port.direction as u8]);
            hash_text(&mut fixed, port.net.name());
        }
        let mut state = Self {
            fixed: *fixed.finalize().as_bytes(),
            instances: TopologyRecordDigest::default(),
            mapped_bindings: TopologyRecordDigest::default(),
        };
        for instance in design.instances() {
            let nets = topology
                .instance_nets
                .get(instance.id)
                .ok_or(crate::TimingAnalysisError::InconsistentTopology)?;
            state.instances.insert(instance_source_topology_digest(
                instance,
                nets,
                &topology.net_names,
            )?);
        }
        Ok(state)
    }

    fn insert_instance(&mut self, instance: &TimingInstance) {
        self.instances.insert(instance_topology_digest(instance));
    }

    fn remove_instance(&mut self, instance: &TimingInstance) {
        self.instances.remove(instance_topology_digest(instance));
    }

    fn insert_mapped_binding(&mut self, mapped: MappedNetId, net: &str) {
        self.mapped_bindings
            .insert(mapped_binding_topology_digest(mapped, net));
    }

    fn remove_mapped_binding(&mut self, mapped: MappedNetId, net: &str) {
        self.mapped_bindings
            .remove(mapped_binding_topology_digest(mapped, net));
    }

    fn fingerprint(&self) -> TimingTopologyFingerprint {
        TimingTopologyFingerprint::seal(self)
    }
}

fn instance_source_topology_digest(
    instance: TimingInstanceView<'_>,
    nets: &[TimingNetId],
    net_names: &analysis::SharedNetNames,
) -> Result<[u8; 32], crate::TimingError> {
    if nets.len() != instance.connection_count() {
        return Err(crate::TimingAnalysisError::InconsistentTopology.into());
    }
    let mut hash = blake3::Hasher::new();
    hash.update(b"opto.timing.instance-topology.v2\0");
    hash.update(&instance.id.raw().to_le_bytes());
    hash_text(&mut hash, instance.name);
    hash_text(&mut hash, instance.cell);
    hash.update(&(instance.connection_count() as u64).to_le_bytes());
    for (connection, &net) in instance.connections().zip(nets) {
        hash_text(&mut hash, connection.pin);
        hash_text(
            &mut hash,
            net_names
                .get(net.index())
                .ok_or(crate::TimingAnalysisError::InconsistentTopology)?,
        );
    }
    Ok(*hash.finalize().as_bytes())
}

fn instance_topology_digest(instance: &TimingInstance) -> [u8; 32] {
    let mut hash = blake3::Hasher::new();
    hash.update(b"opto.timing.instance-topology.v2\0");
    hash.update(&instance.id.raw().to_le_bytes());
    hash_text(&mut hash, &instance.name);
    hash_text(&mut hash, &instance.cell);
    hash.update(&(instance.connections.len() as u64).to_le_bytes());
    for connection in &instance.connections {
        hash_text(&mut hash, &connection.pin);
        hash_text(&mut hash, &connection.net);
    }
    *hash.finalize().as_bytes()
}

fn mapped_binding_topology_digest(mapped: MappedNetId, net: &str) -> [u8; 32] {
    let mut hash = blake3::Hasher::new();
    hash.update(b"opto.timing.mapped-net-binding.v2\0");
    hash.update(&(mapped.index() as u64).to_le_bytes());
    hash_text(&mut hash, net);
    *hash.finalize().as_bytes()
}

#[derive(Debug, Clone, Copy)]
struct TimingAnalysisInputsFingerprint([u8; 32]);

impl TimingAnalysisInputsFingerprint {
    fn seal(library: &TimingLibrary, parasitics: &Parasitics) -> Self {
        let mut hash = blake3::Hasher::new();
        hash.update(b"opto.timing.analysis-inputs.v1\0");
        hash.update(&library.analysis_fingerprint().bytes());
        hash.update(&parasitics.content_fingerprint().bytes());
        Self(*hash.finalize().as_bytes())
    }
}

/// Semantic identity of a complete timing/power analysis model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TimingGeneration([u8; 32]);

impl TimingGeneration {
    #[must_use]
    /// Borrows the stable 256-bit generation digest.
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    fn seal(topology: TimingTopologyFingerprint, inputs: TimingAnalysisInputsFingerprint) -> Self {
        let mut hash = blake3::Hasher::new();
        hash.update(b"opto.timing.generation.v4\0");
        hash.update(&topology.0);
        hash.update(&inputs.0);
        Self(*hash.finalize().as_bytes())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
/// Kind discriminator for one independently shared model allocation.
pub enum SharedTimingComponentKind {
    /// Compact timing design.
    Design,
    /// Static graph arcs.
    GraphArcs,
    /// Net names.
    NetNames,
    /// Port-to-net bindings.
    PortNets,
    /// Persistent port bindings.
    PortBindings,
    /// Net-to-port bindings.
    NetPorts,
    /// Outgoing arc adjacency.
    OutgoingArcs,
    /// Incoming arc adjacency.
    IncomingArcs,
    /// Primary inputs.
    PrimaryInputs,
    /// Sequential outputs.
    SequentialOutputs,
    /// Per-instance nets.
    InstanceNets,
    /// Per-instance cells.
    InstanceCells,
    /// Timing-to-mapped net translation.
    TimingToMappedNets,
    /// Mapped-to-timing net translation.
    MappedToTimingNets,
    /// Mapped port nets.
    MappedPortNets,
    /// Sparse instance positions.
    InstancePositions,
    /// Topological order.
    TopologicalOrder,
    /// Inverse topological positions.
    TopologicalPositions,
    /// Dependency predecessors.
    DependencyPredecessors,
    /// Dependency successors.
    DependencySuccessors,
    /// Dependency positions.
    DependencyPositions,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Exact identity and logical resident bytes of one shared allocation group.
pub struct SharedTimingComponent {
    /// Component category.
    pub kind: SharedTimingComponentKind,
    /// Allocation identity used to detect sharing.
    pub identity: usize,
    /// Logical resident bytes.
    pub bytes: usize,
}

fn hash_text(hash: &mut blake3::Hasher, text: &str) {
    hash.update(&(text.len() as u64).to_le_bytes());
    hash.update(text.as_bytes());
}

pub(crate) fn btree_memory_bytes<K, V>(len: usize) -> usize {
    opto_core::resident::slice_bytes::<(K, V, [usize; 4])>(len)
}

#[derive(Debug)]
pub(crate) struct SealedTopology {
    pub(crate) net_names: analysis::SharedNetNames,
    pub(crate) port_nets: Box<[TimingNetId]>,
    pub(crate) instance_nets: analysis::InstanceNetArena,
    pub(crate) construction_scratch_high_water_bytes: usize,
}

const CONSTANT_LOW_NET: &str = "\0opto.constant.0";
const CONSTANT_HIGH_NET: &str = "\0opto.constant.1";

pub(crate) const fn constant_net_name(value: bool) -> &'static str {
    if value {
        CONSTANT_HIGH_NET
    } else {
        CONSTANT_LOW_NET
    }
}

pub(crate) fn constant_net_value(name: &str) -> Option<bool> {
    match name {
        CONSTANT_LOW_NET => Some(false),
        CONSTANT_HIGH_NET => Some(true),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq)]
/// Flat design input used to construct a timing topology.
pub struct TimingDesign {
    /// Persistent design-object identity.
    pub id: DesignId,
    /// User-visible design name.
    pub name: String,
    /// Design ports in stable order.
    pub ports: Vec<TimingPort>,
    /// Cell instances with stable IDs.
    pub instances: Vec<TimingInstance>,
}

#[derive(Debug)]
/// Sealed timing graph bound to one library and parasitic view.
pub struct TimingModel {
    mapped_generation: Option<MappedGenerationId>,
    pub(crate) design: SharedTimingDesign,
    pub(crate) library: TimingLibrary,
    pub(crate) graph: analysis::TimingGraph,
    pub(crate) timing_to_mapped_net: opto_core::PagedCowVec<Option<MappedNetId>>,
    pub(crate) mapped_to_timing_net: opto_core::PagedCowVec<Option<TimingNetId>>,
    mapped_port_nets: Arc<[MappedNetId]>,
    pub(crate) instance_positions: InstancePositions,
    pub(crate) object_bindings: Arc<TimingObjectBindings>,
    topology: TimingTopologyState,
    analysis_inputs: TimingAnalysisInputsFingerprint,
    generation: TimingGeneration,
    construction_scratch_high_water_bytes: usize,
}

/// Borrowed, schema-sealed source for constructing another characterized view
/// without rebuilding its static timing graph.
#[derive(Debug, Clone)]
pub struct PreparedTimingTopology<'a> {
    source: &'a TimingModel,
    schema: TimingTopologySchema,
}

impl PreparedTimingTopology<'_> {
    #[must_use]
    /// Returns the exact compatible library schema.
    pub const fn schema(&self) -> &TimingTopologySchema {
        &self.schema
    }

    #[must_use]
    /// Returns bytes shared with forked views.
    pub fn shared_memory_bytes(&self) -> usize {
        self.source
            .shared_components()
            .iter()
            .map(|component| component.bytes)
            .sum()
    }

    #[must_use]
    /// Describes allocations shared with forked views.
    pub fn shared_components(&self) -> Vec<SharedTimingComponent> {
        self.source.shared_components()
    }
}

/// Dense, typed lookup from stable instance IDs to positions in
/// `TimingDesign::instances`.
///
/// Mapped designs allocate instance IDs from a compact 32-bit arena. Keeping
/// this relation in a `BTreeMap` put a tree lookup and one heap node per
/// instance on every timing propagation path. The non-zero encoded position
/// keeps each slot to four bytes while still preserving sparse stable IDs.
#[derive(Debug)]
pub(crate) struct InstancePositions {
    slots: opto_core::PagedCowVec<Option<NonZeroU32>>,
}

impl InstancePositions {
    fn build(design: &SharedTimingDesign) -> Result<Self, crate::TimingError> {
        if design.instance_count() > u32::MAX as usize {
            return Err(crate::TimingModelError::Capacity {
                resource: "instance position arena",
            }
            .into());
        }
        let row_count = design
            .instances()
            .map(|instance| instance.id.raw() as usize + 1)
            .max()
            .unwrap_or(0);
        let mut slots = opto_core::PagedCowVec::new(None);
        slots
            .try_resize(row_count)
            .map_err(|_| instance_position_capacity())?;
        for (position, instance) in design.instances().enumerate() {
            let encoded = encode_instance_position(position)?;
            if slots
                .try_set(instance.id.raw() as usize, Some(encoded))
                .map_err(|_| instance_position_capacity())?
                .flatten()
                .is_some()
            {
                return Err(crate::TimingModelError::DuplicateInstanceId {
                    id: instance.id.raw(),
                }
                .into());
            }
        }
        Ok(Self { slots })
    }

    pub(crate) fn get(&self, id: TimingInstanceId) -> Option<usize> {
        self.slots
            .get(id.raw() as usize)
            .copied()
            .flatten()
            .map(|position| position.get() as usize - 1)
    }

    pub(super) fn insert(
        &mut self,
        id: TimingInstanceId,
        position: usize,
    ) -> Result<Option<usize>, crate::TimingError> {
        let encoded = encode_instance_position(position)?;
        let index = id.raw() as usize;
        Ok(self
            .slots
            .try_set(index, Some(encoded))
            .map_err(|_| instance_position_capacity())?
            .flatten()
            .map(|old| old.get() as usize - 1))
    }

    pub(super) fn remove(
        &mut self,
        id: TimingInstanceId,
    ) -> Result<Option<usize>, crate::TimingError> {
        let index = id.raw() as usize;
        if index >= self.slots.len() {
            return Ok(None);
        }
        let old = self
            .slots
            .try_set(index, None)
            .map_err(|_| instance_position_capacity())?
            .flatten()
            .map(|position| position.get() as usize - 1);
        while self.slots.get(self.slots.len().saturating_sub(1)) == Some(&None) {
            self.slots.truncate(self.slots.len() - 1);
        }
        Ok(old)
    }
}

fn encode_instance_position(position: usize) -> Result<NonZeroU32, crate::TimingError> {
    position
        .checked_add(1)
        .and_then(|position| u32::try_from(position).ok())
        .and_then(NonZeroU32::new)
        .ok_or_else(instance_position_capacity)
}

fn instance_position_capacity() -> crate::TimingError {
    crate::TimingModelError::Capacity {
        resource: "instance position arena",
    }
    .into()
}

fn mapped_binding_capacity() -> crate::TimingError {
    crate::TimingModelError::Capacity {
        resource: "mapped-net binding arena",
    }
    .into()
}

impl Default for InstancePositions {
    fn default() -> Self {
        Self {
            slots: opto_core::PagedCowVec::new(None),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Port declaration and its connected timing net.
pub struct TimingPort {
    /// Persistent port-object identity.
    pub id: PortId,
    /// The user-visible port object name. This is deliberately separate from
    /// the connected timing net: multiple ports may alias the same mapped net.
    pub name: String,
    /// Connected logical timing net.
    pub net: TimingNet,
    /// Timing signal-flow direction.
    pub direction: TimingPortDirection,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Named logical net with an optional mapped-net identity.
pub struct TimingNet {
    name: String,
    mapped: Option<MappedNetId>,
}

impl TimingNet {
    #[must_use]
    /// Constructs a source-level net with no mapped-net binding.
    pub fn named(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            mapped: None,
        }
    }

    #[must_use]
    /// Constructs a net bound to a mapped-net identity.
    pub fn mapped(name: impl Into<String>, mapped: MappedNetId) -> Self {
        Self {
            name: name.into(),
            mapped: Some(mapped),
        }
    }

    #[must_use]
    /// Returns the timing-net name.
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    /// Returns the mapped-net identity, when bound.
    pub const fn mapped_id(&self) -> Option<MappedNetId> {
        self.mapped
    }
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[repr(u8)]
/// Signal-flow direction of a top-level timing port.
pub enum TimingPortDirection {
    /// Signal enters the design.
    Input,
    /// Signal leaves the design.
    Output,
    /// Bidirectional signal.
    Inout,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Cell instance used to build or update the timing model.
pub struct TimingInstance {
    /// Stable instance identity retained across region edits.
    pub id: TimingInstanceId,
    /// Unique instance name.
    pub name: String,
    /// Target-library cell name.
    pub cell: String,
    /// Pin-to-net bindings.
    pub connections: Vec<TimingConnection>,
}

pub(crate) fn design_memory_bytes_for_instances<'a>(
    instances: impl IntoIterator<Item = &'a TimingInstance>,
) -> usize {
    instances
        .into_iter()
        .map(|instance| {
            instance.connections.iter().fold(
                std::mem::size_of::<TimingInstance>()
                    .saturating_add(opto_core::resident::allocation_bytes(instance.name.len()))
                    .saturating_add(opto_core::resident::allocation_bytes(instance.cell.len()))
                    .saturating_add(opto_core::resident::slice_bytes::<TimingConnection>(
                        instance.connections.len(),
                    )),
                |bytes, connection| {
                    bytes
                        .saturating_add(opto_core::resident::allocation_bytes(connection.pin.len()))
                        .saturating_add(opto_core::resident::allocation_bytes(connection.net.len()))
                },
            )
        })
        .sum()
}

/// A cell identifier in the flattened `TimingLibrary::cells` arena.
///
/// This ID is generated by the timing linker. It is intentionally distinct
/// from `MappedCell::library_cell`, whose index belongs to the synthesis
/// mapping-library view and can differ when the resolution library set is a
/// superset of the mapping library set.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct LibraryCellId(NonZeroU32);

impl std::fmt::Debug for LibraryCellId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_tuple("LibraryCellId")
            .field(&self.raw())
            .finish()
    }
}

impl LibraryCellId {
    pub(crate) fn from_index(index: usize) -> Result<Self, crate::TimingError> {
        u32::try_from(index)
            .ok()
            .and_then(|index| index.checked_add(1))
            .and_then(NonZeroU32::new)
            .map(Self)
            .ok_or_else(|| {
                crate::TimingModelError::Capacity {
                    resource: "library cell ID",
                }
                .into()
            })
    }

    #[must_use]
    /// Returns the zero-based library-cell arena index.
    pub const fn raw(self) -> u32 {
        self.0.get() - 1
    }

    #[must_use]
    /// Returns the zero-based library-cell arena index as `usize`.
    pub const fn index(self) -> usize {
        self.raw() as usize
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
/// Deterministic set of instance and mapped-net changes for one region.
pub struct TimingRegionDelta {
    mapped_generation: Option<MappedGenerationId>,
    updates: BTreeMap<TimingInstanceId, Option<TimingInstance>>,
    mapped_net_bindings: BTreeMap<MappedNetId, Option<String>>,
}

impl TimingRegionDelta {
    #[must_use]
    /// Creates an empty regional edit.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds or replaces an instance exactly once.
    ///
    /// # Errors
    ///
    /// Returns [`crate::TimingModelError::DuplicateInstanceUpdate`] if this
    /// delta already contains the same stable ID.
    pub fn set_instance(&mut self, instance: TimingInstance) -> Result<(), crate::TimingError> {
        let id = instance.id;
        if self.updates.insert(id, Some(instance)).is_some() {
            return Err(crate::TimingModelError::DuplicateInstanceUpdate { id: id.raw() }.into());
        }
        Ok(())
    }

    /// Removes an instance by stable ID.
    ///
    /// # Errors
    ///
    /// Returns [`crate::TimingModelError::DuplicateInstanceUpdate`] if this
    /// delta already contains the ID.
    pub fn remove_instance(&mut self, id: TimingInstanceId) -> Result<(), crate::TimingError> {
        if self.updates.insert(id, None).is_some() {
            return Err(crate::TimingModelError::DuplicateInstanceUpdate { id: id.raw() }.into());
        }
        Ok(())
    }

    /// Combines independently prepared edits into one propagation transaction.
    /// Repeated entries are permitted only when they describe the same final
    /// state, which is common for adjacent mapped regions sharing a net.
    ///
    /// # Errors
    ///
    /// Returns [`crate::TimingModelError::ForeignMappedRegionEdit`] for different
    /// mapped generations, or a duplicate-update error when the same instance
    /// or mapped net is assigned conflicting final state.
    pub fn merge(&mut self, other: Self) -> Result<(), crate::TimingError> {
        if self.mapped_generation != other.mapped_generation {
            return Err(crate::TimingModelError::ForeignMappedRegionEdit.into());
        }
        for (id, update) in other.updates {
            if let Some(existing) = self.updates.get(&id) {
                if existing != &update {
                    return Err(
                        crate::TimingModelError::DuplicateInstanceUpdate { id: id.raw() }.into(),
                    );
                }
                continue;
            }
            self.updates.insert(id, update);
        }
        for (net, binding) in other.mapped_net_bindings {
            if let Some(existing) = self.mapped_net_bindings.get(&net) {
                if existing != &binding {
                    return Err(crate::TimingModelError::DuplicateMappedNetUpdate { net }.into());
                }
                continue;
            }
            self.mapped_net_bindings.insert(net, binding);
        }
        Ok(())
    }

    #[must_use]
    /// Returns whether the delta contains no instance or net-binding changes.
    pub fn is_empty(&self) -> bool {
        self.updates.is_empty() && self.mapped_net_bindings.is_empty()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
/// Dense net identity scoped to one [`TimingModel`] generation.
pub struct TimingNetId(u32);

impl TimingNetId {
    pub(crate) fn from_index(index: usize) -> Result<Self, crate::TimingError> {
        u32::try_from(index)
            .map(Self)
            .map_err(|_| crate::TimingModelError::Capacity { resource: "net ID" }.into())
    }

    #[must_use]
    /// Returns the underlying zero-based graph index.
    pub const fn raw(self) -> u32 {
        self.0
    }

    pub(crate) const fn from_raw(raw: u32) -> Self {
        Self(raw)
    }

    #[must_use]
    pub(crate) const fn index(self) -> usize {
        self.0 as usize
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
/// Stable instance identity used across timing region edits.
pub struct TimingInstanceId(u32);

impl TimingInstanceId {
    /// Wraps a stable mapped-instance ID.
    #[must_use]
    pub fn from_raw(raw: u32) -> Self {
        Self(raw)
    }

    /// Returns the underlying mapped-instance ID.
    #[must_use]
    pub fn raw(self) -> u32 {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Named connection from a library pin to a logical net.
pub struct TimingConnection {
    /// Target-library pin name.
    pub pin: String,
    /// Connected logical net name.
    pub net: String,
}

#[cfg(test)]
mod storage_tests {
    use super::*;

    #[test]
    fn library_cell_option_is_one_word() {
        assert_eq!(
            std::mem::size_of::<Option<LibraryCellId>>(),
            std::mem::size_of::<u32>()
        );
        let cell = LibraryCellId::from_index(17).unwrap();
        assert_eq!(cell.raw(), 17);
        assert_eq!(format!("{cell:?}"), "LibraryCellId(17)");
    }

    #[test]
    fn instance_positions_preserve_sparse_stable_ids_in_dense_slots() {
        let mut positions = InstancePositions::default();
        assert_eq!(
            positions.insert(TimingInstanceId::from_raw(42), 7).unwrap(),
            None
        );
        assert_eq!(positions.get(TimingInstanceId::from_raw(42)), Some(7));
        assert_eq!(positions.get(TimingInstanceId::from_raw(41)), None);
        assert_eq!(positions.slots.len(), 43);
        assert!(
            positions
                .slots
                .shared_pages()
                .map(|(_, bytes)| bytes)
                .sum::<usize>()
                >= 43 * std::mem::size_of::<u32>()
        );
        assert_eq!(
            positions.remove(TimingInstanceId::from_raw(42)).unwrap(),
            Some(7)
        );
        assert_eq!(positions.slots.len(), 0);
    }
}
