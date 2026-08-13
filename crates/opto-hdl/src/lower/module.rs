// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

#![allow(
    clippy::wildcard_imports,
    reason = "this private lowering stage consumes the parent module's internal lowering prelude; \
              the prelude is the boundary between native views and Rust-owned RTL"
)]

use super::*;

pub(crate) fn compilation(
    slang: &SlangCompilation,
    options: &FrontendOptions,
    runtime: &opto_runtime::ExecutionContext,
) -> Result<DbUpdate, HdlError> {
    if slang.module_count() == 0 {
        return Err(HdlError::invalid(
            "verilog frontend: slang produced no modules",
        ));
    }

    let top = select_top(slang, options)?;
    let tasks = slang
        .modules()
        .enumerate()
        .map(|(ordinal, module)| {
            opto_runtime::Task::new(opto_runtime::TaskKey::new(0, ordinal as u64), module)
        })
        .collect::<Vec<_>>();
    let modules = runtime.map_ordered(tasks, |module| {
        let module = module.materialize().map_err(frontend_error)?;
        lower_module(&module)
    })?;

    Ok(DbUpdate {
        modules,
        top,
        diagnostics: Vec::new(),
    })
}

fn select_top(
    slang: &SlangCompilation,
    options: &FrontendOptions,
) -> Result<Option<String>, HdlError> {
    if let Some(top) = &options.top {
        if has_module(slang, top)? {
            return Ok(Some(top.clone()));
        }
        return Err(HdlError::invalid(format!(
            "verilog frontend: top module '{top}' not found"
        )));
    }

    if let Some(top) = slang.top().map_err(frontend_error)? {
        if has_module(slang, top)? {
            return Ok(Some(top.to_string()));
        }
        return Err(HdlError::invalid(format!(
            "verilog frontend: slang selected missing top module '{top}'"
        )));
    }

    if slang.module_count() == 1 {
        let name = slang
            .modules()
            .next()
            .expect("module count was checked")
            .name()
            .map_err(frontend_error)?;
        return Ok(Some(name.to_string()));
    }

    Ok(None)
}

fn has_module(slang: &SlangCompilation, name: &str) -> Result<bool, HdlError> {
    for module in slang.modules() {
        if module.name().map_err(frontend_error)? == name {
            return Ok(true);
        }
    }
    Ok(false)
}

#[allow(
    clippy::too_many_lines,
    reason = "module publication validates and lowers ports, signals, memories, procedures, \
              continuous assignments, and annotations before exposing any partial RTL module"
)]
fn lower_module(module: &SlangMaterializedModule<'_>) -> Result<RtlModule, HdlError> {
    let module_name = module.name().map_err(frontend_error)?;
    if module_name.trim().is_empty() {
        return Err(HdlError::invalid(
            "verilog frontend: slang produced module with empty name",
        ));
    }

    let mut rtl = ModuleLowerer::new(module_name);
    lower_module_annotations(module, &mut rtl)?;
    let writes = classify_writes(module)?;
    for port in module.ports() {
        let name = port.name().map_err(frontend_error)?;
        let width = port.width();
        if width == 0 {
            return Err(HdlError::invalid(format!(
                "verilog frontend: port '{name}' has zero width"
            )));
        }
        let ty = WordType::new(width, port.is_signed(), LogicStateKind::FourState)
            .map_err(HdlError::Ir)?;
        let source = rtl.declaration_span(b"port", name.as_bytes(), "port");
        let port_id = rtl
            .add_port(
                name,
                lower_direction(port.direction().map_err(frontend_error)?),
                ty,
                source,
            )
            .map_err(HdlError::Ir)?;
        let signal = rtl
            .port(port_id)
            .expect("newly added port must exist")
            .signal;
        let layout = lower_type_layout(port.type_layout().map_err(frontend_error)?)?;
        rtl.set_signal_type_layout(signal, &layout)
            .map_err(HdlError::Ir)?;
        rtl.set_signal_resolution(
            signal,
            lower_resolution(port.resolution().map_err(frontend_error)?),
        )
        .map_err(HdlError::Ir)?;
        lower_annotations(
            port.attributes(),
            &mut rtl,
            AnnotationTarget::Port(port_id),
            "port attribute",
        )?;
    }
    for net in module.nets() {
        let name = net.name().map_err(frontend_error)?;
        let width = net.width();
        if width == 0 {
            return Err(HdlError::invalid(format!(
                "verilog frontend: net '{name}' has zero width"
            )));
        }
        let layout = net.type_layout().map_err(frontend_error)?;
        let type_layout = layout.map(lower_type_layout).transpose()?;
        let has_unpacked = type_layout
            .as_ref()
            .is_some_and(TypeLayoutSpec::contains_unpacked_array);
        let writes = writes.get(name).copied().unwrap_or_default();
        if has_unpacked && writes.is_mixed() {
            return Err(HdlError::unsupported(format!(
                "verilog frontend: unpacked storage '{name}' has mixed {} drivers",
                writes.description()
            )));
        }
        if has_unpacked && writes.contains(WriteClass::Flop) && !writes.has_multi_event_flop() {
            let Some((depth, element_width)) =
                layout.map(unpacked_memory_shape).transpose()?.flatten()
            else {
                return Err(HdlError::unsupported(
                    "verilog frontend: procedural unpacked storage must use contiguous leading unpacked dimensions",
                ));
            };
            if net.is_process_local() {
                return Err(HdlError::unsupported(
                    "verilog frontend: process-local unpacked arrays are not supported",
                ));
            }
            if lower_resolution(net.resolution().map_err(frontend_error)?)
                != SignalResolution::SingleDriver
            {
                return Err(HdlError::unsupported(
                    "verilog frontend: unpacked memory nets require single-driver resolution",
                ));
            }
            let flattened_width = element_width.checked_mul(depth.get()).ok_or_else(|| {
                HdlError::invalid("verilog frontend: unpacked memory width exceeds 32-bit capacity")
            })?;
            if flattened_width != width {
                return Err(HdlError::invalid(format!(
                    "verilog frontend: unpacked memory '{name}' has inconsistent flattened width"
                )));
            }
            let element_type = WordType::new(
                element_width,
                net.element_is_signed(),
                LogicStateKind::FourState,
            )
            .map_err(HdlError::Ir)?;
            let source = rtl.declaration_span(b"memory", name.as_bytes(), "memory");
            let memory = rtl
                .add_memory(name, element_type, depth, source)
                .map_err(HdlError::Ir)?;
            lower_annotations(
                net.attributes(),
                &mut rtl,
                AnnotationTarget::Memory(memory),
                "memory attribute",
            )?;
            continue;
        }
        let ty = WordType::new(width, net.is_signed(), LogicStateKind::FourState)
            .map_err(HdlError::Ir)?;
        let source = if net.is_process_local() {
            rtl.declaration_span(b"process-local", name.as_bytes(), "process local")
        } else {
            rtl.declaration_span(b"net", name.as_bytes(), "net")
        };
        let signal = if net.is_process_local() {
            rtl.add_process_local_signal(name, ty, source)
        } else {
            rtl.add_wire(name, ty, source)
        }
        .map_err(HdlError::Ir)?;
        if let Some(layout) = type_layout {
            rtl.set_signal_type_layout(signal, &layout)
                .map_err(HdlError::Ir)?;
        }
        rtl.set_signal_resolution(
            signal,
            lower_resolution(net.resolution().map_err(frontend_error)?),
        )
        .map_err(HdlError::Ir)?;
        lower_annotations(
            net.attributes(),
            &mut rtl,
            AnnotationTarget::Signal(signal),
            "net attribute",
        )?;
    }
    for instance in module.instances() {
        let instance_name = instance.name().map_err(frontend_error)?;
        let mut connections = Vec::with_capacity(instance.connections().len());
        for connection in instance.connections() {
            let port = connection.port().map_err(frontend_error)?;
            if port.trim().is_empty() {
                return Err(HdlError::invalid(
                    "verilog frontend: instance connection has empty port name",
                ));
            }
            let mut key = Vec::new();
            key.extend_from_slice(&(instance_name.len() as u64).to_le_bytes());
            key.extend_from_slice(instance_name.as_bytes());
            key.extend_from_slice(&(port.len() as u64).to_le_bytes());
            key.extend_from_slice(port.as_bytes());
            let path = rtl.syntax_root(b"instance-port", &key);
            let value = lower_expression(
                &mut rtl,
                connection.expression().map_err(frontend_error)?,
                path,
            )?;
            connections.push((
                port.to_string(),
                value,
                rtl.declaration_span(b"instance-connection", &key, "instance connection"),
            ));
        }
        let instance_source =
            rtl.declaration_span(b"instance", instance_name.as_bytes(), "module instance");
        let instance_id = rtl
            .add_instance(
                instance_name,
                instance.module_name().map_err(frontend_error)?,
                connections,
                instance_source,
            )
            .map_err(HdlError::Ir)?;
        lower_annotations(
            instance.attributes(),
            &mut rtl,
            AnnotationTarget::Instance(instance_id),
            "instance attribute",
        )?;
    }
    for assign in module.assigns() {
        let lhs_expression = assign.lhs().map_err(frontend_error)?;
        let key = target_identity(lhs_expression)?;
        let path = rtl.syntax_root(b"continuous-assignment", &key);
        let lhs = lower_target(&mut rtl, lhs_expression, path.child(0))?.continuous()?;
        let rhs_expression = assign.rhs().map_err(frontend_error)?;
        let source = rtl.identified_span(
            rhs_expression.source().map_err(frontend_error)?,
            "continuous assign",
            path,
        );
        let rhs = lower_expression(&mut rtl, rhs_expression, path.child(1))?;
        rtl.connect(lhs, rhs, source.clone()).map_err(|error| {
            HdlError::invalid(format!(
                "verilog frontend: module '{module_name}' continuous assignment at {}: {error}",
                source_location_text(&source)
            ))
        })?;
    }
    for procedure in module.procedures() {
        lower_procedure(&mut rtl, procedure)?;
    }
    rtl.finish()
}

fn lower_module_annotations(
    source: &SlangMaterializedModule<'_>,
    target: &mut ModuleLowerer,
) -> Result<(), HdlError> {
    let mut black_box = false;
    for annotation in source.attributes() {
        let name = annotation.name().map_err(frontend_error)?;
        if name == "blackbox" {
            black_box |= annotation_boolean(annotation, name)?;
        }
        lower_synthesis_directive(
            annotation,
            target,
            AnnotationTarget::Module,
            "module attribute",
        )?;
        lower_annotation(
            annotation,
            target,
            AnnotationTarget::Module,
            "module attribute",
        )?;
    }
    if black_box {
        target.set_definition_kind(DefinitionKind::BlackBox);
    }
    Ok(())
}

fn lower_annotations<'a>(
    annotations: impl IntoIterator<Item = SlangAttribute<'a>>,
    target: &mut ModuleLowerer,
    owner: AnnotationTarget,
    construct: &'static str,
) -> Result<(), HdlError> {
    for annotation in annotations {
        lower_synthesis_directive(annotation, target, owner, construct)?;
        lower_annotation(annotation, target, owner, construct)?;
    }
    Ok(())
}

fn lower_synthesis_directive(
    annotation: SlangAttribute<'_>,
    target: &mut ModuleLowerer,
    owner: AnnotationTarget,
    construct: &'static str,
) -> Result<(), HdlError> {
    let name = annotation.name().map_err(frontend_error)?;
    let kind = match (name, owner) {
        (
            "dont_touch",
            AnnotationTarget::Module | AnnotationTarget::Signal(_) | AnnotationTarget::Instance(_),
        ) => SynthesisDirectiveKind::DontTouch,
        ("keep_hierarchy", AnnotationTarget::Module | AnnotationTarget::Instance(_)) => {
            SynthesisDirectiveKind::Ungroup
        }
        ("keep", AnnotationTarget::Signal(_)) => SynthesisDirectiveKind::KeepSignal,
        ("async_reg", AnnotationTarget::Signal(_)) => SynthesisDirectiveKind::AsyncRegister,
        _ => return Ok(()),
    };
    let mut enabled = annotation_boolean(annotation, name)?;
    if kind == SynthesisDirectiveKind::Ungroup {
        enabled = !enabled;
    }
    let source = annotation.source().map_err(frontend_error)?;
    let source = target.source_span(source, construct);
    target
        .set_synthesis_directive(owner, kind, enabled, source)
        .map_err(HdlError::Ir)
}

fn annotation_boolean(annotation: SlangAttribute<'_>, name: &str) -> Result<bool, HdlError> {
    match annotation.value().map_err(frontend_error)? {
        SlangAttributeValue::Integer { bits, .. } => {
            if !bits.bytes().all(|bit| matches!(bit, b'0' | b'1')) {
                return Err(HdlError::invalid(format!(
                    "verilog frontend: synthesis attribute '{name}' has an unknown integer bit"
                )));
            }
            Ok(bits.bytes().any(|bit| bit == b'1'))
        }
        SlangAttributeValue::String(value) | SlangAttributeValue::Other(value) => {
            match value.trim().to_ascii_lowercase().as_str() {
                "1" | "true" | "yes" | "on" => Ok(true),
                "0" | "false" | "no" | "off" => Ok(false),
                _ => Err(HdlError::invalid(format!(
                    "verilog frontend: synthesis attribute '{name}' expects a boolean value"
                ))),
            }
        }
    }
}

fn lower_annotation(
    annotation: SlangAttribute<'_>,
    target: &mut ModuleLowerer,
    owner: AnnotationTarget,
    construct: &'static str,
) -> Result<(), HdlError> {
    let name = annotation.name().map_err(frontend_error)?;
    let value = match annotation.value().map_err(frontend_error)? {
        SlangAttributeValue::Integer {
            bits,
            width,
            signed,
        } => {
            let bits = ConstBits::from_bin_str(bits).map_err(HdlError::Constant)?;
            if bits.width() != width {
                return Err(HdlError::invalid(format!(
                    "verilog frontend: attribute '{name}' stores {} bits but reports width {width}",
                    bits.width()
                )));
            }
            AnnotationValueSpec::Integer { bits, signed }
        }
        SlangAttributeValue::String(value) => AnnotationValueSpec::String(value.to_string()),
        SlangAttributeValue::Other(value) => AnnotationValueSpec::Other(value.to_string()),
    };
    let source = annotation.source().map_err(frontend_error)?;
    let source = target.source_span(source, construct);
    target
        .add_annotation(owner, name, value, source)
        .map_err(HdlError::Ir)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum WriteClass {
    Continuous,
    Combinational,
    Flop,
}

impl WriteClass {
    const fn bit(self) -> u8 {
        match self {
            Self::Continuous => 1,
            Self::Combinational => 2,
            Self::Flop => 4,
        }
    }
}

#[derive(Clone, Copy, Default)]
struct WriteClasses {
    classes: u8,
    multi_event_flop: bool,
}

impl WriteClasses {
    fn insert(&mut self, class: WriteClass) {
        self.classes |= class.bit();
    }

    fn insert_flop(&mut self, multi_event: bool) {
        self.insert(WriteClass::Flop);
        self.multi_event_flop |= multi_event;
    }

    const fn contains(self, class: WriteClass) -> bool {
        self.classes & class.bit() != 0
    }

    const fn has_multi_event_flop(self) -> bool {
        self.multi_event_flop
    }

    const fn is_mixed(self) -> bool {
        self.classes.count_ones() > 1
    }

    fn description(self) -> String {
        [
            self.contains(WriteClass::Continuous)
                .then_some("continuous"),
            self.contains(WriteClass::Combinational)
                .then_some("combinational/latch procedural"),
            self.contains(WriteClass::Flop)
                .then_some("edge-triggered procedural"),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" and ")
    }
}

fn classify_writes<'a>(
    module: &'a SlangMaterializedModule<'_>,
) -> Result<HashMap<&'a str, WriteClasses>, HdlError> {
    let mut writes: HashMap<&str, WriteClasses> = HashMap::new();
    for assignment in module.assigns() {
        if let Some(name) = assignment_signal_name(assignment.lhs().map_err(frontend_error)?)? {
            writes
                .entry(name)
                .or_default()
                .insert(WriteClass::Continuous);
        }
    }

    for procedure in module.procedures() {
        let kind = procedure.kind().map_err(frontend_error)?;
        let class = match kind {
            SlangProcedureKind::Flop => WriteClass::Flop,
            SlangProcedureKind::Comb
            | SlangProcedureKind::Latch
            | SlangProcedureKind::CombOrLatch => WriteClass::Combinational,
        };
        let multi_event_flop = kind == SlangProcedureKind::Flop && procedure.events().len() > 1;
        for block in procedure.blocks() {
            for effect in block.effects() {
                if let Some(name) = assignment_signal_name(effect.lhs().map_err(frontend_error)?)? {
                    let writes = writes.entry(name).or_default();
                    if class == WriteClass::Flop {
                        writes.insert_flop(multi_event_flop);
                    } else {
                        writes.insert(class);
                    }
                }
            }
        }
    }
    Ok(writes)
}

fn assignment_signal_name(expression: SlangExpression<'_>) -> Result<Option<&str>, HdlError> {
    match expression.kind().map_err(frontend_error)? {
        SlangExpressionKind::Signal(signal) => Ok(Some(signal.name)),
        SlangExpressionKind::DynamicExtract { value, .. } => assignment_signal_name(value),
        _ => Ok(None),
    }
}

fn lower_type_layout(layout: SlangTypeLayout<'_>) -> Result<TypeLayoutSpec, HdlError> {
    match layout.kind().map_err(frontend_error)? {
        SlangTypeLayoutKind::Scalar => Ok(TypeLayoutSpec::Scalar),
        SlangTypeLayoutKind::Array => {
            let range = layout.array_range().map_err(frontend_error)?;
            let kind = match layout.array_kind().map_err(frontend_error)? {
                SlangArrayKind::Packed => ArrayKind::Packed,
                SlangArrayKind::Unpacked => ArrayKind::Unpacked,
            };
            let range = match kind {
                ArrayKind::Packed => IndexRange {
                    left: range.left,
                    right: range.right,
                },
                ArrayKind::Unpacked => IndexRange {
                    left: range.right,
                    right: range.left,
                },
            };
            Ok(TypeLayoutSpec::Array {
                kind,
                range,
                element: Box::new(lower_type_layout(
                    layout.array_element().map_err(frontend_error)?,
                )?),
            })
        }
        SlangTypeLayoutKind::Struct => Ok(TypeLayoutSpec::Struct {
            fields: layout
                .fields()
                .map_err(frontend_error)?
                .map(|field| {
                    Ok(TypeLayoutFieldSpec {
                        name: field.name().map_err(frontend_error)?.to_string(),
                        bit_offset: field.bit_offset(),
                        layout: lower_type_layout(field.layout().map_err(frontend_error)?)?,
                    })
                })
                .collect::<Result<Vec<_>, HdlError>>()?,
        }),
    }
}

fn unpacked_memory_shape(
    mut layout: SlangTypeLayout<'_>,
) -> Result<Option<(NonZeroU32, u32)>, HdlError> {
    let mut depth = 1u32;
    let mut unpacked = false;
    while layout.kind().map_err(frontend_error)? == SlangTypeLayoutKind::Array
        && layout.array_kind().map_err(frontend_error)? == SlangArrayKind::Unpacked
    {
        let range = layout.array_range().map_err(frontend_error)?;
        let dimension = range
            .left
            .abs_diff(range.right)
            .checked_add(1)
            .ok_or_else(|| {
                HdlError::invalid("verilog frontend: unpacked memory depth exceeds 32-bit capacity")
            })?;
        depth = depth.checked_mul(dimension).ok_or_else(|| {
            HdlError::invalid("verilog frontend: unpacked memory depth exceeds 32-bit capacity")
        })?;
        layout = layout.array_element().map_err(frontend_error)?;
        unpacked = true;
    }
    if !unpacked {
        return Ok(None);
    }
    let element = lower_type_layout(layout)?;
    if element.contains_unpacked_array() {
        return Err(HdlError::unsupported(
            "verilog frontend: unpacked memory dimensions must be contiguous",
        ));
    }
    Ok(Some((
        NonZeroU32::new(depth).expect("an unpacked dimension has nonzero depth"),
        element.width().map_err(HdlError::Ir)?,
    )))
}

fn lower_direction(direction: SlangPortDirection) -> PortDirection {
    match direction {
        SlangPortDirection::Input => PortDirection::Input,
        SlangPortDirection::Output => PortDirection::Output,
        SlangPortDirection::Inout => PortDirection::Inout,
    }
}

fn lower_resolution(resolution: SlangNetResolution) -> SignalResolution {
    match resolution {
        SlangNetResolution::SingleDriver => SignalResolution::SingleDriver,
        SlangNetResolution::WiredAnd => SignalResolution::WiredAnd,
        SlangNetResolution::WiredOr => SignalResolution::WiredOr,
    }
}
