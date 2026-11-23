// SPDX-License-Identifier: CC0-1.0

//! Unit cover for the two decisions this module takes: WHICH engine a query text reaches,
//! and how a graph's stored data crosses into the engine that writes it and back.
//!
//! What a query ANSWERS over the wire is pinned by
//! `tests/all/execute_query_data_plane.rs`, end to end through the router; these are the
//! facts that are about the conversion itself.

use ::std::collections::HashMap;
use ::std::time::{Duration, Instant, SystemTime};

use ::aws_smithy_types::{Document, Number};
use ::serde_json::json;

use super::*;

/// A deadline far enough out that no case here is decided by the clock.
fn soon() -> Instant {
    Instant::now() + Duration::from_secs(30)
}

/// A graph holding one labelled node carrying one property, and nothing else.
fn one_person() -> GraphData {
    let mut data = GraphData::empty(SystemTime::UNIX_EPOCH);
    let mut node = GraphNode::default();
    node.labels.insert("Person".to_string());
    node.properties
        .insert("name".to_string(), json!("Alice"));
    data.insert_node("n1", node, SystemTime::UNIX_EPOCH)
        .expect("a graph takes one node");
    data
}

/// RULE: which engine reads a query follows from what the query SAYS and what it BINDS —
/// a clause that changes the graph, a `$name` the request binds data through, or a
/// relationship a pattern binds a variable to — and not from the characters in the text,
/// so a keyword or a `$` inside a string literal or a comment is not one.
#[test]
fn the_engine_is_chosen_from_what_the_query_says() {
    // (case, the query, whether the updating engine owns it)
    let rows: &[(&str, &str, bool)] = &[
        ("a bare projection", "RETURN 1 AS x", false),
        ("a read of the graph", "MATCH (p:Person) RETURN p.name AS name", false),
        ("a create", "CREATE (p:Person {name: 'Bob'})", true),
        ("a merge", "MERGE (p:Person {name: 'Bob'}) RETURN p", true),
        ("a set", "MATCH (p:Person) SET p.age = 25", true),
        ("a remove", "MATCH (p:Person) REMOVE p.age", true),
        ("a delete", "MATCH (p:Person) DELETE p", true),
        ("a detach delete", "MATCH (p:Person) DETACH DELETE p", true),
        (
            "a parameterized read",
            "MATCH (p:Person {name: $name}) RETURN p.name AS name",
            true,
        ),
        (
            "a parameter in a projection",
            "RETURN $name AS name",
            true,
        ),
        (
            "a parameter as a whole property map",
            "MATCH (p:Person $props) RETURN p",
            true,
        ),
        (
            "a clause keyword inside a string literal",
            "MATCH (p:Person) WHERE p.name = 'CREATE' RETURN p.name AS name",
            false,
        ),
        (
            "a dollar inside a string literal",
            "MATCH (p:Person) WHERE p.name = '$name' RETURN p.name AS name",
            false,
        ),
        (
            "a relationship a pattern traverses",
            "MATCH (a)-[r:KNOWS]->(b) RETURN r.since AS since",
            true,
        ),
        (
            "a relationship pattern binding no variable",
            "MATCH (a)-[:KNOWS]->(b) RETURN a.name AS name",
            true,
        ),
        (
            "a text that is no query at all",
            "MATCH (n RETURN n",
            false,
        ),
    ];
    for (case, query, updating) in rows {
        let owned = beyond_the_reading_engine(query);
        assert_eq!(
            owned, *updating,
            "case {case:?}: expected the updating engine to own it = {updating}, got {owned}"
        );
    }
}

/// RULE: the data a graph holds crosses into the engine and back BY ID, so every element
/// keeps the id it is addressed by, the labels it carries and every property set on it —
/// a query, a summary and a later query cannot disagree about an element.
#[test]
fn the_stored_data_crosses_into_the_engine_and_back_unchanged() {
    let mut data = GraphData::empty(SystemTime::UNIX_EPOCH);
    let mut city = GraphNode::default();
    city.labels.insert("City".to_string());
    city.labels.insert("Place".to_string());
    city.properties.insert("name".to_string(), json!("Berlin"));
    city.properties.insert("founded".to_string(), json!(1237));
    city.properties.insert("coastal".to_string(), json!(false));
    city.properties
        .insert("districts".to_string(), json!(["Mitte", "Kreuzberg"]));
    data.insert_node("c1", city, SystemTime::UNIX_EPOCH)
        .expect("a graph takes a node");
    data.insert_node("c2", GraphNode::default(), SystemTime::UNIX_EPOCH)
        .expect("a graph takes a node");
    data.insert_edge(
        "e1",
        GraphEdge {
            label: "ROAD".to_string(),
            from_node: "c1".to_string(),
            to_node: "c2".to_string(),
            properties: [("km".to_string(), json!(42))].into_iter().collect(),
        },
        SystemTime::UNIX_EPOCH,
    )
    .expect("a graph takes an edge");

    let graph = as_property_graph(&data).expect("the stored data is a property graph");
    let mut written = data.clone();
    commit(&mut written, &graph, SystemTime::UNIX_EPOCH).expect("the data is taken back");

    assert_eq!(
        written.nodes.keys().collect::<Vec<&String>>(),
        data.nodes.keys().collect::<Vec<&String>>(),
        "case \"the node ids\": expected the ids to survive the round trip"
    );
    for (id, node) in &data.nodes {
        let back = written.nodes.get(id).expect("every node comes back");
        assert_eq!(
            back.labels, node.labels,
            "case {id:?}: expected the labels to survive, got {:?}",
            back.labels
        );
        assert_eq!(
            back.properties, node.properties,
            "case {id:?}: expected the properties to survive, got {:?}",
            back.properties
        );
    }
    let back = written.edges.get("e1").expect("the edge comes back");
    assert_eq!(
        (
            back.label.as_str(),
            back.from_node.as_str(),
            back.to_node.as_str(),
        ),
        ("ROAD", "c1", "c2"),
        "case \"the edge\": expected the type and both endpoints to survive, got {back:?}"
    );
    assert_eq!(
        back.properties,
        [("km".to_string(), json!(42))].into_iter().collect(),
        "case \"the edge properties\": expected them to survive, got {:?}",
        back.properties
    );
}

/// RULE: a query that writes leaves the written data in the graph, and one that is
/// refused leaves the graph exactly as it was.
#[test]
fn a_write_lands_and_a_refusal_changes_nothing() {
    let mut data = one_person();
    let payload = run_updating_query(
        &mut data,
        "CREATE (p:Person {name: 'Bob', age: 25})",
        QueryParameters::new(),
        soon(),
        SystemTime::UNIX_EPOCH,
    )
    .expect("a create runs");
    assert_eq!(
        String::from_utf8(payload).expect("the payload is text"),
        r#"{"results":[]}"#,
        "case \"a bare create\": expected the clause's own (empty) result set"
    );
    assert_eq!(
        data.nodes.len(),
        2,
        "case \"a bare create\": expected the node to be stored, got {:?}",
        data.nodes
    );
    let created = data
        .nodes
        .values()
        .find(|node| node.properties.get("name") == Some(&json!("Bob")))
        .expect("the created node is in the graph");
    assert_eq!(
        created.properties.get("age"),
        Some(&json!(25)),
        "case \"a bare create\": expected the properties as written, got {:?}",
        created.properties
    );

    // A query the engine cannot evaluate changes nothing: the graph still holds exactly
    // the two nodes above.
    let before = data.nodes.clone();
    let refused = run_updating_query(
        &mut data,
        "CREATE (p:Person {name: nosuchfunction(1)})",
        QueryParameters::new(),
        soon(),
        SystemTime::UNIX_EPOCH,
    );
    assert!(
        refused.is_err(),
        "case \"a create naming an unknown function\": expected a refusal, got a result"
    );
    assert_eq!(
        data.nodes.len(),
        before.len(),
        "case \"a create naming an unknown function\": expected the graph to be unchanged, \
         got {:?}",
        data.nodes
    );
}

/// RULE: a `$name` binds the matching entry of `parameters` as DATA — the value is
/// matched and stored opaquely, whatever characters it carries — and a parameter set that
/// matches nothing answers no rows rather than a refusal.
#[test]
fn a_parameter_binds_its_value_as_data() {
    let mut data = GraphData::empty(SystemTime::UNIX_EPOCH);
    let parameters: QueryParameters = [
        ("name".to_string(), json!("O'Brien")),
        ("age".to_string(), json!(41)),
    ]
    .into_iter()
    .collect();
    run_updating_query(
        &mut data,
        "CREATE (p:Person {name: $name, age: $age})",
        parameters.clone(),
        soon(),
        SystemTime::UNIX_EPOCH,
    )
    .expect("a parameterized create runs");
    let stored = data
        .nodes
        .values()
        .next()
        .expect("the created node is in the graph");
    assert_eq!(
        stored.properties.get("name"),
        Some(&json!("O'Brien")),
        "case \"a value carrying the string delimiter\": expected it stored intact, got {:?}",
        stored.properties
    );

    let matched = run_updating_query(
        &mut data,
        "MATCH (p:Person {name: $name, age: $age}) RETURN p.name AS name, p.age AS age",
        parameters,
        soon(),
        SystemTime::UNIX_EPOCH,
    )
    .expect("a parameterized read runs");
    assert_eq!(
        String::from_utf8(matched).expect("the payload is text"),
        r#"{"results":[{"name":"O'Brien","age":41}]}"#,
        "case \"the same parameters\": expected the stored row back"
    );

    let other: QueryParameters = [
        ("name".to_string(), json!("Nobody")),
        ("age".to_string(), json!(41)),
    ]
    .into_iter()
    .collect();
    let empty = run_updating_query(
        &mut data,
        "MATCH (p:Person {name: $name, age: $age}) RETURN p.name AS name, p.age AS age",
        other,
        soon(),
        SystemTime::UNIX_EPOCH,
    )
    .expect("a parameter set matching nothing is still a query");
    assert_eq!(
        String::from_utf8(empty).expect("the payload is text"),
        r#"{"results":[]}"#,
        "case \"a parameter set that matches nothing\": expected no rows"
    );
}

/// RULE: a query referencing a parameter the request did not carry is refused as the
/// missing parameter it is, naming the parameter — never as a text that does not parse.
#[test]
fn an_unbound_parameter_is_reported_as_the_missing_parameter() {
    let mut data = one_person();
    let refused = run_updating_query(
        &mut data,
        "MATCH (p:Person {name: $name}) RETURN p",
        QueryParameters::new(),
        soon(),
        SystemTime::UNIX_EPOCH,
    )
    .expect_err("a query referencing a parameter nothing bound cannot run");
    match refused {
        QueryFailure::MissingParameter(name) => assert_eq!(
            name, "name",
            "case \"an unbound parameter\": expected the parameter to be named, got {name:?}"
        ),
        other => panic!("case \"an unbound parameter\": expected a missing parameter, got {other:?}"),
    }
}

/// RULE: a property value this graph holds is one of openCypher's scalars, so a write
/// that sets a property to a list or a map is refused and stores nothing — while a value
/// another path already stored under the same key is left alone, and a list a query only
/// ANSWERS with is not a property value at all.
#[test]
fn a_write_stores_only_scalar_property_values() {
    let mut data = one_person();
    // (case, the query, whether it is taken)
    let rows: &[(&str, &str, bool)] = &[
        ("a scalar", "CREATE (p:Person {name: 'Bob'})", true),
        ("a list", "CREATE (p:Person {tags: [1, 2]})", false),
        ("a map", "CREATE (p:Person {at: {lat: 1}})", false),
        (
            "a list set on a matched element",
            "MATCH (p:Person) SET p.tags = ['a']",
            false,
        ),
        (
            "a list the query only answers with",
            "CREATE (p:Person {name: 'Ada'}) RETURN [1, 2] AS pair",
            true,
        ),
    ];
    for (case, query, taken) in rows {
        let before = data.nodes.clone();
        let run = run_updating_query(
            &mut data,
            query,
            QueryParameters::new(),
            soon(),
            SystemTime::UNIX_EPOCH,
        );
        assert_eq!(
            run.is_ok(),
            *taken,
            "case {case:?}: expected taken = {taken}, got {run:?}"
        );
        if *taken {
            continue;
        }
        assert_eq!(
            data.nodes.len(),
            before.len(),
            "case {case:?}: a refused write must store nothing, got {:?}",
            data.nodes
        );
    }

    // A list another path stored is left alone by a write that touched something else.
    let mut loaded = GraphData::empty(SystemTime::UNIX_EPOCH);
    let mut node = GraphNode::default();
    node.labels.insert("Person".to_string());
    node.properties
        .insert("tags".to_string(), json!(["a", "b"]));
    loaded
        .insert_node("n1", node, SystemTime::UNIX_EPOCH)
        .expect("the store takes what a load wrote");
    run_updating_query(
        &mut loaded,
        "MATCH (p:Person) SET p.name = 'Bob'",
        QueryParameters::new(),
        soon(),
        SystemTime::UNIX_EPOCH,
    )
    .expect("a write beside a stored list is taken");
    assert_eq!(
        loaded.nodes["n1"].properties.get("tags"),
        Some(&json!(["a", "b"])),
        "case \"a stored list beside a write\": expected it left alone, got {:?}",
        loaded.nodes["n1"].properties
    );
}

/// RULE: a query that leaves the elements exactly as it found them writes NOTHING — not
/// the elements and not the instant the statistics over them were computed at, which a
/// later read reports. A query that did change them moves both.
#[test]
fn an_evaluation_that_changed_nothing_leaves_the_data_and_its_statistics_alone() {
    let computed_at = SystemTime::UNIX_EPOCH;
    let mut data = one_person();
    data.statistics_computed_at = computed_at;
    // (case, the query the engine evaluates, whether the data changes)
    let rows: &[(&str, &str, bool)] = &[
        (
            "a read through a relationship pattern",
            "MATCH (p:Person)-[:KNOWS]->(other) RETURN other.name AS name",
            false,
        ),
        (
            "a parameterized read",
            "MATCH (p:Person {name: $name}) RETURN p.name AS name",
            false,
        ),
        (
            "a merge that matched",
            "MERGE (p:Person {name: $name}) RETURN p.name AS name",
            false,
        ),
        (
            "a delete that matched nothing",
            "MATCH (p:Person {name: $other}) DELETE p",
            false,
        ),
        (
            "a set of a property that already holds that value",
            "MATCH (p:Person) SET p.name = $name",
            false,
        ),
        ("a create", "CREATE (p:Person {name: $other})", true),
    ];
    let parameters: QueryParameters = [
        ("name".to_string(), json!("Alice")),
        ("other".to_string(), json!("Grace")),
    ]
    .into_iter()
    .collect();
    for (case, query, changes) in rows {
        let before = data.nodes.clone();
        run_updating_query(
            &mut data,
            query,
            parameters.clone(),
            soon(),
            SystemTime::now(),
        )
        .unwrap_or_else(|refused| panic!("case {case:?}: expected a run, got {refused:?}"));
        if *changes {
            assert_ne!(
                data.nodes, before,
                "case {case:?}: expected the data to change"
            );
            assert_ne!(
                data.statistics_computed_at, computed_at,
                "case {case:?}: expected the statistics instant to move with the data"
            );
            continue;
        }
        assert_eq!(
            data.nodes, before,
            "case {case:?}: expected the data to be left alone"
        );
        assert_eq!(
            data.statistics_computed_at, computed_at,
            "case {case:?}: a query that changed nothing must leave the statistics instant \
             where it was"
        );
    }
}

/// RULE: every form a `Document` carries crosses into the data a parameter binds
/// unchanged, and a value nesting past what this endpoint reads is refused rather than
/// read.
#[test]
fn the_requests_parameters_are_read_as_the_data_they_are() {
    let supplied: HashMap<String, Document> = [
        ("text".to_string(), Document::String("Bob".to_string())),
        ("whole".to_string(), Document::Number(Number::PosInt(25))),
        ("signed".to_string(), Document::Number(Number::NegInt(-7))),
        ("fraction".to_string(), Document::Number(Number::Float(1.5))),
        ("flag".to_string(), Document::Bool(true)),
        ("nothing".to_string(), Document::Null),
        (
            "list".to_string(),
            Document::Array(vec![
                Document::Number(Number::PosInt(1)),
                Document::String("two".to_string()),
            ]),
        ),
        (
            "map".to_string(),
            Document::Object(
                [("key".to_string(), Document::String("value".to_string()))]
                    .into_iter()
                    .collect(),
            ),
        ),
    ]
    .into_iter()
    .collect();
    let read = request_parameters(Some(&supplied)).expect("every form is read");
    // (case, the name, the value it must bind)
    let rows: &[(&str, &str, ::serde_json::Value)] = &[
        ("a string", "text", json!("Bob")),
        ("a whole number", "whole", json!(25)),
        ("a negative number", "signed", json!(-7)),
        ("a fractional number", "fraction", json!(1.5)),
        ("a boolean", "flag", json!(true)),
        ("null", "nothing", ::serde_json::Value::Null),
        ("a list", "list", json!([1, "two"])),
        ("a map", "map", json!({"key": "value"})),
    ];
    for (case, name, expected) in rows {
        assert_eq!(
            read.get(*name),
            Some(expected),
            "case {case:?}: expected the value as sent, got {:?}",
            read.get(*name)
        );
    }
    assert!(
        request_parameters(None)
            .expect("a request carrying no parameters binds none")
            .is_empty(),
        "case \"no parameters at all\": expected no bindings"
    );

    // A value nesting one level past what a parameter is read through is refused, and one
    // at the bound is read.
    let mut nested = Document::Number(Number::PosInt(0));
    for _ in 0..MAX_PARAMETER_DEPTH {
        nested = Document::Array(vec![nested]);
    }
    let supplied: HashMap<String, Document> = [("deep".to_string(), nested.clone())]
        .into_iter()
        .collect();
    assert!(
        request_parameters(Some(&supplied)).is_err(),
        "case \"a parameter nesting past the bound\": expected a refusal"
    );
    let mut within = Document::Number(Number::PosInt(0));
    for _ in 0..(MAX_PARAMETER_DEPTH - 1) {
        within = Document::Array(vec![within]);
    }
    let supplied: HashMap<String, Document> = [("deep".to_string(), within)].into_iter().collect();
    assert!(
        request_parameters(Some(&supplied)).is_ok(),
        "case \"a parameter nesting at the bound\": expected it to be read"
    );
}
