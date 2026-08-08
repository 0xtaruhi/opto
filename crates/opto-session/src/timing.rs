// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use opto_ir::word;
use opto_timing::{
    PortBindings, TimingConnection, TimingDesign, TimingInstance, TimingInstanceId, TimingNet,
    TimingPort, TimingPortDirection,
};

pub(super) fn design(
    module: &word::WordModule,
    id: opto_db::DesignId,
    port_bindings: &PortBindings,
) -> Result<TimingDesign, crate::SessionError> {
    let ports = module
        .ports()
        .iter()
        .enumerate()
        .flat_map(|(index, port)| {
            let name = module.name_str(port.name);
            let id = port_bindings.get(index);
            let direction = port_direction(port.direction);
            (0..port.ty.width()).map(move |bit| {
                let name = if port.ty.width() == 1 {
                    name.to_string()
                } else {
                    format!("{name}[{bit}]")
                };
                Ok(TimingPort {
                    id: id.ok_or_else(|| {
                        crate::SessionError::state(format!(
                            "timing: port '{name}' has no typed object ID"
                        ))
                    })?,
                    net: TimingNet::named(name.clone()),
                    name,
                    direction,
                })
            })
        })
        .collect::<Result<Vec<_>, crate::SessionError>>()?;
    let mut instances = Vec::new();
    for (index, instance) in module.instances().iter().enumerate() {
        let mut connections = Vec::new();
        let instance_name = module.name_str(instance.name);
        for connection in &instance.connections {
            let Some(net) = net_name(module, connection.value)? else {
                continue;
            };
            let pin = module.name_str(connection.port);
            connections.push(TimingConnection {
                pin: pin.to_string(),
                net,
            });
        }
        instances.push(TimingInstance {
            id: TimingInstanceId::from_raw(index.try_into().map_err(|_| {
                crate::SessionError::capacity("timing: instance ID exceeds 32-bit capacity")
            })?),
            name: instance_name.to_string(),
            cell: module.name_str(instance.module).to_string(),
            connections,
        });
    }
    Ok(TimingDesign {
        id,
        name: module.name().to_string(),
        ports,
        instances,
    })
}

fn port_direction(direction: word::PortDirection) -> TimingPortDirection {
    match direction {
        word::PortDirection::Input => TimingPortDirection::Input,
        word::PortDirection::Output => TimingPortDirection::Output,
        word::PortDirection::Inout => TimingPortDirection::Inout,
    }
}

fn net_name(
    module: &word::WordModule,
    value_id: word::ValueId,
) -> Result<Option<String>, crate::SessionError> {
    let value = module.value(value_id).ok_or_else(|| {
        crate::SessionError::state(format!("report_timing: unknown RTL value {value_id:?}"))
    })?;
    let word::ValueKind::Signal(reference) = value.kind else {
        return Ok(None);
    };
    let signal = module.signal(reference.signal).ok_or_else(|| {
        crate::SessionError::state(format!(
            "report_timing: unknown RTL signal {:?}",
            reference.signal
        ))
    })?;
    Ok(signal.name.map(|name| {
        let name = module.name_str(name);
        if reference.lsb == 0 && reference.width() == signal.ty.width() {
            name.to_string()
        } else if reference.width() == 1 {
            format!("{name}[{}]", reference.lsb)
        } else {
            format!(
                "{name}[{}:{}]",
                reference.lsb + reference.width() - 1,
                reference.lsb
            )
        }
    }))
}
