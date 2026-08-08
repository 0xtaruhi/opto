// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::logic_constant;
use super::network::{LogicGraph, LogicNodeId};
use hashbrown::{HashMap, HashSet};
use opto_ir::word;
use opto_runtime::ExecutionContext;

/// Immutable region-local Boolean graph consumed by technology mapping.
pub(crate) struct RegionLogicGraph {
    network: LogicGraph,
    implementations: Box<[RegionLogicImplementation]>,
    inputs: Box<[word::ValueId]>,
}

pub(crate) struct RegionLogicImplementation {
    pass: &'static str,
    value_nodes: Box<[(word::ValueId, LogicNodeId)]>,
}

#[derive(Clone, Copy)]
pub(crate) struct RegionLogicOptions<'a> {
    pub(crate) optimize: bool,
    pub(crate) config: crate::SynthesisConfig,
    pub(crate) runtime: &'a ExecutionContext,
    pub(crate) incremental: Option<super::rewrite::RewriteIncremental<'a>>,
    pub(crate) boundary_inputs: &'a [word::ValueId],
}

impl RegionLogicGraph {
    pub(crate) fn new_cached(
        module: &word::WordModule,
        roots: &[word::ValueId],
        requirements: &[Option<f64>],
        options: RegionLogicOptions<'_>,
    ) -> Result<Self, crate::SynthError> {
        Self::build(module, roots, requirements, options)
    }

    fn build(
        module: &word::WordModule,
        roots: &[word::ValueId],
        requirements: &[Option<f64>],
        options: RegionLogicOptions<'_>,
    ) -> Result<Self, crate::SynthError> {
        if roots.len() != requirements.len() {
            return Err(crate::SynthError::invariant(
                "Boolean subject requirements do not align with roots",
            ));
        }
        let RegionLogicOptions {
            optimize,
            config,
            runtime,
            incremental,
            boundary_inputs,
        } = options;
        let mut builder = LogicNetworkBuilder::new(module, boundary_inputs)?;
        for &root in roots {
            builder.value(root)?;
        }
        builder.network.freeze();
        crate::api::diagnostics::trace!(
            crate::api::diagnostics::SynthTrace::timing(config.diagnostics),
            "logic.network",
            "nodes={}",
            builder.network.node_count()
        );
        let inputs = builder
            .input_nodes
            .iter()
            .map(|&node| {
                node_value(&builder.node_values, node).ok_or_else(|| {
                    crate::SynthError::invariant(format!(
                        "logic subject input node {} has no word-level value",
                        node.index()
                    ))
                })
            })
            .collect::<Result<Vec<_>, _>>()?
            .into_boxed_slice();
        let root_entries = roots
            .iter()
            .zip(requirements)
            .filter_map(|(root, requirement)| {
                builder
                    .nodes
                    .get(root)
                    .copied()
                    .flatten()
                    .map(|node| (*root, node, *requirement))
            })
            .collect::<Vec<_>>();
        let root_nodes = root_entries
            .iter()
            .map(|&(_, node, _)| node)
            .collect::<Vec<_>>();
        let root_requirements = root_entries
            .iter()
            .map(|&(_, _, requirement)| requirement)
            .collect::<Vec<_>>();
        let optimized = super::pipeline::optimize(
            std::mem::replace(&mut builder.network, LogicGraph::new()),
            &root_nodes,
            &root_requirements,
            optimize,
            config.diagnostics,
            runtime,
            incremental,
        )?;
        let mut value_nodes = builder
            .nodes
            .into_iter()
            .filter_map(|(value, node)| {
                node.and_then(|node| {
                    super::rewrite::remap_literal(&optimized.remap, node).map(|node| (value, node))
                })
            })
            .collect::<Vec<_>>();
        value_nodes.sort_unstable_by_key(|&(value, _)| value);
        let mut implementations = Vec::with_capacity(1 + optimized.alternatives.len());
        implementations.push(RegionLogicImplementation {
            pass: "baseline",
            value_nodes: value_nodes.into_boxed_slice(),
        });
        for alternative in optimized.alternatives {
            if alternative.roots.len() != root_entries.len() {
                return Err(crate::SynthError::invariant(
                    "AXM alternative roots do not align with subject roots",
                ));
            }
            let mut value_nodes = root_entries
                .iter()
                .map(|&(value, _, _)| value)
                .zip(alternative.roots)
                .collect::<Vec<_>>();
            value_nodes.sort_unstable_by_key(|&(value, _)| value);
            value_nodes.dedup_by_key(|(value, _)| *value);
            implementations.push(RegionLogicImplementation {
                pass: alternative.pass,
                value_nodes: value_nodes.into_boxed_slice(),
            });
        }
        crate::api::diagnostics::trace!(
            crate::api::diagnostics::SynthTrace::timing(config.diagnostics),
            "logic.network_optimized",
            "nodes={} implementations={}",
            optimized.network.node_count(),
            implementations.len()
        );
        Ok(Self {
            network: optimized.network,
            implementations: implementations.into_boxed_slice(),
            inputs,
        })
    }

    pub(crate) fn network(&self) -> &LogicGraph {
        &self.network
    }

    pub(crate) fn inputs(&self) -> &[word::ValueId] {
        &self.inputs
    }

    #[cfg(test)]
    pub(crate) fn node(&self, value: word::ValueId) -> Option<LogicNodeId> {
        self.implementations.first()?.node(value)
    }

    pub(crate) fn implementations(&self) -> &[RegionLogicImplementation] {
        &self.implementations
    }
}

impl RegionLogicImplementation {
    pub(crate) const fn pass(&self) -> &'static str {
        self.pass
    }

    pub(crate) fn node(&self, value: word::ValueId) -> Option<LogicNodeId> {
        let index = self
            .value_nodes
            .binary_search_by_key(&value, |&(candidate, _)| candidate)
            .ok()?;
        Some(self.value_nodes[index].1)
    }
}

struct LogicNetworkBuilder<'a> {
    module: &'a word::WordModule,
    network: LogicGraph,
    nodes: HashMap<word::ValueId, Option<LogicNodeId>>,
    input_index: HashMap<u64, u32>,
    input_nodes: Vec<LogicNodeId>,
    node_values: Vec<[Option<word::ValueId>; 2]>,
    pending: Vec<(word::ValueId, bool)>,
    active: HashSet<word::ValueId>,
    boundary_inputs: HashSet<word::ValueId>,
    boundary_nodes: HashMap<word::ValueId, LogicNodeId>,
}

impl<'a> LogicNetworkBuilder<'a> {
    fn new(
        module: &'a word::WordModule,
        boundary_inputs: &[word::ValueId],
    ) -> Result<Self, crate::SynthError> {
        let mut boundary = HashSet::with_capacity(boundary_inputs.len());
        for &value in boundary_inputs {
            if module.value(value).is_none() {
                return Err(crate::SynthError::invariant(
                    "region boundary references an unknown logic value",
                ));
            }
            boundary.insert(value);
        }
        Ok(Self {
            module,
            network: LogicGraph::new(),
            nodes: HashMap::new(),
            input_index: HashMap::new(),
            input_nodes: Vec::new(),
            node_values: Vec::new(),
            pending: Vec::new(),
            active: HashSet::new(),
            boundary_inputs: boundary,
            boundary_nodes: HashMap::new(),
        })
    }

    fn value(&mut self, value: word::ValueId) -> Result<Option<LogicNodeId>, crate::SynthError> {
        if let Some(node) = self.nodes.get(&value).copied() {
            return Ok(node);
        }
        self.pending.clear();
        self.pending.push((value, false));
        while let Some((current, expanded)) = self.pending.pop() {
            if self.nodes.contains_key(&current) {
                continue;
            }
            if expanded {
                self.active.remove(&current);
                let node = self.compute_value(current)?;
                if let Some(node) = node {
                    self.record_node_value(node, current);
                }
                self.nodes.insert(current, node);
                continue;
            }
            if self.boundary_inputs.contains(&current) {
                self.active.remove(&current);
                let node = self.boundary_input(current)?;
                self.record_node_value(node, current);
                self.nodes.insert(current, Some(node));
                continue;
            }
            if !self.active.insert(current) {
                return Err(crate::SynthError::invariant(format!(
                    "cyclic scalar logic operation graph at {current:?}"
                )));
            }
            self.pending.push((current, true));
            let stored = self.module.value(current).ok_or_else(|| {
                crate::SynthError::invariant(format!("unknown RTL value {current:?}"))
            })?;
            if stored.ty.width() != 1 {
                continue;
            }
            let word::ValueKind::Operation(operation) = stored.kind else {
                continue;
            };
            let operation = self.module.operation(operation).ok_or_else(|| {
                crate::SynthError::invariant(format!("unknown RTL operation for {current:?}"))
            })?;
            let inputs = scalar_operation_operands(&operation.kind);
            for input in inputs.into_iter().flatten().rev() {
                if !self.nodes.contains_key(&input) {
                    self.pending.push((input, false));
                }
            }
        }
        self.nodes.get(&value).copied().ok_or_else(|| {
            crate::SynthError::invariant(format!("logic lowering did not resolve {value:?}"))
        })
    }
}

/// Enumerates every value the Boolean subject for `roots` will treat as a leaf.
///
/// The subject descends only through scalar logic operations, so a signal read,
/// a wider value, and an opaque operation such as a register or an extract are
/// all leaves — including a wire that this region itself drives, because the
/// subject never follows a connect. No set derived from boundary contracts or
/// cross-region dataflow can predict that, which is why the region interface is
/// enumerated here with the rule the subject actually applies.
pub(crate) fn subject_leaves(
    module: &word::WordModule,
    roots: &[word::ValueId],
    boundary_inputs: &[word::ValueId],
) -> Vec<word::ValueId> {
    let boundary = boundary_inputs
        .iter()
        .copied()
        .collect::<std::collections::BTreeSet<_>>();
    let mut leaves = std::collections::BTreeSet::new();
    let mut seen = std::collections::BTreeSet::new();
    let mut pending = roots.to_vec();
    while let Some(value) = pending.pop() {
        if !seen.insert(value) {
            continue;
        }
        if boundary.contains(&value) {
            leaves.insert(value);
            continue;
        }
        let Some(stored) = module.value(value) else {
            continue;
        };
        if stored.ty.width() != 1 {
            leaves.insert(value);
            continue;
        }
        match &stored.kind {
            word::ValueKind::Signal(_) => {
                leaves.insert(value);
            }
            word::ValueKind::Constant(_) => {}
            word::ValueKind::Operation(operation) => {
                let Some(operation) = module.operation(*operation) else {
                    leaves.insert(value);
                    continue;
                };
                let operands = scalar_operation_operands(&operation.kind);
                if operands.iter().all(Option::is_none) {
                    leaves.insert(value);
                    continue;
                }
                pending.extend(operands.into_iter().flatten());
            }
        }
    }
    leaves.into_iter().collect()
}

/// The operands a scalar logic operation decomposes into.
///
/// An operation with no operands here is opaque to the Boolean subject and
/// therefore becomes one of its leaves. This is the single definition of that
/// rule: the subject descends by it, and the region interface enumerates its
/// leaves by it, so the two cannot drift apart.
pub(crate) fn scalar_operation_operands(kind: &word::OpKind) -> [Option<word::ValueId>; 3] {
    match kind {
        word::OpKind::Unary { arg, .. } => [Some(*arg), None, None],
        word::OpKind::Binary { left, right, .. } => [Some(*left), Some(*right), None],
        word::OpKind::Mux {
            cond,
            then_value,
            else_value,
        } => [Some(*cond), Some(*then_value), Some(*else_value)],
        word::OpKind::Cast { value, .. } => [Some(*value), None, None],
        word::OpKind::Concat { .. }
        | word::OpKind::Extract { .. }
        | word::OpKind::DynamicExtract { .. }
        | word::OpKind::DynamicInsert { .. }
        | word::OpKind::Register(_)
        | word::OpKind::Latch(_) => [None, None, None],
    }
}

impl LogicNetworkBuilder<'_> {
    fn compute_value(
        &mut self,
        value: word::ValueId,
    ) -> Result<Option<LogicNodeId>, crate::SynthError> {
        let stored = self
            .module
            .value(value)
            .ok_or_else(|| crate::SynthError::invariant(format!("unknown RTL value {value:?}")))?;
        if stored.ty.width() != 1 {
            return Ok(None);
        }
        let base = match stored.kind.clone() {
            word::ValueKind::Signal(reference) => self.input(reference).map(Some),
            word::ValueKind::Constant(bits) => {
                Ok(logic_constant(&bits).map(LogicGraph::constant).or_else(|| {
                    self.undefined_scalar(value)
                        .then(|| LogicGraph::constant(false))
                }))
            }
            word::ValueKind::Operation(operation) => {
                let operation = self.module.operation(operation).ok_or_else(|| {
                    crate::SynthError::invariant(format!("unknown RTL operation {operation:?}"))
                })?;
                Self::validate_canonical_operation(value, &operation.kind)?;
                self.operation(&operation.kind)
            }
        }?;
        Ok(base)
    }

    fn input(&mut self, reference: word::SignalRef) -> Result<LogicNodeId, crate::SynthError> {
        if reference.width() != 1 {
            return Err(crate::SynthError::invariant(
                "logic network input must be a scalar signal reference",
            ));
        }
        let key = (u64::from(reference.signal.raw()) << 32) | u64::from(reference.lsb);
        if let Some(&index) = self.input_index.get(&key) {
            return Ok(self.input_nodes[index as usize]);
        }
        let index = self.input_nodes.len();
        let node = self.network.variable(index).ok_or_else(|| {
            crate::SynthError::capacity("logic network exceeds 32-bit input capacity")
        })?;
        let dense: u32 = index.try_into().map_err(|_| {
            crate::SynthError::capacity("logic network exceeds 32-bit input capacity")
        })?;
        self.input_index.insert(key, dense);
        self.input_nodes.push(node);
        Ok(node)
    }

    fn boundary_input(&mut self, value: word::ValueId) -> Result<LogicNodeId, crate::SynthError> {
        let stored = self.module.value(value).ok_or_else(|| {
            crate::SynthError::invariant(format!("unknown regional boundary value {value:?}"))
        })?;
        match &stored.kind {
            word::ValueKind::Signal(reference) => return self.input(*reference),
            word::ValueKind::Operation(operation) => {
                let operation = self.module.operation(*operation).ok_or_else(|| {
                    crate::SynthError::invariant(format!(
                        "unknown regional boundary operation {operation:?}"
                    ))
                })?;
                Self::validate_canonical_operation(value, &operation.kind)?;
            }
            word::ValueKind::Constant(_) => {}
        }
        if let Some(&node) = self.boundary_nodes.get(&value) {
            return Ok(node);
        }
        let node = self
            .network
            .variable(self.input_nodes.len())
            .ok_or_else(|| {
                crate::SynthError::capacity("region logic graph exceeds 32-bit input capacity")
            })?;
        self.input_nodes.push(node);
        self.boundary_nodes.insert(value, node);
        Ok(node)
    }

    fn validate_canonical_operation(
        value: word::ValueId,
        kind: &word::OpKind,
    ) -> Result<(), crate::SynthError> {
        let canonical = match kind {
            word::OpKind::Unary { .. }
            | word::OpKind::Mux { .. }
            | word::OpKind::Cast { .. }
            | word::OpKind::Register(_)
            | word::OpKind::Latch(_) => true,
            word::OpKind::Binary { op, .. } => matches!(
                op,
                word::BinaryOp::BitAnd
                    | word::BinaryOp::BitOr
                    | word::BinaryOp::BitXor
                    | word::BinaryOp::LogicalAnd
                    | word::BinaryOp::LogicalOr
                    | word::BinaryOp::Eq
                    | word::BinaryOp::Ne
            ),
            word::OpKind::Concat { .. }
            | word::OpKind::Extract { .. }
            | word::OpKind::DynamicExtract { .. }
            | word::OpKind::DynamicInsert { .. } => false,
        };
        if canonical {
            return Ok(());
        }
        Err(crate::SynthError::invariant(format!(
            "non-canonical Word operation {kind:?} for scalar value {value:?} reached the Boolean mapper"
        )))
    }

    fn operation(&mut self, kind: &word::OpKind) -> Result<Option<LogicNodeId>, crate::SynthError> {
        Ok(match kind {
            word::OpKind::Unary { op, arg } => {
                let Some(arg) = self.resolved(*arg)? else {
                    return Ok(None);
                };
                match op {
                    word::UnaryOp::BitNot | word::UnaryOp::LogicalNot => Some(LogicGraph::not(arg)),
                    word::UnaryOp::ReductionAnd
                    | word::UnaryOp::ReductionOr
                    | word::UnaryOp::ReductionXor => Some(arg),
                }
            }
            word::OpKind::Binary { op, left, right } => {
                match (self.undefined_scalar(*left), self.undefined_scalar(*right)) {
                    (true, _) => return self.resolved(*right),
                    (_, true) => return self.resolved(*left),
                    _ => {}
                }
                let Some(left) = self.resolved(*left)? else {
                    return Ok(None);
                };
                let Some(right) = self.resolved(*right)? else {
                    return Ok(None);
                };
                match op {
                    word::BinaryOp::BitAnd | word::BinaryOp::LogicalAnd => {
                        Some(self.network.and(left, right))
                    }
                    word::BinaryOp::BitOr | word::BinaryOp::LogicalOr => {
                        Some(self.network.or(left, right))
                    }
                    word::BinaryOp::BitXor | word::BinaryOp::Ne => {
                        Some(self.network.xor(left, right))
                    }
                    word::BinaryOp::Eq => Some(self.network.xor(left, right).inverted()),
                    word::BinaryOp::Add
                    | word::BinaryOp::Sub
                    | word::BinaryOp::Mul
                    | word::BinaryOp::Div
                    | word::BinaryOp::Mod
                    | word::BinaryOp::Lt
                    | word::BinaryOp::Le
                    | word::BinaryOp::Gt
                    | word::BinaryOp::Ge
                    | word::BinaryOp::Shl
                    | word::BinaryOp::Shr
                    | word::BinaryOp::Ashr => None,
                }
            }
            word::OpKind::Mux {
                cond,
                then_value,
                else_value,
            } => {
                if self.undefined_scalar(*cond) {
                    return self.resolved(*else_value);
                }
                let Some(cond) = self.resolved(*cond)? else {
                    return Ok(None);
                };
                match (
                    self.undefined_scalar(*then_value),
                    self.undefined_scalar(*else_value),
                ) {
                    (true, _) => return self.resolved(*else_value),
                    (_, true) => return self.resolved(*then_value),
                    _ => {}
                }
                let Some(then_value) = self.resolved(*then_value)? else {
                    return Ok(None);
                };
                let Some(else_value) = self.resolved(*else_value)? else {
                    return Ok(None);
                };
                Some(self.network.mux(cond, then_value, else_value))
            }
            word::OpKind::Cast { value, .. } => self.resolved(*value)?,
            word::OpKind::Concat { .. }
            | word::OpKind::Extract { .. }
            | word::OpKind::DynamicExtract { .. }
            | word::OpKind::DynamicInsert { .. }
            | word::OpKind::Register(_)
            | word::OpKind::Latch(_) => None,
        })
    }

    fn undefined_scalar(&self, value: word::ValueId) -> bool {
        let Some(stored) = self.module.value(value) else {
            return false;
        };
        if stored.ty.width() != 1 {
            return false;
        }
        let word::ValueKind::Constant(bits) = &stored.kind else {
            return false;
        };
        matches!(bits.as_slice(), [opto_ir::BitVal::X | opto_ir::BitVal::Z])
    }

    fn resolved(&self, value: word::ValueId) -> Result<Option<LogicNodeId>, crate::SynthError> {
        self.nodes.get(&value).copied().ok_or_else(|| {
            crate::SynthError::invariant(format!("unresolved logic dependency {value:?}"))
        })
    }

    fn record_node_value(&mut self, node: LogicNodeId, value: word::ValueId) {
        if self.node_values.len() <= node.index() {
            self.node_values.resize(node.index() + 1, [None; 2]);
        }
        let slot = &mut self.node_values[node.index()][usize::from(node.is_inverted())];
        if slot.is_none() {
            *slot = Some(value);
        }
    }
}

fn node_value(values: &[[Option<word::ValueId>; 2]], node: LogicNodeId) -> Option<word::ValueId> {
    values
        .get(node.index())
        .and_then(|phases| phases[usize::from(node.is_inverted())])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scalar_signal_aliases_share_one_hard_boundary_node() {
        let mut module = word::WordModule::new("boundary_alias");
        let ty = word::WordType::bits(1).unwrap();
        let signal = module
            .add_wire("state", ty, word::SourceSpan::default())
            .unwrap();
        let first = module
            .read_signal(signal, word::SourceSpan::default())
            .unwrap();
        let second = module
            .read_signal(signal, word::SourceSpan::default())
            .unwrap();
        let root = module
            .binary(
                word::BinaryOp::BitAnd,
                first,
                second,
                word::SourceSpan::default(),
            )
            .unwrap();

        let graph = RegionLogicGraph::new_cached(
            &module,
            &[root],
            &[None],
            RegionLogicOptions {
                optimize: false,
                config: crate::SynthesisConfig::default(),
                runtime: crate::test_runtime(),
                incremental: None,
                boundary_inputs: &[first, second],
            },
        )
        .unwrap();

        assert_eq!(graph.inputs().len(), 1);
        assert_eq!(graph.node(first), graph.node(second));
    }

    #[test]
    fn rejects_word_extract_semantics_at_the_boolean_mapper_boundary() {
        let mut module = word::WordModule::new("uncanonical_boundary");
        let input = module
            .add_port(
                "input",
                word::PortDirection::Input,
                word::WordType::bits(2).unwrap(),
                word::SourceSpan::default(),
            )
            .unwrap();
        let input = module
            .read_signal(
                module.port(input).unwrap().signal,
                word::SourceSpan::default(),
            )
            .unwrap();
        let extract = module
            .extract(input, 0, 1, word::SourceSpan::default())
            .unwrap();

        let error = RegionLogicGraph::new_cached(
            &module,
            &[extract],
            &[None],
            RegionLogicOptions {
                optimize: false,
                config: crate::SynthesisConfig::default(),
                runtime: crate::test_runtime(),
                incremental: None,
                boundary_inputs: &[extract],
            },
        )
        .err()
        .expect("Word extract must not be accepted as a Boolean terminal");

        assert!(error.to_string().contains("non-canonical Word operation"));
        assert!(error.to_string().contains("Extract"));
    }
}
