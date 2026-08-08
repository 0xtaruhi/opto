// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! RTL procedure normalization and the validated Word-IR boundary.
//!
//! Nothing outside this domain may call the procedural lowering internals.
//! Consumers enter through [`lower_to_validated_word`] and receive canonical,
//! structurally validated Word IR.

mod cfg;
mod emission;
mod events;
mod helpers;
mod normalizer;
mod predicate;
mod resolved_net;
mod rewrite;
mod state;
mod validation;

use cfg::ProcedureCfg;
use helpers::{
    SignalResolutionContext, block_effects, constant_value, extract_assignment,
    inferred_reset_kind, memory_write_data, normalized_enable, predicate_enable, resolve_signal,
    target_layout, whole_target_name,
};
use opto_ir::{BitVal, ConstBits, proc, rtl::RtlModule, word};
use predicate::{MaterializedPredicate, Predicate, PredicateArena};
use resolved_net::lower_resolved_nets;
use rewrite::{RewriteScratch, rewrite_value};
use state::{Assignment, Coverage, FrameId, ResetList, Slot, StateArena, TargetKey};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::num::NonZeroU32;
pub(crate) use validation::lower_to_validated_word;

fn derived_source(
    parent: &word::SourceSpan,
    construct: &'static str,
    role: impl AsRef<[u8]>,
) -> Result<word::SourceSpan, crate::SynthError> {
    parent.derived(construct, role).ok_or_else(|| {
        crate::SynthError::invariant(format!(
            "cannot derive generated '{construct}' identity from an unanchored source construct"
        ))
    })
}

#[cfg(test)]
mod tests;

/// Lowers immutable procedural CFGs into structural Word IR.
///
/// This is an internal frontend phase, not the validated frontend boundary.
/// Callers outside the frontend must use `frontend::lower_to_validated_word`.
fn lower_procedures(
    rtl: RtlModule,
    runtime: &opto_runtime::ExecutionContext,
    observer: &mut dyn FnMut(crate::SynthesisProgress),
) -> Result<word::WordModule, crate::SynthError> {
    rtl.validate()
        .map_err(|error| crate::SynthError::invalid(error.to_string()))?;
    let (mut module, procedures) = rtl.into_parts();
    let reads = module
        .memory_read_ports()
        .iter()
        .enumerate()
        .map(|(index, port)| (port.data, index))
        .collect::<BTreeMap<_, _>>();
    let mut outputs = vec![None; procedures.blocks().len()];
    let mut edge_guards = vec![None; procedures.edges().len()];
    let mut rewrite_scratch = RewriteScratch::default();
    let mut incomplete_comb = Vec::new();
    let cfgs = run_frontend_stage(observer, crate::StageId::NORMALIZATION_CFG_ANALYSIS, || {
        let cfg_tasks = (0..procedures.procedures().len())
            .map(|index| {
                let procedure = proc::ProcedureId::from_index(index)
                    .map_err(|error| crate::SynthError::capacity(error.to_string()))?;
                let ordinal = u64::try_from(index).map_err(|_| {
                    crate::SynthError::capacity("procedure count exceeds 64-bit task-key capacity")
                })?;
                Ok(opto_runtime::Task::new(
                    opto_runtime::TaskKey::new(0, ordinal),
                    procedure,
                ))
            })
            .collect::<Result<Vec<_>, crate::SynthError>>()?;
        runtime.map_ordered(cfg_tasks, |procedure| {
            ProcedureCfg::canonicalize(&module, &procedures, procedure)
        })
    })?;
    run_frontend_stage(
        observer,
        crate::StageId::NORMALIZATION_PROCEDURE_COMMIT,
        || {
            for (index, cfg) in cfgs.into_iter().enumerate() {
                let procedure = proc::ProcedureId::from_index(index)
                    .map_err(|error| crate::SynthError::capacity(error.to_string()))?;
                ProcedureNormalizer::new(
                    procedure,
                    cfg,
                    ProcedureInput {
                        module: &mut module,
                        procedures: &procedures,
                        reads: &reads,
                        outputs: &mut outputs,
                        edge_guards: &mut edge_guards,
                        rewrite_scratch: &mut rewrite_scratch,
                        incomplete_comb: &mut incomplete_comb,
                    },
                )?
                .run()?;
            }
            Ok(())
        },
    )?;
    let observability = crate::word::uses::netlist_observability(&module)?;
    for assignment in &incomplete_comb {
        if observability.observes_signal(assignment.target.signal)? {
            return Err(crate::SynthError::invalid(format!(
                "always_comb target '{}' is not assigned on every observable control-flow path",
                assignment.target_name(&module)
            )));
        }
    }
    module
        .remove_process_locals()
        .map_err(crate::SynthError::from)?;
    Ok(module)
}

fn run_frontend_stage<T>(
    observer: &mut dyn FnMut(crate::SynthesisProgress),
    stage: crate::StageId,
    operation: impl FnOnce() -> Result<T, crate::SynthError>,
) -> Result<T, crate::SynthError> {
    observer(crate::SynthesisProgress::started(stage));
    match operation() {
        Ok(output) => {
            observer(crate::SynthesisProgress::completed(stage));
            Ok(output)
        }
        Err(error) => {
            observer(crate::SynthesisProgress::failed(stage));
            Err(error)
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct ExecutionState {
    visible: FrameId,
    scheduled: FrameId,
}

#[derive(Debug, Clone, Copy)]
struct BlockOutput {
    state: ExecutionState,
    guard: Predicate,
}

#[derive(Debug, Clone)]
struct PendingWrite {
    memory: word::MemoryId,
    address: word::ValueId,
    data: word::ValueId,
    mask: Option<word::MemoryWriteMask>,
    guard: MaterializedPredicate,
    blocking: bool,
    source: word::SourceSpan,
}

#[derive(Debug, Clone, Copy)]
struct EventControl {
    event: proc::SensitivityEvent,
    value: word::ValueId,
    asserted: Predicate,
}

#[derive(Debug, Clone)]
struct DecisionChoice {
    edges: smallvec::SmallVec<[proc::EdgeId; 2]>,
    predicate: Predicate,
}

struct ProcedureNormalizer<'a> {
    module: &'a mut word::WordModule,
    procedures: &'a proc::ProcModule,
    procedure_id: proc::ProcedureId,
    procedure: &'a proc::Procedure,
    cfg: ProcedureCfg,
    layout: BTreeMap<word::SignalId, Vec<TargetKey>>,
    keys: Vec<TargetKey>,
    bases: BTreeMap<TargetKey, word::ValueId>,
    reads: &'a BTreeMap<word::SignalId, usize>,
    rewrite_scratch: &'a mut RewriteScratch,
    predicates: PredicateArena,
    event_controls: Vec<EventControl>,
    decision_choices: BTreeMap<proc::BlockId, Vec<DecisionChoice>>,
    states: StateArena,
    outputs: &'a mut [Option<BlockOutput>],
    edge_guards: &'a mut [Option<Predicate>],
    writes: Vec<PendingWrite>,
    incomplete_comb: &'a mut Vec<Assignment>,
}

struct ProcedureInput<'a> {
    module: &'a mut word::WordModule,
    procedures: &'a proc::ProcModule,
    reads: &'a BTreeMap<word::SignalId, usize>,
    outputs: &'a mut [Option<BlockOutput>],
    edge_guards: &'a mut [Option<Predicate>],
    rewrite_scratch: &'a mut RewriteScratch,
    incomplete_comb: &'a mut Vec<Assignment>,
}
