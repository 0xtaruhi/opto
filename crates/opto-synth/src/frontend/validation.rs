// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! The synthesis frontend boundary.
//!
//! Back-end planning must only observe a sealed Word module: procedures and
//! resolved nets have been lowered, source-level combinational feedback has
//! been rejected, and all structural references have been validated. Keeping
//! this sequence behind one entry point prevents callers from accidentally
//! consuming a partially normalized design.

use opto_ir::{rtl::RtlModule, word};

pub(crate) fn lower_to_validated_word(
    rtl: RtlModule,
    reference_ports: &crate::ReferencePortMap,
    runtime: &opto_runtime::ExecutionContext,
    observer: &mut dyn FnMut(crate::SynthesisProgress),
) -> Result<word::WordModule, crate::SynthError> {
    let mut module = super::lower_procedures(rtl, runtime, observer)?;
    super::lower_resolved_nets(&mut module, reference_ports)?;
    scalarize_instance_connections(&mut module)?;
    super::seal_observable_dont_cares(&mut module, reference_ports)?;
    crate::word::cycle::validate_combinational_acyclic(&module)?;
    crate::api::check::check_word_design_with_references(&module, reference_ports)
        .map_err(|error| crate::SynthError::invalid(error.to_string()))?;
    validate_operation_identities(&module)?;
    Ok(module)
}

fn scalarize_instance_connections(module: &mut word::WordModule) -> Result<(), crate::SynthError> {
    for (instance, port, value, source) in crate::word::instances::snapshot(module) {
        let width = module
            .value(value)
            .ok_or_else(|| crate::SynthError::invariant("instance connection disappeared"))?
            .ty
            .width();
        if width == 1 {
            continue;
        }
        let mut bits = Vec::with_capacity(width as usize);
        let stored = module
            .value(value)
            .ok_or_else(|| crate::SynthError::invariant("instance connection disappeared"))?
            .clone();
        for bit in (0..width).rev() {
            let bit_source = source
                .derived("instance connection bit", u64::from(bit).to_le_bytes())
                .ok_or_else(|| {
                    crate::SynthError::invariant("cannot derive instance connection bit identity")
                })?;
            let scalar = match &stored.kind {
                word::ValueKind::Signal(reference) => module.read_signal_slice(
                    reference.signal,
                    reference.lsb.checked_add(bit).ok_or_else(|| {
                        crate::SynthError::capacity("instance connection signal offset")
                    })?,
                    1,
                    bit_source,
                ),
                word::ValueKind::Constant(_) | word::ValueKind::Operation(_) => {
                    module.extract(value, bit, 1, bit_source)
                }
            }
            .map_err(crate::SynthError::from)?;
            bits.push(scalar);
        }
        let value = module
            .concat(bits, source)
            .map_err(crate::SynthError::from)?;
        let instance = word::InstId::from_index(instance).map_err(crate::SynthError::from)?;
        let port = module.name_str(port).to_string();
        module
            .set_instance_connection_value(instance, &port, value)
            .map_err(crate::SynthError::from)?;
    }
    Ok(())
}

fn validate_operation_identities(module: &word::WordModule) -> Result<(), crate::SynthError> {
    for (index, operation) in module.operations().iter().enumerate() {
        if operation.source.identity().is_none() {
            return Err(crate::SynthError::invariant(format!(
                "sealed Word operation {index} ({}) has no stable source identity",
                operation.source.construct_name().unwrap_or("generated IR")
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use opto_ir::{proc::ProcBuilder, word::SourceSpan};

    #[test]
    fn vector_instance_connections_are_sealed_as_scalar_concatenations() {
        let mut module = word::WordModule::new("vector_instance");
        let ty = word::WordType::bits(4).unwrap();
        let signal = module
            .add_wire("value", ty, SourceSpan::stable("value"))
            .unwrap();
        let value = module
            .read_signal(signal, SourceSpan::stable("read"))
            .unwrap();
        module
            .add_instance(
                "macro",
                "memory",
                vec![("data".to_string(), value, SourceSpan::stable("connection"))],
                SourceSpan::stable("instance"),
            )
            .unwrap();

        scalarize_instance_connections(&mut module).unwrap();

        let value = module.instances()[0].connections[0].value;
        let word::ValueKind::Operation(operation) = module.value(value).unwrap().kind else {
            panic!("vector connection was not scalarized");
        };
        let word::OpKind::Concat { parts } = &module.operation(operation).unwrap().kind else {
            panic!("vector connection is not a concatenation");
        };
        assert_eq!(parts.len(), 4);
        assert!(
            parts
                .iter()
                .all(|part| module.value(*part).unwrap().ty.width() == 1)
        );
    }

    #[test]
    fn sealed_frontend_rejects_combinational_feedback() {
        let mut module = word::WordModule::new("feedback");
        let bit = word::WordType::bits(1).unwrap();
        module
            .add_port(
                "unused_input",
                word::PortDirection::Input,
                bit,
                SourceSpan::default(),
            )
            .unwrap();
        module
            .add_port(
                "unused_output",
                word::PortDirection::Output,
                bit,
                SourceSpan::default(),
            )
            .unwrap();
        let left = module.add_wire("left", bit, SourceSpan::default()).unwrap();
        let right = module
            .add_wire("right", bit, SourceSpan::default())
            .unwrap();
        let left_value = module.read_signal(left, SourceSpan::default()).unwrap();
        let right_value = module.read_signal(right, SourceSpan::default()).unwrap();
        module
            .connect(
                word::LValue::signal(left),
                right_value,
                SourceSpan::default(),
            )
            .unwrap();
        module
            .connect(
                word::LValue::signal(right),
                left_value,
                SourceSpan::default(),
            )
            .unwrap();
        let rtl = RtlModule::new(module, ProcBuilder::new().seal().unwrap()).unwrap();

        let runtime = opto_runtime::ExecutionContext::default();
        let error =
            lower_to_validated_word(rtl, &crate::ReferencePortMap::new(), &runtime, &mut |_| {})
                .unwrap_err();
        assert!(matches!(error, crate::SynthError::CombinationalCycle(_)));
    }
}
