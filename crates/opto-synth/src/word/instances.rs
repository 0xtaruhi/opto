// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use opto_ir::word;

/// One source-instance port connection captured before lowering rewires it.
pub(crate) type InstanceConnectionSnapshot =
    (usize, opto_ir::NameId, word::ValueId, word::SourceSpan);

/// Captures every source-instance port connection before mutation begins.
pub(crate) fn snapshot(module: &word::WordModule) -> Vec<InstanceConnectionSnapshot> {
    module
        .instances()
        .iter()
        .enumerate()
        .flat_map(|(instance_index, instance)| {
            instance.connections.iter().map(move |connection| {
                (
                    instance_index,
                    connection.port,
                    connection.value,
                    connection.source.clone(),
                )
            })
        })
        .collect()
}
