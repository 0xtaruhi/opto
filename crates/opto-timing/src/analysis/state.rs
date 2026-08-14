// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

mod slots;

pub(super) use slots::*;

#[derive(Debug)]
pub(crate) struct PropagationState {
    pub(super) arrivals: ArrivalSlotStore,
    pub(super) requireds: RequiredSlotStore,
    pub(super) paths: Option<PathArena>,
    pub(super) origins: OriginArena,
    pub(super) tags: TagArena,
}

impl PropagationState {
    pub(crate) const fn tracks_paths(&self) -> bool {
        self.paths.is_some()
    }

    pub(crate) fn owned_memory_bytes(&self) -> usize {
        let arrivals = self.arrivals.owned_memory_bytes();
        let requireds = self.requireds.owned_memory_bytes();
        let paths = self.paths.as_ref().map_or(0, |paths| {
            opto_core::resident::slice_bytes::<PathNode>(paths.nodes.capacity()).saturating_add(
                paths
                    .nodes
                    .iter()
                    .map(|node| opto_core::resident::allocation_bytes(node.step.point.capacity()))
                    .sum::<usize>(),
            )
        });
        let origins =
            opto_core::resident::slice_bytes::<ArrivalOrigin>(self.origins.values.capacity())
                .saturating_add(btree_memory_bytes::<OriginKey, OriginId>(
                    self.origins.ids.len(),
                ))
                .saturating_add(
                    self.origins
                        .values
                        .iter()
                        .map(|origin| {
                            opto_core::resident::allocation_bytes(origin.startpoint.capacity())
                                .saturating_add(opto_core::resident::allocation_bytes(
                                    origin.startpoint_description.capacity(),
                                ))
                        })
                        .sum::<usize>(),
                );
        let tag_spill = |key: &TagKey| {
            if key.path_exceptions.spilled() {
                opto_core::resident::slice_bytes::<ExceptionCandidate>(
                    key.path_exceptions.capacity(),
                )
            } else {
                0
            }
        };
        let tags = opto_core::resident::slice_bytes::<TagKey>(self.tags.entries.value_capacity())
            .saturating_add(btree_memory_bytes::<TagKey, TagId>(self.tags.entries.len()))
            .saturating_add(
                self.tags
                    .entries
                    .values()
                    .iter()
                    .map(tag_spill)
                    .sum::<usize>(),
            )
            .saturating_add(
                self.tags
                    .entries
                    .reverse_keys()
                    .map(tag_spill)
                    .sum::<usize>(),
            );
        arrivals
            .saturating_add(requireds)
            .saturating_add(paths)
            .saturating_add(origins)
            .saturating_add(tags)
    }
}

fn btree_memory_bytes<K, V>(len: usize) -> usize {
    opto_core::resident::slice_bytes::<(K, V, [usize; 4])>(len)
}

#[derive(Debug, Clone, Copy)]
pub(super) struct RequiredState {
    pub(super) tag: TagId,
    pub(super) required: f64,
}

impl PartialEq for RequiredState {
    fn eq(&self, other: &Self) -> bool {
        self.tag == other.tag && self.required.to_bits() == other.required.to_bits()
    }
}

impl Eq for RequiredState {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct PathId(u32);

#[derive(Debug)]
pub(super) struct PathNode {
    pub(super) previous: Option<PathId>,
    pub(super) step: PathStep,
}

#[derive(Debug, Default)]
pub(super) struct PathArena {
    pub(super) nodes: Vec<PathNode>,
}

impl PathArena {
    pub(super) fn push(
        &mut self,
        previous: Option<PathId>,
        step: PathStep,
    ) -> Result<PathId, crate::TimingError> {
        if self.nodes.len() >= u32::MAX as usize {
            return Err(crate::TimingAnalysisError::Capacity {
                resource: "path predecessor arena",
            }
            .into());
        }
        let id = PathId(self.nodes.len().try_into().map_err(|_| {
            crate::TimingAnalysisError::Capacity {
                resource: "path predecessor arena",
            }
        })?);
        self.nodes.push(PathNode { previous, step });
        Ok(id)
    }

    pub(super) fn chain(
        &mut self,
        mut previous: Option<PathId>,
        steps: impl IntoIterator<Item = PathStep>,
    ) -> Result<PathId, crate::TimingError> {
        for step in steps {
            previous = Some(self.push(previous, step)?);
        }
        previous.ok_or_else(|| {
            crate::TimingAnalysisError::EmptyPath {
                operation: "create",
            }
            .into()
        })
    }

    pub(super) fn materialize(&self, end: PathId) -> Result<Vec<PathStep>, crate::TimingError> {
        let mut steps = Vec::new();
        let mut current = Some(end);
        while let Some(id) = current {
            let node = self
                .nodes
                .get(
                    usize::try_from(id.0).map_err(|_| crate::TimingAnalysisError::Capacity {
                        resource: "path predecessor index",
                    })?,
                )
                .ok_or(crate::TimingAnalysisError::UnknownPathPredecessor { id: id.0 })?;
            steps.push(node.step.clone());
            current = node.previous;
        }
        steps.reverse();
        Ok(steps)
    }

    /// Rebuilds the predecessor arena from paths still referenced by arrivals.
    ///
    /// Every live path ID is remapped together with the arena; compaction may
    /// not cross an edit journal that still contains old path IDs.
    pub(super) fn compact(
        &mut self,
        arrivals: &mut ArrivalSlotStore,
    ) -> Result<(), crate::TimingError> {
        let mut compacted = Self::default();
        let mut remapped = std::collections::HashMap::new();
        let Some(path_ids) = arrivals.path_ids_mut() else {
            return Ok(());
        };
        for path in path_ids {
            *path = self
                .copy_path(PathId(*path), &mut compacted, &mut remapped)?
                .0;
        }
        *self = compacted;
        Ok(())
    }

    pub(super) fn copy_path(
        &self,
        end: PathId,
        target: &mut Self,
        remapped: &mut std::collections::HashMap<PathId, PathId>,
    ) -> Result<PathId, crate::TimingError> {
        if let Some(&mapped) = remapped.get(&end) {
            return Ok(mapped);
        }
        let mut pending = Vec::new();
        let mut current = Some(end);
        let mut previous = None;
        while let Some(id) = current {
            if let Some(&mapped) = remapped.get(&id) {
                previous = Some(mapped);
                break;
            }
            let node = self.node(id)?;
            pending.push(id);
            current = node.previous;
        }
        for id in pending.into_iter().rev() {
            let node = self.node(id)?;
            let mapped = target.push(previous, node.step.clone())?;
            remapped.insert(id, mapped);
            previous = Some(mapped);
        }
        previous.ok_or_else(|| {
            crate::TimingAnalysisError::EmptyPath {
                operation: "compact",
            }
            .into()
        })
    }

    pub(super) fn node(&self, id: PathId) -> Result<&PathNode, crate::TimingError> {
        let index = usize::try_from(id.0).map_err(|_| crate::TimingAnalysisError::Capacity {
            resource: "path predecessor index",
        })?;
        self.nodes
            .get(index)
            .ok_or_else(|| crate::TimingAnalysisError::UnknownPathPredecessor { id: id.0 }.into())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct OriginId(u32);

impl OriginId {
    pub(super) const fn raw(self) -> u32 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum OriginKey {
    PrimaryInput {
        port: usize,
        delay_row: Option<usize>,
    },
    Sequential {
        net: usize,
        launch: usize,
        clock: ClockSlot,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct TagId(u32);

impl opto_core::ArenaIndex for TagId {
    type Error = crate::TimingError;

    fn try_from_index(index: usize) -> Result<Self, Self::Error> {
        let raw = u32::try_from(index).map_err(|_| crate::TimingAnalysisError::Capacity {
            resource: "arrival tag arena",
        })?;
        Ok(Self(raw))
    }

    fn index(self) -> usize {
        self.0 as usize
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct TagKey {
    pub(super) launch_domain: LaunchDomain,
    pub(super) path_exceptions: SmallVec<[ExceptionCandidate; 1]>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum LaunchDomain {
    PrimaryInput,
    Clock { clock: ClockSlot, edge: TimingEdge },
}

#[derive(Debug, Default)]
pub(super) struct TagArena {
    entries: opto_core::DenseInterner<TagId, TagKey>,
}

impl TagArena {
    pub(super) fn len(&self) -> usize {
        self.entries.len()
    }

    pub(super) fn intern(&mut self, key: TagKey) -> Result<TagId, crate::TimingError> {
        self.entries.intern(key)
    }

    pub(super) fn key(&self, id: TagId) -> Result<&TagKey, crate::TimingError> {
        self.entries
            .get(id)
            .ok_or_else(|| crate::TimingAnalysisError::UnknownArrivalTag { id: id.0 }.into())
    }

    pub(super) fn truncate(&mut self, len: usize) {
        self.entries.truncate(len);
    }

    pub(super) fn intern_family(
        &mut self,
        timing: &TimingContext,
        launch_domain: LaunchDomain,
        candidates: &[ExceptionCandidate],
    ) -> Result<TagId, crate::TimingError> {
        const MAX_VARIANTS: usize = 1 << 20;
        let mut radices = Vec::with_capacity(candidates.len());
        let mut variants = 1usize;
        for candidate in candidates {
            let exception = timing.path_exception_by_slot(candidate.slot).ok_or(
                crate::TimingAnalysisError::UnknownPathException {
                    index: u32::try_from(candidate.slot.index())
                        .expect("timing constraint slots originate from nonzero u32 values"),
                },
            )?;
            let radix = exception.through.len() + 1;
            variants = variants
                .checked_mul(radix)
                .filter(|count| *count <= MAX_VARIANTS)
                .ok_or(crate::TimingAnalysisError::Capacity {
                    resource: "path-exception tag family",
                })?;
            radices.push(radix);
        }

        let mut values = vec![0usize; candidates.len()];
        let mut initial = None;
        for variant in 0..variants {
            let mut key_candidates: SmallVec<[ExceptionCandidate; 1]> =
                SmallVec::from_slice(candidates);
            for (candidate, &progress) in key_candidates.iter_mut().zip(&values) {
                candidate.through_progress = u16::try_from(progress)
                    .expect("path-exception validation bounds through progress");
            }
            let id = self.intern(TagKey {
                launch_domain,
                path_exceptions: key_candidates,
            })?;
            if variant == 0 {
                initial = Some(id);
            }
            for (value, radix) in values.iter_mut().zip(&radices) {
                *value += 1;
                if *value < *radix {
                    break;
                }
                *value = 0;
            }
        }
        initial.ok_or_else(|| {
            crate::TimingAnalysisError::Capacity {
                resource: "empty path-exception tag family",
            }
            .into()
        })
    }

    pub(super) fn advance(
        &self,
        current: TagId,
        timing: &TimingContext,
        points: &[TimingEndpoint],
        edge: TimingEdge,
    ) -> Result<TagId, crate::TimingError> {
        let current_key = self.key(current)?;
        let path_exceptions =
            advance_candidates(timing, &current_key.path_exceptions, points, edge)?;
        if path_exceptions == current_key.path_exceptions {
            return Ok(current);
        }
        let key = TagKey {
            launch_domain: current_key.launch_domain,
            path_exceptions,
        };
        self.entries
            .find(&key)
            .ok_or_else(|| crate::TimingAnalysisError::UnknownArrivalTagTransition.into())
    }
}

#[derive(Debug, Default)]
pub(super) struct OriginArena {
    pub(super) values: Vec<ArrivalOrigin>,
    pub(super) ids: BTreeMap<OriginKey, OriginId>,
}

impl OriginArena {
    pub(super) fn intern(
        &mut self,
        key: OriginKey,
        origin: ArrivalOrigin,
    ) -> Result<OriginId, crate::TimingError> {
        if let Some(&id) = self.ids.get(&key) {
            self.values[id.0 as usize] = origin;
            return Ok(id);
        }
        let id = OriginId(self.values.len().try_into().map_err(|_| {
            crate::TimingAnalysisError::Capacity {
                resource: "arrival origin arena",
            }
        })?);
        self.values.push(origin);
        self.ids.insert(key, id);
        Ok(id)
    }

    pub(super) fn get(&self, id: OriginId) -> Result<&ArrivalOrigin, crate::TimingError> {
        self.values
            .get(id.0 as usize)
            .ok_or_else(|| crate::TimingAnalysisError::UnknownArrivalOrigin { id: id.0 }.into())
    }

    /// Restores overwritten origins and truncates identities created by an edit.
    pub(super) fn restore(
        &mut self,
        len: usize,
        values: impl IntoIterator<Item = (OriginId, ArrivalOrigin)>,
    ) {
        for (id, value) in values {
            if let Some(current) = self.values.get_mut(id.0 as usize) {
                *current = value;
            }
        }
        self.values.truncate(len);
        self.ids.retain(|_, id| (id.0 as usize) < len);
    }
}

#[derive(Debug, Clone)]
pub(super) struct ArrivalOrigin {
    pub(super) startpoint: String,
    pub(super) startpoint_description: String,
    pub(super) launch_clock: Option<LaunchClock>,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct ArrivalState {
    pub(super) tag: TagId,
    pub(super) origin: OriginId,
    pub(super) delay: f64,
    pub(super) transition: Option<f64>,
    pub(super) path: Option<PathId>,
}

impl PartialEq for ArrivalState {
    fn eq(&self, other: &Self) -> bool {
        self.tag == other.tag
            && self.origin == other.origin
            && self.delay.to_bits() == other.delay.to_bits()
            && self.transition.map(f64::to_bits) == other.transition.map(f64::to_bits)
            && self.path == other.path
    }
}

impl Eq for ArrivalState {}

impl ArrivalState {
    pub(super) fn materialize(
        &self,
        paths: &PathArena,
        origins: &OriginArena,
    ) -> Result<Arrival, crate::TimingError> {
        let origin = origins.get(self.origin)?;
        let path = self.path.ok_or(crate::TimingAnalysisError::EmptyPath {
            operation: "materialize",
        })?;
        Ok(Arrival {
            startpoint: origin.startpoint.clone(),
            startpoint_description: origin.startpoint_description.clone(),
            delay: self.delay,
            steps: paths.materialize(path)?,
        })
    }
}

pub(super) struct CandidateInputs<'analysis, 'model> {
    pub(super) timing: &'analysis TimingContext,
    pub(super) model: &'model TimingModel,
    pub(super) design: &'model crate::model::SharedTimingDesign,
    pub(super) library: &'model TimingLibrary,
    pub(super) options: &'analysis ReportTimingOptions,
    pub(super) graph: &'analysis TimingGraph,
    pub(super) arrivals: &'analysis ArrivalSlotStore,
    pub(super) paths: &'analysis PathArena,
    pub(super) origins: &'analysis OriginArena,
    pub(super) tags: &'analysis TagArena,
}

pub(super) struct PropagationInputs<'analysis, 'model> {
    pub(super) timing: &'analysis TimingContext,
    pub(super) model: &'model TimingModel,
    pub(super) design: &'model crate::model::SharedTimingDesign,
    pub(super) library: &'model TimingLibrary,
    pub(super) options: &'analysis ReportTimingOptions,
    pub(super) graph: &'model TimingGraph,
}
