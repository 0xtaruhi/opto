// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::RevisionId;
use crate::mapped::{MappedBuilder, PortDirection};

fn fixture() -> (MappedNetlist, CellId, NetId, NetId) {
    let mut builder = MappedBuilder::new("top", RevisionId::INITIAL).unwrap();
    let a = builder.add_net(Some("a")).unwrap();
    let y = builder.add_net(Some("y")).unwrap();
    builder.add_port("a", PortDirection::Input, &[a]).unwrap();
    builder.add_port("y", PortDirection::Output, &[y]).unwrap();
    let cell = builder
        .add_cell(
            "U0",
            "INVX1",
            Some(0),
            &[
                ("A".to_string(), Some(0), ConnectionSignal::Net(a)),
                ("Y".to_string(), Some(1), ConnectionSignal::Net(y)),
            ],
        )
        .unwrap();
    (builder.freeze().unwrap(), cell, a, y)
}

#[test]
fn multi_cell_delta_commits_and_rolls_back_exactly() {
    let (mut netlist, cell, a, y) = fixture();
    let before_names = netlist.names.entry_count();
    let before_revision = netlist.edit_revision();
    let input_pins_before = netlist.pins_on_net(a).unwrap().collect::<Vec<_>>();
    let output_pins_before = netlist.pins_on_net(y).unwrap().collect::<Vec<_>>();
    let snapshot = netlist.snapshot_region([cell], [a, y]).unwrap();
    let mut delta = RegionDelta::new(snapshot);
    let branch = delta.add_net(Some("branch".to_string())).unwrap();
    let added = delta
        .add_cell(
            CellSpec::new("U1", "BUFX1", Some(1))
                .connect("A", Some(0), ConnectionRef::Net(a))
                .connect("Y", Some(1), ConnectionRef::NewNet(branch)),
        )
        .unwrap();
    delta.replace_cell(cell, "INVX2", Some(2)).unwrap();

    let applied = netlist.apply_region_delta(delta).unwrap();
    let added_cell = applied.added_cell(added).unwrap();
    let added_net = applied.added_net(branch).unwrap();
    assert_eq!(netlist.cell_count(), 2);
    assert_eq!(netlist.net_count(), 3);
    assert_eq!(netlist.cell_type(cell), Some("INVX2"));
    assert_eq!(netlist.cell_name(added_cell), Some("U1"));
    assert_eq!(netlist.pins_on_net(a).unwrap().count(), 2);
    assert_eq!(netlist.pins_on_net(added_net).unwrap().count(), 1);
    assert!(
        netlist
            .pins_on_net(a)
            .unwrap()
            .all(|pin| netlist.pin_owner(pin).is_some())
    );

    netlist.rollback_region_delta(applied).unwrap();
    assert_eq!(netlist.cell_count(), 1);
    assert_eq!(netlist.net_count(), 2);
    assert_eq!(netlist.cell_type(cell), Some("INVX1"));
    assert_eq!(netlist.names.entry_count(), before_names);
    assert_eq!(netlist.edit_revision(), before_revision);
    assert_eq!(
        netlist.pins_on_net(a).unwrap().collect::<Vec<_>>(),
        input_pins_before
    );
    assert_eq!(
        netlist.pins_on_net(y).unwrap().collect::<Vec<_>>(),
        output_pins_before
    );
    assert!(netlist.pins_on_net(added_net).is_none());
}

#[test]
fn temporary_ids_cannot_cross_region_delta_owners() {
    let (mut netlist, _, _, _) = fixture();
    let snapshot = netlist.snapshot_region([], []).unwrap();
    let mut first = RegionDelta::new(snapshot.clone());
    let foreign = first.add_net(Some("foreign".to_string())).unwrap();
    let mut second = RegionDelta::new(snapshot);
    let _local = second.add_net(Some("local".to_string())).unwrap();
    second
        .add_cell(CellSpec::new("U1", "BUF", None).connect(
            "A",
            None,
            ConnectionRef::NewNet(foreign),
        ))
        .unwrap();

    let error = netlist.apply_region_delta(second).unwrap_err();
    assert!(error.to_string().contains("unknown temporary net"));
}

#[test]
fn disjoint_snapshot_survives_an_unrelated_commit_but_overlap_conflicts() {
    let (mut netlist, cell, a, y) = fixture();
    let first_snapshot = netlist.snapshot_region([cell], [a]).unwrap();
    let disjoint_snapshot = netlist.snapshot_region([], [y]).unwrap();

    let mut first = RegionDelta::new(first_snapshot.clone());
    first.replace_cell(cell, "INVX2", Some(2)).unwrap();
    netlist.apply_region_delta(first).unwrap();

    let mut disjoint = RegionDelta::new(disjoint_snapshot);
    disjoint.rename_net(y, Some("result".to_string())).unwrap();
    assert!(netlist.apply_region_delta(disjoint).is_ok());

    let mut stale = RegionDelta::new(first_snapshot);
    stale.replace_cell(cell, "INVX4", Some(4)).unwrap();
    assert_eq!(
        netlist.apply_region_delta(stale).unwrap_err(),
        RegionConflict::StaleCell(cell)
    );
}

#[test]
fn cell_adjacency_changes_invalidate_snapshots_of_connected_nets() {
    let (mut netlist, cell, a, y) = fixture();
    let before_add = netlist.snapshot_region([], [a]).unwrap();
    let addition = netlist.snapshot_region([], [a]).unwrap();
    let mut addition = RegionDelta::new(addition);
    addition
        .add_cell(CellSpec::new("U1", "BUFX1", Some(1)).connect(
            "A",
            Some(0),
            ConnectionRef::Net(a),
        ))
        .unwrap();
    netlist.apply_region_delta(addition).unwrap();
    let mut stale_add = RegionDelta::new(before_add);
    stale_add.rename_net(a, Some("input".to_string())).unwrap();
    assert_eq!(
        netlist.apply_region_delta(stale_add).unwrap_err(),
        RegionConflict::StaleNet(a)
    );

    let before_remove = netlist.snapshot_region([], [y]).unwrap();
    let removal = netlist.snapshot_region([cell], [a, y]).unwrap();
    let mut removal = RegionDelta::new(removal);
    removal.remove_cell(cell).unwrap();
    netlist.apply_region_delta(removal).unwrap();
    let mut stale_remove = RegionDelta::new(before_remove);
    stale_remove
        .rename_net(y, Some("output".to_string()))
        .unwrap();
    assert_eq!(
        netlist.apply_region_delta(stale_remove).unwrap_err(),
        RegionConflict::StaleNet(y)
    );
}

#[test]
fn removing_a_referenced_net_is_rejected_before_mutation() {
    let (mut netlist, cell, a, _) = fixture();
    let snapshot = netlist.snapshot_region([cell], [a]).unwrap();
    let mut delta = RegionDelta::new(snapshot);
    delta.remove_net(a).unwrap();
    let revision = netlist.edit_revision();

    assert!(netlist.apply_region_delta(delta).is_err());
    assert!(netlist.is_live_net(a));
    assert_eq!(netlist.edit_revision(), revision);
}

#[test]
fn reconnect_updates_intrusive_adjacency_and_rollback_restores_order() {
    let (mut netlist, cell, a, y) = fixture();
    let pins = netlist.pin_ids(cell).unwrap().collect::<Vec<_>>();
    let before_a = netlist.pins_on_net(a).unwrap().collect::<Vec<_>>();
    let before_y = netlist.pins_on_net(y).unwrap().collect::<Vec<_>>();
    let snapshot = netlist.snapshot_region([cell], [a, y]).unwrap();
    let mut delta = RegionDelta::new(snapshot);
    delta.reconnect_pin(pins[0], ConnectionRef::Net(y)).unwrap();

    let applied = netlist.apply_region_delta(delta).unwrap();
    assert_eq!(applied.renamed_nets().count(), 0);
    assert_eq!(netlist.pins_on_net(a).unwrap().count(), 0);
    assert_eq!(
        netlist.pins_on_net(y).unwrap().collect::<Vec<_>>(),
        vec![pins[1], pins[0]]
    );

    netlist.rollback_region_delta(applied).unwrap();
    assert_eq!(
        netlist.pins_on_net(a).unwrap().collect::<Vec<_>>(),
        before_a
    );
    assert_eq!(
        netlist.pins_on_net(y).unwrap().collect::<Vec<_>>(),
        before_y
    );
}

#[test]
fn applied_delta_separates_net_renames_from_adjacency_changes() {
    let (mut netlist, cell, a, y) = fixture();
    let snapshot = netlist.snapshot_region([cell], [a, y]).unwrap();
    let mut delta = RegionDelta::new(snapshot);
    delta.rename_net(a, Some("a".to_string())).unwrap();

    let unchanged = netlist.apply_region_delta(delta).unwrap();
    assert_eq!(unchanged.renamed_nets().count(), 0);
    netlist.rollback_region_delta(unchanged).unwrap();

    let snapshot = netlist.snapshot_region([], [a]).unwrap();
    let mut delta = RegionDelta::new(snapshot);
    delta.rename_net(a, Some("input".to_string())).unwrap();
    let renamed = netlist.apply_region_delta(delta).unwrap();
    assert_eq!(renamed.renamed_nets().collect::<Vec<_>>(), vec![a]);
}

#[test]
fn publication_repacks_tombstones_and_seals_editing() {
    let mut builder = MappedBuilder::new("top", RevisionId::INITIAL).unwrap();
    let internal = builder.add_net(Some("dead_net")).unwrap();
    let cell = builder
        .add_cell(
            "dead_cell",
            "BUFX1",
            Some(0),
            &[
                ("A".to_string(), Some(0), ConnectionSignal::Constant(false)),
                ("Y".to_string(), Some(1), ConnectionSignal::Net(internal)),
            ],
        )
        .unwrap();
    let mut netlist = builder.freeze().unwrap();
    let snapshot = netlist.snapshot_region([cell], [internal]).unwrap();
    let mut delta = RegionDelta::new(snapshot);
    delta.remove_cell(cell).unwrap();
    delta.remove_net(internal).unwrap();
    netlist.apply_region_delta(delta).unwrap();

    let (published, remap) = netlist.finalize_for_publication().unwrap();
    netlist = published;
    assert_eq!(remap.cell(cell), None);
    assert_eq!(netlist.cell_count(), 0);
    assert_eq!(netlist.net_count(), 0);
    assert_eq!(netlist.cell_slot_count(), 0);
    assert_eq!(netlist.net_slot_count(), 0);
    assert!(!netlist.is_live_cell(cell));
    assert!(!netlist.is_live_net(internal));

    let snapshot = netlist.snapshot_region([], []).unwrap();
    assert!(matches!(
        netlist.apply_region_delta(RegionDelta::new(snapshot)),
        Err(RegionConflict::Invalid(_))
    ));
    assert!(netlist.finalize_for_publication().is_err());
}
