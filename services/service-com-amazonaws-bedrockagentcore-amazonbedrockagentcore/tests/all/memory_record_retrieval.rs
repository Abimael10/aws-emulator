// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for `RetrieveMemoryRecords`, the RELEVANCE search over an AgentCore
//! Memory container's long-term memory records.
//!
//! The rules under test:
//!
//! * a search reports the container's stored records that the request's scope selects and
//!   whose own content is about the query — the share of the query's distinct terms a
//!   record's content carries is its `score`, the most relevant record first, and a record
//!   carrying none of those terms is no match for the query and is not reported;
//! * the scope is the same narrowing a plain listing offers — the exact namespace, the
//!   namespace hierarchy, the strategy association and the metadata expressions — every
//!   filter narrowing together, with the strategy and the metadata read off
//!   `searchCriteria`;
//! * `searchCriteria.topK` bounds how many of the top-scoring records the search reports,
//!   and the response pages over that ranking in pages of `maxResults`;
//! * each reported summary is what was stored (the same record `GetMemoryRecord` reports)
//!   plus the relevance this search ranked it by;
//! * a search is scoped to the container it addresses: a container holding no matching
//!   record is an empty page, while a `memoryId` naming no container at all is refused at
//!   the container level;
//! * a request the search cannot answer as asked — a `nextToken` it never issued, a
//!   metadata expression with no value to compare against — is refused
//!   `ValidationException` (400).
//!
//! Every case drives real HTTP through the same `MultiServiceRouter` the emulator serves
//! with, so the wire shape (routes, status codes, `restJson1` bodies, modelled error codes)
//! is exercised, not just the handler functions.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;
use std::sync::Arc;

use service_com_amazonaws_bedrockagentcore_amazonbedrockagentcore as agentcore;

/// A memory id matching `MemoryId`'s pattern
/// `^(arn:...:memory/)?[a-zA-Z][a-zA-Z0-9-_]{0,99}-[a-zA-Z0-9]{10}$`.
const MEMORY: &str = "search-memory-0123456789";
/// The same container addressed by its ARN spelling, which that same pattern admits.
const MEMORY_ARN: &str =
    "arn:aws:bedrock-agentcore:us-east-1:123456789012:memory/search-memory-0123456789";
/// A container this suite never registers.
const OTHER_MEMORY: &str = "absent-memory-9876543210";

/// How close a reported `score` has to be to the share the rule computes.
const SCORE_TOLERANCE: f64 = 1e-9;

/// The emulator, with request authentication off so a test signs nothing, and the memory
/// container this suite searches already created through the shared inventory the CONTROL
/// plane's `CreateMemory` writes.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    super::harness::create_memory(&engine, MEMORY);
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(agentcore::EmulatedService::new(engine));
    router
}

/// Send one request and return its status and parsed JSON body (`Value::Null` for an empty
/// body). The SigV4 credential scope is what routes the request to this service.
async fn send(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let payload = body.map_or_else(bytes::Bytes::new, |v| bytes::Bytes::from(v.to_string()));
    let request = http::Request::builder()
        .method(method)
        .uri(format!(
            "https://bedrock-agentcore.us-east-1.amazonaws.com{path}"
        ))
        .header(
            "authorization",
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/us-east-1/bedrock-agentcore/aws4_request, \
             SignedHeaders=host, Signature=0",
        )
        .header("content-type", "application/json")
        .body(payload)
        .expect("build the request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, parsed)
}

/// `BatchCreateMemoryRecords`: `memoryId` in the path, the records in the body.
async fn batch_create(router: &MultiServiceRouter, memory: &str, body: Value) -> (u16, Value) {
    send(
        router,
        "POST",
        &format!(
            "/memories/{}/memoryRecords/batchCreate",
            super::harness::label(memory)
        ),
        Some(body),
    )
    .await
}

/// `RetrieveMemoryRecords`: `memoryId` in the path, the search criteria in the body.
async fn retrieve(router: &MultiServiceRouter, memory: &str, body: Value) -> (u16, Value) {
    send(
        router,
        "POST",
        &format!("/memories/{}/retrieve", super::harness::label(memory)),
        Some(body),
    )
    .await
}

/// `GetMemoryRecord`, so a reported summary can be compared with the stored record itself.
async fn get_record(router: &MultiServiceRouter, memory: &str, record: &str) -> (u16, Value) {
    send(
        router,
        "GET",
        &format!(
            "/memories/{}/memoryRecord/{}",
            super::harness::label(memory),
            super::harness::label(record)
        ),
        None,
    )
    .await
}

fn error_code(body: &Value) -> &str {
    body.get("__type").and_then(Value::as_str).unwrap_or("")
}

/// One `MemoryMetadataFilterExpression`: the metadata key, the relationship, and the value
/// to compare against (absent for the two operators that take none).
fn metadata_filter(key: &str, operator: &str, value: Option<Value>) -> Value {
    let mut expression = json!({"left": {"metadataKey": key}, "operator": operator});
    if let Some(value) = value {
        expression["right"] = json!({"metadataValue": value});
    }
    expression
}

/// A search body: the required `searchCriteria.searchQuery`, plus whatever else the case
/// states (`namespace`, `namespacePath`, `maxResults`, `nextToken`, `topK`, ...).
fn search(query: &str, extra: Value) -> Value {
    let mut body = json!({"searchCriteria": {"searchQuery": query}});
    if let Value::Object(members) = extra {
        for (name, value) in members {
            match name.as_str() {
                // `memoryStrategyId`, `topK` and `metadataFilters` are members of the
                // search criteria; everything else is a member of the request itself.
                "memoryStrategyId" | "topK" | "metadataFilters" => {
                    body["searchCriteria"][name] = value;
                }
                _ => body[name] = value,
            }
        }
    }
    body
}

/// The records every case is searched against, created one batch at a time so their
/// creation order — the order a tie in relevance is broken by — is known.
///
/// Rows differ along the content (so the share of a query's terms each one carries
/// differs), the namespace depth, the strategy association and the metadata they carry.
/// Answers the label -> `memoryRecordId` pairs, oldest first.
async fn searched_fixture(router: &MultiServiceRouter) -> Vec<(&'static str, String)> {
    let entries: Vec<(&'static str, Value)> = vec![
        (
            "window-seat",
            json!({
                "requestIdentifier": "search-1",
                "namespaces": ["support/facts/actor-1"],
                "content": {"text": "the user prefers window seats on long flights"},
                "timestamp": 1_700_000_000,
                "metadata": {"topic": {"stringValue": "travel"}},
            }),
        ),
        (
            "aisle-seat",
            json!({
                "requestIdentifier": "search-2",
                "namespaces": ["support/preferences/actor-1"],
                "content": {"text": "the user likes aisle seats"},
                "timestamp": 1_700_000_100,
                "memoryStrategyId": "userPreference-abc123",
                "metadata": {"topic": {"stringValue": "travel"}},
            }),
        ),
        (
            "vegetarian",
            json!({
                "requestIdentifier": "search-3",
                "namespaces": ["support/facts/actor-2"],
                "content": {"text": "the user is vegetarian"},
                "timestamp": 1_700_000_200,
                "metadata": {"topic": {"stringValue": "diet"}},
            }),
        ),
        (
            "seat-note",
            json!({
                "requestIdentifier": "search-4",
                "namespaces": ["notes/actor-3"],
                "content": {"text": "window seats"},
                "timestamp": 1_700_000_300,
            }),
        ),
    ];

    let mut created: Vec<(&'static str, String)> = Vec::new();
    for (name, entry) in entries {
        let (status, body) = batch_create(router, MEMORY, json!({"records": [entry]})).await;
        assert_eq!(
            status, 201,
            "fixture {name:?}: expected HTTP 201 from BatchCreateMemoryRecords, got {status} \
             with {body:?}"
        );
        let id = body["successfulRecords"][0]["memoryRecordId"]
            .as_str()
            .unwrap_or_else(|| panic!("fixture {name:?}: expected a minted id, got {body:?}"))
            .to_string();
        created.push((name, id));
    }
    created
}

/// The fixture labels a response reports, in the order it reported them.
fn reported(body: &Value, fixture: &[(&'static str, String)]) -> Vec<String> {
    body["memoryRecordSummaries"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|summary| {
            let id = summary["memoryRecordId"].as_str().unwrap_or_default();
            fixture
                .iter()
                .find(|(_, created)| created == id)
                .map_or_else(|| format!("unknown record {id}"), |(name, _)| (*name).to_string())
        })
        .collect()
}

/// The score a response reported for one fixture label, or `None` when it did not report
/// that record.
fn score_of(body: &Value, fixture: &[(&'static str, String)], label: &str) -> Option<f64> {
    let wanted = fixture
        .iter()
        .find(|(name, _)| *name == label)
        .map(|(_, id)| id.clone())?;
    body["memoryRecordSummaries"]
        .as_array()?
        .iter()
        .find(|summary| summary["memoryRecordId"].as_str() == Some(wanted.as_str()))
        .and_then(|summary| summary["score"].as_f64())
}

/// A search reports the records whose content is about the query, ranked by the share of
/// the query's distinct terms each one carries, and reports no record carrying none of
/// them.
///
/// Rows differ along what the rule generalises over: how many of the query's terms a
/// record carries, how many terms the query names, the letter case and punctuation it is
/// written with, a tie in relevance (broken by the more recently created record), and a
/// query nothing is about.
#[tokio::test]
async fn a_search_ranks_the_records_the_query_is_about() {
    let router = emulator();
    let fixture = searched_fixture(&router).await;

    // (case, the query, the fixture labels expected back in that order)
    let cases: Vec<(&str, &str, Vec<&str>)> = vec![
        (
            "both terms carried by two records, one by a third",
            "window seats",
            vec!["seat-note", "window-seat", "aisle-seat"],
        ),
        (
            "the same query in another case, with punctuation",
            "WINDOW, SEATS!",
            vec!["seat-note", "window-seat", "aisle-seat"],
        ),
        (
            "two of three terms carried by one record, one of three by two others",
            "seats flights sunglasses",
            vec!["window-seat", "seat-note", "aisle-seat"],
        ),
        ("one term, one record", "vegetarian", vec!["vegetarian"]),
        ("one term, one record, mid-sentence", "aisle", vec!["aisle-seat"]),
        (
            "a term three records share",
            "prefers likes vegetarian",
            vec!["vegetarian", "aisle-seat", "window-seat"],
        ),
        ("a query no record is about", "quantum entanglement", vec![]),
        ("a query naming no term at all", "...", vec![]),
    ];

    for (case, query, expected) in cases {
        let (status, body) = retrieve(&router, MEMORY, search(query, json!({}))).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from RetrieveMemoryRecords, got {status} with \
             {body:?}"
        );
        let seen = reported(&body, &fixture);
        assert_eq!(
            seen,
            expected.iter().map(ToString::to_string).collect::<Vec<_>>(),
            "case {case:?}: expected the records {expected:?} in that order, got {seen:?} from \
             {body:?}"
        );
        assert!(
            body["nextToken"].is_null(),
            "case {case:?}: expected no nextToken on a complete page, got {:?}",
            body["nextToken"]
        );
    }
}

/// The `score` a search reports is the share of the query's distinct terms the record's
/// content carries, so a record answering the whole query scores 1 and one answering half
/// of it scores 0.5.
///
/// Rows differ along the share itself (1, 2/3, 1/2, 1/3) and along how many terms the
/// query names, including a query that repeats a term — the terms are DISTINCT, so the
/// repeat does not change the share.
#[tokio::test]
async fn a_search_scores_the_share_of_the_query_a_record_carries() {
    let router = emulator();
    let fixture = searched_fixture(&router).await;

    // (case, the query, the label, the share expected)
    let cases: Vec<(&str, &str, &str, f64)> = vec![
        ("the whole query", "window seats", "seat-note", 1.0),
        ("the whole query in a longer record", "window seats", "window-seat", 1.0),
        ("half the query", "window seats", "aisle-seat", 0.5),
        (
            "two of three terms",
            "seats flights sunglasses",
            "window-seat",
            2.0 / 3.0,
        ),
        (
            "one of three terms",
            "seats flights sunglasses",
            "aisle-seat",
            1.0 / 3.0,
        ),
        (
            "a repeated term counts once",
            "window window seats",
            "seat-note",
            1.0,
        ),
        (
            "one of four terms",
            "vegetarian cycling music photography",
            "vegetarian",
            0.25,
        ),
    ];

    for (case, query, label, expected) in cases {
        let (status, body) = retrieve(&router, MEMORY, search(query, json!({}))).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} with {body:?}"
        );
        let actual = score_of(&body, &fixture, label);
        assert!(
            actual.is_some_and(|score| (score - expected).abs() < SCORE_TOLERANCE),
            "case {case:?}: expected {label:?} to score {expected}, got {actual:?} from {body:?}"
        );
    }
}

/// A reported summary is the record as it was stored — the same record `GetMemoryRecord`
/// reports — plus the relevance this search ranked it by.
#[tokio::test]
async fn a_searched_summary_reports_what_was_stored_plus_its_score() {
    let router = emulator();
    let fixture = searched_fixture(&router).await;

    let (status, body) = retrieve(&router, MEMORY, search("the user window seats", json!({}))).await;
    assert_eq!(
        status, 200,
        "expected HTTP 200 from RetrieveMemoryRecords, got {status} with {body:?}"
    );
    let summaries = body["memoryRecordSummaries"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        summaries.len(),
        fixture.len(),
        "expected every fixture record to be about this query, got {summaries:?}"
    );

    for summary in &summaries {
        let id = summary["memoryRecordId"].as_str().unwrap_or_default();
        let (status, read) = get_record(&router, MEMORY, id).await;
        assert_eq!(
            status, 200,
            "case {id:?}: expected HTTP 200 from GetMemoryRecord, got {status} with {read:?}"
        );
        let stored = &read["memoryRecord"];
        for member in [
            "memoryRecordId",
            "content",
            "memoryStrategyId",
            "namespaces",
            "createdAt",
            "timestamp",
            "metadata",
        ] {
            assert_eq!(
                summary[member], stored[member],
                "case {id:?}: expected the searched summary's {member:?} to be what was stored \
                 ({:?}), got {:?}",
                stored[member], summary[member]
            );
        }
        assert!(
            summary["score"].as_f64().is_some_and(|score| score > 0.0),
            "case {id:?}: expected a relevance score above zero on a search result, got {:?}",
            summary["score"]
        );
    }
}

/// A search narrows the container's records exactly as a listing does before it ranks
/// them: the exact namespace, the namespace hierarchy, the strategy association and the
/// metadata expressions, each narrowing on its own and all of them narrowing together.
///
/// Rows span the scope members the request carries (source: smithy_query query="operation"
/// operation="RetrieveMemoryRecords" — `namespace` is "the exact namespace", `namespacePath`
/// returns the records whose "namespace falls under the same parent hierarchy", and
/// `searchCriteria` carries `memoryStrategyId` and `metadataFilters`), with one row per
/// metadata operator class and one for a scope nothing satisfies.
#[tokio::test]
async fn a_search_narrows_by_the_scope_the_request_asks_for() {
    let router = emulator();
    let fixture = searched_fixture(&router).await;

    // (case, what the request adds to the query "the user", the labels expected in order)
    let cases: Vec<(&str, Value, Vec<&str>)> = vec![
        (
            "no scope at all",
            json!({}),
            vec!["vegetarian", "aisle-seat", "window-seat"],
        ),
        (
            "the exact namespace of one record",
            json!({"namespace": "support/facts/actor-1"}),
            vec!["window-seat"],
        ),
        (
            "the exact namespace of another record",
            json!({"namespace": "support/preferences/actor-1"}),
            vec!["aisle-seat"],
        ),
        (
            "a namespace hierarchy covering two records",
            json!({"namespacePath": "support/facts"}),
            vec!["vegetarian", "window-seat"],
        ),
        (
            "a namespace hierarchy covering every support record",
            json!({"namespacePath": "support"}),
            vec!["vegetarian", "aisle-seat", "window-seat"],
        ),
        (
            "the strategy association one record carries",
            json!({"memoryStrategyId": "userPreference-abc123"}),
            vec!["aisle-seat"],
        ),
        (
            "a metadata value one record carries",
            json!({"metadataFilters": [metadata_filter("topic", "EQUALS_TO", Some(json!({"stringValue": "diet"})))]}),
            vec!["vegetarian"],
        ),
        (
            "a metadata key three records carry",
            json!({"metadataFilters": [metadata_filter("topic", "EXISTS", None)]}),
            vec!["vegetarian", "aisle-seat", "window-seat"],
        ),
        (
            "a metadata substring",
            json!({"metadataFilters": [metadata_filter("topic", "CONTAINS", Some(json!({"stringValue": "rave"})))]}),
            vec!["aisle-seat", "window-seat"],
        ),
        (
            "a namespace and a metadata value together",
            json!({
                "namespacePath": "support",
                "metadataFilters": [metadata_filter("topic", "EQUALS_TO", Some(json!({"stringValue": "travel"})))],
            }),
            vec!["aisle-seat", "window-seat"],
        ),
        (
            "a scope no record satisfies",
            json!({"namespace": "support/facts/actor-9"}),
            vec![],
        ),
        (
            "filters that select disjoint records select none",
            json!({
                "namespace": "support/facts/actor-1",
                "memoryStrategyId": "userPreference-abc123",
            }),
            vec![],
        ),
    ];

    for (case, extra, expected) in cases {
        let (status, body) = retrieve(&router, MEMORY, search("the user", extra)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} with {body:?}"
        );
        let seen = reported(&body, &fixture);
        assert_eq!(
            seen,
            expected.iter().map(ToString::to_string).collect::<Vec<_>>(),
            "case {case:?}: expected the records {expected:?} in that order, got {seen:?} from \
             {body:?}"
        );
    }
}

/// `searchCriteria.topK` bounds how many of the top-scoring records the search reports,
/// taking them off the top of the ranking.
///
/// Rows cover the published minimum of `topK` (1, source: smithy_query query="shape"
/// shape="SearchCriteria" — `@range(min: 1, max: 100)`), the values between it and the
/// number of matching records, the published maximum (100), and the member left out, which
/// defaults to 10.
#[tokio::test]
async fn top_k_bounds_how_many_records_a_search_reports() {
    let router = emulator();
    let fixture = searched_fixture(&router).await;
    // The whole ranking for this query, most relevant first.
    let ranking = ["seat-note", "window-seat", "aisle-seat"];

    let cases: Vec<(&str, Value, usize)> = vec![
        ("the minimum, one record", json!({"topK": 1}), 1),
        ("two records", json!({"topK": 2}), 2),
        ("as many as match", json!({"topK": 3}), 3),
        ("more than match", json!({"topK": 4}), 3),
        ("the maximum, 100", json!({"topK": 100}), 3),
        ("no topK at all, the default of 10", json!({}), 3),
    ];

    for (case, extra, expected) in cases {
        let (status, body) = retrieve(&router, MEMORY, search("window seats", extra)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} with {body:?}"
        );
        let seen = reported(&body, &fixture);
        assert_eq!(
            seen,
            ranking[..expected]
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            "case {case:?}: expected the top {expected} of the ranking, got {seen:?} from {body:?}"
        );
    }
}

/// A search pages deterministically over its ranking: whatever the page size, the pages
/// walk the same most-relevant-first sequence exactly once and the last page carries no
/// token.
#[tokio::test]
async fn a_search_pages_over_every_page_size() {
    let router = emulator();
    let fixture = searched_fixture(&router).await;
    let ranking: Vec<String> = ["seat-note", "window-seat", "aisle-seat"]
        .iter()
        .map(ToString::to_string)
        .collect();

    // Rows: every page size from one record per page to more than the ranking holds, plus
    // the published maximum of `maxResults` (100) and its minimum (1).
    for page_size in [1_usize, 2, 3, 4, 100] {
        let mut collected: Vec<String> = Vec::new();
        let mut token = Value::Null;
        let mut pages = 0;
        loop {
            let mut extra = json!({"maxResults": page_size});
            if let Some(value) = token.as_str() {
                extra["nextToken"] = json!(value);
            }
            let (status, page) = retrieve(&router, MEMORY, search("window seats", extra)).await;
            assert_eq!(
                status, 200,
                "page size {page_size}: expected HTTP 200, got {status} with {page:?}"
            );
            let now = reported(&page, &fixture);
            assert!(
                now.len() <= page_size,
                "page size {page_size}: expected at most {page_size} records on a page, got {now:?}"
            );
            collected.extend(now);
            token = page["nextToken"].clone();
            pages += 1;
            assert!(
                pages <= ranking.len() + 1,
                "page size {page_size}: expected the walk to end, got {pages} pages so far \
                 ({collected:?})"
            );
            if token.is_null() {
                break;
            }
        }
        assert_eq!(
            collected, ranking,
            "page size {page_size}: expected every matching record once, most relevant first, \
             got {collected:?}"
        );
    }
}

/// A search reports the records of the container it addresses and nothing else: the ARN
/// spelling addresses the same container, a container with nothing stored in it is an empty
/// page, and a `memoryId` naming no container at all is refused at the container level.
#[tokio::test]
async fn a_search_is_scoped_to_its_memory_container() {
    let router = emulator();
    let fixture = searched_fixture(&router).await;

    let (status, body) = retrieve(&router, MEMORY_ARN, search("window seats", json!({}))).await;
    assert_eq!(
        status, 200,
        "case \"the container's ARN spelling\": expected HTTP 200, got {status} with {body:?}"
    );
    assert_eq!(
        reported(&body, &fixture),
        vec![
            "seat-note".to_string(),
            "window-seat".to_string(),
            "aisle-seat".to_string()
        ],
        "case \"the container's ARN spelling\": expected the same ranking, got {body:?}"
    );

    // A container this plane holds nothing for reports nothing — the container exists, so
    // this is an empty page rather than a refusal.
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    super::harness::create_memory(&engine, OTHER_MEMORY);
    let mut empty_router = MultiServiceRouter::new();
    empty_router.set_request_authentication(false);
    empty_router.register_service(agentcore::EmulatedService::new(engine));
    let (status, body) = retrieve(&empty_router, OTHER_MEMORY, search("window seats", json!({}))).await;
    assert_eq!(
        status, 200,
        "case \"a container with nothing stored\": expected HTTP 200, got {status} with {body:?}"
    );
    assert_eq!(
        body["memoryRecordSummaries"],
        json!([]),
        "case \"a container with nothing stored\": expected an empty page, got {body:?}"
    );

    // A container the account does not hold is refused, with the sentence the memory
    // records API words its container level with.
    let (status, body) = retrieve(&router, OTHER_MEMORY, search("window seats", json!({}))).await;
    assert_eq!(
        status, 404,
        "case \"a container that was never created\": expected HTTP 404, got {status} with {body:?}"
    );
    assert_eq!(
        error_code(&body),
        "ResourceNotFoundException",
        "case \"a container that was never created\": expected ResourceNotFoundException, got \
         {body:?}"
    );
    assert_eq!(
        body["message"], "MemoryItem does not exist",
        "case \"a container that was never created\": expected the container-level not-found \
         message, got {:?}",
        body["message"]
    );
}

/// A search refuses a request it cannot answer as asked: a `nextToken` it never issued
/// (another paginated operation's included), a query larger than this plane reads (the
/// modelled `searchQuery` ceiling of 10000 characters, and the distinct-term count that
/// length implies), and a metadata expression whose operator compares against a value the
/// request did not give it (`right` is "required for all operators except EXISTS and
/// NOT_EXISTS").
#[tokio::test]
async fn a_search_refuses_a_request_it_cannot_answer() {
    let router = emulator();
    let _ = searched_fixture(&router).await;

    let listing_token = aws_smithy_types::base64::encode("memoryRecords:1");
    // One character past the modelled maximum of `searchCriteria.searchQuery`
    // (`@length(min: 1, max: 10000)`), which is the ceiling on the query text this plane
    // reads at all.
    let oversized: String = "a".repeat(10_001);
    let cases: Vec<(&str, &str, Value)> = vec![
        (
            "a token that is not base64",
            "window seats",
            json!({"nextToken": "not a token"}),
        ),
        (
            "base64 that is not a cursor",
            "window seats",
            json!({"nextToken": "YWJjZGU="}),
        ),
        (
            "a token the plain listing issued",
            "window seats",
            json!({"nextToken": listing_token.as_str()}),
        ),
        (
            "EQUALS_TO with no value to compare against",
            "window seats",
            json!({"metadataFilters": [metadata_filter("topic", "EQUALS_TO", None)]}),
        ),
        (
            "CONTAINS with no value to compare against",
            "window seats",
            json!({"metadataFilters": [metadata_filter("topic", "CONTAINS", None)]}),
        ),
        (
            "BEFORE with no value to compare against",
            "window seats",
            json!({"metadataFilters": [metadata_filter("observedAt", "BEFORE", None)]}),
        ),
        ("a query past the length the plane reads", &oversized, json!({})),
    ];

    for (case, query, extra) in cases {
        let (status, body) = retrieve(&router, MEMORY, search(query, extra)).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400 from RetrieveMemoryRecords, got {status} with \
             {body:?}"
        );
        assert_eq!(
            error_code(&body),
            "ValidationException",
            "case {case:?}: expected ValidationException, got {body:?}"
        );
    }

    // The operators that take no value are answered, not refused.
    for operator in ["EXISTS", "NOT_EXISTS"] {
        let (status, body) = retrieve(
            &router,
            MEMORY,
            search(
                "window seats",
                json!({"metadataFilters": [metadata_filter("topic", operator, None)]}),
            ),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {operator:?}: expected HTTP 200 for an operator that needs no value, got \
             {status} with {body:?}"
        );
    }
}
