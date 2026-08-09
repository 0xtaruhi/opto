// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::{
    BitVal, ConstBits, LogicStateKind, MemoryReadTiming, Value, ValueId, ValueKind, WordError,
    WordModule,
};

impl WordModule {
    /// Replaces an operation result with an equivalent constant while keeping
    /// its value ID stable. The now-unreferenced operation is removed by the
    /// next netlist compaction.
    ///
    /// # Errors
    ///
    /// Returns [`WordError`] if `value` is unknown or not an operation result,
    /// the width differs, or a two-state result is replaced with `X`/`Z` bits.
    pub fn replace_operation_result_with_constant(
        &mut self,
        value: ValueId,
        bits: ConstBits,
    ) -> Result<(), WordError> {
        let stored = self
            .values
            .get_mut(value.index())
            .ok_or_else(|| WordError::new(format!("unknown RTL value {value:?}")))?;
        if !matches!(stored.kind, ValueKind::Operation(_)) {
            return Err(WordError::new(format!(
                "value {value:?} is not an operation result"
            )));
        }
        validate_constant_replacement(stored, &bits)?;
        stored.kind = ValueKind::Constant(bits);
        Ok(())
    }

    /// Rewrites every structural use through a complete, type-preserving value
    /// replacement table.
    ///
    /// The table is validated in full before mutation. An operation input is
    /// rewritten only when its replacement remains earlier than the operation
    /// result; structural sinks may reference any value. This preserves the
    /// topological Word IR invariant even when a signal alias resolves to a
    /// driver emitted later in source order. Values and producing operations
    /// remain allocated until the caller explicitly compacts the module.
    ///
    /// # Errors
    ///
    /// Returns [`WordError`] before mutation if the table is incomplete, an ID
    /// is unknown, a replacement changes type, or an operation would gain a
    /// non-topological input.
    pub fn rewrite_value_uses(&mut self, replacements: &[ValueId]) -> Result<(), WordError> {
        if replacements.len() != self.values.len() {
            return Err(WordError::new(format!(
                "value replacement table has {} entries for {} values",
                replacements.len(),
                self.values.len()
            )));
        }
        for (index, &replacement) in replacements.iter().enumerate() {
            let original = ValueId::from_index(index)?;
            let original_ty = self.value_ty(original)?;
            let replacement_ty = self.value_ty(replacement)?;
            if original_ty != replacement_ty {
                return Err(WordError::new(format!(
                    "value replacement {original:?} -> {replacement:?} changes type"
                )));
            }
        }
        let replace = |value: &mut ValueId| {
            *value = replacements[value.index()];
        };
        for operation in &mut self.operations {
            operation.kind.for_each_input_mut(|value| {
                let replacement = replacements[value.index()];
                if replacement.index() < operation.result.index() {
                    *value = replacement;
                }
            });
        }
        for connect in &mut self.connects {
            replace(&mut connect.value);
            if let Some(dynamic) = &mut connect.target.dynamic {
                replace(&mut dynamic.offset);
            }
        }
        for connection in self
            .instances
            .iter_mut()
            .flat_map(|instance| &mut instance.connections)
        {
            replace(&mut connection.value);
        }
        for port in &mut self.memory_read_ports {
            replace(&mut port.address);
            if let MemoryReadTiming::Synchronous { clock, enable, .. } = &mut port.timing {
                replace(&mut clock.value);
                if let Some(enable) = enable {
                    replace(&mut enable.value);
                }
            }
        }
        for port in &mut self.memory_write_ports {
            replace(&mut port.address);
            replace(&mut port.data);
            replace(&mut port.clock.value);
            if let Some(enable) = &mut port.enable {
                replace(&mut enable.value);
            }
            if let Some(mask) = &mut port.mask {
                replace(&mut mask.value);
            }
        }
        Ok(())
    }
}

fn validate_constant_replacement(stored: &Value, bits: &ConstBits) -> Result<(), WordError> {
    if bits.width() != stored.ty.width() {
        return Err(WordError::new(format!(
            "constant width {} does not match value width {}",
            bits.width(),
            stored.ty.width()
        )));
    }
    if stored.ty.state() == LogicStateKind::TwoState
        && bits
            .as_slice()
            .iter()
            .any(|bit| matches!(bit, BitVal::X | BitVal::Z))
    {
        return Err(WordError::new(
            "two-state constant cannot contain x or z bits",
        ));
    }
    Ok(())
}
