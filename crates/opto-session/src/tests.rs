// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

mod formats;
mod frontend;
mod incremental;
mod libraries;
mod mapping;
mod objects;
mod parasitics;
mod power;
mod state;
mod timing;

use super::*;
use opto_db::{Cell, DesignIndex, Direction, Port};
use opto_ir::ConstBits;
use opto_ir::rtl::RtlModule;
use opto_ir::word::{
    BinaryOp, LValue, LogicStateKind, PortDirection, SourceSpan, UnaryOp, WordModule, WordType,
};
use opto_library::library_source_names;

fn rtl(module: WordModule) -> RtlModule {
    RtlModule::structural(module).unwrap()
}

fn empty_rtl_module(name: &str) -> RtlModule {
    rtl(WordModule::new(name))
}

fn install_test_mapping_library(session: &mut Session) {
    let library = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../qualification/libraries/opto_test.lib");
    session.set_lib_search_path(vec![library.parent().unwrap().to_path_buf()]);
    session.read_libs(&[library]).unwrap();
}

/// Builds the Word source a design index describes.
///
/// A checkpoint stores the source and rederives the object index from it, so a
/// fixture whose index names ports its source does not have is not a design any
/// installation can produce. Mirroring the ports keeps the fixture realistic.
fn rtl_module_for(design: &DesignIndex) -> RtlModule {
    let mut module = WordModule::new(&design.name);
    for port in &design.ports {
        let direction = match port.direction {
            Direction::Input => opto_ir::word::PortDirection::Input,
            Direction::Output => opto_ir::word::PortDirection::Output,
            Direction::Inout => opto_ir::word::PortDirection::Inout,
        };
        module
            .add_port(
                design.name_str(port.name),
                direction,
                opto_ir::word::WordType::bits(port.width.max(1))
                    .expect("test port width is representable"),
                opto_ir::word::SourceSpan::default(),
            )
            .expect("test design port is unique");
    }
    rtl(module)
}

fn install_test_design(session: &mut Session, mut design: DesignIndex) {
    let signal_names = design
        .nets
        .iter()
        .map(|net| net.name)
        .chain(
            design
                .cells
                .iter()
                .flat_map(|cell| cell.connections.iter())
                .flat_map(|connection| connection.signals.iter().copied()),
        )
        .collect::<Vec<_>>();
    for name in signal_names {
        if !design.used_signals.contains(&name) {
            design.used_signals.push(name);
        }
    }
    let source = rtl_module_for(&design);
    session
        .install_design_fresh(source, RevisionId::INITIAL, design)
        .unwrap();
}

fn design_names(session: &Session) -> Vec<String> {
    session.state.designs.keys().cloned().collect()
}

fn rtl_module_with_instance(name: &str, reference: &str) -> RtlModule {
    let mut module = WordModule::new(name);
    module
        .add_instance("u0", reference, Vec::new(), SourceSpan::default())
        .unwrap();
    rtl(module)
}

fn hierarchy_leaf(name: &str, width: u32, invert: bool) -> RtlModule {
    let mut module = WordModule::new(name);
    let ty = WordType::new(width, false, LogicStateKind::FourState).unwrap();
    let a = module
        .add_port("a", PortDirection::Input, ty, SourceSpan::default())
        .unwrap();
    let y = module
        .add_port("y", PortDirection::Output, ty, SourceSpan::default())
        .unwrap();
    let mut value = module
        .read_signal(module.port(a).unwrap().signal, SourceSpan::default())
        .unwrap();
    if invert {
        value = module
            .unary(
                UnaryOp::BitNot,
                value,
                SourceSpan::stable("session-tests/hierarchy-leaf/invert"),
            )
            .unwrap();
    }
    module
        .connect(
            LValue::signal(module.port(y).unwrap().signal),
            value,
            SourceSpan::default(),
        )
        .unwrap();
    rtl(module)
}

fn hierarchy_parent(name: &str, width: u32, children: &[(&str, &str)]) -> RtlModule {
    let mut module = WordModule::new(name);
    let ty = WordType::new(width, false, LogicStateKind::FourState).unwrap();
    let a = module
        .add_port("a", PortDirection::Input, ty, SourceSpan::default())
        .unwrap();
    let y = module
        .add_port("y", PortDirection::Output, ty, SourceSpan::default())
        .unwrap();
    let a_value = module
        .read_signal(module.port(a).unwrap().signal, SourceSpan::default())
        .unwrap();
    let y_value = module
        .read_signal(module.port(y).unwrap().signal, SourceSpan::default())
        .unwrap();
    let mut first_child_output = None;
    for &(instance, reference) in children {
        let output = if children.len() == 1 {
            y_value
        } else {
            let signal = module
                .add_wire(
                    format!("{instance}_y"),
                    ty,
                    SourceSpan::construct("child output"),
                )
                .unwrap();
            let value = module.read_signal(signal, SourceSpan::default()).unwrap();
            first_child_output.get_or_insert(value);
            value
        };
        module
            .add_instance(
                instance,
                reference,
                vec![
                    ("a".to_string(), a_value, SourceSpan::default()),
                    ("y".to_string(), output, SourceSpan::default()),
                ],
                SourceSpan::default(),
            )
            .unwrap();
    }
    if let Some(value) = first_child_output {
        module
            .connect(
                LValue::signal(module.port(y).unwrap().signal),
                value,
                SourceSpan::construct("parent output"),
            )
            .unwrap();
    }
    rtl(module)
}

fn independent_mapping_cones(left_operation: BinaryOp) -> RtlModule {
    let mut module = WordModule::new("top");
    let ty = WordType::new(1, false, LogicStateKind::FourState).unwrap();
    let ports = ["a", "b", "c", "d"]
        .into_iter()
        .map(|name| {
            module
                .add_port(name, PortDirection::Input, ty, SourceSpan::default())
                .unwrap()
        })
        .collect::<Vec<_>>();
    let y_left = module
        .add_port("y_left", PortDirection::Output, ty, SourceSpan::default())
        .unwrap();
    let y_right = module
        .add_port("y_right", PortDirection::Output, ty, SourceSpan::default())
        .unwrap();
    let values = ports
        .into_iter()
        .map(|port| {
            module
                .read_signal(module.port(port).unwrap().signal, SourceSpan::default())
                .unwrap()
        })
        .collect::<Vec<_>>();
    let left = module
        .binary(
            left_operation,
            values[0],
            values[1],
            SourceSpan::stable("session-tests/independent-cones/left"),
        )
        .unwrap();
    let right = module
        .binary(
            BinaryOp::BitXor,
            values[2],
            values[3],
            SourceSpan::stable("session-tests/independent-cones/right"),
        )
        .unwrap();
    for (port, value) in [(y_left, left), (y_right, right)] {
        module
            .connect(
                LValue::signal(module.port(port).unwrap().signal),
                value,
                SourceSpan::default(),
            )
            .unwrap();
    }
    rtl(module)
}

fn unsupported_tri_state_leaf(name: &str) -> RtlModule {
    let mut module = WordModule::new(name);
    let ty = WordType::new(1, false, LogicStateKind::FourState).unwrap();
    module
        .add_port("a", PortDirection::Input, ty, SourceSpan::default())
        .unwrap();
    let y = module
        .add_port("y", PortDirection::Output, ty, SourceSpan::default())
        .unwrap();
    let value = module
        .constant(
            ConstBits::from_bin_str("z").unwrap(),
            ty,
            SourceSpan::default(),
        )
        .unwrap();
    module
        .connect(
            LValue::signal(module.port(y).unwrap().signal),
            value,
            SourceSpan::default(),
        )
        .unwrap();
    rtl(module)
}

static TEST_PATH_SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

#[derive(Debug)]
struct TestPath(PathBuf);

impl TestPath {
    fn new(name: &str) -> Self {
        let sequence = TEST_PATH_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Self(std::env::temp_dir().join(format!(
            "{}-{}-{sequence}-{name}",
            env!("CARGO_PKG_NAME"),
            std::process::id(),
        )))
    }
}

impl std::ops::Deref for TestPath {
    type Target = PathBuf;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl AsRef<std::path::Path> for TestPath {
    fn as_ref(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for TestPath {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn temp_file(name: &str) -> TestPath {
    TestPath::new(name)
}

fn temp_dir(name: &str) -> TestPath {
    let path = temp_file(name);
    std::fs::create_dir_all(&path).unwrap();
    path
}
