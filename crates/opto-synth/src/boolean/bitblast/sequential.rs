// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::{BitBlaster, word};

impl BitBlaster<'_> {
    pub(super) fn register_bits(
        &mut self,
        register: &word::RegisterOp,
        source: &word::SourceSpan,
    ) -> Result<Vec<word::ValueId>, crate::SynthError> {
        let data = self.value(register.d)?;
        let clock = self.scalar_value(register.clock)?;
        let enable = if let Some(enable) = register.enable {
            Some(word::Enable {
                value: self.scalar_value(enable.value)?,
                active_high: enable.active_high,
            })
        } else {
            None
        };
        let reset_controls = register
            .resets
            .iter()
            .map(|reset| self.scalar_value(reset.value))
            .collect::<Result<Vec<_>, _>>()?;
        let reset_values = register
            .resets
            .iter()
            .map(|reset| self.value(reset.reset_value))
            .collect::<Result<Vec<_>, _>>()?;
        let mut bits = Vec::with_capacity(data.len() as usize);
        for index in 0..data.len() {
            let resets = register
                .resets
                .iter()
                .zip(&reset_controls)
                .zip(&reset_values)
                .map(|((reset, &value), values)| word::Reset {
                    kind: reset.kind,
                    value,
                    active_high: reset.active_high,
                    reset_value: self.bit(*values, index),
                })
                .collect();
            bits.push(
                self.module
                    .register(
                        word::RegisterOp {
                            name: register.name,
                            d: self.bit(data, index),
                            clock,
                            edge: register.edge,
                            enable,
                            resets,
                        },
                        source.clone(),
                    )
                    .map_err(crate::SynthError::from)?,
            );
        }
        Ok(bits)
    }

    pub(super) fn latch_bits(
        &mut self,
        latch: &word::LatchOp,
        source: &word::SourceSpan,
    ) -> Result<Vec<word::ValueId>, crate::SynthError> {
        let data = self.value(latch.d)?;
        let enable = word::Enable {
            value: self.scalar_value(latch.enable.value)?,
            active_high: latch.enable.active_high,
        };
        let reset_controls = latch
            .resets
            .iter()
            .map(|reset| self.scalar_value(reset.value))
            .collect::<Result<Vec<_>, _>>()?;
        let reset_values = latch
            .resets
            .iter()
            .map(|reset| self.value(reset.reset_value))
            .collect::<Result<Vec<_>, _>>()?;
        let mut bits = Vec::with_capacity(data.len() as usize);
        for index in 0..data.len() {
            let resets = latch
                .resets
                .iter()
                .zip(&reset_controls)
                .zip(&reset_values)
                .map(|((reset, &value), values)| word::Reset {
                    kind: reset.kind,
                    value,
                    active_high: reset.active_high,
                    reset_value: self.bit(*values, index),
                })
                .collect();
            bits.push(
                self.module
                    .latch(
                        word::LatchOp {
                            name: latch.name,
                            d: self.bit(data, index),
                            enable,
                            resets,
                        },
                        source.clone(),
                    )
                    .map_err(crate::SynthError::from)?,
            );
        }
        Ok(bits)
    }
}
