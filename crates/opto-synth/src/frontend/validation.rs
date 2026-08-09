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
    crate::word::cycle::validate_combinational_acyclic(&module)?;
    crate::api::check::check_design_with_references(&module, reference_ports)
        .map_err(|error| crate::SynthError::invalid(error.to_string()))?;
    validate_operation_identities(&module)?;
    Ok(module)
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
