// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Procedural-local publication for joint source-order normalization.

use super::{ProcExprKind, TransientProcModule, TransientTarget};
use crate::proc::{AssignmentMode, ProcError};
use crate::word::WordModule;

impl TransientProcModule {
    /// Materializes activation locals as typed process-normalization signals.
    ///
    /// Loop analysis needs first-class [`crate::proc::ProcLocalId`] identities,
    /// but their values cannot be substituted before blocking assignments to
    /// module signals are normalized: doing so would move a local read from
    /// its source-order capture point to a later use. `SignalKind::ProcessLocal`
    /// is the existing joint-normalization representation for exactly this
    /// interval. The synthesis frontend removes every such signal at its
    /// procedure-normalization phase boundary.
    ///
    /// # Errors
    ///
    /// Returns [`ProcError`] for residual loops, nonblocking automatic-local
    /// assignments, duplicate materialized names, or invalid Word IR.
    pub fn materialize_locals(mut self, module: &mut WordModule) -> Result<Self, ProcError> {
        self.validate()?;
        self.validate_acyclic()?;
        if !self.loop_regions.is_empty() {
            return Err(ProcError::new(
                "procedural locals can be materialized only after all loops",
            ));
        }
        if self.locals.is_empty() {
            return Ok(self);
        }

        let mut signals = Vec::with_capacity(self.locals.len());
        let mut values = Vec::with_capacity(self.locals.len());
        for local in &self.locals {
            let signal = module
                .add_process_local_signal(&local.name, local.ty, local.source.clone())
                .map_err(|error| ProcError::new(error.to_string()))?;
            let value = module
                .read_signal(signal, local.source.clone())
                .map_err(|error| ProcError::new(error.to_string()))?;
            signals.push(signal);
            values.push(value);
        }

        for expression in &mut self.expressions {
            if let ProcExprKind::LocalRead(local) = expression.kind {
                expression.kind = ProcExprKind::ModuleValue(values[local.index()]);
            }
        }
        for effect in &mut self.effects {
            let TransientTarget::Local { local, select } = effect.target else {
                continue;
            };
            if effect.mode != AssignmentMode::Blocking {
                return Err(ProcError::new(
                    "nonblocking automatic-local assignment cannot be materialized",
                ));
            }
            effect.target = TransientTarget::Signal {
                signal: signals[local.index()],
                select,
            };
        }
        self.locals = Box::new([]);
        self.validate()?;
        Ok(self)
    }
}
