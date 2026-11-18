// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for `ExecuteQuery` — the graph's own openCypher data plane — driven
//! over the wire (`restJson1`) through the same router the emulator serves from.
//!
//! The rules every assertion here is about, in the order the operation decides them:
//!
//! 1. **authorization first.** "The IAM user or role making the request must have a
//!    policy attached that allows one of the following IAM actions in that cluster,
//!    depending on the query: `neptune-graph:ReadDataViaQuery`
//!    `neptune-graph:WriteDataViaQuery` `neptune-graph:DeleteDataViaQuery`"
//!    (`smithy_query(query="doc",shape="ExecuteQuery")`), so a caller whose identity
//!    policies allow none of them is refused `AccessDeniedException` (403) naming the
//!    principal, the three actions and the graph ARN — whatever the query says, and
//!    whether or not the graph exists.
//! 2. **the endpoint next.** The operation is bound to the host prefix
//!    `{graphIdentifier}.` (`smithy_query(query="operation",operation="ExecuteQuery")` ->
//!    endpoint), which is registered only while the graph is serving, so a
//!    `graphIdentifier` that names no serving graph of the Region reaches no data plane
//!    at all and is answered `ResourceNotFoundException` (404).
//! 3. **then the query.** A query text the openCypher parser cannot read is refused
//!    `ValidationException` (400) with the modelled `MALFORMED_QUERY` reason and the
//!    position the parser could not continue at; a query it can read is EVALUATED over
//!    the graph's stored nodes and edges, and the payload is what that evaluation
//!    produced.
//!
//! So the tables below span what those rules generalise over rather than re-running one
//! request: the four kinds of credential a call can be signed with crossed with the
//! policies they carry, every status this service puts a graph into plus an identifier no
//! graph holds, several shapes of malformed text, and queries of different shapes
//! (projection, filter, traversal, aggregate, ordering) over two graphs holding DIFFERENT
//! data — so no answer here can be a constant.
//!
//! The authorization rule is spanned twice over: once along WHERE a grant is held and
//! WHICH identity holds it (an inline policy, a managed policy's default version, an
//! attachment with no readable document, a permissions boundary; an IAM user, a user under
//! a path, and a session of an assumed role under either), and once along WHEN it is held
//! — the documents the account holds change between calls, and each call is decided by
//! what the IAM store holds at the moment it arrives.
//!
//! The data is seeded through the store's own writers (`crate::state::GraphData`), which
//! is where a bulk load and a restore write it too: the engine behind this operation
//! evaluates read-only openCypher, so a query cannot be used to write the rows another
//! query reads.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde_json::{json, Value};
use service_com_amazonaws_iam_awsidentitymanagementv20100508::ops::types::{
    AttachedPermissionsBoundary, Policy, PolicyVersion, Role, User,
};
use service_com_amazonaws_iam_awsidentitymanagementv20100508::state as iam;
use service_com_amazonaws_neptunegraph_amazonneptunegraph::state::{
    service_state, GraphEdge, GraphNode, MAX_QUERY_STRING_CHARS, PROVISIONING_SPAN,
};
use service_com_amazonaws_neptunegraph_amazonneptunegraph::EmulatedService;
use service_com_amazonaws_sts_awssecuritytokenservicev20110615 as sts;
use smithy_server_runtime::MultiServiceRouter;

/// The region the test's SigV4 credential scope names.
const REGION: &str = "us-east-1";

/// The smallest `provisionedMemory` the model allows (`range 16..24576`).
const MIN_PROVISIONED_MEMORY: i64 = 16;

/// The access key id no store holds, which is the emulator's own published credential:
/// the caller nothing bounds.
const AMBIENT_KEY: &str = "AKIDEXAMPLE";

/// The query every row that is not about the query TEXT sends: a well-formed openCypher
/// read the engine evaluates over whatever the graph holds.
const ANY_QUERY: &str = "MATCH (n) RETURN count(n) AS nodes";

/// This service and STS mounted on the SAME shared engine, so the identity a credential
/// speaks for is resolved from the records STS itself answers from, and the IAM store the
/// policies are read from is the one on that engine.
fn emulator() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.register_service(EmulatedService::new(Arc::clone(&engine)));
    router.register_service(sts::EmulatedService::new(Arc::clone(&engine)));
    (router, engine)
}

/// Dispatch one `restJson1` request signed (in the routing sense) with `access_key_id`.
///
/// `graph_identifier` is sent as the `graphIdentifier` header, which is where the model
/// binds it for this operation (`smithy_query(query="operation",operation="ExecuteQuery")`
/// -> input).
async fn call_as(
    router: &MultiServiceRouter,
    access_key_id: &str,
    method: &str,
    path: &str,
    graph_identifier: Option<&str>,
    body: Option<Value>,
) -> (u16, Value) {
    let payload = body.map_or_else(Vec::new, |value| {
        serde_json::to_vec(&value).expect("a JSON request body serializes")
    });
    let mut request = http::Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential={access_key_id}/20240101/{REGION}/neptune-graph/\
                 aws4_request, SignedHeaders=host, Signature=deadbeef"
            ),
        );
    if let Some(graph_identifier) = graph_identifier {
        request = request.header("graphIdentifier", graph_identifier);
    }
    let request = request
        .body(bytes::Bytes::from(payload))
        .expect("a well-formed test request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One `ExecuteQuery` call, as the emulator's own credential.
async fn execute_query(
    router: &MultiServiceRouter,
    graph_identifier: &str,
    query: &str,
) -> (u16, Value) {
    execute_query_as(router, AMBIENT_KEY, graph_identifier, query).await
}

/// One `ExecuteQuery` call signed with `access_key_id`.
async fn execute_query_as(
    router: &MultiServiceRouter,
    access_key_id: &str,
    graph_identifier: &str,
    query: &str,
) -> (u16, Value) {
    call_as(
        router,
        access_key_id,
        "POST",
        "/queries",
        Some(graph_identifier),
        Some(json!({"query": query, "language": "OPEN_CYPHER"})),
    )
    .await
}

/// The wire error code of a failure body (`__type`), without its namespace.
fn error_code(body: &Value) -> String {
    body["__type"]
        .as_str()
        .unwrap_or_default()
        .rsplit('#')
        .next()
        .unwrap_or_default()
        .to_string()
}

/// Create a graph and wait until it is serving, returning its id and its ARN.
///
/// It waits for the CONDITION — the status a read reports — rather than for a span, with
/// a deadline generous beside the one the provisioning transition is owed on.
async fn graph_in_service(router: &MultiServiceRouter, name: &str) -> (String, String) {
    let (status, created) = call_as(
        router,
        AMBIENT_KEY,
        "POST",
        "/graphs",
        None,
        Some(json!({"graphName": name, "provisionedMemory": MIN_PROVISIONED_MEMORY})),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {name:?}: expected 201 from CreateGraph, got {status}: {created}"
    );
    let id = created["id"]
        .as_str()
        .unwrap_or_else(|| panic!("case {name:?}: no id in {created}"))
        .to_string();
    let arn = created["arn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {name:?}: no arn in {created}"))
        .to_string();
    for _ in 0..40 {
        let (get_status, graph) =
            call_as(router, AMBIENT_KEY, "GET", &format!("/graphs/{id}"), None, None).await;
        assert_eq!(
            get_status, 200,
            "case {name:?}: expected 200 from GetGraph, got {get_status}: {graph}"
        );
        if graph["status"] == json!("AVAILABLE") {
            return (id, arn);
        }
        tokio::time::sleep(PROVISIONING_SPAN / 4).await;
    }
    panic!("case {name:?}: the graph never reached AVAILABLE");
}

/// One seeded node: its id, its labels, and the properties set on it.
type SeedNode<'a> = (&'a str, &'a [&'a str], &'a [(&'a str, Value)]);

/// One seeded edge: its id, its label, and the nodes it joins.
type SeedEdge<'a> = (&'a str, &'a str, &'a str, &'a str);

/// Write `nodes` and `edges` into the graph's stored data through the store's own
/// writers — the writers a bulk load and a restore use.
fn seed(
    engine: &Arc<engine::InMemoryEngine>,
    case: &str,
    graph_id: &str,
    nodes: &[SeedNode<'_>],
    edges: &[SeedEdge<'_>],
) {
    let state = service_state(&**engine).expect("the neptune-graph store is mounted");
    let mut store = state.lock();
    let graph = store
        .graphs
        .get_mut(graph_id)
        .unwrap_or_else(|| panic!("case {case:?}: the store holds no graph {graph_id}"));
    let written_at = std::time::SystemTime::now();
    for (id, labels, properties) in nodes {
        graph
            .data
            .insert_node(
                id,
                GraphNode {
                    labels: labels.iter().map(|label| (*label).to_string()).collect::<BTreeSet<String>>(),
                    properties: properties
                        .iter()
                        .map(|(key, value)| ((*key).to_string(), value.clone()))
                        .collect::<BTreeMap<String, Value>>(),
                },
                written_at,
            )
            .unwrap_or_else(|refused| panic!("case {case:?}: the node {id} was refused: {refused}"));
    }
    for (id, label, from, to) in edges {
        graph
            .data
            .insert_edge(
                id,
                GraphEdge {
                    label: (*label).to_string(),
                    from_node: (*from).to_string(),
                    to_node: (*to).to_string(),
                    properties: BTreeMap::new(),
                },
                written_at,
            )
            .unwrap_or_else(|refused| panic!("case {case:?}: the edge {id} was refused: {refused}"));
    }
}

/// Record, in the IAM store of the shared engine, that the account holds `access_key_id`
/// for an IAM user named `user_name` carrying `policy` (when it carries one) as an inline
/// policy — the records `iam:CreateUser`, `iam:CreateAccessKey` and `iam:PutUserPolicy`
/// write — and answer that user's ARN.
///
/// The user at the root of the account's path, which is the one kind of principal the
/// suites below that are not about the kind of principal are signed as: it is this file's
/// one seeding of an identity ([`hold_identity`]) with one inline policy on it.
fn hold_user(
    engine: &Arc<engine::InMemoryEngine>,
    user_name: &str,
    access_key_id: &str,
    policy: Option<&str>,
) -> String {
    let principal = Principal::IamUser { path: "/" };
    let arn = hold_identity(engine, principal, user_name, access_key_id);
    if let Some(document) = policy {
        put_inline_policy(engine, principal, user_name, "query", document);
    }
    arn
}

/// The rows a result payload carries: `{"results": [ … ]}` is the openCypher result form
/// this data plane answers in.
fn rows(case: &str, payload: &Value) -> Vec<Value> {
    payload["results"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: no results array in {payload}"))
        .clone()
}

#[tokio::test]
async fn a_query_is_evaluated_over_the_graphs_stored_data() {
    let (router, engine) = emulator();

    // TWO graphs holding DIFFERENT data, so the same query cannot answer both: the rows
    // below are read from whichever graph the request named.
    let (cities, _) = graph_in_service(&router, "query-cities").await;
    seed(
        &engine,
        "the cities graph",
        &cities,
        &[
            ("c1", &["city"], &[("name", json!("Berlin")), ("population", json!(3_600_000))]),
            ("c2", &["city"], &[("name", json!("Paris")), ("population", json!(2_100_000))]),
            ("c3", &["city"], &[("name", json!("Lisbon")), ("population", json!(545_000))]),
            ("a1", &["airport"], &[("name", json!("BER"))]),
        ],
        &[
            ("r1", "route", "c1", "c2"),
            ("r2", "route", "c2", "c3"),
            ("s1", "serves", "a1", "c1"),
        ],
    );
    let (one_city, _) = graph_in_service(&router, "query-one-city").await;
    seed(
        &engine,
        "the one-city graph",
        &one_city,
        &[("c1", &["city"], &[("name", json!("Reykjavik"))])],
        &[],
    );

    // (case, the query, the graph it runs on, the rows it must answer)
    let rows_of: &[(&str, &str, &str, Value)] = &[
        (
            "a count over every node the graph holds",
            ANY_QUERY,
            &cities,
            json!([{"nodes": 4}]),
        ),
        (
            "a projection that reads nothing of the graph",
            "RETURN 1 AS one",
            &cities,
            json!([{"one": 1}]),
        ),
        (
            "the same projection on the graph holding other data",
            "RETURN 1 AS one",
            &one_city,
            json!([{"one": 1}]),
        ),
        (
            "a property projection, ordered",
            "MATCH (c:city) RETURN c.name AS name ORDER BY name",
            &cities,
            json!([{"name": "Berlin"}, {"name": "Lisbon"}, {"name": "Paris"}]),
        ),
        (
            "the same query on the graph holding other data",
            "MATCH (c:city) RETURN c.name AS name ORDER BY name",
            &one_city,
            json!([{"name": "Reykjavik"}]),
        ),
        (
            "a label the graph holds one node of",
            "MATCH (a:airport) RETURN a.name AS name",
            &cities,
            json!([{"name": "BER"}]),
        ),
        (
            "a filter over a stored property",
            "MATCH (c:city) WHERE c.population > 1000000 RETURN c.name AS name ORDER BY name",
            &cities,
            json!([{"name": "Berlin"}, {"name": "Paris"}]),
        ),
        (
            "a traversal of a stored edge",
            "MATCH (a:city)-[:route]->(b:city) RETURN a.name AS departs, b.name AS arrives \
             ORDER BY departs",
            &cities,
            json!([
                {"departs": "Berlin", "arrives": "Paris"},
                {"departs": "Paris", "arrives": "Lisbon"},
            ]),
        ),
        (
            "an incoming traversal",
            "MATCH (c:city)<-[:serves]-(a:airport) RETURN c.name AS served",
            &cities,
            json!([{"served": "Berlin"}]),
        ),
        (
            "an aggregate over the stored nodes",
            "MATCH (c:city) RETURN count(c) AS total",
            &cities,
            json!([{"total": 3}]),
        ),
        (
            "an aggregate over the other graph's nodes",
            "MATCH (c:city) RETURN count(c) AS total",
            &one_city,
            json!([{"total": 1}]),
        ),
        (
            "a bound limit",
            "MATCH (c:city) RETURN c.name AS name ORDER BY name LIMIT 1",
            &cities,
            json!([{"name": "Berlin"}]),
        ),
        (
            "a pattern no stored edge satisfies",
            "MATCH (a:airport)-[:route]->(c:city) RETURN a.name AS name",
            &cities,
            json!([]),
        ),
    ];

    for (case, query, graph_id, expected) in rows_of {
        let (status, payload) = execute_query(&router, graph_id, query).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 from ExecuteQuery, got {status}: {payload}"
        );
        let answered = Value::Array(rows(case, &payload));
        assert_eq!(
            answered, *expected,
            "case {case:?}: expected the rows {expected}, got {answered}"
        );
    }

    // A node returned whole is reported as this service reports one: its id, its labels
    // and its properties, read from the store.
    let (status, payload) = execute_query(
        &router,
        &one_city,
        "MATCH (c:city) RETURN c AS node",
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"a bound node\": expected 200, got {status}: {payload}"
    );
    assert_eq!(
        Value::Array(rows("a bound node", &payload)),
        json!([{"node": {
            "~id": "c1",
            "~entityType": "node",
            "~labels": ["city"],
            "~properties": {"name": "Reykjavik"},
        }}]),
        "case \"a bound node\": unexpected node projection in {payload}"
    );
}

#[tokio::test]
async fn a_query_the_parser_cannot_read_is_refused_as_a_validation_exception() {
    let (router, _engine) = emulator();
    let (graph_id, _) = graph_in_service(&router, "query-malformed").await;

    // Texts no openCypher grammar accepts, differing in WHERE they stop being readable:
    // an unclosed pattern, a clause with nothing to project, a keyword that is not one,
    // text that is not a query at all, and a second line that breaks.
    let malformed: &[(&str, &str)] = &[
        ("an unclosed node pattern", "MATCH (n RETURN n"),
        ("a RETURN with nothing to project", "MATCH (n) RETURN"),
        ("a keyword that is not one", "MATHC (n) RETURN n"),
        ("text that is not a query", "!!!"),
        ("a break on the second line", "MATCH (n)\nRETRUN n"),
        ("a dangling relationship", "MATCH (n)-[:r]-> RETURN n"),
    ];

    for (case, query) in malformed {
        let (status, body) = execute_query(&router, &graph_id, query).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected 400, got {status}: {body}"
        );
        assert_eq!(
            error_code(&body),
            "ValidationException",
            "case {case:?}: unexpected error code in {body}"
        );
        assert_eq!(
            body["reason"],
            json!("MALFORMED_QUERY"),
            "case {case:?}: expected the MALFORMED_QUERY reason, got {body}"
        );
        let message = body["message"].as_str().unwrap_or_default();
        assert!(
            message.starts_with("Invalid input") && message.contains("(line "),
            "case {case:?}: expected the refusal to state the position it was read to, \
             got {message:?}"
        );
    }

    // The graph is untouched by a refused query: it still serves, and a well-formed query
    // on it is still answered.
    let (status, payload) = execute_query(&router, &graph_id, ANY_QUERY).await;
    assert_eq!(
        status, 200,
        "case \"a well-formed query after the refusals\": expected 200, got {status}: \
         {payload}"
    );

    // A text past the length this service reads is refused for that, before it is parsed:
    // a different fact about the request, so a different modelled reason.
    let oversized = format!(
        "MATCH (n) RETURN n // {}",
        "x".repeat(MAX_QUERY_STRING_CHARS)
    );
    let (status, body) = execute_query(&router, &graph_id, &oversized).await;
    assert_eq!(
        status, 400,
        "case \"a text past the length the service reads\": expected 400, got {status}: \
         {body}"
    );
    assert_eq!(
        error_code(&body),
        "ValidationException",
        "case \"a text past the length the service reads\": unexpected error code in {body}"
    );
    assert_eq!(
        body["reason"],
        json!("QUERY_TOO_LARGE"),
        "case \"a text past the length the service reads\": expected the QUERY_TOO_LARGE \
         reason, got {body}"
    );
}

#[tokio::test]
async fn a_query_reaches_only_a_graph_whose_data_plane_is_serving() {
    let (router, engine) = emulator();

    // Every status this service puts a graph into, and whether a graph in it serves its
    // own endpoint. Source for the set: the modelled `GraphStatus` enum
    // (`smithy_query(query="shape",shape="GraphStatus")`) minus `FAILED`, which no path
    // in this service produces.
    let statuses: &[(&str, bool)] = &[
        ("CREATING", false),
        ("AVAILABLE", true),
        ("UPDATING", false),
        ("SNAPSHOTTING", false),
        ("IMPORTING", false),
        ("STOPPING", false),
        ("STOPPED", false),
        ("STARTING", false),
        ("RESETTING", false),
        ("DELETING", false),
    ];

    let (graph_id, _) = graph_in_service(&router, "query-statuses").await;
    let not_found = format!(
        "No graph found for customer [{}] with graph id [{graph_id}]",
        engine::emulator_account_id()
    );
    for (graph_status, serving) in statuses {
        // The status is parked with nothing owed out of it, so the answer cannot depend
        // on how long the call took to arrive.
        {
            let state = service_state(&*engine).expect("the neptune-graph store is mounted");
            let mut store = state.lock();
            let graph = store
                .graphs
                .get_mut(&graph_id)
                .expect("the graph was created over the wire");
            graph.status = parked_status(graph_status);
            graph.pending = None;
            graph.deletes_at = None;
        }
        let case = format!("a query on a graph in {graph_status}");
        let (status, body) = execute_query(&router, &graph_id, ANY_QUERY).await;
        if *serving {
            assert_eq!(
                status, 200,
                "case {case:?}: a serving graph answers its own data plane, got {status}: \
                 {body}"
            );
            continue;
        }
        assert_eq!(status, 404, "case {case:?}: expected 404, got {status}: {body}");
        assert_eq!(
            error_code(&body),
            "ResourceNotFoundException",
            "case {case:?}: unexpected error code in {body}"
        );
        assert_eq!(
            body["message"],
            json!(not_found),
            "case {case:?}: unexpected message in {body}"
        );
    }

    // And an identifier no graph holds at all is the same answer: there is no endpoint at
    // that address either.
    let absent = "g-0000000000";
    let (status, body) = execute_query(&router, absent, ANY_QUERY).await;
    assert_eq!(
        status, 404,
        "case \"an identifier no graph holds\": expected 404, got {status}: {body}"
    );
    assert_eq!(
        body["message"],
        json!(format!(
            "No graph found for customer [{}] with graph id [{absent}]",
            engine::emulator_account_id()
        )),
        "case \"an identifier no graph holds\": unexpected message in {body}"
    );
}

/// The stored status one wire `GraphStatus` value names.
fn parked_status(
    wire: &str,
) -> service_com_amazonaws_neptunegraph_amazonneptunegraph::state::GraphStatus {
    use service_com_amazonaws_neptunegraph_amazonneptunegraph::state::GraphStatus;
    match wire {
        "CREATING" => GraphStatus::Creating,
        "AVAILABLE" => GraphStatus::Available,
        "UPDATING" => GraphStatus::Updating,
        "SNAPSHOTTING" => GraphStatus::Snapshotting,
        "IMPORTING" => GraphStatus::Importing,
        "STOPPING" => GraphStatus::Stopping,
        "STOPPED" => GraphStatus::Stopped,
        "STARTING" => GraphStatus::Starting,
        "RESETTING" => GraphStatus::Resetting,
        "DELETING" => GraphStatus::Deleting,
        other => panic!("no stored status is named {other:?}"),
    }
}

#[tokio::test]
async fn a_caller_whose_policies_allow_no_query_action_is_denied() {
    let (router, engine) = emulator();
    let (graph_id, graph_arn) = graph_in_service(&router, "query-authorization").await;

    /// A document allowing one action on one resource.
    fn allow(action: &str, resource: &str) -> String {
        format!(
            r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow",
            "Action":"{action}","Resource":"{resource}"}}]}}"#
        )
    }

    let other_graph_arn = graph_arn.replace(&graph_id, "g-9999999999");
    // (case, the user the key is held for, its access key id, the policy it carries,
    // whether the query is served). The rows differ along WHICH grant the caller holds:
    // one of the three query actions suffices, a grant for another graph or another
    // action does not, and an explicit deny beats a wildcard grant.
    let rows: &[(&str, &str, &str, Option<String>, bool)] = &[
        ("no policy at all", "analyst-none", "AKIAQUERYNONE", None, false),
        (
            "read on this graph",
            "analyst-read",
            "AKIAQUERYREAD",
            Some(allow("neptune-graph:ReadDataViaQuery", &graph_arn)),
            true,
        ),
        (
            "write on this graph",
            "analyst-write",
            "AKIAQUERYWRITE",
            Some(allow("neptune-graph:WriteDataViaQuery", &graph_arn)),
            true,
        ),
        (
            "delete on this graph",
            "analyst-delete",
            "AKIAQUERYDELETE",
            Some(allow("neptune-graph:DeleteDataViaQuery", &graph_arn)),
            true,
        ),
        (
            "every action of the service on every resource",
            "analyst-all",
            "AKIAQUERYALL",
            Some(allow("neptune-graph:*", "*")),
            true,
        ),
        (
            "read on another graph",
            "analyst-other",
            "AKIAQUERYOTHER",
            Some(allow("neptune-graph:ReadDataViaQuery", &other_graph_arn)),
            false,
        ),
        (
            "a control-plane action on this graph",
            "analyst-control",
            "AKIAQUERYCONTROL",
            Some(allow("neptune-graph:GetGraph", &graph_arn)),
            false,
        ),
        (
            "another service's actions",
            "analyst-elsewhere",
            "AKIAQUERYELSE",
            Some(allow("s3:GetObject", "*")),
            false,
        ),
        (
            "a wildcard grant with the query actions denied",
            "analyst-denied",
            "AKIAQUERYDENIED",
            Some(
                r#"{"Version":"2012-10-17","Statement":[
                {"Effect":"Allow","Action":"neptune-graph:*","Resource":"*"},
                {"Effect":"Deny","Action":["neptune-graph:ReadDataViaQuery",
                 "neptune-graph:WriteDataViaQuery","neptune-graph:DeleteDataViaQuery"],
                 "Resource":"*"}]}"#
                    .to_string(),
            ),
            false,
        ),
    ];

    for (case, user_name, access_key_id, policy, served) in rows {
        let principal = hold_user(&engine, user_name, access_key_id, policy.as_deref());
        let (status, body) =
            execute_query_as(&router, access_key_id, &graph_id, ANY_QUERY).await;
        if *served {
            assert_eq!(
                status, 200,
                "case {case:?}: expected the query to be served, got {status}: {body}"
            );
            continue;
        }
        assert_eq!(status, 403, "case {case:?}: expected 403, got {status}: {body}");
        assert_eq!(
            error_code(&body),
            "AccessDeniedException",
            "case {case:?}: unexpected error code in {body}"
        );
        assert_eq!(
            body["message"],
            json!(format!(
                "User: {principal} is not authorized to perform: \
                 neptune-graph:DeleteDataViaQuery, neptune-graph:ReadDataViaQuery, \
                 neptune-graph:WriteDataViaQuery on resource: {graph_arn}"
            )),
            "case {case:?}: unexpected message in {body}"
        );
    }

    // The emulator's own published credential holds no policy of the account's and is the
    // one caller this decision cannot refuse.
    let (status, payload) = execute_query(&router, &graph_id, ANY_QUERY).await;
    assert_eq!(
        status, 200,
        "case \"the emulator's own credential\": expected 200, got {status}: {payload}"
    );
}

#[tokio::test]
async fn a_query_naming_anything_outside_the_engines_read_only_subset_is_refused() {
    let (router, engine) = emulator();
    let (graph_id, _) = graph_in_service(&router, "query-surface").await;
    seed(
        &engine,
        "the surface graph",
        &graph_id,
        &[("n1", &["Person"], &[("name", json!("Ada"))])],
        &[],
    );

    // What the engine accepts is a READ-ONLY Cypher subset, so everything a caller might
    // reach the host or the store with is outside it and is refused rather than run. The
    // rows span the shapes that would be an escape if one existed, and the modelled
    // reason each is refused under says WHICH way it is outside the subset: a CLAUSE the
    // grammar does not accept (a bulk load from a file, a procedure call, and the four
    // write clauses the engine's own documentation states it does not support) is the
    // unsupported operation it is, while a FUNCTION the evaluator does not implement
    // (named here after a file read, an HTTP fetch of the instance-metadata address and a
    // dynamic import) is the illegal argument it is.
    let outside: &[(&str, &str, &str)] = &[
        (
            "a clause that loads a file",
            "LOAD CSV FROM 'file:///etc/passwd' AS line RETURN line",
            "UNSUPPORTED_OPERATION",
        ),
        (
            "a procedure call",
            "CALL dbms.listConfig() YIELD name RETURN name",
            "UNSUPPORTED_OPERATION",
        ),
        (
            "a function that would read a file",
            "MATCH (n) RETURN readFile('/etc/passwd') AS leaked",
            "ILLEGAL_ARGUMENT",
        ),
        (
            "a function that would fetch a URL",
            "MATCH (n) RETURN httpGet('http://169.254.169.254/latest/meta-data/') AS leaked",
            "ILLEGAL_ARGUMENT",
        ),
        (
            "a dynamic import",
            "MATCH (n) RETURN apocLoadJson('file:///etc/passwd') AS leaked",
            "ILLEGAL_ARGUMENT",
        ),
        (
            "a create",
            "CREATE (n:Person {name: 'Eve'}) RETURN n",
            "UNSUPPORTED_OPERATION",
        ),
        (
            "a merge",
            "MERGE (n:Person {name: 'Eve'}) RETURN n",
            "UNSUPPORTED_OPERATION",
        ),
        (
            "a write to a property",
            "MATCH (n) SET n.name = 'Eve' RETURN n",
            "UNSUPPORTED_OPERATION",
        ),
        (
            "a delete",
            "MATCH (n) DELETE n RETURN n",
            "UNSUPPORTED_OPERATION",
        ),
    ];

    for (case, query, reason) in outside {
        let (status, body) = execute_query(&router, &graph_id, query).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected 400, got {status}: {body}"
        );
        assert_eq!(
            error_code(&body),
            "ValidationException",
            "case {case:?}: unexpected error code in {body}"
        );
        // A well-formed query naming a clause this engine does not run is refused as the
        // UNSUPPORTED operation it is, not as a text that does not parse: a caller must
        // be able to tell "this endpoint does not do that" from "fix your query".
        assert_eq!(
            body["reason"],
            json!(reason),
            "case {case:?}: expected the {reason} reason, got {body}"
        );
    }

    // And the graph is untouched by every one of them: the write clauses stored nothing,
    // so the one node it was seeded with is the one node it still holds.
    let (status, payload) = execute_query(
        &router,
        &graph_id,
        "MATCH (n) RETURN count(n) AS nodes, collect(n.name) AS names",
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"the graph after the refusals\": expected 200, got {status}: {payload}"
    );
    assert_eq!(
        Value::Array(rows("the graph after the refusals", &payload)),
        json!([{"nodes": 1, "names": ["Ada"]}]),
        "case \"the graph after the refusals\": a refused query must change nothing, got \
         {payload}"
    );
}

#[tokio::test]
async fn authorization_is_decided_before_the_graph_is_resolved() {
    let (router, engine) = emulator();
    let absent = "g-0000000000";
    let principal = hold_user(&engine, "analyst-early", "AKIAQUERYEARLY", None);

    // No graph exists at all, so the only decision this call can meet is the one taken
    // from the request alone: the caller learns it may not query, not what the account
    // holds.
    let (status, body) = execute_query_as(&router, "AKIAQUERYEARLY", absent, ANY_QUERY).await;
    assert_eq!(
        status, 403,
        "case \"an unauthorized caller naming no graph\": expected 403, got {status}: {body}"
    );
    let message = body["message"].as_str().unwrap_or_default();
    assert!(
        message.starts_with(&format!("User: {principal} is not authorized to perform:"))
            && message.ends_with(&format!(":graph/{absent}")),
        "case \"an unauthorized caller naming no graph\": expected the denial to name the \
         principal and the graph the request addressed, got {message:?}"
    );
    // A malformed query is not read either: the decision is taken before the text.
    let (syntax_status, syntax_body) =
        execute_query_as(&router, "AKIAQUERYEARLY", absent, "MATCH (n RETURN n").await;
    assert_eq!(
        syntax_status, 403,
        "case \"an unauthorized caller sending a malformed query\": expected 403, got \
         {syntax_status}: {syntax_body}"
    );
}

/// The sentence this endpoint words a parse refusal with, as a pattern over its variable
/// parts: the input the parse stopped on, the alternatives the grammar admitted at that
/// position, and the position as a line, a column and an offset.
///
/// Source: the form the openCypher endpoint states this class of refusal in —
/// `Invalid input '<input>': expected <alternatives> (line <line>, column <column>
/// (offset: <offset>))`.
const REFUSAL_SENTENCE: &str = r"^Invalid input '(?<input>.*)': expected (?<expected>.+) \(line (?<line>\d+), column (?<column>\d+) \(offset: (?<offset>\d+)\)\)$";

/// What a refusal names where the parse stopped at the end of the submitted text.
const END_OF_INPUT: &str = "EOI";

#[tokio::test]
async fn a_parse_refusal_states_the_input_the_alternatives_and_the_position() {
    let (router, _engine) = emulator();
    let (graph_id, _) = graph_in_service(&router, "query-refusal-wording").await;
    let sentence = regex::Regex::new(REFUSAL_SENTENCE).expect("the refusal pattern compiles");

    // Texts no openCypher grammar accepts, spanning what the rule generalises over:
    // WHAT is still open where the parse stops (a node pattern, a list, a projection,
    // nothing at all), WHERE it stops (mid-line, at the end of the text, on a later
    // line) and whether the text ahead of it is ASCII.
    let malformed: &[(&str, &str)] = &[
        ("an unclosed node pattern", "MATCH (n RETURN n"),
        ("a text that is no query at all", "!!!"),
        ("a projection with nothing to project", "MATCH (n) RETURN"),
        ("a break on the second line", "MATCH (n)\nRETRUN n"),
        ("an unclosed list", "UNWIND [0 AS x RETURN x"),
        (
            "a break after multi-byte text",
            "MATCH (n) WHERE n.name = 'é' RETRUN n",
        ),
    ];

    let mut listed: BTreeSet<String> = BTreeSet::new();
    for (case, query) in malformed {
        let (status, body) = execute_query(&router, &graph_id, query).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected 400, got {status}: {body}"
        );
        assert_eq!(
            error_code(&body),
            "ValidationException",
            "case {case:?}: unexpected error code in {body}"
        );
        assert_eq!(
            body["reason"],
            json!("MALFORMED_QUERY"),
            "case {case:?}: expected the MALFORMED_QUERY reason, got {body}"
        );
        let message = body["message"].as_str().unwrap_or_default();
        let stated = sentence.captures(message).unwrap_or_else(|| {
            panic!("case {case:?}: expected the refusal sentence, got {message:?}")
        });

        // Every variable part is a reading of the SUBMITTED text, so the test reads that
        // text itself rather than trusting the sentence: the offset is a character
        // position in the query, the line and the column are that same position, and the
        // input is the character standing there (escaped when it is a control
        // character), or the end of the input when the text ends there.
        let offset: usize = stated["offset"]
            .parse()
            .unwrap_or_else(|_| panic!("case {case:?}: expected an offset, got {message:?}"));
        let characters: Vec<char> = query.chars().collect();
        assert!(
            offset <= characters.len(),
            "case {case:?}: expected a position within the {} characters submitted, got \
             {offset}",
            characters.len()
        );
        let read = &characters[..offset];
        let line = read.iter().filter(|character| **character == '\n').count() + 1;
        let column = read
            .iter()
            .rev()
            .take_while(|character| **character != '\n')
            .count()
            + 1;
        assert_eq!(
            stated["line"].parse::<usize>().ok(),
            Some(line),
            "case {case:?}: expected line {line} for offset {offset}, got {message:?}"
        );
        assert_eq!(
            stated["column"].parse::<usize>().ok(),
            Some(column),
            "case {case:?}: expected column {column} for offset {offset}, got {message:?}"
        );
        let input = characters.get(offset).map_or_else(
            || END_OF_INPUT.to_string(),
            |found| {
                if found.is_control() {
                    found.escape_debug().to_string()
                } else {
                    found.to_string()
                }
            },
        );
        assert_eq!(
            &stated["input"], &input,
            "case {case:?}: expected the input at offset {offset} to be reported, got \
             {message:?}"
        );

        // The alternatives are the grammar's own, so each is named rather than quoted
        // out of a parser's internal wording, and none of them is empty.
        let alternatives = stated["expected"].to_string();
        for alternative in alternatives
            .split(", ")
            .flat_map(|part| part.split(" or "))
        {
            assert!(
                !alternative.trim().is_empty() && !alternative.contains('`'),
                "case {case:?}: expected each alternative to be named, got \
                 {alternatives:?}"
            );
        }
        listed.insert(alternatives);
    }

    // The list is the grammar's answer for ONE position, so texts that stop at different
    // positions cannot all be answered with the same list.
    assert!(
        listed.len() > 1,
        "expected the alternatives to be read off the grammar at each position, got \
         {listed:?}"
    );

    // And none of the refusals disturbed the graph or the session: the projection that
    // reads nothing of the graph still answers its one row.
    let (status, payload) = execute_query(&router, &graph_id, "RETURN 1 AS one").await;
    assert_eq!(
        status, 200,
        "case \"a projection after the refusals\": expected 200, got {status}: {payload}"
    );
    assert_eq!(
        Value::Array(rows("a projection after the refusals", &payload)),
        json!([{"one": 1}]),
        "case \"a projection after the refusals\": expected the one projected row, got \
         {payload}"
    );
}

/// The three IAM actions a query is served under, as the operation's own documentation
/// lists them (`smithy_query(query="doc",shape="ExecuteQuery")`), in the order a denial
/// names them. One of them suffices for the query to run.
const QUERY_ACTIONS: [&str; 3] = [
    "neptune-graph:DeleteDataViaQuery",
    "neptune-graph:ReadDataViaQuery",
    "neptune-graph:WriteDataViaQuery",
];

/// How long a seeded role session lasts: far longer than a run of this suite, so no row's
/// credential expires while it is in use.
const SESSION_LIFETIME: Duration = Duration::from_secs(3600);

/// An AWS-managed policy allowing every action on every resource, whose document the IAM
/// emulator carries in its catalog of AWS's own published policies.
const ADMINISTRATOR_ACCESS_ARN: &str = "arn:aws:iam::aws:policy/AdministratorAccess";

/// A policy of AWS's OWN managed-policy namespace that the IAM emulator carries no
/// document for — a grant belonging to AWS rather than to the account, whose statements
/// nothing here can read. The row that uses it asks the IAM store to confirm that, so it
/// stays the case it is named for.
///
/// What the row asserts is the deliberate reading this emulator takes of an attachment it
/// cannot read, stated in the module docs of `crate::state`'s query-authorization part: on
/// AWS that attachment IS a set of statements, and the account really holds it, so
/// refusing the call would withhold access the real service grants — whereas serving it
/// withholds nothing a readable document says, because an explicit deny in a document
/// that CAN be read still refuses (the row above it) and a permissions boundary the store
/// does hold still caps the grant (the rows below it).
const UNREADABLE_POLICY_ARN: &str = "arn:aws:iam::aws:policy/NeptuneFullAccess";

/// The sentence a caller whose identity policies allow none of [`QUERY_ACTIONS`] on the
/// graph it named is refused with: the principal that signed, the actions it would have
/// needed and the resource it addressed.
fn query_denial_sentence(principal: &str, graph_arn: &str) -> String {
    format!(
        "User: {principal} is not authorized to perform: {} on resource: {graph_arn}",
        QUERY_ACTIONS.join(", ")
    )
}

/// One identity-policy document: a single statement of `effect` over `actions` and
/// `resources`.
fn document(effect: &str, actions: &[&str], resources: &[&str]) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{"Effect": effect, "Action": actions, "Resource": resources}],
    })
    .to_string()
}

/// Which kind of IAM principal a query is signed as, and the IAM path the identity is
/// held under — the path its own ARN carries, which a session ARN does not.
#[derive(Clone, Copy, Debug)]
enum Principal {
    /// A session of an assumed role: the permissions in force are the ROLE's, held under
    /// the role's own ARN.
    AssumedRole { path: &'static str },
    /// An IAM user, addressed by its own ARN, whose permissions are its own.
    IamUser { path: &'static str },
}

/// Every kind of principal each row of the table below is run for.
const PRINCIPAL_KINDS: [(&str, Principal); 4] = [
    ("an IAM user", Principal::IamUser { path: "/" }),
    (
        "an IAM user under a path",
        Principal::IamUser {
            path: "/team/analytics/",
        },
    ),
    (
        "a session of an assumed role",
        Principal::AssumedRole { path: "/" },
    ),
    (
        "a session of a role under a path",
        Principal::AssumedRole {
            path: "/team/analytics/",
        },
    ),
];

/// Where a row holds the document it puts in the account.
#[derive(Clone, Debug)]
enum Source {
    /// An inline policy embedded on the principal itself, where `iam:PutUserPolicy` and
    /// `iam:PutRolePolicy` write one.
    Inline(String),
    /// A managed policy the account owns, attached to the principal — what
    /// `iam:CreatePolicy` plus `iam:AttachUserPolicy` / `iam:AttachRolePolicy` leave
    /// behind. Each version as `(version id, document, whether it is the default)`.
    Attached(Vec<(String, String, bool)>),
    /// An attachment of the policy this ARN names, with no document of the ACCOUNT's
    /// behind it: AWS's own catalog answers for it, or nothing does.
    AttachedByArn(String),
    /// A permissions boundary carrying this document, attached as a managed policy —
    /// where `iam:PutUserPermissionsBoundary` and `iam:PutRolePermissionsBoundary` put
    /// one.
    Boundary(String),
}

/// Put the IAM identity `name` of this kind into the account and hold `access_key_id` for
/// it — the records `iam:CreateUser` plus `iam:CreateAccessKey` write for a user, and the
/// ones `sts:AssumeRole` writes for a session — and answer the principal ARN
/// `sts:GetCallerIdentity` reports for that credential.
fn hold_identity(
    engine: &Arc<engine::InMemoryEngine>,
    principal: Principal,
    name: &str,
    access_key_id: &str,
) -> String {
    let account = engine::emulator_account_id().to_string();
    let state = iam::service_state(&**engine).expect("the engine carries the IAM store");
    match principal {
        Principal::IamUser { path } => {
            let arn = iam::user_arn(path, name);
            let mut store = state.lock();
            store.users.insert(
                iam::name_key(name),
                User {
                    path: path.to_string(),
                    user_name: name.to_string(),
                    user_id: sts::state::unique_id("AIDA", &arn),
                    arn: arn.clone(),
                    create_date: iam::now(),
                    password_last_used: None,
                    permissions_boundary: None,
                    tags: None,
                },
            );
            store.access_keys.insert(
                access_key_id.to_string(),
                iam::AccessKeyRecord {
                    user_key: iam::name_key(name),
                    status: iam::ACCESS_KEY_ACTIVE_STATUS.to_string(),
                    create_date: iam::now(),
                },
            );
            arn
        }
        Principal::AssumedRole { path } => {
            let role_arn = iam::role_arn(path, name);
            state.lock().roles.insert(
                iam::name_key(name),
                Role {
                    path: path.to_string(),
                    role_name: name.to_string(),
                    role_id: sts::state::unique_id("AROA", &role_arn),
                    arn: role_arn.clone(),
                    create_date: iam::now(),
                    assume_role_policy_document: None,
                    description: None,
                    max_session_duration: None,
                    permissions_boundary: None,
                    tags: None,
                    role_last_used: None,
                    source_role_template: None,
                },
            );
            let session_name = format!("{name}-session");
            let arn = format!("arn:aws:sts::{account}:assumed-role/{name}/{session_name}");
            let vended = sts::state::NewSession {
                role_arn: Some(role_arn.clone()),
                ..sts::state::NewSession::new(
                    sts::state::Identity {
                        user_id: format!(
                            "{}:{session_name}",
                            sts::state::unique_id("AROA", &role_arn)
                        ),
                        account,
                        arn: arn.clone(),
                    },
                    sts::state::SessionKind::AssumedRole,
                    i32::try_from(SESSION_LIFETIME.as_secs()).unwrap_or(i32::MAX),
                )
            };
            sts::state::service_state(&**engine)
                .expect("the engine carries the STS store")
                .lock()
                .insert(vended.into_session(access_key_id, SystemTime::now()))
                .expect("the session registry has room for one session");
            arn
        }
    }
}

/// Embed an inline identity policy on the principal, where `iam:PutUserPolicy` and
/// `iam:PutRolePolicy` write one — overwriting a policy of the same name, as those calls
/// do.
fn put_inline_policy(
    engine: &Arc<engine::InMemoryEngine>,
    principal: Principal,
    name: &str,
    policy_name: &str,
    policy_document: &str,
) {
    let state = iam::service_state(&**engine).expect("the engine carries the IAM store");
    let mut store = state.lock();
    let held = match principal {
        Principal::AssumedRole { .. } => &mut store.role_inline_policies,
        Principal::IamUser { .. } => &mut store.user_inline_policies,
    };
    held.entry(iam::name_key(name)).or_default().insert(
        iam::name_key(policy_name),
        iam::InlinePolicy {
            policy_name: policy_name.to_string(),
            policy_document: policy_document.to_string(),
        },
    );
}

/// Remove an inline identity policy from the principal, where `iam:DeleteUserPolicy` and
/// `iam:DeleteRolePolicy` remove one.
fn delete_inline_policy(
    engine: &Arc<engine::InMemoryEngine>,
    principal: Principal,
    name: &str,
    policy_name: &str,
) {
    let state = iam::service_state(&**engine).expect("the engine carries the IAM store");
    let mut store = state.lock();
    let held = match principal {
        Principal::AssumedRole { .. } => &mut store.role_inline_policies,
        Principal::IamUser { .. } => &mut store.user_inline_policies,
    };
    if let Some(policies) = held.get_mut(&iam::name_key(name)) {
        policies.remove(&iam::name_key(policy_name));
    }
}

/// Attach the managed policy `policy_arn` names to the principal, where
/// `iam:AttachUserPolicy` and `iam:AttachRolePolicy` write the attachment.
fn attach_policy(
    engine: &Arc<engine::InMemoryEngine>,
    principal: Principal,
    name: &str,
    policy_arn: &str,
) {
    let state = iam::service_state(&**engine).expect("the engine carries the IAM store");
    let mut store = state.lock();
    let attached = match principal {
        Principal::AssumedRole { .. } => &mut store.role_attached_policies,
        Principal::IamUser { .. } => &mut store.user_attached_policies,
    };
    attached
        .entry(iam::name_key(name))
        .or_default()
        .insert(policy_arn.to_string());
}

/// Set (or, with `None`, lift) the permissions boundary in force on the principal, where
/// `iam:PutUserPermissionsBoundary` and `iam:PutRolePermissionsBoundary` write one.
fn set_boundary(
    engine: &Arc<engine::InMemoryEngine>,
    principal: Principal,
    name: &str,
    policy_arn: Option<&str>,
) {
    let attached = policy_arn.map(|arn| AttachedPermissionsBoundary {
        permissions_boundary_type: Some(iam::PERMISSIONS_BOUNDARY_TYPE.to_string()),
        permissions_boundary_arn: Some(arn.to_string()),
    });
    let state = iam::service_state(&**engine).expect("the engine carries the IAM store");
    let mut store = state.lock();
    match principal {
        Principal::AssumedRole { .. } => {
            let role = store
                .roles
                .get_mut(&iam::name_key(name))
                .unwrap_or_else(|| panic!("the account holds a role named {name}"));
            role.permissions_boundary = attached;
        }
        Principal::IamUser { .. } => {
            let user = store
                .users
                .get_mut(&iam::name_key(name))
                .unwrap_or_else(|| panic!("the account holds a user named {name}"));
            user.permissions_boundary = attached;
        }
    }
}

/// Put a managed policy the ACCOUNT owns into the IAM store, where `iam:CreatePolicy`
/// (plus one `iam:CreatePolicyVersion` per further version) writes it, and answer its ARN.
fn hold_managed_policy(
    engine: &Arc<engine::InMemoryEngine>,
    policy_name: &str,
    versions: &[(String, String, bool)],
) -> String {
    let arn = iam::policy_arn("/", policy_name);
    let created = iam::now();
    let default_version_id = versions
        .iter()
        .find(|(_, _, is_default)| *is_default)
        .map(|(version_id, _, _)| version_id.clone());
    let held: BTreeMap<String, PolicyVersion> = versions
        .iter()
        .map(|(version_id, policy_document, is_default)| {
            (
                version_id.clone(),
                PolicyVersion {
                    document: Some(policy_document.clone()),
                    version_id: Some(version_id.clone()),
                    is_default_version: *is_default,
                    create_date: Some(created),
                },
            )
        })
        .collect();
    let state = iam::service_state(&**engine).expect("the engine carries the IAM store");
    state.lock().policies.insert(
        iam::name_key(policy_name),
        iam::ManagedPolicy {
            policy: Policy {
                arn: Some(arn.clone()),
                policy_id: Some(sts::state::unique_id("ANPA", &arn)),
                policy_name: Some(policy_name.to_string()),
                path: Some("/".to_string()),
                default_version_id,
                attachment_count: Some(1),
                permissions_boundary_usage_count: Some(0),
                is_attachable: true,
                description: None,
                create_date: Some(created),
                update_date: Some(created),
                tags: None,
            },
            versions: held,
        },
    );
    arn
}

/// Hold every document of `sources` where its variant says, for the principal `name`.
fn hold_sources(
    engine: &Arc<engine::InMemoryEngine>,
    principal: Principal,
    name: &str,
    label: &str,
    sources: &[Source],
) {
    for (index, source) in sources.iter().enumerate() {
        match source {
            Source::Inline(policy_document) => put_inline_policy(
                engine,
                principal,
                name,
                &format!("{label}-inline-{index}"),
                policy_document,
            ),
            Source::Attached(versions) => {
                let arn =
                    hold_managed_policy(engine, &format!("{label}-managed-{index}"), versions);
                attach_policy(engine, principal, name, &arn);
            }
            Source::AttachedByArn(arn) => attach_policy(engine, principal, name, arn),
            Source::Boundary(policy_document) => {
                let arn = hold_managed_policy(
                    engine,
                    &format!("{label}-boundary-{index}"),
                    &[("v1".to_string(), policy_document.clone(), true)],
                );
                set_boundary(engine, principal, name, Some(&arn));
            }
        }
    }
}

/// RULE: a query is served only when the identity-based policies in force on the calling
/// principal allow one of the three query actions on the graph the request named, capped
/// by the principal's permissions boundary — read out of the IAM store wherever those
/// documents are held and whichever kind of identity holds them — and a caller they allow
/// none for is refused `AccessDeniedException` (403) naming that principal, those three
/// actions and that graph.
#[tokio::test]
async fn every_policy_source_and_principal_kind_decides_the_query_alike() {
    let (router, engine) = emulator();
    let (graph_id, graph_arn) = graph_in_service(&router, "query-policy-sources").await;
    // Two stored nodes, so an answered query is read off the graph rather than being the
    // empty result any graph would give.
    seed(
        &engine,
        "the policy-sources graph",
        &graph_id,
        &[
            ("n1", &["Person"], &[("name", json!("Ada"))]),
            ("n2", &["Person"], &[("name", json!("Grace"))]),
        ],
        &[],
    );
    let other_graph_arn = graph_arn.replace(&graph_id, "g-9999999999");

    // The row below is only about an attachment with no readable document if the store
    // really holds none for it, which the store itself is asked.
    {
        let store = iam::service_state(&*engine).expect("the engine carries the IAM store");
        assert!(
            store
                .lock()
                .default_version_document(UNREADABLE_POLICY_ARN)
                .is_none(),
            "case \"an attachment this emulator carries no document for\": \
             {UNREADABLE_POLICY_ARN} must be a policy no document is held for"
        );
    }

    let read_here = document("Allow", &["neptune-graph:ReadDataViaQuery"], &[&graph_arn]);
    let write_here = document("Allow", &["neptune-graph:WriteDataViaQuery"], &[&graph_arn]);
    let delete_here = document("Allow", &["neptune-graph:DeleteDataViaQuery"], &[&graph_arn]);
    let read_elsewhere = document(
        "Allow",
        &["neptune-graph:ReadDataViaQuery"],
        &[&other_graph_arn],
    );
    let every_action_of_the_service = document("Allow", &["neptune-graph:*"], &["*"]);
    let deny_every_query_action = document("Deny", &QUERY_ACTIONS, &["*"]);
    let another_service = document("Allow", &["s3:GetObject"], &["*"]);

    // (case, where the account holds the principal's documents, whether the query is
    // served). The rows differ along WHERE a statement lives and WHICH way it bears on
    // the query — an inline policy, a managed policy's default version, a version that is
    // not the default, an attachment AWS's catalog answers for, one nothing answers for,
    // and a permissions boundary — and each is run for every kind of principal below, so
    // no row is decided by the shape of one credential.
    let cases: Vec<(&str, Vec<Source>, bool)> = vec![
        ("no document at all", Vec::new(), false),
        (
            "an inline grant of the read action on this graph",
            vec![Source::Inline(read_here.clone())],
            true,
        ),
        (
            "an inline grant naming another graph",
            vec![Source::Inline(read_elsewhere.clone())],
            false,
        ),
        (
            "an attached policy granting the write action in its default version",
            vec![Source::Attached(vec![(
                "v1".to_string(),
                write_here.clone(),
                true,
            )])],
            true,
        ),
        (
            "an attached policy granting the delete action outside its default version",
            vec![Source::Attached(vec![
                ("v1".to_string(), delete_here.clone(), false),
                ("v2".to_string(), another_service.clone(), true),
            ])],
            false,
        ),
        (
            "an inline deny beside an attached grant of every action of the service",
            vec![
                Source::Inline(deny_every_query_action.clone()),
                Source::Attached(vec![(
                    "v1".to_string(),
                    every_action_of_the_service.clone(),
                    true,
                )]),
            ],
            false,
        ),
        (
            "an attached policy of AWS's own catalog granting every action",
            vec![Source::AttachedByArn(ADMINISTRATOR_ACCESS_ARN.to_string())],
            true,
        ),
        // Served, not refused: see [`UNREADABLE_POLICY_ARN`] for the reading this pins.
        (
            "an attachment this emulator carries no document for",
            vec![Source::AttachedByArn(UNREADABLE_POLICY_ARN.to_string())],
            true,
        ),
        (
            "an inline grant under a boundary naming another service",
            vec![
                Source::Inline(read_here.clone()),
                Source::Boundary(another_service.clone()),
            ],
            false,
        ),
        (
            "an inline grant under a boundary naming every action of the service",
            vec![
                Source::Inline(read_here.clone()),
                Source::Boundary(every_action_of_the_service.clone()),
            ],
            true,
        ),
    ];

    let mut blamed: Vec<String> = Vec::new();
    for (row, (case, sources, served)) in cases.iter().enumerate() {
        for (kind, (kind_case, principal)) in PRINCIPAL_KINDS.iter().enumerate() {
            let case = format!("{case} held for {kind_case}");
            let label = format!("q{row}k{kind}");
            let name = format!("neptune-query-{label}");
            // A long-term key is spelled `AKIA…` and a session's `ASIA…`; neither carries
            // the identity's name, so a principal read off the key id cannot pass a row.
            let access_key_id = match principal {
                Principal::AssumedRole { .. } => format!("ASIA{}", label.to_uppercase()),
                Principal::IamUser { .. } => format!("AKIA{}", label.to_uppercase()),
            };
            let principal_arn = hold_identity(&engine, *principal, &name, &access_key_id);
            hold_sources(&engine, *principal, &name, &label, sources);

            let (status, body) =
                execute_query_as(&router, &access_key_id, &graph_id, ANY_QUERY).await;
            if *served {
                assert_eq!(
                    status, 200,
                    "case {case:?}: expected the query to be served, got {status}: {body}"
                );
                assert_eq!(
                    Value::Array(rows(&case, &body)),
                    json!([{"nodes": 2}]),
                    "case {case:?}: expected the count of the graph's stored nodes, got \
                     {body}"
                );
                continue;
            }
            assert_eq!(
                status, 403,
                "case {case:?}: expected 403, got {status}: {body}"
            );
            assert_eq!(
                error_code(&body),
                "AccessDeniedException",
                "case {case:?}: unexpected error code in {body}"
            );
            assert_eq!(
                body["message"],
                json!(query_denial_sentence(&principal_arn, &graph_arn)),
                "case {case:?}: unexpected message in {body}"
            );
            blamed.push(principal_arn);
        }
    }

    // The refusals are not one sentence asserted many times: each names the identity its
    // own credential speaks for, in the spelling that identity carries.
    for spelling in [":user/", ":assumed-role/", "/team/analytics/"] {
        assert!(
            blamed.iter().any(|arn| arn.contains(spelling)),
            "case \"the principals the refusals name\": expected one spelled with \
             {spelling:?}, got {blamed:?}"
        );
    }
}

/// Every step of the sequence below: the query is sent again and must be answered by what
/// the IAM store holds AT THAT MOMENT.
///
/// A served step answers the count of the graph's stored nodes, `nodes`, so a step the
/// decision let through is an evaluation over the graph rather than a bare 200.
async fn assert_query_outcome(
    router: &MultiServiceRouter,
    case: &str,
    access_key_id: &str,
    graph: (&str, &str),
    principal_arn: &str,
    served: Option<i64>,
) {
    let (graph_id, graph_arn) = graph;
    let (status, body) = execute_query_as(router, access_key_id, graph_id, ANY_QUERY).await;
    if let Some(nodes) = served {
        assert_eq!(
            status, 200,
            "case {case:?}: expected the query to be served, got {status}: {body}"
        );
        assert_eq!(
            Value::Array(rows(case, &body)),
            json!([{"nodes": nodes}]),
            "case {case:?}: expected the count of the graph's stored nodes, got {body}"
        );
        return;
    }
    assert_eq!(
        status, 403,
        "case {case:?}: expected 403, got {status}: {body}"
    );
    assert_eq!(
        error_code(&body),
        "AccessDeniedException",
        "case {case:?}: unexpected error code in {body}"
    );
    assert_eq!(
        body["message"],
        json!(query_denial_sentence(principal_arn, graph_arn)),
        "case {case:?}: unexpected message in {body}"
    );
}

/// RULE (the same decision, from the other side): it is taken from the IAM store on every
/// call, so a document written, replaced, capped or removed between two queries decides
/// the second one — no call is answered by what was in force for an earlier call.
#[tokio::test]
async fn the_decision_follows_the_policies_the_account_holds_at_each_call() {
    let (router, engine) = emulator();
    let (graph_id, graph_arn) = graph_in_service(&router, "query-live-policies").await;
    // Three stored nodes, so a step the decision lets through answers this graph's own
    // count rather than the empty result any graph would give.
    seed(
        &engine,
        "the live-policies graph",
        &graph_id,
        &[
            ("n1", &["Person"], &[("name", json!("Ada"))]),
            ("n2", &["Person"], &[("name", json!("Grace"))]),
            ("n3", &["Person"], &[("name", json!("Alan"))]),
        ],
        &[],
    );
    let graph = (graph_id.as_str(), graph_arn.as_str());
    let served = Some(3);
    let refused = None;
    let principal = Principal::IamUser { path: "/" };
    let name = "neptune-query-live";
    let access_key_id = "AKIAQUERYLIVE";
    let principal_arn = hold_identity(&engine, principal, name, access_key_id);

    let grant = document("Allow", &["neptune-graph:ReadDataViaQuery"], &[&graph_arn]);
    let deny = document("Deny", &QUERY_ACTIONS, &["*"]);
    let boundary = hold_managed_policy(
        &engine,
        "neptune-query-live-cap",
        &[(
            "v1".to_string(),
            document("Allow", &["s3:GetObject"], &["*"]),
            true,
        )],
    );

    // Each step changes the account the way one IAM call would, then asks again. The
    // outcome alternates, so no step's answer can be a constant or a cached decision.
    assert_query_outcome(
        &router,
        "before any policy is written",
        access_key_id,
        graph,
        &principal_arn,
        refused,
    )
    .await;

    put_inline_policy(&engine, principal, name, "query", &grant);
    assert_query_outcome(
        &router,
        "after a grant of the read action is written",
        access_key_id,
        graph,
        &principal_arn,
        served,
    )
    .await;

    put_inline_policy(&engine, principal, name, "query", &deny);
    assert_query_outcome(
        &router,
        "after the grant is replaced by a deny",
        access_key_id,
        graph,
        &principal_arn,
        refused,
    )
    .await;

    put_inline_policy(&engine, principal, name, "query", &grant);
    assert_query_outcome(
        &router,
        "after the deny is replaced by the grant again",
        access_key_id,
        graph,
        &principal_arn,
        served,
    )
    .await;

    set_boundary(&engine, principal, name, Some(&boundary));
    assert_query_outcome(
        &router,
        "after a boundary naming no query action is attached",
        access_key_id,
        graph,
        &principal_arn,
        refused,
    )
    .await;

    set_boundary(&engine, principal, name, None);
    assert_query_outcome(
        &router,
        "after the boundary is lifted",
        access_key_id,
        graph,
        &principal_arn,
        served,
    )
    .await;

    delete_inline_policy(&engine, principal, name, "query");
    assert_query_outcome(
        &router,
        "after the grant is deleted",
        access_key_id,
        graph,
        &principal_arn,
        refused,
    )
    .await;
}
