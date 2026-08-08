// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use std::collections::BTreeSet;
use std::sync::Arc;

fn mapped_verilog_text(netlist: &opto_ir::mapped::MappedNetlist) -> String {
    let mut output = Vec::new();
    opto_formats::write_mapped_verilog(&mut output, netlist).unwrap();
    String::from_utf8(output).expect("Verilog writer only emits UTF-8 text")
}

fn artifact_revision(session: &Session, design: &str) -> RevisionId {
    session
        .state
        .designs
        .get(design)
        .and_then(|record| record.synthesis_binding.as_ref())
        .map(|binding| binding.published_revision)
        .expect("synthesized design has a published artifact binding")
}

#[test]
fn hierarchical_synthesis_publishes_one_flat_root_artifact_and_reuses_it() {
    let child = hierarchy_leaf("child", 2, true);
    let top = hierarchy_parent("top", 2, &[("u_child", "child")]);
    let mut session = Session::new();
    install_test_mapping_library(&mut session);
    session
        .apply_db_update(
            DbUpdate {
                modules: vec![top, child],
                top: Some("top".to_string()),
            },
            CurrentDesignPolicy::ElaboratedTop,
        )
        .unwrap();

    let mut first_events = Vec::new();
    session
        .synthesize_observed(SynthesisEffort::Medium, &mut |event| {
            first_events.push(event);
        })
        .unwrap();
    assert!(matches!(
        first_events.first(),
        Some(SynthesisEvent::Started {
            design,
            ..
        }) if design == "top"
    ));
    assert!(matches!(
        &first_events[1],
        SynthesisEvent::ArtifactCompleted { design, metrics }
            if design == "top"
                && metrics.source_change.changed_operations
                    == metrics.source_change.operations
    ));
    assert!(matches!(
        &first_events[2],
        SynthesisEvent::DesignInformationUpdateStarted { design, .. } if design == "top"
    ));
    assert_eq!(
        &first_events[3],
        &SynthesisEvent::Completed {
            design: "top".to_string(),
            synthesized: true,
        }
    );
    let synthesized_revision = session.revision();
    let completed_work = {
        let metrics = session.process.runtime.metrics();
        (metrics.completed_task_callbacks, metrics.completed_batches)
    };
    let top_mapped = session
        .state
        .designs
        .get("top")
        .unwrap()
        .synthesized
        .as_ref()
        .unwrap()
        .mapped();
    assert_eq!(top_mapped.cell_count(), 2);
    assert_eq!(top_mapped.design_instance_count(), 0);
    assert!(
        session
            .state
            .designs
            .get("top")
            .unwrap()
            .mapped_object_index
            .is_some()
    );
    assert!(
        session
            .state
            .designs
            .get("child")
            .unwrap()
            .synthesized
            .is_none()
    );

    let mut reused_events = Vec::new();
    session
        .synthesize_observed(SynthesisEffort::Medium, &mut |event| {
            reused_events.push(event);
        })
        .unwrap();
    assert_eq!(
        reused_events,
        [SynthesisEvent::Completed {
            design: "top".to_string(),
            synthesized: false,
        }]
    );
    assert_eq!(session.revision(), synthesized_revision);
    let metrics = session.process.runtime.metrics();
    assert_eq!(
        (metrics.completed_task_callbacks, metrics.completed_batches),
        completed_work
    );

    let path = temp_file("synthesized-hierarchy.v");
    session
        .write_hdl_file(Some(path.clone()), &[], true)
        .unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::remove_file(path).unwrap();
    assert!(text.contains("module top"));
    assert_eq!(text.matches("module ").count(), 1);
    assert!(!text.contains("child u_child"));
}

#[test]
fn high_effort_synthesis_uses_the_canonical_flat_root_artifact() {
    let dir = temp_dir("high-effort-synthesis-root");
    let lib_path = dir.join("demo.lib");
    std::fs::write(
        &lib_path,
        r#"
library (demo) {
  cell (INV) {
    area : 1.0;
    pin (A) { direction : input; }
    pin (Y) {
      direction : output;
      function : "!A";
      timing () {
        related_pin : "A";
        timing_sense : negative_unate;
        cell_rise (t) { values ( "0.2" ); }
        cell_fall (t) { values ( "0.2" ); }
      }
    }
  }
}
"#,
    )
    .unwrap();
    let leaf = hierarchy_leaf("leaf", 2, true);
    let middle = hierarchy_parent("middle", 2, &[("u_leaf", "leaf")]);
    let top = hierarchy_parent("top", 2, &[("u_middle", "middle")]);
    let mut session = Session::new();
    session.set_lib_search_path(vec![PathBuf::from(dir.display().to_string())]);
    session.read_libs(&[PathBuf::from("demo.lib")]).unwrap();
    session
        .apply_db_update(
            DbUpdate {
                modules: vec![top, middle, leaf],
                top: Some("top".to_string()),
            },
            CurrentDesignPolicy::ElaboratedTop,
        )
        .unwrap();

    let mut events = Vec::new();
    session
        .synthesize_observed(SynthesisEffort::High, &mut |event| events.push(event))
        .unwrap();
    assert!(matches!(
        events.first(),
        Some(SynthesisEvent::Started {
            design,
            ..
        }) if design == "top"
    ));
    assert!(matches!(
        &events[1],
        SynthesisEvent::ArtifactCompleted { design, .. } if design == "top"
    ));
    assert_eq!(
        events.last(),
        Some(&SynthesisEvent::Completed {
            design: "top".to_string(),
            synthesized: true,
        })
    );
    assert!(
        session
            .state
            .designs
            .get("middle")
            .unwrap()
            .synthesized
            .is_none()
    );
    assert!(
        session
            .state
            .designs
            .get("leaf")
            .unwrap()
            .synthesized
            .is_none()
    );

    let mapped = session
        .state
        .designs
        .get("top")
        .unwrap()
        .synthesized
        .as_ref()
        .unwrap()
        .mapped();
    assert_eq!(mapped.design_instance_count(), 0);
    assert_eq!(mapped.cell_count(), 2);
    let area = session.report_area().unwrap();
    assert!(area.contains("Number of cells: 2"));
    assert!(area.contains("Total cell area: 2.000000"));
    let timing = session
        .report_timing(&ReportTimingOptions {
            from: vec!["a[0]".to_string()],
            to: vec!["y[0]".to_string()],
            ..ReportTimingOptions::default()
        })
        .unwrap();
    assert!(timing.contains("0.200"), "{timing}");
    let qor = session.report_qor().unwrap();
    assert!(qor.contains("Timing paths: 0"), "{qor}");
    assert!(!qor.contains("Critical Path Length:"), "{qor}");
    let netlist = dir.join("flat.v");
    session
        .write_hdl_file(Some(netlist.clone()), &[], true)
        .unwrap();
    let netlist = std::fs::read_to_string(netlist).unwrap();
    assert_eq!(netlist.matches("module ").count(), 1);

    let mut cached_events = Vec::new();
    session
        .synthesize_observed(SynthesisEffort::High, &mut |event| {
            cached_events.push(event);
        })
        .unwrap();
    assert_eq!(
        cached_events,
        [SynthesisEvent::Completed {
            design: "top".to_string(),
            synthesized: false,
        }]
    );

    session
        .apply_db_update(
            DbUpdate {
                modules: vec![hierarchy_leaf("leaf", 2, false)],
                top: Some("top".to_string()),
            },
            CurrentDesignPolicy::ElaboratedTop,
        )
        .unwrap();
    let mut changed_events = Vec::new();
    session
        .synthesize_observed(SynthesisEffort::High, &mut |event| {
            changed_events.push(event);
        })
        .unwrap();
    assert!(matches!(
        changed_events.get(1),
        Some(SynthesisEvent::ArtifactCompleted { design, .. }) if design == "top"
    ));
    assert_eq!(
        session
            .state
            .designs
            .get("top")
            .unwrap()
            .synthesized
            .as_ref()
            .unwrap()
            .mapped()
            .cell_count(),
        0
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn switching_effort_resynthesizes_the_same_canonical_root_artifact() {
    let leaf = hierarchy_leaf("leaf", 2, true);
    let top = hierarchy_parent("top", 2, &[("u_leaf", "leaf")]);
    let mut session = Session::new();
    install_test_mapping_library(&mut session);
    session
        .apply_db_update(
            DbUpdate {
                modules: vec![top, leaf],
                top: Some("top".to_string()),
            },
            CurrentDesignPolicy::ElaboratedTop,
        )
        .unwrap();
    session.synthesize().unwrap();

    for effort in [SynthesisEffort::High, SynthesisEffort::Medium] {
        let mut events = Vec::new();
        session
            .synthesize_observed(effort, &mut |event| events.push(event))
            .unwrap();
        let artifact = events
            .iter()
            .find_map(|event| match event {
                SynthesisEvent::ArtifactCompleted { design, metrics } if design == "top" => {
                    Some(metrics)
                }
                _ => None,
            })
            .expect("effort switch must resynthesis the root");
        assert!(artifact.source_change.changed_operations <= artifact.source_change.operations);
        assert_eq!(
            session
                .state
                .designs
                .get("top")
                .unwrap()
                .synthesis_binding
                .as_ref()
                .unwrap()
                .content_key
                .effort,
            effort
        );
    }
}

#[test]
fn timing_uses_each_designs_canonical_root_artifact() {
    let leaf = hierarchy_leaf("leaf", 1, true);
    let middle = hierarchy_parent("middle", 1, &[("u_leaf", "leaf")]);
    let top = hierarchy_parent("top", 1, &[("u_middle", "middle")]);
    let mut session = Session::new();
    install_test_mapping_library(&mut session);
    session
        .apply_db_update(
            DbUpdate {
                modules: vec![top, middle, leaf],
                top: Some("top".to_string()),
            },
            CurrentDesignPolicy::ElaboratedTop,
        )
        .unwrap();
    session.synthesize().unwrap();
    session.set_current_design("middle").unwrap();
    session
        .synthesize_observed(SynthesisEffort::High, &mut |_| {})
        .unwrap();
    session.set_current_design("top").unwrap();

    assert_eq!(
        session
            .state
            .designs
            .get("top")
            .unwrap()
            .synthesis_binding
            .as_ref()
            .unwrap()
            .content_key
            .effort,
        SynthesisEffort::Medium
    );
    assert_eq!(
        session
            .state
            .designs
            .get("middle")
            .unwrap()
            .synthesis_binding
            .as_ref()
            .unwrap()
            .content_key
            .effort,
        SynthesisEffort::High
    );
    assert!(
        session
            .state
            .designs
            .get("leaf")
            .unwrap()
            .synthesized
            .is_none()
    );
}

#[test]
fn changing_a_leaf_body_resynthesizes_the_root_artifact() {
    let left = hierarchy_leaf("left", 1, false);
    let right = hierarchy_leaf("right", 1, false);
    let top = hierarchy_parent("top", 1, &[("u_left", "left"), ("u_right", "right")]);
    let mut session = Session::new();
    install_test_mapping_library(&mut session);
    session
        .apply_db_update(
            DbUpdate {
                modules: vec![top, left, right],
                top: Some("top".to_string()),
            },
            CurrentDesignPolicy::ElaboratedTop,
        )
        .unwrap();
    session.synthesize().unwrap();
    let original_top = artifact_revision(&session, "top");

    session
        .apply_db_update(
            DbUpdate {
                modules: vec![hierarchy_leaf("left", 1, true)],
                top: Some("top".to_string()),
            },
            CurrentDesignPolicy::ElaboratedTop,
        )
        .unwrap();
    let mut events = Vec::new();
    session
        .synthesize_observed(SynthesisEffort::Medium, &mut |event| events.push(event))
        .unwrap();
    assert!(matches!(
        events.first(),
        Some(SynthesisEvent::Started { .. })
    ));
    assert!(
        matches!(
            &events[1],
            SynthesisEvent::ArtifactCompleted { design, metrics }
                if design == "top"
                    && metrics.source_change.changed_values > 0
                    && metrics.source_change.changed_operations == 1
                    && metrics.source_change.changed_boundaries > 0
        ),
        "{events:#?}"
    );
    assert!(matches!(
        &events[2],
        SynthesisEvent::DesignInformationUpdateStarted { design, .. } if design == "top"
    ));
    assert_eq!(
        &events[3],
        &SynthesisEvent::Completed {
            design: "top".to_string(),
            synthesized: true,
        }
    );

    assert_ne!(artifact_revision(&session, "top"), original_top);
    assert!(
        session
            .state
            .designs
            .get("left")
            .unwrap()
            .synthesized
            .is_none()
    );
    assert!(
        session
            .state
            .designs
            .get("right")
            .unwrap()
            .synthesized
            .is_none()
    );
}

#[test]
fn changing_a_child_interface_resynthesizes_the_root_artifact() {
    let top = hierarchy_parent("top", 1, &[("u_middle", "middle")]);
    let middle = hierarchy_parent("middle", 1, &[("u_leaf", "leaf")]);
    let leaf = hierarchy_leaf("leaf", 1, false);
    let mut session = Session::new();
    install_test_mapping_library(&mut session);
    session
        .apply_db_update(
            DbUpdate {
                modules: vec![top, middle, leaf],
                top: Some("top".to_string()),
            },
            CurrentDesignPolicy::ElaboratedTop,
        )
        .unwrap();
    session.synthesize().unwrap();
    let original_top = artifact_revision(&session, "top");

    let (mut changed_leaf, procedures) = hierarchy_leaf("leaf", 1, false).into_parts();
    changed_leaf
        .add_port(
            "unused",
            PortDirection::Input,
            WordType::new(1, false, LogicStateKind::FourState).unwrap(),
            SourceSpan::default(),
        )
        .unwrap();
    let changed_leaf = RtlModule::new(changed_leaf, procedures).unwrap();
    session
        .apply_db_update(
            DbUpdate {
                modules: vec![changed_leaf],
                top: Some("top".to_string()),
            },
            CurrentDesignPolicy::ElaboratedTop,
        )
        .unwrap();

    let mut events = Vec::new();
    session
        .synthesize_observed(SynthesisEffort::Medium, &mut |event| events.push(event))
        .unwrap();
    assert!(matches!(
        events.as_slice(),
        [
            SynthesisEvent::Started { .. },
            SynthesisEvent::ArtifactCompleted { design: root, .. },
            SynthesisEvent::DesignInformationUpdateStarted { design: current, .. },
            SynthesisEvent::Completed {
                synthesized: true,
                ..
            }
        ] if root == "top" && current == "top"
    ));
    assert_ne!(artifact_revision(&session, "top"), original_top);
    assert!(
        session
            .state
            .designs
            .get("middle")
            .unwrap()
            .synthesized
            .is_none()
    );
    assert!(
        session
            .state
            .designs
            .get("leaf")
            .unwrap()
            .synthesized
            .is_none()
    );
}

#[test]
fn source_update_reports_changed_components() {
    let dir = temp_dir("incremental-mapping-components");
    std::fs::write(
        dir.join("demo.lib"),
        r#"
library (demo) {
  cell (AND2) {
    area : 2.0;
    pin (A) { direction : input; }
    pin (B) { direction : input; }
    pin (Y) { direction : output; function : "A B"; }
  }
  cell (OR2) {
    area : 2.0;
    pin (A) { direction : input; }
    pin (B) { direction : input; }
    pin (Y) { direction : output; function : "A+B"; }
  }
  cell (XOR2) {
    area : 3.0;
    pin (A) { direction : input; }
    pin (B) { direction : input; }
    pin (Y) { direction : output; function : "A^B"; }
  }
  cell (INV) {
    area : 1.0;
    pin (A) { direction : input; }
    pin (Y) { direction : output; function : "!A"; }
  }
}

"#,
    )
    .unwrap();
    let mut session = Session::new();
    session.set_lib_search_path(vec![PathBuf::from(dir.display().to_string())]);
    session.read_libs(&[PathBuf::from("demo.lib")]).unwrap();
    session
        .apply_db_update(
            DbUpdate {
                modules: vec![independent_mapping_cones(BinaryOp::BitAnd)],
                top: Some("top".to_string()),
            },
            CurrentDesignPolicy::ElaboratedTop,
        )
        .unwrap();
    session.synthesize().unwrap();

    session
        .apply_db_update(
            DbUpdate {
                modules: vec![independent_mapping_cones(BinaryOp::BitOr)],
                top: Some("top".to_string()),
            },
            CurrentDesignPolicy::ElaboratedTop,
        )
        .unwrap();
    let mut events = Vec::new();
    session
        .synthesize_observed(SynthesisEffort::Medium, &mut |event| events.push(event))
        .unwrap();

    assert!(
        matches!(
            &events[1],
            SynthesisEvent::ArtifactCompleted { design, metrics }
                // The source graph still identifies the untouched cone and
                // reuses its Boolean recipes. Both cones intentionally share
                // one small-design synthesis region, so changing either cone
                // rebuilds that single regional decision.
                if design == "top"
                    && metrics.source_change.changed_operations == 1
                    && metrics.source_change.reused_regions >= 1
                    && metrics.boolean_recipe_hits > 0
                    && metrics.synthesis_regions == 1
                    && metrics.regional_decision_hits == 0
                    && metrics.regional_decision_misses == 1
        ),
        "{events:#?}"
    );

    let warm = session
        .state
        .designs
        .get("top")
        .unwrap()
        .synthesized
        .as_ref()
        .unwrap();
    let warm_verilog = mapped_verilog_text(warm.mapped());
    let warm_provenance = opto_archive::to_bytes(warm.implementation_db()).unwrap();
    let warm_report = warm.report().clone();

    let mut cold = Session::new();
    cold.set_lib_search_path(vec![PathBuf::from(dir.display().to_string())]);
    cold.read_libs(&[PathBuf::from("demo.lib")]).unwrap();
    cold.apply_db_update(
        DbUpdate {
            modules: vec![independent_mapping_cones(BinaryOp::BitOr)],
            top: Some("top".to_string()),
        },
        CurrentDesignPolicy::ElaboratedTop,
    )
    .unwrap();
    cold.synthesize().unwrap();
    let cold = cold
        .state
        .designs
        .get("top")
        .unwrap()
        .synthesized
        .as_ref()
        .unwrap();
    assert_eq!(mapped_verilog_text(cold.mapped()), warm_verilog);
    assert_eq!(
        opto_archive::to_bytes(cold.implementation_db()).unwrap(),
        warm_provenance
    );
    assert_eq!(cold.report().design, warm_report.design);
    assert_eq!(cold.report().ports, warm_report.ports);
    assert_eq!(cold.report().cells, warm_report.cells);
    assert_eq!(cold.report().nets, warm_report.nets);
    assert_eq!(
        cold.report().total_cell_area.to_bits(),
        warm_report.total_cell_area.to_bits()
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn checkpoint_restores_cross_process_synthesis_state() {
    let dir = temp_dir("cross-process-incremental-checkpoint");
    std::fs::write(
        dir.join("demo.lib"),
        r#"
library (demo) {
  cell (AND2) {
    area : 2.0;
    pin (A) { direction : input; }
    pin (B) { direction : input; }
    pin (Y) { direction : output; function : "A B"; }
  }
  cell (OR2) {
    area : 2.0;
    pin (A) { direction : input; }
    pin (B) { direction : input; }
    pin (Y) { direction : output; function : "A+B"; }
  }
  cell (XOR2) {
    area : 3.0;
    pin (A) { direction : input; }
    pin (B) { direction : input; }
    pin (Y) { direction : output; function : "A^B"; }
  }
}
"#,
    )
    .unwrap();
    let configure = |session: &mut Session| {
        session.set_lib_search_path(vec![PathBuf::from(dir.display().to_string())]);
        session.read_libs(&[PathBuf::from("demo.lib")]).unwrap();
    };

    let mut original = Session::new();
    configure(&mut original);
    original
        .apply_db_update(
            DbUpdate {
                modules: vec![independent_mapping_cones(BinaryOp::BitAnd)],
                top: Some("top".to_string()),
            },
            CurrentDesignPolicy::ElaboratedTop,
        )
        .unwrap();
    original.synthesize().unwrap();
    let checkpoint = dir.join("top.ock");
    original.write_checkpoint_file(&checkpoint).unwrap();

    let mut restored = Session::new();
    restored.process.runtime =
        ExecutionContext::new(&opto_runtime::ExecutionConfig { max_threads: 1 }).unwrap();
    configure(&mut restored);
    assert_eq!(restored.read_checkpoint_file(&checkpoint).unwrap(), "top");
    assert_eq!(restored.current_design(), Some("top"));
    let restored_synthesis = restored
        .state
        .designs
        .get("top")
        .unwrap()
        .synthesized
        .as_ref()
        .unwrap();
    restored_synthesis.validate_checkpoint().unwrap();
    let mut unchanged_events = Vec::new();
    restored
        .synthesize_observed(SynthesisEffort::Medium, &mut |event| {
            unchanged_events.push(event);
        })
        .unwrap();
    assert_eq!(
        unchanged_events,
        [SynthesisEvent::Completed {
            design: "top".to_string(),
            synthesized: false,
        }]
    );

    restored
        .apply_db_update(
            DbUpdate {
                modules: vec![independent_mapping_cones(BinaryOp::BitOr)],
                top: Some("top".to_string()),
            },
            CurrentDesignPolicy::ElaboratedTop,
        )
        .unwrap();
    let mut incremental_events = Vec::new();
    restored
        .synthesize_observed(SynthesisEffort::Medium, &mut |event| {
            incremental_events.push(event);
        })
        .unwrap();
    assert!(
        matches!(
            &incremental_events[1],
            SynthesisEvent::ArtifactCompleted { design, metrics }
                if design == "top"
                    && metrics.source_change.changed_operations == 1
                    && metrics.synthesis_regions == 1
                    && metrics.regional_decision_hits == 0
                    && metrics.regional_decision_misses == 1
        ),
        "{incremental_events:#?}"
    );
    let restored_verilog = mapped_verilog_text(
        restored
            .state
            .designs
            .get("top")
            .unwrap()
            .synthesized
            .as_ref()
            .unwrap()
            .mapped(),
    );

    let mut cold = Session::new();
    configure(&mut cold);
    cold.apply_db_update(
        DbUpdate {
            modules: vec![independent_mapping_cones(BinaryOp::BitOr)],
            top: Some("top".to_string()),
        },
        CurrentDesignPolicy::ElaboratedTop,
    )
    .unwrap();
    cold.synthesize().unwrap();
    let cold_verilog = mapped_verilog_text(
        cold.state
            .designs
            .get("top")
            .unwrap()
            .synthesized
            .as_ref()
            .unwrap()
            .mapped(),
    );
    assert_eq!(restored_verilog, cold_verilog);

    std::fs::write(
        dir.join("demo.lib"),
        r#"
library (demo) {
  cell (AND2) {
    area : 7.0;
    pin (A) { direction : input; }
    pin (B) { direction : input; }
    pin (Y) { direction : output; function : "A B"; }
  }
  cell (OR2) {
    area : 7.0;
    pin (A) { direction : input; }
    pin (B) { direction : input; }
    pin (Y) { direction : output; function : "A+B"; }
  }
  cell (XOR2) {
    area : 8.0;
    pin (A) { direction : input; }
    pin (B) { direction : input; }
    pin (Y) { direction : output; function : "A^B"; }
  }
}
"#,
    )
    .unwrap();
    let mut changed_library = Session::new();
    configure(&mut changed_library);
    changed_library.read_checkpoint_file(&checkpoint).unwrap();
    let mut changed_library_events = Vec::new();
    changed_library
        .synthesize_observed(SynthesisEffort::Medium, &mut |event| {
            changed_library_events.push(event);
        })
        .unwrap();
    assert!(matches!(
        &changed_library_events[1],
        SynthesisEvent::ArtifactCompleted { design, metrics }
            if design == "top" && metrics.regional_decision_hits == 0
    ));

    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn rereading_identical_rtl_keeps_incremental_synthesis_artifacts() {
    let mut session = Session::new();
    install_test_mapping_library(&mut session);
    session
        .apply_db_update(
            DbUpdate {
                modules: vec![hierarchy_leaf("top", 1, true)],
                top: Some("top".to_string()),
            },
            CurrentDesignPolicy::ElaboratedTop,
        )
        .unwrap();
    session.synthesize().unwrap();
    let implementation = artifact_revision(&session, "top");
    let source_revision = session.state.designs.get("top").unwrap().source_revision;

    session
        .apply_db_update(
            DbUpdate {
                modules: vec![hierarchy_leaf("top", 1, true)],
                top: Some("top".to_string()),
            },
            CurrentDesignPolicy::ElaboratedTop,
        )
        .unwrap();
    let read_revision = session.revision();
    session.synthesize().unwrap();

    assert_eq!(session.revision(), read_revision);
    assert_eq!(
        session.state.designs.get("top").unwrap().source_revision,
        source_revision
    );
    assert_eq!(artifact_revision(&session, "top"), implementation);
}

#[test]
fn failed_root_synthesis_does_not_publish_a_partial_artifact() {
    let mut session = Session::new();
    install_test_mapping_library(&mut session);
    session
        .apply_db_update(
            DbUpdate {
                modules: vec![
                    hierarchy_parent("top", 1, &[("u_left", "left"), ("u_right", "right")]),
                    hierarchy_leaf("left", 1, false),
                    hierarchy_leaf("right", 1, false),
                ],
                top: Some("top".to_string()),
            },
            CurrentDesignPolicy::ElaboratedTop,
        )
        .unwrap();
    session.synthesize().unwrap();
    let top_implementation = artifact_revision(&session, "top");

    session
        .apply_db_update(
            DbUpdate {
                modules: vec![
                    hierarchy_leaf("left", 1, true),
                    unsupported_tri_state_leaf("right"),
                ],
                top: Some("top".to_string()),
            },
            CurrentDesignPolicy::ElaboratedTop,
        )
        .unwrap();
    let read_revision = session.revision();
    assert!(session.synthesize().is_err());

    assert_eq!(session.revision(), read_revision);
    assert!(
        session
            .state
            .designs
            .get("left")
            .unwrap()
            .synthesized
            .is_none()
    );
    assert!(
        session
            .state
            .designs
            .get("right")
            .unwrap()
            .synthesized
            .is_none()
    );
    assert_eq!(artifact_revision(&session, "top"), top_implementation);
}

#[test]
fn synthesize_publication_traces_only_the_root_artifact() {
    #[derive(Default)]
    struct GateState {
        arrived: BTreeSet<String>,
        released: BTreeSet<String>,
    }

    let mut session = Session::with_parallelism(4).unwrap();
    install_test_mapping_library(&mut session);
    session
        .apply_db_update(
            DbUpdate {
                modules: vec![
                    hierarchy_parent("top", 1, &[("u_left", "left"), ("u_right", "right")]),
                    hierarchy_leaf("left", 1, true),
                    hierarchy_leaf("right", 1, false),
                ],
                top: Some("top".to_string()),
            },
            CurrentDesignPolicy::ElaboratedTop,
        )
        .unwrap();
    let gate = Arc::new((
        std::sync::Mutex::new(GateState::default()),
        std::sync::Condvar::new(),
    ));
    let (arrived_tx, arrived_rx) = std::sync::mpsc::channel::<String>();
    let (finished_tx, finished_rx) = std::sync::mpsc::channel::<String>();
    let controller_gate = Arc::clone(&gate);
    let controller = std::thread::spawn(move || {
        let arrived = (0..1)
            .map(|_| arrived_rx.recv().unwrap())
            .collect::<BTreeSet<_>>();
        assert_eq!(arrived, BTreeSet::from(["top".to_string()]));
        let (lock, changed) = &*controller_gate;
        lock.lock().unwrap().released.insert("top".to_string());
        changed.notify_all();
        assert_eq!(finished_rx.recv().unwrap(), "top");
    });
    let trace_gate = Arc::clone(&gate);
    let trace = move |trace: SynthesisTrace| {
        let first = {
            let (lock, _) = &*trace_gate;
            lock.lock()
                .unwrap()
                .arrived
                .insert(trace.design.to_string())
        };
        if first {
            arrived_tx.send(trace.design.to_string()).unwrap();
            let (lock, changed) = &*trace_gate;
            let mut state = lock.lock().unwrap();
            while !state.released.contains(trace.design.as_ref()) {
                state = changed.wait(state).unwrap();
            }
        }
        if trace.progress.stage == opto_synth::StageId::FINALIZATION
            && trace.progress.status == opto_synth::SynthesisProgressStatus::Completed
        {
            finished_tx.send(trace.design.to_string()).unwrap();
        }
    };
    let mut events = Vec::new();
    session
        .synthesize_traced(&mut |event| events.push(event), &trace)
        .unwrap();
    controller.join().unwrap();

    let published = events
        .iter()
        .filter_map(|event| match event {
            SynthesisEvent::ArtifactCompleted { design, .. } => Some(design.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(published, ["top"]);
}

#[test]
fn linked_root_synthesis_is_deterministic_across_thread_counts() {
    fn synthesize_with_threads(max_threads: usize) -> String {
        let mut session = Session::new();
        install_test_mapping_library(&mut session);
        session.process.runtime =
            ExecutionContext::new(&opto_runtime::ExecutionConfig { max_threads }).unwrap();
        session
            .apply_db_update(
                DbUpdate {
                    modules: vec![
                        hierarchy_parent("top", 1, &[("u_left", "left"), ("u_right", "right")]),
                        hierarchy_leaf("left", 1, true),
                        hierarchy_leaf("right", 1, true),
                    ],
                    top: Some("top".to_string()),
                },
                CurrentDesignPolicy::ElaboratedTop,
            )
            .unwrap();
        session.synthesize().unwrap();
        let mut output = Vec::new();
        session
            .write_verilog_modules(
                &mut output,
                &["top".to_string(), "left".to_string(), "right".to_string()],
            )
            .unwrap();
        String::from_utf8(output).expect("Verilog writer only emits UTF-8 text")
    }

    assert_eq!(synthesize_with_threads(1), synthesize_with_threads(4));
}
