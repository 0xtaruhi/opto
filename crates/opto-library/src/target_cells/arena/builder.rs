// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::super::{BooleanFunction, TargetPin, TargetSequential, TargetTimingArc};
use super::{
    ArcDelayModel, ArenaRange, CellRecord, FunctionNode, LibraryError, LocalCellId, LookupTable,
    PinReceiverCapacitanceModel, PinRecord, SequentialRecord, TargetCell, TargetCellArena,
    TargetDelayModelId, TargetFunctionId, TargetMemory, TargetNameId, TargetPinId,
    TargetReceiverModelId, TargetSequentialId, TargetTableId, TargetTimingArcId, TextRange,
    TimingArcRecord,
};
use std::collections::HashMap;

#[derive(Default)]
pub(super) struct ArenaBuilder {
    names: HashMap<String, TargetNameId>,
    cells: Vec<CellRecord>,
    pins: Vec<PinRecord>,
    timing_arcs: Vec<TimingArcRecord>,
    sequential: Vec<SequentialRecord>,
    state_variables: Vec<TargetNameId>,
    functions: Vec<FunctionNode>,
    function_ids: HashMap<FunctionNode, TargetFunctionId>,
    tables: Vec<LookupTable>,
    delay_models: Vec<ArcDelayModel>,
    receiver_models: Vec<PinReceiverCapacitanceModel>,
    memories: Vec<TargetMemory>,
}

impl ArenaBuilder {
    pub(super) fn seal(mut self, cells: Vec<TargetCell>) -> Result<TargetCellArena, LibraryError> {
        for cell in cells {
            self.push_cell(cell)?;
        }
        let (text, names) = self.seal_names()?;
        Ok(TargetCellArena {
            text,
            names,
            cells: self.cells.into_boxed_slice(),
            pins: self.pins.into_boxed_slice(),
            timing_arcs: self.timing_arcs.into_boxed_slice(),
            sequential: self.sequential.into_boxed_slice(),
            state_variables: self.state_variables.into_boxed_slice(),
            functions: self.functions.into_boxed_slice(),
            tables: self.tables.into_boxed_slice(),
            delay_models: self.delay_models.into_boxed_slice(),
            receiver_models: self.receiver_models.into_boxed_slice(),
            memories: self.memories.into_boxed_slice(),
        })
    }

    fn push_cell(&mut self, cell: TargetCell) -> Result<(), LibraryError> {
        let name = self.intern_name(cell.name)?;
        let pins = self.push_pins(cell.pins)?;
        let sequential = self.push_sequential(cell.sequential)?;
        let memory = cell
            .memory
            .map(|memory| {
                let index = u32::try_from(self.memories.len()).map_err(|_| {
                    LibraryError::ArenaCapacity {
                        arena: "target memories",
                    }
                })?;
                self.memories.push(memory);
                Ok::<_, LibraryError>(index)
            })
            .transpose()?;
        self.cells.push(CellRecord {
            name,
            area: cell.area,
            dont_use: cell.dont_use,
            usage: cell.usage,
            clock_gate: cell.clock_gate,
            pins,
            sequential,
            memory,
        });
        LocalCellId::new(self.cells.len() - 1)?;
        Ok(())
    }

    fn push_pins(&mut self, pins: Vec<TargetPin>) -> Result<ArenaRange, LibraryError> {
        let start = self.pins.len();
        for pin in pins {
            let record = PinRecord {
                name: self.intern_name(pin.name)?,
                direction: pin.direction,
                function: self.intern_optional_function(pin.function.as_ref())?,
                three_state: self.intern_optional_function(pin.three_state.as_ref())?,
                capacitance: pin.capacitance,
                rise_capacitance: pin.rise_capacitance,
                fall_capacitance: pin.fall_capacitance,
                receiver_capacitance: self.push_receiver(pin.receiver_capacitance)?,
                fanout_load: pin.fanout_load,
                next_state_type: pin.next_state_type,
                clock_gate_role: pin.clock_gate_role,
                timing_arcs: self.push_timing_arcs(pin.timing_arcs)?,
            };
            self.pins.push(record);
            TargetPinId::new(self.pins.len() - 1)?;
        }
        range(start, self.pins.len(), "target pins")
    }

    fn push_timing_arcs(&mut self, arcs: Vec<TargetTimingArc>) -> Result<ArenaRange, LibraryError> {
        let start = self.timing_arcs.len();
        for arc in arcs {
            let record = TimingArcRecord {
                related_pin: self.intern_name(arc.related_pin)?,
                timing_type: arc.timing_type,
                timing_sense: arc.timing_sense,
                delay_model: self.push_delay(arc.delay_model)?,
                rise_constraint: self.push_table(arc.rise_constraint)?,
                fall_constraint: self.push_table(arc.fall_constraint)?,
            };
            self.timing_arcs.push(record);
            TargetTimingArcId::new(self.timing_arcs.len() - 1)?;
        }
        range(start, self.timing_arcs.len(), "target timing arcs")
    }

    fn push_sequential(
        &mut self,
        sequential: Vec<TargetSequential>,
    ) -> Result<ArenaRange, LibraryError> {
        let start = self.sequential.len();
        for sequential in sequential {
            let names = sequential
                .state_variables
                .into_iter()
                .map(|name| self.intern_name(name))
                .collect::<Result<Vec<_>, _>>()?;
            let state_variables = ArenaRange::append(&mut self.state_variables, names)?;
            let record = SequentialRecord {
                kind: sequential.kind,
                state_variables,
                clocked_on: self.intern_optional_function(sequential.clocked_on.as_ref())?,
                next_state: self.intern_optional_function(sequential.next_state.as_ref())?,
                enable: self.intern_optional_function(sequential.enable.as_ref())?,
                clear: self.intern_optional_function(sequential.clear.as_ref())?,
                preset: self.intern_optional_function(sequential.preset.as_ref())?,
            };
            self.sequential.push(record);
            TargetSequentialId::new(self.sequential.len() - 1)?;
        }
        range(
            start,
            self.sequential.len(),
            "target sequential declarations",
        )
    }

    fn intern_name(&mut self, name: String) -> Result<TargetNameId, LibraryError> {
        if let Some(&id) = self.names.get(name.as_str()) {
            return Ok(id);
        }
        let id = TargetNameId::new(self.names.len())?;
        self.names.insert(name, id);
        Ok(id)
    }

    fn intern_optional_function(
        &mut self,
        function: Option<&BooleanFunction>,
    ) -> Result<Option<TargetFunctionId>, LibraryError> {
        function
            .map(|function| self.intern_function(function))
            .transpose()
    }

    fn intern_function(
        &mut self,
        function: &BooleanFunction,
    ) -> Result<TargetFunctionId, LibraryError> {
        let node = match function {
            BooleanFunction::Const(value) => FunctionNode::Const(*value),
            BooleanFunction::Pin(name) => FunctionNode::Pin(self.intern_name(name.clone())?),
            BooleanFunction::Not(argument) => FunctionNode::Not(self.intern_function(argument)?),
            BooleanFunction::And(left, right) => {
                FunctionNode::And(self.intern_function(left)?, self.intern_function(right)?)
            }
            BooleanFunction::Or(left, right) => {
                FunctionNode::Or(self.intern_function(left)?, self.intern_function(right)?)
            }
            BooleanFunction::Xor(left, right) => {
                FunctionNode::Xor(self.intern_function(left)?, self.intern_function(right)?)
            }
            BooleanFunction::Imp(left, right) => {
                FunctionNode::Imp(self.intern_function(left)?, self.intern_function(right)?)
            }
            BooleanFunction::Iff(left, right) => {
                FunctionNode::Iff(self.intern_function(left)?, self.intern_function(right)?)
            }
            BooleanFunction::Cond(condition, when_true, when_false) => FunctionNode::Cond(
                self.intern_function(condition)?,
                self.intern_function(when_true)?,
                self.intern_function(when_false)?,
            ),
        };
        if let Some(&id) = self.function_ids.get(&node) {
            return Ok(id);
        }
        let id = TargetFunctionId::new(self.functions.len())?;
        self.functions.push(node);
        self.function_ids.insert(node, id);
        Ok(id)
    }

    fn push_table(
        &mut self,
        table: Option<LookupTable>,
    ) -> Result<Option<TargetTableId>, LibraryError> {
        table
            .map(|table| {
                let id = TargetTableId::new(self.tables.len())?;
                self.tables.push(table);
                Ok(id)
            })
            .transpose()
    }

    fn push_delay(
        &mut self,
        model: Option<ArcDelayModel>,
    ) -> Result<Option<TargetDelayModelId>, LibraryError> {
        model
            .map(|model| {
                let id = TargetDelayModelId::new(self.delay_models.len())?;
                self.delay_models.push(model);
                Ok(id)
            })
            .transpose()
    }

    fn push_receiver(
        &mut self,
        model: Option<PinReceiverCapacitanceModel>,
    ) -> Result<Option<TargetReceiverModelId>, LibraryError> {
        model
            .map(|model| {
                let id = TargetReceiverModelId::new(self.receiver_models.len())?;
                self.receiver_models.push(model);
                Ok(id)
            })
            .transpose()
    }

    fn seal_names(&mut self) -> Result<(Box<str>, Box<[TextRange]>), LibraryError> {
        let mut names = std::mem::take(&mut self.names)
            .into_iter()
            .collect::<Vec<_>>();
        names.sort_unstable_by_key(|(_, id)| id.slot());
        let mut text = String::new();
        let mut ranges = Vec::with_capacity(names.len());
        for (name, _) in names {
            let start = u32::try_from(text.len()).map_err(|_| LibraryError::ArenaCapacity {
                arena: "target-library text",
            })?;
            text.push_str(&name);
            let end = u32::try_from(text.len()).map_err(|_| LibraryError::ArenaCapacity {
                arena: "target-library text",
            })?;
            ranges.push(TextRange {
                start,
                len: end - start,
            });
        }
        Ok((text.into_boxed_str(), ranges.into_boxed_slice()))
    }
}

fn range(start: usize, end: usize, arena: &'static str) -> Result<ArenaRange, LibraryError> {
    Ok(ArenaRange {
        start: u32::try_from(start).map_err(|_| LibraryError::ArenaCapacity { arena })?,
        len: u32::try_from(
            end.checked_sub(start)
                .ok_or(LibraryError::ArenaCapacity { arena })?,
        )
        .map_err(|_| LibraryError::ArenaCapacity { arena })?,
    })
}
