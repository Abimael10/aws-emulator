// SPDX-License-Identifier: CC0-1.0

//! The Neptune Analytics graph — and the snapshot of one — a stack declares, end to end: both
//! resources exist in that service itself under the identities it minted, the values the types
//! publish are the ones it reported, deleting the stack takes them away, and a declaration that
//! service REFUSES fails the resource with its own refusal and rolls the stack back.
//!
//! RULE: a resource type Amazon Web Services publishes is one a template may declare, and a
//! declaration of it is DEPLOYED by calling the service that owns the type — so the handle the
//! stack publishes (`Ref`) resolves in that service, the values the type publishes are the ones
//! that service reported, and what that service refuses is a FAILED RESOURCE rolled back rather
//! than a template-format error.
//!
//! The rows of the deploy table differ along what the rule generalises over: where the graph's
//! provisioned memory sits inside the modelled range (its minimum, a value between, its
//! maximum), how many replicas it keeps (each value of the modelled range), whether the
//! declaration carries a graph NAME at all, whether it asks for public connectivity, deletion
//! protection and a vector index, which published return value the outputs read (`Ref`,
//! `Fn::GetAtt GraphArn`, `GraphId`, `Endpoint`), the template spelling (JSON with long-form
//! intrinsics, YAML with the short-form tags), and whether the stack also declares a SNAPSHOT of
//! the graph it creates.
//!
//! The refusal rows differ along the CLASS of declaration Neptune Analytics refuses: a
//! provisioned memory below the modelled minimum and one above its maximum, and a replica count
//! above its own. Each is the owning service's own refusal, quoted on the resource that failed —
//! never the `Template format error: Unrecognized resource types` a type CloudFormation does not
//! know is refused with, which no row of either table may ever see.
//!
//! Everything goes over the wire each service speaks through the one router `harness.rs` builds —
//! the CloudFormation calls over its awsQuery wire, the Neptune Analytics calls over that
//! service's own `restJson1` wire — so what is asserted is what a client that deploys the
//! template and then calls neptune-graph observes.

use std::collections::BTreeMap;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{
    call, create_stack, member, router, settled_stack_status, stack_event_reasons, REGION,
    SETTLE_DEADLINE, SETTLE_POLL_INTERVAL,
};
use crate::stack_lifecycle::{describe_stack, form_encode, output_value};

/// The two resource types under test.
const GRAPH: &str = "AWS::NeptuneGraph::Graph";
const SNAPSHOT: &str = "AWS::NeptuneGraph::GraphSnapshot";

/// The fixed text CloudFormation refuses a template declaring a type it does NOT know with,
/// which no row of either table may ever see.
const UNRECOGNIZED_TYPES: &str = "Unrecognized resource types";

/// The status both a graph and a snapshot are usable in (`GraphStatus` / `SnapshotStatus`:
/// `smithy_query(query="shape",shape="GraphStatus,SnapshotStatus",
/// model_path="services/service-com-amazonaws-neptunegraph-amazonneptunegraph/smithy-model/\
/// com-amazonaws-neptunegraph-amazonneptunegraph.json")`).
const AVAILABLE: &str = "AVAILABLE";

/// The bounds `provisionedMemory` and `replicaCount` publish (`@range 16..24576` and
/// `@range 0..2` on `CreateGraph`'s own members, from that same model).
const MIN_PROVISIONED_MEMORY: i64 = 16;
const MAX_PROVISIONED_MEMORY: i64 = 24576;
const MAX_REPLICA_COUNT: i64 = 2;

/// The error code Neptune Analytics answers for a graph or a snapshot it does not hold, and the
/// status that error carries (the modelled 404 of `GetGraph` / `GetGraphSnapshot`).
const RESOURCE_NOT_FOUND: &str = "ResourceNotFoundException";
const NOT_FOUND_STATUS: u16 = 404;

/// The key a stack of this suite carries its own tag under, which CloudFormation propagates to
/// every resource it creates.
const STACK_TAG_KEY: &str = "sim2real";

/// One Neptune Analytics call over the `restJson1` wire its model declares, on the method and URI
/// that operation's own `@http` binding names, signed for `neptune-graph` — that model's own
/// `sigv4Name` — in the Region the stack's resources live in.
async fn neptune_graph(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> (u16, Value) {
    let payload = body.map(ToString::to_string).unwrap_or_default();
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://neptune-graph.{REGION}.amazonaws.com{uri}"))
        .header("host", format!("neptune-graph.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/neptune-graph/\
                 aws4_request, SignedHeaders=host, Signature=0123456789abcdef"
            ),
        )
        .body(bytes::Bytes::from(payload))
        .expect("a well-formed Neptune Analytics request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let raw = response.into_body();
    let parsed = if raw.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&raw).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// That service's own read of one graph, by the id it minted.
async fn get_graph(router: &MultiServiceRouter, id: &str) -> (u16, Value) {
    neptune_graph(router, "GET", &format!("/graphs/{id}"), None).await
}

/// That service's own read of one snapshot, by the id it minted.
async fn get_snapshot(router: &MultiServiceRouter, id: &str) -> (u16, Value) {
    neptune_graph(router, "GET", &format!("/snapshots/{id}"), None).await
}

/// Every graph that service lists for the account and Region.
async fn list_graphs(router: &MultiServiceRouter) -> Vec<Value> {
    let (status, answered) = neptune_graph(router, "GET", "/graphs", None).await;
    assert_eq!(
        status, 200,
        "expected HTTP 200 from ListGraphs, got {status} with {answered}"
    );
    answered
        .get("graphs")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// Every snapshot that service lists for one graph, by the id the stack published for it.
async fn list_graph_snapshots(router: &MultiServiceRouter, graph_id: &str) -> Vec<Value> {
    let (status, answered) = neptune_graph(
        router,
        "GET",
        &format!("/snapshots?graphIdentifier={graph_id}"),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "expected HTTP 200 from ListGraphSnapshots for {graph_id}, got {status} with {answered}"
    );
    answered
        .get("graphSnapshots")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// The tags that service holds on one resource, by its ARN — the `@httpLabel` of
/// `GET /tags/{resourceArn}`, escaped as a single path segment the way an SDK sends a non-greedy
/// label.
async fn list_tags(router: &MultiServiceRouter, arn: &str) -> BTreeMap<String, String> {
    let (status, answered) =
        neptune_graph(router, "GET", &format!("/tags/{}", form_encode(arn)), None).await;
    assert_eq!(
        status, 200,
        "expected HTTP 200 from ListTagsForResource for {arn}, got {status} with {answered}"
    );
    answered
        .get("tags")
        .and_then(Value::as_object)
        .map(|tags| {
            tags.iter()
                .map(|(key, value)| {
                    (
                        key.clone(),
                        value.as_str().unwrap_or_default().to_string(),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Submit `template` as `stack_name`, carrying the stack's own tag, and answer the `CreateStack`
/// body: a type Amazon Web Services publishes is ADMITTED, so this never sees the template-format
/// refusal.
async fn submitted(
    router: &MultiServiceRouter,
    case: &str,
    stack_name: &str,
    template: &str,
) -> String {
    let (status, body) = create_stack(
        router,
        stack_name,
        template,
        &[
            member("Tags.member.1.Key", STACK_TAG_KEY),
            member("Tags.member.1.Value", stack_name),
        ],
    )
    .await;
    assert!(
        !body.contains(UNRECOGNIZED_TYPES),
        "case {case:?}: expected a type Amazon Web Services publishes to be recognised, got \
         {body}"
    );
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack — a {GRAPH} is a type a template may \
         declare — got {status} with body {body}"
    );
    body
}

/// Deploy `template` as `stack_name` under the stack's own tag, and answer the `DescribeStacks`
/// body of the COMPLETED stack, quoting the stack's own failure reasons when the create did not
/// complete.
async fn deployed(
    router: &MultiServiceRouter,
    case: &str,
    stack_name: &str,
    template: &str,
) -> String {
    submitted(router, case, stack_name, template).await;
    let reached = settled_stack_status(case, router, REGION, stack_name).await;
    assert_eq!(
        reached, "CREATE_COMPLETE",
        "case {case:?}: expected the create to complete, got {reached} with failure reasons {:?}",
        stack_event_reasons(router, REGION, stack_name).await
    );
    let (status, described) = describe_stack(router, stack_name).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeStacks, got {status} with {described}"
    );
    described
}

/// Whether `id` has the form `GraphId` publishes (`^g-[a-z0-9]{10}$`), and the same for a
/// `SnapshotId` (`^gs-[a-z0-9]{10}$`).
fn is_a_graph_id(id: &str) -> bool {
    has_minted_form(id, "g-")
}

fn is_a_snapshot_id(id: &str) -> bool {
    has_minted_form(id, "gs-")
}

/// The one reading of either minted-id form: the type's prefix and exactly ten lower-case
/// alphanumeric characters.
fn has_minted_form(id: &str, prefix: &str) -> bool {
    id.strip_prefix(prefix).is_some_and(|digits| {
        digits.len() == 10
            && digits
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
    })
}

/// Whether `arn` has the form that service builds a graph's ARN in: its own ARN namespace, the
/// Region the stack deployed in, and the graph's minted id under the `graph/` path.
fn is_a_graph_arn(arn: &str, id: &str) -> bool {
    arn.starts_with("arn:")
        && arn.contains(&format!(":neptune-graph:{REGION}:"))
        && arn.ends_with(&format!(":graph/{id}"))
}

/// Read one graph until that service reports it `AVAILABLE`, or answer the last read once the
/// deadline passes: the status is one the CLOCK makes due, so what is waited for is the CONDITION
/// rather than a span of wall clock.
async fn available_graph(router: &MultiServiceRouter, case: &str, id: &str) -> Value {
    settled_record(router, case, id, Reading::Graph).await
}

/// The same wait for one snapshot.
async fn available_snapshot(router: &MultiServiceRouter, case: &str, id: &str) -> Value {
    settled_record(router, case, id, Reading::Snapshot).await
}

/// Which of the two resources a wait is reading.
#[derive(Debug, Clone, Copy)]
enum Reading {
    Graph,
    Snapshot,
}

/// The one wait both reads above are: look again until that service reports the record
/// `AVAILABLE` or the harness's own generous deadline passes, so a loaded host cannot decide the
/// outcome.
async fn settled_record(
    router: &MultiServiceRouter,
    case: &str,
    id: &str,
    reading: Reading,
) -> Value {
    let deadline = std::time::Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, record) = match reading {
            Reading::Graph => get_graph(router, id).await,
            Reading::Snapshot => get_snapshot(router, id).await,
        };
        assert_eq!(
            status, 200,
            "case {case:?}: expected Neptune Analytics to report the {reading:?} the stack \
             created under {id:?}, got {status} with {record}"
        );
        let reported = record
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if reported == AVAILABLE || std::time::Instant::now() >= deadline {
            return record;
        }
        tokio::time::sleep(SETTLE_POLL_INTERVAL).await;
    }
}

/// What one row expects of the graph its stack declares.
struct Declared {
    /// The output key whose value is what `Ref` returned — the graph's minted id.
    ref_output: &'static str,
    /// The output keys reading the attributes the type publishes, where the row reads them.
    arn_output: Option<&'static str>,
    id_output: Option<&'static str>,
    endpoint_output: Option<&'static str>,
    /// The name the declaration states, or `None` where it states none and CloudFormation
    /// decides one.
    graph_name: Option<&'static str>,
    /// The configuration the graph must come up with.
    provisioned_memory: i64,
    replica_count: i64,
    public_connectivity: bool,
    deletion_protection: bool,
    /// The vector-index width the declaration asks for, where it asks for one.
    vector_dimension: Option<i64>,
    /// The type's own `Tags` the declaration states, which the graph must carry beside the
    /// stack's own propagated tag.
    tags: &'static [(&'static str, &'static str)],
    /// The output key reading the `Ref` of the SNAPSHOT the stack declares of this graph, where
    /// the row declares one, and the name that snapshot is taken under.
    snapshot_ref_output: Option<&'static str>,
    snapshot_name: Option<&'static str>,
}

/// One row: a template and the graph it declares.
struct Row {
    case: &'static str,
    stack_name: &'static str,
    template: String,
    declared: Declared,
}

fn rows() -> Vec<Row> {
    vec![
        // The issue's own stack: a graph at the published MINIMUM memory with no replicas, every
        // return value read back, in JSON with long-form intrinsics.
        Row {
            case: "a graph at the minimum provisioned memory, every return value read back",
            stack_name: "nggraph-round-trip",
            template: json!({
                "AWSTemplateFormatVersion": "2010-09-09",
                "Resources": {
                    "Graph": {
                        "Type": GRAPH,
                        "Properties": {
                            "GraphName": "nggraph-round-trip",
                            "ProvisionedMemory": MIN_PROVISIONED_MEMORY,
                            "ReplicaCount": 0,
                            "DeletionProtection": false,
                            "PublicConnectivity": false,
                            "Tags": [{"Key": "sim2real-nggraph", "Value": "round-trip"}],
                        }
                    }
                },
                "Outputs": {
                    "GraphRef": {"Value": {"Ref": "Graph"}},
                    "GraphArn": {"Value": {"Fn::GetAtt": ["Graph", "GraphArn"]}},
                    "GraphId": {"Value": {"Fn::GetAtt": ["Graph", "GraphId"]}},
                    "GraphEndpoint": {"Value": {"Fn::GetAtt": ["Graph", "Endpoint"]}}
                }
            })
            .to_string(),
            declared: Declared {
                ref_output: "GraphRef",
                arn_output: Some("GraphArn"),
                id_output: Some("GraphId"),
                endpoint_output: Some("GraphEndpoint"),
                graph_name: Some("nggraph-round-trip"),
                provisioned_memory: MIN_PROVISIONED_MEMORY,
                replica_count: 0,
                public_connectivity: false,
                deletion_protection: false,
                vector_dimension: None,
                tags: &[("sim2real-nggraph", "round-trip")],
                snapshot_ref_output: None,
                snapshot_name: None,
            },
        },
        // A graph and a SNAPSHOT of it in one stack, the snapshot naming the graph with a `Ref`:
        // a memory value BETWEEN the bounds, the maximum replica count, public connectivity on
        // and a vector index declared.
        Row {
            case: "a graph with a snapshot of it, mid-range memory, maximum replicas",
            stack_name: "nggraph-with-snapshot",
            template: json!({
                "Resources": {
                    "Graph": {
                        "Type": GRAPH,
                        "Properties": {
                            "GraphName": "nggraph-with-snapshot",
                            "ProvisionedMemory": 128,
                            "ReplicaCount": MAX_REPLICA_COUNT,
                            "PublicConnectivity": true,
                            "VectorSearchConfiguration": {"VectorSearchDimension": 1536},
                        }
                    },
                    "Snapshot": {
                        "Type": SNAPSHOT,
                        "Properties": {
                            "GraphIdentifier": {"Ref": "Graph"},
                            "SnapshotName": "nggraph-with-snapshot-snap",
                            "Tags": [{"Key": "sim2real-nggraph", "Value": "snapshot"}],
                        }
                    }
                },
                "Outputs": {
                    "GraphRef": {"Value": {"Ref": "Graph"}},
                    "GraphArn": {"Value": {"Fn::GetAtt": ["Graph", "GraphArn"]}},
                    "SnapshotRef": {"Value": {"Ref": "Snapshot"}},
                    "SnapshotArn": {"Value": {"Fn::GetAtt": ["Snapshot", "Arn"]}},
                    "SnapshotId": {"Value": {"Fn::GetAtt": ["Snapshot", "Id"]}}
                }
            })
            .to_string(),
            declared: Declared {
                ref_output: "GraphRef",
                arn_output: Some("GraphArn"),
                id_output: None,
                endpoint_output: None,
                graph_name: Some("nggraph-with-snapshot"),
                provisioned_memory: 128,
                replica_count: MAX_REPLICA_COUNT,
                public_connectivity: true,
                deletion_protection: false,
                vector_dimension: Some(1536),
                tags: &[],
                snapshot_ref_output: Some("SnapshotRef"),
                snapshot_name: Some("nggraph-with-snapshot-snap"),
            },
        },
        // A graph at the published MAXIMUM memory, with ONE replica, deletion protection on and
        // no name declared at all — CloudFormation decides one — in YAML with short-form
        // intrinsics.
        Row {
            case: "a graph at the maximum provisioned memory, no declared name, YAML",
            stack_name: "nggraph-generated-name",
            template: format!(
                "\
Resources:
  Graph:
    Type: {GRAPH}
    Properties:
      ProvisionedMemory: {MAX_PROVISIONED_MEMORY}
      ReplicaCount: 1
      DeletionProtection: true
Outputs:
  GraphRef:
    Value: !Ref Graph
  GraphId:
    Value: !GetAtt Graph.GraphId
"
            ),
            declared: Declared {
                ref_output: "GraphRef",
                arn_output: None,
                id_output: Some("GraphId"),
                endpoint_output: None,
                graph_name: None,
                provisioned_memory: MAX_PROVISIONED_MEMORY,
                replica_count: 1,
                public_connectivity: false,
                deletion_protection: true,
                vector_dimension: None,
                tags: &[],
                snapshot_ref_output: None,
                snapshot_name: None,
            },
        },
    ]
}

#[tokio::test]
async fn a_deployed_stack_publishes_the_graph_neptune_analytics_minted() {
    for row in rows() {
        let router = router();
        let case = row.case;
        let declared = &row.declared;
        let described = deployed(&router, case, row.stack_name, &row.template).await;

        let published = output_value(&described, declared.ref_output)
            .unwrap_or_else(|| panic!("case {case:?}: no {} output in {described}", declared.ref_output));
        assert!(
            is_a_graph_id(&published),
            "case {case:?}: expected Ref to publish an id of the modelled graph form, got \
             {published:?}"
        );

        // THE RULE: the graph the stack claims to have created is a graph Neptune Analytics
        // holds, under the identity the stack published, configured as the template declared.
        let graph = available_graph(&router, case, &published).await;
        assert_eq!(
            (
                graph.get("status").and_then(Value::as_str),
                graph.get("id").and_then(Value::as_str),
            ),
            (Some(AVAILABLE), Some(published.as_str())),
            "case {case:?}: expected an available graph under the published identity, got {graph}"
        );
        assert_eq!(
            (
                graph.get("provisionedMemory").and_then(Value::as_i64),
                graph.get("replicaCount").and_then(Value::as_i64),
                graph.get("publicConnectivity").and_then(Value::as_bool),
                graph.get("deletionProtection").and_then(Value::as_bool),
            ),
            (
                Some(declared.provisioned_memory),
                Some(declared.replica_count),
                Some(declared.public_connectivity),
                Some(declared.deletion_protection),
            ),
            "case {case:?}: expected the declared configuration to reach the graph, got {graph}"
        );
        assert_eq!(
            graph
                .pointer("/vectorSearchConfiguration/dimension")
                .and_then(Value::as_i64),
            declared.vector_dimension,
            "case {case:?}: expected the declared vector dimension, got {graph}"
        );
        let reported_name = graph
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        match declared.graph_name {
            Some(name) => assert_eq!(
                reported_name, name,
                "case {case:?}: expected the declared graph name, got {reported_name:?}"
            ),
            // A declaration that states no name is created under one CloudFormation decided,
            // which has to be a name that service's own `GraphName` rule admits.
            None => assert!(
                !reported_name.is_empty()
                    && !reported_name.starts_with("g-")
                    && reported_name.chars().all(|c| {
                        c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'
                    })
                    && reported_name.starts_with(|c: char| c.is_ascii_lowercase()),
                "case {case:?}: expected a generated name the modelled GraphName admits, got \
                 {reported_name:?}"
            ),
        }

        // ...and the listing that service serves resolves the same identity.
        let listed = list_graphs(&router).await;
        let summary = listed
            .iter()
            .find(|summary| summary.get("id").and_then(Value::as_str) == Some(&published))
            .unwrap_or_else(|| {
                panic!("case {case:?}: expected ListGraphs to list {published:?}, got {listed:?}")
            });
        assert_eq!(
            summary.get("name").and_then(Value::as_str),
            Some(reported_name.as_str()),
            "case {case:?}: expected the listed graph to carry the name it was created under, \
             got {summary}"
        );

        // The values the type publishes are that service's own: the id it minted, the ARN it
        // built around that id and the endpoint it serves the graph on.
        let arn = graph
            .get("arn")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        assert!(
            is_a_graph_arn(&arn, &published),
            "case {case:?}: expected the graph's ARN to carry the minted id, got {arn:?}"
        );
        let endpoint = graph
            .get("endpoint")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        for (key, expected) in [
            (declared.arn_output, arn.clone()),
            (declared.id_output, published.clone()),
            (declared.endpoint_output, endpoint.clone()),
        ] {
            let Some(key) = key else { continue };
            let read = output_value(&described, key)
                .unwrap_or_else(|| panic!("case {case:?}: no {key} output in {described}"));
            assert_eq!(
                read, expected,
                "case {case:?}: expected the published {key} to be {expected:?}, got {read:?}"
            );
        }

        // The tags the graph carries are the declaration's own beside the stack's, and none of
        // the records CloudFormation keeps in the namespace AWS reserves for itself.
        let held = list_tags(&router, &arn).await;
        let mut expected: BTreeMap<String, String> = declared
            .tags
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect();
        expected.insert(STACK_TAG_KEY.to_string(), row.stack_name.to_string());
        assert_eq!(
            held, expected,
            "case {case:?}: expected the declared tags beside the stack's own, got {held:?}"
        );

        // A stack that also declares a SNAPSHOT publishes the id that service minted for it, and
        // the snapshot it holds is the capture of the graph of the same stack.
        if let (Some(key), Some(name)) = (declared.snapshot_ref_output, declared.snapshot_name) {
            let snapshot_id = output_value(&described, key)
                .unwrap_or_else(|| panic!("case {case:?}: no {key} output in {described}"));
            assert!(
                is_a_snapshot_id(&snapshot_id),
                "case {case:?}: expected Ref to publish an id of the modelled snapshot form, got \
                 {snapshot_id:?}"
            );
            let snapshot = available_snapshot(&router, case, &snapshot_id).await;
            assert_eq!(
                (
                    snapshot.get("status").and_then(Value::as_str),
                    snapshot.get("id").and_then(Value::as_str),
                    snapshot.get("name").and_then(Value::as_str),
                    snapshot.get("sourceGraphId").and_then(Value::as_str),
                ),
                (
                    Some(AVAILABLE),
                    Some(snapshot_id.as_str()),
                    Some(name),
                    Some(published.as_str()),
                ),
                "case {case:?}: expected the snapshot of the stack's own graph, got {snapshot}"
            );
            // The two values the snapshot type publishes are that service's own: the id it
            // minted and the ARN it built around that id.
            let snapshot_arn = snapshot
                .get("arn")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            assert!(
                snapshot_arn.ends_with(&format!(":graph-snapshot/{snapshot_id}")),
                "case {case:?}: expected the snapshot's ARN to carry the minted id, got \
                 {snapshot_arn:?}"
            );
            for (key, expected) in [
                ("SnapshotArn", snapshot_arn.clone()),
                ("SnapshotId", snapshot_id.clone()),
            ] {
                let read = output_value(&described, key)
                    .unwrap_or_else(|| panic!("case {case:?}: no {key} output in {described}"));
                assert_eq!(
                    read, expected,
                    "case {case:?}: expected the published {key} to be {expected:?}, got {read:?}"
                );
            }

            // ...and the listing that service serves for the graph the snapshot was taken of
            // reports exactly that one snapshot, under the identity the stack published.
            let listed: Vec<(String, String)> = list_graph_snapshots(&router, &published)
                .await
                .iter()
                .map(|summary| {
                    (
                        summary
                            .get("id")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        summary
                            .get("arn")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                    )
                })
                .collect();
            assert_eq!(
                listed,
                vec![(snapshot_id.clone(), snapshot_arn.clone())],
                "case {case:?}: expected ListGraphSnapshots to report the snapshot the stack \
                 published, got {listed:?}"
            );
        }
    }
}

#[tokio::test]
async fn a_declared_configuration_reaches_the_graph_in_either_spelling() {
    // A template states a value either in JSON's own spelling or as the TEXT a parameter
    // reference resolves to — `Ref` on a parameter answers that parameter's value, which a stack
    // operation carries as a string whatever the parameter's declared type — so both spellings
    // have to reach the owning service as the kinds `provisionedMemory` and `replicaCount` are
    // carried in.
    // (case, the stack, the memory the parameter states, the replicas it states)
    let rows: [(&str, &str, &str, &str); 3] = [
        (
            "the published minimum memory and no replicas, as parameter text",
            "nggraph-typed-minimum",
            "16",
            "0",
        ),
        (
            "a memory between the published bounds, as parameter text",
            "nggraph-typed-middle",
            "256",
            "1",
        ),
        (
            "the published maximum memory and the maximum replicas, as parameter text",
            "nggraph-typed-maximum",
            "24576",
            "2",
        ),
    ];

    for (case, stack_name, memory, replicas) in rows {
        let router = router();
        let template = json!({
            "Parameters": {
                "Memory": {"Type": "Number"},
                "Replicas": {"Type": "Number"}
            },
            "Resources": {
                "Graph": {
                    "Type": GRAPH,
                    "Properties": {
                        "GraphName": stack_name,
                        "ProvisionedMemory": {"Ref": "Memory"},
                        "ReplicaCount": {"Ref": "Replicas"},
                    }
                }
            },
            "Outputs": {"GraphRef": {"Value": {"Ref": "Graph"}}}
        })
        .to_string();

        let (status, body) = create_stack(
            &router,
            stack_name,
            &template,
            &[
                member("Parameters.member.1.ParameterKey", "Memory"),
                member("Parameters.member.1.ParameterValue", memory),
                member("Parameters.member.2.ParameterKey", "Replicas"),
                member("Parameters.member.2.ParameterValue", replicas),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {status} with {body}"
        );
        let reached = settled_stack_status(case, &router, REGION, stack_name).await;
        assert_eq!(
            reached, "CREATE_COMPLETE",
            "case {case:?}: expected the create to complete, got {reached} with failure reasons \
             {:?}",
            stack_event_reasons(&router, REGION, stack_name).await
        );
        let (_, described) = describe_stack(&router, stack_name).await;
        let published = output_value(&described, "GraphRef")
            .unwrap_or_else(|| panic!("case {case:?}: no GraphRef output in {described}"));
        let graph = available_graph(&router, case, &published).await;
        assert_eq!(
            (
                graph.get("provisionedMemory").and_then(Value::as_i64),
                graph.get("replicaCount").and_then(Value::as_i64),
            ),
            (
                Some(memory.parse::<i64>().expect("a numeric row")),
                Some(replicas.parse::<i64>().expect("a numeric row")),
            ),
            "case {case:?}: expected the parameter's value to reach the graph as a number, got \
             {graph}"
        );
    }
}

#[tokio::test]
async fn deleting_the_stack_deletes_the_graph_and_its_snapshot() {
    let router = router();
    let case = "a graph and its snapshot deleted with their stack";
    let stack_name = "nggraph-deleted";
    let template = json!({
        "Resources": {
            "Graph": {
                "Type": GRAPH,
                "Properties": {
                    "GraphName": "nggraph-deleted",
                    "ProvisionedMemory": MIN_PROVISIONED_MEMORY,
                    "ReplicaCount": 0,
                }
            },
            "Snapshot": {
                "Type": SNAPSHOT,
                "Properties": {
                    "GraphIdentifier": {"Ref": "Graph"},
                    "SnapshotName": "nggraph-deleted-snap",
                }
            }
        },
        "Outputs": {
            "GraphRef": {"Value": {"Ref": "Graph"}},
            "SnapshotRef": {"Value": {"Ref": "Snapshot"}}
        }
    })
    .to_string();

    let described = deployed(&router, case, stack_name, &template).await;
    let graph_id = output_value(&described, "GraphRef")
        .unwrap_or_else(|| panic!("case {case:?}: no GraphRef output in {described}"));
    let snapshot_id = output_value(&described, "SnapshotRef")
        .unwrap_or_else(|| panic!("case {case:?}: no SnapshotRef output in {described}"));
    // Both exist before the delete, each read back through its own service.
    available_graph(&router, case, &graph_id).await;
    available_snapshot(&router, case, &snapshot_id).await;

    let (status, deleted) = call(&router, "DeleteStack", &[member("StackName", stack_name)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DeleteStack, got {status} with body {deleted}"
    );
    settle_the_deletion(&router, case, stack_name, &graph_id, &snapshot_id).await;

    // The resources the stack created are gone from the owning service, which is what makes a
    // stack's resources the stack's own rather than leftovers.
    for (reading, id) in [
        (Reading::Graph, graph_id.clone()),
        (Reading::Snapshot, snapshot_id.clone()),
    ] {
        let (status, answered) = match reading {
            Reading::Graph => get_graph(&router, &id).await,
            Reading::Snapshot => get_snapshot(&router, &id).await,
        };
        assert_eq!(
            (status, answered.get("__type").and_then(Value::as_str)),
            (NOT_FOUND_STATUS, Some(RESOURCE_NOT_FOUND)),
            "case {case:?}: expected the {reading:?} {id:?} to be gone with the stack, got \
             {status} with {answered}"
        );
    }
    let listed = list_graphs(&router).await;
    assert!(
        listed.is_empty(),
        "case {case:?}: expected Neptune Analytics to hold no graph after the stack went, got \
         {listed:?}"
    );
}

/// Wait until the graph and the snapshot of the deleted stack are both gone from the owning
/// service.
///
/// Waits for the CONDITION rather than for a span, under the harness's own generous deadline, so
/// a loaded host cannot decide the outcome.
async fn settle_the_deletion(
    router: &MultiServiceRouter,
    case: &str,
    stack_name: &str,
    graph_id: &str,
    snapshot_id: &str,
) {
    let deadline = std::time::Instant::now() + SETTLE_DEADLINE;
    loop {
        // A deletion the owning service has not finished is a wait CloudFormation holds open,
        // and a read of the stack is what asks that service again — the same call a client's own
        // waiter makes while it polls the stack.
        let _ = describe_stack(router, stack_name).await;
        let (graph_status, _) = get_graph(router, graph_id).await;
        let (snapshot_status, _) = get_snapshot(router, snapshot_id).await;
        if graph_status == NOT_FOUND_STATUS && snapshot_status == NOT_FOUND_STATUS {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: expected the stack's deletion to take the graph {graph_id:?} and the \
             snapshot {snapshot_id:?} away, last read {graph_status} / {snapshot_status}; the \
             stack's own failure reasons were {:?}",
            stack_event_reasons(router, REGION, stack_name).await
        );
        tokio::time::sleep(SETTLE_POLL_INTERVAL).await;
    }
}

/// One declaration Neptune Analytics itself refuses, by the CLASS of refusal it is.
struct Refused {
    case: &'static str,
    stack_name: &'static str,
    graph_name: &'static str,
    /// The configuration the declaration asks for, one member of which is outside the bound its
    /// own model publishes.
    provisioned_memory: i64,
    replica_count: i64,
}

/// The rows differ along WHICH modelled bound the declaration breaks and on which side of it:
/// the memory below its minimum (the issue's own case), the memory above its maximum, and the
/// replica count above its own maximum.
const REFUSED: &[Refused] = &[
    Refused {
        case: "provisioned memory below the modelled minimum",
        stack_name: "nggraph-memory-too-small",
        graph_name: "nggraph-memory-too-small",
        provisioned_memory: 1,
        replica_count: 0,
    },
    Refused {
        case: "provisioned memory above the modelled maximum",
        stack_name: "nggraph-memory-too-large",
        graph_name: "nggraph-memory-too-large",
        provisioned_memory: MAX_PROVISIONED_MEMORY + 1,
        replica_count: 0,
    },
    Refused {
        case: "more replicas than the modelled maximum",
        stack_name: "nggraph-too-many-replicas",
        graph_name: "nggraph-too-many-replicas",
        provisioned_memory: MIN_PROVISIONED_MEMORY,
        replica_count: MAX_REPLICA_COUNT + 1,
    },
];

#[tokio::test]
async fn a_declaration_neptune_analytics_refuses_rolls_the_stack_back() {
    for row in REFUSED {
        let router = router();
        let case = row.case;
        let template = json!({
            "Resources": {
                "Graph": {
                    "Type": GRAPH,
                    "Properties": {
                        "GraphName": row.graph_name,
                        "ProvisionedMemory": row.provisioned_memory,
                        "ReplicaCount": row.replica_count,
                        "DeletionProtection": false,
                        "PublicConnectivity": false,
                        "Tags": [{"Key": "sim2real-nggraph", "Value": "refused"}],
                    }
                }
            },
            "Outputs": {"GraphRef": {"Value": {"Ref": "Graph"}}}
        })
        .to_string();

        // What the OWNING service answers for exactly that create is what the failed resource has
        // to quote, so it is read from that service itself rather than written out here.
        let (status, refusal) = neptune_graph(
            &router,
            "POST",
            "/graphs",
            Some(&json!({
                "graphName": format!("{}-direct", row.graph_name),
                "provisionedMemory": row.provisioned_memory,
                "replicaCount": row.replica_count,
            })),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected Neptune Analytics to refuse the configuration itself, got \
             {status} with {refusal}"
        );
        let sentence = refusal
            .get("message")
            .or_else(|| refusal.get("Message"))
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("case {case:?}: no message in {refusal}"))
            .to_string();

        submitted(&router, case, row.stack_name, &template).await;
        let reached = settled_stack_status(case, &router, REGION, row.stack_name).await;
        assert_eq!(
            reached, "ROLLBACK_COMPLETE",
            "case {case:?}: expected the failed create to be rolled back rather than publish an \
             identity Neptune Analytics never minted, got {reached} with failure reasons {:?}",
            stack_event_reasons(&router, REGION, row.stack_name).await
        );
        let (_, described) = describe_stack(&router, row.stack_name).await;
        assert_eq!(
            output_value(&described, "GraphRef"),
            None,
            "case {case:?}: expected no output for a resource nothing created, got {described}"
        );

        // The failure is the OWNING service's own refusal, on the resource that failed, inside
        // the envelope a resource provider reports one in — and never a refusal of the template.
        let (_, events) = call(
            &router,
            "DescribeStackEvents",
            &[member("StackName", row.stack_name)],
        )
        .await;
        assert!(
            !events.contains(UNRECOGNIZED_TYPES) && !events.contains("VALIDATION_ERROR"),
            "case {case:?}: expected no template-format error for a type AWS publishes, got \
             {events}"
        );
        let reasons = stack_event_reasons(&router, REGION, row.stack_name).await;
        let expected_envelope = "(Service: NeptuneGraph, Status Code: 400,";
        let expected_suffix = "HandlerErrorCode: InvalidRequest";
        assert!(
            reasons.iter().any(|reason| reason.contains(&sentence)
                && reason.contains(expected_envelope)
                && reason.contains("SDK Attempt Count:")
                && reason.contains(expected_suffix)),
            "case {case:?}: expected a failure reason carrying {sentence:?}, \
             {expected_envelope:?} and {expected_suffix:?}, got {reasons:?}"
        );

        // And nothing was left behind: that service holds no graph under the refused name.
        let listed = list_graphs(&router).await;
        assert!(
            !listed
                .iter()
                .any(|graph| graph.get("name").and_then(Value::as_str) == Some(row.graph_name)),
            "case {case:?}: expected Neptune Analytics to list no graph named {:?}, got \
             {listed:?}",
            row.graph_name
        );
    }
}
