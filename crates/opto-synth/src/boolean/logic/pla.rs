// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Complete-truth small-support multi-output cube factoring.

use super::TruthTable;
use super::network::{LogicGraph, LogicNode, LogicNodeId};
use super::rewrite::{Plan, Synthesizer};
use hashbrown::{HashMap, HashSet};
use opto_runtime::{ExecutionContext, Task, TaskKey};
use smallvec::SmallVec;
use std::sync::Arc;
use std::time::{Duration, Instant};

const INPUT_CAP: usize = u128::BITS.ilog2() as usize;
const RELATION_SEARCH_TASK_DOMAIN: u32 = 0x5253_5243;
const RELATION_PLAN_TASK_DOMAIN: u32 = 0x5250_4c4e;
type Simulation = (Vec<(u32, LogicNodeId)>, Vec<u128>, u128);

pub(super) struct MultiOutputSubject {
    pub(super) network: LogicGraph,
    pub(super) roots: Box<[LogicNodeId]>,
    pub(super) profile: MultiOutputProfile,
}

pub(super) struct MultiOutputProfile {
    pub(super) cover: Duration,
    pub(super) factoring: Duration,
    pub(super) resubstitution: Duration,
    pub(super) relation_checks: usize,
    pub(super) plan_queries: usize,
}

#[derive(Default)]
struct RelationProfile {
    checks: usize,
    plan_queries: usize,
}

impl RelationProfile {
    fn merge(&mut self, other: &Self) {
        self.checks += other.checks;
        self.plan_queries += other.plan_queries;
    }
}

struct RelationContext<'a> {
    variables: &'a [LogicNodeId],
    outputs: &'a [u128],
    input_count: usize,
    full: u128,
    runtime: Option<&'a ExecutionContext>,
}

struct Cube {
    bits: u128,
    literals: Box<[(usize, bool)]>,
}

pub(super) fn build_multi_output(
    source: &LogicGraph,
    roots: &[LogicNodeId],
    runtime: &ExecutionContext,
) -> Result<Option<MultiOutputSubject>, crate::SynthError> {
    if roots.len() < 2 {
        return Ok(None);
    }
    let Some((inputs, values, full)) = simulate(source) else {
        return Ok(None);
    };
    let outputs = roots.iter().map(|&root| signal_value(&values, root, full));
    synthesize_with_inputs(
        inputs.into_iter().map(|(origin, _)| origin),
        outputs,
        Some(runtime),
    )
}

#[cfg(test)]
pub(super) fn synthesize_multi_output(
    input_count: usize,
    outputs: impl IntoIterator<Item = u128>,
) -> Option<MultiOutputSubject> {
    (input_count <= INPUT_CAP).then_some(())?;
    synthesize_with_inputs(
        (0..input_count).map(|input| u32::try_from(input).expect("window input is bounded")),
        outputs,
        None,
    )
    .expect("serial truth synthesis cannot fail")
}

fn synthesize_with_inputs(
    inputs: impl IntoIterator<Item = u32>,
    outputs: impl IntoIterator<Item = u128>,
    runtime: Option<&ExecutionContext>,
) -> Result<Option<MultiOutputSubject>, crate::SynthError> {
    let inputs = inputs.into_iter().collect::<Vec<_>>();
    let mut network = LogicGraph::new();
    let Some(variables) = inputs
        .iter()
        .map(|&origin| network.variable(origin as usize))
        .collect::<Option<Vec<_>>>()
    else {
        return Ok(None);
    };
    synthesize_from_graph(network, &variables, outputs, runtime)
}

fn synthesize_from_graph(
    mut network: LogicGraph,
    variables: &[LogicNodeId],
    outputs: impl IntoIterator<Item = u128>,
    runtime: Option<&ExecutionContext>,
) -> Result<Option<MultiOutputSubject>, crate::SynthError> {
    let outputs = outputs.into_iter().collect::<Vec<_>>();
    if outputs.len() < 2 || variables.len() > INPUT_CAP {
        return Ok(None);
    }
    let assignments = 1usize << variables.len();
    let full = if assignments == u128::BITS as usize {
        u128::MAX
    } else {
        (1u128 << assignments) - 1
    };
    let outputs = outputs
        .into_iter()
        .map(|bits| bits & full)
        .collect::<Vec<_>>();
    let phases = outputs
        .iter()
        .map(|bits| bits.count_ones() > (!bits & full).count_ones())
        .collect::<Vec<_>>();
    let targets = outputs
        .iter()
        .zip(&phases)
        .map(|(&bits, &inverted)| if inverted { !bits & full } else { bits })
        .collect::<Vec<_>>();
    let cover_started = Instant::now();
    let cubes = enumerate_cubes(variables.len(), full);
    let Some(terms) = cover(&cubes, &targets) else {
        return Ok(None);
    };
    let cover_elapsed = cover_started.elapsed();

    let factoring_started = Instant::now();
    let (factored_products, factor_definitions) = factor_products(&cubes, &terms, variables.len());
    let mut factors = variables
        .iter()
        .flat_map(|&variable| [variable, variable.inverted()])
        .collect::<Vec<_>>();
    for &(left, right) in &factor_definitions {
        factors.push(network.and(factors[left], factors[right]));
    }
    let mut cube_nodes = vec![None; cubes.len()];
    let mut choice_roots = Vec::with_capacity(outputs.len());
    for (output, selected) in terms.iter().enumerate() {
        let mut sum = LogicGraph::constant(false);
        for &cube in selected {
            let cube = if let Some(node) = cube_nodes[cube] {
                node
            } else {
                let node = product(&mut network, &factors, &factored_products[cube]);
                cube_nodes[cube] = Some(node);
                node
            };
            sum = or(&mut network, sum, cube);
        }
        choice_roots.push(if phases[output] { sum.inverted() } else { sum });
    }
    let factoring_elapsed = factoring_started.elapsed();
    let resubstitution_started = Instant::now();
    let (choice_roots, relation_profile) = resubstitute_roots(
        &mut network,
        &choice_roots,
        cubes
            .len()
            .saturating_mul(outputs.len())
            .saturating_mul(INPUT_CAP)
            .saturating_mul(super::MAX_MATCH_INPUTS),
        &RelationContext {
            variables,
            outputs: &outputs,
            input_count: variables.len(),
            full,
            runtime,
        },
    )?;
    let resubstitution_elapsed = resubstitution_started.elapsed();
    network.freeze();
    Ok(Some(MultiOutputSubject {
        network,
        roots: choice_roots.into_boxed_slice(),
        profile: MultiOutputProfile {
            cover: cover_elapsed,
            factoring: factoring_elapsed,
            resubstitution: resubstitution_elapsed,
            relation_checks: relation_profile.checks,
            plan_queries: relation_profile.plan_queries,
        },
    }))
}

fn factor_products(
    cubes: &[Cube],
    terms: &[Vec<usize>],
    input_count: usize,
) -> (Vec<Vec<usize>>, Vec<(usize, usize)>) {
    let mut products = cubes
        .iter()
        .map(|cube| {
            cube.literals
                .iter()
                .map(|&(input, inverted)| input * 2 + usize::from(inverted))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let mut seen = vec![false; cubes.len()];
    for &cube in terms.iter().flatten() {
        seen[cube] = true;
    }
    let mut definitions = Vec::new();
    loop {
        let mut counts = HashMap::<(usize, usize), usize>::new();
        for product in products
            .iter()
            .enumerate()
            .filter(|(cube, _)| seen[*cube])
            .map(|(_, product)| product)
        {
            for left in 0..product.len() {
                for right in left + 1..product.len() {
                    let pair = if product[left] < product[right] {
                        (product[left], product[right])
                    } else {
                        (product[right], product[left])
                    };
                    *counts.entry(pair).or_default() += 1;
                }
            }
        }
        let Some((pair, uses)) =
            counts
                .into_iter()
                .max_by(|(left_pair, left_uses), (right_pair, right_uses)| {
                    left_uses
                        .cmp(right_uses)
                        .then_with(|| right_pair.cmp(left_pair))
                })
        else {
            break;
        };
        if uses < 2 {
            break;
        }
        let factor = input_count * 2 + definitions.len();
        definitions.push(pair);
        for product in &mut products {
            if let (Some(left), Some(right)) = (
                product.iter().position(|&item| item == pair.0),
                product.iter().position(|&item| item == pair.1),
            ) {
                let (first, second) = if left < right {
                    (left, right)
                } else {
                    (right, left)
                };
                product.swap_remove(second);
                product.swap_remove(first);
                product.push(factor);
            }
        }
    }
    (products, definitions)
}

fn resubstitute_roots(
    network: &mut LogicGraph,
    fallback: &[LogicNodeId],
    budget: usize,
    context: &RelationContext<'_>,
) -> Result<(Vec<LogicNodeId>, RelationProfile), crate::SynthError> {
    let variables = context.variables;
    let outputs = context.outputs;
    let input_count = context.input_count;
    let full = context.full;
    let runtime = context.runtime;
    let mut profile = RelationProfile::default();
    let variable_bits = (0..input_count)
        .map(|input| variable_bits(input, input_count))
        .collect::<Vec<_>>();
    let features = variable_bits
        .iter()
        .copied()
        .chain(outputs.iter().copied())
        .collect::<Vec<_>>();
    let separations = outputs
        .iter()
        .map(|&target| SeparationMatrix::new(target, &features, input_count, full))
        .collect::<Vec<_>>();
    let mut order = (0..outputs.len()).collect::<Vec<_>>();
    let gains = outputs
        .iter()
        .enumerate()
        .map(|(divisor_index, &divisor)| {
            outputs
                .iter()
                .zip(&separations)
                .map(|(&target, separation)| {
                    relation_gain(
                        divisor,
                        target,
                        divisor_index,
                        input_count,
                        separation,
                        &mut profile,
                    )
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    order.sort_unstable_by(|&left, &right| {
        let outgoing = |output: usize| gains[output].iter().sum::<usize>();
        let incoming = |output: usize| gains.iter().map(|row| row[output]).sum::<usize>();
        outgoing(right)
            .saturating_add(incoming(left))
            .cmp(&outgoing(left).saturating_add(incoming(right)))
            .then_with(|| outgoing(right).cmp(&outgoing(left)))
            .then_with(|| left.cmp(&right))
    });
    let supports = order
        .iter()
        .map(|&output| {
            (0..input_count)
                .filter(|&input| depends_on(outputs[output], input, input_count))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let capacities = supports
        .iter()
        .enumerate()
        .map(|(position, support)| relation_capacity(position, support.len()))
        .collect::<Vec<_>>();
    let budgets = allocate_relation_budgets(&capacities, budget);
    let work = order
        .iter()
        .copied()
        .zip(supports)
        .zip(budgets)
        .enumerate()
        .map(|(position, ((output, support), budget))| RelationWork {
            position,
            output,
            support,
            budget,
        })
        .collect::<Vec<_>>();
    let analyze = |mut work: RelationWork| {
        let mut local_profile = RelationProfile::default();
        let search = relation_candidates(
            outputs[work.output],
            RelationInputs {
                support: &work.support,
                variable_bits: &variable_bits,
                outputs,
                divisors: &order[..work.position],
                separation: &separations[work.output],
                input_count,
                full,
            },
            &mut work.budget,
            &mut local_profile,
        );
        Ok::<_, crate::SynthError>((work.output, search, local_profile))
    };
    let analyzed = if let Some(runtime) = runtime {
        let tasks = work
            .into_iter()
            .map(|work| {
                Task::new(
                    TaskKey::new(
                        RELATION_SEARCH_TASK_DOMAIN,
                        u64::try_from(work.position).expect("output count is bounded"),
                    ),
                    work,
                )
            })
            .collect();
        runtime.map_ordered(tasks, analyze)?
    } else {
        work.into_iter()
            .map(analyze)
            .collect::<Result<Vec<_>, _>>()?
    };
    let searches = analyzed
        .into_iter()
        .map(|(output, search, local_profile)| {
            profile.merge(&local_profile);
            (output, search)
        })
        .collect::<Vec<_>>();
    let plans = solve_relation_plans(&searches, input_count, runtime)?;
    let mut roots = fallback.to_owned();
    for (position, (output, search)) in searches.into_iter().enumerate() {
        let selected = if let Some(zero) = search.zero {
            Some((zero.plan, zero.features))
        } else {
            search
                .candidates
                .into_iter()
                .min_by_key(|candidate| plans[&candidate.key].0)
                .map(|candidate| (plans[&candidate.key].1.clone(), candidate.features))
        };
        if let Some((plan, features)) = selected {
            let leaves = features
                .into_iter()
                .map(|feature| match feature {
                    RelationFeature::Variable(input) => variables[input],
                    RelationFeature::Divisor(divisor) => roots[order[divisor]],
                })
                .collect::<SmallVec<[LogicNodeId; super::MAX_MATCH_INPUTS]>>();
            roots[output] = plan.materialize(network, &leaves);
        }
        debug_assert_eq!(output, order[position]);
    }
    Ok((roots, profile))
}

fn relation_gain(
    divisor: u128,
    target: u128,
    divisor_index: usize,
    input_count: usize,
    separation: &SeparationMatrix,
    profile: &mut RelationProfile,
) -> usize {
    if divisor == target {
        return 0;
    }
    let support = (0..input_count)
        .filter(|&input| depends_on(target, input, input_count))
        .collect::<Vec<_>>();
    (0..super::MAX_MATCH_INPUTS.min(support.len()))
        .find(|&count| {
            let mut found = false;
            visit_combinations(support.len(), count, |positions| {
                profile.checks += 1;
                let variables = positions
                    .iter()
                    .fold(0usize, |mask, &position| mask | 1 << support[position]);
                found =
                    separation.determines(variables, &std::iter::once(input_count + divisor_index));
                !found
            });
            found
        })
        .map_or(0, |count| support.len().saturating_sub(count))
}

#[derive(Clone, Copy)]
struct RelationInputs<'a> {
    support: &'a [usize],
    variable_bits: &'a [u128],
    outputs: &'a [u128],
    divisors: &'a [usize],
    separation: &'a SeparationMatrix,
    input_count: usize,
    full: u128,
}

struct RelationWork {
    position: usize,
    output: usize,
    support: Vec<usize>,
    budget: usize,
}

fn relation_capacity(divisor_count: usize, support_count: usize) -> usize {
    (1..=super::MAX_MATCH_INPUTS.min(divisor_count))
        .map(|divisors| {
            let raw_cap = super::MAX_MATCH_INPUTS - divisors;
            let raw = (0..=raw_cap.min(support_count))
                .map(|count| combination_count(support_count, count))
                .sum::<usize>();
            combination_count(divisor_count, divisors) * raw
        })
        .sum()
}

fn combination_count(items: usize, choices: usize) -> usize {
    let choices = choices.min(items - choices);
    (1..=choices).fold(1, |count, index| count * (items + 1 - index) / index)
}

fn allocate_relation_budgets(capacities: &[usize], mut budget: usize) -> Vec<usize> {
    capacities
        .iter()
        .enumerate()
        .map(|(position, &capacity)| {
            let share = budget / (capacities.len() - position);
            let allocated = capacity.min(share);
            budget -= allocated;
            allocated
        })
        .collect()
}

type RelationPlanKey = (u64, usize);
type RelationPlan = (u32, Arc<Plan>);

#[derive(Clone, Copy)]
enum RelationFeature {
    Variable(usize),
    Divisor(usize),
}

struct RelationCandidate {
    key: RelationPlanKey,
    features: SmallVec<[RelationFeature; super::MAX_MATCH_INPUTS]>,
}

struct ZeroCostRelation {
    plan: Arc<Plan>,
    features: SmallVec<[RelationFeature; super::MAX_MATCH_INPUTS]>,
}

#[derive(Default)]
struct RelationSearch {
    candidates: Vec<RelationCandidate>,
    zero: Option<ZeroCostRelation>,
}

fn relation_candidates(
    target: u128,
    inputs: RelationInputs<'_>,
    budget: &mut usize,
    profile: &mut RelationProfile,
) -> RelationSearch {
    let RelationInputs {
        support,
        variable_bits,
        outputs,
        divisors: built,
        separation,
        input_count,
        full,
    } = inputs;
    let mut search = RelationSearch::default();
    for count in 1..=super::MAX_MATCH_INPUTS.min(built.len()) {
        visit_combinations(built.len(), count, |divisors| {
            let raw_cap = super::MAX_MATCH_INPUTS.saturating_sub(divisors.len());
            for raw_count in 0..=raw_cap.min(support.len()) {
                visit_combinations(support.len(), raw_count, |positions| {
                    if *budget == 0 {
                        return false;
                    }
                    *budget -= 1;
                    profile.checks += 1;
                    let variables = positions
                        .iter()
                        .fold(0usize, |mask, &position| mask | 1 << support[position]);
                    if !separation.determines(
                        variables,
                        &divisors
                            .iter()
                            .map(|&position| input_count + built[position]),
                    ) {
                        return true;
                    }
                    let mut bits = positions
                        .iter()
                        .map(|&position| variable_bits[support[position]])
                        .collect::<Vec<_>>();
                    bits.extend(divisors.iter().map(|&position| outputs[built[position]]));
                    let (truth, care) = project(target, &bits, full)
                        .expect("separating features determine the target");
                    profile.plan_queries += 1;
                    let features = positions
                        .iter()
                        .map(|&position| RelationFeature::Variable(support[position]))
                        .chain(
                            divisors
                                .iter()
                                .map(|&position| RelationFeature::Divisor(position)),
                        )
                        .collect::<SmallVec<[_; super::MAX_MATCH_INPUTS]>>();
                    if let Some(plan) = zero_cost_plan(truth, care) {
                        search.zero = Some(ZeroCostRelation { plan, features });
                        return false;
                    }
                    search.candidates.push(RelationCandidate {
                        key: (truth.bits, truth.input_count),
                        features,
                    });
                    true
                });
                if *budget == 0 || search.zero.is_some() {
                    return false;
                }
            }
            true
        });
        if *budget == 0 || search.zero.is_some() {
            return search;
        }
    }
    search
}

fn zero_cost_plan(truth: TruthTable, care: u64) -> Option<Arc<Plan>> {
    let assignments = 1usize << truth.input_count;
    let full = if assignments == u64::BITS as usize {
        u64::MAX
    } else {
        (1 << assignments) - 1
    };
    let bits = truth.bits & full;
    if bits & care == 0 {
        return Some(Arc::new(Plan::Constant(false)));
    }
    if (bits ^ full) & care == 0 {
        return Some(Arc::new(Plan::Constant(true)));
    }
    (0..truth.input_count).find_map(|var| {
        let variable = (0..assignments).fold(0, |value, assignment| {
            value | (((assignment >> var) & 1) as u64) << assignment
        });
        (bits == variable || bits == variable ^ full).then(|| {
            Arc::new(Plan::Literal {
                var: u8::try_from(var).expect("relation input count is bounded"),
                inverted: bits != variable,
            })
        })
    })
}

fn solve_relation_plans(
    searches: &[(usize, RelationSearch)],
    input_count: usize,
    runtime: Option<&ExecutionContext>,
) -> Result<HashMap<RelationPlanKey, RelationPlan>, crate::SynthError> {
    let mut seen = HashSet::new();
    let keys = searches
        .iter()
        .flat_map(|(_, search)| &search.candidates)
        .filter_map(|candidate| seen.insert(candidate.key).then_some(candidate.key))
        .collect::<Vec<_>>();
    if keys.is_empty() {
        return Ok(HashMap::new());
    }
    let workers = runtime
        .map_or(1, ExecutionContext::parallelism)
        .min(keys.len());
    let grain = keys.len().div_ceil(workers);
    let capacity =
        ((1usize << input_count) * (1usize << super::MAX_MATCH_INPUTS)).div_ceil(workers);
    let solve = |keys: &[RelationPlanKey]| {
        let mut synthesizer = Synthesizer::with_plan_capacity(capacity);
        Ok::<_, crate::SynthError>(
            keys.iter()
                .map(|&(bits, input_count)| {
                    let truth = TruthTable { input_count, bits };
                    ((bits, input_count), synthesizer.plan(truth))
                })
                .collect::<Vec<_>>(),
        )
    };
    let planned = if let Some(runtime) = runtime {
        let tasks = keys
            .chunks(grain)
            .enumerate()
            .map(|(ordinal, keys)| {
                Task::new(
                    TaskKey::new(
                        RELATION_PLAN_TASK_DOMAIN,
                        u64::try_from(ordinal).expect("relation task count is bounded"),
                    ),
                    keys,
                )
            })
            .collect::<Vec<_>>();
        runtime.map_ordered(tasks, solve)?
    } else {
        vec![solve(&keys)?]
    };
    Ok(planned.into_iter().flatten().collect())
}

/// Bit-parallel proof that a feature set separates every pair of assignments
/// on which the target differs. This is equivalent to functional dependency,
/// but avoids reconstructing the projected truth table for rejected sets.
struct SeparationMatrix {
    words: usize,
    features: Box<[u64]>,
    variable_unions: Box<[u64]>,
    full: Box<[u64]>,
}

impl SeparationMatrix {
    fn new(target: u128, features: &[u128], variable_count: usize, full: u128) -> Self {
        let assignments = full.count_ones() as usize;
        let ones = (0..assignments)
            .filter(|&assignment| target & (1 << assignment) != 0)
            .collect::<Vec<_>>();
        let zeros = (0..assignments)
            .filter(|&assignment| target & (1 << assignment) == 0)
            .collect::<Vec<_>>();
        let conflicts = ones.len() * zeros.len();
        let words = conflicts.div_ceil(u64::BITS as usize);
        let mut matrix = vec![0u64; features.len() * words];
        for (feature, &bits) in features.iter().enumerate() {
            let mut conflict = 0;
            for &one in &ones {
                for &zero in &zeros {
                    if ((bits >> one) ^ (bits >> zero)) & 1 != 0 {
                        matrix[feature * words + conflict / u64::BITS as usize] |=
                            1 << (conflict % u64::BITS as usize);
                    }
                    conflict += 1;
                }
            }
        }
        let mut complete = vec![u64::MAX; words];
        let remainder = conflicts % u64::BITS as usize;
        if remainder != 0 {
            *complete.last_mut().expect("a remainder requires one word") = (1 << remainder) - 1;
        }
        let mut variable_unions = vec![0u64; (1 << variable_count) * words];
        for mask in 1usize..1 << variable_count {
            let variable = mask.trailing_zeros() as usize;
            let prior = mask & (mask - 1);
            for word in 0..words {
                variable_unions[mask * words + word] =
                    variable_unions[prior * words + word] | matrix[variable * words + word];
            }
        }
        Self {
            words,
            features: matrix.into_boxed_slice(),
            variable_unions: variable_unions.into_boxed_slice(),
            full: complete.into_boxed_slice(),
        }
    }

    fn determines(
        &self,
        variable_mask: usize,
        features: &(impl Iterator<Item = usize> + Clone),
    ) -> bool {
        (0..self.words).all(|word| {
            (*features).clone().fold(
                self.variable_unions[variable_mask * self.words + word],
                |covered, feature| covered | self.features[feature * self.words + word],
            ) == self.full[word]
        })
    }
}

fn depends_on(bits: u128, input: usize, input_count: usize) -> bool {
    (0..1usize << input_count)
        .filter(|assignment| assignment & (1 << input) == 0)
        .any(|assignment| {
            let other = assignment | (1 << input);
            ((bits >> assignment) ^ (bits >> other)) & 1 != 0
        })
}

fn project(target: u128, features: &[u128], full: u128) -> Option<(TruthTable, u64)> {
    let assignments = full.count_ones() as usize;
    let mut values = [None; 1 << super::MAX_MATCH_INPUTS];
    let mut care = 0u64;
    let mut truth = 0u64;
    for assignment in 0..assignments {
        let pattern = features
            .iter()
            .enumerate()
            .fold(0, |pattern, (input, bits)| {
                pattern | (((bits >> assignment) & 1) as usize) << input
            });
        let value = (target >> assignment) & 1 != 0;
        if values[pattern].is_some_and(|current| current != value) {
            return None;
        }
        values[pattern] = Some(value);
        care |= 1 << pattern;
        truth |= u64::from(value) << pattern;
    }
    Some((
        TruthTable {
            input_count: features.len(),
            bits: truth,
        },
        care,
    ))
}

fn simulate(source: &LogicGraph) -> Option<Simulation> {
    let mut inputs = Vec::new();
    let mut positions = HashMap::new();
    for index in 0..source.node_count() {
        let node = LogicNodeId::from_index(index);
        if let LogicNode::Var(origin) = source.node(node) {
            if inputs.len() == INPUT_CAP {
                return None;
            }
            positions.insert(origin, inputs.len());
            inputs.push((origin, node));
        }
    }
    let assignments = 1usize << inputs.len();
    let full = if assignments == u128::BITS as usize {
        u128::MAX
    } else {
        (1u128 << assignments) - 1
    };
    let variables = (0..inputs.len())
        .map(|position| variable_bits(position, inputs.len()))
        .collect::<Vec<_>>();
    let mut values = Vec::with_capacity(source.node_count());
    for index in 0..source.node_count() {
        let value = match source.node(LogicNodeId::from_index(index)) {
            LogicNode::Const(value) => full * u128::from(value),
            LogicNode::Var(origin) => variables[*positions.get(&origin)?],
            LogicNode::And(left, right) => {
                signal_value(&values, left, full) & signal_value(&values, right, full)
            }
            LogicNode::Xor(left, right) => {
                signal_value(&values, left, full) ^ signal_value(&values, right, full)
            }
            LogicNode::Mux {
                cond,
                then_value,
                else_value,
            } => {
                let select = signal_value(&values, cond, full);
                (select & signal_value(&values, then_value, full))
                    | (!select & signal_value(&values, else_value, full) & full)
            }
        };
        values.push(value);
    }
    Some((inputs, values, full))
}

fn signal_value(values: &[u128], signal: LogicNodeId, full: u128) -> u128 {
    let value = values[signal.index()];
    if signal.is_inverted() {
        value ^ full
    } else {
        value
    }
}

fn visit_combinations(
    item_count: usize,
    choice_count: usize,
    mut visit: impl FnMut(&[usize]) -> bool,
) {
    if choice_count == 0 {
        visit(&[]);
        return;
    }
    if choice_count > item_count {
        return;
    }
    let mut positions = (0..choice_count).collect::<Vec<_>>();
    loop {
        if !visit(&positions) {
            return;
        }
        let Some(pivot) = (0..choice_count)
            .rev()
            .find(|&index| positions[index] < item_count - choice_count + index)
        else {
            return;
        };
        positions[pivot] += 1;
        for index in pivot + 1..choice_count {
            positions[index] = positions[index - 1] + 1;
        }
    }
}

fn enumerate_cubes(input_count: usize, full: u128) -> Vec<Cube> {
    let mut cubes = Vec::new();
    for mut code in 1..3usize.pow(u32::try_from(input_count).expect("input count is bounded")) {
        let mut bits = full;
        let mut literals = Vec::new();
        for input in 0..input_count {
            let phase = code % 3;
            code /= 3;
            if phase == 0 {
                continue;
            }
            let variable = variable_bits(input, input_count);
            let inverted = phase == 1;
            bits &= if inverted { !variable } else { variable };
            literals.push((input, inverted));
        }
        cubes.push(Cube {
            bits,
            literals: literals.into_boxed_slice(),
        });
    }
    cubes
}

fn variable_bits(position: usize, input_count: usize) -> u128 {
    (0..1usize << input_count).fold(0, |bits, assignment| {
        bits | u128::from(assignment & (1 << position) != 0) << assignment
    })
}

fn cover(cubes: &[Cube], targets: &[u128]) -> Option<Vec<Vec<usize>>> {
    let mut uncovered = targets.to_vec();
    let mut terms = vec![Vec::new(); targets.len()];
    let mut used = vec![false; cubes.len()];
    while uncovered.iter().any(|&bits| bits != 0) {
        let mut best = None;
        for (index, cube) in cubes.iter().enumerate().filter(|(index, _)| !used[*index]) {
            let mut covered = 0u32;
            let mut cost = cube.literals.len().saturating_sub(1);
            for (output, (&target, &remaining)) in targets.iter().zip(&uncovered).enumerate() {
                if cube.bits & !target == 0 && cube.bits & remaining != 0 {
                    covered += (cube.bits & remaining).count_ones();
                    cost += usize::from(!terms[output].is_empty());
                }
            }
            if covered == 0 {
                continue;
            }
            let candidate = (index, covered, cost);
            if best.is_none_or(|current| precedes(candidate, current, cubes)) {
                best = Some(candidate);
            }
        }
        let (chosen, _, _) = best?;
        used[chosen] = true;
        for (output, (&target, remaining)) in targets.iter().zip(&mut uncovered).enumerate() {
            if cubes[chosen].bits & !target == 0 && cubes[chosen].bits & *remaining != 0 {
                *remaining &= !cubes[chosen].bits;
                terms[output].push(chosen);
            }
        }
    }
    Some(terms)
}

fn precedes(candidate: (usize, u32, usize), current: (usize, u32, usize), cubes: &[Cube]) -> bool {
    let (candidate_index, candidate_cover, candidate_cost) = candidate;
    let (current_index, current_cover, current_cost) = current;
    match (candidate_cost, current_cost) {
        (0, 0) => {}
        (0, _) => return true,
        (_, 0) => return false,
        _ => {
            let left = u64::from(candidate_cover) * current_cost as u64;
            let right = u64::from(current_cover) * candidate_cost as u64;
            if left != right {
                return left > right;
            }
        }
    }
    candidate_cover > current_cover
        || (candidate_cover == current_cover
            && (cubes[candidate_index].literals.len(), candidate_index)
                < (cubes[current_index].literals.len(), current_index))
}

fn product(network: &mut LogicGraph, factors: &[LogicNodeId], product: &[usize]) -> LogicNodeId {
    product
        .iter()
        .map(|&factor| factors[factor])
        .reduce(|left, right| network.and(left, right))
        .unwrap_or_else(|| LogicGraph::constant(true))
}

fn or(network: &mut LogicGraph, left: LogicNodeId, right: LogicNodeId) -> LogicNodeId {
    network.and(left.inverted(), right.inverted()).inverted()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn factors_a_cube_shared_by_multiple_outputs() {
        let a = variable_bits(0, 3);
        let b = variable_bits(1, 3);
        let c = variable_bits(2, 3);
        let outputs = [a & b & c, a & b & !c & 0xff];
        let subject = synthesize_multi_output(3, outputs).unwrap();

        for (actual, expected) in subject.roots.iter().zip(outputs) {
            assert_eq!(
                subject.network.truth_table(*actual, 3).bits,
                u64::try_from(expected).unwrap()
            );
        }
    }

    #[test]
    fn separation_filter_matches_exact_projection() {
        let inputs = (0..4)
            .map(|input| variable_bits(input, 4))
            .collect::<Vec<_>>();
        let target = (inputs[0] & inputs[1]) ^ (inputs[2] | inputs[3]);
        let features = inputs
            .iter()
            .copied()
            .chain([inputs[0] ^ inputs[2], inputs[1] & inputs[3]])
            .collect::<Vec<_>>();
        let full = 0xffff;
        let separation = SeparationMatrix::new(target, &features, inputs.len(), full);

        for count in 0..=super::super::MAX_MATCH_INPUTS {
            visit_combinations(features.len(), count, |selected| {
                let bits = selected
                    .iter()
                    .map(|&feature| features[feature])
                    .collect::<Vec<_>>();
                assert_eq!(
                    separation.determines(
                        selected
                            .iter()
                            .filter(|&&feature| feature < inputs.len())
                            .fold(0usize, |mask, &feature| mask | 1 << feature),
                        &selected
                            .iter()
                            .copied()
                            .filter(|&feature| feature >= inputs.len()),
                    ),
                    project(target, &bits, full).is_some()
                );
                true
            });
        }
    }

    #[test]
    fn preserves_complete_seven_input_truths() {
        let inputs = (0..INPUT_CAP)
            .map(|input| variable_bits(input, INPUT_CAP))
            .collect::<Vec<_>>();
        let outputs = [
            inputs
                .iter()
                .copied()
                .reduce(|left, right| left ^ right)
                .unwrap(),
            (inputs[0] & inputs[6]) | (inputs[2] & !inputs[4]),
        ];
        let subject = synthesize_multi_output(INPUT_CAP, outputs).unwrap();
        let (_, values, full) = simulate(&subject.network).unwrap();

        for (actual, expected) in subject.roots.iter().zip(outputs) {
            assert_eq!(signal_value(&values, *actual, full), expected);
        }
    }
}
