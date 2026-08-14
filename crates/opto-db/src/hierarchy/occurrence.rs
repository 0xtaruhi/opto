// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Explicit hierarchy occurrence materialization.
//!
//! A linked [`DefinitionGraph`] expands into compact structure-of-arrays
//! storage. Path strings are reconstructed only at reporting boundaries.

use super::{
    Definition, DefinitionGraph, DefinitionGraphError, DefinitionGraphOwner, DefinitionId,
    DefinitionInstance, InstanceOrdinal, LinkBinding, NameTable, OccurrenceGraphError,
    OccurrenceId, append_path,
};
use opto_core::OwnerToken;

/// A sealed, explicitly materialized hierarchy occurrence graph.
///
/// The graph stores no hierarchical path strings. Its four structure-of-array
/// arenas use compact typed IDs and preserve source-order preorder. The root is
/// always [`OccurrenceId::ROOT`]; it has no parent, incoming instance, or link
/// binding, and [`Self::bound_definition`] resolves it to the root definition.
#[derive(Debug)]
pub struct OccurrenceGraph {
    // Materialization and reporting must use the same graph or one of its clones.
    definition_graph_identity: OwnerToken<DefinitionGraphOwner>,
    parents: Box<[Option<OccurrenceId>]>,
    owner_definitions: Box<[DefinitionId]>,
    instance_ordinals: Box<[Option<InstanceOrdinal>]>,
    link_bindings: Box<[Option<LinkBinding>]>,
}

struct MaterializeFrame {
    occurrence: OccurrenceId,
    definition: DefinitionId,
    next_instance: usize,
}

impl OccurrenceGraph {
    /// Returns the exact number of nodes that materialization would create.
    ///
    /// The count includes the root plus every instance edge under every
    /// reachable definition occurrence. It is checked against both arithmetic
    /// overflow and the 32-bit dense-ID capacity.
    ///
    /// # Errors
    ///
    /// Returns [`OccurrenceGraphError`] if the hierarchy is unresolved,
    /// recursive, arithmetically overflowing, or exceeds dense-ID capacity.
    pub fn node_count(graph: &DefinitionGraph) -> Result<usize, OccurrenceGraphError> {
        occurrence_layout(graph, OccurrenceScope::All).map(|layout| layout.node_count)
    }

    /// Returns the number of root and design-bound occurrences.
    ///
    /// External leaf instances are excluded because they belong to the
    /// implementation inside each design occurrence rather than to the design
    /// hierarchy itself.
    ///
    /// # Errors
    ///
    /// Returns [`OccurrenceGraphError`] under the same layout conditions as
    /// [`Self::node_count`].
    pub fn design_node_count(graph: &DefinitionGraph) -> Result<usize, OccurrenceGraphError> {
        occurrence_layout(graph, OccurrenceScope::Designs).map(|layout| layout.node_count)
    }

    /// Materializes a fully linked definition graph in stable source order.
    ///
    /// # Errors
    ///
    /// Returns [`OccurrenceGraphError`] if layout validation fails or a linked
    /// definition/instance reference is inconsistent during materialization.
    pub fn materialize(graph: &DefinitionGraph) -> Result<Self, OccurrenceGraphError> {
        Self::materialize_with_scope(graph, OccurrenceScope::All)
    }

    /// Materializes only root and design-bound occurrences in stable source
    /// order.
    ///
    /// Incoming instance ordinals still address the original definition, so
    /// path formatting and child-port linking remain exact even when external
    /// instances precede a design instance.
    ///
    /// # Errors
    ///
    /// Returns [`OccurrenceGraphError`] under the same validation and capacity
    /// conditions as [`Self::materialize`].
    pub fn materialize_designs(graph: &DefinitionGraph) -> Result<Self, OccurrenceGraphError> {
        Self::materialize_with_scope(graph, OccurrenceScope::Designs)
    }

    fn materialize_with_scope(
        graph: &DefinitionGraph,
        scope: OccurrenceScope,
    ) -> Result<Self, OccurrenceGraphError> {
        if !graph.is_linked() {
            return Err(OccurrenceGraphError::UnresolvedHierarchy {
                occurrences: graph.unresolved_occurrence_count(),
            });
        }
        let layout = occurrence_layout(graph, scope)?;
        let mut parents = Vec::new();
        let mut owner_definitions = Vec::new();
        let mut instance_ordinals = Vec::new();
        let mut link_bindings = Vec::new();
        reserve_occurrence_arena(&mut parents, layout)?;
        reserve_occurrence_arena(&mut owner_definitions, layout)?;
        reserve_occurrence_arena(&mut instance_ordinals, layout)?;
        reserve_occurrence_arena(&mut link_bindings, layout)?;

        parents.push(None);
        owner_definitions.push(graph.root());
        instance_ordinals.push(None);
        link_bindings.push(None);

        let mut stack = Vec::new();
        stack
            .try_reserve_exact(graph.postorder().len())
            .map_err(|_| OccurrenceGraphError::Allocation {
                required_bytes: layout.storage_bytes,
            })?;
        stack.push(MaterializeFrame {
            occurrence: OccurrenceId::ROOT,
            definition: graph.root(),
            next_instance: 0,
        });

        while let Some(frame) = stack.last_mut() {
            let Some(instance) = graph.instance(frame.definition, frame.next_instance) else {
                stack.pop();
                continue;
            };
            let ordinal = frame.next_instance;
            frame.next_instance += 1;
            let parent = frame.occurrence;
            let owner = frame.definition;
            let binding = instance.binding();
            if !scope.includes(binding) {
                continue;
            }
            let occurrence = OccurrenceId::from_index(parents.len())?;

            parents.push(Some(parent));
            owner_definitions.push(owner);
            instance_ordinals.push(Some(InstanceOrdinal::from_index(ordinal)?));
            link_bindings.push(Some(binding));

            match binding {
                LinkBinding::Design {
                    definition: child, ..
                } => stack.push(MaterializeFrame {
                    occurrence,
                    definition: child,
                    next_instance: 0,
                }),
                LinkBinding::External { .. } => {}
                LinkBinding::Unresolved => {
                    unreachable!("unresolved hierarchies are rejected before materialization")
                }
            }
        }

        debug_assert_eq!(parents.len(), layout.node_count);
        Ok(Self {
            definition_graph_identity: graph.identity.clone(),
            parents: parents.into_boxed_slice(),
            owner_definitions: owner_definitions.into_boxed_slice(),
            instance_ordinals: instance_ordinals.into_boxed_slice(),
            link_bindings: link_bindings.into_boxed_slice(),
        })
    }

    #[must_use]
    /// Returns the root occurrence identity.
    pub fn root(&self) -> OccurrenceId {
        debug_assert!(!self.parents.is_empty());
        OccurrenceId::ROOT
    }

    #[must_use]
    /// Returns the number of materialized occurrences, including the root.
    pub fn len(&self) -> usize {
        self.parents.len()
    }

    #[must_use]
    /// Returns `true` when no occurrence was materialized.
    ///
    /// Successfully constructed graphs always contain the root, so this is
    /// primarily useful to generic collection consumers.
    pub fn is_empty(&self) -> bool {
        self.parents.is_empty()
    }

    /// Returns whether `id` addresses a materialized node in this graph.
    #[must_use]
    pub fn contains(&self, id: OccurrenceId) -> bool {
        id.index() < self.len()
    }

    /// Iterates over all occurrence IDs in deterministic preorder.
    ///
    /// # Panics
    ///
    /// Panics only if the materialized node arena exceeds the capacity checked
    /// before allocation.
    #[must_use]
    pub fn ids(&self) -> impl ExactSizeIterator<Item = OccurrenceId> + '_ {
        (0..self.len()).map(|index| {
            OccurrenceId::from_index(index).expect("materialized occurrence IDs fit capacity")
        })
    }

    #[must_use]
    /// Returns the parent occurrence, or `None` for the root or an invalid ID.
    pub fn parent(&self, id: OccurrenceId) -> Option<OccurrenceId> {
        self.parents.get(id.index()).copied().flatten()
    }

    /// Returns the definition that owns an occurrence's incoming instance.
    ///
    /// The root has no incoming instance, so its owner is defined to be the
    /// root definition itself.
    #[must_use]
    pub fn owner_definition(&self, id: OccurrenceId) -> Option<DefinitionId> {
        self.owner_definitions.get(id.index()).copied()
    }

    #[must_use]
    /// Returns the source-order instance ordinal that created `id`.
    ///
    /// The root and invalid IDs return `None`.
    pub fn instance_ordinal(&self, id: OccurrenceId) -> Option<usize> {
        self.instance_ordinals
            .get(id.index())
            .copied()
            .flatten()
            .map(InstanceOrdinal::index)
    }

    #[must_use]
    /// Returns the incoming link binding for a non-root occurrence.
    pub fn link_binding(&self, id: OccurrenceId) -> Option<LinkBinding> {
        self.link_bindings.get(id.index()).copied().flatten()
    }

    /// Returns the design definition represented by this occurrence.
    ///
    /// The root and design-bound instance occurrences return a definition;
    /// external leaf occurrences return `None`.
    #[must_use]
    pub fn bound_definition(&self, id: OccurrenceId) -> Option<DefinitionId> {
        match *self.link_bindings.get(id.index())? {
            None => self.owner_definition(id),
            Some(LinkBinding::Design { definition, .. }) => Some(definition),
            Some(LinkBinding::External { .. }) => None,
            Some(LinkBinding::Unresolved) => {
                unreachable!("sealed occurrence graphs contain no unresolved bindings")
            }
        }
    }

    #[must_use]
    /// Resolves the definition instance represented by `id`.
    ///
    /// The root, invalid IDs, and IDs paired with a different definition graph
    /// return `None`.
    pub fn instance<'a>(
        &self,
        graph: &'a DefinitionGraph,
        id: OccurrenceId,
    ) -> Option<DefinitionInstance<'a>> {
        if !self.definition_graph_identity.same_owner(&graph.identity) {
            return None;
        }
        let ordinal = self.instance_ordinal(id)?;
        graph.instance(self.owner_definition(id)?, ordinal)
    }

    /// Formats a hierarchical instance path on demand at a reporting boundary.
    ///
    /// No path text is retained in the occurrence graph. The root occurrence
    /// has the empty path. Invalid IDs and IDs paired with a different
    /// definition graph return `None`.
    #[must_use]
    pub fn format_path(&self, graph: &DefinitionGraph, id: OccurrenceId) -> Option<String> {
        if !self.definition_graph_identity.same_owner(&graph.identity) {
            return None;
        }
        let mut names = Vec::new();
        let mut current = Some(id);
        while let Some(occurrence) = current {
            let parent = self.parents.get(occurrence.index()).copied()?;
            if parent.is_some() {
                names.push(self.instance(graph, occurrence)?.name());
            }
            current = parent;
        }
        let capacity = names
            .iter()
            .map(|name| name.len())
            .sum::<usize>()
            .saturating_add(names.len().saturating_sub(1));
        let mut path = String::with_capacity(capacity);
        for name in names.into_iter().rev() {
            append_path(&mut path, name);
        }
        Some(path)
    }
}

#[derive(Clone, Copy)]
struct OccurrenceLayout {
    node_count: usize,
    storage_bytes: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum OccurrenceScope {
    All,
    Designs,
}

impl OccurrenceScope {
    fn includes(self, binding: LinkBinding) -> bool {
        self == Self::All || matches!(binding, LinkBinding::Design { .. })
    }
}

fn occurrence_layout(
    graph: &DefinitionGraph,
    scope: OccurrenceScope,
) -> Result<OccurrenceLayout, OccurrenceGraphError> {
    let node_count =
        graph
            .definitions
            .iter()
            .enumerate()
            .try_fold(1u64, |total, (index, definition)| {
                let id =
                    DefinitionId::from_index(index).map_err(|_| OccurrenceGraphError::Capacity)?;
                let instances = u64::try_from(
                    definition
                        .instances
                        .iter()
                        .filter(|instance| scope.includes(instance.binding))
                        .count(),
                )
                .map_err(|_| OccurrenceGraphError::Capacity)?;
                let edges = graph
                    .occurrence_count(id)
                    .checked_mul(instances)
                    .ok_or(OccurrenceGraphError::Capacity)?;
                total
                    .checked_add(edges)
                    .ok_or(OccurrenceGraphError::Capacity)
            })?;
    if node_count > u64::from(u32::MAX) {
        return Err(OccurrenceGraphError::Capacity);
    }
    let node_count = usize::try_from(node_count).map_err(|_| OccurrenceGraphError::Capacity)?;
    let bytes_per_node = std::mem::size_of::<Option<OccurrenceId>>()
        .checked_add(std::mem::size_of::<DefinitionId>())
        .and_then(|bytes| bytes.checked_add(std::mem::size_of::<Option<InstanceOrdinal>>()))
        .and_then(|bytes| bytes.checked_add(std::mem::size_of::<Option<LinkBinding>>()))
        .ok_or(OccurrenceGraphError::Capacity)?;
    let storage_bytes = node_count
        .checked_mul(bytes_per_node)
        .ok_or(OccurrenceGraphError::Capacity)?;
    Ok(OccurrenceLayout {
        node_count,
        storage_bytes,
    })
}

fn reserve_occurrence_arena<T>(
    arena: &mut Vec<T>,
    layout: OccurrenceLayout,
) -> Result<(), OccurrenceGraphError> {
    arena
        .try_reserve_exact(layout.node_count)
        .map_err(|_| OccurrenceGraphError::Allocation {
            required_bytes: layout.storage_bytes,
        })
}

pub(super) fn compute_postorder(
    definitions: &[Definition],
    names: &NameTable,
    root: DefinitionId,
) -> Result<Vec<DefinitionId>, DefinitionGraphError> {
    struct Frame {
        definition: DefinitionId,
        next_instance: usize,
    }

    let mut colors = vec![0u8; definitions.len()];
    let mut stack = vec![Frame {
        definition: root,
        next_instance: 0,
    }];
    let mut path = vec![root];
    let mut postorder = Vec::new();
    colors[root.index()] = 1;

    while let Some(frame) = stack.last_mut() {
        let instances = &definitions[frame.definition.index()].instances;
        let mut target = None;
        while let Some(instance) = instances.get(frame.next_instance) {
            frame.next_instance += 1;
            if let LinkBinding::Design { definition, .. } = instance.binding {
                target = Some(definition);
                break;
            }
        }
        if let Some(target) = target {
            match colors[target.index()] {
                0 => {
                    colors[target.index()] = 1;
                    stack.push(Frame {
                        definition: target,
                        next_instance: 0,
                    });
                    path.push(target);
                }
                1 => {
                    let cycle_start = path
                        .iter()
                        .position(|candidate| *candidate == target)
                        .expect("visiting definitions are present in the DFS path");
                    let mut cycle = path[cycle_start..]
                        .iter()
                        .map(|id| {
                            names
                                .resolve(definitions[id.index()].name)
                                .expect("definitions reference interned names")
                                .to_string()
                        })
                        .collect::<Vec<_>>();
                    cycle.push(
                        names
                            .resolve(definitions[target.index()].name)
                            .expect("definitions reference interned names")
                            .to_string(),
                    );
                    return Err(DefinitionGraphError::RecursiveHierarchy(cycle));
                }
                2 => {}
                _ => unreachable!("DFS colors are limited to 0, 1, and 2"),
            }
            continue;
        }

        let finished = stack.pop().expect("the DFS stack is not empty").definition;
        path.pop();
        colors[finished.index()] = 2;
        postorder.push(finished);
    }
    Ok(postorder)
}

pub(super) fn count_occurrences(
    definitions: &[Definition],
    names: &NameTable,
    root: DefinitionId,
    postorder: &[DefinitionId],
) -> Result<(Vec<u64>, u64), DefinitionGraphError> {
    let mut counts = vec![0u64; definitions.len()];
    counts[root.index()] = 1;
    let mut unresolved = 0u64;
    for parent in postorder.iter().rev().copied() {
        let parent_count = counts[parent.index()];
        for instance in &definitions[parent.index()].instances {
            match instance.binding {
                LinkBinding::Design {
                    definition: child, ..
                } => {
                    counts[child.index()] = counts[child.index()]
                        .checked_add(parent_count)
                        .ok_or_else(|| {
                            DefinitionGraphError::OccurrenceOverflow(
                                names
                                    .resolve(definitions[child.index()].name)
                                    .expect("definitions reference interned names")
                                    .to_string(),
                            )
                        })?;
                }
                LinkBinding::Unresolved => {
                    unresolved = unresolved.checked_add(parent_count).ok_or_else(|| {
                        DefinitionGraphError::OccurrenceOverflow(
                            names
                                .resolve(instance.reference)
                                .expect("instance references use interned names")
                                .to_string(),
                        )
                    })?;
                }
                LinkBinding::External { .. } => {}
            }
        }
    }
    Ok((counts, unresolved))
}
