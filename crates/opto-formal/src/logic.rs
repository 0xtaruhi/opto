// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! SAT encodings for Boolean logic networks.
//!
//! Networks are encoded into CNF lazily from the requested outputs. Boundary
//! inputs with the same origin share one SAT literal across compared networks.

use super::{FormalError, ProofOutcome, ProofReport};
use std::collections::BTreeMap;
use varisat::{ExtendFormula, Lit, Solver};

/// Proves equivalence between two Boolean networks at corresponding outputs.
///
/// A proof is returned when the XOR of all output pairs is unsatisfiable.
///
/// # Errors
///
/// Returns an error for empty or mismatched output sets, malformed network
/// references, capacity overflow, or SAT solver failure.
pub fn prove_logic_network_equivalence(
    reference: &opto_ir::logic::LogicNetwork,
    reference_outputs: &[opto_ir::logic::Lit],
    implementation: &opto_ir::logic::LogicNetwork,
    implementation_outputs: &[opto_ir::logic::Lit],
) -> Result<ProofOutcome, FormalError> {
    if reference_outputs.len() != implementation_outputs.len() {
        return Err(FormalError::invalid(format!(
            "logic miter output count mismatch: reference={}, implementation={}",
            reference_outputs.len(),
            implementation_outputs.len()
        )));
    }
    if reference_outputs.is_empty() {
        return Err(FormalError::invalid(
            "logic miter requires at least one cut output",
        ));
    }

    let mut miter = LogicMiter::new();
    let reference_lits = miter.encode_network(reference, reference_outputs)?;
    let implementation_lits = miter.encode_network(implementation, implementation_outputs)?;

    let differences = reference_lits
        .into_iter()
        .zip(implementation_lits)
        .map(|(reference, implementation)| miter.xor(reference, implementation))
        .collect::<Vec<_>>();
    miter.add_clause(&differences);
    let satisfiable = miter.solver.solve().map_err(|source| FormalError::Solver {
        context: "logic miter",
        source,
    })?;
    if satisfiable {
        return Ok(ProofOutcome::disproved(
            "logic miter found a cut-boundary counterexample",
        ));
    }
    Ok(ProofOutcome::proved(ProofReport {
        encoded_values: miter.encoded_nodes,
        clauses: miter.clauses,
    }))
}

/// One boundary assignment that separates a refuted pair.
///
/// The caller owns the meaning of the origins: they are the `origin` values of
/// the encoded network's input nodes. Only inputs the solver actually assigned
/// appear, so a caller folding these into simulation vectors must supply its own
/// value for an absent origin rather than assuming a default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundaryRefutation {
    assignment: Vec<(u32, bool)>,
}

impl BoundaryRefutation {
    #[must_use]
    /// Returns the assigned boundary origins in ascending order.
    pub fn assignment(&self) -> &[(u32, bool)] {
        &self.assignment
    }
}

/// Partition simulation-equivalent literal classes using one incremental SAT
/// instance. Each returned member names an earlier, formally equivalent
/// representative in its input class. The explicit budgets bound both solver
/// work and the amount of equivalence information retained by callers.
///
/// Every refuted pair appends one [`BoundaryRefutation`] to `refutations`. A
/// caller that refines its own candidate nomination from those assignments
/// converges in far fewer solver calls than one that re-nominates the same
/// separable pair every round.
///
/// # Errors
///
/// Returns an error for malformed network references, compact-capacity
/// overflow, or an incremental SAT solver failure.
pub fn prove_logic_literal_partitions(
    network: &opto_ir::logic::LogicNetwork,
    classes: &[Vec<opto_ir::logic::Lit>],
    max_representatives: usize,
    max_pairs: usize,
    refutations: &mut Vec<BoundaryRefutation>,
) -> Result<Vec<Vec<Option<usize>>>, FormalError> {
    let outputs = classes.iter().flatten().copied().collect::<Vec<_>>();
    if outputs.is_empty() || max_representatives == 0 || max_pairs == 0 {
        return Ok(classes
            .iter()
            .map(|class| vec![None; class.len()])
            .collect());
    }

    let mut class_offsets = Vec::with_capacity(classes.len());
    let mut next_offset = 0usize;
    for class in classes {
        class_offsets.push(next_offset);
        next_offset = next_offset.checked_add(class.len()).ok_or_else(|| {
            FormalError::invalid("logic equivalence class storage exceeds capacity")
        })?;
    }
    let mut encoder = LogicMiter::new();
    let encoded_literals = encoder.encode_network(network, &outputs)?;
    let mut representatives = classes
        .iter()
        .map(|class| vec![None; class.len()])
        .collect::<Vec<_>>();
    let mut unresolved = classes
        .iter()
        .map(|class| (0..class.len()).collect::<Vec<_>>())
        .collect::<Vec<_>>();
    let mut pair_count = 0usize;

    for _ in 0..max_representatives {
        let mut advanced = false;
        for class_index in 0..classes.len() {
            if unresolved[class_index].len() < 2 {
                continue;
            }
            advanced = true;
            let representative = unresolved[class_index][0];
            let mut remaining = Vec::new();
            for &alternative in &unresolved[class_index][1..] {
                if pair_count == max_pairs {
                    encoder.solver.assume(&[]);
                    return Ok(representatives);
                }
                pair_count += 1;
                let base = class_offsets[class_index];
                if prove_encoded_literal_equivalence(
                    &mut encoder,
                    encoded_literals[base + representative],
                    encoded_literals[base + alternative],
                    refutations,
                )? {
                    representatives[class_index][alternative] = Some(representative);
                } else {
                    remaining.push(alternative);
                }
            }
            unresolved[class_index] = remaining;
        }
        if !advanced {
            break;
        }
    }
    encoder.solver.assume(&[]);
    Ok(representatives)
}

fn prove_encoded_literal_equivalence(
    encoder: &mut LogicMiter,
    left: Lit,
    right: Lit,
    refutations: &mut Vec<BoundaryRefutation>,
) -> Result<bool, FormalError> {
    for assumption in [[left, !right], [!left, right]] {
        encoder.solver.assume(&assumption);
        let separable = encoder
            .solver
            .solve()
            .map_err(|source| FormalError::Solver {
                context: "logic equivalence sweep",
                source,
            })?;
        if separable {
            refutations.push(encoder.boundary_assignment());
            return Ok(false);
        }
    }
    Ok(true)
}

struct LogicMiter {
    solver: Solver<'static>,
    constant_false: Lit,
    inputs: BTreeMap<u32, Lit>,
    clauses: usize,
    encoded_nodes: usize,
}

pub(crate) trait LogicEncoding {
    fn constant_false(&mut self) -> Lit;
    fn input(&mut self, origin: u32) -> Result<Lit, FormalError>;
    fn and(&mut self, left: Lit, right: Lit) -> Lit;
    fn xor(&mut self, left: Lit, right: Lit) -> Lit;
    fn mux(&mut self, select: Lit, then_value: Lit, else_value: Lit) -> Lit;
}

pub(crate) fn encode_logic_network(
    encoder: &mut impl LogicEncoding,
    network: &opto_ir::logic::LogicNetwork,
    outputs: &[opto_ir::logic::Lit],
) -> Result<(Vec<Lit>, usize), FormalError> {
    use opto_ir::logic::{NodeId, NodeKind};

    let mut encoded_nodes = vec![None; network.node_count()];
    encoded_nodes[NodeId::CONSTANT.index()] = Some(encoder.constant_false());
    let mut pending = outputs
        .iter()
        .map(|literal| literal.node())
        .filter(|node| *node != NodeId::CONSTANT)
        .collect::<Vec<_>>();
    let mut order = Vec::new();
    let mut visited = vec![false; network.node_count()];
    while let Some(node) = pending.pop() {
        let slot = visited.get_mut(node.index()).ok_or_else(|| {
            FormalError::invalid(format!("logic miter references unknown node {node:?}"))
        })?;
        if *slot {
            continue;
        }
        *slot = true;
        order.push(node);
        let fanins = network.fanin_count(node).ok_or_else(|| {
            FormalError::invalid(format!("logic miter references unknown node {node:?}"))
        })?;
        for index in 0..fanins {
            let fanin = network.fanin(node, index).ok_or_else(|| {
                FormalError::invalid(format!("logic miter node {node:?} has no fanin {index}"))
            })?;
            if fanin.node() != NodeId::CONSTANT {
                pending.push(fanin.node());
            }
        }
    }
    order.sort_by_key(|node| network.level(*node).unwrap_or(u32::MAX));

    for node in order.iter().copied() {
        let kind = network.kind(node).ok_or_else(|| {
            FormalError::invalid(format!("logic miter references unknown node {node:?}"))
        })?;
        let output = match kind {
            NodeKind::Constant => encoder.constant_false(),
            NodeKind::Input => {
                let origin = network.origin(node).ok_or_else(|| {
                    FormalError::invalid(format!(
                        "logic miter input {node:?} has no boundary origin"
                    ))
                })?;
                encoder.input(origin)?
            }
            NodeKind::And => {
                let left = encoded_literal(network, &encoded_nodes, node, 0)?;
                let right = encoded_literal(network, &encoded_nodes, node, 1)?;
                encoder.and(left, right)
            }
            NodeKind::Xor => {
                let left = encoded_literal(network, &encoded_nodes, node, 0)?;
                let right = encoded_literal(network, &encoded_nodes, node, 1)?;
                encoder.xor(left, right)
            }
            NodeKind::Mux => {
                let select = encoded_literal(network, &encoded_nodes, node, 0)?;
                let then_value = encoded_literal(network, &encoded_nodes, node, 1)?;
                let else_value = encoded_literal(network, &encoded_nodes, node, 2)?;
                encoder.mux(select, then_value, else_value)
            }
        };
        encoded_nodes[node.index()] = Some(output);
    }

    let outputs = outputs
        .iter()
        .map(|literal| {
            encoded_nodes
                .get(literal.node().index())
                .copied()
                .flatten()
                .map(|value| if literal.is_inverted() { !value } else { value })
                .ok_or_else(|| {
                    FormalError::invalid(format!(
                        "logic miter output references unencoded node {:?}",
                        literal.node()
                    ))
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok((outputs, order.len()))
}

impl LogicMiter {
    fn new() -> Self {
        let mut solver = Solver::new();
        let constant_false = solver.new_lit();
        solver.add_clause(&[!constant_false]);
        Self {
            solver,
            constant_false,
            inputs: BTreeMap::new(),
            clauses: 1,
            encoded_nodes: 0,
        }
    }

    /// Reads the boundary half of the current satisfying assignment.
    ///
    /// Only encoded inputs are reported, in ascending origin order, so the
    /// result is stable across solver runs that assign unrelated internal
    /// variables differently.
    fn boundary_assignment(&self) -> BoundaryRefutation {
        let model = self.solver.model().unwrap_or_default();
        let mut values = vec![None; model.iter().map(|lit| lit.index() + 1).max().unwrap_or(0)];
        for literal in model {
            values[literal.index()] = Some(literal.is_positive());
        }
        BoundaryRefutation {
            assignment: self
                .inputs
                .iter()
                .filter_map(|(&origin, literal)| {
                    let value = values.get(literal.index()).copied().flatten()?;
                    Some((origin, value != literal.is_negative()))
                })
                .collect(),
        }
    }

    fn encode_network(
        &mut self,
        network: &opto_ir::logic::LogicNetwork,
        outputs: &[opto_ir::logic::Lit],
    ) -> Result<Vec<Lit>, FormalError> {
        let (outputs, encoded_nodes) = encode_logic_network(self, network, outputs)?;
        self.encoded_nodes += encoded_nodes;
        Ok(outputs)
    }

    fn and(&mut self, left: Lit, right: Lit) -> Lit {
        let output = self.solver.new_lit();
        self.add_clause(&[!left, !right, output]);
        self.add_clause(&[left, !output]);
        self.add_clause(&[right, !output]);
        output
    }

    fn xor(&mut self, left: Lit, right: Lit) -> Lit {
        let output = self.solver.new_lit();
        self.add_clause(&[!left, !right, !output]);
        self.add_clause(&[!left, right, output]);
        self.add_clause(&[left, !right, output]);
        self.add_clause(&[left, right, !output]);
        output
    }

    fn mux(&mut self, select: Lit, then_value: Lit, else_value: Lit) -> Lit {
        let output = self.solver.new_lit();
        self.add_clause(&[!select, !then_value, output]);
        self.add_clause(&[!select, then_value, !output]);
        self.add_clause(&[select, !else_value, output]);
        self.add_clause(&[select, else_value, !output]);
        output
    }

    fn add_clause(&mut self, clause: &[Lit]) {
        self.solver.add_clause(clause);
        self.clauses += 1;
    }
}

impl LogicEncoding for LogicMiter {
    fn constant_false(&mut self) -> Lit {
        self.constant_false
    }

    fn input(&mut self, origin: u32) -> Result<Lit, FormalError> {
        Ok(*self
            .inputs
            .entry(origin)
            .or_insert_with(|| self.solver.new_lit()))
    }

    fn and(&mut self, left: Lit, right: Lit) -> Lit {
        self.and(left, right)
    }

    fn xor(&mut self, left: Lit, right: Lit) -> Lit {
        self.xor(left, right)
    }

    fn mux(&mut self, select: Lit, then_value: Lit, else_value: Lit) -> Lit {
        self.mux(select, then_value, else_value)
    }
}

fn encoded_literal(
    network: &opto_ir::logic::LogicNetwork,
    encoded: &[Option<Lit>],
    node: opto_ir::logic::NodeId,
    fanin: usize,
) -> Result<Lit, FormalError> {
    let literal = network.fanin(node, fanin).ok_or_else(|| {
        FormalError::invalid(format!("logic miter node {node:?} has no fanin {fanin}"))
    })?;
    let value = encoded
        .get(literal.node().index())
        .copied()
        .flatten()
        .ok_or_else(|| {
            FormalError::invalid(format!(
                "logic miter node {node:?} is not topological at fanin {:?}",
                literal.node()
            ))
        })?;
    Ok(if literal.is_inverted() { !value } else { value })
}
