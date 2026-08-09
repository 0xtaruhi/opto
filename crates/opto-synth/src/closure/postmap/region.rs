// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use opto_ir::mapped::{CellId, MappedNetlist};

pub(super) const REGION_TASK_CELL_BUDGET: usize = 256;

pub(super) fn scheduling_cell_budget(frontier: usize, workers: usize) -> usize {
    frontier
        .div_ceil(workers.max(1))
        .clamp(1, REGION_TASK_CELL_BUDGET)
}

/// One bounded post-map scheduling chunk.
#[derive(Debug)]
pub(super) struct RegionWork {
    ordinal: u64,
    pub(super) cells: Vec<CellId>,
}

impl RegionWork {
    pub(super) fn task_ordinal(&self) -> u64 {
        self.ordinal
    }
}

/// Partitions a generation-local cell frontier into deterministic scheduling
/// chunks. Hierarchical scope improves locality but never changes eligibility
/// or the semantics of a candidate search.
pub(super) fn scoped_work(
    mapped: &MappedNetlist,
    eligible: &std::collections::HashSet<CellId>,
    cell_budget: usize,
) -> Result<Vec<RegionWork>, crate::SynthError> {
    if cell_budget == 0 {
        return Err(crate::SynthError::invariant(
            "mapped work-chunk cell budget must be non-zero",
        ));
    }
    let mut scopes = std::collections::BTreeMap::<&str, Vec<CellId>>::new();
    for &cell in eligible {
        if !mapped.is_live_cell(cell) {
            continue;
        }
        let name = mapped.cell_name(cell).ok_or_else(|| {
            crate::SynthError::invariant(format!("live mapped cell {cell:?} has no stable name"))
        })?;
        let scope = name.rsplit_once('/').map_or("", |(scope, _)| scope);
        scopes.entry(scope).or_default().push(cell);
    }
    let mut work = Vec::new();
    for (scope_index, (_, mut cells)) in scopes.into_iter().enumerate() {
        cells.sort_unstable();
        let scope = u32::try_from(scope_index)
            .map_err(|_| crate::SynthError::capacity("mapped work scope count"))?;
        for (chunk_index, cells) in cells.chunks(cell_budget).enumerate() {
            let chunk = u32::try_from(chunk_index)
                .map_err(|_| crate::SynthError::capacity("mapped work chunks per scope"))?;
            work.push(RegionWork {
                ordinal: (u64::from(scope) << 32) | u64::from(chunk),
                cells: cells.to_vec(),
            });
        }
    }
    Ok(work)
}
