// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

impl TimingModel {
    /// Seals this compacted model as the topology source for sibling views.
    #[must_use]
    pub fn prepared_topology(&self) -> PreparedTimingTopology<'_> {
        PreparedTimingTopology {
            source: self,
            schema: self.library.topology_schema(),
        }
    }

    /// Forks a characterized view while sharing the prepared static arc and
    /// packed adjacency topology. A mismatched library schema is rejected
    /// before any follower graph is constructed.
    ///
    /// # Errors
    ///
    /// Returns [`crate::TimingModelError::IncompatibleTopologySchema`] when the
    /// follower library changes arc topology, or an analysis/model error when
    /// parasitic validation or follower value-column construction fails.
    pub fn fork_prepared_view(
        prepared: &PreparedTimingTopology<'_>,
        library: TimingLibrary,
        parasitics: Parasitics,
    ) -> Result<Self, crate::TimingError> {
        if library.topology_schema() != prepared.schema {
            return Err(crate::TimingModelError::IncompatibleTopologySchema.into());
        }
        let source = prepared.source;
        let construction_scratch_high_water_bytes = validation_scratch_bytes(&source.design);
        let analysis_inputs = TimingAnalysisInputsFingerprint::seal(&library, &parasitics);
        let generation = TimingGeneration::seal(source.topology.fingerprint(), analysis_inputs);
        let graph = analysis::TimingGraph::fork_view(
            &source.graph,
            &source.design,
            &source.instance_positions,
            &library,
            parasitics,
        )?;
        Ok(Self {
            mapped_generation: source.mapped_generation,
            design: source
                .design
                .fork_shared()
                .ok_or(crate::TimingAnalysisError::InconsistentTopology)?,
            library,
            graph,
            timing_to_mapped_net: source.timing_to_mapped_net.fork_shared(),
            mapped_to_timing_net: source.mapped_to_timing_net.fork_shared(),
            mapped_port_nets: Arc::clone(&source.mapped_port_nets),
            instance_positions: InstancePositions {
                slots: source.instance_positions.slots.fork_shared(),
            },
            object_bindings: Arc::clone(&source.object_bindings),
            topology: source.topology,
            analysis_inputs,
            generation,
            construction_scratch_high_water_bytes,
        })
    }

    /// Builds a timing model without extracted parasitics.
    ///
    /// # Errors
    ///
    /// Returns an error when design IDs, cell links, topology, or graph
    /// capacities are invalid.
    pub fn new(design: TimingDesign, library: TimingLibrary) -> Result<Self, crate::TimingError> {
        Self::new_with_parasitics(design, library, Parasitics::default())
    }

    /// Builds a timing model and seals its topology/input generation.
    ///
    /// # Errors
    ///
    /// Returns an error when design IDs, target-cell links, parasitic
    /// annotations, topology, or compact graph capacities are invalid.
    pub fn new_with_parasitics(
        design: TimingDesign,
        library: TimingLibrary,
        parasitics: Parasitics,
    ) -> Result<Self, crate::TimingError> {
        let owned_design_scratch_bytes = design_memory_bytes_for_instances(design.instances.iter());
        let topology = SealedTopology::flat(&design)?;
        let design = SharedTimingDesign::seal(design)?;
        let mut model = Self::from_sealed_source(
            design,
            topology,
            library,
            parasitics,
            None,
            owned_design_scratch_bytes,
        )?;
        let port_bindings = model
            .design
            .ports()
            .iter()
            .filter_map(|port| {
                port.net
                    .mapped_id()
                    .map(|mapped| (mapped, Some(port.net.name().to_string())))
            })
            .collect::<BTreeMap<_, _>>();
        model.construction_scratch_high_water_bytes = model
            .construction_scratch_high_water_bytes
            .max(sparse_mapped_binding_scratch_bytes(&port_bindings));
        model.install_mapped_net_bindings(port_bindings)?;
        Ok(model)
    }

    pub(super) fn from_sealed_source(
        design: SharedTimingDesign,
        topology: SealedTopology,
        library: TimingLibrary,
        parasitics: Parasitics,
        mapped_generation: Option<MappedGenerationId>,
        source_scratch_high_water_bytes: usize,
    ) -> Result<Self, crate::TimingError> {
        let instance_positions = InstancePositions::build(&design)?;
        let topology_state = TimingTopologyState::from_source(&design, &topology)?;
        let mut construction_scratch_high_water_bytes = source_scratch_high_water_bytes
            .max(sealed_topology_scratch_bytes(&topology))
            .max(validation_scratch_bytes(&design));
        let analysis_inputs = TimingAnalysisInputsFingerprint::seal(&library, &parasitics);
        let generation = TimingGeneration::seal(topology_state.fingerprint(), analysis_inputs);
        let mut mapped_port_nets = design
            .ports()
            .iter()
            .filter_map(|port| port.net.mapped_id())
            .collect::<Vec<_>>();
        mapped_port_nets.sort_unstable();
        mapped_port_nets.dedup();
        let graph =
            analysis::TimingGraph::build_with_topology(&design, &library, parasitics, topology)?;
        construction_scratch_high_water_bytes = construction_scratch_high_water_bytes
            .max(graph.construction_scratch_high_water_bytes());
        let mut timing_to_mapped_net = opto_core::PagedCowVec::new(None);
        timing_to_mapped_net
            .try_resize(graph.net_count())
            .map_err(|_| mapped_binding_capacity())?;
        Ok(Self {
            mapped_generation,
            design,
            library,
            graph,
            timing_to_mapped_net,
            mapped_to_timing_net: opto_core::PagedCowVec::new(None),
            mapped_port_nets: mapped_port_nets.into(),
            instance_positions,
            object_bindings: Arc::new(TimingObjectBindings::default()),
            topology: topology_state,
            analysis_inputs,
            generation,
            construction_scratch_high_water_bytes,
        })
    }

    #[must_use]
    /// Borrows the flat design input retained by the model.
    pub fn design(&self) -> TimingDesignView<'_> {
        TimingDesignView { model: self }
    }

    #[must_use]
    /// Borrows the resolved library view bound to this generation.
    pub fn library(&self) -> &TimingLibrary {
        &self.library
    }

    #[must_use]
    /// Returns the hash of topology, library, and parasitic inputs.
    pub const fn generation(&self) -> TimingGeneration {
        self.generation
    }

    /// Releases construction slack before the immutable model is cached.
    ///
    /// This preserves every typed ID and semantic ordering; only allocation
    /// capacity is normalized.
    ///
    /// # Errors
    ///
    /// Returns an error if compact design, graph, adjacency, or value storage
    /// cannot be rebuilt within checked capacity.
    pub fn compact(&mut self) -> Result<(), crate::TimingError> {
        let replaced_design_bytes = self.design.compact()?;
        self.construction_scratch_high_water_bytes = self
            .construction_scratch_high_water_bytes
            .max(replaced_design_bytes);
        compact_library_view(&mut self.library);
        self.graph.compact()?;
        Ok(())
    }

    /// Installs persistent object identities used by path-exception matching.
    pub fn set_object_bindings(&mut self, bindings: impl Into<Arc<TimingObjectBindings>>) {
        self.object_bindings = bindings.into();
    }

    /// Deterministic logical resident size for derived model allocations and
    /// the materialized library selection view whose lifetime it extends.
    ///
    /// Canonical Liberty/power arenas and compact parasitics are external
    /// inputs to this derived-model boundary; they are not charged again to
    /// each timing model.
    #[must_use]
    pub fn resident_memory_bytes(&self) -> usize {
        size_of::<Self>()
            .saturating_add(self.design.shared_memory_bytes())
            .saturating_add(self.design.exclusive_memory_bytes())
            .saturating_add(library_view_memory_bytes(&self.library))
            .saturating_add(self.graph.owned_memory_bytes())
            .saturating_add(self.timing_to_mapped_net.owned_memory_bytes())
            .saturating_add(opto_core::resident::slice_bytes::<MappedNetId>(
                self.mapped_port_nets.len(),
            ))
            .saturating_add(self.mapped_to_timing_net.owned_memory_bytes())
            .saturating_add(self.instance_positions.slots.owned_memory_bytes())
            .saturating_add(self.object_bindings.resident_memory_bytes())
    }

    #[must_use]
    /// Returns peak temporary bytes used while constructing the model.
    pub const fn construction_scratch_high_water_bytes(&self) -> usize {
        self.construction_scratch_high_water_bytes
    }

    #[must_use]
    /// Describes independently shared model allocations.
    pub fn shared_components(&self) -> Vec<SharedTimingComponent> {
        let mut components = self.graph.shared_components();
        let bytes = self.design.shared_memory_bytes();
        if bytes != 0 {
            components.push(SharedTimingComponent {
                kind: SharedTimingComponentKind::Design,
                identity: self.design.shared_identity(),
                bytes,
            });
        }
        for (identity, bytes) in self.timing_to_mapped_net.shared_pages() {
            components.push(SharedTimingComponent {
                kind: SharedTimingComponentKind::TimingToMappedNets,
                identity,
                bytes,
            });
        }
        for (identity, bytes) in self.mapped_to_timing_net.shared_pages() {
            components.push(SharedTimingComponent {
                kind: SharedTimingComponentKind::MappedToTimingNets,
                identity,
                bytes,
            });
        }
        for (identity, bytes) in self.instance_positions.slots.shared_pages() {
            components.push(SharedTimingComponent {
                kind: SharedTimingComponentKind::InstancePositions,
                identity,
                bytes,
            });
        }
        let (kind, identity, bytes) = (
            SharedTimingComponentKind::MappedPortNets,
            Arc::as_ptr(&self.mapped_port_nets).cast::<MappedNetId>() as usize,
            opto_core::resident::slice_bytes::<MappedNetId>(self.mapped_port_nets.len()),
        );
        {
            if bytes != 0 {
                components.push(SharedTimingComponent {
                    kind,
                    identity,
                    bytes,
                });
            }
        }
        components
    }

    #[must_use]
    /// Returns the number of dense timing-graph nets.
    pub fn net_count(&self) -> usize {
        self.graph.net_count()
    }

    /// Iterates over all graph-local net IDs in dense order.
    ///
    /// # Panics
    ///
    /// Panics if a sealed graph somehow contains more nets than its compact
    /// `u32` ID representation can address.
    #[must_use]
    pub fn net_ids(&self) -> impl ExactSizeIterator<Item = TimingNetId> + '_ {
        (0..self.net_count()).map(|index| {
            TimingNetId::from_index(index)
                .expect("sealed timing graph net count already satisfies ID capacity")
        })
    }

    /// User-visible label for reports and external annotation binding. Core
    /// algorithms use `TimingNetId` and never resolve this text back to an ID.
    #[must_use]
    pub fn net_name(&self, net: TimingNetId) -> Option<Cow<'_, str>> {
        self.graph.net_name(net.index()).map(Cow::Borrowed)
    }

    #[must_use]
    /// Borrows the nets connected to an instance's pins in connection order.
    pub fn instance_nets(&self, instance: TimingInstanceId) -> Option<&[TimingNetId]> {
        self.graph.instance_nets(instance)
    }

    #[must_use]
    /// Returns the number of live instances.
    pub fn instance_count(&self) -> usize {
        self.design.instance_count()
    }

    /// Iterates over instances in deterministic design order.
    ///
    /// # Panics
    ///
    /// Panics if a sealed design's reported instance count contains a hole;
    /// model construction and region commits preserve contiguous rows.
    #[must_use]
    pub fn instances(&self) -> impl ExactSizeIterator<Item = TimingInstanceRef<'_>> {
        (0..self.instance_count()).map(|row| {
            self.instance_at(row)
                .expect("sealed timing instance rows are contiguous")
        })
    }

    #[must_use]
    /// Returns the instance at a design-order row.
    pub fn instance_at(&self, row: usize) -> Option<TimingInstanceRef<'_>> {
        let id = self.design.instance(row)?.id;
        Some(TimingInstanceRef { model: self, id })
    }

    #[must_use]
    /// Resolves a stable instance ID without assuming it is a vector index.
    pub fn instance_ref(&self, id: TimingInstanceId) -> Option<TimingInstanceRef<'_>> {
        self.instance_positions
            .get(id)
            .map(|_| TimingInstanceRef { model: self, id })
    }

    #[must_use]
    /// Borrows an instance's report name by stable ID.
    pub fn instance_name(&self, instance: TimingInstanceId) -> Option<Cow<'_, str>> {
        self.flat_instance(instance)
            .map(|instance| Cow::Borrowed(instance.name))
    }

    /// Resolves a hierarchical instance label at an external annotation
    /// boundary. Analysis kernels retain typed IDs and never repeat this lookup.
    #[must_use]
    pub fn instance_id(&self, name: &str) -> Option<TimingInstanceId> {
        self.design.instance_id(name)
    }

    #[must_use]
    /// Returns the graph nets bound to a persistent port ID.
    pub fn port_nets(&self, port: PortId) -> &[TimingNetId] {
        self.graph.port_nets(port)
    }

    #[must_use]
    /// Returns whether a net is driven by a primary input port.
    pub fn net_is_input_port(&self, net: TimingNetId) -> bool {
        self.graph.net_is_input_port(net)
    }

    #[must_use]
    /// Recognizes the model's internal constant-low or constant-high net.
    pub fn constant_net_value(&self, net: TimingNetId) -> Option<bool> {
        self.net_name(net).as_deref().and_then(constant_net_value)
    }

    #[must_use]
    /// Returns the linked library-cell ID for an instance.
    pub fn instance_library_cell_id(&self, instance: TimingInstanceId) -> Option<LibraryCellId> {
        self.graph.instance_cell_index(instance)
    }

    #[must_use]
    /// Resolves a model-local library-cell ID.
    pub fn library_cell(&self, id: LibraryCellId) -> Option<TargetCellRef<'_>> {
        self.library.cells.get(id.index())
    }

    pub(crate) fn flat_instance(
        &self,
        id: TimingInstanceId,
    ) -> Option<super::design::TimingInstanceView<'_>> {
        self.instance_positions
            .get(id)
            .and_then(|position| self.design.instance(position))
    }

    pub(crate) fn owned_instance_at(&self, row: usize) -> Option<TimingInstance> {
        let instance = self.design.instance(row)?;
        let nets = self.graph.instance_nets(instance.id)?;
        if nets.len() != instance.connection_count() {
            return None;
        }
        Some(TimingInstance {
            id: instance.id,
            name: instance.name.to_string(),
            cell: instance.cell.to_string(),
            connections: instance
                .connections()
                .zip(nets)
                .map(|(connection, &net)| TimingConnection {
                    pin: connection.pin.to_string(),
                    net: self
                        .graph
                        .net_name(net.index())
                        .expect("instance net IDs reference live graph names")
                        .to_string(),
                })
                .collect(),
        })
    }

    pub(crate) fn owned_design(&self) -> TimingDesign {
        TimingDesign {
            id: self.design.id(),
            name: self.design.name().to_string(),
            ports: self.design.ports().to_vec(),
            instances: (0..self.design.instance_count())
                .map(|row| {
                    self.owned_instance_at(row)
                        .expect("live timing design rows have canonical graph net bindings")
                })
                .collect(),
        }
    }

    pub(crate) fn instance_cell(&self, instance: TimingInstanceId) -> Option<&str> {
        self.flat_instance(instance).map(|instance| instance.cell)
    }

    pub(crate) fn mapped_net(&self, net: TimingNetId) -> Option<MappedNetId> {
        self.timing_to_mapped_net
            .get(net.index())
            .copied()
            .flatten()
    }

    #[must_use]
    /// Maps a synthesis net ID into this timing generation.
    pub fn mapped_timing_net(&self, net: MappedNetId) -> Option<TimingNetId> {
        self.mapped_to_timing_net
            .get(net.index())
            .copied()
            .flatten()
    }

    #[must_use]
    /// Resolves an external net name at annotation/report boundaries.
    pub fn net_id(&self, name: &str) -> Option<TimingNetId> {
        self.graph
            .net_id(name)
            .and_then(|index| TimingNetId::from_index(index).ok())
    }

    pub(super) fn instance_connection_count(&self, instance: TimingInstanceId) -> usize {
        self.flat_instance(instance)
            .map_or(0, super::design::TimingInstanceView::connection_count)
    }

    pub(super) fn instance_connection(
        &self,
        instance: TimingInstanceId,
        index: usize,
    ) -> Option<TimingConnectionRef<'_>> {
        let connection = self.flat_instance(instance)?.connection(index)?;
        let net = *self.instance_nets(instance)?.get(index)?;
        Some(TimingConnectionRef {
            pin: connection.pin,
            net,
        })
    }
}

fn compact_library_view(library: &mut TimingLibrary) {
    for text in [
        &mut library.name,
        &mut library.operating_conditions,
        &mut library.wire_load,
        &mut library.wire_load_mode,
    ]
    .into_iter()
    .flatten()
    {
        text.shrink_to_fit();
    }
    if let Some(model) = &mut library.wire_load_model {
        model.name.shrink_to_fit();
    }
}

fn library_view_memory_bytes(library: &TimingLibrary) -> usize {
    library.retained_view_memory_bytes()
}

fn sparse_mapped_binding_scratch_bytes(bindings: &BTreeMap<MappedNetId, Option<String>>) -> usize {
    btree_memory_bytes::<MappedNetId, Option<String>>(bindings.len()).saturating_add(
        bindings
            .values()
            .flatten()
            .map(|name| opto_core::resident::allocation_bytes(name.len()))
            .sum(),
    )
}

pub(super) struct DenseMappedBindings {
    pub(super) timing_to_mapped: opto_core::PagedCowVec<Option<MappedNetId>>,
    pub(super) mapped_to_timing: opto_core::PagedCowVec<Option<TimingNetId>>,
    pub(super) scratch_high_water_bytes: usize,
}

pub(super) fn seal_dense_mapped_bindings<'a>(
    graph: &analysis::TimingGraph,
    topology: &mut TimingTopologyState,
    mapped_slot_count: usize,
    bindings: impl IntoIterator<Item = (MappedNetId, std::borrow::Cow<'a, str>)>,
) -> Result<DenseMappedBindings, crate::TimingError> {
    let mut timing_to_mapped = opto_core::PagedCowVec::new(None);
    timing_to_mapped
        .try_resize(graph.net_count())
        .map_err(|_| mapped_binding_capacity())?;
    let mut mapped_to_timing = opto_core::PagedCowVec::new(None);
    mapped_to_timing
        .try_resize(mapped_slot_count)
        .map_err(|_| mapped_binding_capacity())?;
    let mut scratch_high_water = 0usize;
    for (mapped, name) in bindings {
        if let std::borrow::Cow::Owned(name) = &name {
            scratch_high_water =
                scratch_high_water.max(opto_core::resident::allocation_bytes(name.len()));
        }
        if mapped.index() >= mapped_to_timing.len() {
            mapped_to_timing
                .try_resize(mapped.index() + 1)
                .map_err(|_| mapped_binding_capacity())?;
        }
        let timing_index = graph.net_id(&name).ok_or_else(|| {
            crate::TimingModelError::MappedNetMissingGraphNet {
                mapped,
                name: name.clone().into_owned(),
            }
        })?;
        let timing = TimingNetId::from_index(timing_index)?;
        if let Some(other) = timing_to_mapped.get(timing_index).copied().flatten()
            && other != mapped
        {
            return Err(crate::TimingModelError::MappedNetAlias {
                name: name.into_owned(),
                first: other,
                second: mapped,
            }
            .into());
        }
        if let Some(old_timing) = mapped_to_timing
            .try_set(mapped.index(), Some(timing))
            .map_err(|_| mapped_binding_capacity())?
            .flatten()
        {
            timing_to_mapped
                .try_set(old_timing.index(), None)
                .map_err(|_| mapped_binding_capacity())?;
        }
        timing_to_mapped
            .try_set(timing_index, Some(mapped))
            .map_err(|_| mapped_binding_capacity())?;
    }

    topology.mapped_bindings = TopologyRecordDigest::default();
    for mapped_index in 0..mapped_to_timing.len() {
        let Some(timing) = mapped_to_timing.get(mapped_index).copied().flatten() else {
            continue;
        };
        let mapped = MappedNetId::from_index(mapped_index).map_err(crate::TimingError::Mapped)?;
        let name = graph
            .net_name(timing.index())
            .expect("dense mapped binding references a live timing net");
        topology.insert_mapped_binding(mapped, name);
    }
    Ok(DenseMappedBindings {
        timing_to_mapped,
        mapped_to_timing,
        scratch_high_water_bytes: scratch_high_water,
    })
}

fn validation_scratch_bytes(design: &SharedTimingDesign) -> usize {
    validation_scratch_bytes_for_counts(design.ports().len())
}

fn validation_scratch_bytes_for_counts(ports: usize) -> usize {
    opto_core::resident::slice_bytes::<u32>(ports)
}

fn sealed_topology_scratch_bytes(topology: &SealedTopology) -> usize {
    topology.construction_scratch_high_water_bytes
}

#[derive(Clone, Copy)]
/// Lifetime-bound view of one stable timing instance.
pub struct TimingInstanceRef<'a> {
    model: &'a TimingModel,
    id: TimingInstanceId,
}

impl std::fmt::Debug for TimingInstanceRef<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TimingInstanceRef")
            .field("id", &self.id)
            .field("name", &self.name())
            .field("cell", &self.cell())
            .finish_non_exhaustive()
    }
}

impl<'a> TimingInstanceRef<'a> {
    #[must_use]
    /// Returns the stable instance identity.
    pub const fn id(self) -> TimingInstanceId {
        self.id
    }

    #[must_use]
    /// Returns the instance name.
    ///
    /// # Panics
    ///
    /// Panics if this reference outlives no model but its stable instance ID is
    /// absent from that same sealed model, indicating internal corruption.
    pub fn name(self) -> Cow<'a, str> {
        self.model
            .instance_name(self.id)
            .expect("timing instance references originate from the sealed model")
    }

    #[must_use]
    /// Returns the target-library cell name.
    ///
    /// # Panics
    ///
    /// Panics if this reference's stable instance ID has no cell record in the
    /// same sealed model.
    pub fn cell(self) -> &'a str {
        self.model
            .instance_cell(self.id)
            .expect("timing instance references originate from the sealed model")
    }

    #[must_use]
    /// Borrows pin-connected nets in connection order.
    ///
    /// # Panics
    ///
    /// Panics if this reference's stable instance ID has no typed connection
    /// row in the same sealed model.
    pub fn nets(self) -> &'a [TimingNetId] {
        self.model
            .instance_nets(self.id)
            .expect("sealed timing instances have typed net bindings")
    }

    /// Iterates over pin/net bindings in declaration order.
    #[must_use]
    pub fn connections(self) -> TimingConnections<'a> {
        TimingConnections {
            instance: self,
            index: 0,
            len: self.model.instance_connection_count(self.id),
        }
    }

    #[must_use]
    /// Finds the graph net connected to a named library pin.
    pub fn pin_net(self, pin: &str) -> Option<TimingNetId> {
        self.connections()
            .find_map(|connection| (connection.pin == pin).then_some(connection.net))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Borrowed pin-to-net binding for one timing instance.
pub struct TimingConnectionRef<'a> {
    /// Target-library pin name.
    pub pin: &'a str,
    /// Connected graph-local net.
    pub net: TimingNetId,
}

/// Exact-size iterator over an instance's pin-to-net bindings.
#[derive(Debug)]
pub struct TimingConnections<'a> {
    instance: TimingInstanceRef<'a>,
    index: usize,
    len: usize,
}

impl<'a> Iterator for TimingConnections<'a> {
    type Item = TimingConnectionRef<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        let index = self.index;
        self.index += usize::from(index < self.len);
        (index < self.len)
            .then(|| {
                self.instance
                    .model
                    .instance_connection(self.instance.id, index)
            })
            .flatten()
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.len - self.index;
        (remaining, Some(remaining))
    }
}

impl ExactSizeIterator for TimingConnections<'_> {}
