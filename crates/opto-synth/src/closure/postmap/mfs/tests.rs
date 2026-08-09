// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::{
    CellFunction, DriverIndex, ResynthesisObjective, cell_functions, closed_dying_cone,
    optimization_boundary_nets, region_boundary_nets, resynthesis_cells, sorted_candidate_nets,
};
use hashbrown::{HashMap, HashSet};
use opto_ir::mapped::{
    CellSpec, ConnectionRef, ConnectionSignal, MappedBuilder, NetId, PortDirection, RegionDelta,
};

fn function(name: &str, input_count: usize, truth_bits: u64) -> (String, CellFunction) {
    (
        name.to_string(),
        CellFunction {
            inputs: (0..input_count).map(|index| format!("A{index}")).collect(),
            output: "Z".to_string(),
            truth_bits,
            input_count,
            library_index: 0,
            area: 1.0,
            delay: 1.0,
            transition: 1.0,
        },
    )
}

#[test]
fn mfs_catalog_excludes_forbidden_cells() {
    let cell = |name: &str| {
        let pin = |name: &str, direction, function: Option<&str>| opto_library::TargetPin {
            name: name.to_string(),
            direction,
            function: function
                .map(|function| opto_library::BooleanFunction::parse(function).unwrap()),
            three_state: None,
            capacitance: None,
            rise_capacitance: None,
            fall_capacitance: None,
            receiver_capacitance: None,
            fanout_load: None,
            next_state_type: None,
            timing_arcs: Vec::new(),
            clock_gate_role: None,
        };
        opto_library::TargetCell {
            name: name.to_string(),
            area: Some(1.0),
            dont_use: false,
            usage: opto_library::TargetCellUsage::default(),
            pins: vec![
                pin("A", opto_library::TargetPinDirection::Input, None),
                pin("Y", opto_library::TargetPinDirection::Output, Some("A")),
            ],
            sequential: Vec::new(),
            clock_gate: None,
            memory: None,
        }
    };
    let eligible = cell("ELIGIBLE");
    let mut isolation = cell("ISOLATION");
    isolation.usage = opto_library::TargetCellUsage::ISOLATION;
    let mut dont_use = cell("DONT_USE");
    dont_use.dont_use = true;
    let library: opto_library::TargetCellSet = vec![eligible, isolation, dont_use].into();

    let functions = cell_functions(&library);

    assert_eq!(functions.len(), 1);
    assert!(functions.contains_key("ELIGIBLE"));
}

#[test]
fn equal_area_resynthesis_cells_use_name_as_a_stable_tie_breaker() {
    let functions = HashMap::from([
        function("INV_Z", 1, 0b01),
        function("INV_A", 1, 0b01),
        function("AND_Z", 2, 0b1000),
        function("AND_A", 2, 0b1000),
        function("MAJ_Z", 3, 0b1110_1000),
        function("MAJ_A", 3, 0b1110_1000),
        function("WIDE", 6, 0x8000_0000_0000_0000),
    ]);

    let cells = resynthesis_cells(&functions, ResynthesisObjective::Area);

    assert_eq!(
        cells.inverter.as_ref().map(|cell| cell.name.as_str()),
        Some("INV_A")
    );
    assert_eq!(cells.by_input_count[2][0].name, "AND_A");
    assert_eq!(cells.by_input_count[3][0].name, "MAJ_A");
    assert_eq!(cells.by_input_count[6][0].name, "WIDE");
}

#[test]
fn driver_index_refreshes_only_edited_nets() {
    let mut builder = MappedBuilder::new("top", opto_ir::RevisionId::INITIAL).unwrap();
    let input = builder.add_net(Some("input")).unwrap();
    let output = builder.add_net(Some("output")).unwrap();
    let original = builder
        .add_cell(
            "original",
            "BUF",
            None,
            &[
                ("A0".to_string(), None, ConnectionSignal::Net(input)),
                ("Z".to_string(), None, ConnectionSignal::Net(output)),
            ],
        )
        .unwrap();
    let mut mapped = builder.freeze().unwrap();
    let functions = HashMap::from([function("BUF", 1, 0b10)]);
    let mut drivers = DriverIndex::build(&mapped, &functions);
    assert_eq!(drivers.driver(&mapped, output), Some(original));

    let snapshot = mapped.snapshot_region([original], [input, output]).unwrap();
    let mut delta = RegionDelta::new(snapshot);
    let replacement = delta
        .add_cell(
            CellSpec::new("replacement", "BUF", None)
                .connect("A0", None, ConnectionRef::Net(input))
                .connect("Z", None, ConnectionRef::Net(output)),
        )
        .unwrap();
    delta.remove_cell(original).unwrap();
    let edit = mapped.apply_region_delta(delta).unwrap();
    let replacement = edit
        .added_cells()
        .find_map(|(temporary, cell)| (temporary == replacement).then_some(cell))
        .unwrap();

    assert_eq!(drivers.driver(&mapped, output), None);
    drivers.refresh(&mapped, &functions, [output]);
    assert_eq!(drivers.driver(&mapped, output), Some(replacement));
}

#[test]
fn timing_resynthesis_catalog_selects_fast_cells_independently_of_area_catalog() {
    let (slow_name, mut slow) = function("AND_SMALL", 2, 0b1000);
    slow.area = 1.0;
    slow.delay = 2.0;
    let (fast_name, mut fast) = function("AND_FAST", 2, 0b1000);
    fast.area = 3.0;
    fast.delay = 0.5;
    let functions = HashMap::from([(slow_name, slow), (fast_name, fast)]);

    let area = resynthesis_cells(&functions, ResynthesisObjective::Area);
    let timing = resynthesis_cells(&functions, ResynthesisObjective::Timing);
    assert_eq!(area.by_input_count[2][0].name, "AND_SMALL");
    assert_eq!(timing.by_input_count[2][0].name, "AND_FAST");
}

#[test]
fn hierarchy_anchors_partition_regions_without_blocking_optimization() {
    let mut builder = MappedBuilder::new("top", opto_ir::RevisionId::INITIAL).unwrap();
    let input = builder.add_net(Some("input")).unwrap();
    let hierarchy = builder.add_net(Some("hierarchy")).unwrap();
    builder
        .add_port("input", PortDirection::Input, &[input])
        .unwrap();
    builder
        .add_design_instance(
            "u_child",
            "child",
            &[("a".to_string(), vec![ConnectionSignal::Net(hierarchy)])],
        )
        .unwrap();
    let mapped = builder.freeze().unwrap();

    let optimization = optimization_boundary_nets(&mapped);
    let regions = region_boundary_nets(&mapped);
    assert!(optimization.contains(&input));
    assert!(!optimization.contains(&hierarchy));
    assert!(regions.contains(&input));
    assert!(regions.contains(&hierarchy));
}

#[test]
fn elaborated_hierarchy_labels_create_typed_region_boundaries() {
    let mut builder = MappedBuilder::new("top", opto_ir::RevisionId::INITIAL).unwrap();
    let input = builder.add_net(Some("input")).unwrap();
    let boundary = builder.add_net(Some("boundary")).unwrap();
    let output = builder.add_net(Some("u_child/output")).unwrap();
    builder
        .add_cell(
            "U0",
            "BUF",
            None,
            &[
                ("A".to_string(), None, ConnectionSignal::Net(input)),
                ("Y".to_string(), None, ConnectionSignal::Net(boundary)),
            ],
        )
        .unwrap();
    builder
        .add_cell(
            "u_child/U1",
            "BUF",
            None,
            &[
                ("A".to_string(), None, ConnectionSignal::Net(boundary)),
                ("Y".to_string(), None, ConnectionSignal::Net(output)),
            ],
        )
        .unwrap();
    let mapped = builder.freeze().unwrap();

    assert!(!optimization_boundary_nets(&mapped).contains(&boundary));
    assert!(region_boundary_nets(&mapped).contains(&boundary));
}

#[test]
fn dying_cone_excludes_shared_nets_and_their_transitive_drivers() {
    let mut builder = MappedBuilder::new("top", opto_ir::RevisionId::INITIAL).unwrap();
    let source = builder.add_net(Some("source")).unwrap();
    let upstream = builder.add_net(Some("upstream")).unwrap();
    let shared = builder.add_net(Some("shared")).unwrap();
    let output = builder.add_net(Some("output")).unwrap();
    let upstream_cell = builder
        .add_cell(
            "upstream_cell",
            "BUF",
            None,
            &[
                ("A".to_string(), None, ConnectionSignal::Net(source)),
                ("Y".to_string(), None, ConnectionSignal::Net(upstream)),
            ],
        )
        .unwrap();
    let driver = builder
        .add_cell(
            "driver",
            "BUF",
            None,
            &[
                ("A".to_string(), None, ConnectionSignal::Net(upstream)),
                ("Y".to_string(), None, ConnectionSignal::Net(shared)),
            ],
        )
        .unwrap();
    let root = builder
        .add_cell(
            "root",
            "BUF",
            None,
            &[
                ("A".to_string(), None, ConnectionSignal::Net(shared)),
                ("Y".to_string(), None, ConnectionSignal::Net(output)),
            ],
        )
        .unwrap();
    builder
        .add_cell(
            "external_consumer",
            "BUF",
            None,
            &[("A".to_string(), None, ConnectionSignal::Net(shared))],
        )
        .unwrap();
    let mapped = builder.freeze().unwrap();

    let closed = closed_dying_cone(
        &mapped,
        root,
        &[],
        &[(driver, shared, 1.0), (upstream_cell, upstream, 1.0)],
    );

    assert!(closed.is_empty());
}

#[test]
fn candidate_nets_are_ordered_by_typed_id_not_hash_bucket() {
    let first = NetId::from_index(1).unwrap();
    let second = NetId::from_index(2).unwrap();
    let third = NetId::from_index(3).unwrap();
    let mut bits = HashMap::new();
    bits.insert(third, vec![3]);
    bits.insert(first, vec![1]);
    bits.insert(second, vec![2]);
    let tainted = HashSet::from([second]);

    let ordered = sorted_candidate_nets(&bits, &tainted)
        .into_iter()
        .map(|(net, _)| net)
        .collect::<Vec<_>>();

    assert_eq!(ordered, [first, third]);
}
