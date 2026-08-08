// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Cross-arena validation for word-level module state.
//!
//! These checks reject foreign IDs, inconsistent types, invalid memory ports,
//! and divergent name indexes before a module crosses a publication boundary.

use super::{
    LValue, Memory, MemoryClock, MemoryId, MemoryReadPort, MemoryReadTiming, MemoryWritePort,
    PortDirection, SignalId, SignalKind, SignalResolution, ValueId, WordError, WordModule,
    WordType, dense_id,
};

impl WordModule {
    pub(in crate::word) fn value_ty(&self, value: ValueId) -> Result<WordType, WordError> {
        self.values
            .get(value.index())
            .map(|value| value.ty)
            .ok_or_else(|| WordError::new(format!("unknown RTL value {value:?}")))
    }

    pub(in crate::word) fn signal_ty(&self, signal: SignalId) -> Result<WordType, WordError> {
        self.signals
            .get(signal.index())
            .map(|signal| signal.ty)
            .ok_or_else(|| WordError::new(format!("unknown RTL signal {signal:?}")))
    }

    pub(in crate::word) fn lvalue_ty(&self, lvalue: &LValue) -> Result<WordType, WordError> {
        let signal_ty = self.signal_ty(lvalue.signal)?;
        if lvalue.range.is_some() && lvalue.dynamic.is_some() {
            return Err(WordError::new(
                "lvalue cannot have both static and dynamic ranges",
            ));
        }
        if let Some(dynamic) = lvalue.dynamic {
            let offset_ty = self.value_ty(dynamic.offset)?;
            if offset_ty.is_signed() {
                return Err(WordError::new("dynamic lvalue offset must be unsigned"));
            }
            if dynamic.width.get() > signal_ty.width() {
                return Err(WordError::new(format!(
                    "dynamic lvalue width {} exceeds signal width {}",
                    dynamic.width.get(),
                    signal_ty.width()
                )));
            }
            return WordType::new(dynamic.width.get(), false, signal_ty.state());
        }
        if let Some(range) = lvalue.range {
            let high = range.msb.max(range.lsb);
            if high >= signal_ty.width() {
                return Err(WordError::new(format!(
                    "lvalue range [{}:{}] exceeds signal width {}",
                    range.msb,
                    range.lsb,
                    signal_ty.width()
                )));
            }
            WordType::new(range.width(), false, signal_ty.state())
        } else {
            Ok(signal_ty)
        }
    }

    pub(in crate::word) fn require_value_width(
        &self,
        value: ValueId,
        width: u32,
        context: &str,
    ) -> Result<(), WordError> {
        let ty = self.value_ty(value)?;
        if ty.width() != width {
            return Err(WordError::new(format!(
                "{context} must be {width} bit wide, got {}",
                ty.width()
            )));
        }
        Ok(())
    }

    /// Validates memory names, name indexes, and every read/write port contract.
    ///
    /// # Errors
    ///
    /// Returns [`WordError`] on the first memory name, range, width, clock,
    /// enable, mask, or write-priority invariant violation.
    pub fn validate_memories(&self) -> Result<(), WordError> {
        for (index, memory) in self.memories.iter().enumerate() {
            let id = MemoryId::from_index(index)?;
            if self.names.resolve(memory.name).is_none()
                || dense_id(&self.named_memories, memory.name) != Some(id)
                || dense_id(&self.named_signals, memory.name).is_some()
            {
                return Err(WordError::new(format!(
                    "memory {id:?} has an invalid or conflicting name"
                )));
            }
        }
        for (slot, memory) in self.named_memories.iter().enumerate() {
            if let Some(memory) = memory {
                let stored = self.memory(*memory).ok_or_else(|| {
                    WordError::new("memory name index references an unknown memory")
                })?;
                if stored.name.raw() as usize != slot {
                    return Err(WordError::new(
                        "memory name index does not match the stored memory name",
                    ));
                }
            }
        }
        for (index, port) in self.memory_read_ports.iter().enumerate() {
            self.validate_memory_read_port(port, Some(index))?;
        }
        for (index, port) in self.memory_write_ports.iter().enumerate() {
            self.validate_memory_write_port(port, Some(index))?;
        }
        Ok(())
    }

    pub(super) fn validate_memory_read_port(
        &self,
        port: &MemoryReadPort,
        current: Option<usize>,
    ) -> Result<(), WordError> {
        let memory = self.validate_memory_address(port.memory, port.address, "read")?;
        let data = self
            .signal(port.data)
            .ok_or_else(|| WordError::new("memory read port references an unknown data signal"))?;
        if data.ty != memory.element_type {
            return Err(WordError::new(format!(
                "memory read data type {:?} does not match element type {:?}",
                data.ty, memory.element_type
            )));
        }
        let valid_target = match data.kind {
            SignalKind::Wire => true,
            SignalKind::Port(id) => self
                .port(id)
                .is_some_and(|port| port.direction == PortDirection::Output),
            SignalKind::Register | SignalKind::ProcessLocal => false,
        };
        if !valid_target || data.resolution != SignalResolution::SingleDriver {
            return Err(WordError::new(
                "memory read data must drive a single-driver wire or output port",
            ));
        }
        if self
            .memory_read_ports
            .iter()
            .enumerate()
            .any(|(index, other)| Some(index) != current && other.data == port.data)
            || self
                .connects
                .iter()
                .any(|connect| connect.target.signal == port.data)
        {
            return Err(WordError::new(
                "memory read data signal must have exactly one memory-port driver",
            ));
        }
        if let MemoryReadTiming::Synchronous { clock, enable, .. } = port.timing {
            self.validate_memory_clock(clock, "read")?;
            if let Some(enable) = enable {
                self.require_value_width(enable.value, 1, "memory read enable")?;
            }
        }
        Ok(())
    }

    pub(super) fn validate_memory_write_port(
        &self,
        port: &MemoryWritePort,
        current: Option<usize>,
    ) -> Result<(), WordError> {
        let memory = self.validate_memory_address(port.memory, port.address, "write")?;
        let data = self.value_ty(port.data)?;
        if data != memory.element_type {
            return Err(WordError::new(format!(
                "memory write data type {:?} does not match element type {:?}",
                data, memory.element_type
            )));
        }
        self.validate_memory_clock(port.clock, "write")?;
        if let Some(enable) = port.enable {
            self.require_value_width(enable.value, 1, "memory write enable")?;
        }
        if let Some(mask) = port.mask {
            let mask_type = self.value_ty(mask.value)?;
            let covered = mask_type
                .width()
                .checked_mul(mask.granularity.get())
                .ok_or_else(|| WordError::new("memory write mask width overflows"))?;
            if mask_type.is_signed() || covered != memory.element_type.width() {
                return Err(WordError::new(format!(
                    "memory write mask covers {covered} bits, expected {} unsigned bits",
                    memory.element_type.width()
                )));
            }
        }
        if self
            .memory_write_ports
            .iter()
            .enumerate()
            .any(|(index, other)| {
                Some(index) != current
                    && other.memory == port.memory
                    && other.priority == port.priority
            })
        {
            return Err(WordError::new(format!(
                "memory write priority {} is not unique",
                port.priority
            )));
        }
        Ok(())
    }

    fn validate_memory_address(
        &self,
        memory: MemoryId,
        address: ValueId,
        direction: &str,
    ) -> Result<&Memory, WordError> {
        let memory = self.memory(memory).ok_or_else(|| {
            WordError::new(format!(
                "memory {direction} port references an unknown memory"
            ))
        })?;
        let address_type = self.value_ty(address)?;
        let minimum_width = (u32::BITS - (memory.depth.get() - 1).leading_zeros()).max(1);
        if address_type.is_signed() || address_type.width() < minimum_width {
            return Err(WordError::new(format!(
                "memory {direction} address must be unsigned and at least {minimum_width} bits wide"
            )));
        }
        Ok(memory)
    }

    fn validate_memory_clock(&self, clock: MemoryClock, direction: &str) -> Result<(), WordError> {
        self.require_value_width(clock.value, 1, &format!("memory {direction} port clock"))
    }
}
