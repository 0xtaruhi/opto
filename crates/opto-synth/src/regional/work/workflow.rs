// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Bounded multi-item workflows over immutable work rows.

use super::{FusionTaskId, WorkEntitySet, WorkGraph, WorkItem, WorkItemId};
use opto_runtime::{ExecutionContext, Task, TaskKey};
use std::collections::BTreeSet;

const FUSION_TASK_DOMAIN: u32 = 0x4655_534e;
const REDUCE_TASK_DOMAIN: u32 = 0x5245_4455;
const MAX_FUSION_WORK: u64 = 1 << 20;

#[derive(Debug)]
/// One admitted two-item scope with an exact combined footprint.
pub(crate) struct FusionItem {
    id: FusionTaskId,
    members: [usize; 2],
    core: WorkEntitySet,
    halo: WorkEntitySet,
    estimated_work: u64,
}

#[derive(Debug)]
/// Deterministically colored disjoint waves of bounded fusion scopes.
pub(crate) struct FusionPlan {
    items: Box<[FusionItem]>,
    waves: opto_core::PackedRows<u32>,
}

impl FusionItem {
    pub(crate) const fn id(&self) -> FusionTaskId {
        self.id
    }

    pub(crate) const fn members(&self) -> [usize; 2] {
        self.members
    }
}

impl FusionPlan {
    pub(crate) fn wave_count(&self) -> usize {
        self.waves.row_count()
    }

    /// Executes one conflict-free wave in parallel and preserves task order.
    pub(crate) fn execute_wave<T, F>(
        &self,
        wave: usize,
        runtime: &ExecutionContext,
        operation: F,
    ) -> Result<Vec<T>, crate::SynthError>
    where
        T: Send,
        F: Fn(&FusionItem, &ExecutionContext) -> Result<T, crate::SynthError> + Send + Sync,
    {
        let tasks = self
            .waves
            .row(wave)
            .iter()
            .map(|&row| {
                let item = &self.items[row as usize];
                let mut ordinal = [0; 8];
                ordinal.copy_from_slice(&item.id.0[..8]);
                Task::new(
                    TaskKey::new(FUSION_TASK_DOMAIN, u64::from_le_bytes(ordinal)),
                    item,
                )
                .with_estimated_work(item.estimated_work)
                .with_estimated_memory(
                    item.core
                        .cardinality()
                        .saturating_add(item.halo.cardinality())
                        .max(1),
                )
            })
            .collect();
        runtime.map_ordered_composite(tasks, |item, inner| operation(item, inner))
    }
}

impl WorkGraph {
    /// Forms one bounded fusion proposal for every adjacent semantic pair and
    /// colors exact item overlap into deterministic disjoint waves.
    pub(crate) fn fusion_plan(&self) -> Result<FusionPlan, crate::SynthError> {
        let mut pairs = BTreeSet::new();
        for (left, successors) in self.successors.iter().enumerate() {
            for successor in successors {
                let right = self.item_rows.get(successor).copied().ok_or_else(|| {
                    crate::SynthError::invariant(
                        "fusion proposal references an unknown successor item",
                    )
                })?;
                if left != right {
                    pairs.insert([left.min(right), left.max(right)]);
                }
            }
        }
        let mut items = Vec::with_capacity(pairs.len());
        for members in pairs {
            let left = &self.items[members[0]];
            let right = &self.items[members[1]];
            let estimated_work = left.estimated_work.saturating_add(right.estimated_work);
            if estimated_work > MAX_FUSION_WORK {
                continue;
            }
            let core = left.core.union(&right.core)?;
            let halo = left.halo.union(&right.halo)?.difference(&core)?;
            items.push(FusionItem {
                id: fusion_id(self, members),
                members,
                core,
                halo,
                estimated_work: estimated_work.max(1),
            });
        }
        items.sort_by_key(FusionItem::id);
        let mut waves = Vec::<Vec<u32>>::new();
        let mut occupied = Vec::<BTreeSet<usize>>::new();
        for (row, item) in items.iter().enumerate() {
            let wave = occupied
                .iter()
                .position(|members| item.members.iter().all(|member| !members.contains(member)))
                .unwrap_or(occupied.len());
            if wave == occupied.len() {
                occupied.push(BTreeSet::new());
                waves.push(Vec::new());
            }
            occupied[wave].extend(item.members);
            waves[wave].push(u32::try_from(row).map_err(|_| {
                crate::SynthError::capacity("fusion proposal count exceeds 32-bit capacity")
            })?);
        }
        Ok(FusionPlan {
            items: items.into_boxed_slice(),
            waves: opto_core::PackedRows::try_from_rows(waves)
                .map_err(|_| crate::SynthError::capacity("fusion overlap waves"))?,
        })
    }

    /// Runs deterministic map/shuffle/reduce without granting the reducer any
    /// structural mutation authority.
    pub(crate) fn map_reduce<K, V, R, M, F>(
        &self,
        runtime: &ExecutionContext,
        map: M,
        reduce: F,
    ) -> Result<Vec<(K, R)>, crate::SynthError>
    where
        K: Clone + Ord + Send,
        V: Send,
        R: Send,
        M: Fn(usize, &WorkItem, &ExecutionContext) -> Result<(K, V), crate::SynthError>
            + Send
            + Sync,
        F: Fn(K, Vec<(WorkItemId, V)>, &ExecutionContext) -> Result<R, crate::SynthError>
            + Send
            + Sync,
    {
        let tasks = self
            .items
            .iter()
            .enumerate()
            .map(|(row, item)| {
                Task::new(TaskKey::new(REDUCE_TASK_DOMAIN, row as u64), (row, item))
                    .with_estimated_work(item.estimated_work)
                    .with_estimated_memory(item.estimated_memory)
            })
            .collect();
        let mut mapped = runtime.map_ordered_composite(tasks, |(row, item), inner| {
            let (key, value) = map(row, item, inner)?;
            Ok::<_, crate::SynthError>((key, item.id, value))
        })?;
        mapped.sort_by(|left, right| (&left.0, left.1).cmp(&(&right.0, right.1)));
        let mut groups = Vec::<(K, Vec<(WorkItemId, V)>)>::new();
        for (key, item, value) in mapped {
            if groups.last().is_none_or(|(candidate, _)| candidate != &key) {
                groups.push((key.clone(), Vec::new()));
            }
            groups
                .last_mut()
                .expect("a reduce group was just installed")
                .1
                .push((item, value));
        }
        let tasks = groups
            .into_iter()
            .enumerate()
            .map(|(row, group)| Task::new(TaskKey::new(REDUCE_TASK_DOMAIN + 1, row as u64), group))
            .collect();
        runtime.map_ordered_composite(tasks, |(key, values), inner| {
            let output = reduce(key.clone(), values, inner)?;
            Ok((key, output))
        })
    }

    /// Propagates bounded non-negative analytical prices from successors to
    /// predecessors over the immutable work graph.
    pub(crate) fn backward_prices(&self, local: &[u64]) -> Result<Box<[u64]>, crate::SynthError> {
        if local.len() != self.items.len() {
            return Err(crate::SynthError::invariant(
                "analytical prices do not cover every work item",
            ));
        }
        let mut indegree = self
            .predecessors
            .iter()
            .map(<[WorkItemId]>::len)
            .collect::<Vec<_>>();
        let mut ready = indegree
            .iter()
            .enumerate()
            .filter_map(|(row, &degree)| (degree == 0).then_some(row))
            .collect::<BTreeSet<_>>();
        let mut order = Vec::with_capacity(self.items.len());
        while let Some(row) = ready.pop_first() {
            order.push(row);
            for successor in &self.successors[row] {
                let successor = self.item_rows[successor];
                indegree[successor] -= 1;
                if indegree[successor] == 0 {
                    ready.insert(successor);
                }
            }
        }
        if order.len() != self.items.len() {
            return Err(crate::SynthError::invariant(
                "work-item dependency graph is cyclic",
            ));
        }
        let mut prices = local.to_vec();
        for &row in order.iter().rev() {
            let successor = self.successors[row]
                .iter()
                .map(|id| prices[self.item_rows[id]])
                .max()
                .unwrap_or(0);
            prices[row] = prices[row].saturating_add(successor);
        }
        Ok(prices.into_boxed_slice())
    }
}

fn fusion_id(work: &WorkGraph, members: [usize; 2]) -> FusionTaskId {
    let mut digest = blake3::Hasher::new();
    digest.update(b"opto/fusion-task/v1\0");
    digest.update(&work.design.revision.bytes());
    for member in members {
        digest.update(&work.items[member].id.0);
    }
    FusionTaskId::from_bytes(*digest.finalize().as_bytes())
}
