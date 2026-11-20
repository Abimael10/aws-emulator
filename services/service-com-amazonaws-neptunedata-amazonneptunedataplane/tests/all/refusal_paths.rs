// SPDX-License-Identifier: CC0-1.0

//! The neptunedata data plane's REFUSAL paths, end to end over HTTP.
//!
//! Three rules are under test, each over a SET of inputs rather than one recorded request:
//!
//! 1. Every modelled refusal this service answers carries its text in the member a client
//!    reads as the error message (`message`) as well as in the modelled `detailedMessage`,
//!    names the error CLASS — the exception's own name, never the numeric HTTP status — in
//!    `code`, and names its own request in `requestId`.
//! 2. A `queryId` no registry holds is refused `InvalidParameterException` (HTTP 400) by
//!    BOTH query languages' status and cancel endpoints, with the supplied id in the
//!    message, and the two languages keep separate registries.
//! 3. A query neither parser can read is refused `MalformedQueryException` (HTTP 400) with
//!    that parser's own message — where the parse stopped, in the submitted query's own
//!    coordinates — rather than answered, and a machine-learning id that names no resource
//!    this engine holds is refused `MLResourceNotFoundException` (HTTP 404), which is also
//!    what a read of the endpoint such a refused create did NOT make answers.
//!
//! Every case goes through the router, so it exercises routing, deserialization, the
//! handler, the persisted state and serialization — the same path a client takes.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_neptunedata_amazonneptunedataplane::state::{
    ComAmazonawsNeptunedataAmazonneptunedataplaneState, MlJobRecord, MAX_ML_ENDPOINTS,
    ML_ENDPOINT_PROVISIONING, ML_ENDPOINT_STATUS_CREATING, ML_ENDPOINT_STATUS_IN_SERVICE,
    ML_JOB_STATUS_COMPLETED, ML_RESOURCE_NOT_FOUND_MESSAGE,
};
use service_com_amazonaws_neptunedata_amazonneptunedataplane::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// A SigV4 credential scope naming this service. `neptune-db` is neptunedata's sigv4
/// signing name, and it is what routes a request to this service.
const SIGNED_FOR_NEPTUNE_DB: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260101/us-east-1/neptune-db/aws4_request, \
     SignedHeaders=host, Signature=abc";

/// The region the credential scope above names, which is the region every ARN this
/// emulator mints for that request carries.
const SIGNED_REGION: &str = "us-east-1";

/// How long a poll for a clock-owed transition waits before giving up: many times over the
/// span the transition is due in, so a loaded or slow host still reaches it. Only a
/// transition that never falls due spends it; one that does ends the poll as soon as it is
/// seen.
const POLL_DEADLINE: Duration = Duration::from_secs(180);

/// How long one poll waits before looking again. Short next to [`POLL_DEADLINE`], so the
/// test ends as soon as the condition holds rather than after a fixed wait.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// One emulator: a router carrying only this service, over one shared engine. The engine is
/// handed back so a test can seed and read the registries the handlers read and write.
struct Emulator {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
}

fn emulator() -> Emulator {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(Arc::clone(&engine)));
    Emulator { router, engine }
}

/// What the emulator answered: the HTTP status and the body, as JSON when it is JSON.
struct Answer {
    status: u16,
    body: Value,
    text: String,
}

impl Answer {
    /// The wire error code (`__type`) without its shape namespace, or the empty string when
    /// the body is not a modelled error.
    fn error_code(&self) -> &str {
        self.body["__type"]
            .as_str()
            .unwrap_or_default()
            .rsplit('#')
            .next()
            .unwrap_or_default()
    }

    /// The modelled `detailedMessage` member.
    fn detailed_message(&self) -> &str {
        self.body["detailedMessage"].as_str().unwrap_or_default()
    }

    /// The member a client reads as the error's message.
    fn message(&self) -> &str {
        self.body["message"].as_str().unwrap_or_default()
    }
}

async fn call(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> Answer {
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
    let bytes = response.into_body().to_vec();
    Answer {
        status,
        body: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        text: String::from_utf8_lossy(&bytes).into_owned(),
    }
}

// ---------------------------------------------------------------------------------------
// Rule 1: a refusal's text reaches the member a client reads it from.
// ---------------------------------------------------------------------------------------

/// One request this service refuses, and the refusal it must answer with. The rows differ
/// along WHICH operation refuses and why, so the rule is tested across the service's
/// refusal paths rather than on one of them.
#[derive(Debug, Clone, Copy)]
struct Refusal {
    case: &'static str,
    method: &'static str,
    uri: &'static str,
    body: Option<&'static str>,
    status: u16,
    code: &'static str,
}

const REFUSALS: &[Refusal] = &[
    Refusal {
        case: "a Gremlin query status for an id no query minted",
        method: "GET",
        uri: "/gremlin/status/not-a-query-id",
        body: None,
        status: 400,
        code: "InvalidParameterException",
    },
    Refusal {
        case: "a Gremlin cancel for an id no query minted",
        method: "DELETE",
        uri: "/gremlin/status/not-a-query-id",
        body: None,
        status: 400,
        code: "InvalidParameterException",
    },
    Refusal {
        case: "an openCypher query status for an id no query minted",
        method: "GET",
        uri: "/opencypher/status/not-a-query-id",
        body: None,
        status: 400,
        code: "InvalidParameterException",
    },
    Refusal {
        case: "an openCypher cancel for an id no query minted",
        method: "DELETE",
        uri: "/opencypher/status/not-a-query-id",
        body: None,
        status: 400,
        code: "InvalidParameterException",
    },
    Refusal {
        case: "a Gremlin query that is not a traversal",
        method: "POST",
        uri: "/gremlin",
        body: Some(r#"{"gremlin":"g.V(("}"#),
        status: 400,
        code: "MalformedQueryException",
    },
    Refusal {
        case: "an openCypher query the parser cannot read",
        method: "POST",
        uri: "/opencypher",
        body: Some(r#"{"query":"MATCH (n:Person RETURN n"}"#),
        status: 400,
        code: "MalformedQueryException",
    },
    Refusal {
        case: "an inference endpoint for a model-training job this engine does not hold",
        method: "POST",
        uri: "/ml/endpoints",
        body: Some(r#"{"id":"endpoint-1","mlModelTrainingJobId":"job-nobody-started"}"#),
        status: 404,
        code: "MLResourceNotFoundException",
    },
    Refusal {
        case: "a read of an inference endpoint this engine does not hold",
        method: "GET",
        uri: "/ml/endpoints/endpoint-nobody-created",
        body: None,
        status: 404,
        code: "MLResourceNotFoundException",
    },
    Refusal {
        case: "a fast reset presenting a token no initiate minted",
        method: "POST",
        uri: "/system",
        body: Some(r#"{"action":"performDatabaseReset","token":"not-the-token"}"#),
        status: 400,
        code: "InvalidParameterException",
    },
    Refusal {
        case: "a Gremlin explain with no query to explain",
        method: "POST",
        uri: "/gremlin/explain",
        body: Some(r#"{"gremlin":"   "}"#),
        status: 400,
        code: "MissingParameterException",
    },
    Refusal {
        case: "an openCypher query whose parameters cannot bind anything",
        method: "POST",
        uri: "/opencypher",
        body: Some(r#"{"query":"RETURN $name AS name","parameters":"name=Alice"}"#),
        status: 400,
        code: "InvalidParameterException",
    },
    Refusal {
        case: "an openCypher query carrying no query to run",
        method: "POST",
        uri: "/opencypher",
        body: Some(r#"{"query":"   "}"#),
        status: 400,
        code: "MissingParameterException",
    },
];

/// The body of `refusal`, as the case states it.
fn case_body(refusal: &Refusal) -> Option<Value> {
    refusal
        .body
        .map(|payload| serde_json::from_str::<Value>(payload).expect("the case body is JSON"))
}

#[tokio::test]
async fn every_refusal_carries_its_text_in_the_member_a_client_reads() {
    let Emulator { router, .. } = emulator();
    for refusal in REFUSALS {
        let answer = call(&router, refusal.method, refusal.uri, case_body(refusal)).await;
        assert_eq!(
            answer.status, refusal.status,
            "case {:?}: expected HTTP {}, got {} with {:?}",
            refusal.case, refusal.status, answer.status, answer.text
        );
        assert_eq!(
            answer.error_code(),
            refusal.code,
            "case {:?}: expected {}, got {:?}",
            refusal.case,
            refusal.code,
            answer.text
        );
        assert!(
            !answer.message().is_empty(),
            "case {:?}: the refusal must carry a client-visible message, got {:?}",
            refusal.case,
            answer.text
        );
        assert_eq!(
            answer.message(),
            answer.detailed_message(),
            "case {:?}: one refusal carries one text, got {:?}",
            refusal.case,
            answer.text
        );
    }
}

/// The members a modelled neptunedata refusal body carries. The first three are the
/// members every exception shape of this service declares — source:
/// smithy_query(query="shape", shape="MalformedQueryException,InvalidParameterException,
/// MissingParameterException,MLResourceNotFoundException"), whose member set is
/// `detailedMessage`, `requestId` and `code` — and `message` is the fourth the data plane
/// writes the same text under.
const REFUSAL_BODY_MEMBERS: &[&str] = &["detailedMessage", "requestId", "code", "message"];

#[tokio::test]
async fn every_refusal_body_names_its_exception_class_and_its_own_request() {
    let Emulator { router, .. } = emulator();
    let mut request_ids: Vec<String> = Vec::new();
    for refusal in REFUSALS {
        let answer = call(&router, refusal.method, refusal.uri, case_body(refusal)).await;
        for member in REFUSAL_BODY_MEMBERS {
            let carried = answer.body[*member].as_str().unwrap_or_default();
            assert!(
                !carried.is_empty(),
                "case {:?}: the refusal body carries {member} as a non-empty string, got {:?}",
                refusal.case,
                answer.text
            );
        }
        // `code` is the error CLASS a client branches on — the exception's own name, which
        // is also what the `__type` header-and-body classification names — and never the
        // numeric HTTP status, which the status line already carries.
        assert_eq!(
            answer.body["code"].as_str(),
            Some(refusal.code),
            "case {:?}: code carries the exception class {}, got {:?}",
            refusal.case,
            refusal.code,
            answer.body["code"]
        );
        assert_eq!(
            answer.body["code"].as_str(),
            Some(answer.error_code()),
            "case {:?}: code and the wire classification name one class, got {:?}",
            refusal.case,
            answer.text
        );
        request_ids.push(
            answer.body["requestId"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
        );
    }

    // Two refusals never name the same request: `requestId` identifies ONE answer.
    let minted = request_ids.len();
    request_ids.sort();
    request_ids.dedup();
    assert_eq!(
        request_ids.len(),
        minted,
        "each refusal names its own request, {minted} refusals carry {} ids",
        request_ids.len()
    );
}

// ---------------------------------------------------------------------------------------
// Rule 2: an id no registry holds is refused, by both languages, naming the id.
// ---------------------------------------------------------------------------------------

/// The query ids the status family is asked about. The rows differ along the FORM of the
/// id: a word that is no id at all, a well-formed id nothing minted, a number, and a long
/// one — none of them is in a registry, so each must be refused the same way.
const UNKNOWN_QUERY_IDS: &[&str] = &[
    "totally-invalid-query-id-not-a-uuid",
    "00000000-0000-0000-0000-000000000000",
    "42",
    "a-very-long-query-id-that-no-engine-would-ever-have-minted-for-a-query-0123456789",
];

/// The status and cancel endpoints of both query languages, by method and path prefix.
/// Source: smithy_query(query="operation", operation="GetGremlinQueryStatus,
/// CancelGremlinQuery,GetOpenCypherQueryStatus,CancelOpenCypherQuery") — each one's http
/// binding.
const QUERY_ID_ENDPOINTS: &[(&str, &str, &str)] = &[
    ("GetGremlinQueryStatus", "GET", "/gremlin/status"),
    ("CancelGremlinQuery", "DELETE", "/gremlin/status"),
    ("GetOpenCypherQueryStatus", "GET", "/opencypher/status"),
    ("CancelOpenCypherQuery", "DELETE", "/opencypher/status"),
];

#[tokio::test]
async fn an_unknown_query_id_is_refused_by_both_languages_naming_the_id() {
    let Emulator { router, .. } = emulator();
    for (operation, method, prefix) in QUERY_ID_ENDPOINTS {
        for query_id in UNKNOWN_QUERY_IDS {
            let case = format!("{operation} on {query_id:?}");
            let answer = call(&router, method, &format!("{prefix}/{query_id}"), None).await;
            assert_eq!(
                answer.status, 400,
                "case {case:?}: expected HTTP 400, got {} with {:?}",
                answer.status, answer.text
            );
            assert_eq!(
                answer.error_code(),
                "InvalidParameterException",
                "case {case:?}: expected InvalidParameterException, got {:?}",
                answer.text
            );
            let expected = format!("Supplied queryId {query_id} is invalid");
            assert_eq!(
                answer.message(),
                expected,
                "case {case:?}: expected message {expected:?}, got {:?}",
                answer.message()
            );
        }
    }
}

#[tokio::test]
async fn each_language_has_its_own_query_registry() {
    let Emulator { router, engine } = emulator();

    // A Gremlin explain is answered within the call, so the query it read is in the
    // Gremlin registry — and in that one only.
    let explained = call(
        &router,
        "POST",
        "/gremlin/explain",
        Some(json!({ "gremlin": "g.V().has('code','ANC')" })),
    )
    .await;
    assert_eq!(
        explained.status, 200,
        "an explain of a traversal must be answered, got {:?}",
        explained.text
    );

    // An openCypher query is evaluated within the call, so its query is in the openCypher
    // registry, under an id of its own.
    let answered = call(
        &router,
        "POST",
        "/opencypher",
        Some(json!({ "query": "RETURN 1 AS one" })),
    )
    .await;
    assert_eq!(
        answered.status, 200,
        "an openCypher query must be answered, got {:?}",
        answered.text
    );

    let store = engine.state::<ComAmazonawsNeptunedataAmazonneptunedataplaneState>();
    let (gremlin_id, gremlin_query, open_cypher_id, open_cypher_query) = {
        let held = store.lock();
        let gremlin = held
            .gremlin_queries
            .values()
            .next()
            .expect("the explain recorded the query it read");
        let open_cypher = held
            .open_cypher_queries
            .values()
            .next()
            .expect("the openCypher endpoint recorded the query it ran");
        (
            gremlin.query_id.clone(),
            gremlin.query_string.clone(),
            open_cypher.query_id.clone(),
            open_cypher.query_string.clone(),
        )
    };
    assert_ne!(
        gremlin_id, open_cypher_id,
        "two queries the engine accepted carry two ids"
    );

    // Each id is reported by ITS language's status, with the query string as submitted.
    for (case, prefix, query_id, query_string) in [
        (
            "the Gremlin registry",
            "/gremlin/status",
            &gremlin_id,
            &gremlin_query,
        ),
        (
            "the openCypher registry",
            "/opencypher/status",
            &open_cypher_id,
            &open_cypher_query,
        ),
    ] {
        let status = call(&router, "GET", &format!("{prefix}/{query_id}"), None).await;
        assert_eq!(
            status.status, 200,
            "case {case:?}: expected the status of {query_id}, got {:?}",
            status.text
        );
        assert_eq!(
            status.body["queryString"].as_str(),
            Some(query_string.as_str()),
            "case {case:?}: the status reports the query as submitted, got {:?}",
            status.text
        );
        assert_eq!(
            status.body["queryEvalStats"]["cancelled"].as_bool(),
            Some(false),
            "case {case:?}: a query that was answered was not cancelled, got {:?}",
            status.text
        );
    }

    // And by the OTHER language's status it is an id nothing minted.
    for (case, prefix, query_id) in [
        (
            "a Gremlin id is not an openCypher one",
            "/opencypher/status",
            &gremlin_id,
        ),
        (
            "an openCypher id is not a Gremlin one",
            "/gremlin/status",
            &open_cypher_id,
        ),
    ] {
        let crossed = call(&router, "GET", &format!("{prefix}/{query_id}"), None).await;
        assert_eq!(
            crossed.error_code(),
            "InvalidParameterException",
            "case {case:?}: expected InvalidParameterException, got {:?}",
            crossed.text
        );
    }

    // The openCypher list is served from the same registry: the query above has finished,
    // so the engine owes no answer for it and it is not one of the current queries.
    let listed = call(&router, "GET", "/opencypher/status", None).await;
    assert_eq!(
        listed.status, 200,
        "the openCypher query list must be served, got {:?}",
        listed.text
    );
    assert_eq!(
        listed.body["acceptedQueryCount"].as_i64(),
        Some(0),
        "a finished query is not one the engine still owes an answer, got {:?}",
        listed.text
    );
    assert!(
        listed.body["queries"]
            .as_array()
            .is_none_or(|queries| queries.is_empty()),
        "a finished query is not one of the current queries, got {:?}",
        listed.text
    );

    // A cancel of a query the openCypher registry holds is accepted, and the status it
    // reports afterwards says so.
    let cancelled = call(
        &router,
        "DELETE",
        &format!("/opencypher/status/{open_cypher_id}"),
        None,
    )
    .await;
    assert_eq!(
        cancelled.status, 200,
        "a cancel of a query the registry holds must be accepted, got {:?}",
        cancelled.text
    );
}

// ---------------------------------------------------------------------------------------
// Rule 3a: a Gremlin query that is not a traversal is refused, with the parser's message.
// ---------------------------------------------------------------------------------------

/// One query no Gremlin parser can read, and where the parse stops in it. `position` is the
/// 0-based character position in `line` that the parser gave up at, and `input` the query
/// text up to and including that character — both as the Gremlin endpoint's own message
/// reports them.
#[derive(Debug, Clone, Copy)]
struct GremlinRefusal {
    case: &'static str,
    query: &'static str,
    line: usize,
    position: usize,
    input: &'static str,
}

const GREMLIN_REFUSALS: &[GremlinRefusal] = &[
    GremlinRefusal {
        case: "an argument list opened where an argument belongs",
        query: "g.addV('label').property((((",
        line: 1,
        position: 25,
        input: "g.addV('label').property((",
    },
    GremlinRefusal {
        case: "two arguments with no comma between them",
        query: "g.V().has('code' 'ANC')",
        line: 1,
        position: 17,
        input: "g.V().has('code' '",
    },
    GremlinRefusal {
        case: "a query that starts from another traversal source",
        query: "h.V().count()",
        line: 1,
        position: 0,
        input: "h",
    },
    GremlinRefusal {
        case: "two dots between steps",
        query: "g.V()..count()",
        line: 1,
        position: 6,
        input: "g.V()..",
    },
    GremlinRefusal {
        case: "an argument list that is never closed",
        query: "g.V(",
        line: 1,
        position: 4,
        input: "g.V(",
    },
    GremlinRefusal {
        case: "a parse that stops on the second line of the query",
        query: "g.V()\n.has('code' 'ANC')",
        line: 2,
        position: 12,
        input: "g.V()\\n.has('code' '",
    },
];

/// The two Gremlin endpoints that READ a submitted traversal, by path. Both read the query
/// before deciding anything, so a query that is not a traversal is refused the same way by
/// each.
const GREMLIN_ENDPOINTS: &[(&str, &str)] = &[
    ("ExecuteGremlinQuery", "/gremlin"),
    ("ExecuteGremlinExplainQuery", "/gremlin/explain"),
];

#[tokio::test]
async fn a_gremlin_query_that_is_not_a_traversal_is_refused_with_the_parsers_message() {
    let Emulator { router, engine } = emulator();
    for (operation, uri) in GREMLIN_ENDPOINTS {
        for refusal in GREMLIN_REFUSALS {
            let case = format!("{operation}: {}", refusal.case);
            let answer = call(
                &router,
                "POST",
                uri,
                Some(json!({ "gremlin": refusal.query })),
            )
            .await;
            assert_eq!(
                answer.status, 400,
                "case {case:?}: expected HTTP 400, got {} with {:?}",
                answer.status, answer.text
            );
            assert_eq!(
                answer.error_code(),
                "MalformedQueryException",
                "case {case:?}: expected MalformedQueryException, got {:?}",
                answer.text
            );
            let expected = format!(
                "Failed to interpret Gremlin query: Query parsing failed at line {}, \
                 character position at {}, error message : no viable alternative at input '{}'",
                refusal.line, refusal.position, refusal.input
            );
            assert_eq!(
                answer.message(),
                expected,
                "case {case:?}: expected message {expected:?}, got {:?}",
                answer.message()
            );
        }
    }

    // A query that was refused is not a query the engine accepted, so neither registry
    // holds anything for it.
    let store = engine.state::<ComAmazonawsNeptunedataAmazonneptunedataplaneState>();
    let recorded = store.lock().gremlin_queries.len();
    assert_eq!(
        recorded, 0,
        "a refused query must leave the registry empty, it holds {recorded}"
    );
}

#[tokio::test]
async fn a_traversal_this_engine_cannot_evaluate_is_not_answered_with_a_result() {
    let Emulator { router, engine } = emulator();
    // A traversal the parser READS is not refused as malformed — there is no engine here
    // that can evaluate one, so the call is answered as not implemented rather than with a
    // result nothing computed, and nothing is recorded as a query the engine accepted.
    for query in [
        "g.V().has('code','ANC')",
        "g.addV('airport').property('code','ANC')",
        "g.V().count()",
    ] {
        let answer = call(&router, "POST", "/gremlin", Some(json!({ "gremlin": query }))).await;
        assert_eq!(
            answer.status, 501,
            "case {query:?}: a traversal nothing evaluated must not be answered with a \
             result, got {} with {:?}",
            answer.status, answer.text
        );
        assert_eq!(
            answer.error_code(),
            "NotImplemented",
            "case {query:?}: expected NotImplemented, got {:?}",
            answer.text
        );
    }
    let store = engine.state::<ComAmazonawsNeptunedataAmazonneptunedataplaneState>();
    let recorded = store.lock().gremlin_queries.len();
    assert_eq!(
        recorded, 0,
        "a query nothing evaluated is not one the engine accepted, the registry holds \
         {recorded}"
    );
}

// ---------------------------------------------------------------------------------------
// Rule 3b: an openCypher query the parser cannot read is refused, with its message.
// ---------------------------------------------------------------------------------------

/// One query the openCypher parser cannot read, and the refusal it must get: the input the
/// parser could not use, what it expected there, and where it stopped — line and column
/// counting from 1, offset from 0, as the openCypher endpoint's own message reports them.
#[derive(Debug, Clone, Copy)]
struct OpenCypherRefusal {
    case: &'static str,
    query: &'static str,
    input: &'static str,
    expected: &'static str,
    line: usize,
    column: usize,
    offset: usize,
}

const OPEN_CYPHER_REFUSALS: &[OpenCypherRefusal] = &[
    OpenCypherRefusal {
        case: "a clause keyword one letter short",
        query: "CREAT (n:`Label`) SET n.x = ",
        input: " ",
        expected: "e/E",
        line: 1,
        column: 6,
        offset: 5,
    },
    OpenCypherRefusal {
        case: "another clause keyword one letter short",
        query: "MATC (n) RETURN n",
        input: " ",
        expected: "h/H",
        line: 1,
        column: 5,
        offset: 4,
    },
    OpenCypherRefusal {
        case: "a keyword one letter short in the middle of the query",
        query: "MATCH (n) RETUR n",
        input: " ",
        expected: "n/N",
        line: 1,
        column: 16,
        offset: 15,
    },
    OpenCypherRefusal {
        case: "a keyword one letter short on the second line",
        query: "MATCH (n)\nRETUR n",
        input: " ",
        expected: "n/N",
        line: 2,
        column: 6,
        offset: 15,
    },
    OpenCypherRefusal {
        case: "a pattern that is never closed",
        query: "MATCH (n:Person RETURN n",
        input: "RETURN",
        expected: ")",
        line: 1,
        column: 17,
        offset: 16,
    },
    OpenCypherRefusal {
        case: "a query that ends where the parser needed more",
        query: "CREATE (n:`Label`",
        input: "EOI",
        expected: ")",
        line: 1,
        column: 18,
        offset: 17,
    },
    OpenCypherRefusal {
        case: "a keyword one letter short that the query then ends after",
        query: "MATCH (n) RETUR",
        input: "EOI",
        expected: "n/N",
        line: 1,
        column: 16,
        offset: 15,
    },
    OpenCypherRefusal {
        case: "a keyword one letter short on the third line",
        query: "MATCH (n)\nWHERE n.x = 1\nRETUR n",
        input: " ",
        expected: "n/N",
        line: 3,
        column: 6,
        offset: 29,
    },
    // The position is counted in CHARACTERS, not bytes: the label below carries a
    // multi-byte character, so a byte count would report one column and one offset too
    // many.
    OpenCypherRefusal {
        case: "a keyword one letter short after multi-byte text",
        query: "MATCH (n:`Café`) RETUR n",
        input: " ",
        expected: "n/N",
        line: 1,
        column: 23,
        offset: 22,
    },
];

#[tokio::test]
async fn an_open_cypher_query_the_parser_cannot_read_is_refused_with_its_message() {
    let Emulator { router, engine } = emulator();
    for refusal in OPEN_CYPHER_REFUSALS {
        let answer = call(
            &router,
            "POST",
            "/opencypher",
            Some(json!({ "query": refusal.query })),
        )
        .await;
        assert_eq!(
            answer.status, 400,
            "case {:?}: expected HTTP 400, got {} with {:?}",
            refusal.case, answer.status, answer.text
        );
        assert_eq!(
            answer.error_code(),
            "MalformedQueryException",
            "case {:?}: expected MalformedQueryException, got {:?}",
            refusal.case,
            answer.text
        );
        let expected = format!(
            "Invalid input '{}': expected '{}' (line {}, column {} (offset: {}))",
            refusal.input, refusal.expected, refusal.line, refusal.column, refusal.offset
        );
        assert_eq!(
            answer.message(),
            expected,
            "case {:?}: expected message {expected:?}, got {:?}",
            refusal.case,
            answer.message()
        );
        // The same text under the modelled member, and the error CLASS in `code`: a client
        // reads the diagnostic from `message` and branches on `code`.
        assert_eq!(
            answer.detailed_message(),
            expected,
            "case {:?}: expected detailedMessage {expected:?}, got {:?}",
            refusal.case,
            answer.detailed_message()
        );
        assert_eq!(
            answer.body["code"].as_str(),
            Some("MalformedQueryException"),
            "case {:?}: code carries the exception class, got {:?}",
            refusal.case,
            answer.body["code"]
        );
    }

    // A query the parser refused is not one the engine ran, so it is not in the registry
    // and it changed nothing in the graph.
    let store = engine.state::<ComAmazonawsNeptunedataAmazonneptunedataplaneState>();
    let recorded = store.lock().open_cypher_queries.len();
    assert_eq!(
        recorded, 0,
        "a refused query must leave the registry empty, it holds {recorded}"
    );
    let read_back = call(
        &router,
        "POST",
        "/opencypher",
        Some(json!({ "query": "MATCH (n) RETURN count(n) AS nodes" })),
    )
    .await;
    assert_eq!(
        read_back.body["results"][0]["nodes"].as_i64(),
        Some(0),
        "a refused query writes nothing, got {:?}",
        read_back.text
    );
}

// ---------------------------------------------------------------------------------------
// Rule 3c: a machine-learning id that names no resource this engine holds is refused.
// ---------------------------------------------------------------------------------------

/// Seed one completed ML job of `kind` into the registry the `StartML*Job` operations write,
/// so an endpoint can be created for a job this engine really holds. `kind` is
/// `"training"` or `"transform"`.
fn seed_completed_job(engine: &engine::InMemoryEngine, kind: &str, job_id: &str) {
    let store = engine.state::<ComAmazonawsNeptunedataAmazonneptunedataplaneState>();
    let mut held = store.lock();
    let record = MlJobRecord {
        id: job_id.to_string(),
        arn: ::engine::emulator_arn(
            "sagemaker",
            SIGNED_REGION,
            &format!("{kind}-job/{job_id}"),
        ),
        created_at: SystemTime::now(),
        status: ML_JOB_STATUS_COMPLETED.to_string(),
        output_location: Some(format!("s3://neptune-ml/{job_id}/")),
    };
    match kind {
        "training" => held.ml.model_training_jobs.insert(job_id.to_string(), record),
        "transform" => held
            .ml
            .model_transform_jobs
            .insert(job_id.to_string(), record),
        other => panic!("case {other:?}: a seeded job is a training or a transform job"),
    };
}

/// One `CreateMLEndpoint` naming no ML resource this engine holds. The rows differ along
/// WHICH member carries the missing id and whether the request names an endpoint id of its
/// own, so the refusal is tested across the ways the create can name nothing.
#[derive(Debug, Clone, Copy)]
struct MissingMlResource {
    case: &'static str,
    body: &'static str,
    /// The endpoint id a later read asks about, which no refused create may have made.
    endpoint_id: &'static str,
}

const MISSING_ML_RESOURCES: &[MissingMlResource] = &[
    MissingMlResource {
        case: "a model-training job nothing started",
        body: r#"{"id":"endpoint-for-missing-training-job",
                  "mlModelTrainingJobId":"training-job-nobody-started"}"#,
        endpoint_id: "endpoint-for-missing-training-job",
    },
    MissingMlResource {
        case: "a model-transform job nothing started",
        body: r#"{"id":"endpoint-for-missing-transform-job",
                  "mlModelTransformJobId":"transform-job-nobody-started"}"#,
        endpoint_id: "endpoint-for-missing-transform-job",
    },
    MissingMlResource {
        case: "an update of an endpoint nothing created",
        body: r#"{"id":"endpoint-nobody-created","update":true,
                  "mlModelTrainingJobId":"seeded-training-job"}"#,
        endpoint_id: "endpoint-nobody-created",
    },
];

#[tokio::test]
async fn an_ml_id_that_names_no_resource_is_refused_and_creates_nothing() {
    let Emulator { router, engine } = emulator();
    // The update case names a job that IS held, so what it is refused for is the endpoint
    // it names rather than the job.
    seed_completed_job(&engine, "training", "seeded-training-job");

    for case in MISSING_ML_RESOURCES {
        let body: Value = serde_json::from_str(case.body).expect("the case body is JSON");
        let created = call(&router, "POST", "/ml/endpoints", Some(body)).await;
        assert_eq!(
            created.status, 404,
            "case {:?}: expected HTTP 404, got {} with {:?}",
            case.case, created.status, created.text
        );
        assert_eq!(
            created.error_code(),
            "MLResourceNotFoundException",
            "case {:?}: expected MLResourceNotFoundException, got {:?}",
            case.case,
            created.text
        );
        assert_eq!(
            created.message(),
            ML_RESOURCE_NOT_FOUND_MESSAGE,
            "case {:?}: expected the ML not-found message, got {:?}",
            case.case,
            created.message()
        );

        // Nothing was left behind: the read of the endpoint the refused create named is
        // refused with the same class.
        let read = call(
            &router,
            "GET",
            &format!("/ml/endpoints/{}", case.endpoint_id),
            None,
        )
        .await;
        assert_eq!(
            read.status, 404,
            "case {:?}: a refused create leaves no endpoint, got {} with {:?}",
            case.case, read.status, read.text
        );
        assert_eq!(
            read.error_code(),
            "MLResourceNotFoundException",
            "case {:?}: expected MLResourceNotFoundException, got {:?}",
            case.case,
            read.text
        );
        assert_eq!(
            read.message(),
            ML_RESOURCE_NOT_FOUND_MESSAGE,
            "case {:?}: expected the ML not-found message, got {:?}",
            case.case,
            read.message()
        );
    }
}

#[tokio::test]
async fn a_create_naming_no_job_at_all_is_refused_as_a_missing_parameter() {
    let Emulator { router, .. } = emulator();
    let created = call(
        &router,
        "POST",
        "/ml/endpoints",
        Some(json!({ "id": "endpoint-without-a-job" })),
    )
    .await;
    assert_eq!(
        created.error_code(),
        "MissingParameterException",
        "an endpoint serves one model, so a create naming no job is missing a parameter, \
         got {:?}",
        created.text
    );
    assert!(
        created.message().contains("mlModelTrainingJobId")
            && created.message().contains("mlModelTransformJobId"),
        "the refusal names both members that can carry the job, got {:?}",
        created.message()
    );
}

/// One endpoint a create makes, over a job this engine holds. The rows differ along which
/// job kind the endpoint serves and whether the request names an id, an instance type and
/// an instance count of its own.
#[derive(Debug, Clone, Copy)]
struct Created {
    case: &'static str,
    kind: &'static str,
    job_id: &'static str,
    body: &'static str,
    /// The id the answer must carry, or `None` when the request named none and the service
    /// has to autogenerate a timestamped one.
    id: Option<&'static str>,
}

const CREATED: &[Created] = &[
    Created {
        case: "an endpoint for a completed model-training job",
        kind: "training",
        job_id: "training-job-1",
        body: r#"{"id":"endpoint-1","mlModelTrainingJobId":"training-job-1"}"#,
        id: Some("endpoint-1"),
    },
    Created {
        case: "an endpoint for a completed model-transform job",
        kind: "transform",
        job_id: "transform-job-1",
        body: r#"{"id":"endpoint-2","mlModelTransformJobId":"transform-job-1",
                  "instanceType":"ml.m5.2xlarge","instanceCount":2}"#,
        id: Some("endpoint-2"),
    },
    Created {
        case: "an endpoint whose id the service has to autogenerate",
        kind: "training",
        job_id: "training-job-2",
        body: r#"{"mlModelTrainingJobId":"training-job-2","modelName":"kge"}"#,
        id: None,
    },
];

#[tokio::test]
async fn an_endpoint_for_a_job_this_engine_holds_is_created_and_read_back() {
    let Emulator { router, engine } = emulator();
    for case in CREATED {
        seed_completed_job(&engine, case.kind, case.job_id);
        let body: Value = serde_json::from_str(case.body).expect("the case body is JSON");
        let created = call(&router, "POST", "/ml/endpoints", Some(body)).await;
        assert_eq!(
            created.status, 200,
            "case {:?}: expected HTTP 200, got {} with {:?}",
            case.case, created.status, created.text
        );
        let endpoint_id = created.body["id"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if let Some(expected) = case.id {
            assert_eq!(
                endpoint_id, expected,
                "case {:?}: the answer carries the id the request named, got {:?}",
                case.case, created.text
            );
        } else {
            assert!(
                !endpoint_id.is_empty(),
                "case {:?}: a create that named no id gets an autogenerated one, got {:?}",
                case.case,
                created.text
            );
        }
        let expected_arn = ::engine::emulator_arn(
            "sagemaker",
            SIGNED_REGION,
            &format!("endpoint/{endpoint_id}"),
        );
        assert_eq!(
            created.body["arn"].as_str(),
            Some(expected_arn.as_str()),
            "case {:?}: the answer carries the endpoint's own ARN, got {:?}",
            case.case,
            created.text
        );
        assert!(
            created.body["creationTimeInMillis"].as_i64().unwrap_or(0) > 0,
            "case {:?}: a created endpoint carries the instant it was created, got {:?}",
            case.case,
            created.text
        );

        // The read reports the endpoint the create made: a provisioned endpoint is not in
        // service the instant it is created, so a read straight afterwards says so.
        let read = call(
            &router,
            "GET",
            &format!("/ml/endpoints/{endpoint_id}"),
            None,
        )
        .await;
        assert_eq!(
            read.status, 200,
            "case {:?}: the created endpoint must be readable, got {} with {:?}",
            case.case, read.status, read.text
        );
        assert_eq!(
            read.body["id"].as_str(),
            Some(endpoint_id.as_str()),
            "case {:?}: the read reports the endpoint that was asked about, got {:?}",
            case.case,
            read.text
        );
        assert_eq!(
            read.body["status"].as_str(),
            Some(ML_ENDPOINT_STATUS_CREATING),
            "case {:?}: an endpoint is provisioned before it serves, got {:?}",
            case.case,
            read.text
        );
        assert_eq!(
            read.body["endpoint"]["arn"].as_str(),
            Some(expected_arn.as_str()),
            "case {:?}: the endpoint definition carries its ARN, got {:?}",
            case.case,
            read.text
        );
        assert_eq!(
            read.body["endpointConfig"]["name"].as_str(),
            Some(endpoint_id.as_str()),
            "case {:?}: the endpoint configuration is the endpoint's own, got {:?}",
            case.case,
            read.text
        );
    }
}

#[tokio::test]
async fn an_endpoint_comes_into_service_a_span_after_it_was_created() {
    let Emulator { router, engine } = emulator();
    seed_completed_job(&engine, "training", "training-job-3");
    let created = call(
        &router,
        "POST",
        "/ml/endpoints",
        Some(json!({
            "id": "endpoint-3",
            "mlModelTrainingJobId": "training-job-3",
        })),
    )
    .await;
    assert_eq!(
        created.status, 200,
        "the endpoint must be created, got {} with {:?}",
        created.status, created.text
    );

    // The clock owes the transition, so the test waits for the CONDITION rather than for a
    // span: it looks again until the endpoint is in service or a generous deadline passes.
    let deadline = ::tokio::time::Instant::now() + POLL_DEADLINE;
    let mut seen = String::new();
    while ::tokio::time::Instant::now() < deadline {
        let read = call(&router, "GET", "/ml/endpoints/endpoint-3", None).await;
        seen = read.body["status"].as_str().unwrap_or_default().to_string();
        if seen == ML_ENDPOINT_STATUS_IN_SERVICE {
            break;
        }
        assert_eq!(
            seen, ML_ENDPOINT_STATUS_CREATING,
            "an endpoint is being created until it is in service, got {:?}",
            read.text
        );
        ::tokio::time::sleep(POLL_INTERVAL).await;
    }
    assert_eq!(
        seen,
        ML_ENDPOINT_STATUS_IN_SERVICE,
        "an endpoint must come into service within {POLL_DEADLINE:?} of the \
         {ML_ENDPOINT_PROVISIONING:?} span the create started, last saw {seen:?}"
    );

    // A create over an endpoint that already holds the id is refused rather than silently
    // replacing it, and an update of it is served.
    let again = call(
        &router,
        "POST",
        "/ml/endpoints",
        Some(json!({
            "id": "endpoint-3",
            "mlModelTrainingJobId": "training-job-3",
        })),
    )
    .await;
    assert_eq!(
        again.error_code(),
        "PreconditionsFailedException",
        "a create over an existing endpoint is refused, got {:?}",
        again.text
    );
    let updated = call(
        &router,
        "POST",
        "/ml/endpoints",
        Some(json!({
            "id": "endpoint-3",
            "update": true,
            "mlModelTrainingJobId": "training-job-3",
        })),
    )
    .await;
    assert_eq!(
        updated.status, 200,
        "an update of an endpoint that is there must be served, got {} with {:?}",
        updated.status, updated.text
    );
}

#[tokio::test]
async fn the_endpoint_registry_is_bounded() {
    let Emulator { router, engine } = emulator();
    seed_completed_job(&engine, "training", "training-job-4");
    // Every create writes to the registry, so the registry has a ceiling: the create that
    // would pass it is refused rather than growing this engine without end.
    for index in 0..MAX_ML_ENDPOINTS {
        let created = call(
            &router,
            "POST",
            "/ml/endpoints",
            Some(json!({
                "id": format!("endpoint-{index}"),
                "mlModelTrainingJobId": "training-job-4",
            })),
        )
        .await;
        assert_eq!(
            created.status, 200,
            "case {index}: a create within the bound must be served, got {} with {:?}",
            created.status, created.text
        );
    }
    let refused = call(
        &router,
        "POST",
        "/ml/endpoints",
        Some(json!({
            "id": "one-endpoint-too-many",
            "mlModelTrainingJobId": "training-job-4",
        })),
    )
    .await;
    assert_eq!(
        refused.error_code(),
        "ConstraintViolationException",
        "the create past the bound must be refused, got {:?}",
        refused.text
    );
    assert!(
        refused.message().contains(&MAX_ML_ENDPOINTS.to_string()),
        "the refusal names the bound the request met, got {:?}",
        refused.message()
    );
    // An update of an endpoint the registry already holds does not grow it, so it is still
    // served at the bound.
    let updated = call(
        &router,
        "POST",
        "/ml/endpoints",
        Some(json!({
            "id": "endpoint-0",
            "update": true,
            "mlModelTrainingJobId": "training-job-4",
        })),
    )
    .await;
    assert_eq!(
        updated.status, 200,
        "an update at the bound must be served, got {} with {:?}",
        updated.status, updated.text
    );
}

#[tokio::test]
async fn an_endpoint_for_a_job_that_has_not_finished_is_refused() {
    let Emulator { router, engine } = emulator();
    // A job that is still running has produced no model, so there is nothing for an
    // endpoint to serve: the state the job is in is seeded here because only the
    // `StartMLModelTrainingJob` operation mints one, and it is not implemented yet.
    let store = engine.state::<ComAmazonawsNeptunedataAmazonneptunedataplaneState>();
    store.lock().ml.model_training_jobs.insert(
        "running-job".to_string(),
        MlJobRecord {
            id: "running-job".to_string(),
            arn: ::engine::emulator_arn("sagemaker", SIGNED_REGION, "training-job/running-job"),
            created_at: SystemTime::now(),
            status: "InProgress".to_string(),
            output_location: None,
        },
    );

    let created = call(
        &router,
        "POST",
        "/ml/endpoints",
        Some(json!({
            "id": "endpoint-for-a-running-job",
            "mlModelTrainingJobId": "running-job",
        })),
    )
    .await;
    assert_eq!(
        created.error_code(),
        "PreconditionsFailedException",
        "an endpoint serves the model of a completed job, got {:?}",
        created.text
    );
    assert!(
        created.message().contains("running-job"),
        "the refusal names the job that is not ready, got {:?}",
        created.message()
    );
    let read = call(
        &router,
        "GET",
        "/ml/endpoints/endpoint-for-a-running-job",
        None,
    )
    .await;
    assert_eq!(
        read.status, 404,
        "a refused create leaves no endpoint, got {} with {:?}",
        read.status, read.text
    );
}
