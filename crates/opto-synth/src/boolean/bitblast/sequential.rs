// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::{BitBackend, BitBlaster, ScalarBit, word};

impl<B: BitBackend> BitBlaster<'_, B> {
    pub(super) fn register_bits(
        &mut self,
        register: &word::RegisterOp,
        source: &word::SourceSpan,
    ) -> Result<Vec<ScalarBit>, crate::SynthError> {
        let data = self.value(register.d)?;
        let clock = self.scalar_value(register.clock)?;
        let clock = self.backend.word_value(clock).expect("Word backend bit");
        let enable = if let Some(enable) = register.enable {
            let value = self.scalar_value(enable.value)?;
            Some(word::Enable {
                value: self.backend.word_value(value).expect("Word backend bit"),
                active_high: enable.active_high,
            })
        } else {
            None
        };
        let reset_controls = register
            .resets
            .iter()
            .map(|reset| {
                let bit = self.scalar_value(reset.value)?;
                self.backend.word_value(bit).ok_or_else(|| {
                    crate::SynthError::invariant("AXM sequential control has no Word shell binding")
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let reset_values = register
            .resets
            .iter()
            .map(|reset| self.value(reset.reset_value))
            .collect::<Result<Vec<_>, _>>()?;
        let mut bits = Vec::with_capacity(data.len() as usize);
        for index in 0..data.len() {
            let resets = self.scalar_resets(
                &register.resets,
                &reset_controls,
                &reset_values,
                index,
                source,
            )?;
            let value = self
                .module
                .register(
                    word::RegisterOp {
                        name: register.name,
                        d: self
                            .backend
                            .word_value(self.bit(data, index))
                            .expect("Word backend bit"),
                        clock,
                        edge: register.edge,
                        enable,
                        resets,
                    },
                    source.clone(),
                )
                .map_err(crate::SynthError::from)?;
            bits.push(self.backend.import_word(self.module, value));
        }
        Ok(bits)
    }

    pub(super) fn latch_bits(
        &mut self,
        latch: &word::LatchOp,
        source: &word::SourceSpan,
    ) -> Result<Vec<ScalarBit>, crate::SynthError> {
        let data = self.value(latch.d)?;
        let enable_value = self.scalar_value(latch.enable.value)?;
        let enable = word::Enable {
            value: self
                .backend
                .word_value(enable_value)
                .expect("Word backend bit"),
            active_high: latch.enable.active_high,
        };
        let reset_controls = latch
            .resets
            .iter()
            .map(|reset| {
                let bit = self.scalar_value(reset.value)?;
                self.backend.word_value(bit).ok_or_else(|| {
                    crate::SynthError::invariant("AXM sequential control has no Word shell binding")
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let reset_values = latch
            .resets
            .iter()
            .map(|reset| self.value(reset.reset_value))
            .collect::<Result<Vec<_>, _>>()?;
        let mut bits = Vec::with_capacity(data.len() as usize);
        for index in 0..data.len() {
            let resets =
                self.scalar_resets(&latch.resets, &reset_controls, &reset_values, index, source)?;
            let value = self
                .module
                .latch(
                    word::LatchOp {
                        name: latch.name,
                        d: self
                            .backend
                            .word_value(self.bit(data, index))
                            .expect("Word backend bit"),
                        enable,
                        resets,
                    },
                    source.clone(),
                )
                .map_err(crate::SynthError::from)?;
            bits.push(self.backend.import_word(self.module, value));
        }
        Ok(bits)
    }

    fn scalar_resets(
        &mut self,
        resets: &[word::Reset],
        controls: &[word::ValueId],
        values: &[super::BitSpan],
        index: u32,
        source: &word::SourceSpan,
    ) -> Result<Vec<word::Reset>, crate::SynthError> {
        if resets.len() != controls.len() || resets.len() != values.len() {
            return Err(crate::SynthError::invariant(
                "scalar reset inputs do not preserve sequential reset identity",
            ));
        }
        resets
            .iter()
            .zip(controls)
            .zip(values)
            .map(|((reset, &value), values)| {
                Ok(word::Reset {
                    kind: reset.kind,
                    value,
                    active_high: reset.active_high,
                    reset_value: self.published_reset_value(self.bit(*values, index), source)?,
                })
            })
            .collect()
    }

    fn published_reset_value(
        &mut self,
        bit: ScalarBit,
        source: &word::SourceSpan,
    ) -> Result<word::ValueId, crate::SynthError> {
        let value = self.backend.word_value(bit).ok_or_else(|| {
            crate::SynthError::invariant("sequential reset has no Word shell value")
        })?;
        let Some(stored) = self.module.value(value).cloned() else {
            return Err(crate::SynthError::invariant(
                "sequential reset references an unknown Word value",
            ));
        };
        let word::ValueKind::Constant(bits) = stored.kind else {
            return Ok(value);
        };
        let [constant] = bits.as_slice() else {
            return Err(crate::SynthError::invariant(
                "scalar sequential reset has a non-scalar constant",
            ));
        };
        let resolved =
            crate::boolean::resolve_publication_bit(*constant, self.module.name(), &stored.source)?;
        if resolved == *constant {
            return Ok(value);
        }
        let bit = self.constant(resolved, stored.ty.state(), source)?;
        self.backend.word_value(bit).ok_or_else(|| {
            crate::SynthError::invariant("published sequential reset has no Word value")
        })
    }
}
