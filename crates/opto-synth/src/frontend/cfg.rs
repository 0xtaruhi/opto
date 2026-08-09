// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Canonical, read-only view of one sealed procedural CFG.
//!
//! The frontend IR remains an immutable source record.  This view removes
//! control-only noise before target-specific state propagation without
//! rewriting effects or manufacturing Word IR.

use opto_ir::{BitVal, proc, word};
use std::collections::{BTreeSet, HashMap};

#[derive(Debug, Clone, Copy)]
struct LocalBlocks {
    start: usize,
    len: usize,
}

impl LocalBlocks {
    fn new(blocks: &[proc::BlockId]) -> Result<Self, crate::SynthError> {
        let Some(first) = blocks.first() else {
            return Err(crate::SynthError::invariant(
                "sealed procedure has no blocks",
            ));
        };
        let local = Self {
            start: first.index(),
            len: blocks.len(),
        };
        if blocks
            .iter()
            .enumerate()
            .any(|(index, block)| local.start + index != block.index())
        {
            return Err(crate::SynthError::invariant(
                "sealed procedure blocks are not contiguous",
            ));
        }
        Ok(local)
    }

    fn index(self, block: proc::BlockId) -> usize {
        let index = block
            .index()
            .checked_sub(self.start)
            .expect("canonical CFG block precedes its procedure range");
        assert!(
            index < self.len,
            "canonical CFG block exceeds its procedure range"
        );
        index
    }

    fn block(self, index: usize) -> Result<proc::BlockId, crate::SynthError> {
        if index >= self.len {
            return Err(crate::SynthError::invariant(
                "local CFG index exceeds its procedure range",
            ));
        }
        proc::BlockId::from_index(self.start + index)
            .map_err(|error| crate::SynthError::capacity(error.to_string()))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MergeSite {
    Block(proc::BlockId),
    Exit,
}

#[derive(Debug, Clone, Copy)]
pub(super) enum MergeOrigin {
    Edge(proc::EdgeId),
    Return(proc::BlockId),
}

#[derive(Debug, Clone, Copy)]
pub(super) struct SwitchArm {
    pub(super) pattern: word::ValueId,
    pub(super) edge: proc::EdgeId,
}

#[derive(Debug, Clone)]
pub(super) enum Terminator {
    Return,
    Jump {
        edge: proc::EdgeId,
    },
    Branch {
        condition: word::ValueId,
        then_edge: proc::EdgeId,
        else_edge: proc::EdgeId,
    },
    Switch {
        selector: word::ValueId,
        arms: Box<[SwitchArm]>,
        default: proc::EdgeId,
    },
}

impl Terminator {
    fn try_for_each_edge<E>(
        &self,
        mut visit: impl FnMut(proc::EdgeId) -> Result<(), E>,
    ) -> Result<(), E> {
        match self {
            Self::Return => {}
            Self::Jump { edge } => visit(*edge)?,
            Self::Branch {
                then_edge,
                else_edge,
                ..
            } => {
                visit(*then_edge)?;
                visit(*else_edge)?;
            }
            Self::Switch { arms, default, .. } => {
                for arm in arms {
                    visit(arm.edge)?;
                }
                visit(*default)?;
            }
        }
        Ok(())
    }

    fn is_decision(&self) -> bool {
        matches!(self, Self::Branch { .. } | Self::Switch { .. })
    }
}

#[derive(Debug)]
struct DominatorTree {
    local: LocalBlocks,
    depth: Vec<u32>,
    intervals: Vec<(u32, u32)>,
}

impl DominatorTree {
    fn build(
        local: LocalBlocks,
        entry: proc::BlockId,
        order: &[proc::BlockId],
        predecessors: &opto_core::PackedRows<proc::EdgeId>,
        edge_sources: &HashMap<proc::EdgeId, proc::BlockId>,
    ) -> Result<Self, crate::SynthError> {
        let mut immediate = vec![None; local.len];
        let mut depth = vec![0; local.len];
        immediate[local.index(entry)] = Some(entry);
        for &block in order.iter().skip(1) {
            let mut sources = predecessors
                .row(local.index(block))
                .iter()
                .filter_map(|edge| edge_sources.get(edge).copied());
            let Some(mut common) = sources.next() else {
                return Err(crate::SynthError::invariant(
                    "reachable canonical CFG block has no predecessor",
                ));
            };
            for source in sources {
                common = intersect(local, common, source, &immediate, &depth)?;
            }
            immediate[local.index(block)] = Some(common);
            depth[local.index(block)] =
                depth[local.index(common)].checked_add(1).ok_or_else(|| {
                    crate::SynthError::capacity(
                        "procedural dominator depth exceeds 32-bit capacity",
                    )
                })?;
        }

        let children = opto_core::PackedRows::try_from_entries(
            local.len,
            order
                .iter()
                .copied()
                .skip(1)
                .map(|block| {
                    let parent = immediate[local.index(block)].ok_or_else(|| {
                        crate::SynthError::invariant("canonical CFG dominator chain is incomplete")
                    })?;
                    Ok((local.index(parent), block))
                })
                .collect::<Result<Vec<_>, crate::SynthError>>()?,
        )
        .map_err(|error| crate::SynthError::invariant(error.to_string()))?;
        let mut intervals = vec![(0, 0); local.len];
        let mut timestamp = 1u32;
        let mut pending = vec![(entry, false)];
        while let Some((block, exiting)) = pending.pop() {
            let interval = intervals.get_mut(local.index(block)).ok_or_else(|| {
                crate::SynthError::invariant("canonical CFG dominator is out of range")
            })?;
            if exiting {
                interval.1 = timestamp;
            } else {
                interval.0 = timestamp;
                pending.push((block, true));
                pending.extend(
                    children
                        .row(local.index(block))
                        .iter()
                        .rev()
                        .copied()
                        .map(|child| (child, false)),
                );
            }
            timestamp = timestamp.checked_add(1).ok_or_else(|| {
                crate::SynthError::capacity(
                    "procedural dominator traversal exceeds 32-bit capacity",
                )
            })?;
        }
        Ok(Self {
            local,
            depth,
            intervals,
        })
    }

    fn depth(&self, block: proc::BlockId) -> u32 {
        self.depth[self.local.index(block)]
    }

    fn dominates(
        &self,
        dominator: proc::BlockId,
        block: proc::BlockId,
    ) -> Result<bool, crate::SynthError> {
        let &(dominator_entry, dominator_exit) = self
            .intervals
            .get(self.local.index(dominator))
            .ok_or_else(|| {
                crate::SynthError::invariant("canonical CFG dominator is out of range")
            })?;
        let &(block_entry, block_exit) =
            self.intervals.get(self.local.index(block)).ok_or_else(|| {
                crate::SynthError::invariant("canonical CFG dominated block is out of range")
            })?;
        if dominator_entry == 0 || dominator_exit == 0 || block_entry == 0 || block_exit == 0 {
            return Err(crate::SynthError::invariant(
                "canonical CFG dominator interval is incomplete",
            ));
        }
        Ok(dominator_entry <= block_entry && block_exit <= dominator_exit)
    }
}

#[derive(Debug)]
pub(super) struct ProcedureCfg {
    local: LocalBlocks,
    entry: proc::BlockId,
    blocks: Box<[proc::BlockId]>,
    order: Box<[proc::BlockId]>,
    returns: Box<[proc::BlockId]>,
    terminators: Vec<Option<Terminator>>,
    edge_sources: HashMap<proc::EdgeId, proc::BlockId>,
    edge_targets: HashMap<proc::EdgeId, proc::BlockId>,
    predecessors: opto_core::PackedRows<proc::EdgeId>,
    dominators: DominatorTree,
    block_decisions: opto_core::PackedRows<proc::BlockId>,
    exit_decisions: Box<[proc::BlockId]>,
}

impl ProcedureCfg {
    pub(super) fn canonicalize(
        module: &word::WordModule,
        procedures: &proc::ProcModule,
        procedure_id: proc::ProcedureId,
    ) -> Result<Self, crate::SynthError> {
        let procedure = procedures
            .procedure(procedure_id)
            .ok_or_else(|| crate::SynthError::invariant("procedural definition disappeared"))?;
        let owned = procedures
            .procedure_blocks(procedure_id)
            .ok_or_else(|| crate::SynthError::invariant("procedural block range disappeared"))?
            .collect::<Vec<_>>();
        let local = LocalBlocks::new(&owned)?;
        let mut canonicalizer = Canonicalizer {
            module,
            procedures,
            procedure,
            owned: &owned,
            local,
            threaded: vec![None; local.len],
            visiting: vec![false; local.len],
            terminators: (0..local.len).map(|_| None).collect(),
            edge_targets: HashMap::new(),
        };
        let entry = canonicalizer.thread_target(procedure.entry)?;
        let mut reached = vec![false; local.len];
        let mut pending = vec![entry];
        while let Some(block) = pending.pop() {
            if std::mem::replace(&mut reached[local.index(block)], true) {
                continue;
            }
            let terminator = canonicalizer.canonical_terminator(block)?;
            terminator.try_for_each_edge(|edge| {
                let target = canonical_edge_target(&canonicalizer.edge_targets, edge)?;
                pending.push(target);
                Ok::<(), crate::SynthError>(())
            })?;
            canonicalizer.terminators[local.index(block)] = Some(terminator);
        }

        let blocks = owned
            .iter()
            .copied()
            .filter(|block| reached[local.index(*block)])
            .collect::<Vec<_>>();
        let mut edge_entries = Vec::new();
        for &block in &blocks {
            let terminator = canonical_terminator_at(local, &canonicalizer.terminators, block)?;
            terminator.try_for_each_edge(|edge| {
                edge_entries.push((
                    local.index(canonical_edge_target(&canonicalizer.edge_targets, edge)?),
                    edge,
                ));
                Ok::<(), crate::SynthError>(())
            })?;
        }
        let predecessors = opto_core::PackedRows::try_from_entries(local.len, edge_entries)
            .map_err(|error| crate::SynthError::invariant(error.to_string()))?;
        let order = topological_order(
            local,
            procedure,
            &blocks,
            entry,
            &canonicalizer.terminators,
            &canonicalizer.edge_targets,
            &predecessors,
        )?;
        let mut returns = Vec::new();
        for &block in &order {
            if matches!(
                canonical_terminator_at(local, &canonicalizer.terminators, block)?,
                Terminator::Return
            ) {
                returns.push(block);
            }
        }
        let edge_sources =
            live_edge_sources(local, procedures, &blocks, &canonicalizer.terminators)?;
        let dominators = DominatorTree::build(local, entry, &order, &predecessors, &edge_sources)?;
        let postdominators = immediate_postdominators(
            local,
            &order,
            &canonicalizer.terminators,
            &canonicalizer.edge_targets,
        )?;
        let mut block_decision_entries = Vec::new();
        let mut exit_decisions = Vec::new();
        for &block in &order {
            let terminator = canonical_terminator_at(local, &canonicalizer.terminators, block)?;
            if !terminator.is_decision() {
                continue;
            }
            match postdominators[local.index(block)] {
                Some(PostDominator::Block(join)) => {
                    block_decision_entries.push((local.index(join), block));
                }
                Some(PostDominator::Exit) => exit_decisions.push(block),
                None => {
                    return Err(crate::SynthError::invariant(
                        "canonical decision has no immediate post-dominator",
                    ));
                }
            }
        }
        let decision_order = |left: &proc::BlockId, right: &proc::BlockId| {
            dominators
                .depth(*left)
                .cmp(&dominators.depth(*right))
                .then_with(|| left.cmp(right))
        };
        block_decision_entries.sort_by(|(_, left), (_, right)| decision_order(left, right));
        exit_decisions.sort_by(decision_order);
        let block_decisions =
            opto_core::PackedRows::try_from_entries(local.len, block_decision_entries)
                .map_err(|error| crate::SynthError::invariant(error.to_string()))?;

        Ok(Self {
            local,
            entry,
            blocks: blocks.into_boxed_slice(),
            order: order.into_boxed_slice(),
            returns: returns.into_boxed_slice(),
            terminators: canonicalizer.terminators,
            edge_sources,
            edge_targets: canonicalizer.edge_targets,
            predecessors,
            dominators,
            block_decisions,
            exit_decisions: exit_decisions.into_boxed_slice(),
        })
    }

    pub(super) fn entry(&self) -> proc::BlockId {
        self.entry
    }

    pub(super) fn blocks(&self) -> &[proc::BlockId] {
        &self.blocks
    }

    pub(super) fn order(&self) -> &[proc::BlockId] {
        &self.order
    }

    pub(super) fn returns(&self) -> &[proc::BlockId] {
        &self.returns
    }

    pub(super) fn terminator(
        &self,
        block: proc::BlockId,
    ) -> Result<&Terminator, crate::SynthError> {
        self.terminators
            .get(self.local.index(block))
            .and_then(Option::as_ref)
            .ok_or_else(|| crate::SynthError::invariant("block is outside the canonical CFG"))
    }

    pub(super) fn predecessors(&self, block: proc::BlockId) -> &[proc::EdgeId] {
        self.predecessors.row(self.local.index(block))
    }

    pub(super) fn edge_source(
        &self,
        edge: proc::EdgeId,
    ) -> Result<proc::BlockId, crate::SynthError> {
        self.edge_sources
            .get(&edge)
            .copied()
            .ok_or_else(|| crate::SynthError::invariant("edge is outside the canonical CFG"))
    }

    pub(super) fn decisions(&self, site: MergeSite) -> &[proc::BlockId] {
        match site {
            MergeSite::Block(block) => self.block_decisions.row(self.local.index(block)),
            MergeSite::Exit => &self.exit_decisions,
        }
    }

    pub(super) fn edge_target(
        &self,
        edge: proc::EdgeId,
    ) -> Result<proc::BlockId, crate::SynthError> {
        self.edge_targets
            .get(&edge)
            .copied()
            .ok_or_else(|| crate::SynthError::invariant("edge is outside the canonical CFG"))
    }

    pub(super) fn choice_contains(
        &self,
        choice: &[proc::EdgeId],
        site: MergeSite,
        origin: MergeOrigin,
    ) -> Result<bool, crate::SynthError> {
        let (&representative, remaining) = choice.split_first().ok_or_else(|| {
            crate::SynthError::invariant("canonical control choice has no CFG edge")
        })?;
        let choice_target = self.edge_target(representative)?;
        for &edge in remaining {
            if self.edge_target(edge)? != choice_target {
                return Err(crate::SynthError::invariant(
                    "canonical control choice spans different successors",
                ));
            }
        }
        match (site, origin) {
            (MergeSite::Block(join), MergeOrigin::Edge(incoming)) if choice_target == join => {
                Ok(choice.contains(&incoming))
            }
            (MergeSite::Block(_), MergeOrigin::Edge(incoming)) => {
                let source = self.edge_source(incoming)?;
                self.dominators.dominates(choice_target, source)
            }
            (MergeSite::Exit, MergeOrigin::Return(block)) => {
                self.dominators.dominates(choice_target, block)
            }
            (MergeSite::Block(_), MergeOrigin::Return(_))
            | (MergeSite::Exit, MergeOrigin::Edge(_)) => Err(crate::SynthError::invariant(
                "procedural merge origin does not match its merge site",
            )),
        }
    }
}

struct Canonicalizer<'a> {
    module: &'a word::WordModule,
    procedures: &'a proc::ProcModule,
    procedure: &'a proc::Procedure,
    owned: &'a [proc::BlockId],
    local: LocalBlocks,
    threaded: Vec<Option<proc::BlockId>>,
    visiting: Vec<bool>,
    terminators: Vec<Option<Terminator>>,
    edge_targets: HashMap<proc::EdgeId, proc::BlockId>,
}

impl Canonicalizer<'_> {
    fn thread_target(&mut self, start: proc::BlockId) -> Result<proc::BlockId, crate::SynthError> {
        let mut path = Vec::new();
        let mut block = start;
        let target = loop {
            let index = self.local.index(block);
            let cached = self.threaded.get(index).copied().ok_or_else(|| {
                crate::SynthError::invariant("procedural jump targets an out-of-range block")
            })?;
            if let Some(target) = cached {
                break target;
            }
            let visiting = self.visiting.get_mut(index).ok_or_else(|| {
                crate::SynthError::invariant("procedural jump targets an out-of-range block")
            })?;
            if std::mem::replace(visiting, true) {
                return Err(crate::SynthError::unsupported(format!(
                    "procedure at {:?} contains a control-flow cycle",
                    self.procedure.source
                )));
            }
            path.push(block);
            let stored = self.procedures.block(block).ok_or_else(|| {
                crate::SynthError::invariant("procedural jump targets an unknown block")
            })?;
            if stored.effect_count() != 0 {
                break block;
            }
            let proc::TerminatorKind::Jump { edge } = stored.terminator.kind else {
                break block;
            };
            block = self.original_edge_target(edge)?;
        };
        for block in path {
            let index = self.local.index(block);
            *self.visiting.get_mut(index).ok_or_else(|| {
                crate::SynthError::invariant("threaded CFG block is out of range")
            })? = false;
            *self.threaded.get_mut(index).ok_or_else(|| {
                crate::SynthError::invariant("threaded CFG block is out of range")
            })? = Some(target);
        }
        Ok(target)
    }

    fn canonical_terminator(
        &mut self,
        block: proc::BlockId,
    ) -> Result<Terminator, crate::SynthError> {
        let stored = self
            .procedures
            .block(block)
            .ok_or_else(|| crate::SynthError::invariant("canonical CFG block disappeared"))?;
        match stored.terminator.kind {
            proc::TerminatorKind::Return => Ok(Terminator::Return),
            proc::TerminatorKind::Jump { edge } => {
                self.bind_edge(edge)?;
                Ok(Terminator::Jump { edge })
            }
            proc::TerminatorKind::Branch {
                condition,
                then_edge,
                else_edge,
            } => {
                if let Some(value) = boolean_constant(self.module, condition) {
                    let edge = if value { then_edge } else { else_edge };
                    self.bind_edge(edge)?;
                    return Ok(Terminator::Jump { edge });
                }
                let then_target = self.bind_edge(then_edge)?;
                let else_target = self.bind_edge(else_edge)?;
                if then_target == else_target {
                    Ok(Terminator::Jump { edge: then_edge })
                } else {
                    Ok(Terminator::Branch {
                        condition,
                        then_edge,
                        else_edge,
                    })
                }
            }
            proc::TerminatorKind::Switch {
                selector, default, ..
            } => {
                let arms = self
                    .procedures
                    .switch_arms(block)
                    .ok_or_else(|| crate::SynthError::invariant("switch arm table disappeared"))?
                    .map(|(_, arm)| SwitchArm {
                        pattern: arm.pattern,
                        edge: arm.edge,
                    })
                    .collect::<Vec<_>>();
                if let Some(selector_bits) = known_constant(self.module, selector) {
                    // A constant selector decides the switch only once every
                    // pattern it has to rule out is constant too. `case (1'b1)`
                    // over variable patterns is the one-hot select idiom: which
                    // arm runs is decided at runtime, and treating an unmatched
                    // constant selector as "take the default" would discard the
                    // whole statement.
                    let mut folded = Some(default);
                    for arm in &arms {
                        let Some(pattern) = known_constant(self.module, arm.pattern) else {
                            folded = None;
                            break;
                        };
                        if pattern == selector_bits {
                            folded = Some(arm.edge);
                            break;
                        }
                    }
                    if let Some(edge) = folded {
                        self.bind_edge(edge)?;
                        return Ok(Terminator::Jump { edge });
                    }
                }
                let mut common_target = Some(self.bind_edge(default)?);
                for arm in &arms {
                    let target = self.bind_edge(arm.edge)?;
                    if common_target.is_some_and(|common| common != target) {
                        common_target = None;
                    }
                }
                if common_target.is_some() {
                    Ok(Terminator::Jump { edge: default })
                } else {
                    Ok(Terminator::Switch {
                        selector,
                        arms: arms.into_boxed_slice(),
                        default,
                    })
                }
            }
        }
    }

    fn bind_edge(&mut self, edge: proc::EdgeId) -> Result<proc::BlockId, crate::SynthError> {
        let target = self.thread_target(self.original_edge_target(edge)?)?;
        if let Some(previous) = self.edge_targets.insert(edge, target)
            && previous != target
        {
            return Err(crate::SynthError::invariant(
                "canonical control-flow edge changed target",
            ));
        }
        Ok(target)
    }

    fn original_edge_target(&self, edge: proc::EdgeId) -> Result<proc::BlockId, crate::SynthError> {
        let record = self.procedures.edge(edge).ok_or_else(|| {
            crate::SynthError::invariant("control-flow edge is not in the procedure")
        })?;
        if self.owned.binary_search(&record.target).is_err() {
            return Err(crate::SynthError::invariant(
                "control-flow edge crosses procedure ownership",
            ));
        }
        Ok(record.target)
    }
}

fn boolean_constant(module: &word::WordModule, value: word::ValueId) -> Option<bool> {
    let bits = known_constant(module, value)?;
    (bits.len() == 1).then(|| bits[0] == BitVal::One)
}

fn known_constant(module: &word::WordModule, value: word::ValueId) -> Option<&[BitVal]> {
    let word::ValueKind::Constant(bits) = &module.value(value)?.kind else {
        return None;
    };
    bits.as_slice()
        .iter()
        .all(|bit| matches!(bit, BitVal::Zero | BitVal::One))
        .then(|| bits.as_slice())
}

fn canonical_terminator_at(
    local: LocalBlocks,
    terminators: &[Option<Terminator>],
    block: proc::BlockId,
) -> Result<&Terminator, crate::SynthError> {
    terminators
        .get(local.index(block))
        .and_then(Option::as_ref)
        .ok_or_else(|| crate::SynthError::invariant("reachable block has no canonical terminator"))
}

fn canonical_edge_target(
    edge_targets: &HashMap<proc::EdgeId, proc::BlockId>,
    edge: proc::EdgeId,
) -> Result<proc::BlockId, crate::SynthError> {
    edge_targets
        .get(&edge)
        .copied()
        .ok_or_else(|| crate::SynthError::invariant("canonical edge has no target"))
}

fn topological_order(
    local: LocalBlocks,
    procedure: &proc::Procedure,
    blocks: &[proc::BlockId],
    entry: proc::BlockId,
    terminators: &[Option<Terminator>],
    edge_targets: &HashMap<proc::EdgeId, proc::BlockId>,
    predecessors: &opto_core::PackedRows<proc::EdgeId>,
) -> Result<Vec<proc::BlockId>, crate::SynthError> {
    let mut indegree = vec![0usize; local.len];
    for &block in blocks {
        indegree[local.index(block)] = predecessors.row(local.index(block)).len();
    }
    let mut ready = blocks
        .iter()
        .copied()
        .filter(|block| indegree[local.index(*block)] == 0)
        .collect::<BTreeSet<_>>();
    if !ready.contains(&entry) {
        return Err(crate::SynthError::unsupported(format!(
            "procedure at {:?} contains a control-flow cycle",
            procedure.source
        )));
    }
    let mut order = Vec::with_capacity(blocks.len());
    while let Some(block) = ready.pop_first() {
        order.push(block);
        canonical_terminator_at(local, terminators, block)?.try_for_each_edge(|edge| {
            let target = canonical_edge_target(edge_targets, edge)?;
            let target_indegree = indegree.get_mut(local.index(target)).ok_or_else(|| {
                crate::SynthError::invariant("canonical edge targets an out-of-range block")
            })?;
            *target_indegree = target_indegree.checked_sub(1).ok_or_else(|| {
                crate::SynthError::invariant("canonical CFG predecessor count is inconsistent")
            })?;
            if *target_indegree == 0 {
                ready.insert(target);
            }
            Ok::<(), crate::SynthError>(())
        })?;
    }
    if order.len() != blocks.len() {
        return Err(crate::SynthError::unsupported(format!(
            "procedure at {:?} contains a control-flow cycle",
            procedure.source
        )));
    }
    Ok(order)
}

fn live_edge_sources(
    local: LocalBlocks,
    procedures: &proc::ProcModule,
    blocks: &[proc::BlockId],
    terminators: &[Option<Terminator>],
) -> Result<HashMap<proc::EdgeId, proc::BlockId>, crate::SynthError> {
    let mut sources = HashMap::new();
    for &block in blocks {
        canonical_terminator_at(local, terminators, block)?.try_for_each_edge(|edge| {
            let record = procedures.edge(edge).ok_or_else(|| {
                crate::SynthError::invariant("canonical CFG references an unknown edge")
            })?;
            if record.from != block {
                return Err(crate::SynthError::invariant(
                    "canonical CFG edge source does not match its terminator",
                ));
            }
            if sources.insert(edge, block).is_some() {
                return Err(crate::SynthError::invariant(
                    "canonical edge appears in more than one terminator",
                ));
            }
            Ok::<(), crate::SynthError>(())
        })?;
    }
    Ok(sources)
}

fn intersect(
    local: LocalBlocks,
    mut left: proc::BlockId,
    mut right: proc::BlockId,
    immediate: &[Option<proc::BlockId>],
    depth: &[u32],
) -> Result<proc::BlockId, crate::SynthError> {
    while left != right {
        if depth[local.index(left)] >= depth[local.index(right)] {
            left = immediate[local.index(left)].ok_or_else(|| {
                crate::SynthError::invariant("canonical CFG dominator chain is incomplete")
            })?;
        } else {
            right = immediate[local.index(right)].ok_or_else(|| {
                crate::SynthError::invariant("canonical CFG dominator chain is incomplete")
            })?;
        }
    }
    Ok(left)
}

#[derive(Debug, Clone, Copy)]
enum PostDominator {
    Block(proc::BlockId),
    Exit,
}

fn immediate_postdominators(
    local: LocalBlocks,
    order: &[proc::BlockId],
    terminators: &[Option<Terminator>],
    edge_targets: &HashMap<proc::EdgeId, proc::BlockId>,
) -> Result<Vec<Option<PostDominator>>, crate::SynthError> {
    let exit = local.len;
    let mut immediate = vec![None; local.len + 1];
    let mut depth = vec![0u32; local.len + 1];
    immediate[exit] = Some(exit);
    for &block in order.iter().rev() {
        let terminator = canonical_terminator_at(local, terminators, block)?;
        let mut successors = Vec::new();
        terminator.try_for_each_edge(|edge| {
            successors.push(local.index(canonical_edge_target(edge_targets, edge)?));
            Ok::<(), crate::SynthError>(())
        })?;
        if successors.is_empty() {
            successors.push(exit);
        }
        let mut common = successors[0];
        for &successor in &successors[1..] {
            common = intersect_index(common, successor, &immediate, &depth)?;
        }
        immediate[local.index(block)] = Some(common);
        depth[local.index(block)] = depth[common].checked_add(1).ok_or_else(|| {
            crate::SynthError::capacity("procedural post-dominator depth exceeds 32-bit capacity")
        })?;
    }
    let mut result = vec![None; local.len];
    for &block in order {
        result[local.index(block)] = Some(match immediate[local.index(block)] {
            Some(index) if index == exit => PostDominator::Exit,
            Some(index) => PostDominator::Block(local.block(index)?),
            None => {
                return Err(crate::SynthError::invariant(
                    "canonical CFG post-dominator chain is incomplete",
                ));
            }
        });
    }
    Ok(result)
}

fn intersect_index(
    mut left: usize,
    mut right: usize,
    immediate: &[Option<usize>],
    depth: &[u32],
) -> Result<usize, crate::SynthError> {
    while left != right {
        if depth[left] >= depth[right] {
            left = immediate[left].ok_or_else(|| {
                crate::SynthError::invariant("canonical CFG post-dominator chain is incomplete")
            })?;
        } else {
            right = immediate[right].ok_or_else(|| {
                crate::SynthError::invariant("canonical CFG post-dominator chain is incomplete")
            })?;
        }
    }
    Ok(left)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(index: usize) -> proc::BlockId {
        proc::BlockId::from_index(index).expect("test block index fits")
    }

    fn edge(index: usize) -> proc::EdgeId {
        proc::EdgeId::from_index(index).expect("test edge index fits")
    }

    #[test]
    fn dominator_intervals_cover_nested_and_diamond_paths() {
        let blocks = (0..5).map(block).collect::<Vec<_>>();
        let predecessors = opto_core::PackedRows::try_from_entries(
            blocks.len(),
            [
                (blocks[1].index(), edge(0)),
                (blocks[2].index(), edge(1)),
                (blocks[3].index(), edge(2)),
                (blocks[3].index(), edge(3)),
                (blocks[4].index(), edge(4)),
            ],
        )
        .expect("test predecessor rows are valid");
        let sources = HashMap::from([
            (edge(0), blocks[0]),
            (edge(1), blocks[0]),
            (edge(2), blocks[1]),
            (edge(3), blocks[2]),
            (edge(4), blocks[3]),
        ]);
        let local = LocalBlocks::new(&blocks).expect("test block range is valid");

        let dominators = DominatorTree::build(local, blocks[0], &blocks, &predecessors, &sources)
            .expect("test dominator tree builds");

        for &block in &blocks {
            assert!(dominators.dominates(blocks[0], block).unwrap());
            assert!(dominators.dominates(block, block).unwrap());
        }
        assert!(!dominators.dominates(blocks[1], blocks[2]).unwrap());
        assert!(!dominators.dominates(blocks[1], blocks[3]).unwrap());
        assert!(dominators.dominates(blocks[3], blocks[4]).unwrap());
        assert!(!dominators.dominates(blocks[4], blocks[3]).unwrap());
    }
}
