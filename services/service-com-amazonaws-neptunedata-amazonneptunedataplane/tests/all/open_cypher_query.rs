// SPDX-License-Identifier: CC0-1.0

//! `ExecuteOpenCypherQuery` end to end over HTTP.
//!
//! The rule under test: the endpoint EVALUATES the openCypher query it was handed against
//! this engine's stored graph. So what a query writes is what a later query reads back,
//! what a query answers follows from the graph and the parameters the request carried, and
//! a query the engine cannot run is refused through the modelled error that states why —
//! never answered with something that looks computed.
//!
//! Each suite drives the rule over a SET of inputs: the recorded shape (create a node,
//! read it back, bind a parameter) is one row of each table, and the rows differ along
//! what the rule generalises over — the kind of a property's value, how many labels an
//! element carries, which direction and type a pattern walks, which clause writes, and
//! which bound a query meets.
//!
//! Every case goes through the router, so it exercises routing, deserialization, the
//! handler, the persisted graph and serialization — the same path a client takes.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_neptunedata_amazonneptunedataplane::state::{
    MAX_QUERY_CHARS, MAX_QUERY_NESTING_DEPTH, MAX_QUERY_RESULT_ROWS,
};
use service_com_amazonaws_neptunedata_amazonneptunedataplane::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// A SigV4 credential scope naming this service. `neptune-db` is neptunedata's sigv4
/// signing name, and it is what routes a request to this service.
const SIGNED_FOR_NEPTUNE_DB: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260101/us-east-1/neptune-db/aws4_request, \
     SignedHeaders=host, Signature=abc";

/// The members a node reports on the wire.
/// Source: Neptune's openCypher JSON result format, which spells an element's own
/// members with a `~` prefix.
const ID_MEMBER: &str = "~id";
const ENTITY_TYPE_MEMBER: &str = "~entityType";
const LABELS_MEMBER: &str = "~labels";
const PROPERTIES_MEMBER: &str = "~properties";

/// One emulator: a router carrying only this service, over one shared engine.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(engine));
    router
}

/// What the emulator answered: the HTTP status and the JSON body.
struct Answer {
    status: u16,
    body: Value,
}

impl Answer {
    /// The rows the answer carries, which the `results` member holds.
    fn rows(&self) -> Vec<Value> {
        self.body["results"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    }

    /// The wire error code (`__type`) without its shape namespace, or the empty string on
    /// a success.
    fn error_code(&self) -> &str {
        self.body["__type"]
            .as_str()
            .unwrap_or_default()
            .rsplit('#')
            .next()
            .unwrap_or_default()
    }

    fn detailed_message(&self) -> &str {
        self.body["detailedMessage"].as_str().unwrap_or_default()
    }
}

async fn call(router: &MultiServiceRouter, method: &str, uri: &str, body: Value) -> Answer {
    let payload = body.to_string();
    let request = ::http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "neptune-db.us-east-1.amazonaws.com")
        .header("authorization", SIGNED_FOR_NEPTUNE_DB)
        .header("content-type", "application/json")
        .body(Bytes::from(payload))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let body = response.into_body().to_vec();
    Answer {
        status,
        body: serde_json::from_slice(&body).unwrap_or(Value::Null),
    }
}

/// `ExecuteOpenCypherQuery` is `POST /opencypher`, with the query in the body member
/// `query` and its parameters in `parameters`.
/// Source: smithy_query(query="operation", operation="ExecuteOpenCypherQuery") — the http
/// binding and `openCypherQuery`'s `jsonName`.
async fn query(router: &MultiServiceRouter, text: &str) -> Answer {
    call(router, "POST", "/opencypher", json!({ "query": text })).await
}

async fn query_with(router: &MultiServiceRouter, text: &str, parameters: &str) -> Answer {
    call(
        router,
        "POST",
        "/opencypher",
        json!({ "query": text, "parameters": parameters }),
    )
    .await
}

/// A query that must succeed, with its rows.
async fn answered(router: &MultiServiceRouter, case: &str, text: &str) -> Vec<Value> {
    let answer = query(router, text).await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: {text:?} must be answered, got {} with {}",
        answer.status, answer.body
    );
    answer.rows()
}

/// One element written and read back. The rows differ along the KIND of value a property
/// carries, how many labels an element has, and whether the pattern writes a
/// relationship — everything "a write is what a later read sees" generalises over.
#[derive(Debug, Clone, Copy)]
struct RoundTrip {
    case: &'static str,
    /// The query that writes.
    write: &'static str,
    /// A later query that reads what the write left.
    read: &'static str,
    /// The rows that read must answer.
    expected: fn() -> Vec<Value>,
}

const ROUND_TRIPS: &[RoundTrip] = &[
    RoundTrip {
        case: "a labelled node with a string and an integer property",
        write: "CREATE (n:Person {name: 'Alice', age: 30})",
        read: "MATCH (n:Person {name: 'Alice'}) RETURN n.name AS name, n.age AS age",
        expected: || vec![json!({ "name": "Alice", "age": 30 })],
    },
    RoundTrip {
        case: "a fractional property keeps its value",
        write: "CREATE (n:Reading {celsius: 21.5})",
        read: "MATCH (n:Reading) RETURN n.celsius AS celsius",
        expected: || vec![json!({ "celsius": 21.5 })],
    },
    RoundTrip {
        case: "a boolean property keeps its value",
        write: "CREATE (n:Flagged {active: false})",
        read: "MATCH (n:Flagged) RETURN n.active AS active",
        expected: || vec![json!({ "active": false })],
    },
    RoundTrip {
        case: "a list property keeps its items and their order",
        write: "CREATE (n:Basket {items: ['apple', 'pear', 'plum']})",
        read: "MATCH (n:Basket) RETURN n.items AS items",
        expected: || vec![json!({ "items": ["apple", "pear", "plum"] })],
    },
    RoundTrip {
        case: "a node carries every label the pattern gave it",
        write: "CREATE (n:Person:Employee {name: 'Bob'})",
        read: "MATCH (n:Employee) RETURN labels(n) AS labels",
        expected: || vec![json!({ "labels": ["Person", "Employee"] })],
    },
    RoundTrip {
        case: "a node with no label at all is still matched and read",
        write: "CREATE (n {code: 'ANC'})",
        read: "MATCH (n) RETURN n.code AS code",
        expected: || vec![json!({ "code": "ANC" })],
    },
    RoundTrip {
        case: "a relationship joins the two nodes the pattern created, and carries its own properties",
        write: "CREATE (a:Airport {code: 'ANC'})-[r:ROUTE {dist: 1448}]->(b:Airport {code: 'SEA'})",
        read: "MATCH (a)-[r:ROUTE]->(b) RETURN a.code AS from, b.code AS to, r.dist AS dist",
        expected: || vec![json!({ "from": "ANC", "to": "SEA", "dist": 1448 })],
    },
    RoundTrip {
        case: "several nodes created by one query are all read back, in the order asked for",
        write: "CREATE (:City {name: 'Perth'}), (:City {name: 'Hobart'}), (:City {name: 'Darwin'})",
        read: "MATCH (c:City) RETURN c.name AS name ORDER BY name",
        expected: || {
            vec![
                json!({ "name": "Darwin" }),
                json!({ "name": "Hobart" }),
                json!({ "name": "Perth" }),
            ]
        },
    },
    RoundTrip {
        case: "a query with no reading clause answers one row from its own projection",
        write: "CREATE (n:Noted {at: 'start'})",
        read: "RETURN 1 + 1 AS two",
        expected: || vec![json!({ "two": 2 })],
    },
];

#[tokio::test]
async fn what_a_query_writes_is_what_a_later_query_reads() {
    for case in ROUND_TRIPS {
        // A fresh emulator per row, so each case reads only what its own write left.
        let router = emulator();
        let written = query(&router, case.write).await;
        assert_eq!(
            written.status, 200,
            "case {:?}: the write must be accepted, got {} with {}",
            case.case, written.status, written.body
        );
        assert!(
            written.rows().is_empty(),
            "case {:?}: a query with no RETURN answers no rows, got {:?}",
            case.case,
            written.rows()
        );

        let rows = answered(&router, case.case, case.read).await;
        assert_eq!(
            rows,
            (case.expected)(),
            "case {:?}: {:?} must answer what the write left, got {rows:?}",
            case.case,
            case.read
        );
    }
}

#[tokio::test]
async fn a_returned_element_is_spelled_from_the_store() {
    let router = emulator();
    let created = answered(
        &router,
        "a created node is returned",
        "CREATE (n:Person {name: 'Alice', age: 30}) RETURN n",
    )
    .await;
    let [created_row] = created.as_slice() else {
        panic!("one CREATE answers one row, got {created:?}");
    };
    let node = &created_row["n"];
    assert_eq!(
        node[ENTITY_TYPE_MEMBER], "node",
        "a node reports what kind of element it is, got {node}"
    );
    assert_eq!(
        node[LABELS_MEMBER],
        json!(["Person"]),
        "a node reports every label it carries, got {node}"
    );
    assert_eq!(
        node[PROPERTIES_MEMBER],
        json!({ "name": "Alice", "age": 30 }),
        "a node reports every property set on it, got {node}"
    );
    let id = node[ID_MEMBER]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(!id.is_empty(), "a node is addressed by an id, got {node}");

    // The id is the element's own, so a later read of the same node reports it again and
    // `id()` answers it.
    let read = answered(
        &router,
        "the same node read back",
        "MATCH (n:Person) RETURN n, id(n) AS identity",
    )
    .await;
    let [read_row] = read.as_slice() else {
        panic!("one stored node answers one row, got {read:?}");
    };
    assert_eq!(
        read_row["n"], *node,
        "a later read reports the same node, got {}",
        read_row["n"]
    );
    assert_eq!(
        read_row["identity"],
        json!(id),
        "id() answers the id the node is addressed by, got {}",
        read_row["identity"]
    );

    let relationship = answered(
        &router,
        "a created relationship is returned",
        "MATCH (p:Person) CREATE (p)-[r:LIKES {since: 2020}]->(:Food {name: 'fig'}) RETURN r",
    )
    .await;
    let [relationship_row] = relationship.as_slice() else {
        panic!("one CREATE answers one row, got {relationship:?}");
    };
    let edge = &relationship_row["r"];
    assert_eq!(
        edge[ENTITY_TYPE_MEMBER], "relationship",
        "a relationship reports what kind of element it is, got {edge}"
    );
    assert_eq!(
        edge["~type"], "LIKES",
        "a relationship reports its type, got {edge}"
    );
    assert_eq!(
        edge["~start"],
        json!(id),
        "a relationship reports the node it leaves, got {edge}"
    );
    assert_eq!(
        edge[PROPERTIES_MEMBER],
        json!({ "since": 2020 }),
        "a relationship reports its own properties, got {edge}"
    );
    assert!(
        edge["~end"].as_str().is_some_and(|end| end != id),
        "a relationship reports the node it enters, got {edge}"
    );
}

/// One parameterised query. The rows differ along the KIND of value a parameter binds and
/// where in the query it is used — a property value, a predicate, a whole property map, a
/// list a query expands.
#[derive(Debug, Clone, Copy)]
struct Parameterised {
    case: &'static str,
    write: &'static str,
    parameters: &'static str,
    read: &'static str,
    expected: fn() -> Vec<Value>,
}

const PARAMETERISED: &[Parameterised] = &[
    Parameterised {
        case: "a string parameter binds a property value",
        write: "CREATE (n:Person {name: $name})",
        parameters: r#"{"name": "Alice"}"#,
        read: "MATCH (n:Person) RETURN n.name AS name",
        expected: || vec![json!({ "name": "Alice" })],
    },
    Parameterised {
        case: "an integer parameter binds a property value",
        write: "CREATE (n:Person {age: $age})",
        parameters: r#"{"age": 41}"#,
        read: "MATCH (n:Person) RETURN n.age AS age",
        expected: || vec![json!({ "age": 41 })],
    },
    Parameterised {
        case: "a fractional parameter binds a property value",
        write: "CREATE (n:Reading {celsius: $celsius})",
        parameters: r#"{"celsius": -3.25}"#,
        read: "MATCH (n:Reading) RETURN n.celsius AS celsius",
        expected: || vec![json!({ "celsius": -3.25 })],
    },
    Parameterised {
        case: "a boolean parameter binds a property value",
        write: "CREATE (n:Flagged {active: $active})",
        parameters: r#"{"active": true}"#,
        read: "MATCH (n:Flagged) RETURN n.active AS active",
        expected: || vec![json!({ "active": true })],
    },
    Parameterised {
        case: "a list parameter binds a property value",
        write: "CREATE (n:Basket {items: $items})",
        parameters: r#"{"items": ["apple", "pear"]}"#,
        read: "MATCH (n:Basket) RETURN n.items AS items",
        expected: || vec![json!({ "items": ["apple", "pear"] })],
    },
    Parameterised {
        case: "a map parameter binds a whole set of properties",
        write: "CREATE (n:Person $props)",
        parameters: r#"{"props": {"name": "Carol", "age": 29}}"#,
        read: "MATCH (n:Person) RETURN n.name AS name, n.age AS age",
        expected: || vec![json!({ "name": "Carol", "age": 29 })],
    },
    Parameterised {
        case: "a list parameter is expanded into one row per item",
        write: "UNWIND $names AS name CREATE (:Person {name: name})",
        parameters: r#"{"names": ["Dan", "Eve", "Fay"]}"#,
        read: "MATCH (n:Person) RETURN n.name AS name ORDER BY name",
        expected: || {
            vec![
                json!({ "name": "Dan" }),
                json!({ "name": "Eve" }),
                json!({ "name": "Fay" }),
            ]
        },
    },
];

#[tokio::test]
async fn a_parameter_binds_the_value_the_request_carried() {
    for case in PARAMETERISED {
        let router = emulator();
        let written = query_with(&router, case.write, case.parameters).await;
        assert_eq!(
            written.status, 200,
            "case {:?}: the write must be accepted, got {} with {}",
            case.case, written.status, written.body
        );
        let rows = answered(&router, case.case, case.read).await;
        assert_eq!(
            rows,
            (case.expected)(),
            "case {:?}: the parameter must bind what the request carried, got {rows:?}",
            case.case
        );
    }
}

#[tokio::test]
async fn a_parameter_is_read_in_a_predicate_and_in_a_projection() {
    let router = emulator();
    let written = query_with(
        &router,
        "UNWIND $people AS person CREATE (:Person {name: person.name, age: person.age})",
        r#"{"people": [{"name": "Alice", "age": 30}, {"name": "Bob", "age": 44}]}"#,
    )
    .await;
    assert_eq!(
        written.status, 200,
        "the write must be accepted, got {} with {}",
        written.status, written.body
    );

    let answer = query_with(
        &router,
        "MATCH (n:Person) WHERE n.age > $older_than RETURN n.name AS name, $label AS kind",
        r#"{"older_than": 35, "label": "senior"}"#,
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "a parameterised predicate must be answered, got {} with {}",
        answer.status, answer.body
    );
    assert_eq!(
        answer.rows(),
        vec![json!({ "name": "Bob", "kind": "senior" })],
        "a parameter in a predicate selects, and one in a projection is returned, got {:?}",
        answer.rows()
    );
}

#[tokio::test]
async fn concurrent_writes_are_every_one_committed() {
    let router = Arc::new(emulator());
    // Eight writers at once, each writing one node: the store's own lock serialises them,
    // so the graph holds every one of them afterwards.
    let writers = (0..8).map(|index| {
        let router = Arc::clone(&router);
        tokio::spawn(async move {
            let text = format!("CREATE (:Writer {{index: {index}}})");
            let answer = query(&router, &text).await;
            (index, answer.status)
        })
    });
    for writer in writers.collect::<Vec<_>>() {
        let (index, status) = writer.await.expect("a writer task runs to completion");
        assert_eq!(
            status, 200,
            "case writer {index}: every concurrent write must be accepted, got {status}"
        );
    }

    let rows = answered(
        &router,
        "every concurrent write is committed",
        "MATCH (n:Writer) RETURN n.index AS index ORDER BY index",
    )
    .await;
    let written: Vec<Value> = (0..8).map(|index| json!({ "index": index })).collect();
    assert_eq!(
        rows, written,
        "every concurrent write must be readable afterwards, got {rows:?}"
    );

    let counted = answered(
        &router,
        "the count follows from the stored nodes",
        "MATCH (n:Writer) RETURN count(n) AS total",
    )
    .await;
    assert_eq!(
        counted,
        vec![json!({ "total": 8 })],
        "count() must count what the store holds, got {counted:?}"
    );
}

#[tokio::test]
async fn a_query_that_fails_part_way_writes_nothing() {
    let router = emulator();
    // The first clause writes; the second cannot, because a node is not a property value.
    // The query is one transaction, so neither write survives it.
    let refused = query(
        &router,
        "CREATE (a:Txn {name: 'first'}) CREATE (b:Txn {bad: a})",
    )
    .await;
    assert_eq!(
        refused.status, 400,
        "a write the graph's rules refuse must be refused, got {} with {}",
        refused.status, refused.body
    );
    assert_eq!(
        refused.error_code(),
        "ConstraintViolationException",
        "a value no property can hold is a constraint violation, got {}",
        refused.body
    );

    let rows = answered(
        &router,
        "a failed write leaves the graph as it was",
        "MATCH (n:Txn) RETURN count(n) AS total",
    )
    .await;
    assert_eq!(
        rows,
        vec![json!({ "total": 0 })],
        "a query that failed part way must leave nothing behind, got {rows:?}"
    );
}

/// The graph the pattern-matching table below reads: three airports and the routes
/// between them.
const ROUTE_GRAPH: &str = "CREATE (anc:Airport {code: 'ANC', size: 3}), \
     (sea:Airport {code: 'SEA', size: 5}), \
     (pdx:Airport {code: 'PDX', size: 2}), \
     (anc)-[:ROUTE {dist: 1448}]->(sea), \
     (sea)-[:ROUTE {dist: 129}]->(pdx), \
     (sea)-[:NEARBY]->(pdx)";

/// One pattern read over the same graph. The rows differ along what a pattern selects by:
/// a label, a direction, a relationship type, a property in the pattern, a predicate, the
/// number of hops.
#[derive(Debug, Clone, Copy)]
struct Selection {
    case: &'static str,
    read: &'static str,
    expected: fn() -> Vec<Value>,
}

const SELECTIONS: &[Selection] = &[
    Selection {
        case: "an outgoing hop selects the node it enters",
        read: "MATCH (a:Airport {code: 'ANC'})-[:ROUTE]->(b) RETURN b.code AS code",
        expected: || vec![json!({ "code": "SEA" })],
    },
    Selection {
        case: "an incoming hop selects the node it leaves",
        read: "MATCH (a:Airport {code: 'PDX'})<-[:ROUTE]-(b) RETURN b.code AS code",
        expected: || vec![json!({ "code": "SEA" })],
    },
    Selection {
        case: "an undirected hop selects both ways round",
        read: "MATCH (a:Airport {code: 'SEA'})-[:ROUTE]-(b) RETURN b.code AS code ORDER BY code",
        expected: || vec![json!({ "code": "ANC" }), json!({ "code": "PDX" })],
    },
    Selection {
        case: "the relationship type selects which hop is walked",
        read: "MATCH (a)-[:NEARBY]->(b) RETURN a.code AS from, b.code AS to",
        expected: || vec![json!({ "from": "SEA", "to": "PDX" })],
    },
    Selection {
        case: "a property in the relationship pattern selects the hop",
        read: "MATCH (a)-[:ROUTE {dist: 129}]->(b) RETURN a.code AS from, b.code AS to",
        expected: || vec![json!({ "from": "SEA", "to": "PDX" })],
    },
    Selection {
        case: "a predicate filters the rows a pattern made",
        read: "MATCH (a:Airport) WHERE a.size >= 3 RETURN a.code AS code ORDER BY code",
        expected: || vec![json!({ "code": "ANC" }), json!({ "code": "SEA" })],
    },
    Selection {
        case: "two hops reach the node two routes away",
        read: "MATCH (a:Airport {code: 'ANC'})-[:ROUTE]->()-[:ROUTE]->(c) RETURN c.code AS code",
        expected: || vec![json!({ "code": "PDX" })],
    },
    Selection {
        case: "a variable-length hop reaches everything within its range",
        read: "MATCH (a:Airport {code: 'ANC'})-[:ROUTE*1..2]->(c) RETURN c.code AS code ORDER BY code",
        expected: || vec![json!({ "code": "PDX" }), json!({ "code": "SEA" })],
    },
    Selection {
        case: "a relationship is traversed at most once in one match",
        read: "MATCH (a)-[:ROUTE]-(b)-[:ROUTE]-(c) RETURN a.code AS one, b.code AS two, c.code AS three ORDER BY one",
        expected: || {
            vec![
                json!({ "one": "ANC", "two": "SEA", "three": "PDX" }),
                json!({ "one": "PDX", "two": "SEA", "three": "ANC" }),
            ]
        },
    },
    Selection {
        case: "a pattern nothing matches answers no rows",
        read: "MATCH (a:Airport {code: 'LHR'})-[:ROUTE]->(b) RETURN b.code AS code",
        expected: Vec::new,
    },
    Selection {
        case: "an OPTIONAL MATCH that matches nothing keeps the row and binds null",
        read: "MATCH (a:Airport {code: 'PDX'}) OPTIONAL MATCH (a)-[:ROUTE]->(b) \
               RETURN a.code AS code, b AS onwards",
        expected: || vec![json!({ "code": "PDX", "onwards": null })],
    },
    Selection {
        case: "a relationship property is projected from the hop that carries it",
        read: "MATCH ()-[r:ROUTE]->() RETURN r.dist AS dist ORDER BY dist",
        expected: || vec![json!({ "dist": 129 }), json!({ "dist": 1448 })],
    },
];

#[tokio::test]
async fn a_pattern_selects_what_the_stored_graph_holds() {
    let router = emulator();
    let seeded = query(&router, ROUTE_GRAPH).await;
    assert_eq!(
        seeded.status, 200,
        "the graph must be written, got {} with {}",
        seeded.status, seeded.body
    );

    for case in SELECTIONS {
        let rows = answered(&router, case.case, case.read).await;
        assert_eq!(
            rows,
            (case.expected)(),
            "case {:?}: {:?} must answer from the stored graph, got {rows:?}",
            case.case,
            case.read
        );
    }
}

/// One projection over the same graph. The rows differ along what a projection does:
/// aggregate, group, de-duplicate, order, page.
const PROJECTIONS: &[Selection] = &[
    Selection {
        case: "count(*) counts the rows the pattern made",
        read: "MATCH (a:Airport) RETURN count(*) AS total",
        expected: || vec![json!({ "total": 3 })],
    },
    Selection {
        case: "an aggregate groups by the items that are not aggregates",
        read: "MATCH (a:Airport)-[:ROUTE]->(b) RETURN a.code AS code, count(b) AS onwards \
               ORDER BY code",
        expected: || {
            vec![
                json!({ "code": "ANC", "onwards": 1 }),
                json!({ "code": "SEA", "onwards": 1 }),
            ]
        },
    },
    Selection {
        case: "sum, min, max and avg are taken over the values of the group",
        read: "MATCH (a:Airport) RETURN sum(a.size) AS total, min(a.size) AS smallest, \
               max(a.size) AS largest, avg(a.size) AS mean",
        expected: || {
            vec![json!({
                "total": 10,
                "smallest": 2,
                "largest": 5,
                "mean": 10.0 / 3.0,
            })]
        },
    },
    Selection {
        case: "collect gathers the values of the group into a list",
        read: "MATCH (a:Airport) WITH a ORDER BY a.code RETURN collect(a.code) AS codes",
        expected: || vec![json!({ "codes": ["ANC", "PDX", "SEA"] })],
    },
    Selection {
        case: "DISTINCT drops the duplicate rows a pattern made",
        read: "MATCH (a)-[]->(b) RETURN DISTINCT b.code AS code ORDER BY code",
        expected: || vec![json!({ "code": "PDX" }), json!({ "code": "SEA" })],
    },
    Selection {
        case: "ORDER BY DESC with SKIP and LIMIT pages the ordered rows",
        read: "MATCH (a:Airport) RETURN a.code AS code ORDER BY a.size DESC SKIP 1 LIMIT 1",
        expected: || vec![json!({ "code": "ANC" })],
    },
    Selection {
        case: "an aggregate over no rows at all still answers one row",
        read: "MATCH (a:Spaceport) RETURN count(a) AS total",
        expected: || vec![json!({ "total": 0 })],
    },
    Selection {
        case: "a WITH clause projects, filters and carries into the next clause",
        read: "MATCH (a:Airport) WITH a.code AS code WHERE code <> 'SEA' RETURN code ORDER BY code",
        expected: || vec![json!({ "code": "ANC" }), json!({ "code": "PDX" })],
    },
    Selection {
        case: "a column with no alias is named by the expression the query wrote",
        read: "MATCH (a:Airport {code: 'ANC'}) RETURN a.size",
        expected: || vec![json!({ "a.size": 3 })],
    },
];

#[tokio::test]
async fn a_projection_computes_from_the_rows_it_was_given() {
    let router = emulator();
    let seeded = query(&router, ROUTE_GRAPH).await;
    assert_eq!(
        seeded.status, 200,
        "the graph must be written, got {} with {}",
        seeded.status, seeded.body
    );

    for case in PROJECTIONS {
        let rows = answered(&router, case.case, case.read).await;
        assert_eq!(
            rows,
            (case.expected)(),
            "case {:?}: {:?} must answer from the rows it was given, got {rows:?}",
            case.case,
            case.read
        );
    }
}

/// One update, and what a later read must then see. The rows differ along which clause
/// writes and what it changes.
#[derive(Debug, Clone, Copy)]
struct Update {
    case: &'static str,
    write: &'static str,
    read: &'static str,
    expected: fn() -> Vec<Value>,
}

const UPDATES: &[Update] = &[
    Update {
        case: "SET assigns a property",
        write: "MATCH (a:Airport {code: 'ANC'}) SET a.size = 9",
        read: "MATCH (a:Airport {code: 'ANC'}) RETURN a.size AS size",
        expected: || vec![json!({ "size": 9 })],
    },
    Update {
        case: "SET of null removes the property",
        write: "MATCH (a:Airport {code: 'ANC'}) SET a.size = null",
        read: "MATCH (a:Airport {code: 'ANC'}) RETURN a.size AS size",
        expected: || vec![json!({ "size": null })],
    },
    Update {
        case: "SET += merges a map of properties into the ones already set",
        write: "MATCH (a:Airport {code: 'ANC'}) SET a += {city: 'Anchorage', size: 4}",
        read: "MATCH (a:Airport {code: 'ANC'}) RETURN a.city AS city, a.size AS size",
        expected: || vec![json!({ "city": "Anchorage", "size": 4 })],
    },
    Update {
        case: "SET = replaces every property with the map assigned",
        write: "MATCH (a:Airport {code: 'ANC'}) SET a = {code: 'ANC'}",
        read: "MATCH (a:Airport {code: 'ANC'}) RETURN a.size AS size, a.code AS code",
        expected: || vec![json!({ "size": null, "code": "ANC" })],
    },
    Update {
        case: "SET adds a label a later pattern matches by",
        write: "MATCH (a:Airport {code: 'ANC'}) SET a:Hub",
        read: "MATCH (a:Hub) RETURN labels(a) AS labels",
        expected: || vec![json!({ "labels": ["Airport", "Hub"] })],
    },
    Update {
        case: "REMOVE takes a property away",
        write: "MATCH (a:Airport {code: 'ANC'}) REMOVE a.size",
        read: "MATCH (a:Airport {code: 'ANC'}) RETURN a.size AS size",
        expected: || vec![json!({ "size": null })],
    },
    Update {
        case: "DETACH DELETE removes the node and the relationships it was joined by",
        write: "MATCH (a:Airport {code: 'SEA'}) DETACH DELETE a",
        read: "MATCH (a:Airport) RETURN a.code AS code ORDER BY code",
        expected: || vec![json!({ "code": "ANC" }), json!({ "code": "PDX" })],
    },
    Update {
        case: "DELETE removes a relationship, leaving the nodes it joined",
        write: "MATCH ()-[r:NEARBY]->() DELETE r",
        read: "MATCH ()-[r:NEARBY]->() RETURN count(r) AS total",
        expected: || vec![json!({ "total": 0 })],
    },
    Update {
        case: "MERGE creates the pattern when nothing matches it",
        write: "MERGE (a:Airport {code: 'LHR'})",
        read: "MATCH (a:Airport {code: 'LHR'}) RETURN count(a) AS total",
        expected: || vec![json!({ "total": 1 })],
    },
    Update {
        case: "MERGE creates nothing when the pattern already matches",
        write: "MERGE (a:Airport {code: 'ANC'}) ON MATCH SET a.seen = true",
        read: "MATCH (a:Airport {code: 'ANC'}) RETURN count(a) AS total, a.seen AS seen",
        expected: || vec![json!({ "total": 1, "seen": true })],
    },
];

#[tokio::test]
async fn an_update_is_what_a_later_read_sees() {
    for case in UPDATES {
        let router = emulator();
        let seeded = query(&router, ROUTE_GRAPH).await;
        assert_eq!(
            seeded.status, 200,
            "case {:?}: the graph must be written, got {} with {}",
            case.case, seeded.status, seeded.body
        );
        let written = query(&router, case.write).await;
        assert_eq!(
            written.status, 200,
            "case {:?}: the update must be accepted, got {} with {}",
            case.case, written.status, written.body
        );
        let rows = answered(&router, case.case, case.read).await;
        assert_eq!(
            rows,
            (case.expected)(),
            "case {:?}: the update must be what the later read sees, got {rows:?}",
            case.case
        );
    }
}

/// One query the engine refuses, and the modelled error it is refused through. The rows
/// differ along WHY a query cannot be answered: it is not a query, it names a clause this
/// engine does not run, it asks for something that is not there, or it is past a bound.
#[derive(Debug, Clone, Copy)]
struct Refusal {
    case: &'static str,
    query: &'static str,
    parameters: Option<&'static str>,
    status: u16,
    code: &'static str,
}

const REFUSALS: &[Refusal] = &[
    Refusal {
        case: "a blank query carries nothing to run",
        query: "   ",
        parameters: None,
        status: 400,
        code: "MissingParameterException",
    },
    Refusal {
        case: "a text that is not openCypher is refused by the parser",
        query: "MATCH (n:Person RETURN n",
        parameters: None,
        status: 400,
        code: "MalformedQueryException",
    },
    Refusal {
        case: "a word that is not a clause is refused by the parser",
        query: "SELECT * FROM people",
        parameters: None,
        status: 400,
        code: "MalformedQueryException",
    },
    Refusal {
        case: "a function this engine does not implement is refused by name",
        query: "MATCH (n) RETURN percentileDisc(n.age, 0.5) AS half",
        parameters: None,
        status: 400,
        code: "MalformedQueryException",
    },
    Refusal {
        case: "a variable the query never bound is refused",
        query: "MATCH (n) RETURN m.name AS name",
        parameters: None,
        status: 400,
        code: "MalformedQueryException",
    },
    Refusal {
        case: "a parameter the request did not carry is refused, naming it",
        query: "MATCH (n:Person {name: $name}) RETURN n",
        parameters: None,
        status: 400,
        code: "InvalidParameterException",
    },
    Refusal {
        case: "parameters that are not JSON cannot bind anything",
        query: "RETURN $name AS name",
        parameters: Some("name=Alice"),
        status: 400,
        code: "InvalidParameterException",
    },
    Refusal {
        case: "parameters that are not a JSON object cannot bind anything",
        query: "RETURN $name AS name",
        parameters: Some(r#"["Alice"]"#),
        status: 400,
        code: "InvalidParameterException",
    },
];

#[tokio::test]
async fn a_query_the_engine_cannot_answer_is_refused_through_its_modelled_error() {
    let router = emulator();
    for case in REFUSALS {
        let answer = match case.parameters {
            Some(parameters) => query_with(&router, case.query, parameters).await,
            None => query(&router, case.query).await,
        };
        assert_eq!(
            answer.status, case.status,
            "case {:?}: expected HTTP {}, got {} with {}",
            case.case, case.status, answer.status, answer.body
        );
        assert_eq!(
            answer.error_code(),
            case.code,
            "case {:?}: expected {}, got {}",
            case.case,
            case.code,
            answer.body
        );
        assert!(
            !answer.detailed_message().is_empty(),
            "case {:?}: a refusal says what was wrong, got {}",
            case.case,
            answer.body
        );
    }
}

#[tokio::test]
async fn a_parameter_refusal_names_the_parameter_the_query_wanted() {
    let router = emulator();
    let answer = query(&router, "MATCH (n:Person {name: $wanted}) RETURN n").await;
    assert!(
        answer.detailed_message().contains("wanted"),
        "the refusal names the parameter the query referenced, got {}",
        answer.body
    );
}

#[tokio::test]
async fn deleting_a_node_that_still_has_relationships_is_refused() {
    let router = emulator();
    let seeded = query(&router, ROUTE_GRAPH).await;
    assert_eq!(
        seeded.status, 200,
        "the graph must be written, got {} with {}",
        seeded.status, seeded.body
    );

    let refused = query(&router, "MATCH (a:Airport {code: 'SEA'}) DELETE a").await;
    assert_eq!(
        refused.status, 400,
        "deleting a joined node must be refused, got {} with {}",
        refused.status, refused.body
    );
    assert_eq!(
        refused.error_code(),
        "ConstraintViolationException",
        "a delete the graph's rules refuse is a constraint violation, got {}",
        refused.body
    );

    let rows = answered(
        &router,
        "the refused delete removed nothing",
        "MATCH (a:Airport) RETURN count(a) AS total",
    )
    .await;
    assert_eq!(
        rows,
        vec![json!({ "total": 3 })],
        "a refused delete must leave every node, got {rows:?}"
    );
}

#[tokio::test]
async fn a_clause_this_engine_does_not_run_is_refused_as_unsupported() {
    let router = emulator();
    // `LOAD CSV` reads a file, which this engine has none of: a well-formed query naming
    // it is a different answer to the caller than a text that is not a query.
    let answer = query(
        &router,
        "LOAD CSV WITH HEADERS FROM 'https://example.test/x.csv' AS row RETURN row",
    )
    .await;
    assert_eq!(
        answer.status, 400,
        "a clause this engine does not run must be refused, got {} with {}",
        answer.status, answer.body
    );
    assert_eq!(
        answer.error_code(),
        "UnsupportedOperationException",
        "the refusal says the clause is not supported, got {}",
        answer.body
    );
}

#[tokio::test]
async fn query_length_is_bounded_at_the_engine_ceiling() {
    let router = emulator();
    // One character inside the ceiling is read (and refused for its own reason, if any);
    // one past it is refused as too large before the parser sees it.
    let filler = "a".repeat(MAX_QUERY_CHARS - "RETURN '' AS text".len());
    let within = format!("RETURN '{filler}' AS text");
    assert_eq!(
        within.chars().count(),
        MAX_QUERY_CHARS,
        "the longest query a request may carry is {MAX_QUERY_CHARS} characters"
    );
    let answer = query(&router, &within).await;
    assert_eq!(
        answer.status, 200,
        "a query at the ceiling must be read, got {} with {}",
        answer.status, answer.body
    );

    let past = format!("RETURN '{filler}x' AS text");
    let answer = query(&router, &past).await;
    assert_eq!(
        answer.status, 400,
        "a query past the ceiling must be refused, got {} with {}",
        answer.status, answer.body
    );
    assert_eq!(
        answer.error_code(),
        "QueryTooLargeException",
        "a query past the length ceiling is too large, got {}",
        answer.body
    );
}

#[tokio::test]
async fn nesting_is_bounded_at_the_engine_ceiling() {
    let router = emulator();
    let within = format!(
        "RETURN {}1{} AS one",
        "(".repeat(MAX_QUERY_NESTING_DEPTH),
        ")".repeat(MAX_QUERY_NESTING_DEPTH)
    );
    let answer = query(&router, &within).await;
    assert_eq!(
        answer.status, 200,
        "a query at the nesting ceiling must be read, got {} with {}",
        answer.status, answer.body
    );
    assert_eq!(
        answer.rows(),
        vec![json!({ "one": 1 })],
        "a query at the nesting ceiling must still be evaluated, got {:?}",
        answer.rows()
    );

    let past = format!(
        "RETURN {}1{} AS one",
        "(".repeat(MAX_QUERY_NESTING_DEPTH + 1),
        ")".repeat(MAX_QUERY_NESTING_DEPTH + 1)
    );
    let answer = query(&router, &past).await;
    assert_eq!(
        answer.status, 400,
        "a query past the nesting ceiling must be refused, got {} with {}",
        answer.status, answer.body
    );
    assert_eq!(
        answer.error_code(),
        "QueryLimitException",
        "a query past the nesting ceiling meets a system limit, got {}",
        answer.body
    );
}

#[tokio::test]
async fn a_result_larger_than_the_engine_answers_with_is_refused() {
    let router = emulator();
    let rows = MAX_QUERY_RESULT_ROWS + 1;
    let answer = query(&router, &format!("UNWIND range(1, {rows}) AS x RETURN x")).await;
    assert_eq!(
        answer.status, 400,
        "a result past the row ceiling must be refused, got {} with {}",
        answer.status, answer.body
    );
    assert_eq!(
        answer.error_code(),
        "QueryLimitException",
        "a result past the row ceiling meets a system limit, got {}",
        answer.body
    );
}

/// One query that meets a bound on what one evaluation may BUILD or how far it may
/// descend, and the limit error it is refused through. The rows differ along which bound
/// is met: the size of a value, the depth of an expression, the depth of a walk.
const BOUNDED: &[Refusal] = &[
    Refusal {
        case: "a chain of doublings asks for more than one value may carry",
        // Each clause doubles the value before it, so a few hundred characters of query
        // ask for gigabytes: the size is charged and refused before it is built.
        query: "WITH 'xxxxxxxx' AS a WITH a + a AS a WITH a + a AS a WITH a + a AS a \
                WITH a + a AS a WITH a + a AS a WITH a + a AS a WITH a + a AS a \
                WITH a + a AS a WITH a + a AS a WITH a + a AS a WITH a + a AS a \
                WITH a + a AS a WITH a + a AS a WITH a + a AS a WITH a + a AS a \
                WITH a + a AS a WITH a + a AS a WITH a + a AS a WITH a + a AS a \
                WITH a + a AS a WITH a + a AS a WITH a + a AS a WITH a + a AS a \
                WITH a + a AS a WITH a + a AS a WITH a + a AS a WITH a + a AS a \
                WITH a + a AS a WITH a + a AS a RETURN size(a) AS size",
        parameters: None,
        status: 400,
        code: "QueryLimitException",
    },
    Refusal {
        case: "a chain of list doublings asks for more items than one list may hold",
        query: "WITH [1, 2, 3, 4] AS l WITH l + l AS l WITH l + l AS l WITH l + l AS l \
                WITH l + l AS l WITH l + l AS l WITH l + l AS l WITH l + l AS l \
                WITH l + l AS l WITH l + l AS l WITH l + l AS l WITH l + l AS l \
                WITH l + l AS l WITH l + l AS l WITH l + l AS l WITH l + l AS l \
                WITH l + l AS l WITH l + l AS l WITH l + l AS l WITH l + l AS l \
                RETURN size(l) AS size",
        parameters: None,
        status: 400,
        code: "QueryLimitException",
    },
    Refusal {
        case: "a run of prefix operators nests a parse as deeply as brackets would",
        query: "RETURN NOT NOT NOT NOT NOT NOT NOT NOT NOT NOT NOT NOT NOT NOT NOT NOT \
                NOT NOT NOT NOT NOT NOT NOT NOT NOT NOT NOT NOT NOT NOT NOT NOT NOT NOT \
                NOT NOT NOT NOT NOT NOT NOT NOT NOT NOT NOT NOT NOT NOT NOT NOT NOT NOT \
                NOT NOT NOT NOT NOT NOT NOT NOT NOT NOT NOT NOT NOT NOT NOT NOT NOT NOT \
                true AS flag",
        parameters: None,
        status: 400,
        code: "QueryLimitException",
    },
    Refusal {
        case: "a variable-length pattern asking to walk further than the engine walks",
        query: "MATCH (a:Airport)-[:ROUTE*1..20]->(b) RETURN b.code AS code",
        parameters: None,
        status: 400,
        code: "QueryLimitException",
    },
    Refusal {
        case: "a negative hop range is not a range of hops",
        query: "MATCH (a:Airport)-[:ROUTE*-2..3]->(b) RETURN b.code AS code",
        parameters: None,
        status: 400,
        code: "MalformedQueryException",
    },
];

#[tokio::test]
async fn a_query_past_a_bound_on_what_it_builds_is_refused_with_that_limit() {
    let router = emulator();
    let seeded = query(&router, ROUTE_GRAPH).await;
    assert_eq!(
        seeded.status, 200,
        "the graph must be written, got {} with {}",
        seeded.status, seeded.body
    );

    for case in BOUNDED {
        let answer = query(&router, case.query).await;
        assert_eq!(
            answer.status, case.status,
            "case {:?}: expected HTTP {}, got {} with {}",
            case.case, case.status, answer.status, answer.body
        );
        assert_eq!(
            answer.error_code(),
            case.code,
            "case {:?}: expected {}, got {}",
            case.case,
            case.code,
            answer.body
        );
    }

    // The graph is still readable afterwards: a refused query leaves the engine serving.
    let rows = answered(
        &router,
        "the engine serves the next query",
        "MATCH (a:Airport) RETURN count(a) AS total",
    )
    .await;
    assert_eq!(
        rows,
        vec![json!({ "total": 3 })],
        "a refused query must leave the engine serving, got {rows:?}"
    );
}

/// One expression whose value follows from openCypher's own rules rather than from
/// whatever the host's arithmetic or comparison happened to produce.
const COMPUTED: &[Selection] = &[
    Selection {
        case: "numbers order by their value, fractions included",
        read: "UNWIND [3.5, 1.25, 2.0, 1.0] AS x RETURN x ORDER BY x",
        expected: || {
            vec![
                json!({ "x": 1.0 }),
                json!({ "x": 1.25 }),
                json!({ "x": 2.0 }),
                json!({ "x": 3.5 }),
            ]
        },
    },
    Selection {
        case: "an integer equals the fraction of the same number",
        read: "RETURN 2 = 2.0 AS same",
        expected: || vec![json!({ "same": true })],
    },
    Selection {
        case: "two maps are not ordered against each other",
        read: "RETURN {a: 1} < {a: 2} AS ordered",
        expected: || vec![json!({ "ordered": null })],
    },
    Selection {
        case: "an empty search string replaces nothing",
        read: "RETURN replace('abc', '', 'x') AS text",
        expected: || vec![json!({ "text": "abc" })],
    },
    Selection {
        case: "a list is read by position from either end",
        read: "WITH [10, 20, 30] AS l RETURN l[0] AS first, l[-1] AS last, l[1..] AS rest",
        expected: || vec![json!({ "first": 10, "last": 30, "rest": [20, 30] })],
    },
];

#[tokio::test]
async fn an_expression_is_computed_by_opencyphers_own_rules() {
    let router = emulator();
    for case in COMPUTED {
        let rows = answered(&router, case.case, case.read).await;
        assert_eq!(
            rows,
            (case.expected)(),
            "case {:?}: {:?} must answer what openCypher states, got {rows:?}",
            case.case,
            case.read
        );
    }
}

/// One expression whose value openCypher does not define, which is refused rather than
/// answered with something that looks computed.
const UNCOMPUTABLE: &[Refusal] = &[
    Refusal {
        case: "dividing a number by zero has no value",
        query: "RETURN 1.0 / 0.0 AS quotient",
        parameters: None,
        status: 400,
        code: "MalformedQueryException",
    },
    Refusal {
        case: "the root of a negative number is not a number",
        query: "RETURN sqrt(-4.0) AS root",
        parameters: None,
        status: 400,
        code: "MalformedQueryException",
    },
    Refusal {
        case: "a negative start is not a position in a string",
        query: "RETURN substring('abcdef', -1) AS part",
        parameters: None,
        status: 400,
        code: "MalformedQueryException",
    },
    Refusal {
        case: "a length that is not an integer is not a length",
        query: "RETURN substring('abcdef', 1, 'two') AS part",
        parameters: None,
        status: 400,
        code: "MalformedQueryException",
    },
];

#[tokio::test]
async fn an_expression_the_language_does_not_define_is_refused() {
    let router = emulator();
    for case in UNCOMPUTABLE {
        let answer = query(&router, case.query).await;
        assert_eq!(
            answer.status, case.status,
            "case {:?}: expected HTTP {}, got {} with {}",
            case.case, case.status, answer.status, answer.body
        );
        assert_eq!(
            answer.error_code(),
            case.code,
            "case {:?}: expected {}, got {}",
            case.case,
            case.code,
            answer.body
        );
    }
}

#[tokio::test]
async fn a_members_name_is_not_a_property_a_query_may_write() {
    let router = emulator();
    let seeded = query(&router, "CREATE (n:Person {name: 'Alice'}) RETURN n").await;
    assert_eq!(
        seeded.status, 200,
        "the node must be written, got {} with {}",
        seeded.status, seeded.body
    );
    let id = seeded
        .rows()
        .first()
        .and_then(|row| row["n"][ID_MEMBER].as_str().map(str::to_owned))
        .unwrap_or_default();
    assert!(!id.is_empty(), "the node reports an id, got {}", seeded.body);

    // The members an element reports (its id above all) are the store's to report: a
    // query that could write one as a property would make an element report an id it is
    // not addressed by.
    let refused = query_with(
        &router,
        "MATCH (n:Person) SET n += $props",
        r#"{"props": {"~id": "forged"}}"#,
    )
    .await;
    assert_eq!(
        refused.status, 400,
        "writing a reserved member must be refused, got {} with {}",
        refused.status, refused.body
    );
    assert_eq!(
        refused.error_code(),
        "ConstraintViolationException",
        "a reserved member is a constraint on what a query may write, got {}",
        refused.body
    );

    let rows = answered(
        &router,
        "the node still reports the id it is addressed by",
        "MATCH (n:Person) RETURN n",
    )
    .await;
    let reported = rows
        .first()
        .and_then(|row| row["n"][ID_MEMBER].as_str().map(str::to_owned))
        .unwrap_or_default();
    assert_eq!(
        reported, id,
        "a refused write must leave the element's own id, got {rows:?}"
    );
}

#[tokio::test]
async fn the_names_and_labels_one_element_carries_are_bounded() {
    let router = emulator();
    // A label is a string the caller chose and the store keeps it for as long as the
    // element, so its length is bounded like a value's.
    let long = "A".repeat(5_000);
    let answer = query(&router, &format!("CREATE (n:{long})")).await;
    assert_eq!(
        answer.status, 400,
        "a label past the name ceiling must be refused, got {} with {}",
        answer.status, answer.body
    );
    assert_eq!(
        answer.error_code(),
        "ConstraintViolationException",
        "a name past the ceiling is a constraint on the write, got {}",
        answer.body
    );

    let seeded = query(&router, "CREATE (n:Labelled {name: 'one'})").await;
    assert_eq!(
        seeded.status, 200,
        "the node must be written, got {} with {}",
        seeded.status, seeded.body
    );
    // Labels are de-duplicated by value, so what has to be bounded is how many DIFFERENT
    // ones one node can be made to carry.
    let labels: String = (0..80).map(|index| format!(":L{index}")).collect();
    let answer = query(&router, &format!("MATCH (n:Labelled) SET n{labels}")).await;
    assert_eq!(
        answer.status, 400,
        "more labels than one node carries must be refused, got {} with {}",
        answer.status, answer.body
    );
    assert_eq!(
        answer.error_code(),
        "ConstraintViolationException",
        "a label count past the ceiling is a constraint on the write, got {}",
        answer.body
    );

    // The refused write left the node as it was: one query is one transaction.
    let rows = answered(
        &router,
        "the node still carries the label it was created with",
        "MATCH (n:Labelled) RETURN labels(n) AS labels",
    )
    .await;
    assert_eq!(
        rows,
        vec![json!({ "labels": ["Labelled"] })],
        "a refused write must leave the labels it found, got {rows:?}"
    );
}

#[tokio::test]
async fn a_fast_reset_empties_the_graph() {
    let router = emulator();
    let seeded = query(&router, ROUTE_GRAPH).await;
    assert_eq!(
        seeded.status, 200,
        "the graph must be written, got {} with {}",
        seeded.status, seeded.body
    );

    // `ExecuteFastReset` is `POST /system`: the initiate step mints the token the perform
    // step has to present.
    let initiated = call(
        &router,
        "POST",
        "/system",
        json!({ "action": "initiateDatabaseReset" }),
    )
    .await;
    assert_eq!(
        initiated.status, 200,
        "the reset must be initiated, got {} with {}",
        initiated.status, initiated.body
    );
    let token = initiated.body["payload"]["token"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        !token.is_empty(),
        "the initiate step mints a token, got {}",
        initiated.body
    );

    let performed = call(
        &router,
        "POST",
        "/system",
        json!({ "action": "performDatabaseReset", "token": token }),
    )
    .await;
    assert_eq!(
        performed.status, 200,
        "the reset must be performed, got {} with {}",
        performed.status, performed.body
    );

    let rows = answered(
        &router,
        "a reset database holds no data",
        "MATCH (n) RETURN count(n) AS nodes",
    )
    .await;
    assert_eq!(
        rows,
        vec![json!({ "nodes": 0 })],
        "a performed reset empties the graph, got {rows:?}"
    );
}
