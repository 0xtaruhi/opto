// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::{DIVISOR_CAP, PlanRecipe, RecipeNode, WINDOW_CUT_LEAVES};
use crate::incremental::IncrementalRunMetrics;
use std::hash::{Hash, Hasher};
use std::sync::RwLock;

// A regional synthesis revisits the same six-input recipes across many local
// graphs. Keep that working set resident without making cache size scale with
// the design.
const RECIPE_CACHE_CAPACITY: usize = 65_536;
const RECIPE_CACHE_WAYS: usize = 4;
const RECIPE_CACHE_SETS: usize = RECIPE_CACHE_CAPACITY / RECIPE_CACHE_WAYS;
const CACHED_RECIPE_NODES: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum RecipeObjective {
    Area,
    Timing,
    Divisor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct RewriteRecipeKey {
    objective: RecipeObjective,
    truth: u64,
    care: u64,
    input_count: u8,
    divisor_count: u8,
    arrivals: [u32; WINDOW_CUT_LEAVES],
    divisors: [u64; DIVISOR_CAP],
}

impl RewriteRecipeKey {
    pub(super) fn area(truth: crate::boolean::logic::TruthTable) -> Self {
        Self::new(RecipeObjective::Area, truth, u64::MAX, &[], &[])
    }

    pub(super) fn timing(truth: crate::boolean::logic::TruthTable, arrivals: &[u32]) -> Self {
        Self::new(RecipeObjective::Timing, truth, u64::MAX, &[], arrivals)
    }

    pub(super) fn divisor(
        truth: crate::boolean::logic::TruthTable,
        care: u64,
        divisors: &[u64],
    ) -> Self {
        Self::new(RecipeObjective::Divisor, truth, care, divisors, &[])
    }

    fn new(
        objective: RecipeObjective,
        truth: crate::boolean::logic::TruthTable,
        care: u64,
        divisors: &[u64],
        arrivals: &[u32],
    ) -> Self {
        let assignments = 1usize << truth.input_count;
        let full = super::full_truth_mask(assignments);
        let mut stored_divisors = [0; DIVISOR_CAP];
        for (stored, &bits) in stored_divisors.iter_mut().zip(divisors) {
            *stored = bits & full;
        }
        let mut stored_arrivals = [0; WINDOW_CUT_LEAVES];
        stored_arrivals[..arrivals.len()].copy_from_slice(arrivals);
        Self {
            objective,
            truth: truth.bits & full,
            care: care & full,
            input_count: u8::try_from(truth.input_count)
                .expect("rewrite windows have at most eight inputs"),
            divisor_count: u8::try_from(divisors.len())
                .expect("rewrite divisor storage fits a compact count"),
            arrivals: stored_arrivals,
            divisors: stored_divisors,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct CompactRecipe {
    nodes: [RecipeNode; CACHED_RECIPE_NODES],
    len: u8,
}

impl CompactRecipe {
    fn capture(recipe: &PlanRecipe) -> Option<Self> {
        let len = u8::try_from(recipe.0.len()).ok()?;
        if recipe.0.len() > CACHED_RECIPE_NODES {
            return None;
        }
        let mut nodes = [RecipeNode::Constant(false); CACHED_RECIPE_NODES];
        nodes[..recipe.0.len()].copy_from_slice(&recipe.0);
        Some(Self { nodes, len })
    }

    fn materialize(self) -> PlanRecipe {
        PlanRecipe(
            self.nodes[..usize::from(self.len)]
                .to_vec()
                .into_boxed_slice(),
        )
    }
}

#[derive(Debug, Clone, Copy)]
struct RewriteRecipeEntry {
    key: RewriteRecipeKey,
    cost: u32,
    recipe: CompactRecipe,
}

#[derive(Debug)]
struct RewriteRecipeSet {
    entries: [Option<RewriteRecipeEntry>; RECIPE_CACHE_WAYS],
    next_victim: u8,
}

impl Default for RewriteRecipeSet {
    fn default() -> Self {
        Self {
            entries: [None; RECIPE_CACHE_WAYS],
            next_victim: 0,
        }
    }
}

/// Process-local cache of portable Boolean synthesis recipes.
///
/// Keys contain every truth, care, divisor, and arrival input used by recipe
/// synthesis. Payloads contain no graph IDs and have a fixed inline bound, so
/// a hit is valid across source revisions and insertion/renumbering edits.
#[derive(Debug)]
pub(crate) struct RewriteRecipeCache {
    sets: Box<[RwLock<RewriteRecipeSet>]>,
}

impl Default for RewriteRecipeCache {
    fn default() -> Self {
        Self {
            sets: (0..RECIPE_CACHE_SETS)
                .map(|_| RwLock::new(RewriteRecipeSet::default()))
                .collect(),
        }
    }
}

impl RewriteRecipeCache {
    fn set(&self, key: RewriteRecipeKey) -> &RwLock<RewriteRecipeSet> {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        key.hash(&mut hasher);
        let set_count =
            u64::try_from(RECIPE_CACHE_SETS).expect("recipe cache set count must fit a hash word");
        let index = usize::try_from(hasher.finish() % set_count)
            .expect("recipe cache index is bounded by the set count");
        &self.sets[index]
    }

    pub(super) fn lookup(
        &self,
        key: RewriteRecipeKey,
        metrics: &IncrementalRunMetrics,
    ) -> Result<Option<(u32, PlanRecipe)>, crate::SynthError> {
        let set = self
            .set(key)
            .read()
            .map_err(|_| crate::SynthError::invariant("Boolean recipe cache lock is poisoned"))?;
        let hit = set
            .entries
            .iter()
            .flatten()
            .find(|entry| entry.key == key)
            .copied();
        drop(set);
        if let Some(entry) = hit {
            metrics.boolean_recipe_hit();
            Ok(Some((entry.cost, entry.recipe.materialize())))
        } else {
            metrics.boolean_recipe_miss();
            Ok(None)
        }
    }

    pub(super) fn insert(
        &self,
        key: RewriteRecipeKey,
        cost: u32,
        recipe: &PlanRecipe,
    ) -> Result<(), crate::SynthError> {
        let Some(recipe) = CompactRecipe::capture(recipe) else {
            return Ok(());
        };
        let mut set = self
            .set(key)
            .write()
            .map_err(|_| crate::SynthError::invariant("Boolean recipe cache lock is poisoned"))?;
        if set.entries.iter().flatten().any(|entry| entry.key == key) {
            return Ok(());
        }
        let index = set
            .entries
            .iter()
            .position(Option::is_none)
            .unwrap_or(usize::from(set.next_victim));
        set.entries[index] = Some(RewriteRecipeEntry { key, cost, recipe });
        if index == usize::from(set.next_victim) || set.entries.iter().all(Option::is_some) {
            set.next_victim = u8::try_from((index + 1) % RECIPE_CACHE_WAYS)
                .expect("recipe cache associativity fits the victim cursor");
        }
        Ok(())
    }
}
