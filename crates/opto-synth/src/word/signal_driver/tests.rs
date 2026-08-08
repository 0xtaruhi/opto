// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn bit_ranged_connects_resolve_per_bit_instead_of_becoming_opaque() {
    let mut module = word::WordModule::new("top");
    let bit = word::WordType::bits(1).unwrap();
    let word2 = word::WordType::bits(2).unwrap();
    let [low, high] = ["low", "high"].map(|name| {
        let port = module
            .add_port(
                name,
                word::PortDirection::Input,
                bit,
                word::SourceSpan::default(),
            )
            .unwrap();
        module
            .read_signal(
                module.port(port).unwrap().signal,
                word::SourceSpan::default(),
            )
            .unwrap()
    });
    let packed = module
        .add_wire("packed", word2, word::SourceSpan::default())
        .unwrap();
    for (lsb, driver) in [(0, low), (1, high)] {
        module
            .connect(
                word::LValue::signal(packed).with_range(word::BitRange { msb: lsb, lsb }),
                driver,
                word::SourceSpan::default(),
            )
            .unwrap();
    }

    let drivers = SignalDriverIndex::new(&module).unwrap();
    let mut reference = |lsb, width| {
        let value = module
            .read_signal_slice(packed, lsb, width, word::SourceSpan::default())
            .unwrap();
        match &module.value(value).unwrap().kind {
            word::ValueKind::Signal(reference) => *reference,
            other => panic!("expected a signal reference, got {other:?}"),
        }
    };

    let low_bit = reference(0, 1);
    let high_bit = reference(1, 1);
    let both = reference(0, 2);
    assert_eq!(drivers.resolve_reference(low_bit), Some(vec![(low, 0)]));
    assert_eq!(drivers.resolve_reference(high_bit), Some(vec![(high, 0)]));
    assert_eq!(
        drivers.resolve_reference(both),
        Some(vec![(low, 0), (high, 0)])
    );
    assert_eq!(drivers.reference_drivers(both), Some(vec![low, high]));
}

#[test]
fn a_descending_connect_range_maps_driver_bits_in_reverse() {
    let mut module = word::WordModule::new("top");
    let word2 = word::WordType::bits(2).unwrap();
    let source = module
        .add_port(
            "source",
            word::PortDirection::Input,
            word2,
            word::SourceSpan::default(),
        )
        .unwrap();
    let source = module
        .read_signal(
            module.port(source).unwrap().signal,
            word::SourceSpan::default(),
        )
        .unwrap();
    let packed = module
        .add_wire("packed", word2, word::SourceSpan::default())
        .unwrap();
    module
        .connect(
            word::LValue::signal(packed).with_range(word::BitRange { msb: 0, lsb: 1 }),
            source,
            word::SourceSpan::default(),
        )
        .unwrap();

    let drivers = SignalDriverIndex::new(&module).unwrap();
    let mut reference = |lsb| {
        let value = module
            .read_signal_slice(packed, lsb, 1, word::SourceSpan::default())
            .unwrap();
        match &module.value(value).unwrap().kind {
            word::ValueKind::Signal(reference) => *reference,
            other => panic!("expected a signal reference, got {other:?}"),
        }
    };

    let low = reference(0);
    let high = reference(1);
    assert_eq!(drivers.resolve_reference(low), Some(vec![(source, 1)]));
    assert_eq!(drivers.resolve_reference(high), Some(vec![(source, 0)]));
}

#[test]
fn a_bit_driven_by_two_connects_is_unresolved() {
    let mut module = word::WordModule::new("top");
    let bit = word::WordType::bits(1).unwrap();
    let word2 = word::WordType::bits(2).unwrap();
    let [first, second] = ["first", "second"].map(|name| {
        let port = module
            .add_port(
                name,
                word::PortDirection::Input,
                bit,
                word::SourceSpan::default(),
            )
            .unwrap();
        module
            .read_signal(
                module.port(port).unwrap().signal,
                word::SourceSpan::default(),
            )
            .unwrap()
    });
    let packed = module
        .add_wire("packed", word2, word::SourceSpan::default())
        .unwrap();
    for driver in [first, second] {
        module
            .connect(
                word::LValue::signal(packed).with_range(word::BitRange { msb: 0, lsb: 0 }),
                driver,
                word::SourceSpan::default(),
            )
            .unwrap();
    }

    let drivers = SignalDriverIndex::new(&module).unwrap();
    let value = module
        .read_signal_slice(packed, 0, 1, word::SourceSpan::default())
        .unwrap();
    let word::ValueKind::Signal(reference) = &module.value(value).unwrap().kind else {
        panic!("expected a signal reference");
    };

    assert_eq!(drivers.resolve_reference(*reference), None);
}

#[test]
fn a_dynamically_targeted_signal_stays_opaque_for_every_bit() {
    let mut module = word::WordModule::new("top");
    let bit = word::WordType::bits(1).unwrap();
    let word2 = word::WordType::bits(2).unwrap();
    let index = module
        .add_port(
            "index",
            word::PortDirection::Input,
            bit,
            word::SourceSpan::default(),
        )
        .unwrap();
    let index = module
        .read_signal(
            module.port(index).unwrap().signal,
            word::SourceSpan::default(),
        )
        .unwrap();
    let data = module
        .add_port(
            "data",
            word::PortDirection::Input,
            word2,
            word::SourceSpan::default(),
        )
        .unwrap();
    let data = module
        .read_signal(
            module.port(data).unwrap().signal,
            word::SourceSpan::default(),
        )
        .unwrap();
    let packed = module
        .add_wire("packed", word2, word::SourceSpan::default())
        .unwrap();
    // The driver is as wide as the signal, so only the dynamic offset makes the
    // covered bits unknowable. A width check alone would not reject this.
    module
        .connect(
            word::LValue::signal(packed)
                .with_dynamic_range(index, std::num::NonZeroU32::new(2).unwrap()),
            data,
            word::SourceSpan::default(),
        )
        .unwrap();

    let drivers = SignalDriverIndex::new(&module).unwrap();
    let value = module
        .read_signal_slice(packed, 0, 1, word::SourceSpan::default())
        .unwrap();
    let word::ValueKind::Signal(reference) = module.value(value).unwrap().kind else {
        panic!("expected a signal reference");
    };

    assert_eq!(drivers.resolve_reference(reference), None);
}
