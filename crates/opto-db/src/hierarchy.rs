// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Definition linking and compact hierarchy occurrence identities.
//!
//! [`DefinitionGraph`] resolves instance references through an ordered provider
//! list without expanding the hierarchy. [`OccurrenceGraph`] performs that
//! expansion explicitly when a consumer needs occurrence-local state.

use opto_core::{DenseId, NameId, NameTable, OwnerToken};
use std::fmt;

mod occurrence;

pub use occurrence::OccurrenceGraph;
use occurrence::{compute_postorder, count_occurrences};

enum DefinitionTag {}
enum ProviderTag {}
enum OccurrenceTag {}
enum InstanceOrdinalTag {}
pub(super) enum DefinitionGraphOwner {}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
/// A dense definition identity scoped to one [`DefinitionGraph`].
///
/// The ID is an index, not a persistent database key. It must not be mixed with
/// an ID obtained from another graph.
pub struct DefinitionId(DenseId<DefinitionTag>);

impl DefinitionId {
    fn from_index(index: usize) -> Result<Self, DefinitionGraphError> {
        DenseId::from_index(index)
            .map(Self)
            .map_err(|_| DefinitionGraphError::Capacity)
    }

    fn index(self) -> usize {
        self.0.index()
    }
}

impl fmt::Debug for DefinitionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("DefinitionId")
            .field(&self.index())
            .finish()
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
/// A dense identity scoped to one sealed [`OccurrenceGraph`].
///
/// IDs are assigned by a deterministic source-order preorder traversal. The
/// first ID always identifies the root design occurrence. The identity remains
/// one 32-bit word and deliberately carries no graph tag, so callers must not
/// mix IDs obtained from different graphs.
pub struct OccurrenceId(DenseId<OccurrenceTag>);

impl OccurrenceId {
    /// The root occurrence, which is always the first materialized node.
    pub const ROOT: Self = Self(DenseId::FIRST);

    fn from_index(index: usize) -> Result<Self, OccurrenceGraphError> {
        DenseId::from_index(index)
            .map(Self)
            .map_err(|_| OccurrenceGraphError::Capacity)
    }

    #[must_use]
    /// Returns this ID's zero-based arena index.
    pub fn index(self) -> usize {
        self.0.index()
    }
}

impl fmt::Debug for OccurrenceId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("OccurrenceId")
            .field(&self.index())
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct InstanceOrdinal(DenseId<InstanceOrdinalTag>);

impl InstanceOrdinal {
    fn from_index(index: usize) -> Result<Self, OccurrenceGraphError> {
        DenseId::from_index(index)
            .map(Self)
            .map_err(|_| OccurrenceGraphError::Capacity)
    }

    fn index(self) -> usize {
        self.0.index()
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
/// A dense provider identity scoped to one [`DefinitionGraph`].
///
/// Rebuilding from identical ordered inputs reproduces the same IDs, but an ID
/// must not be persisted across design or library revisions.
pub struct ProviderId(DenseId<ProviderTag>);

impl ProviderId {
    fn from_index(index: usize) -> Result<Self, DefinitionGraphError> {
        DenseId::from_index(index)
            .map(Self)
            .map_err(|_| DefinitionGraphError::Capacity)
    }

    #[must_use]
    /// Returns this ID's zero-based provider-table index.
    pub fn index(self) -> usize {
        self.0.index()
    }
}

impl fmt::Debug for ProviderId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("ProviderId")
            .field(&self.index())
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Unlinked input for one design definition.
pub struct DefinitionInput {
    /// The unique design name.
    pub name: String,
    /// Instances in stable source order.
    pub instances: Vec<InstanceInput>,
}

impl DefinitionInput {
    /// Creates a definition input from its name and ordered instances.
    #[must_use]
    pub fn new(name: impl Into<String>, instances: Vec<InstanceInput>) -> Self {
        Self {
            name: name.into(),
            instances,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Unlinked input for one instance declaration.
pub struct InstanceInput {
    /// The instance name within its containing definition.
    pub name: String,
    /// The design or external symbol named by the instance.
    pub reference: String,
}

impl InstanceInput {
    #[must_use]
    /// Creates an instance input from its local name and referenced symbol.
    pub fn new(name: impl Into<String>, reference: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            reference: reference.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// The namespace supplied by a hierarchy link provider.
pub enum LinkProviderKind {
    /// Design definitions supplied to [`DefinitionGraph::build`].
    Definitions,
    /// Opaque implementation symbols supplied by a library or black-box set.
    External,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Ordered input describing one hierarchy link provider.
///
/// During linking, the first provider that contains a reference wins.
pub struct LinkProviderInput {
    label: String,
    kind: LinkProviderInputKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum LinkProviderInputKind {
    Definitions,
    External(Vec<String>),
}

impl LinkProviderInput {
    /// Creates a provider that exposes the graph's design definitions.
    #[must_use]
    pub fn definitions(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            kind: LinkProviderInputKind::Definitions,
        }
    }

    /// Creates an external provider exposing the given symbol names.
    #[must_use]
    pub fn external(label: impl Into<String>, symbols: impl IntoIterator<Item = String>) -> Self {
        Self {
            label: label.into(),
            kind: LinkProviderInputKind::External(symbols.into_iter().collect()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StoredLinkProvider {
    id: ProviderId,
    label: NameId,
    kind: LinkProviderKind,
}

#[derive(Clone, Copy)]
/// Borrowed metadata for a linked provider.
pub struct LinkProvider<'a> {
    names: &'a NameTable,
    stored: &'a StoredLinkProvider,
}

impl<'a> LinkProvider<'a> {
    #[must_use]
    /// Returns the provider's graph-local identity.
    pub fn id(&self) -> ProviderId {
        self.stored.id
    }

    #[must_use]
    /// Returns the provider's diagnostic label.
    ///
    /// # Panics
    ///
    /// Panics only if a private provider label no longer resolves in the graph's
    /// owned name table.
    pub fn label(&self) -> &'a str {
        self.names
            .resolve(self.stored.label)
            .expect("provider labels reference interned names")
    }

    #[must_use]
    /// Returns the namespace kind supplied by this provider.
    pub fn kind(&self) -> LinkProviderKind {
        self.stored.kind
    }
}

impl fmt::Debug for LinkProvider<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LinkProvider")
            .field("id", &self.id())
            .field("label", &self.label())
            .field("kind", &self.kind())
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// The concrete provider selected for one instance reference.
pub enum LinkBinding {
    /// The reference resolves to another design definition.
    Design {
        /// The provider that won ordered lookup.
        provider: ProviderId,
        /// The linked child definition.
        definition: DefinitionId,
    },
    /// The reference resolves to an opaque external symbol.
    External {
        /// The provider that won ordered lookup.
        provider: ProviderId,
    },
    /// No provider contains the reference.
    Unresolved,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StoredDefinitionInstance {
    name: NameId,
    reference: NameId,
    binding: LinkBinding,
}

#[derive(Clone, Copy)]
/// Borrowed view of an instance stored in a [`DefinitionGraph`].
pub struct DefinitionInstance<'a> {
    names: &'a NameTable,
    stored: &'a StoredDefinitionInstance,
}

impl<'a> DefinitionInstance<'a> {
    /// Returns the instance name within its containing definition.
    ///
    /// # Panics
    ///
    /// Panics only if the stored instance name violates the graph's interned-name
    /// invariant.
    #[must_use]
    pub fn name(&self) -> &'a str {
        self.names
            .resolve(self.stored.name)
            .expect("instances reference interned names")
    }

    /// Returns the unresolved source reference text.
    ///
    /// # Panics
    ///
    /// Panics only if the stored source reference violates the graph's
    /// interned-name invariant.
    #[must_use]
    pub fn reference(&self) -> &'a str {
        self.names
            .resolve(self.stored.reference)
            .expect("instance references use interned names")
    }

    /// Returns the result of ordered provider linking.
    #[must_use]
    pub fn binding(&self) -> LinkBinding {
        self.stored.binding
    }

    #[must_use]
    /// Returns the interned instance-name identity.
    pub fn name_id(&self) -> NameId {
        self.stored.name
    }

    #[must_use]
    /// Returns the interned reference-name identity.
    pub fn reference_id(&self) -> NameId {
        self.stored.reference
    }
}

impl fmt::Debug for DefinitionInstance<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DefinitionInstance")
            .field("name", &self.name())
            .field("reference", &self.reference())
            .field("binding", &self.binding())
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Definition {
    name: NameId,
    instances: Box<[StoredDefinitionInstance]>,
}

#[derive(Debug, Clone)]
/// A sealed, linked graph of definitions and their instance references.
///
/// The graph retains definitions once and computes occurrence multiplicities
/// without expanding instance paths. Its dense IDs are valid only for this
/// graph revision.
pub struct DefinitionGraph {
    // Clones share this token; independently built graphs never do.
    identity: OwnerToken<DefinitionGraphOwner>,
    root: DefinitionId,
    names: NameTable,
    definitions: Box<[Definition]>,
    definition_by_name: Box<[Option<DefinitionId>]>,
    providers: Box<[StoredLinkProvider]>,
    postorder: Box<[DefinitionId]>,
    occurrence_counts: Box<[u64]>,
    unresolved_occurrences: u64,
    first_unresolved: Option<FirstUnresolved>,
}

impl PartialEq for DefinitionGraph {
    fn eq(&self, other: &Self) -> bool {
        self.root == other.root
            && self.names == other.names
            && self.definitions == other.definitions
            && self.definition_by_name == other.definition_by_name
            && self.providers == other.providers
            && self.postorder == other.postorder
            && self.occurrence_counts == other.occurrence_counts
            && self.unresolved_occurrences == other.unresolved_occurrences
            && self.first_unresolved == other.first_unresolved
    }
}

impl Eq for DefinitionGraph {}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FirstUnresolved {
    path: Box<[NameId]>,
    parent: DefinitionId,
    instance: usize,
}

impl DefinitionGraph {
    /// Builds a linked hierarchy using providers strictly from left to right.
    ///
    /// Definitions are searchable only at an explicit definitions provider;
    /// the root remains the caller-selected definition regardless of provider
    /// order. Later matches are discarded because the first provider owns the
    /// binding unambiguously.
    ///
    /// # Errors
    ///
    /// Returns [`DefinitionGraphError`] for empty/duplicate definitions,
    /// invalid providers or root, unresolved references, recursive hierarchy,
    /// or compact name/ID capacity overflow.
    pub fn build(
        definitions: impl IntoIterator<Item = DefinitionInput>,
        providers: impl IntoIterator<Item = LinkProviderInput>,
        root: &str,
    ) -> Result<Self, DefinitionGraphError> {
        Self::build_checked(definitions, providers, root)
    }

    #[allow(
        clippy::too_many_lines,
        reason = "graph construction shares one name table and provider-order binding transaction"
    )]
    fn build_checked(
        definitions: impl IntoIterator<Item = DefinitionInput>,
        providers: impl IntoIterator<Item = LinkProviderInput>,
        root: &str,
    ) -> Result<Self, DefinitionGraphError> {
        let inputs = definitions.into_iter().collect::<Vec<_>>();
        let mut names = NameTable::new();
        let mut definition_by_name = vec![None; names.entry_count()];
        let mut canonical_names = Vec::with_capacity(inputs.len());
        for (index, input) in inputs.iter().enumerate() {
            if input.name.is_empty() {
                return Err(DefinitionGraphError::EmptyDefinitionName);
            }
            let name = names
                .intern(&input.name)
                .map_err(|_| DefinitionGraphError::Capacity)?;
            let id = DefinitionId::from_index(index)?;
            definition_by_name.resize(names.entry_count(), None);
            if definition_by_name[name.raw() as usize]
                .replace(id)
                .is_some()
            {
                return Err(DefinitionGraphError::DuplicateDefinition(
                    input.name.clone(),
                ));
            }
            canonical_names.push(name);
        }
        let root = names
            .get(root)
            .and_then(|name| definition_by_name[name.raw() as usize])
            .ok_or_else(|| DefinitionGraphError::MissingRoot(root.to_string()))?;

        for input in &inputs {
            for instance in &input.instances {
                if instance.name.is_empty() {
                    return Err(DefinitionGraphError::EmptyInstanceName(input.name.clone()));
                }
                if instance.reference.is_empty() {
                    return Err(DefinitionGraphError::EmptyReference {
                        definition: input.name.clone(),
                        instance: instance.name.clone(),
                    });
                }
                names
                    .intern(&instance.name)
                    .map_err(|_| DefinitionGraphError::Capacity)?;
                names
                    .intern(&instance.reference)
                    .map_err(|_| DefinitionGraphError::Capacity)?;
            }
        }

        let mut link_providers = Vec::new();
        let mut candidates = vec![None; names.entry_count()];
        for (index, input) in providers.into_iter().enumerate() {
            if input.label.is_empty() {
                return Err(DefinitionGraphError::EmptyProviderLabel);
            }
            let id = ProviderId::from_index(index)?;
            let label = names
                .intern(&input.label)
                .map_err(|_| DefinitionGraphError::Capacity)?;
            candidates.resize(names.entry_count(), None);
            let (kind, symbols) = match input.kind {
                LinkProviderInputKind::Definitions => {
                    for (definition, name) in canonical_names.iter().copied().enumerate() {
                        candidates[name.raw() as usize].get_or_insert(LinkBinding::Design {
                            provider: id,
                            definition: DefinitionId::from_index(definition)?,
                        });
                    }
                    (LinkProviderKind::Definitions, None)
                }
                LinkProviderInputKind::External(symbols) => {
                    (LinkProviderKind::External, Some(symbols))
                }
            };
            if let Some(symbols) = symbols {
                for symbol in symbols {
                    if symbol.is_empty() {
                        return Err(DefinitionGraphError::EmptyProviderSymbol(
                            input.label.clone(),
                        ));
                    }
                    if let Some(symbol) = names.get(&symbol) {
                        candidates[symbol.raw() as usize]
                            .get_or_insert(LinkBinding::External { provider: id });
                    }
                }
            }
            link_providers.push(StoredLinkProvider { id, label, kind });
        }

        let mut graph_definitions = Vec::with_capacity(inputs.len());
        for (input, name) in inputs.into_iter().zip(canonical_names) {
            let mut instances = Vec::with_capacity(input.instances.len());
            for instance in input.instances {
                let instance_name = names
                    .get(&instance.name)
                    .expect("instance names were interned during validation");
                let reference = names
                    .get(&instance.reference)
                    .expect("instance references were interned during validation");
                let binding =
                    candidates[reference.raw() as usize].unwrap_or(LinkBinding::Unresolved);
                instances.push(StoredDefinitionInstance {
                    name: instance_name,
                    reference,
                    binding,
                });
            }
            graph_definitions.push(Definition {
                name,
                instances: instances.into_boxed_slice(),
            });
        }

        names.freeze().map_err(|_| DefinitionGraphError::Capacity)?;
        definition_by_name.resize(names.entry_count(), None);
        let postorder = compute_postorder(&graph_definitions, &names, root)?;
        let (occurrence_counts, unresolved_occurrences) =
            count_occurrences(&graph_definitions, &names, root, &postorder)?;
        let first_unresolved = find_first_unresolved(&graph_definitions, root, &postorder);
        debug_assert_eq!(first_unresolved.is_some(), unresolved_occurrences != 0);
        Ok(Self {
            identity: OwnerToken::fresh(),
            root,
            names,
            definitions: graph_definitions.into_boxed_slice(),
            definition_by_name: definition_by_name.into_boxed_slice(),
            providers: link_providers.into_boxed_slice(),
            postorder: postorder.into_boxed_slice(),
            occurrence_counts: occurrence_counts.into_boxed_slice(),
            unresolved_occurrences,
            first_unresolved,
        })
    }

    #[must_use]
    /// Returns the selected root definition.
    pub fn root(&self) -> DefinitionId {
        self.root
    }

    /// Returns the number of definitions, including unreachable definitions.
    #[must_use]
    pub fn len(&self) -> usize {
        self.definitions.len()
    }

    /// Returns `true` when the graph contains no definitions.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.definitions.is_empty()
    }

    /// Looks up a definition by its exact name.
    #[must_use]
    pub fn definition_id(&self, name: &str) -> Option<DefinitionId> {
        let name = self.names.get(name)?;
        self.definition_by_name
            .get(name.raw() as usize)
            .copied()
            .flatten()
    }

    ///
    /// # Panics
    ///
    /// Panics if `id` indexes outside this graph's definition arena.
    #[must_use]
    pub fn definition_name(&self, id: DefinitionId) -> &str {
        self.names
            .resolve(self.definition(id).name)
            .expect("definitions reference interned names")
    }

    /// Iterates over a definition's instances in source order.
    ///
    /// # Panics
    ///
    /// Panics if `id` indexes outside this graph's definition arena.
    #[must_use]
    pub fn instances(
        &self,
        id: DefinitionId,
    ) -> impl ExactSizeIterator<Item = DefinitionInstance<'_>> {
        let names = &self.names;
        self.definition(id)
            .instances
            .iter()
            .map(move |stored| DefinitionInstance { names, stored })
    }

    /// Returns the instance at `ordinal`, or `None` for an invalid ID or ordinal.
    #[must_use]
    pub fn instance(&self, id: DefinitionId, ordinal: usize) -> Option<DefinitionInstance<'_>> {
        self.definitions
            .get(id.index())?
            .instances
            .get(ordinal)
            .map(|stored| DefinitionInstance {
                names: &self.names,
                stored,
            })
    }

    /// Iterates over providers in link-search order.
    #[must_use]
    pub fn providers(&self) -> impl ExactSizeIterator<Item = LinkProvider<'_>> {
        let names = &self.names;
        self.providers
            .iter()
            .map(move |stored| LinkProvider { names, stored })
    }

    ///
    /// # Panics
    ///
    /// Panics if `id` indexes outside this graph's provider arena.
    #[must_use]
    pub fn provider(&self, id: ProviderId) -> LinkProvider<'_> {
        LinkProvider {
            names: &self.names,
            stored: &self.providers[id.index()],
        }
    }

    /// Returns reachable definitions in child-before-parent order.
    #[must_use]
    pub fn postorder(&self) -> &[DefinitionId] {
        &self.postorder
    }

    /// Returns how many hierarchy occurrences instantiate `id`.
    ///
    /// The root contributes one occurrence. Unreachable definitions have zero.
    ///
    /// # Panics
    ///
    /// Panics if `id` indexes outside this graph's definition arena.
    #[must_use]
    pub fn occurrence_count(&self, id: DefinitionId) -> u64 {
        self.occurrence_counts[id.index()]
    }

    /// Returns whether `id` is reachable from the root.
    ///
    /// # Panics
    ///
    /// Panics if `id` indexes outside this graph's definition arena.
    #[must_use]
    pub fn is_reachable(&self, id: DefinitionId) -> bool {
        self.occurrence_count(id) != 0
    }

    /// Returns the expanded number of instance occurrences left unresolved.
    #[must_use]
    pub fn unresolved_occurrence_count(&self) -> u64 {
        self.unresolved_occurrences
    }

    /// Returns whether every reachable instance reference is linked.
    #[must_use]
    pub fn is_linked(&self) -> bool {
        self.unresolved_occurrences == 0
    }

    /// Returns the first unresolved occurrence in deterministic source order.
    #[must_use]
    pub fn first_unresolved(&self) -> Option<UnresolvedOccurrence<'_>> {
        let unresolved = self.first_unresolved.as_ref()?;
        Some(UnresolvedOccurrence {
            graph: self,
            unresolved,
        })
    }

    fn definition(&self, id: DefinitionId) -> &Definition {
        &self.definitions[id.index()]
    }
}

#[derive(Clone, Copy)]
enum UnresolvedStep {
    Terminal {
        instance: usize,
    },
    Child {
        instance: usize,
        definition: DefinitionId,
    },
}

fn find_first_unresolved(
    definitions: &[Definition],
    root: DefinitionId,
    postorder: &[DefinitionId],
) -> Option<FirstUnresolved> {
    let mut steps = vec![None; definitions.len()];
    for definition in postorder.iter().copied() {
        steps[definition.index()] = definitions[definition.index()]
            .instances
            .iter()
            .enumerate()
            .find_map(|(instance, child)| match child.binding {
                LinkBinding::Unresolved => Some(UnresolvedStep::Terminal { instance }),
                LinkBinding::Design { definition, .. } if steps[definition.index()].is_some() => {
                    Some(UnresolvedStep::Child {
                        instance,
                        definition,
                    })
                }
                LinkBinding::Design { .. } | LinkBinding::External { .. } => None,
            });
    }

    let mut definition = root;
    let mut path = Vec::new();
    loop {
        match steps[definition.index()]? {
            UnresolvedStep::Terminal { instance } => {
                path.push(definitions[definition.index()].instances[instance].name);
                return Some(FirstUnresolved {
                    path: path.into_boxed_slice(),
                    parent: definition,
                    instance,
                });
            }
            UnresolvedStep::Child {
                instance,
                definition: child,
            } => {
                path.push(definitions[definition.index()].instances[instance].name);
                definition = child;
            }
        }
    }
}

fn append_path(path: &mut String, instance: &str) {
    if !path.is_empty() {
        path.push('/');
    }
    path.push_str(instance);
}

#[derive(Clone, Copy)]
/// Borrowed diagnostic view of an unresolved hierarchy occurrence.
pub struct UnresolvedOccurrence<'a> {
    graph: &'a DefinitionGraph,
    unresolved: &'a FirstUnresolved,
}

impl<'a> UnresolvedOccurrence<'a> {
    /// Formats the root-relative slash-separated instance path.
    ///
    /// # Panics
    ///
    /// Panics only if a stored unresolved path component no longer resolves in
    /// the graph-owned name table.
    #[must_use]
    pub fn path(&self) -> String {
        let mut path = String::new();
        for name in &self.unresolved.path {
            append_path(
                &mut path,
                self.graph
                    .names
                    .resolve(*name)
                    .expect("unresolved paths reference interned names"),
            );
        }
        path
    }

    /// Returns the definition containing the unresolved instance.
    #[must_use]
    pub fn parent(&self) -> DefinitionId {
        self.unresolved.parent
    }

    #[must_use]
    /// Returns the unresolved instance name.
    pub fn name(&self) -> &'a str {
        self.instance().name()
    }

    /// Returns the unresolved reference text.
    #[must_use]
    pub fn reference(&self) -> &'a str {
        self.instance().reference()
    }

    /// Returns the instance binding, which is [`LinkBinding::Unresolved`].
    #[must_use]
    pub fn binding(&self) -> LinkBinding {
        self.instance().binding()
    }

    fn instance(&self) -> DefinitionInstance<'a> {
        self.graph
            .instance(self.unresolved.parent, self.unresolved.instance)
            .expect("first-unresolved metadata references a live instance")
    }
}

impl fmt::Debug for UnresolvedOccurrence<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UnresolvedOccurrence")
            .field("path", &self.path())
            .field("parent", &self.parent())
            .field("instance", &self.instance())
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Failure to size or materialize a hierarchy occurrence graph.
pub enum OccurrenceGraphError {
    /// The graph exceeds the compact ID or addressable storage capacity.
    Capacity,
    /// Allocation failed for the sealed occurrence arenas.
    Allocation {
        /// The total arena payload requested by materialization.
        required_bytes: usize,
    },
    /// Materialization was requested before all occurrences were linked.
    UnresolvedHierarchy {
        /// The expanded number of unresolved occurrences.
        occurrences: u64,
    },
}

impl fmt::Display for OccurrenceGraphError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Capacity => {
                formatter.write_str("occurrence graph exceeds 32-bit ID or storage capacity")
            }
            Self::Allocation { required_bytes } => write!(
                formatter,
                "cannot allocate {required_bytes} bytes for the occurrence graph"
            ),
            Self::UnresolvedHierarchy { occurrences } => write!(
                formatter,
                "cannot materialize an occurrence graph with {occurrences} unresolved occurrences"
            ),
        }
    }
}

impl std::error::Error for OccurrenceGraphError {}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Invalid input or capacity failure while building a definition graph.
pub enum DefinitionGraphError {
    /// A compact ID or occurrence count exceeds its supported capacity.
    Capacity,
    /// A definition has an empty name.
    EmptyDefinitionName,
    /// More than one definition has the enclosed name.
    DuplicateDefinition(String),
    /// The requested root definition does not exist.
    MissingRoot(String),
    /// A link provider has an empty diagnostic label.
    EmptyProviderLabel,
    /// The named provider exposes an empty symbol.
    EmptyProviderSymbol(String),
    /// The named definition contains an empty instance name.
    EmptyInstanceName(String),
    /// An instance has an empty reference.
    EmptyReference {
        /// The containing definition name.
        definition: String,
        /// The instance name.
        instance: String,
    },
    /// Reachable definitions contain the enclosed recursion cycle.
    RecursiveHierarchy(Vec<String>),
    /// Expanding the named definition would overflow the occurrence count.
    OccurrenceOverflow(String),
}

impl fmt::Display for DefinitionGraphError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Capacity => formatter.write_str("definition graph exceeds 32-bit ID capacity"),
            Self::EmptyDefinitionName => formatter.write_str("design name cannot be empty"),
            Self::DuplicateDefinition(name) => write!(formatter, "duplicate design '{name}'"),
            Self::MissingRoot(name) => write!(formatter, "design '{name}' is missing from RTL IR"),
            Self::EmptyProviderLabel => formatter.write_str("link provider label cannot be empty"),
            Self::EmptyProviderSymbol(provider) => {
                write!(
                    formatter,
                    "link provider '{provider}' contains an empty symbol"
                )
            }
            Self::EmptyInstanceName(definition) => {
                write!(
                    formatter,
                    "design '{definition}' contains an unnamed instance"
                )
            }
            Self::EmptyReference {
                definition,
                instance,
            } => write!(
                formatter,
                "instance '{instance}' in design '{definition}' has an empty reference"
            ),
            Self::RecursiveHierarchy(cycle) => write!(
                formatter,
                "recursive design hierarchy is not supported: {}",
                cycle.join(" -> ")
            ),
            Self::OccurrenceOverflow(name) => {
                write!(
                    formatter,
                    "occurrence count for '{name}' exceeds 64-bit capacity"
                )
            }
        }
    }
}

impl std::error::Error for DefinitionGraphError {}

#[cfg(test)]
mod tests;
