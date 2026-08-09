// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::network::{LogicGraph, LogicNodeId};
use super::rewrite::{optimize_network, remap_literal};
use opto_formal::prove_logic_network_equivalence;
use opto_runtime::{ExecutionConfig, ExecutionContext};
use std::sync::OnceLock;

#[derive(Debug, Clone)]
struct RandomGate {
    kind: u8,
    fanin0: usize,
    fanin1: usize,
    fanin2: usize,
    inversions: u8,
}

#[derive(Debug, Clone, Copy)]
struct DeterministicGenerator(u64);

impl DeterministicGenerator {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn index(&mut self, bound: usize) -> usize {
        usize::try_from(self.next() % bound as u64).expect("generated index fits usize")
    }

    #[allow(
        clippy::cast_possible_truncation,
        reason = "the property generator intentionally uses the low native word of its u64 PRNG"
    )]
    fn gate(&mut self) -> RandomGate {
        RandomGate {
            kind: u8::try_from(self.next() % 3).expect("gate kind is bounded"),
            fanin0: self.next() as usize,
            fanin1: self.next() as usize,
            fanin2: self.next() as usize,
            inversions: u8::try_from(self.next() & 7).expect("inversion mask is bounded"),
        }
    }
}

fn runtime() -> &'static ExecutionContext {
    static RUNTIME: OnceLock<ExecutionContext> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        ExecutionContext::new(&ExecutionConfig { max_threads: 1 })
            .expect("property-test runtime is valid")
    })
}

fn maybe_invert(value: LogicNodeId, inversions: u8, bit: u8) -> LogicNodeId {
    if inversions & (1 << bit) == 0 {
        value
    } else {
        value.inverted()
    }
}

#[test]
fn rewrite_and_balance_preserve_deterministically_generated_logic_networks() {
    let mut generator = DeterministicGenerator(0x4f50_544f_5f4c_4f47);
    for case in 0..256 {
        let input_count = generator.index(6) + 1;
        let gate_count = generator.index(47) + 1;
        let mut network = LogicGraph::new();
        let mut values = (0..input_count)
            .map(|index| network.variable(index).expect("bounded input index"))
            .collect::<Vec<_>>();
        for _ in 0..gate_count {
            let gate = generator.gate();
            let left = maybe_invert(values[gate.fanin0 % values.len()], gate.inversions, 0);
            let right = maybe_invert(values[gate.fanin1 % values.len()], gate.inversions, 1);
            let third = maybe_invert(values[gate.fanin2 % values.len()], gate.inversions, 2);
            let value = match gate.kind {
                0 => network.and(left, right),
                1 => network.xor(left, right),
                _ => network.mux(left, right, third),
            };
            values.push(value);
        }
        let root = *values.last().expect("at least one generated gate");
        network.freeze();

        let optimized = optimize_network(
            &network,
            &[root],
            &[None],
            crate::SynthesisDiagnostics::default(),
            runtime(),
        )
        .expect("bounded generated network optimizes");
        let optimized_root =
            remap_literal(&optimized.remap, root).expect("live generated root is preserved");
        let proof = prove_logic_network_equivalence(
            network.storage_network(),
            &[root.lit()],
            optimized.network.storage_network(),
            &[optimized_root.lit()],
        )
        .expect("formal engine accepts generated miter");
        assert!(
            proof.require_proved().is_ok(),
            "deterministic generated case {case} was not equivalent"
        );
    }
}
