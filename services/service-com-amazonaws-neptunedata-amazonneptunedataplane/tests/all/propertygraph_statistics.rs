// SPDX-License-Identifier: CC0-1.0

//! The property-graph statistics endpoints end to end over HTTP:
//! `ManagePropertygraphStatistics`, `GetPropertygraphStatistics`,
//! `DeletePropertygraphStatistics` and `GetPropertygraphSummary`.
//!
//! The rule under test: the endpoints report the ONE statistics state this engine holds for
//! property-graph data. A management call is what starts that state and what sets it — an
//! autocompute mode persists on it, and a `refresh` records a generation run whose counts
//! are the characteristic sets AND the graph summary of the stored graph — a read answers
//! from it, and a delete removes it, so a read before any management call or after a delete
//! is refused with the modelled `StatisticsNotAvailableException` and a delete that found
//! nothing answers 204 instead of 200.
//!
//! The summary endpoint adds one rule of its own: its `mode` is bound to the query string
//! and its own documentation states the values in upper case ("Mode can take one of two
//! values: BASIC (the default), and DETAILED") while the model lists them in lower case, so
//! the value a client sends is matched to the modelled set without regard to ASCII case,
//! and a value matching none of them is refused with the modelled
//! `IllegalArgumentException` rather than with an error class this service does not model.
//!
//! The tables below span what the rules generalise over: which mode a call names, the CASE
//! a summary mode is spelled in, how many calls precede a read, and — for a run's counts —
//! how many nodes the graph holds, which property keys they carry, whether two nodes share
//! a set of keys, whether a node carries no keys at all, and which relationship types leave
//! a node.
//!
//! Every case goes through the router, so it exercises routing, deserialization, the
//! handler, the persisted state and serialization — the same path a client takes.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_neptunedata_amazonneptunedataplane::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// A SigV4 credential scope naming this service. `neptune-db` is neptunedata's sigv4
/// signing name, and it is what routes a request to this service.
const SIGNED_FOR_NEPTUNE_DB: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260101/us-east-1/neptune-db/aws4_request, \
     SignedHeaders=host, Signature=abc";

/// The three values the model's `StatisticsAutoGenerationMode` enum carries, spelled out
/// here rather than imported so a wrong spelling in the handler fails this test.
/// Source: smithy_query(query="shape", shape="StatisticsAutoGenerationMode").
const DISABLE_AUTO_COMPUTE: &str = "disableAutoCompute";
const ENABLE_AUTO_COMPUTE: &str = "enableAutoCompute";
const REFRESH: &str = "refresh";

/// The route every property-graph statistics operation is reached at.
/// Source: smithy_query(query="digest") — the http bindings of the three operations.
const STATISTICS_PATH: &str = "/propertygraph/statistics";

/// `Statistics$statisticsId` when no run has produced statistics: "A value of -1 indicates
/// that no statistics have been generated".
const NO_STATISTICS: &str = "-1";

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

    fn request_id(&self) -> &str {
        self.body["requestId"].as_str().unwrap_or_default()
    }

    fn payload(&self) -> &Value {
        &self.body["payload"]
    }
}

async fn call(router: &MultiServiceRouter, method: &str, uri: &str, body: Option<Value>) -> Answer {
    let payload = body.map(|value| value.to_string()).unwrap_or_default();
    let mut builder = ::http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "neptune-db.us-east-1.amazonaws.com")
        .header("authorization", SIGNED_FOR_NEPTUNE_DB);
    if !payload.is_empty() {
        builder = builder.header("content-type", "application/json");
    }
    let request = builder
        .body(Bytes::from(payload))
        .expect("the test request is well formed");

    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or_else(|error| {
            panic!(
                "{method} {uri} answered a body that is not JSON ({error}): {:?}",
                String::from_utf8_lossy(&bytes)
            )
        })
    };
    Answer { status, body }
}

/// `ManagePropertygraphStatistics` is `POST /propertygraph/statistics`, with `mode` in the
/// JSON body.
async fn manage(router: &MultiServiceRouter, mode: Option<&str>) -> Answer {
    let body = match mode {
        Some(mode) => json!({ "mode": mode }),
        None => json!({}),
    };
    call(router, "POST", STATISTICS_PATH, Some(body)).await
}

/// `GetPropertygraphStatistics` is `GET /propertygraph/statistics`, with no input.
async fn get(router: &MultiServiceRouter) -> Answer {
    call(router, "GET", STATISTICS_PATH, None).await
}

/// `DeletePropertygraphStatistics` is `DELETE /propertygraph/statistics`, with no input.
async fn delete(router: &MultiServiceRouter) -> Answer {
    call(router, "DELETE", STATISTICS_PATH, None).await
}

/// Write to the graph through the openCypher endpoint, the way a client fills it.
async fn write_graph(router: &MultiServiceRouter, case: &str, query: &str) {
    let answer = call(
        router,
        "POST",
        "/opencypher",
        Some(json!({ "query": query })),
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "case {case}: the seed query {query:?} must be answered, got {} with body {:?}",
        answer.status, answer.body
    );
}

/// A management call that must succeed, with the whole answer.
async fn managed(router: &MultiServiceRouter, case: &str, mode: &str) -> Answer {
    let answer = manage(router, Some(mode)).await;
    assert_eq!(
        answer.status, 200,
        "case {case}: mode {mode:?} must be accepted, got {} with body {:?}",
        answer.status, answer.body
    );
    assert_eq!(
        answer.body["status"].as_str(),
        Some("200 OK"),
        "case {case}: expected the served status, got {:?}",
        answer.body["status"]
    );
    answer
}

/// A read that must succeed, with the whole answer.
async fn read(router: &MultiServiceRouter, case: &str) -> Answer {
    let answer = get(router).await;
    assert_eq!(
        answer.status, 200,
        "case {case}: the statistics must be reported, got {} with body {:?}",
        answer.status, answer.body
    );
    assert_eq!(
        answer.body["status"].as_str(),
        Some("200 OK"),
        "case {case}: expected the served status, got {:?}",
        answer.body["status"]
    );
    answer
}

/// Assert a read was refused as statistics-not-available: the status, the modelled error
/// code, and the two members every Neptune error carries (`code` naming the exception
/// class, `requestId` naming the response).
fn assert_not_available(case: &str, answer: &Answer) {
    assert_eq!(
        answer.status, 400,
        "case {case}: expected HTTP 400, got {} with body {:?}",
        answer.status, answer.body
    );
    assert_eq!(
        answer.error_code(),
        "StatisticsNotAvailableException",
        "case {case}: expected the modelled StatisticsNotAvailableException, got {:?}",
        answer.body
    );
    assert_eq!(
        answer.body["code"].as_str(),
        Some("StatisticsNotAvailableException"),
        "case {case}: the error must carry its exception class in `code`, got {:?}",
        answer.body["code"]
    );
    assert!(
        !answer.request_id().is_empty(),
        "case {case}: the error must name its request, got {:?}",
        answer.body["requestId"]
    );
    assert!(
        !answer.detailed_message().is_empty(),
        "case {case}: the error must state what happened, got {:?}",
        answer.body["detailedMessage"]
    );
}

#[tokio::test]
async fn statistics_are_not_available_before_any_management_call() {
    let router = emulator();

    // Nothing has set or generated statistics, so there are none to report — and a read
    // must not create any either.
    for round in 1..=2u32 {
        let case = format!("read #{round} on a fresh engine");
        assert_not_available(&case, &get(&router).await);
    }

    // A delete with nothing to delete says so with 204 rather than 200, and still leaves
    // nothing to report.
    let deleted = delete(&router).await;
    assert_eq!(
        deleted.status, 204,
        "case delete on a fresh engine: expected 204, got {} with body {:?}",
        deleted.status, deleted.body
    );
    assert!(
        deleted.payload().is_null(),
        "case delete on a fresh engine: a delete that removed nothing reports no deletion payload, got {:?}",
        deleted.payload()
    );
    assert_not_available("read after an empty delete", &get(&router).await);
}

/// One sequence of management calls, and the autocompute mode a read must report after it.
/// The rows differ along which mode each call names, how many calls there are, and whether
/// a `refresh` sits between the toggles — the only things the reported mode can key on.
#[derive(Debug, Clone, Copy)]
struct ModeCase {
    calls: &'static [&'static str],
    expected_auto_compute: bool,
}

const MODE_CASES: &[ModeCase] = &[
    ModeCase {
        calls: &[DISABLE_AUTO_COMPUTE],
        expected_auto_compute: false,
    },
    ModeCase {
        calls: &[ENABLE_AUTO_COMPUTE],
        expected_auto_compute: true,
    },
    // A refresh says nothing about the mode, so the engine's own default stands.
    ModeCase {
        calls: &[REFRESH],
        expected_auto_compute: true,
    },
    ModeCase {
        calls: &[DISABLE_AUTO_COMPUTE, ENABLE_AUTO_COMPUTE],
        expected_auto_compute: true,
    },
    ModeCase {
        calls: &[ENABLE_AUTO_COMPUTE, DISABLE_AUTO_COMPUTE],
        expected_auto_compute: false,
    },
    // The same mode twice is the same state: the second call is not a toggle.
    ModeCase {
        calls: &[DISABLE_AUTO_COMPUTE, DISABLE_AUTO_COMPUTE],
        expected_auto_compute: false,
    },
    // A generation run between the toggles must not put the mode back.
    ModeCase {
        calls: &[DISABLE_AUTO_COMPUTE, REFRESH],
        expected_auto_compute: false,
    },
    ModeCase {
        calls: &[REFRESH, DISABLE_AUTO_COMPUTE, REFRESH],
        expected_auto_compute: false,
    },
];

#[tokio::test]
async fn the_autocompute_mode_a_management_call_set_is_what_a_read_reports() {
    for case in MODE_CASES {
        let router = emulator();
        let name = format!("{:?}", case.calls);

        let mut ran_generation = false;
        for mode in case.calls {
            let answer = managed(&router, &name, mode).await;
            ran_generation |= *mode == REFRESH;
            // "This is only returned for refresh mode": a mode that only says HOW
            // statistics are generated performs no run, so it hands back no id.
            if *mode == REFRESH {
                assert!(
                    answer.payload()["statisticsId"].is_string(),
                    "case {name}: a refresh must report the id of the run it performed, got {:?}",
                    answer.body
                );
            } else {
                assert!(
                    answer.payload().is_null(),
                    "case {name}: mode {mode:?} performs no run, so it reports no payload, got {:?}",
                    answer.body
                );
            }
        }

        // The mode persists: it is read back through the reader, and twice over, because a
        // read must not change it.
        for round in 1..=2u32 {
            let answer = read(&router, &name).await;
            assert_eq!(
                answer.payload()["autoCompute"].as_bool(),
                Some(case.expected_auto_compute),
                "case {name} (read #{round}): expected autoCompute={}, got {:?}",
                case.expected_auto_compute,
                answer.payload()
            );
            assert_eq!(
                answer.payload()["active"].as_bool(),
                Some(ran_generation),
                "case {name} (read #{round}): statistics are in use only once a run produced them, got {:?}",
                answer.payload()
            );
            if ran_generation {
                assert_ne!(
                    answer.payload()["statisticsId"].as_str(),
                    Some(NO_STATISTICS),
                    "case {name} (read #{round}): a run has produced statistics, so the id must name it, got {:?}",
                    answer.payload()
                );
            } else {
                assert_eq!(
                    answer.payload()["statisticsId"].as_str(),
                    Some(NO_STATISTICS),
                    "case {name} (read #{round}): no run has produced statistics, so the id must be -1, got {:?}",
                    answer.payload()
                );
                assert!(
                    answer.payload()["date"].is_null()
                        && answer.payload()["signatureInfo"].is_null(),
                    "case {name} (read #{round}): with no run there is no generation instant and nothing counted, got {:?}",
                    answer.payload()
                );
            }
        }
    }
}

/// One graph, and the characteristic sets a run over it must count. The rows differ along
/// how many nodes the graph holds, which property keys they carry, whether two nodes share
/// a set of keys, whether a node carries none, and which relationship types leave a node.
///
/// A node's predicates are its property keys together with the types of the relationships
/// leaving it — the two things the model's `NodeStructure` describes a node's structure by
/// (`nodeProperties` and `distinctOutgoingEdgeLabels`).
#[derive(Debug, Clone, Copy)]
struct SignatureCase {
    case: &'static str,
    /// The openCypher writes that fill the graph, in order.
    seed: &'static [&'static str],
    signature_count: i64,
    instance_count: i64,
    predicate_count: i64,
}

const SIGNATURE_CASES: &[SignatureCase] = &[
    SignatureCase {
        case: "an empty graph has no characteristic set at all",
        seed: &[],
        signature_count: 0,
        instance_count: 0,
        predicate_count: 0,
    },
    SignatureCase {
        case: "one node with one property key",
        seed: &["CREATE (n:Person {name: 'Alice'})"],
        signature_count: 1,
        instance_count: 1,
        predicate_count: 1,
    },
    SignatureCase {
        case: "two nodes carrying the same key are two instances of one set",
        seed: &["CREATE (:Person {name: 'Alice'}), (:Person {name: 'Bob'})"],
        signature_count: 1,
        instance_count: 2,
        predicate_count: 1,
    },
    SignatureCase {
        case: "two nodes carrying different keys are two sets",
        seed: &["CREATE (:Person {name: 'Alice'}), (:City {population: 5})"],
        signature_count: 2,
        instance_count: 2,
        predicate_count: 2,
    },
    SignatureCase {
        case: "a node carrying several keys is one set of all of them",
        seed: &["CREATE (:Person {name: 'Alice', age: 30, active: true})"],
        signature_count: 1,
        instance_count: 1,
        predicate_count: 3,
    },
    SignatureCase {
        case: "a node carrying no predicate belongs to no set",
        seed: &["CREATE (:Person {name: 'Alice'}), (:Placeholder)"],
        signature_count: 1,
        instance_count: 1,
        predicate_count: 1,
    },
    SignatureCase {
        case: "a relationship type leaving a node is one of that node's predicates",
        seed: &["CREATE (:Person {name: 'Alice'})-[:KNOWS]->(:Person {name: 'Bob'})"],
        signature_count: 2,
        instance_count: 2,
        predicate_count: 2,
    },
    SignatureCase {
        case: "two relationship types leaving one node widen that node's set",
        seed: &[
            "CREATE (:Person {name: 'Alice'})-[:KNOWS]->(:Person {name: 'Bob'})",
            "MATCH (a:Person {name: 'Alice'}) CREATE (a)-[:LIKES]->(:Person {name: 'Carol'})",
        ],
        signature_count: 2,
        instance_count: 3,
        predicate_count: 3,
    },
    SignatureCase {
        case: "a relationship between nodes with no properties still makes the start node an instance",
        seed: &["CREATE (:Left)-[:LINKS]->(:Right)"],
        signature_count: 1,
        instance_count: 1,
        predicate_count: 1,
    },
    SignatureCase {
        case: "the same relationship type twice from one node is one predicate",
        seed: &[
            "CREATE (:Person {name: 'Alice'})-[:KNOWS]->(:Person {name: 'Bob'})",
            "MATCH (a:Person {name: 'Alice'}) CREATE (a)-[:KNOWS]->(:Person {name: 'Carol'})",
        ],
        signature_count: 2,
        instance_count: 3,
        predicate_count: 2,
    },
];

#[tokio::test]
async fn a_generation_run_counts_the_characteristic_sets_of_the_stored_graph() {
    for case in SIGNATURE_CASES {
        let router = emulator();
        for query in case.seed {
            write_graph(&router, case.case, query).await;
        }

        let refreshed = managed(&router, case.case, REFRESH).await;
        let run_id = refreshed.payload()["statisticsId"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(
            !run_id.is_empty() && run_id != NO_STATISTICS,
            "case {}: a refresh must report the id of the run it performed, got {:?}",
            case.case,
            refreshed.body
        );

        let answer = read(&router, case.case).await;
        let payload = answer.payload();
        assert_eq!(
            payload["statisticsId"].as_str(),
            Some(run_id.as_str()),
            "case {}: the read must report the run the refresh performed, got {:?}",
            case.case,
            payload
        );
        assert_eq!(
            payload["active"].as_bool(),
            Some(true),
            "case {}: a generated statistics set is in use, got {:?}",
            case.case,
            payload
        );
        // The wire form of a timestamp is the framework's to choose (this protocol's
        // default is epoch seconds); what the read must report is the instant itself.
        let date = &payload["date"];
        assert!(
            date.as_f64().is_some_and(|instant| instant > 0.0)
                || date.as_str().is_some_and(|text| text.contains('T')),
            "case {}: the read must report when the run generated the statistics, got {date:?}",
            case.case
        );
        let signatures = &payload["signatureInfo"];
        assert_eq!(
            signatures["signatureCount"].as_i64(),
            Some(case.signature_count),
            "case {}: expected signatureCount={}, got {signatures:?}",
            case.case,
            case.signature_count
        );
        assert_eq!(
            signatures["instanceCount"].as_i64(),
            Some(case.instance_count),
            "case {}: expected instanceCount={}, got {signatures:?}",
            case.case,
            case.instance_count
        );
        assert_eq!(
            signatures["predicateCount"].as_i64(),
            Some(case.predicate_count),
            "case {}: expected predicateCount={}, got {signatures:?}",
            case.case,
            case.predicate_count
        );
        // "A note about problems in the case where statistics are invalid": the counts were
        // taken from the graph itself, so there is no problem to note.
        assert!(
            payload["note"].is_null(),
            "case {}: nothing is wrong with these statistics, so there is no note, got {:?}",
            case.case,
            payload["note"]
        );
    }
}

#[tokio::test]
async fn a_later_run_reports_a_later_id_and_the_graph_as_it_then_stood() {
    let router = emulator();

    let first = managed(&router, "first run", REFRESH).await;
    let first_id = first.payload()["statisticsId"]
        .as_str()
        .unwrap_or_default()
        .to_string();

    // The first run counted an empty graph, which is what the read reports until another
    // run replaces it: a run is a snapshot, not a live view.
    let before = read(&router, "first run").await;
    assert_eq!(
        before.payload()["signatureInfo"]["instanceCount"].as_i64(),
        Some(0),
        "case first run: the run counted an empty graph, got {:?}",
        before.payload()
    );

    // The graph grows between the runs, so the second run counts something the first could
    // not have.
    write_graph(
        &router,
        "second run",
        "CREATE (:Person {name: 'Alice'})-[:KNOWS]->(:Person {name: 'Bob'})",
    )
    .await;

    // The first run's counts do not move with the graph — only the next run's do.
    let unchanged = read(&router, "first run after a write").await;
    assert_eq!(
        unchanged.payload(),
        before.payload(),
        "case first run after a write: a recorded run must keep the counts it took, got {:?}",
        unchanged.payload()
    );

    let second = managed(&router, "second run", REFRESH).await;
    let second_id = second.payload()["statisticsId"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert_ne!(
        first_id, second_id,
        "case second run: each run must be reported by its own id"
    );
    let (first_number, second_number) = (
        first_id.parse::<u128>().ok(),
        second_id.parse::<u128>().ok(),
    );
    assert!(
        first_number.is_some() && second_number.is_some(),
        "case second run: both ids must be numeric, got {first_id:?} and {second_id:?}"
    );
    assert!(
        second_number > first_number,
        "case second run: the later run's id must be the later one, got {first_id:?} then {second_id:?}"
    );

    let answer = read(&router, "second run").await;
    assert_eq!(
        answer.payload()["statisticsId"].as_str(),
        Some(second_id.as_str()),
        "case second run: the read must report the latest run, got {:?}",
        answer.payload()
    );
    assert_eq!(
        answer.payload()["signatureInfo"]["instanceCount"].as_i64(),
        Some(2),
        "case second run: the run must have counted the graph as it then stood, got {:?}",
        answer.payload()
    );
}

#[tokio::test]
async fn deleting_statistics_leaves_a_read_nothing_to_report() {
    let router = emulator();

    managed(&router, "seeded", DISABLE_AUTO_COMPUTE).await;
    managed(&router, "seeded", REFRESH).await;
    read(&router, "seeded").await;

    let deleted = delete(&router).await;
    assert_eq!(
        deleted.status, 200,
        "case delete: a delete that removed statistics answers 200, got {} with body {:?}",
        deleted.status, deleted.body
    );
    assert_eq!(
        deleted.body["status"].as_str(),
        Some("200 OK"),
        "case delete: expected the served status, got {:?}",
        deleted.body["status"]
    );
    assert_eq!(
        deleted.payload()["active"].as_bool(),
        Some(false),
        "case delete: nothing is in use after the delete, got {:?}",
        deleted.payload()
    );
    assert_eq!(
        deleted.payload()["statisticsId"].as_str(),
        Some(NO_STATISTICS),
        "case delete: no run is occurring after the delete, got {:?}",
        deleted.payload()
    );

    assert_not_available("read after a delete", &get(&router).await);

    // The second delete has nothing left to delete, and says so.
    let again = delete(&router).await;
    assert_eq!(
        again.status, 204,
        "case second delete: expected 204, got {} with body {:?}",
        again.status, again.body
    );
    assert!(
        again.payload().is_null(),
        "case second delete: a delete that removed nothing reports no deletion payload, got {:?}",
        again.payload()
    );

    // And a management call starts the statistics state again from scratch, so the refusal
    // above is about what the delete removed rather than about the endpoint giving up.
    managed(&router, "after the delete", ENABLE_AUTO_COMPUTE).await;
    let restarted = read(&router, "after the delete").await;
    assert_eq!(
        restarted.payload()["autoCompute"].as_bool(),
        Some(true),
        "case after the delete: the mode this call set is what the read reports, got {:?}",
        restarted.payload()
    );
    assert_eq!(
        restarted.payload()["statisticsId"].as_str(),
        Some(NO_STATISTICS),
        "case after the delete: the deleted run must not come back, got {:?}",
        restarted.payload()
    );
}

/// A management call that must be refused, and the modelled error code it must be refused
/// with — the empty string where the refusal is the framework's own, because the value is
/// outside the modelled `StatisticsAutoGenerationMode` enum and never reaches the handler.
/// The rows span the forms a `mode` can arrive in: absent, present but empty or blank, the
/// right word in the wrong case, a near miss, and free text.
/// `MissingParameterException` is the modelled "a required parameter is missing".
const REFUSED_MODES: &[(Option<&str>, &str)] = &[
    (None, "MissingParameterException"),
    (Some(""), ""),
    (Some("   "), ""),
    (Some("REFRESH"), ""),
    (Some("Refresh"), ""),
    (Some("refreshAll"), ""),
    (Some("enable"), ""),
    (Some("disableautocompute"), ""),
];

#[tokio::test]
async fn a_mode_the_service_does_not_accept_is_refused_and_changes_nothing() {
    for (mode, expected_code) in REFUSED_MODES {
        let router = emulator();
        let case = format!("{mode:?}");

        let answer = manage(&router, *mode).await;
        assert_eq!(
            answer.status, 400,
            "case {case}: an unaccepted mode must be refused, got {} with body {:?}",
            answer.status, answer.body
        );
        if !expected_code.is_empty() {
            assert_eq!(
                answer.error_code(),
                *expected_code,
                "case {case}: expected the modelled {expected_code}, got {:?}",
                answer.body
            );
            assert!(
                answer.detailed_message().contains("mode"),
                "case {case}: the message must name the member that was missing, got {:?}",
                answer.detailed_message()
            );
        }

        // A refused call must not start the statistics state either.
        assert_not_available(&case, &get(&router).await);
    }
}

#[tokio::test]
async fn two_reads_of_the_same_statistics_answer_the_same_thing() {
    let router = emulator();
    write_graph(
        &router,
        "repeated read",
        "CREATE (:Person {name: 'Alice', age: 30})-[:KNOWS]->(:Person {name: 'Bob'})",
    )
    .await;
    managed(&router, "repeated read", REFRESH).await;

    let first = read(&router, "repeated read").await;
    let second = read(&router, "repeated read").await;
    assert_eq!(
        first.body, second.body,
        "case repeated read: a read must not change what a later read sees, got {:?} then {:?}",
        first.body, second.body
    );

    // Including after a delete has removed them and a new run has produced new ones: the
    // reader reports the state, never a cached answer.
    delete(&router).await;
    managed(&router, "repeated read after a new run", REFRESH).await;
    let third = read(&router, "repeated read after a new run").await;
    assert_ne!(
        third.payload()["statisticsId"],
        first.payload()["statisticsId"],
        "case repeated read after a new run: the new run must be reported by its own id, got {:?}",
        third.payload()
    );
}

// ---------------------------------------------------------------------------------------
// `GetPropertygraphSummary`: the graph summary the latest run computed, and the `mode` a
// request asks for it in.
// ---------------------------------------------------------------------------------------

/// The route the graph summary is read at.
/// Source: smithy_query(query="operation", operation="GetPropertygraphSummary") — the http
/// binding `GET /propertygraph/statistics/summary` and `mode`'s `httpQuery`.
const SUMMARY_PATH: &str = "/propertygraph/statistics/summary";

/// `PropertygraphSummaryValueMap$version` — the version of the graph summary response
/// Neptune publishes for a property graph.
const SUMMARY_VERSION: &str = "v1";

/// The values the model's `GraphSummaryType` enum carries, as the refusal lists them.
/// Source: smithy_query(query="shape", shape="GraphSummaryType").
const GRAPH_SUMMARY_TYPES: &str = "basic, detailed";

/// `GetPropertygraphSummary` is `GET /propertygraph/statistics/summary`, with `mode` in the
/// query string; `None` sends no `mode` at all, which is the member's default (`basic`).
async fn summary(router: &MultiServiceRouter, mode: Option<&str>) -> Answer {
    let uri = match mode {
        Some(mode) => format!("{SUMMARY_PATH}?mode={mode}"),
        None => SUMMARY_PATH.to_string(),
    };
    call(router, "GET", &uri, None).await
}

/// A summary read that must succeed, with the `graphSummary` it reported.
async fn summarised(router: &MultiServiceRouter, case: &str, mode: Option<&str>) -> Value {
    let answer = summary(router, mode).await;
    assert_eq!(
        answer.status, 200,
        "case {case}: mode {mode:?} must be served, got {} with body {:?}",
        answer.status, answer.body
    );
    // `statusCode` is "the HTTP return code of the request", bound to the response's own
    // status, so the status line above is where it is read and the body carries no copy.
    assert!(
        answer.body["statusCode"].is_null(),
        "case {case}: the return code is the response's status, not a body member, got {:?}",
        answer.body
    );
    assert_eq!(
        answer.payload()["version"].as_str(),
        Some(SUMMARY_VERSION),
        "case {case}: expected the published response version, got {:?}",
        answer.payload()
    );
    // The wire form of a timestamp is the framework's to choose; what the read must report
    // is the instant the run that counted this summary ran.
    let computed = &answer.payload()["lastStatisticsComputationTime"];
    assert!(
        computed.as_f64().is_some_and(|instant| instant > 0.0)
            || computed.as_str().is_some_and(|text| text.contains('T')),
        "case {case}: the summary must report when the statistics were computed, got {computed:?}"
    );
    answer.payload()["graphSummary"].clone()
}

/// One spelling of `mode`, and whether the summary it asks for carries the structures.
/// The rows differ along the CASE the value is spelled in — the model's own lower case, the
/// upper case the member's documentation states, and a mixed case that is neither — and
/// along whether the request names a mode at all.
const SUMMARY_MODES: &[(Option<&str>, bool)] = &[
    (None, false),
    (Some("basic"), false),
    (Some("BASIC"), false),
    (Some("Basic"), false),
    (Some("bAsIc"), false),
    (Some("detailed"), true),
    (Some("DETAILED"), true),
    (Some("Detailed"), true),
    (Some("DeTaIlEd"), true),
];

#[tokio::test]
async fn a_summary_mode_is_read_in_any_case_and_detailed_is_what_adds_the_structures() {
    let router = emulator();
    write_graph(
        &router,
        "mode spellings",
        "CREATE (:Person {name: 'Alice'})-[:KNOWS {since: 2020}]->(:Person {name: 'Bob'})",
    )
    .await;
    managed(&router, "mode spellings", REFRESH).await;

    for (mode, expects_structures) in SUMMARY_MODES {
        let case = format!("mode {mode:?}");
        let graph_summary = summarised(&router, &case, *mode).await;

        // Every mode reports the counts and the lists: only the structures are "only
        // present when the requested mode is DETAILED".
        assert_eq!(
            graph_summary["numNodes"].as_i64(),
            Some(2),
            "case {case}: the summary must count the stored nodes, got {graph_summary:?}"
        );
        assert_eq!(
            graph_summary["nodeLabels"],
            json!(["Person"]),
            "case {case}: the summary must list the stored node labels, got {graph_summary:?}"
        );
        if *expects_structures {
            assert_eq!(
                graph_summary["nodeStructures"],
                json!([
                    { "count": 1, "nodeProperties": ["name"], "distinctOutgoingEdgeLabels": [] },
                    { "count": 1, "nodeProperties": ["name"], "distinctOutgoingEdgeLabels": ["KNOWS"] },
                ]),
                "case {case}: a detailed summary reports the node structures, got {graph_summary:?}"
            );
            assert_eq!(
                graph_summary["edgeStructures"],
                json!([{ "count": 1, "edgeProperties": ["since"] }]),
                "case {case}: a detailed summary reports the edge structures, got {graph_summary:?}"
            );
        } else {
            assert!(
                graph_summary["nodeStructures"].is_null()
                    && graph_summary["edgeStructures"].is_null(),
                "case {case}: the structures are present only for the detailed mode, got {graph_summary:?}"
            );
        }
    }
}

/// A `mode` value that names none of the modelled values, in the spelling the request
/// carries it in. The rows span what a value can be beside a listed one: empty, a word
/// outside the set, a prefix and a suffix of a listed one, a listed one with a space in
/// front of it (percent-encoded, as a query string carries it), and two values at once.
const REFUSED_SUMMARY_MODES: &[(&str, &str)] = &[
    ("", ""),
    ("verbose", "verbose"),
    ("full", "full"),
    ("detaile", "detaile"),
    ("detailedly", "detailedly"),
    ("%20detailed", " detailed"),
    ("basic,detailed", "basic,detailed"),
];

#[tokio::test]
async fn a_summary_mode_outside_the_modelled_values_is_refused_as_an_illegal_argument() {
    let router = emulator();
    write_graph(
        &router,
        "refused modes",
        "CREATE (:Person {name: 'Alice'})",
    )
    .await;
    managed(&router, "refused modes", REFRESH).await;

    for (sent, decoded) in REFUSED_SUMMARY_MODES {
        let case = format!("mode {sent:?}");
        let answer = summary(&router, Some(sent)).await;
        assert_eq!(
            answer.status, 400,
            "case {case}: a value naming no mode must be refused, got {} with body {:?}",
            answer.status, answer.body
        );
        // The refusal must be one of the operation's OWN modelled error classes, so an SDK
        // can dispatch on it; `IllegalArgumentException` is "an argument in a request is
        // not supported".
        assert_eq!(
            answer.error_code(),
            "IllegalArgumentException",
            "case {case}: expected the modelled IllegalArgumentException, got {:?}",
            answer.body
        );
        assert_eq!(
            answer.body["code"].as_str(),
            Some("IllegalArgumentException"),
            "case {case}: the error must carry its exception class in `code`, got {:?}",
            answer.body["code"]
        );
        assert!(
            !answer.request_id().is_empty(),
            "case {case}: the error must name its request, got {:?}",
            answer.body["requestId"]
        );
        let message = answer.detailed_message();
        assert!(
            message.contains("mode")
                && message.contains(decoded)
                && message.contains(GRAPH_SUMMARY_TYPES),
            "case {case}: the message must name the member, the value and the accepted \
             modes, got {message:?}"
        );

        // A refused read changes nothing: the summary a listed value asks for is still
        // there.
        let graph_summary = summarised(&router, &case, Some("DETAILED")).await;
        assert_eq!(
            graph_summary["numNodes"].as_i64(),
            Some(1),
            "case {case}: a refused read must not change the stored summary, got {graph_summary:?}"
        );
    }
}

/// One graph, and the summary a run over it must report. The rows differ along how many
/// nodes and relationships the graph holds, which property keys each carries, whether two
/// elements share a set of keys, whether an element carries none, and which relationship
/// types leave a node.
///
/// The list members are stated as JSON so a row states the ORDER too: every list the
/// summary reports is sorted, so two reads of one run answer the same summary.
#[derive(Debug, Clone, Copy)]
struct SummaryCase {
    case: &'static str,
    /// The openCypher writes that fill the graph, in order.
    seed: &'static [&'static str],
    num_nodes: i64,
    num_edges: i64,
    /// "A list of the distinct node labels in the graph."
    node_labels: &'static str,
    edge_labels: &'static str,
    /// Each distinct property key with the number of elements it is set on.
    node_properties: &'static str,
    edge_properties: &'static str,
    /// "The total number of usages of all node properties."
    total_node_property_values: i64,
    total_edge_property_values: i64,
    /// The structures, which only a detailed summary reports.
    node_structures: &'static str,
    edge_structures: &'static str,
}

const SUMMARY_CASES: &[SummaryCase] = &[
    SummaryCase {
        case: "an empty graph is summarised as empty",
        seed: &[],
        num_nodes: 0,
        num_edges: 0,
        node_labels: "[]",
        edge_labels: "[]",
        node_properties: "[]",
        edge_properties: "[]",
        total_node_property_values: 0,
        total_edge_property_values: 0,
        node_structures: "[]",
        edge_structures: "[]",
    },
    SummaryCase {
        case: "one labelled node carrying one property key",
        seed: &["CREATE (:Person {name: 'Alice'})"],
        num_nodes: 1,
        num_edges: 0,
        node_labels: r#"["Person"]"#,
        edge_labels: "[]",
        node_properties: r#"[{"name": 1}]"#,
        edge_properties: "[]",
        total_node_property_values: 1,
        total_edge_property_values: 0,
        node_structures: r#"[{"count": 1, "nodeProperties": ["name"], "distinctOutgoingEdgeLabels": []}]"#,
        edge_structures: "[]",
    },
    SummaryCase {
        case: "two nodes carrying the same key share one structure and one property entry",
        seed: &["CREATE (:Person {name: 'Alice'}), (:Person {name: 'Bob'})"],
        num_nodes: 2,
        num_edges: 0,
        node_labels: r#"["Person"]"#,
        edge_labels: "[]",
        node_properties: r#"[{"name": 2}]"#,
        edge_properties: "[]",
        total_node_property_values: 2,
        total_edge_property_values: 0,
        node_structures: r#"[{"count": 2, "nodeProperties": ["name"], "distinctOutgoingEdgeLabels": []}]"#,
        edge_structures: "[]",
    },
    SummaryCase {
        case: "nodes with different labels and keys are listed once each, sorted",
        seed: &["CREATE (:Person {name: 'Alice', age: 30}), (:City {population: 5})"],
        num_nodes: 2,
        num_edges: 0,
        node_labels: r#"["City", "Person"]"#,
        edge_labels: "[]",
        node_properties: r#"[{"age": 1}, {"name": 1}, {"population": 1}]"#,
        edge_properties: "[]",
        total_node_property_values: 3,
        total_edge_property_values: 0,
        node_structures: r#"[
            {"count": 1, "nodeProperties": ["age", "name"], "distinctOutgoingEdgeLabels": []},
            {"count": 1, "nodeProperties": ["population"], "distinctOutgoingEdgeLabels": []}
        ]"#,
        edge_structures: "[]",
    },
    SummaryCase {
        case: "a node carrying no property key still has a structure of its own",
        seed: &["CREATE (:Person {name: 'Alice'}), (:Placeholder)"],
        num_nodes: 2,
        num_edges: 0,
        node_labels: r#"["Person", "Placeholder"]"#,
        edge_labels: "[]",
        node_properties: r#"[{"name": 1}]"#,
        edge_properties: "[]",
        total_node_property_values: 1,
        total_edge_property_values: 0,
        node_structures: r#"[
            {"count": 1, "nodeProperties": [], "distinctOutgoingEdgeLabels": []},
            {"count": 1, "nodeProperties": ["name"], "distinctOutgoingEdgeLabels": []}
        ]"#,
        edge_structures: "[]",
    },
    SummaryCase {
        case: "a relationship is counted, labelled and structured by its own keys",
        seed: &["CREATE (:Person {name: 'Alice'})-[:KNOWS {since: 2020}]->(:Person {name: 'Bob'})"],
        num_nodes: 2,
        num_edges: 1,
        node_labels: r#"["Person"]"#,
        edge_labels: r#"["KNOWS"]"#,
        node_properties: r#"[{"name": 2}]"#,
        edge_properties: r#"[{"since": 1}]"#,
        total_node_property_values: 2,
        total_edge_property_values: 1,
        node_structures: r#"[
            {"count": 1, "nodeProperties": ["name"], "distinctOutgoingEdgeLabels": []},
            {"count": 1, "nodeProperties": ["name"], "distinctOutgoingEdgeLabels": ["KNOWS"]}
        ]"#,
        edge_structures: r#"[{"count": 1, "edgeProperties": ["since"]}]"#,
    },
    SummaryCase {
        case: "two relationship types leaving one node widen that node's structure",
        seed: &[
            "CREATE (:Person {name: 'Alice'})-[:KNOWS]->(:Person {name: 'Bob'})",
            "MATCH (a:Person {name: 'Alice'}) CREATE (a)-[:LIKES]->(:Person {name: 'Carol'})",
        ],
        num_nodes: 3,
        num_edges: 2,
        node_labels: r#"["Person"]"#,
        edge_labels: r#"["KNOWS", "LIKES"]"#,
        node_properties: r#"[{"name": 3}]"#,
        edge_properties: "[]",
        total_node_property_values: 3,
        total_edge_property_values: 0,
        node_structures: r#"[
            {"count": 2, "nodeProperties": ["name"], "distinctOutgoingEdgeLabels": []},
            {"count": 1, "nodeProperties": ["name"], "distinctOutgoingEdgeLabels": ["KNOWS", "LIKES"]}
        ]"#,
        edge_structures: r#"[{"count": 2, "edgeProperties": []}]"#,
    },
];

/// The JSON a row states a list member as.
fn stated(case: &str, member: &str, json_text: &str) -> Value {
    serde_json::from_str(json_text)
        .unwrap_or_else(|error| panic!("case {case}: {member} is not JSON ({error}): {json_text}"))
}

#[tokio::test]
async fn a_detailed_summary_reports_the_graph_the_latest_run_counted() {
    for case in SUMMARY_CASES {
        let router = emulator();
        for query in case.seed {
            write_graph(&router, case.case, query).await;
        }
        managed(&router, case.case, REFRESH).await;

        let graph_summary = summarised(&router, case.case, Some("DETAILED")).await;
        for (member, expected) in [
            ("numNodes", case.num_nodes),
            ("numEdges", case.num_edges),
            (
                "totalNodePropertyValues",
                case.total_node_property_values,
            ),
            (
                "totalEdgePropertyValues",
                case.total_edge_property_values,
            ),
        ] {
            assert_eq!(
                graph_summary[member].as_i64(),
                Some(expected),
                "case {}: expected {member}={expected}, got {graph_summary:?}",
                case.case
            );
        }

        for (member, counter, json_text) in [
            ("nodeLabels", "numNodeLabels", case.node_labels),
            ("edgeLabels", "numEdgeLabels", case.edge_labels),
            ("nodeProperties", "numNodeProperties", case.node_properties),
            ("edgeProperties", "numEdgeProperties", case.edge_properties),
        ] {
            let expected = stated(case.case, member, json_text);
            assert_eq!(
                graph_summary[member], expected,
                "case {}: expected {member}={expected:?}, got {:?}",
                case.case, graph_summary[member]
            );
            // The `num…` member is how many distinct entries the list holds, so it is
            // asserted against the list rather than restated.
            let entries = expected.as_array().map(|entries| entries.len() as i64);
            assert_eq!(
                graph_summary[counter].as_i64(),
                entries,
                "case {}: {counter} must count {member}, got {graph_summary:?}",
                case.case
            );
        }

        for (member, json_text) in [
            ("nodeStructures", case.node_structures),
            ("edgeStructures", case.edge_structures),
        ] {
            let expected = stated(case.case, member, json_text);
            assert_eq!(
                graph_summary[member], expected,
                "case {}: expected {member}={expected:?}, got {:?}",
                case.case, graph_summary[member]
            );
        }
    }
}

#[tokio::test]
async fn a_summary_is_the_snapshot_of_the_run_that_computed_it() {
    let router = emulator();

    // Before any run there is no computed summary, in either mode — and a read must not
    // start one.
    for mode in [None, Some("DETAILED")] {
        let case = format!("before any run, mode {mode:?}");
        assert_not_available(&case, &summary(&router, mode).await);
    }

    write_graph(
        &router,
        "first run",
        "CREATE (:Person {name: 'Alice'})-[:KNOWS]->(:Person {name: 'Bob'})",
    )
    .await;
    managed(&router, "first run", REFRESH).await;

    // Two reads of one run answer the same summary: a read reports the state and changes
    // nothing.
    let first = summary(&router, Some("DETAILED")).await;
    let again = summary(&router, Some("DETAILED")).await;
    assert_eq!(
        first.body, again.body,
        "case first run: a read must not change what a later read sees, got {:?} then {:?}",
        first.body, again.body
    );

    // The graph grows after the run, and the run's summary does not move with it.
    write_graph(
        &router,
        "after a later write",
        "CREATE (:City {population: 5})",
    )
    .await;
    let unchanged = summarised(&router, "after a later write", Some("DETAILED")).await;
    assert_eq!(
        unchanged, first.payload()["graphSummary"],
        "case after a later write: a recorded run keeps the summary it counted, got {unchanged:?}"
    );

    // The next run is what counts the graph as it then stands.
    managed(&router, "second run", REFRESH).await;
    let recounted = summarised(&router, "second run", Some("DETAILED")).await;
    assert_eq!(
        recounted["numNodes"].as_i64(),
        Some(3),
        "case second run: the later run must count the graph as it then stood, got {recounted:?}"
    );
    assert_eq!(
        recounted["nodeLabels"],
        json!(["City", "Person"]),
        "case second run: the later run must list the labels it then found, got {recounted:?}"
    );

    // And a delete leaves the summary endpoint nothing to answer from.
    delete(&router).await;
    assert_not_available(
        "after a delete",
        &summary(&router, Some("DETAILED")).await,
    );
}

#[tokio::test]
async fn a_summary_reports_the_vertices_a_gremlin_traversal_added() {
    let router = emulator();

    // Two vertices carrying one fresh label, written through the Gremlin endpoint, which is
    // the other writer of the one stored graph.
    let label = "widget_7";
    for round in 1..=2u32 {
        let answer = call(
            &router,
            "POST",
            "/gremlin",
            Some(json!({ "gremlin": format!("g.addV('{label}')") })),
        )
        .await;
        assert_eq!(
            answer.status, 200,
            "case addV #{round}: the traversal must be answered, got {} with body {:?}",
            answer.status, answer.body
        );
    }
    managed(&router, "gremlin vertices", REFRESH).await;

    let graph_summary = summarised(&router, "gremlin vertices", Some("DETAILED")).await;
    assert_eq!(
        graph_summary["numNodes"].as_i64(),
        Some(2),
        "case gremlin vertices: both added vertices must be counted, got {graph_summary:?}"
    );
    assert_eq!(
        graph_summary["nodeLabels"],
        json!([label]),
        "case gremlin vertices: the label the traversal wrote must be listed, got {graph_summary:?}"
    );
    assert_eq!(
        graph_summary["numNodeLabels"].as_i64(),
        Some(1),
        "case gremlin vertices: one distinct label was written, got {graph_summary:?}"
    );
    assert_eq!(
        graph_summary["nodeStructures"],
        json!([{ "count": 2, "nodeProperties": [], "distinctOutgoingEdgeLabels": [] }]),
        "case gremlin vertices: both vertices have the one empty structure, got {graph_summary:?}"
    );
}
