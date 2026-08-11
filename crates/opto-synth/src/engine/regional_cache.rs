// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Publication of every explored regional mapping context.

use super::regional_mapping::RegionalPlanJournalRecord;
use crate::incremental::RegionalCacheRecord;
use crate::regional::RegionCoverPlanRecord;
use crate::{RegionContextKey, RegionCoverPlan, SynthError};
use std::collections::{BTreeMap, btree_map::Entry};

pub(super) fn publish(
    base_records: &mut Box<[RegionalCacheRecord]>,
    final_plans: &[RegionCoverPlan],
    journal: Box<[RegionalPlanJournalRecord]>,
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
    for journaled in journal {
        let base = bases
            .get(journaled.row.index())
            .ok_or_else(|| SynthError::invariant("regional plan journal row is out of range"))?;
        merge_plan(&mut records, base, journaled.plan, false)?;
    }
    // The journal retains every explored context. The selected plans remain
    // authoritative for contexts that also contain the best checkpoint.
    for (base, plan) in bases.iter().zip(final_plans) {
        merge_plan(&mut records, base, plan.checkpoint_record(), true)?;
    }
    *base_records = records.into_values().collect();
    RegionalCacheRecord::validate_all(base_records)
}

fn merge_plan(
    records: &mut BTreeMap<RegionContextKey, RegionalCacheRecord>,
    base: &RegionalCacheRecord,
    plan: RegionCoverPlanRecord,
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
