// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use opto_ir::word;

/// Conservative low-bit observability for word values.
///
/// A prefix width `n` means that some observable result depends on bits
/// `0..n`. This representation is deliberately dense and compact: adders,
/// subtractors, and multipliers turn every demanded output bit into a low-bit
/// prefix because of carry propagation. Other operations may over-approximate
/// sparse demand, but never remove an observable bit.
pub(super) struct ObservableBits {
    value_prefixes: Box<[u32]>,
}

impl ObservableBits {
    pub(super) fn analyze(module: &word::WordModule) -> Result<Self, crate::SynthError> {
        DemandAnalysis::new(module)?.run()
    }

    pub(super) fn required_prefix(&self, value: word::ValueId) -> u32 {
        self.value_prefixes.get(value.index()).copied().unwrap_or(0)
    }
}

struct DemandAnalysis<'a> {
    module: &'a word::WordModule,
    drivers: SignalDrivers,
    value_prefixes: Vec<u32>,
    signal_prefixes: Vec<u32>,
    pending_values: Vec<word::ValueId>,
    pending_signals: Vec<word::SignalId>,
}

impl<'a> DemandAnalysis<'a> {
    fn new(module: &'a word::WordModule) -> Result<Self, crate::SynthError> {
        if module
            .connects()
            .iter()
            .any(|connect| connect.target.dynamic.is_some())
        {
            return Err(crate::SynthError::unsupported(
                "dynamic connect target reached observability analysis",
            ));
        }
        Ok(Self {
            module,
            drivers: SignalDrivers::build(module)?,
            value_prefixes: vec![0; module.values().len()],
            signal_prefixes: vec![0; module.signals().len()],
            pending_values: Vec::new(),
            pending_signals: Vec::new(),
        })
    }

    fn run(mut self) -> Result<ObservableBits, crate::SynthError> {
        for port in self.module.ports() {
            if matches!(
                port.direction,
                word::PortDirection::Output | word::PortDirection::Inout
            ) {
                self.require_signal(port.signal, port.ty.width())?;
            }
        }
        for value in self
            .module
            .instances()
            .iter()
            .flat_map(|instance| &instance.connections)
            .map(|connection| connection.value)
        {
            self.require_value_full(value)?;
        }

        while !self.pending_values.is_empty() || !self.pending_signals.is_empty() {
            while let Some(value) = self.pending_values.pop() {
                self.propagate_value(value)?;
            }
            while let Some(signal) = self.pending_signals.pop() {
                self.propagate_signal(signal)?;
            }
        }

        Ok(ObservableBits {
            value_prefixes: self.value_prefixes.into_boxed_slice(),
        })
    }

    fn propagate_value(&mut self, value_id: word::ValueId) -> Result<(), crate::SynthError> {
        let demand = self.value_prefixes[value_id.index()];
        let value = self
            .module
            .value(value_id)
            .ok_or_else(|| {
                crate::SynthError::invariant(format!(
                    "observability references unknown value {value_id:?}"
                ))
            })?
            .clone();
        match value.kind {
            word::ValueKind::Signal(reference) => {
                let width = demand.min(reference.width());
                let end = reference.lsb.checked_add(width).ok_or_else(|| {
                    crate::SynthError::invariant("observable signal range overflow")
                })?;
                self.require_signal(reference.signal, end)
            }
            word::ValueKind::Constant(_) => Ok(()),
            word::ValueKind::Operation(operation_id) => {
                let operation = self
                    .module
                    .operation(operation_id)
                    .ok_or_else(|| {
                        crate::SynthError::invariant(format!(
                            "observability references unknown operation {operation_id:?}"
                        ))
                    })?
                    .kind
                    .clone();
                self.propagate_operation(operation, demand)
            }
        }
    }

    fn propagate_operation(
        &mut self,
        operation: word::OpKind,
        demand: u32,
    ) -> Result<(), crate::SynthError> {
        match operation {
            word::OpKind::Unary { op, arg } => match op {
                word::UnaryOp::BitNot => self.require_value(arg, demand),
                word::UnaryOp::LogicalNot
                | word::UnaryOp::ReductionAnd
                | word::UnaryOp::ReductionOr
                | word::UnaryOp::ReductionXor => self.require_value_full(arg),
            },
            word::OpKind::Binary { op, left, right } => match op {
                word::BinaryOp::Add
                | word::BinaryOp::Sub
                | word::BinaryOp::Mul
                | word::BinaryOp::BitAnd
                | word::BinaryOp::BitOr
                | word::BinaryOp::BitXor => {
                    self.require_value(left, demand)?;
                    self.require_value(right, demand)
                }
                word::BinaryOp::LogicalAnd
                | word::BinaryOp::LogicalOr
                | word::BinaryOp::Div
                | word::BinaryOp::Mod
                | word::BinaryOp::Eq
                | word::BinaryOp::Ne
                | word::BinaryOp::Lt
                | word::BinaryOp::Le
                | word::BinaryOp::Gt
                | word::BinaryOp::Ge
                | word::BinaryOp::Shl
                | word::BinaryOp::Shr
                | word::BinaryOp::Ashr => {
                    self.require_value_full(left)?;
                    self.require_value_full(right)
                }
            },
            word::OpKind::Mux {
                cond,
                then_value,
                else_value,
            } => {
                self.require_value_full(cond)?;
                self.require_value(then_value, demand)?;
                self.require_value(else_value, demand)
            }
            word::OpKind::Concat { parts } => {
                let mut remaining = demand;
                for part in parts.into_iter().rev() {
                    if remaining == 0 {
                        break;
                    }
                    let width = self.value_width(part)?;
                    self.require_value(part, remaining.min(width))?;
                    remaining = remaining.saturating_sub(width);
                }
                Ok(())
            }
            word::OpKind::Extract { value, lsb, .. } => {
                let prefix = lsb.checked_add(demand).ok_or_else(|| {
                    crate::SynthError::invariant("observable extract range overflow")
                })?;
                self.require_value(value, prefix)
            }
            word::OpKind::DynamicExtract { value, offset, .. } => {
                self.require_value_full(value)?;
                self.require_value_full(offset)
            }
            word::OpKind::DynamicInsert {
                value,
                offset,
                replacement,
            } => {
                self.require_value_full(value)?;
                self.require_value_full(offset)?;
                self.require_value_full(replacement)
            }
            word::OpKind::Cast { value, .. } => self.require_value(value, demand),
            word::OpKind::Register(register) => {
                self.require_value(register.d, demand)?;
                self.require_value_full(register.clock)?;
                if let Some(enable) = register.enable {
                    self.require_value_full(enable.value)?;
                }
                for reset in &register.resets {
                    self.require_value_full(reset.value)?;
                    self.require_value(reset.reset_value, demand)?;
                }
                Ok(())
            }
            word::OpKind::Latch(latch) => {
                self.require_value(latch.d, demand)?;
                self.require_value_full(latch.enable.value)?;
                for reset in &latch.resets {
                    self.require_value_full(reset.value)?;
                    self.require_value(reset.reset_value, demand)?;
                }
                Ok(())
            }
        }
    }

    fn propagate_signal(&mut self, signal: word::SignalId) -> Result<(), crate::SynthError> {
        let signal_demand = self.signal_prefixes[signal.index()];
        let drivers = self.drivers.for_signal(signal)?.to_vec();
        for connect_index in drivers {
            let connect = self
                .module
                .connects()
                .get(connect_index as usize)
                .ok_or_else(|| {
                    crate::SynthError::invariant(format!(
                        "observability references unknown connect {connect_index}"
                    ))
                })?;
            let source_width = self.value_width(connect.value)?;
            let source_demand = match connect.target.range {
                None => signal_demand.min(source_width),
                Some(range) if range.msb >= range.lsb => {
                    signal_demand.saturating_sub(range.lsb).min(source_width)
                }
                Some(range) => u32::from(signal_demand > range.msb) * source_width,
            };
            self.require_value(connect.value, source_demand)?;
        }
        Ok(())
    }

    fn require_value_full(&mut self, value: word::ValueId) -> Result<(), crate::SynthError> {
        let width = self.value_width(value)?;
        self.require_value(value, width)
    }

    fn require_value(
        &mut self,
        value: word::ValueId,
        prefix: u32,
    ) -> Result<(), crate::SynthError> {
        let width = self.value_width(value)?;
        let prefix = prefix.min(width);
        let slot = self.value_prefixes.get_mut(value.index()).ok_or_else(|| {
            crate::SynthError::invariant(format!(
                "observability references unknown value {value:?}"
            ))
        })?;
        if prefix > *slot {
            *slot = prefix;
            self.pending_values.push(value);
        }
        Ok(())
    }

    fn require_signal(
        &mut self,
        signal: word::SignalId,
        prefix: u32,
    ) -> Result<(), crate::SynthError> {
        let width = self
            .module
            .signal(signal)
            .ok_or_else(|| {
                crate::SynthError::invariant(format!(
                    "observability references unknown signal {signal:?}"
                ))
            })?
            .ty
            .width();
        let prefix = prefix.min(width);
        let slot = self
            .signal_prefixes
            .get_mut(signal.index())
            .ok_or_else(|| {
                crate::SynthError::invariant(format!(
                    "observability references unknown signal {signal:?}"
                ))
            })?;
        if prefix > *slot {
            *slot = prefix;
            self.pending_signals.push(signal);
        }
        Ok(())
    }

    fn value_width(&self, value: word::ValueId) -> Result<u32, crate::SynthError> {
        self.module
            .value(value)
            .map(|value| value.ty.width())
            .ok_or_else(|| {
                crate::SynthError::invariant(format!(
                    "observability references unknown value {value:?}"
                ))
            })
    }
}

struct SignalDrivers {
    rows: opto_core::PackedRows<u32>,
}

impl SignalDrivers {
    fn build(module: &word::WordModule) -> Result<Self, crate::SynthError> {
        let entries = module
            .connects()
            .iter()
            .enumerate()
            .map(|(index, connect)| {
                Ok((
                    connect.target.signal.index(),
                    index.try_into().map_err(|_| {
                        crate::SynthError::capacity(
                            "connect index exceeds 32-bit driver-table capacity",
                        )
                    })?,
                ))
            })
            .collect::<Result<Vec<_>, crate::SynthError>>()?;
        Ok(Self {
            rows: opto_core::PackedRows::try_from_entries(module.signals().len(), entries)
                .map_err(|error| crate::SynthError::invariant(error.to_string()))?,
        })
    }

    fn for_signal(&self, signal: word::SignalId) -> Result<&[u32], crate::SynthError> {
        self.rows.get(signal.index()).ok_or_else(|| {
            crate::SynthError::invariant(format!("driver table has no signal {signal:?}"))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opto_ir::{ConstBits, word::LogicStateKind};

    #[test]
    fn follows_vector_signal_drivers_to_a_low_slice() {
        let mut module = word::WordModule::new("top");
        let wide = word::WordType::new(32, false, LogicStateKind::FourState).unwrap();
        let narrow = word::WordType::new(7, false, LogicStateKind::FourState).unwrap();
        let input = module
            .add_port(
                "a",
                word::PortDirection::Input,
                wide,
                word::SourceSpan::default(),
            )
            .unwrap();
        let output = module
            .add_port(
                "y",
                word::PortDirection::Output,
                narrow,
                word::SourceSpan::default(),
            )
            .unwrap();
        let input_value = module
            .read_signal(
                module.port(input).unwrap().signal,
                word::SourceSpan::default(),
            )
            .unwrap();
        let one = module
            .constant(
                ConstBits::from_bin_str("00000000000000000000000000000001").unwrap(),
                wide,
                word::SourceSpan::default(),
            )
            .unwrap();
        let sum = module
            .binary(
                word::BinaryOp::Add,
                input_value,
                one,
                word::SourceSpan::default(),
            )
            .unwrap();
        let internal = module
            .add_wire("sum", wide, word::SourceSpan::default())
            .unwrap();
        module
            .connect(
                word::LValue::signal(internal),
                sum,
                word::SourceSpan::default(),
            )
            .unwrap();
        let low = module
            .read_signal_slice(internal, 0, 7, word::SourceSpan::default())
            .unwrap();
        module
            .connect(
                word::LValue::signal(module.port(output).unwrap().signal),
                low,
                word::SourceSpan::default(),
            )
            .unwrap();

        let observable = ObservableBits::analyze(&module).unwrap();

        assert_eq!(observable.required_prefix(sum), 7);
    }

    #[test]
    fn higher_arithmetic_bits_require_the_carry_prefix() {
        let mut module = word::WordModule::new("top");
        let wide = word::WordType::new(16, false, LogicStateKind::FourState).unwrap();
        let bit = word::WordType::new(1, false, LogicStateKind::FourState).unwrap();
        let a = module
            .add_port(
                "a",
                word::PortDirection::Input,
                wide,
                word::SourceSpan::default(),
            )
            .unwrap();
        let b = module
            .add_port(
                "b",
                word::PortDirection::Input,
                wide,
                word::SourceSpan::default(),
            )
            .unwrap();
        let y = module
            .add_port(
                "y",
                word::PortDirection::Output,
                bit,
                word::SourceSpan::default(),
            )
            .unwrap();
        let left = module
            .read_signal(module.port(a).unwrap().signal, word::SourceSpan::default())
            .unwrap();
        let right = module
            .read_signal(module.port(b).unwrap().signal, word::SourceSpan::default())
            .unwrap();
        let sum = module
            .binary(
                word::BinaryOp::Add,
                left,
                right,
                word::SourceSpan::default(),
            )
            .unwrap();
        let selected = module
            .extract(sum, 10, 1, word::SourceSpan::default())
            .unwrap();
        module
            .connect(
                word::LValue::signal(module.port(y).unwrap().signal),
                selected,
                word::SourceSpan::default(),
            )
            .unwrap();

        let observable = ObservableBits::analyze(&module).unwrap();

        assert_eq!(observable.required_prefix(sum), 11);
    }
}
