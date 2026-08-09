// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::{
    BooleanFunctionKind, BooleanFunctionRef, TargetCellRef, TargetCellSet, TargetPinRef,
    TargetSequentialRef, TargetTimingArcRef,
};
use serde::Serialize;
use serde::ser::{SerializeSeq, SerializeStruct, SerializeTupleVariant};
use std::sync::Arc;

pub(super) struct FingerprintCells<'a>(pub(super) &'a TargetCellSet);
pub(super) struct FingerprintTopologyCells<'a>(pub(super) &'a TargetCellSet);

pub(super) fn topology_schema_bytes(cells: &TargetCellSet) -> Arc<[u8]> {
    opto_archive::to_bytes(&FingerprintTopologyCells(cells))
        .expect("sealed target cells have a canonical topology encoding")
        .into()
}

impl Serialize for FingerprintTopologyCells<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut sequence = serializer.serialize_seq(Some(self.0.len()))?;
        for cell in self.0.iter() {
            sequence.serialize_element(&FingerprintTopologyCell(cell))?;
        }
        sequence.end()
    }
}

struct FingerprintTopologyCell<'a>(TargetCellRef<'a>);

impl Serialize for FingerprintTopologyCell<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let cell = self.0;
        let mut state = serializer.serialize_struct("TimingTopologyCell", 3)?;
        state.serialize_field("name", cell.name())?;
        state.serialize_field("pins", &FingerprintTopologyPins(cell))?;
        state.serialize_field("sequential", &FingerprintSequential(cell))?;
        state.end()
    }
}

struct FingerprintTopologyPins<'a>(TargetCellRef<'a>);

impl Serialize for FingerprintTopologyPins<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let pins = self.0.pins();
        let mut sequence = serializer.serialize_seq(Some(pins.len()))?;
        for pin in pins {
            sequence.serialize_element(&FingerprintTopologyPin(pin))?;
        }
        sequence.end()
    }
}

struct FingerprintTopologyPin<'a>(TargetPinRef<'a>);

impl Serialize for FingerprintTopologyPin<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let pin = self.0;
        let mut state = serializer.serialize_struct("TimingTopologyPin", 5)?;
        state.serialize_field("name", pin.name())?;
        state.serialize_field("direction", &pin.direction())?;
        state.serialize_field("function", &FingerprintFunctionOption(pin.function()))?;
        state.serialize_field("three_state", &FingerprintFunctionOption(pin.three_state()))?;
        state.serialize_field("timing_arcs", &FingerprintTopologyTimingArcs(pin))?;
        state.end()
    }
}

struct FingerprintTopologyTimingArcs<'a>(TargetPinRef<'a>);

impl Serialize for FingerprintTopologyTimingArcs<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let arcs = self.0.timing_arcs();
        let mut sequence = serializer.serialize_seq(Some(arcs.len()))?;
        for arc in arcs {
            sequence.serialize_element(&FingerprintTopologyTimingArc(arc))?;
        }
        sequence.end()
    }
}

struct FingerprintTopologyTimingArc<'a>(TargetTimingArcRef<'a>);

impl Serialize for FingerprintTopologyTimingArc<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let arc = self.0;
        let mut state = serializer.serialize_struct("TimingTopologyArc", 3)?;
        state.serialize_field("related_pin", arc.related_pin())?;
        state.serialize_field("timing_type", &arc.timing_type())?;
        state.serialize_field("timing_sense", &arc.timing_sense())?;
        state.end()
    }
}

impl Serialize for FingerprintCells<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut sequence = serializer.serialize_seq(Some(self.0.len()))?;
        for cell in self.0.iter() {
            sequence.serialize_element(&FingerprintCell(cell))?;
        }
        sequence.end()
    }
}

struct FingerprintCell<'a>(TargetCellRef<'a>);

impl Serialize for FingerprintCell<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let cell = self.0;
        let mut state = serializer.serialize_struct("TargetCell", 7)?;
        state.serialize_field("name", cell.name())?;
        state.serialize_field("area", &cell.area())?;
        state.serialize_field("dont_use", &cell.dont_use())?;
        state.serialize_field("usage", &cell.usage())?;
        state.serialize_field("pins", &FingerprintPins(cell))?;
        state.serialize_field("sequential", &FingerprintSequential(cell))?;
        state.serialize_field("memory", &cell.memory())?;
        state.end()
    }
}

struct FingerprintPins<'a>(TargetCellRef<'a>);

impl Serialize for FingerprintPins<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let pins = self.0.pins();
        let mut sequence = serializer.serialize_seq(Some(pins.len()))?;
        for pin in pins {
            sequence.serialize_element(&FingerprintPin(pin))?;
        }
        sequence.end()
    }
}

struct FingerprintPin<'a>(TargetPinRef<'a>);

impl Serialize for FingerprintPin<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let pin = self.0;
        let mut state = serializer.serialize_struct("TargetPin", 11)?;
        state.serialize_field("name", pin.name())?;
        state.serialize_field("direction", &pin.direction())?;
        state.serialize_field("function", &FingerprintFunctionOption(pin.function()))?;
        state.serialize_field("three_state", &FingerprintFunctionOption(pin.three_state()))?;
        state.serialize_field("capacitance", &pin.capacitance())?;
        state.serialize_field("rise_capacitance", &pin.rise_capacitance())?;
        state.serialize_field("fall_capacitance", &pin.fall_capacitance())?;
        state.serialize_field("receiver_capacitance", &pin.receiver_capacitance())?;
        state.serialize_field("fanout_load", &pin.fanout_load())?;
        state.serialize_field("next_state_type", &pin.next_state_type())?;
        state.serialize_field("timing_arcs", &FingerprintTimingArcs(pin))?;
        state.end()
    }
}

struct FingerprintTimingArcs<'a>(TargetPinRef<'a>);

impl Serialize for FingerprintTimingArcs<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let arcs = self.0.timing_arcs();
        let mut sequence = serializer.serialize_seq(Some(arcs.len()))?;
        for arc in arcs {
            sequence.serialize_element(&FingerprintTimingArc(arc))?;
        }
        sequence.end()
    }
}

struct FingerprintTimingArc<'a>(TargetTimingArcRef<'a>);

impl Serialize for FingerprintTimingArc<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let arc = self.0;
        let mut state = serializer.serialize_struct("TargetTimingArc", 6)?;
        state.serialize_field("related_pin", arc.related_pin())?;
        state.serialize_field("timing_type", &arc.timing_type())?;
        state.serialize_field("timing_sense", &arc.timing_sense())?;
        state.serialize_field("delay_model", &arc.delay_model())?;
        state.serialize_field("rise_constraint", &arc.rise_constraint())?;
        state.serialize_field("fall_constraint", &arc.fall_constraint())?;
        state.end()
    }
}

struct FingerprintSequential<'a>(TargetCellRef<'a>);

impl Serialize for FingerprintSequential<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let sequential = self.0.sequential();
        let mut sequence = serializer.serialize_seq(Some(sequential.len()))?;
        for declaration in sequential {
            sequence.serialize_element(&FingerprintSequentialDeclaration(declaration))?;
        }
        sequence.end()
    }
}

struct FingerprintSequentialDeclaration<'a>(TargetSequentialRef<'a>);

impl Serialize for FingerprintSequentialDeclaration<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let declaration = self.0;
        let mut state = serializer.serialize_struct("TargetSequential", 7)?;
        state.serialize_field("kind", &declaration.kind())?;
        state.serialize_field("state_variables", &FingerprintStateVariables(declaration))?;
        state.serialize_field(
            "clocked_on",
            &FingerprintFunctionOption(declaration.clocked_on()),
        )?;
        state.serialize_field(
            "next_state",
            &FingerprintFunctionOption(declaration.next_state()),
        )?;
        state.serialize_field("enable", &FingerprintFunctionOption(declaration.enable()))?;
        state.serialize_field("clear", &FingerprintFunctionOption(declaration.clear()))?;
        state.serialize_field("preset", &FingerprintFunctionOption(declaration.preset()))?;
        state.end()
    }
}

struct FingerprintStateVariables<'a>(TargetSequentialRef<'a>);

impl Serialize for FingerprintStateVariables<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let names = self.0.state_variables();
        let mut sequence = serializer.serialize_seq(Some(names.len()))?;
        for name in names {
            sequence.serialize_element(name)?;
        }
        sequence.end()
    }
}

struct FingerprintFunctionOption<'a>(Option<BooleanFunctionRef<'a>>);

impl Serialize for FingerprintFunctionOption<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self.0 {
            Some(function) => serializer.serialize_some(&FingerprintFunction(function)),
            None => serializer.serialize_none(),
        }
    }
}

struct FingerprintFunction<'a>(BooleanFunctionRef<'a>);

impl Serialize for FingerprintFunction<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self.0.kind() {
            BooleanFunctionKind::Const(value) => {
                serializer.serialize_newtype_variant("BooleanFunction", 0, "Const", &value)
            }
            BooleanFunctionKind::Pin(name) => {
                serializer.serialize_newtype_variant("BooleanFunction", 1, "Pin", name)
            }
            BooleanFunctionKind::Not(argument) => serializer.serialize_newtype_variant(
                "BooleanFunction",
                2,
                "Not",
                &FingerprintFunction(argument),
            ),
            BooleanFunctionKind::And(left, right) => {
                serialize_binary_function(serializer, 3, "And", left, right)
            }
            BooleanFunctionKind::Or(left, right) => {
                serialize_binary_function(serializer, 4, "Or", left, right)
            }
            BooleanFunctionKind::Xor(left, right) => {
                serialize_binary_function(serializer, 5, "Xor", left, right)
            }
            BooleanFunctionKind::Imp(left, right) => {
                serialize_binary_function(serializer, 6, "Imp", left, right)
            }
            BooleanFunctionKind::Iff(left, right) => {
                serialize_binary_function(serializer, 7, "Iff", left, right)
            }
            BooleanFunctionKind::Cond(condition, when_true, when_false) => {
                let mut tuple =
                    serializer.serialize_tuple_variant("BooleanFunction", 8, "Cond", 3)?;
                tuple.serialize_field(&FingerprintFunction(condition))?;
                tuple.serialize_field(&FingerprintFunction(when_true))?;
                tuple.serialize_field(&FingerprintFunction(when_false))?;
                tuple.end()
            }
        }
    }
}

fn serialize_binary_function<S>(
    serializer: S,
    variant_index: u32,
    variant: &'static str,
    left: BooleanFunctionRef<'_>,
    right: BooleanFunctionRef<'_>,
) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    let mut tuple =
        serializer.serialize_tuple_variant("BooleanFunction", variant_index, variant, 2)?;
    tuple.serialize_field(&FingerprintFunction(left))?;
    tuple.serialize_field(&FingerprintFunction(right))?;
    tuple.end()
}
