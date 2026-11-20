// SPDX-License-Identifier: CC0-1.0

//! Which services a TRACE SUMMARY reports, end to end over the wire (the `Xray`
//! harness in [`super::xray_wire`]).
//!
//! The rule: the services a trace touched are the NODES of that trace's own
//! service graph, so a summary's `ServiceIds` carry one entry per node —
//! identified by the service's name TOGETHER with the type it was reported with
//! — including the node `Service$Type` documents as `client`, which "represents
//! the clients that sent requests to a root service"
//! (`smithy_query(query="shape",shape="Service",member="Type")`) and which
//! carries that root service's own name. A root segment's caller reported no
//! segment of its own, so that entry exists in no stored document and can only
//! come from the derivation the service map already makes.
//!
//! What TYPES the root entry is the `origin` its segment document carried (the
//! X-Ray segment-documents reference's optional `origin` field, which
//! `GetServiceGraph` reports as `Service$Type`), so the rows below differ along
//! that origin — including a document that carries none — and along whether two
//! frames published under ONE name were reported from different hosts, which is
//! exactly what `origin` exists to express.

use std::collections::BTreeSet;

use serde_json::{json, Value};

use super::xray_wire::{array, root_document, trace_id, Xray};

/// The window every frame below falls inside, well in the past, so no published
/// bound of either read is at stake here.
const WINDOW: (f64, f64) = (1_000.0, 2_000.0);

/// The service that publishes the root segments.
const SERVICE: &str = "flint-motion";

/// `Service$Type` for the node that stands for the callers of a root service.
const CLIENT: &str = "client";

/// A root segment document for `SERVICE`, carrying `origin` when one is given
/// and one downstream call out of the process.
fn frame(id: &str, trace: &str, origin: Option<&str>) -> Value {
    let mut document = root_document(id, trace, SERVICE, 1_000.0, Some(1_021.0));
    if let Some(origin) = origin {
        document["origin"] = json!(origin);
    }
    document["http"] = json!({
        "request": {"method": "GET", "url": "http://localhost/probe"},
        "response": {"status": 200},
    });
    document["subsegments"] = json!([{
        "id": "aaaaaaaaaaaaaaaa",
        "name": "QuartzAPI",
        "namespace": "remote",
        "start_time": 1_000.5_f64,
        "end_time": 1_019.5_f64,
    }]);
    document
}

/// The sorted `Type`s of the entries of `summary`'s `ServiceIds` that are named
/// for `name`, with `""` standing for an entry that omits the member.
fn types_named_for(summary: &Value, name: &str) -> Vec<String> {
    let mut types: Vec<String> = array(summary, "ServiceIds")
        .iter()
        .filter(|id| id.get("Name").and_then(Value::as_str) == Some(name))
        .map(|id| {
            id.get("Type")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        })
        .collect();
    types.sort();
    types
}

/// The sorted `Type`s of the service-graph nodes named for `name` over the
/// window.
async fn graph_types_named_for(xray: &Xray, name: &str) -> Vec<String> {
    let graph = xray
        .call(
            "/ServiceGraph",
            &json!({ "StartTime": WINDOW.0, "EndTime": WINDOW.1 }),
        )
        .await;
    assert_eq!(
        graph.status, 200,
        "the service graph must be readable, got {graph:?}"
    );
    let mut types: Vec<String> = array(&graph.body, "Services")
        .iter()
        .filter(|node| node.get("Name").and_then(Value::as_str) == Some(name))
        .map(|node| {
            node.get("Type")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        })
        .collect();
    types.sort();
    types
}

/// The one summary of the trace `trace`, from a query over the window.
async fn summary_of(xray: &Xray, case: &str, trace: &str) -> Value {
    xray.summary_of(case, WINDOW.0, WINDOW.1, trace).await
}

#[tokio::test]
async fn a_summary_lists_the_root_service_and_the_client_that_called_it() {
    // The rows differ along the `origin` the root document carried: two host
    // types AWS publishes, one neither the issue nor the graph suite names, and
    // a document carrying none at all.
    let origins = [
        Some("AWS::ECS::Container"),
        Some("AWS::EC2::Instance"),
        Some("AWS::ElasticBeanstalk::Environment"),
        None,
    ];
    for (index, origin) in origins.into_iter().enumerate() {
        let xray = Xray::new();
        let nth = u32::try_from(index).expect("the row count fits a u32") + 1;
        let trace = trace_id(nth);
        let case = format!("a root segment reporting origin {origin:?}");

        let put = xray
            .put_segments(&[frame("1111111111111111", &trace, origin).to_string()])
            .await;
        assert_eq!(
            put.status, 200,
            "case {case:?}: seeding must succeed, got {put:?}"
        );
        assert!(
            array(&put.body, "UnprocessedTraceSegments").is_empty(),
            "case {case:?}: the document must be accepted, got {put:?}"
        );

        let summary = summary_of(&xray, &case, &trace).await;
        let actual = types_named_for(&summary, SERVICE);
        let mut expected = vec![origin.unwrap_or_default().to_string(), CLIENT.to_string()];
        expected.sort();
        assert_eq!(
            actual, expected,
            "case {case:?}: expected the root entry typed for its origin beside the synthesised \
             caller, {expected:?}, got {actual:?}"
        );

        // The same pair the service map draws for the same submission: one node
        // set, two readers.
        let drawn = graph_types_named_for(&xray, SERVICE).await;
        assert_eq!(
            actual, drawn,
            "case {case:?}: the summary's services must be the graph's nodes, got {actual:?} and \
             {drawn:?}"
        );

        // The downstream service the frame called is one node of its own, typed
        // by the call rather than by an origin it never carried.
        let downstream = types_named_for(&summary, "QuartzAPI");
        assert_eq!(
            downstream,
            vec!["remote".to_string()],
            "case {case:?}: the called service is listed once, typed by the call, got \
             {downstream:?}"
        );
    }
}

#[tokio::test]
async fn two_frames_of_one_name_reported_from_different_hosts_stay_distinct() {
    let xray = Xray::new();
    let frames = [
        ("AWS::ECS::Container", trace_id(11), "1111111111111111"),
        ("AWS::EC2::Instance", trace_id(12), "2222222222222222"),
    ];
    for (origin, trace, id) in &frames {
        let put = xray
            .put_segments(&[frame(id, trace, Some(origin)).to_string()])
            .await;
        assert_eq!(
            put.status, 200,
            "case {origin:?}: seeding must succeed, got {put:?}"
        );
        assert!(
            array(&put.body, "UnprocessedTraceSegments").is_empty(),
            "case {origin:?}: the document must be accepted, got {put:?}"
        );
    }

    // Each summary reports the host ITS frame was published from, beside the
    // caller of that frame — so a reader can tell the two apart, which is what
    // `origin` is for.
    for (origin, trace, _) in &frames {
        let case = format!("the frame published from {origin}");
        let summary = summary_of(&xray, &case, trace).await;
        let actual = types_named_for(&summary, SERVICE);
        let mut expected = vec![(*origin).to_string(), CLIENT.to_string()];
        expected.sort();
        assert_eq!(
            actual, expected,
            "case {case:?}: expected {expected:?}, got {actual:?}"
        );
    }

    // And a query selecting the service BY that type selects only its frame.
    for (origin, trace, _) in &frames {
        let expression = format!("service(id(name: \"{SERVICE}\", type: \"{origin}\"))");
        let answered = xray.filtered(WINDOW.0, WINDOW.1, &expression).await;
        assert_eq!(
            answered.status, 200,
            "case {expression:?}: the expression must be readable, got {answered:?}"
        );
        let selected: Vec<&str> = array(&answered.body, "TraceSummaries")
            .iter()
            .filter_map(|summary| summary.get("Id").and_then(Value::as_str))
            .collect();
        assert_eq!(
            selected,
            vec![trace.as_str()],
            "case {expression:?}: only the frame published from that host may be selected, got \
             {selected:?}"
        );
    }

    // Both typed root nodes stand in the one service map, beside one client node
    // per root.
    let drawn: BTreeSet<String> = graph_types_named_for(&xray, SERVICE).await.into_iter().collect();
    let expected: BTreeSet<String> = [
        "AWS::ECS::Container".to_string(),
        "AWS::EC2::Instance".to_string(),
        CLIENT.to_string(),
    ]
    .into_iter()
    .collect();
    assert_eq!(
        drawn, expected,
        "case \"the service map\": expected {expected:?}, got {drawn:?}"
    );
}

/// One row of the mixed-origin table below: the case, the `origin` the root
/// document reports, the one the second document of that same service reports,
/// and the `Type`s the service's own ServiceIds entries must carry.
type MixedOriginRow = (
    &'static str,
    Option<&'static str>,
    Option<&'static str>,
    Vec<&'static str>,
);

#[tokio::test]
async fn a_document_that_reports_no_origin_joins_the_node_its_service_reported() {
    // A service's `origin` describes the host its instrumentation ran on, so a
    // document that carried none reports nothing about that host rather than
    // reporting a different one: it belongs to the node its service's other
    // documents reported. The rows differ along WHICH of one service's documents
    // carries the origin — the root, the child, or both — and the last row is the
    // case that genuinely says two hosts.
    let rows: [MixedOriginRow; 4] = [
        (
            "the root reports the origin, its child none",
            Some("AWS::ECS::Container"),
            None,
            vec!["AWS::ECS::Container", CLIENT],
        ),
        (
            "the child reports the origin, the root none",
            None,
            Some("AWS::ECS::Container"),
            vec!["AWS::ECS::Container", CLIENT],
        ),
        (
            "both report the same origin",
            Some("AWS::EC2::Instance"),
            Some("AWS::EC2::Instance"),
            vec!["AWS::EC2::Instance", CLIENT],
        ),
        (
            "the two documents report different hosts",
            Some("AWS::ECS::Container"),
            Some("AWS::EC2::Instance"),
            vec!["AWS::EC2::Instance", "AWS::ECS::Container", CLIENT],
        ),
    ];

    for (index, (case, root_origin, child_origin, expected)) in rows.into_iter().enumerate() {
        let xray = Xray::new();
        let nth = u32::try_from(index).expect("the row count fits a u32") + 21;
        let trace = trace_id(nth);

        // One service name, two documents of it: the root, and a second segment
        // of the same service beneath it.
        let root = frame("1111111111111111", &trace, root_origin);
        let mut child = root_document("2222222222222222", &trace, SERVICE, 1_001.0, Some(1_020.0));
        child["parent_id"] = json!("1111111111111111");
        if let Some(origin) = child_origin {
            child["origin"] = json!(origin);
        }
        let put = xray
            .put_segments(&[root.to_string(), child.to_string()])
            .await;
        assert_eq!(
            put.status, 200,
            "case {case:?}: seeding must succeed, got {put:?}"
        );
        assert!(
            array(&put.body, "UnprocessedTraceSegments").is_empty(),
            "case {case:?}: both documents must be accepted, got {put:?}"
        );

        let summary = summary_of(&xray, case, &trace).await;
        let actual = types_named_for(&summary, SERVICE);
        let mut expected: Vec<String> = expected.into_iter().map(str::to_string).collect();
        expected.sort();
        assert_eq!(
            actual, expected,
            "case {case:?}: expected ServiceIds types {expected:?}, got {actual:?}"
        );

        // The service map draws the same node set for the same documents.
        let drawn = graph_types_named_for(&xray, SERVICE).await;
        assert_eq!(
            drawn, expected,
            "case {case:?}: the graph's nodes must be the summary's services, got {drawn:?}"
        );
    }
}
