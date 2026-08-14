// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Publication of every explored regional mapping context.

use crate::incremental::RegionalCacheRecord;
use crate::{RegionContextKey, RegionCoverPlan, SynthError};
use std::collections::{BTreeMap, btree_map::Entry};

/// Publishes the complete context-sorted cache reachable from this run.
///
/// Journaled explorations are retained, while final selected plans overwrite
/// the same context because they are the authoritative checkpoints. No
/// process-global cache or unreachable historical context is carried forward.
pub(super) fn publish(
    base_records: &mut Box<[RegionalCacheRecord]>,
    final_plans: &[RegionCoverPlan],
    journal: Box<[(crate::RegionRowId, RegionCoverPlan)]>,
) -> Result<(), SynthError> {
    let bases = std::mem::take(base_records).into_vec();
    if bases.len() != final_plans.len() {
        return Err(SynthError::invariant(
            "regional final plans do not align with decision records",
        ));
    }
    let mut records = bases
        .iter()
        .cloned()
        .map(|record| (record.context(), record))
        .collect::<BTreeMap<_, _>>();
    if records.len() != bases.len() {
        return Err(SynthError::invariant(
            "regional decision records contain duplicate context keys",
        ));
    }
    for (row, plan) in journal {
        let base = bases
            .get(row.index())
            .ok_or_else(|| SynthError::invariant("regional plan journal row is out of range"))?;
        merge_plan(&mut records, base, &plan, false)?;
    }
    // The journal retains every explored context. The selected plans remain
    // authoritative for contexts that also contain the best checkpoint.
    for (base, plan) in bases.iter().zip(final_plans) {
        merge_plan(&mut records, base, plan, true)?;
    }
    *base_records = records.into_values().collect();
    RegionalCacheRecord::validate_all(base_records)
}

fn merge_plan(
    records: &mut BTreeMap<RegionContextKey, RegionalCacheRecord>,
    base: &RegionalCacheRecord,
    plan: &RegionCoverPlan,
    replace: bool,
) -> Result<(), SynthError> {
    let mut incoming = if base.context() == plan.context_key() {
        base.clone()
    } else {
        base.with_context(plan.context_key())
    };
    incoming.set_plan(plan);
    match records.entry(incoming.context()) {
        Entry::Vacant(entry) => {
            entry.insert(incoming);
        }
        Entry::Occupied(mut entry) => {
            entry.get().validate_same_decision(&incoming)?;
            if replace || entry.get().plan_region().is_none() {
                entry.insert(incoming);
            }
        }
    }
    Ok(())
}
