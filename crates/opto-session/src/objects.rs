// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use crate::SessionError;
use opto_timing::{TimingObjectKind, TimingPortDirection};

mod attribute;
mod collection;
mod lifecycle;
pub(crate) mod locators;
mod power;
mod query;
mod timing;

fn validate_design_rule_object_class(
    command: &str,
    kind: TimingObjectKind,
) -> Result<(), SessionError> {
    let allowed = match command {
        "set_max_transition" => matches!(
            kind,
            TimingObjectKind::Design | TimingObjectKind::Port(_) | TimingObjectKind::Clock
        ),
        "set_max_capacitance" => matches!(
            kind,
            TimingObjectKind::Design | TimingObjectKind::Port(_) | TimingObjectKind::Clock
        ),
        "set_max_fanout" => matches!(
            kind,
            TimingObjectKind::Design
                | TimingObjectKind::Port(TimingPortDirection::Input | TimingPortDirection::Inout)
        ),
        _ => false,
    };
    allowed.then_some(()).ok_or_else(|| {
        let class = match kind {
            TimingObjectKind::Design => "design",
            TimingObjectKind::Port(_) => "port",
            TimingObjectKind::Clock => "clock",
            TimingObjectKind::Cell => "cell",
            TimingObjectKind::Pin => "pin",
            TimingObjectKind::Net => "net",
        };
        SessionError::object(format!(
            "{command}: object class '{class}' is not valid for this command"
        ))
    })
}
