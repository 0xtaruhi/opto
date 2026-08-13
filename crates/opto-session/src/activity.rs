// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Canonical binding of persistent power targets into one timing generation.

use opto_power::{ActivityAnnotations, SwitchingActivity};
use opto_timing::{NetId, PortId, TimingModel};

#[derive(Debug, Clone, Copy)]
pub(crate) enum ActivityTarget {
    Port(PortId),
    Net(NetId),
}

/// Resolves persistent activity targets against one sealed timing generation.
///
/// Targets removed by optimization contribute no timing nets in every caller.
/// Conflicting live annotations remain an error after port expansion.
pub(crate) fn resolve_activity_annotations(
    model: &TimingModel,
    targets: impl IntoIterator<Item = (ActivityTarget, SwitchingActivity)>,
) -> Result<ActivityAnnotations, String> {
    let mut entries = Vec::new();
    for (target, activity) in targets {
        match target {
            ActivityTarget::Port(port) => {
                let nets = model.port_nets(port);
                entries.extend(nets.iter().map(|&net| (net, activity)));
            }
            ActivityTarget::Net(net) => {
                if let Some(timing_net) = model.net_id_for_object(net) {
                    entries.push((timing_net, activity));
                }
            }
        }
    }
    ActivityAnnotations::new(model.generation(), entries).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use opto_core::ObjectUid;
    use opto_library::{
        ArcDelayModel, BooleanFunction, LookupTable, NldmTimingModel, TargetCell, TargetCellUsage,
        TargetPin, TargetPinDirection, TargetTimingArc, TargetTimingType, TimingSense,
    };
    use opto_timing::{
        DesignId, TimingDesign, TimingLibrary, TimingNet, TimingObjectBindings, TimingPort,
        TimingPortDirection,
    };

    fn object_uid(raw: u64) -> ObjectUid {
        ObjectUid::from_raw(raw).unwrap()
    }

    fn timing_library() -> TimingLibrary {
        let input = TargetPin {
            name: "A".to_string(),
            direction: TargetPinDirection::Input,
            function: None,
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
        let output = TargetPin {
            name: "Y".to_string(),
            direction: TargetPinDirection::Output,
            function: Some(BooleanFunction::Pin("A".to_string())),
            three_state: None,
            capacitance: None,
            rise_capacitance: None,
            fall_capacitance: None,
            receiver_capacitance: None,
            fanout_load: None,
            next_state_type: None,
            timing_arcs: vec![TargetTimingArc {
                related_pin: "A".to_string(),
                timing_type: TargetTimingType::Combinational,
                timing_sense: TimingSense::PositiveUnate,
                delay_model: Some(ArcDelayModel::Nldm(NldmTimingModel::new(
                    Some(LookupTable::scalar(0.1)),
                    Some(LookupTable::scalar(0.1)),
                    None,
                    None,
                ))),
                rise_constraint: None,
                fall_constraint: None,
            }],
            clock_gate_role: None,
        };
        TimingLibrary {
            cells: vec![TargetCell {
                name: "BUF".to_string(),
                area: Some(1.0),
                dont_use: false,
                usage: TargetCellUsage::default(),
                pins: vec![input, output],
                sequential: Vec::new(),
                clock_gate: None,
                memory: None,
            }]
            .into(),
            ..TimingLibrary::default()
        }
    }

    fn model(net_name: &str) -> (TimingModel, PortId, NetId) {
        let port = PortId::from_uid(object_uid(2));
        let net = NetId::from_uid(object_uid(3));
        let mut model = TimingModel::new(
            TimingDesign {
                id: DesignId::from_uid(object_uid(1)),
                name: "top".to_string(),
                ports: vec![TimingPort {
                    id: port,
                    name: "input".to_string(),
                    net: TimingNet::named(net_name),
                    direction: TimingPortDirection::Input,
                }],
                instances: Vec::new(),
            },
            timing_library(),
        )
        .unwrap();
        let mut bindings = TimingObjectBindings::builder();
        bindings.bind_net(net_name, net).unwrap();
        model.set_object_bindings(bindings.finish().unwrap());
        (model, port, net)
    }

    #[test]
    fn resolves_persistent_named_net_without_scanning_the_timing_graph() {
        let (model, _, net) = model("named_net");
        let activity = SwitchingActivity::new(0.4, 0.2, 0.5).unwrap();
        let annotations =
            resolve_activity_annotations(&model, [(ActivityTarget::Net(net), activity)]).unwrap();

        assert!(annotations.contains(model.net_id("named_net").unwrap()));
    }

    #[test]
    fn resolves_anonymous_net_through_the_same_persistent_identity_index() {
        let (model, _, net) = model("_n0");
        let activity = SwitchingActivity::new(0.4, 0.2, 0.5).unwrap();
        let annotations =
            resolve_activity_annotations(&model, [(ActivityTarget::Net(net), activity)]).unwrap();

        assert!(annotations.contains(model.net_id("_n0").unwrap()));
    }

    #[test]
    fn ignores_targets_removed_from_the_sealed_generation() {
        let (model, _, _) = model("named_net");
        let removed = NetId::from_uid(object_uid(99));
        let activity = SwitchingActivity::new(0.4, 0.2, 0.5).unwrap();
        let annotations =
            resolve_activity_annotations(&model, [(ActivityTarget::Net(removed), activity)])
                .unwrap();

        assert!(!annotations.contains(model.net_id("named_net").unwrap()));
    }

    #[test]
    fn rejects_conflicting_port_and_net_annotations_after_resolution() {
        let (model, port, net) = model("named_net");
        let first = SwitchingActivity::new(0.4, 0.2, 0.5).unwrap();
        let second = SwitchingActivity::new(0.6, 0.2, 0.5).unwrap();
        let error = resolve_activity_annotations(
            &model,
            [
                (ActivityTarget::Port(port), first),
                (ActivityTarget::Net(net), second),
            ],
        )
        .unwrap_err();

        assert!(error.contains("conflicting switching-activity annotations"));
    }
}
