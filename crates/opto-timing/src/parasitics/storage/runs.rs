// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

pub(super) fn insert_run(
    runs: &mut Vec<ParasiticRun>,
    store: Arc<ParasiticStore>,
) -> Result<(), crate::TimingError> {
    let mut incoming = ParasiticRun {
        weight: store.work_weight()?,
        store,
    };
    while runs
        .last()
        .is_some_and(|older| older.weight <= incoming.weight.saturating_mul(2))
    {
        let older = runs.pop().expect("the last run was just observed");
        let merged = compact_logical_store(LogicalNetIter::from_stores([
            older.store.as_ref(),
            incoming.store.as_ref(),
        ]))?;
        incoming = ParasiticRun {
            weight: merged.work_weight()?,
            store: Arc::new(merged),
        };
    }
    runs.push(incoming);
    Ok(())
}

pub(super) fn compact_logical_store(
    nets: LogicalNetIter<'_>,
) -> Result<ParasiticStore, crate::TimingError> {
    let mut builder = ParasiticStoreBuilder::default();
    for net in nets {
        builder.push_ref(net, None)?;
    }
    Ok(builder.finish())
}

pub(super) fn logical_nets_cover(
    mut available: LogicalNetIter<'_>,
    required: LogicalNetIter<'_>,
) -> bool {
    let mut candidate = available.next();
    for net in required {
        let required_name = net.name().expect("validated required parasitic name");
        loop {
            let Some(available_net) = candidate else {
                return false;
            };
            match available_net
                .name()
                .expect("validated candidate parasitic name")
                .cmp(required_name)
            {
                Ordering::Less => candidate = available.next(),
                Ordering::Equal => {
                    candidate = available.next();
                    break;
                }
                Ordering::Greater => return false,
            }
        }
    }
    true
}

impl ParasiticStore {
    pub(super) fn work_weight(&self) -> Result<u64, crate::TimingError> {
        let mut bytes = weighted_bytes::<u8>(self.names.stored_bytes())?
            .checked_add(weighted_bytes::<[u32; 4]>(self.names.entry_count())?)
            .ok_or_else(|| capacity("parasitic run weight"))?;
        for allocation in [
            weighted_bytes::<ParasiticNet>(self.nets.len())?,
            weighted_bytes::<ParasiticNode>(self.nodes.len())?,
            weighted_bytes::<ParasiticResistor>(self.resistors.len())?,
            weighted_bytes::<ParasiticConnection>(self.connections.len())?,
        ] {
            bytes = bytes
                .checked_add(allocation)
                .ok_or_else(|| capacity("parasitic run weight"))?;
        }
        Ok(bytes.max(1))
    }
}

impl<'a> LogicalNetIter<'a> {
    pub(super) fn from_database(database: &'a Parasitics) -> Self {
        Self::from_stores(
            std::iter::once(database.base.as_ref())
                .chain(database.runs.iter().map(|run| run.store.as_ref())),
        )
    }

    pub(super) fn from_runs(runs: &'a [ParasiticRun]) -> Self {
        Self::from_stores(runs.iter().map(|run| run.store.as_ref()))
    }

    fn from_stores(stores: impl IntoIterator<Item = &'a ParasiticStore>) -> Self {
        let stores = stores.into_iter().collect::<Box<[_]>>();
        let mut iterator = Self {
            pending: BinaryHeap::with_capacity(stores.len()),
            stores,
        };
        for store in 0..iterator.stores.len() {
            iterator.push(store, 0);
        }
        iterator
    }

    fn push(&mut self, store: usize, row: usize) {
        let source: &'a ParasiticStore = self.stores[store];
        let Some(net) = source.net_ref_at(row) else {
            return;
        };
        self.pending.push(Reverse((
            net.name().expect("validated parasitic net name"),
            Reverse(store),
            row,
        )));
    }
}

impl<'a> Iterator for LogicalNetIter<'a> {
    type Item = ParasiticNetRef<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        let Reverse((name, Reverse(store), row)) = self.pending.pop()?;
        let winner = self.stores[store].net_ref_at(row);
        self.push(store, row + 1);
        while matches!(
            self.pending.peek(),
            Some(Reverse((candidate, _, _))) if *candidate == name
        ) {
            let Reverse((_, Reverse(shadowed_store), shadowed_row)) = self
                .pending
                .pop()
                .expect("a matching shadowed run was just observed");
            self.push(shadowed_store, shadowed_row + 1);
        }
        winner
    }
}
