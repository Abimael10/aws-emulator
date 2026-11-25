// SPDX-License-Identifier: CC0-1.0

//! What `ListAssertions` reports is the service's assertion collection AS IT STANDS, after
//! any sequence of edits and deletions — not the collection as the creates left it.
//!
//! The rule under test: a listing projects the stored assertion records of the addressed
//! service, so every committed mutation of that collection is visible to the next read —
//! an edited record is reported with the text its `UpdateAssertion` wrote, a removed record
//! is reported by nothing, and a collection every member of which has been removed is
//! reported as the empty (still present, `@required`) list. The projection is the same at
//! every page size, so paging over a mutated collection enumerates exactly the surviving
//! records, once each, in creation order.
//!
//! Where `assertion_lifecycle.rs` drives ONE mutation and reads it back, these suites drive
//! PROGRAMS of mutations — interleaved edits and deletions, in varying order, over
//! collections of varying size — and re-read the whole collection after every step at three
//! page sizes. The rows differ along the number of assertions, the kind and order of the
//! mutations, which position in creation order each one touches, and the length and shape of
//! the replacement text.
//!
//! Every suite drives the real wire path (`MultiServiceRouter::dispatch`), so the modelled
//! constraints, the restJson1 (de)serializers and the HTTP status codes are exercised exactly
//! as a client would exercise them.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_resiliencehubv2_ngrhservicecore::state::{
    ASSERTION_SOURCE_USER, MAX_RESULTS_MAX, MAX_RESULTS_MIN,
};
use service_com_amazonaws_resiliencehubv2_ngrhservicecore::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The modelled routes these suites drive.
const CREATE_SERVICE: &str = "/v2/create-service";
const CREATE_ASSERTION: &str = "/v2/create-assertion";
const UPDATE_ASSERTION: &str = "/v2/update-assertion";
const DELETE_ASSERTION: &str = "/v2/delete-assertion";
const LIST_ASSERTIONS: &str = "/v2/list-assertions";

/// A SigV4 `Authorization` header whose credential scope names this service's signing name.
/// Signature verification is off for these tests, so the value only has to parse.
const SIGNED: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDTESTONLY00000000/20260101/us-east-1/resiliencehub/aws4_request, \
     SignedHeaders=host;x-amz-date, Signature=0123456789abcdef";

/// `AssertionText`'s modelled bounds.
///
/// SOURCE: `smithy_query(query="operation", operation="UpdateAssertion")` reports `text`
/// targeting `AssertionText` with `length {min: 1, max: 1000}`.
const ASSERTION_TEXT_MIN_LENGTH: usize = 1;
const ASSERTION_TEXT_MAX_LENGTH: usize = 1000;

/// An emulator holding only this service, over one shared engine.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(engine));
    router
}

async fn call(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let payload = body
        .map(|value| serde_json::to_vec(&value).expect("a JSON body serializes"))
        .unwrap_or_default();
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", SIGNED)
        .header("content-type", "application/json")
        .body(bytes::Bytes::from(payload))
        .expect("a well-formed request");
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

/// Create a service named `name` and answer its ARN.
async fn create_service(router: &MultiServiceRouter, name: &str) -> String {
    let (status, body) = call(
        router,
        "POST",
        CREATE_SERVICE,
        Some(json!({
            "name": name,
            "regions": ["us-east-1"],
            "permissionModel": {"invokerRoleName": "ResilienceHubRole"},
        })),
    )
    .await;
    assert_eq!(status, 200, "creating service {name}: got {status} {body:?}");
    body["service"]["serviceArn"]
        .as_str()
        .unwrap_or_else(|| panic!("no service.serviceArn in {body:?}"))
        .to_string()
}

/// Create one assertion carrying `text` under `service_arn` and answer the `Assertion` the
/// create projected.
async fn create_assertion(
    router: &MultiServiceRouter,
    case: &str,
    service_arn: &str,
    text: &str,
) -> Value {
    let (status, body) = call(
        router,
        "POST",
        CREATE_ASSERTION,
        Some(json!({"serviceArn": service_arn, "text": text})),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateAssertion, got {status} {body:?}"
    );
    body["assertion"].clone()
}

/// One page of a listing, as `(status, body)`.
async fn list_page(router: &MultiServiceRouter, query: &str) -> (u16, Value) {
    call(router, "GET", &format!("{LIST_ASSERTIONS}?{query}"), None).await
}

/// The `assertions` array of a successful listing.
fn listed(case: &str, status: u16, body: &Value) -> Vec<Value> {
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from ListAssertions, got {status} {body:?}"
    );
    body["assertions"]
        .as_array()
        .unwrap_or_else(|| {
            panic!("case {case:?}: the @required assertions member is absent from {body:?}")
        })
        .clone()
}

/// The modelled error code of a refusal, from the restJson1 `__type` discriminator.
fn error_code(body: &Value) -> String {
    body["__type"]
        .as_str()
        .unwrap_or("<no __type>")
        .rsplit(['#', ':'])
        .next()
        .unwrap_or("<no __type>")
        .to_string()
}

/// Every assertion `service_arn` reports, read through as many pages as `max_results`
/// takes. `None` asks for the default page size (no `maxResults` member at all).
async fn list_every_assertion(
    router: &MultiServiceRouter,
    case: &str,
    service_arn: &str,
    max_results: Option<i32>,
) -> Vec<Value> {
    let mut seen: Vec<Value> = Vec::new();
    let mut next: Option<String> = None;
    let mut pages = 0usize;
    loop {
        let mut query = format!("serviceArn={service_arn}");
        if let Some(size) = max_results {
            query.push_str(&format!("&maxResults={size}"));
        }
        if let Some(token) = next.as_deref() {
            query.push_str(&format!("&nextToken={token}"));
        }
        let (status, body) = list_page(router, &query).await;
        let page = listed(case, status, &body);
        if let Some(size) = max_results {
            assert!(
                page.len() <= size as usize,
                "case {case:?}: a page must not exceed maxResults={size}, got {page:?}"
            );
        }
        seen.extend(page);
        pages += 1;
        // Paging over a collection of at most this many records cannot need more pages
        // than it has records plus the final empty one.
        assert!(
            pages <= seen.len() + 2,
            "case {case:?}: paging did not terminate, saw {seen:?}"
        );
        next = body
            .get("nextToken")
            .and_then(Value::as_str)
            .map(str::to_string);
        if next.is_none() {
            break;
        }
    }
    seen
}

/// The id, then the text, of each listed assertion — what the collection holds, in the
/// order it is reported.
fn ids_and_texts(case: &str, assertions: &[Value]) -> Vec<(String, String)> {
    assertions
        .iter()
        .map(|assertion| {
            let id = assertion["assertionId"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: no assertionId in {assertion:?}"));
            let text = assertion["text"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: no text in {assertion:?}"));
            (id.to_string(), text.to_string())
        })
        .collect()
}

/// The replacement texts the edit steps draw on: rows of the `AssertionText` class, from the
/// shortest the model admits to the longest.
///
/// SOURCE: `smithy_query(query="operation", operation="UpdateAssertion")` — `text` targets
/// `AssertionText`, `length {min: 1, max: 1000}`.
#[derive(Debug, Clone, Copy)]
enum Text {
    Shortest,
    Sentence,
    PunctuationAndNewlines,
    NonAscii,
    Longest,
}

impl Text {
    fn value(self) -> String {
        match self {
            Text::Shortest => "a".repeat(ASSERTION_TEXT_MIN_LENGTH),
            Text::Sentence => "Survives the loss of one Region.".to_string(),
            Text::PunctuationAndNewlines => {
                "RTO <= 60s; RPO <= 5s.\nFails over to the standby Region.".to_string()
            }
            Text::NonAscii => "Le service tolère la perte d'une zone — RTO ≤ 60 s.".to_string(),
            Text::Longest => "t".repeat(ASSERTION_TEXT_MAX_LENGTH),
        }
    }
}

/// One mutation of a service's assertion collection. The index names a position in the
/// collection's ORIGINAL creation order, so a program can edit or delete any member of it.
#[derive(Debug, Clone, Copy)]
enum Mutation {
    Edit(usize, Text),
    Delete(usize),
}

/// The mutation programs under test. The rows differ along how many assertions the service
/// holds, which kinds of mutation the program performs, in which order, and which positions
/// in creation order they touch.
fn programs() -> Vec<(&'static str, usize, Vec<Mutation>)> {
    vec![
        (
            "the only assertion edited then deleted",
            1,
            vec![Mutation::Edit(0, Text::Sentence), Mutation::Delete(0)],
        ),
        (
            "the first of three deleted, the last edited",
            3,
            vec![Mutation::Delete(0), Mutation::Edit(2, Text::Longest)],
        ),
        (
            "every assertion edited in turn",
            3,
            vec![
                Mutation::Edit(0, Text::Shortest),
                Mutation::Edit(1, Text::NonAscii),
                Mutation::Edit(2, Text::PunctuationAndNewlines),
            ],
        ),
        (
            "every assertion deleted, out of creation order",
            3,
            vec![
                Mutation::Delete(1),
                Mutation::Delete(2),
                Mutation::Delete(0),
            ],
        ),
        (
            "the same assertion edited twice",
            2,
            vec![
                Mutation::Edit(1, Text::Sentence),
                Mutation::Edit(1, Text::NonAscii),
            ],
        ),
        (
            "edits and deletes interleaved over five",
            5,
            vec![
                Mutation::Edit(0, Text::Sentence),
                Mutation::Delete(2),
                Mutation::Edit(4, Text::Shortest),
                Mutation::Delete(0),
                Mutation::Edit(3, Text::PunctuationAndNewlines),
                Mutation::Delete(4),
            ],
        ),
    ]
}

/// The page sizes every read in these suites is taken at: each modelled bound of
/// `maxResults` (`range 1..100`), a size that splits a collection into several pages, and
/// the default (no `maxResults`).
fn page_sizes() -> Vec<(&'static str, Option<i32>)> {
    vec![
        ("the minimum page size", Some(MAX_RESULTS_MIN)),
        ("a page size of two", Some(2)),
        ("the maximum page size", Some(MAX_RESULTS_MAX)),
        ("the default page size", None),
    ]
}

#[tokio::test]
async fn a_listing_reports_the_assertion_collection_every_mutation_left_behind() {
    let router = emulator();

    // A neighbouring service whose single assertion no program touches: a mutation reaches
    // only the collection it addresses.
    let neighbour = create_service(&router, "Neighbour").await;
    let untouched = create_assertion(
        &router,
        "the neighbouring service",
        &neighbour,
        "The neighbour's assertion.",
    )
    .await;

    for (row, (case, count, program)) in programs().into_iter().enumerate() {
        // `ServiceName` is modelled `^[A-Za-z0-9][A-Za-z0-9_\-]{1,59}$`, so each program
        // gets its own service under a name of that form rather than the case wording.
        let service_arn = create_service(&router, &format!("Program{row}")).await;

        // The collection as the creates leave it: one record per created assertion, each
        // carrying the text its create supplied.
        let mut created: Vec<Value> = Vec::new();
        for index in 0..count {
            created.push(
                create_assertion(
                    &router,
                    case,
                    &service_arn,
                    &format!("Assertion {index} of {case}."),
                )
                .await,
            );
        }
        // What the collection is expected to hold, in creation order: (id, text).
        let mut expected: Vec<(String, String)> = created
            .iter()
            .map(|assertion| {
                (
                    assertion["assertionId"].as_str().unwrap_or_default().to_string(),
                    assertion["text"].as_str().unwrap_or_default().to_string(),
                )
            })
            .collect();

        for (step, mutation) in program.iter().enumerate() {
            let case = format!("{case}, step {step} ({mutation:?})");
            match mutation {
                Mutation::Edit(index, text) => {
                    let text = text.value();
                    let id = created[*index]["assertionId"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string();
                    let (status, body) = call(
                        &router,
                        "POST",
                        UPDATE_ASSERTION,
                        Some(json!({
                            "serviceArn": service_arn,
                            "assertionId": id,
                            "text": text,
                        })),
                    )
                    .await;
                    assert_eq!(
                        status, 200,
                        "case {case:?}: expected HTTP 200 from UpdateAssertion, got {status} \
                         {body:?}"
                    );
                    assert_eq!(
                        body["assertion"]["text"].as_str(),
                        Some(text.as_str()),
                        "case {case:?}: UpdateAssertion must answer the text it wrote, got {:?}",
                        body["assertion"]["text"]
                    );
                    for held in expected.iter_mut() {
                        if held.0 == id {
                            held.1 = text.clone();
                        }
                    }
                }
                Mutation::Delete(index) => {
                    let id = created[*index]["assertionId"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string();
                    let (status, body) = call(
                        &router,
                        "POST",
                        DELETE_ASSERTION,
                        Some(json!({"serviceArn": service_arn, "assertionId": id})),
                    )
                    .await;
                    assert_eq!(
                        status, 200,
                        "case {case:?}: expected HTTP 200 from DeleteAssertion, got {status} \
                         {body:?}"
                    );
                    assert_eq!(
                        body["assertionId"].as_str(),
                        Some(id.as_str()),
                        "case {case:?}: DeleteAssertion must answer the id it removed, got {body:?}"
                    );
                    expected.retain(|held| held.0 != id);
                }
            }

            // The whole collection, read back at every page size: the mutation is in the
            // store, so the listing reports exactly the surviving records — each with the
            // text last written for it — in creation order, and nothing else.
            for (sizing, max_results) in page_sizes() {
                let case = format!("{case} read at {sizing}");
                let assertions =
                    list_every_assertion(&router, &case, &service_arn, max_results).await;
                assert_eq!(
                    ids_and_texts(&case, &assertions),
                    expected,
                    "case {case:?}: expected the surviving assertions and their current text in \
                     creation order, got {assertions:?}"
                );
                for assertion in &assertions {
                    let created_record = created
                        .iter()
                        .find(|candidate| candidate["assertionId"] == assertion["assertionId"])
                        .unwrap_or_else(|| {
                            panic!("case {case:?}: an id no create minted in {assertion:?}")
                        });
                    // A mutation changes the text and the edit stamp; nothing else about the
                    // record moves.
                    for member in ["assertionId", "serviceArn", "source", "createdAt"] {
                        assert_eq!(
                            assertion[member], created_record[member],
                            "case {case:?}: {member} must stay where the create put it, expected \
                             {:?}, got {:?}",
                            created_record[member], assertion[member]
                        );
                    }
                    assert_eq!(
                        assertion["source"].as_str(),
                        Some(ASSERTION_SOURCE_USER),
                        "case {case:?}: a caller-created assertion's source is \
                         {ASSERTION_SOURCE_USER}, got {:?}",
                        assertion["source"]
                    );
                    assert!(
                        assertion["updatedAt"].as_f64() >= assertion["createdAt"].as_f64(),
                        "case {case:?}: updatedAt must not precede createdAt, got {:?} and {:?}",
                        assertion["updatedAt"],
                        assertion["createdAt"]
                    );
                }
            }
        }

        // A program that removed every assertion leaves the @required member present and
        // empty — and no nextToken, there being no further page.
        if expected.is_empty() {
            let (status, body) = list_page(&router, &format!("serviceArn={service_arn}")).await;
            let assertions = listed(case, status, &body);
            assert!(
                assertions.is_empty(),
                "case {case:?}: a collection whose every assertion was deleted is the empty \
                 list, got {assertions:?}"
            );
            assert!(
                body.get("nextToken").is_none_or(Value::is_null),
                "case {case:?}: an empty listing has no next page, got {body:?}"
            );
        }
    }

    // None of the programs reached the neighbouring service's collection.
    let neighbours = list_every_assertion(&router, "the neighbouring service", &neighbour, None)
        .await;
    assert_eq!(
        neighbours,
        vec![untouched],
        "another service's assertion must survive every mutation of another collection, got \
         {neighbours:?}"
    );
}

#[tokio::test]
async fn a_refused_mutation_leaves_the_listed_collection_exactly_as_it_was() {
    let router = emulator();
    let service_arn = create_service(&router, "Refusals").await;
    let elsewhere = create_service(&router, "Elsewhere").await;

    let mut created: Vec<Value> = Vec::new();
    for index in 0..3 {
        created.push(
            create_assertion(
                &router,
                "the addressed service",
                &service_arn,
                &format!("Assertion {index}."),
            )
            .await,
        );
    }
    let foreign = create_assertion(
        &router,
        "another service's assertion",
        &elsewhere,
        "Another service's assertion.",
    )
    .await;
    let foreign_id = foreign["assertionId"]
        .as_str()
        .unwrap_or_default()
        .to_string();

    // One assertion is genuinely removed first, so "an id this service held until now" is
    // among the rows below.
    let removed_id = created[1]["assertionId"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let (status, body) = call(
        &router,
        "POST",
        DELETE_ASSERTION,
        Some(json!({"serviceArn": service_arn, "assertionId": removed_id})),
    )
    .await;
    assert_eq!(status, 200, "deleting the middle assertion: {status} {body:?}");

    let before = list_every_assertion(&router, "after the delete", &service_arn, None).await;
    let expected = ids_and_texts("after the delete", &before);
    assert_eq!(
        expected.len(),
        2,
        "the collection must hold the two surviving assertions, got {before:?}"
    );

    // The rows differ along WHICH assertionId names nothing under this service: one it never
    // held, one it held until the delete above, and one another service holds.
    //
    // SOURCE: `smithy_query(query="operation", operation="DeleteAssertion")` — `assertionId`
    // targets `Uuid`, so each row is of that form.
    const NEVER_MINTED: &str = "0ac1f2e3-4b5d-4c6e-8f90-1a2b3c4d5e6f";
    for (row, assertion_id) in [
        ("an id no service holds", NEVER_MINTED.to_string()),
        ("an id this service held until the delete", removed_id.clone()),
        ("an id another service holds", foreign_id.clone()),
    ] {
        for (op, route, body) in [
            (
                "UpdateAssertion",
                UPDATE_ASSERTION,
                json!({
                    "serviceArn": service_arn,
                    "assertionId": assertion_id,
                    "text": "An edit of an assertion this service does not hold.",
                }),
            ),
            (
                "DeleteAssertion",
                DELETE_ASSERTION,
                json!({"serviceArn": service_arn, "assertionId": assertion_id}),
            ),
        ] {
            let case = format!("{op} with {row}");
            let (status, answered) = call(&router, "POST", route, Some(body)).await;
            assert_eq!(
                status, 404,
                "case {case:?}: expected HTTP 404, got {status} {answered:?}"
            );
            assert_eq!(
                error_code(&answered),
                "ResourceNotFoundException",
                "case {case:?}: expected ResourceNotFoundException, got {answered:?}"
            );

            // A refusal changed nothing: the collection reads back exactly as it did, at
            // every page size.
            for (sizing, max_results) in page_sizes() {
                let case = format!("{case} read at {sizing}");
                let after = list_every_assertion(&router, &case, &service_arn, max_results).await;
                assert_eq!(
                    after, before,
                    "case {case:?}: a refused mutation must leave the listed collection \
                     untouched, got {after:?}"
                );
            }
        }
    }

    // And the other service's collection is still exactly its own one assertion.
    let foreigners = list_every_assertion(&router, "the other service", &elsewhere, None).await;
    assert_eq!(
        foreigners,
        vec![foreign],
        "the other service's collection must be untouched, got {foreigners:?}"
    );
}
