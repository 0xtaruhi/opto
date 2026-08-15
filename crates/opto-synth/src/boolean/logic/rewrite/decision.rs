// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::{
    AND_WEIGHT, Arc, CENSUS_DIVISOR_STORAGE, DIVISOR_DEPTH, HashMap, MUX_WEIGHT, TruthTable,
    VecDeque, WINDOW_CUT_LEAVES, XOR_WEIGHT, plan_level,
};

#[derive(Debug)]
pub(in crate::boolean::logic) enum Plan {
    Constant(bool),
    Literal {
        var: u8,
        inverted: bool,
    },
    And(Arc<Plan>, Arc<Plan>),
    Or(Arc<Plan>, Arc<Plan>),
    Xor(Arc<Plan>, Arc<Plan>),
    Mux {
        select: u8,
        then_plan: Arc<Plan>,
        else_plan: Arc<Plan>,
    },
}

impl Plan {
    #[cfg(test)]
    pub(super) fn truth(&self, input_count: usize, divisors: &[u64]) -> u64 {
        let assignments = 1usize << input_count;
        let mask = super::full_truth_mask(assignments);
        match self {
            Self::Constant(value) => mask * u64::from(*value),
            Self::Literal { var, inverted } => {
                let var = usize::from(*var);
                let bits = if var < input_count {
                    (0..assignments).fold(0, |bits, assignment| {
                        bits | (((assignment >> var) & 1) as u64) << assignment
                    })
                } else {
                    divisors[var - input_count]
                };
                if *inverted { bits ^ mask } else { bits }
            }
            Self::And(left, right) => {
                left.truth(input_count, divisors) & right.truth(input_count, divisors)
            }
            Self::Or(left, right) => {
                left.truth(input_count, divisors) | right.truth(input_count, divisors)
            }
            Self::Xor(left, right) => {
                left.truth(input_count, divisors) ^ right.truth(input_count, divisors)
            }
            Self::Mux {
                select,
                then_plan,
                else_plan,
            } => {
                let select = Self::Literal {
                    var: *select,
                    inverted: false,
                }
                .truth(input_count, divisors);
                (select & then_plan.truth(input_count, divisors))
                    | (!select & else_plan.truth(input_count, divisors) & mask)
            }
        }
    }

    #[cfg(test)]
    pub(super) fn proves(&self, truth: TruthTable, care: u64, divisors: &[u64]) -> bool {
        let mask = super::full_truth_mask(1usize << truth.input_count);
        (self.truth(truth.input_count, divisors) ^ truth.bits) & care & mask == 0
    }
}

type PlanKey = (u64, usize);
type PlanValue = (u32, Arc<Plan>);

#[derive(Clone, Copy)]
struct ShannonContext<'a> {
    input_count: usize,
    divisors: &'a [u64],
    depth: usize,
    full: u64,
    budget: u32,
}

#[derive(Clone, Copy, Hash, PartialEq, Eq)]
struct DivisorPlanHeader {
    bits: u64,
    care: u64,
    divisor_signature: u64,
    divisor_count: u8,
    input_count: u8,
    depth: u8,
}

struct DivisorPlanEntry {
    divisors: Box<[u64]>,
    value: PlanValue,
}

type DivisorPlanBucket = smallvec::SmallVec<[DivisorPlanEntry; 1]>;

#[derive(Clone, Copy, Hash, PartialEq, Eq)]
struct TimingPlanKey {
    bits: u64,
    input_count: u8,
    arrivals: [u32; WINDOW_CUT_LEAVES],
}

const PLAN_CACHE_ENTRIES: usize = 1_024;
const DIVISOR_CACHE_ENTRIES: usize = 256;
const TIMING_CACHE_ENTRIES: usize = 256;

fn divisor_signature(divisors: &[u64], full: u64) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    divisors.iter().fold(OFFSET, |signature, &divisor| {
        (signature ^ (divisor & full)).wrapping_mul(PRIME)
    })
}

#[derive(Clone, Copy)]
enum PlanObjective<'a> {
    Area,
    Timing(&'a [u32]),
}

impl PlanObjective<'_> {
    fn rank(self, (cost, plan): &PlanValue) -> (u32, u32) {
        match self {
            Self::Area => (*cost, 0),
            Self::Timing(arrivals) => (plan_level(plan, arrivals), *cost),
        }
    }
}

struct BoundedCache<K, V> {
    values: HashMap<K, V>,
    insertion_order: VecDeque<K>,
    capacity: usize,
}

impl<K: Clone + std::hash::Hash + Eq, V> BoundedCache<K, V> {
    fn new(capacity: usize) -> Self {
        Self {
            values: HashMap::with_capacity(capacity),
            insertion_order: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    fn get(&self, key: &K) -> Option<&V> {
        self.values.get(key)
    }

    fn insert(&mut self, key: K, value: V) {
        if self.capacity == 0 || self.values.contains_key(&key) {
            return;
        }
        if self.values.len() == self.capacity {
            let oldest = self
                .insertion_order
                .pop_front()
                .expect("a full bounded cache has an insertion-order entry");
            self.values.remove(&oldest);
        }
        self.insertion_order.push_back(key.clone());
        self.values.insert(key, value);
    }
}

struct DivisorPlanCache {
    values: HashMap<DivisorPlanHeader, DivisorPlanBucket>,
    insertion_order: VecDeque<DivisorPlanHeader>,
    entry_count: usize,
    capacity: usize,
}

impl DivisorPlanCache {
    fn new(capacity: usize) -> Self {
        Self {
            values: HashMap::with_capacity(capacity),
            insertion_order: VecDeque::with_capacity(capacity),
            entry_count: 0,
            capacity,
        }
    }

    fn get(&self, header: &DivisorPlanHeader, divisors: &[u64], full: u64) -> Option<&PlanValue> {
        self.values.get(header)?.iter().find_map(|entry| {
            (entry.divisors.len() == divisors.len()
                && entry
                    .divisors
                    .iter()
                    .zip(divisors)
                    .all(|(&cached, &divisor)| cached == divisor & full))
            .then_some(&entry.value)
        })
    }

    fn insert(&mut self, header: DivisorPlanHeader, divisors: Box<[u64]>, value: PlanValue) {
        if self.capacity == 0 {
            return;
        }
        while self.entry_count == self.capacity {
            let oldest = self
                .insertion_order
                .pop_front()
                .expect("a full divisor cache has an insertion-order entry");
            if let Some(bucket) = self.values.remove(&oldest) {
                self.entry_count -= bucket.len();
            }
        }
        self.values
            .entry(header)
            .or_default()
            .push(DivisorPlanEntry { divisors, value });
        self.insertion_order.push_back(header);
        self.entry_count += 1;
    }
}

pub(in crate::boolean::logic) struct Synthesizer {
    plans: BoundedCache<PlanKey, PlanValue>,
    divisor_plans: DivisorPlanCache,
    timing_plans: BoundedCache<TimingPlanKey, PlanValue>,
}

impl Default for Synthesizer {
    fn default() -> Self {
        Self {
            plans: BoundedCache::new(PLAN_CACHE_ENTRIES),
            divisor_plans: DivisorPlanCache::new(DIVISOR_CACHE_ENTRIES),
            timing_plans: BoundedCache::new(TIMING_CACHE_ENTRIES),
        }
    }
}

impl Synthesizer {
    pub(in crate::boolean::logic) fn fresh() -> Self {
        Self::default()
    }

    pub(in crate::boolean::logic) fn plan(&mut self, truth: TruthTable) -> (u32, Arc<Plan>) {
        let assignments = 1usize << truth.input_count;
        let full = super::full_truth_mask(assignments);
        let bits = truth.bits & full;
        let key = (bits, truth.input_count);
        if let Some(memoized) = self.plans.get(&key) {
            return memoized.clone();
        }
        let result = self.search(
            TruthTable {
                input_count: truth.input_count,
                bits,
            },
            PlanObjective::Area,
        );
        self.plans.insert(key, result.clone());
        result
    }

    pub(super) fn timing_plan(&mut self, truth: TruthTable, arrivals: &[u32]) -> (u32, Arc<Plan>) {
        debug_assert_eq!(truth.input_count, arrivals.len());
        let assignments = 1usize << truth.input_count;
        let bits = truth.bits & super::full_truth_mask(assignments);
        let mut padded = [0; WINDOW_CUT_LEAVES];
        padded[..arrivals.len()].copy_from_slice(arrivals);
        let key = TimingPlanKey {
            bits,
            input_count: u8::try_from(truth.input_count)
                .expect("rewrite windows have at most eight inputs"),
            arrivals: padded,
        };
        if let Some(memoized) = self.timing_plans.get(&key) {
            return memoized.clone();
        }
        let result = self.search(
            TruthTable {
                input_count: truth.input_count,
                bits,
            },
            PlanObjective::Timing(arrivals),
        );
        self.timing_plans.insert(key, result.clone());
        result
    }

    fn search(&mut self, truth: TruthTable, objective: PlanObjective<'_>) -> PlanValue {
        let assignments = 1usize << truth.input_count;
        let full = super::full_truth_mask(assignments);
        if truth.bits == 0 {
            return (0, Arc::new(Plan::Constant(false)));
        }
        if truth.bits == full {
            return (0, Arc::new(Plan::Constant(true)));
        }
        let mut support =
            smallvec::SmallVec::<[(usize, TruthTable, TruthTable); WINDOW_CUT_LEAVES]>::new();
        for var in 0..truth.input_count {
            let (negative, positive) = cofactors(truth, var);
            if negative.bits != positive.bits {
                support.push((var, negative, positive));
            }
        }
        if let [(var, negative, _)] = support.as_slice() {
            return (
                0,
                Arc::new(Plan::Literal {
                    var: u8::try_from(*var).expect("rewrite variable fits a literal index"),
                    inverted: negative.bits != 0,
                }),
            );
        }
        let mut best: Option<(u32, Arc<Plan>)> = None;
        for (var, negative, positive) in support {
            let literal = |inverted| {
                Arc::new(Plan::Literal {
                    var: u8::try_from(var).expect("rewrite variable fits a literal index"),
                    inverted,
                })
            };
            let candidate = if negative.bits == 0 {
                let (cost, plan) = self.solve(positive, objective);
                (cost + AND_WEIGHT, Arc::new(Plan::And(literal(false), plan)))
            } else if negative.bits == full {
                let (cost, plan) = self.solve(positive, objective);
                (cost + AND_WEIGHT, Arc::new(Plan::Or(literal(true), plan)))
            } else if positive.bits == 0 {
                let (cost, plan) = self.solve(negative, objective);
                (cost + AND_WEIGHT, Arc::new(Plan::And(literal(true), plan)))
            } else if positive.bits == full {
                let (cost, plan) = self.solve(negative, objective);
                (cost + AND_WEIGHT, Arc::new(Plan::Or(literal(false), plan)))
            } else if negative.bits == positive.bits ^ full {
                let (cost, plan) = self.solve(negative, objective);
                (cost + XOR_WEIGHT, Arc::new(Plan::Xor(literal(false), plan)))
            } else {
                let (then_cost, then_plan) = self.solve(positive, objective);
                let (else_cost, else_plan) = self.solve(negative, objective);
                (
                    then_cost + else_cost + MUX_WEIGHT,
                    Arc::new(Plan::Mux {
                        select: u8::try_from(var).expect("rewrite variable fits a literal index"),
                        then_plan,
                        else_plan,
                    }),
                )
            };
            if best
                .as_ref()
                .is_none_or(|best| objective.rank(&candidate) < objective.rank(best))
            {
                best = Some(candidate);
            }
        }
        best.expect("multi-variable truth tables always decompose")
    }

    fn solve(&mut self, truth: TruthTable, objective: PlanObjective<'_>) -> PlanValue {
        match objective {
            PlanObjective::Area => self.plan(truth),
            PlanObjective::Timing(arrivals) => self.timing_plan(truth, arrivals),
        }
    }

    pub(in crate::boolean::logic) fn divisor_plan(
        &mut self,
        truth: TruthTable,
        care: u64,
        divisors: &[u64],
        depth: usize,
    ) -> (u32, Arc<Plan>) {
        let assignments = 1usize << truth.input_count;
        let full = super::full_truth_mask(assignments);
        let divisor_count = u8::try_from(divisors.len())
            .expect("rewrite divisor count must fit in a literal index");
        let header = DivisorPlanHeader {
            bits: truth.bits & full,
            care: if depth == 0 { care & full } else { full },
            divisor_signature: divisor_signature(divisors, full),
            divisor_count,
            input_count: u8::try_from(truth.input_count)
                .expect("rewrite windows have at most eight inputs"),
            depth: u8::try_from(depth).expect("rewrite recursion depth is input-bounded"),
        };
        if let Some(memoized) = self.divisor_plans.get(&header, divisors, full) {
            return memoized.clone();
        }
        let result = self.bounded_divisor_plan(truth, care, divisors, depth, u32::MAX);
        self.divisor_plans.insert(
            header,
            divisors.iter().map(|divisor| divisor & full).collect(),
            result.clone(),
        );
        result
    }

    fn bounded_divisor_plan(
        &mut self,
        truth: TruthTable,
        care: u64,
        divisors: &[u64],
        depth: usize,
        budget: u32,
    ) -> (u32, Arc<Plan>) {
        let assignments = 1usize << truth.input_count;
        let full = super::full_truth_mask(assignments);
        let care = if depth == 0 { care & full } else { full };
        let bits = truth.bits & full;
        if bits & care == 0 {
            return (0, Arc::new(Plan::Constant(false)));
        }
        if (bits ^ full) & care == 0 {
            return (0, Arc::new(Plan::Constant(true)));
        }
        let literal = |index: usize, inverted: bool| {
            Arc::new(Plan::Literal {
                var: u8::try_from(truth.input_count + index)
                    .expect("rewrite divisor fits a literal index"),
                inverted,
            })
        };
        for (index, &divisor) in divisors.iter().enumerate() {
            let divisor = divisor & full;
            if (divisor ^ bits) & care == 0 {
                return (0, literal(index, false));
            }
            if (divisor ^ bits ^ full) & care == 0 {
                return (0, literal(index, true));
            }
        }
        let normalized = TruthTable {
            input_count: truth.input_count,
            bits,
        };
        let mut best = self.plan(normalized);
        if depth == 0 && !divisors.is_empty() && best.0 > AND_WEIGHT {
            let need_one = bits & care;
            let need_zero = !bits & care & full;
            let phased = divisors
                .iter()
                .map(|&divisor| [divisor & full, (divisor ^ full) & full])
                .collect::<smallvec::SmallVec<[[u64; 2]; CENSUS_DIVISOR_STORAGE]>>();
            let covers_onset = phased
                .iter()
                .map(|&[plain, inverted]| [need_one & !plain == 0, need_one & !inverted == 0])
                .collect::<smallvec::SmallVec<[[bool; 2]; CENSUS_DIVISOR_STORAGE]>>();
            let inside_onset = phased
                .iter()
                .map(|&[plain, inverted]| [need_zero & plain == 0, need_zero & inverted == 0])
                .collect::<smallvec::SmallVec<[[bool; 2]; CENSUS_DIVISOR_STORAGE]>>();
            'pairs: for left in 0..divisors.len() {
                for right in left + 1..divisors.len() {
                    for phases in 0..4usize {
                        let left_phase = phases & 1;
                        let right_phase = (phases >> 1) & 1;
                        let a = phased[left][left_phase];
                        let b = phased[right][right_phase];
                        if covers_onset[left][left_phase]
                            && covers_onset[right][right_phase]
                            && ((a & b) ^ bits) & care == 0
                        {
                            best = (
                                AND_WEIGHT,
                                Arc::new(Plan::And(
                                    literal(left, left_phase != 0),
                                    literal(right, right_phase != 0),
                                )),
                            );
                            break 'pairs;
                        }
                        if inside_onset[left][left_phase]
                            && inside_onset[right][right_phase]
                            && ((a | b) ^ bits) & care == 0
                        {
                            best = (
                                AND_WEIGHT,
                                Arc::new(Plan::Or(
                                    literal(left, left_phase != 0),
                                    literal(right, right_phase != 0),
                                )),
                            );
                            break 'pairs;
                        }
                        if XOR_WEIGHT < best.0 && ((a ^ b) ^ bits) & care == 0 {
                            best = (
                                XOR_WEIGHT,
                                Arc::new(Plan::Xor(
                                    literal(left, left_phase != 0),
                                    literal(right, right_phase != 0),
                                )),
                            );
                        }
                    }
                }
            }
        }
        if depth < DIVISOR_DEPTH {
            for var in 0..truth.input_count {
                let candidate = self.shannon_candidate(
                    ShannonContext {
                        input_count: truth.input_count,
                        divisors,
                        depth,
                        full,
                        budget: best.0.min(budget),
                    },
                    var,
                    cofactors(normalized, var),
                );
                if let Some(candidate) = candidate
                    && candidate.0 < best.0
                {
                    best = candidate;
                }
            }
        }
        best
    }

    fn shannon_candidate(
        &mut self,
        context: ShannonContext<'_>,
        var: usize,
        (negative, positive): (TruthTable, TruthTable),
    ) -> Option<(u32, Arc<Plan>)> {
        let ShannonContext {
            input_count,
            divisors,
            depth,
            full,
            budget,
        } = context;
        if negative.bits == positive.bits {
            return None;
        }
        let var_literal = |inverted| {
            Arc::new(Plan::Literal {
                var: u8::try_from(var).expect("rewrite variable fits a literal index"),
                inverted,
            })
        };
        let cofactored = |keep_positive: bool| {
            divisors
                .iter()
                .map(|&divisor| {
                    let (negative, positive) = cofactors_for(divisor, input_count, var);
                    if keep_positive { positive } else { negative }
                })
                .collect::<smallvec::SmallVec<[u64; CENSUS_DIVISOR_STORAGE]>>()
        };
        let candidate = if negative.bits == 0 {
            if budget <= AND_WEIGHT {
                return None;
            }
            let (cost, plan) = self.bounded_divisor_plan(
                positive,
                full,
                &cofactored(true),
                depth + 1,
                budget - AND_WEIGHT,
            );
            (
                cost.saturating_add(AND_WEIGHT),
                Arc::new(Plan::And(var_literal(false), plan)),
            )
        } else if negative.bits == full {
            if budget <= AND_WEIGHT {
                return None;
            }
            let (cost, plan) = self.bounded_divisor_plan(
                positive,
                full,
                &cofactored(true),
                depth + 1,
                budget - AND_WEIGHT,
            );
            (
                cost.saturating_add(AND_WEIGHT),
                Arc::new(Plan::Or(var_literal(true), plan)),
            )
        } else if positive.bits == 0 {
            if budget <= AND_WEIGHT {
                return None;
            }
            let (cost, plan) = self.bounded_divisor_plan(
                negative,
                full,
                &cofactored(false),
                depth + 1,
                budget - AND_WEIGHT,
            );
            (
                cost.saturating_add(AND_WEIGHT),
                Arc::new(Plan::And(var_literal(true), plan)),
            )
        } else if positive.bits == full {
            if budget <= AND_WEIGHT {
                return None;
            }
            let (cost, plan) = self.bounded_divisor_plan(
                negative,
                full,
                &cofactored(false),
                depth + 1,
                budget - AND_WEIGHT,
            );
            (
                cost.saturating_add(AND_WEIGHT),
                Arc::new(Plan::Or(var_literal(false), plan)),
            )
        } else if negative.bits == positive.bits ^ full {
            if budget <= XOR_WEIGHT {
                return None;
            }
            let (cost, plan) =
                self.bounded_divisor_plan(negative, full, &[], depth + 1, budget - XOR_WEIGHT);
            (
                cost.saturating_add(XOR_WEIGHT),
                Arc::new(Plan::Xor(var_literal(false), plan)),
            )
        } else {
            if budget <= MUX_WEIGHT {
                return None;
            }
            let (then_cost, then_plan) = self.bounded_divisor_plan(
                positive,
                full,
                &cofactored(true),
                depth + 1,
                budget - MUX_WEIGHT,
            );
            if then_cost.saturating_add(MUX_WEIGHT) >= budget {
                return None;
            }
            let (else_cost, else_plan) = self.bounded_divisor_plan(
                negative,
                full,
                &cofactored(false),
                depth + 1,
                budget - MUX_WEIGHT - then_cost,
            );
            (
                then_cost
                    .saturating_add(else_cost)
                    .saturating_add(MUX_WEIGHT),
                Arc::new(Plan::Mux {
                    select: u8::try_from(var).expect("rewrite variable fits a literal index"),
                    then_plan,
                    else_plan,
                }),
            )
        };
        Some(candidate)
    }
}

fn cofactors_for(bits: u64, input_count: usize, var: usize) -> (u64, u64) {
    let truth = TruthTable { input_count, bits };
    let (negative, positive) = cofactors(truth, var);
    (negative.bits, positive.bits)
}

fn cofactors(truth: TruthTable, var: usize) -> (TruthTable, TruthTable) {
    let assignments = 1usize << truth.input_count;
    let full = super::full_truth_mask(assignments);
    let positive_mask = [
        0xaaaa_aaaa_aaaa_aaaa,
        0xcccc_cccc_cccc_cccc,
        0xf0f0_f0f0_f0f0_f0f0,
        0xff00_ff00_ff00_ff00,
        0xffff_0000_ffff_0000,
        0xffff_ffff_0000_0000,
    ][var]
        & full;
    let shift = 1usize << var;
    let negative = truth.bits & !positive_mask & full;
    let positive = truth.bits & positive_mask;
    let negative = (negative | (negative << shift)) & full;
    let positive = (positive | (positive >> shift)) & full;
    (
        TruthTable {
            input_count: truth.input_count,
            bits: negative,
        },
        TruthTable {
            input_count: truth.input_count,
            bits: positive,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reference_cofactors(truth: TruthTable, var: usize) -> (TruthTable, TruthTable) {
        let assignments = 1usize << truth.input_count;
        let mut negative = 0u64;
        let mut positive = 0u64;
        for assignment in 0..assignments {
            let value = (truth.bits >> assignment) & 1;
            if assignment & (1 << var) == 0 {
                negative |= value << assignment;
                negative |= value << (assignment | (1 << var));
            } else {
                positive |= value << assignment;
                positive |= value << (assignment & !(1 << var));
            }
        }
        (
            TruthTable {
                input_count: truth.input_count,
                bits: negative,
            },
            TruthTable {
                input_count: truth.input_count,
                bits: positive,
            },
        )
    }

    #[test]
    fn bit_parallel_cofactors_match_assignment_enumeration() {
        for input_count in 1..=4 {
            let function_count = 1u64 << (1usize << input_count);
            for bits in 0..function_count {
                let truth = TruthTable { input_count, bits };
                for var in 0..input_count {
                    assert_eq!(cofactors(truth, var), reference_cofactors(truth, var));
                }
            }
        }
        for input_count in [5, 6] {
            for bits in [
                0,
                u64::MAX,
                0x0123_4567_89ab_cdef,
                0xfedc_ba98_7654_3210,
                0xa5a5_5a5a_0f0f_f0f0,
            ] {
                let truth = TruthTable { input_count, bits };
                for var in 0..input_count {
                    assert_eq!(cofactors(truth, var), reference_cofactors(truth, var));
                }
            }
        }
    }

    #[test]
    fn bounded_cache_evicts_the_oldest_entry() {
        let mut cache = BoundedCache::new(2);
        cache.insert(1, "one");
        cache.insert(2, "two");
        cache.insert(3, "three");

        assert_eq!(cache.get(&1), None);
        assert_eq!(cache.get(&2), Some(&"two"));
        assert_eq!(cache.get(&3), Some(&"three"));
    }

    #[test]
    fn divisor_cache_verifies_signatures_against_full_keys() {
        let header = DivisorPlanHeader {
            bits: 0,
            care: u64::MAX,
            divisor_signature: 7,
            divisor_count: 1,
            input_count: 1,
            depth: 1,
        };
        let first = (1, Arc::new(Plan::Constant(false)));
        let second = (2, Arc::new(Plan::Constant(true)));
        let mut cache = DivisorPlanCache::new(2);
        cache.insert(header, vec![0x0f].into_boxed_slice(), first);
        cache.insert(header, vec![0xf0].into_boxed_slice(), second);

        assert_eq!(cache.get(&header, &[0x0f], u64::MAX).unwrap().0, 1);
        assert_eq!(cache.get(&header, &[0xf0], u64::MAX).unwrap().0, 2);
        assert!(cache.get(&header, &[0xff], u64::MAX).is_none());
    }

    #[test]
    fn bounded_proof_substitutes_divisors_and_honors_care() {
        let plan = Plan::Xor(
            Arc::new(Plan::Literal {
                var: 0,
                inverted: false,
            }),
            Arc::new(Plan::Literal {
                var: 2,
                inverted: false,
            }),
        );
        let truth = TruthTable {
            input_count: 2,
            bits: 0b0010,
        };
        assert!(plan.proves(truth, 0b0011, &[0b1100]));
        assert!(!plan.proves(truth, u64::MAX, &[0b1100]));
    }
}
