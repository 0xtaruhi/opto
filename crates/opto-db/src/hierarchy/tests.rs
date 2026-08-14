// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

fn definition(name: &str, instances: &[(&str, &str)]) -> DefinitionInput {
    DefinitionInput::new(
        name,
        instances
            .iter()
            .map(|(instance, reference)| InstanceInput::new(*instance, *reference))
            .collect(),
    )
}

fn first_instance_binding(graph: &DefinitionGraph) -> LinkBinding {
    let occurrences = OccurrenceGraph::materialize(graph).unwrap();
    let first = occurrences.ids().nth(1).unwrap();
    occurrences.link_binding(first).unwrap()
}

#[test]
fn structural_equality_does_not_merge_runtime_ownership() {
    let build = || {
        DefinitionGraph::build(
            [
                definition("top", &[("u0", "leaf")]),
                definition("leaf", &[]),
            ],
            [LinkProviderInput::definitions("*")],
            "top",
        )
        .unwrap()
    };
    let first = build();
    let second = build();
    assert_eq!(first, second);

    let occurrences = OccurrenceGraph::materialize(&first).unwrap();
    let child = occurrences.ids().nth(1).unwrap();
    assert!(occurrences.instance(&second, child).is_none());
    assert!(occurrences.format_path(&second, child).is_none());
}

#[test]
fn repeated_hierarchy_is_counted_without_expanding_definitions() {
    let graph = DefinitionGraph::build(
        [
            definition("top", &[("u0", "mid"), ("u1", "mid")]),
            definition("mid", &[("u_leaf", "leaf")]),
            definition("leaf", &[]),
        ],
        [LinkProviderInput::definitions("*")],
        "top",
    )
    .unwrap();

    let mid = graph.definition_id("mid").unwrap();
    let leaf = graph.definition_id("leaf").unwrap();
    assert_eq!(graph.len(), 3);
    assert_eq!(graph.occurrence_count(mid), 2);
    assert_eq!(graph.occurrence_count(leaf), 2);
    assert_eq!(
        graph
            .postorder()
            .iter()
            .map(|id| graph.definition_name(*id))
            .collect::<Vec<_>>(),
        ["leaf", "mid", "top"]
    );
    let occurrences = OccurrenceGraph::materialize(&graph).unwrap();
    assert_eq!(OccurrenceGraph::node_count(&graph).unwrap(), 5);
    assert_eq!(occurrences.len(), 5);
    assert_eq!(occurrences.root(), OccurrenceId::ROOT);
    assert_eq!(occurrences.parent(OccurrenceId::ROOT), None);
    assert_eq!(occurrences.instance_ordinal(OccurrenceId::ROOT), None);
    assert_eq!(occurrences.link_binding(OccurrenceId::ROOT), None);
    assert_eq!(
        occurrences.bound_definition(OccurrenceId::ROOT),
        Some(graph.root())
    );
    assert!(occurrences.instance(&graph, OccurrenceId::ROOT).is_none());
    let ids = occurrences.ids().collect::<Vec<_>>();
    assert_eq!(occurrences.parent(ids[1]), Some(OccurrenceId::ROOT));
    assert_eq!(occurrences.owner_definition(ids[1]), Some(graph.root()));
    assert_eq!(occurrences.instance_ordinal(ids[1]), Some(0));
    assert_eq!(occurrences.parent(ids[2]), Some(ids[1]));
    assert_eq!(occurrences.owner_definition(ids[2]), Some(mid));
    assert_eq!(occurrences.instance_ordinal(ids[2]), Some(0));
    assert_eq!(occurrences.parent(ids[3]), Some(OccurrenceId::ROOT));
    assert_eq!(occurrences.instance_ordinal(ids[3]), Some(1));
    assert_eq!(
        occurrences
            .ids()
            .skip(1)
            .map(|occurrence| occurrences.format_path(&graph, occurrence).unwrap())
            .collect::<Vec<_>>(),
        ["u0", "u0/u_leaf", "u1", "u1/u_leaf"]
    );
}

#[test]
fn hierarchy_text_is_interned_once_and_instances_remain_compact() {
    let graph = DefinitionGraph::build(
        [
            definition("top", &[("u0", "child"), ("u1", "child")]),
            definition("child", &[]),
        ],
        [LinkProviderInput::definitions("*")],
        "top",
    )
    .unwrap();
    let child = graph.definition_id("child").unwrap();
    let first = graph.instance(graph.root(), 0).unwrap();
    let second = graph.instance(graph.root(), 1).unwrap();

    assert_eq!(first.reference_id(), second.reference_id());
    assert_eq!(first.reference_id(), graph.definition(child).name);
    assert_eq!(first.reference(), "child");
    assert_eq!(graph.names.entry_count(), 6); // "", top, child, u0, u1, *
    assert_eq!(
        graph.definition_by_name[first.reference_id().raw() as usize],
        Some(child)
    );
    assert_eq!(std::mem::size_of::<NameId>(), 4);
    assert!(std::mem::size_of::<StoredDefinitionInstance>() <= 24);
}

#[test]
fn occurrence_materialization_rejects_expansion_beyond_dense_id_capacity() {
    let mut definitions = vec![definition("level0", &[])];
    for depth in 1..=32 {
        definitions.push(DefinitionInput::new(
            format!("level{depth}"),
            vec![
                InstanceInput::new("left", format!("level{}", depth - 1)),
                InstanceInput::new("right", format!("level{}", depth - 1)),
            ],
        ));
    }
    let graph = DefinitionGraph::build(
        definitions,
        [LinkProviderInput::definitions("*")],
        "level32",
    )
    .unwrap();

    assert_eq!(
        OccurrenceGraph::node_count(&graph),
        Err(OccurrenceGraphError::Capacity)
    );
    assert!(matches!(
        OccurrenceGraph::materialize(&graph),
        Err(OccurrenceGraphError::Capacity)
    ));
}

#[test]
fn unresolved_references_are_counted_by_occurrence() {
    let graph = DefinitionGraph::build(
        [
            definition("top", &[("u0", "mid"), ("u1", "mid")]),
            definition("mid", &[("u_missing", "missing")]),
        ],
        [LinkProviderInput::definitions("*")],
        "top",
    )
    .unwrap();

    assert_eq!(graph.unresolved_occurrence_count(), 2);
    let first = graph.first_unresolved().unwrap();
    assert_eq!(first.path(), "u0/u_missing");
    assert_eq!(first.reference(), "missing");
}

#[test]
fn first_unresolved_does_not_expand_a_shared_occurrence_tree() {
    let mut definitions = vec![definition("clean0", &[])];
    for depth in 1..=40 {
        definitions.push(definition(
            &format!("clean{depth}"),
            &[
                ("left", &format!("clean{}", depth - 1)),
                ("right", &format!("clean{}", depth - 1)),
            ],
        ));
    }
    definitions.push(definition(
        "top",
        &[("shared", "clean40"), ("u_missing", "missing")],
    ));
    let graph =
        DefinitionGraph::build(definitions, [LinkProviderInput::definitions("*")], "top").unwrap();

    let first = graph.first_unresolved().unwrap();
    assert_eq!(first.path(), "u_missing");
    assert_eq!(first.reference(), "missing");
}

#[test]
fn library_references_are_linked_leaves() {
    let graph = DefinitionGraph::build(
        [definition("top", &[("u_buf", "BUF_X1")])],
        [LinkProviderInput::external(
            "demo.lib",
            ["BUF_X1".to_string()],
        )],
        "top",
    )
    .unwrap();

    assert!(graph.is_linked());
    let provider = graph.providers().next().unwrap().id();
    assert_eq!(
        first_instance_binding(&graph),
        LinkBinding::External { provider }
    );
    let occurrences = OccurrenceGraph::materialize(&graph).unwrap();
    let external = occurrences.ids().nth(1).unwrap();
    assert_eq!(occurrences.parent(external), Some(OccurrenceId::ROOT));
    assert_eq!(occurrences.bound_definition(external), None);

    let designs = OccurrenceGraph::materialize_designs(&graph).unwrap();
    assert_eq!(OccurrenceGraph::design_node_count(&graph).unwrap(), 1);
    assert_eq!(designs.len(), 1);
}

#[test]
fn design_occurrences_preserve_source_ordinals_around_external_leaves() {
    let graph = DefinitionGraph::build(
        [
            definition("top", &[("u_buffer", "BUF_X1"), ("u_child", "child")]),
            definition("child", &[]),
        ],
        [
            LinkProviderInput::definitions("*"),
            LinkProviderInput::external("demo.lib", ["BUF_X1".to_string()]),
        ],
        "top",
    )
    .unwrap();

    let occurrences = OccurrenceGraph::materialize_designs(&graph).unwrap();
    let child = occurrences.ids().nth(1).unwrap();
    assert_eq!(occurrences.len(), 2);
    assert_eq!(occurrences.instance_ordinal(child), Some(1));
    assert_eq!(
        occurrences.format_path(&graph, child).as_deref(),
        Some("u_child")
    );
}

#[test]
fn occurrence_lookups_reject_ids_outside_the_graph() {
    let source_graph = DefinitionGraph::build(
        [
            definition("top", &[("u_child", "child")]),
            definition("child", &[]),
        ],
        [LinkProviderInput::definitions("*")],
        "top",
    )
    .unwrap();
    let source = OccurrenceGraph::materialize_designs(&source_graph).unwrap();
    let foreign = source.ids().nth(1).unwrap();

    let target_graph = DefinitionGraph::build(
        [definition("top", &[])],
        [LinkProviderInput::definitions("*")],
        "top",
    )
    .unwrap();
    let target = OccurrenceGraph::materialize_designs(&target_graph).unwrap();

    assert!(!target.contains(foreign));
    assert_eq!(target.parent(foreign), None);
    assert_eq!(target.owner_definition(foreign), None);
    assert_eq!(target.instance_ordinal(foreign), None);
    assert_eq!(target.link_binding(foreign), None);
    assert_eq!(target.bound_definition(foreign), None);
    assert!(target.instance(&target_graph, foreign).is_none());
    assert!(target.format_path(&target_graph, foreign).is_none());
    assert!(source.format_path(&target_graph, foreign).is_none());
}

#[test]
fn occurrence_lookups_reject_a_foreign_graph_with_matching_dense_ids() {
    let source_graph = DefinitionGraph::build(
        [
            definition("top", &[("u_source", "child")]),
            definition("child", &[]),
        ],
        [LinkProviderInput::definitions("*")],
        "top",
    )
    .unwrap();
    let source = OccurrenceGraph::materialize_designs(&source_graph).unwrap();
    let child = source.ids().nth(1).unwrap();

    let foreign_graph = DefinitionGraph::build(
        [
            definition("top", &[("u_foreign", "child")]),
            definition("child", &[]),
        ],
        [LinkProviderInput::definitions("*")],
        "top",
    )
    .unwrap();

    assert!(source.instance(&foreign_graph, child).is_none());
    assert!(source.format_path(&foreign_graph, child).is_none());
    assert_eq!(
        source.instance(&source_graph, child).unwrap().name(),
        "u_source"
    );
}

#[test]
fn recursive_hierarchy_reports_the_definition_cycle() {
    let error = DefinitionGraph::build(
        [
            definition("top", &[("u_a", "a")]),
            definition("a", &[("u_b", "b")]),
            definition("b", &[("u_a", "a")]),
        ],
        [LinkProviderInput::definitions("*")],
        "top",
    )
    .unwrap_err();

    assert_eq!(
        error.to_string(),
        "recursive design hierarchy is not supported: a -> b -> a"
    );
}

#[test]
fn external_provider_before_definitions_wins_by_order() {
    let graph = DefinitionGraph::build(
        [
            definition("top", &[("u_child", "child")]),
            definition("child", &[]),
        ],
        [
            LinkProviderInput::external("cells.lib", ["child".to_string()]),
            LinkProviderInput::definitions("*"),
        ],
        "top",
    )
    .unwrap();

    let selected = graph.providers().next().unwrap().id();
    assert_eq!(
        first_instance_binding(&graph),
        LinkBinding::External { provider: selected }
    );
    assert_eq!(
        graph
            .postorder()
            .iter()
            .map(|id| graph.definition_name(*id))
            .collect::<Vec<_>>(),
        ["top"]
    );
}

#[test]
fn definitions_provider_before_external_wins_by_order() {
    let graph = DefinitionGraph::build(
        [
            definition("top", &[("u_child", "child")]),
            definition("child", &[]),
        ],
        [
            LinkProviderInput::definitions("*"),
            LinkProviderInput::external("cells.lib", ["child".to_string()]),
        ],
        "top",
    )
    .unwrap();

    let provider = graph.providers().next().unwrap().id();
    let child = graph.definition_id("child").unwrap();
    assert_eq!(
        first_instance_binding(&graph),
        LinkBinding::Design {
            provider,
            definition: child,
        }
    );
    assert_eq!(
        graph
            .postorder()
            .iter()
            .map(|id| graph.definition_name(*id))
            .collect::<Vec<_>>(),
        ["child", "top"]
    );
}

#[test]
fn definitions_provider_in_the_middle_wins_after_an_unmatched_library() {
    let graph = DefinitionGraph::build(
        [
            definition("top", &[("u_child", "child")]),
            definition("child", &[]),
        ],
        [
            LinkProviderInput::external("first.lib", ["OTHER".to_string()]),
            LinkProviderInput::definitions("*"),
            LinkProviderInput::external("second.lib", ["child".to_string()]),
        ],
        "top",
    )
    .unwrap();

    let provider = graph.providers().nth(1).unwrap().id();
    let child = graph.definition_id("child").unwrap();
    assert_eq!(
        first_instance_binding(&graph),
        LinkBinding::Design {
            provider,
            definition: child,
        }
    );
    assert!(graph.names.get("OTHER").is_none());
}

#[test]
fn definitions_are_not_searched_without_a_definitions_provider() {
    let graph = DefinitionGraph::build(
        [
            definition("top", &[("u_child", "child")]),
            definition("child", &[]),
        ],
        [LinkProviderInput::external(
            "cells.lib",
            ["OTHER".to_string()],
        )],
        "top",
    )
    .unwrap();

    assert_eq!(
        graph.instance(graph.root(), 0).unwrap().binding(),
        LinkBinding::Unresolved
    );
    assert!(matches!(
        OccurrenceGraph::materialize(&graph),
        Err(OccurrenceGraphError::UnresolvedHierarchy { occurrences: 1 })
    ));
    assert!(!graph.is_reachable(graph.definition_id("child").unwrap()));
    assert_eq!(graph.unresolved_occurrence_count(), 1);
}

#[test]
fn repeated_external_definitions_use_the_first_provider_once() {
    let graph = DefinitionGraph::build(
        [definition(
            "top",
            &[("u_first", "BUF_X1"), ("u_second", "BUF_X1")],
        )],
        [
            LinkProviderInput::external("first.lib", ["BUF_X1".to_string()]),
            LinkProviderInput::external("second.lib", ["BUF_X1".to_string()]),
        ],
        "top",
    )
    .unwrap();

    let first = graph.providers().next().unwrap().id();
    let occurrences = OccurrenceGraph::materialize(&graph).unwrap();
    assert!(occurrences.ids().skip(1).all(|occurrence| {
        occurrences.link_binding(occurrence) == Some(LinkBinding::External { provider: first })
    }));
}

#[test]
fn duplicate_definition_names_are_a_true_ambiguity() {
    let error = DefinitionGraph::build(
        [
            definition("top", &[("u_child", "child")]),
            definition("child", &[]),
            definition("child", &[]),
        ],
        [LinkProviderInput::definitions("*")],
        "top",
    )
    .unwrap_err();

    assert_eq!(
        error,
        DefinitionGraphError::DuplicateDefinition("child".to_string())
    );
}
