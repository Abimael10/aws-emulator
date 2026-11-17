// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of `GetDataAutomationLibraryEntity`: the read of ONE entity a
//! DataAutomationLibrary holds.
//!
//! RULE. The entities of a (library, entity type) pair are what the ingestion jobs accepted
//! against that library have made of it: an `UPSERT` job's inline entries stand the entity
//! its `entityId` names up (or replace what was stored under it) once that job has
//! completed, and a `DELETE` job's entries take entities out — so a read of the triple
//! reports the description, language and phrases of the latest entry that upserted the id,
//! and refuses anything that resolves to nothing (an unresolved library, another library's
//! entity, the other modelled entity type, an id nothing upserted, an id a delete removed).
//!
//! Every case drives the real `restJson1` wire path — `MultiServiceRouter::dispatch` with a
//! signed-looking request — so routing, HTTP binding, constraint validation, the handler and
//! response serialization are all exercised, exactly as an AWS SDK `bedrock-data-automation`
//! client would.
//!
//! The suites are TABLE-driven: the rows differ along the `entityId` form, whether a
//! description is carried, the language named, the number of phrases and whether a phrase
//! carries a display form — never over one recorded request.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_bedrockdataautomation_amazonbedrockkeystonebuildtimeservice::state;
use service_com_amazonaws_bedrockdataautomation_amazonbedrockkeystonebuildtimeservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

use crate::support::{encode_label, error_code, string_at, timestamp_at};

/// The region every case signs for.
const REGION: &str = "us-east-1";

/// A `libraryArn` that satisfies the modelled pattern and names no library of this store.
const UNKNOWN_LIBRARY_ARN: &str =
    "arn:aws:bedrock:us-east-1:000000000000:data-automation-library/000000000000";

/// A `libraryArn` of ANOTHER Region, well formed but addressing nothing this endpoint serves.
const OTHER_REGION_LIBRARY_ARN: &str =
    "arn:aws:bedrock:eu-west-1:000000000000:data-automation-library/000000000000";

/// How long a case waits for the condition it polls for: the ingestion the submission owes
/// falls due a span after the call, and this budget is deliberately far wider than that
/// span, so a loaded host only makes the loop go round more often.
const POLL_DEADLINE: Duration = Duration::from_secs(120);

/// How long the poll loop parks between looks. The runtime this workspace enables carries
/// no timer, so the test thread parks; a test may wait for a due instant, a handler may not.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// A SigV4 `Authorization` header for this service's signing name (`bedrock`, per
/// `OPERATIONS.md`).
fn authorization() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDTESTKEY/20240101/{REGION}/bedrock/aws4_request, \
         SignedHeaders=host, Signature=0123456789abcdef"
    )
}

fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(engine));
    router
}

/// Dispatch one request and return its status and decoded JSON body.
async fn call(router: &MultiServiceRouter, method: &str, uri: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "bedrock-data-automation.amazonaws.com")
        .header("content-type", "application/json")
        .header("authorization", authorization())
        .body(Bytes::from(
            serde_json::to_vec(body).expect("serializable body"),
        ))
        .expect("a valid test request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let decoded = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, decoded)
}

/// `CreateDataAutomationLibrary`, per its modelled `@http`
/// `{method: PUT, uri: /data-automation-libraries/, code: 201}`. Returns the ARN.
async fn create_library(router: &MultiServiceRouter, name: &str) -> String {
    let (status, payload) = call(
        router,
        "PUT",
        "/data-automation-libraries/",
        &json!({ "libraryName": name }),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {name:?}: expected CreateDataAutomationLibrary 201, got {status} with {payload:?}"
    );
    string_at(&payload, "libraryArn", name)
}

/// The ingestion-job collection path of one library.
fn jobs_uri(library_arn: &str) -> String {
    format!(
        "/data-automation-libraries/{}/library-ingestion-jobs/",
        encode_label(library_arn)
    )
}

/// The entity path, per `GetDataAutomationLibraryEntity`'s modelled `@http`
/// `{method: POST, uri:
/// /data-automation-libraries/{libraryArn}/entityType/{entityType}/entities/{entityId}}`.
fn entity_uri(library_arn: &str, entity_type: &str, entity_id: &str) -> String {
    format!(
        "/data-automation-libraries/{}/entityType/{}/entities/{}",
        encode_label(library_arn),
        encode_label(entity_type),
        encode_label(entity_id)
    )
}

/// A submission body: the four required members, with the inline payload the case names.
fn submission(entity_type: &str, operation_type: &str, payload: Value) -> Value {
    json!({
        "entityType": entity_type,
        "operationType": operation_type,
        "inputConfiguration": { "inlinePayload": payload },
        "outputConfiguration": { "s3Uri": "s3://bda-ingestion/output/" },
    })
}

/// `InvokeDataAutomationLibraryIngestionJob` for a submission that must be accepted.
async fn submit(router: &MultiServiceRouter, library_arn: &str, case: &str, body: &Value) {
    let (status, payload) = call(router, "PUT", &jobs_uri(library_arn), body).await;
    assert_eq!(
        status, 201,
        "case {case:?}: expected InvokeDataAutomationLibraryIngestionJob 201, got {status} with \
         {payload:?}"
    );
}

/// `GetDataAutomationLibraryEntity`.
async fn get_entity(
    router: &MultiServiceRouter,
    library_arn: &str,
    entity_type: &str,
    entity_id: &str,
) -> (u16, Value) {
    call(
        router,
        "POST",
        &entity_uri(library_arn, entity_type, entity_id),
        &json!({}),
    )
    .await
}

/// Look for the entity until the read ANSWERS what the case is waiting for, or the generous
/// [`POLL_DEADLINE`] passes.
///
/// The ingestion a submission owes falls due a span after the call, so what a case waits for
/// is the CONDITION (the entity is there, or it is gone) rather than a length of time: the
/// loop looks again until it holds.
async fn poll_entity(
    router: &MultiServiceRouter,
    library_arn: &str,
    entity_type: &str,
    entity_id: &str,
    want: u16,
    case: &str,
) -> Value {
    let started = Instant::now();
    let mut last = (0u16, Value::Null);
    while started.elapsed() < POLL_DEADLINE {
        let answer = get_entity(router, library_arn, entity_type, entity_id).await;
        if answer.0 == want {
            return answer.1;
        }
        last = answer;
        std::thread::sleep(POLL_INTERVAL);
    }
    panic!(
        "case {case:?}: the read never answered {want} for entity {entity_id:?}; last answer was \
         {last:?}"
    )
}

/// The `vocabulary` member of an answered `entity` union, or a panic naming the case.
fn vocabulary(answer: &Value, case: &str) -> Value {
    answer
        .get("entity")
        .and_then(|entity| entity.get("vocabulary"))
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: no entity.vocabulary in {answer:?}"))
}

/// One row of the upsert table: the case, the `entityId`, the entity info an upsert carries
/// and the phrase count it describes.
type UpsertRow<'a> = (&'a str, &'a str, Value, usize);

/// RULE: once the ingestion that upserted it has completed, the read of the triple reports
/// that entity exactly as the upsert described it — the id, the description, the language and
/// every phrase, with the instant the ingestion completed as its `lastModifiedTime`.
///
/// The rows differ along the `entityId` form, whether a description is carried, the language
/// named (both from the modelled `Language` enum,
/// `smithy_query(query="shape",shape="Language")` -> `["EN", "DE", "ES", "FR", "IT", "PT",
/// "JA", "KO", "CN", "TW", "HK"]`, including values the issue's own case does not use), the
/// number of phrases and whether a phrase carries a `displayAsText`.
#[tokio::test]
async fn an_upserted_entity_is_reported_as_the_ingestion_described_it() {
    let router = emulator();
    let library = create_library(&router, "entity-read-library").await;

    let rows: Vec<UpsertRow<'_>> = vec![
        (
            "one phrase, no description",
            "vocab-1",
            json!({
                "entityId": "vocab-1",
                "language": "EN",
                "phrases": [{ "text": "mockingbird" }],
            }),
            1,
        ),
        (
            "an underscored id, a description and three phrases",
            "vocab_two",
            json!({
                "entityId": "vocab_two",
                "description": "birds of the coast",
                "language": "DE",
                "phrases": [
                    { "text": "nachtigall" },
                    { "text": "amsel", "displayAsText": "Amsel" },
                    { "text": "eisvogel" },
                ],
            }),
            3,
        ),
        (
            "a numeric id and a non-latin language",
            "0123456789",
            json!({
                "entityId": "0123456789",
                "description": "a short note",
                "language": "JA",
                "phrases": [{ "text": "kawasemi", "displayAsText": "Kawasemi" }],
            }),
            1,
        ),
    ];

    for (case, entity_id, info, phrases) in rows {
        let body = submission(
            "VOCABULARY",
            "UPSERT",
            json!({ "upsertEntitiesInfo": [{ "vocabulary": info.clone() }] }),
        );
        submit(&router, &library, case, &body).await;

        let answer = poll_entity(&router, &library, "VOCABULARY", entity_id, 200, case).await;
        let reported = vocabulary(&answer, case);
        assert_eq!(
            string_at(&reported, "entityId", case),
            entity_id,
            "case {case:?}: expected the upserted id, got {reported:?}"
        );
        assert_eq!(
            reported.get("language"),
            info.get("language"),
            "case {case:?}: expected the language the upsert carried, got {reported:?}"
        );
        assert_eq!(
            reported.get("description"),
            info.get("description"),
            "case {case:?}: expected the description the upsert carried, got {reported:?}"
        );
        assert_eq!(
            reported.get("phrases"),
            info.get("phrases"),
            "case {case:?}: expected the phrases the upsert carried, got {reported:?}"
        );
        assert_eq!(
            reported
                .get("phrases")
                .and_then(Value::as_array)
                .map(Vec::len),
            Some(phrases),
            "case {case:?}: expected {phrases} phrase(s), got {reported:?}"
        );

        // Reporting an entity writes nothing, so a second read answers the same thing.
        let (status, again) = get_entity(&router, &library, "VOCABULARY", entity_id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: a second read must answer 200, got {status} with {again:?}"
        );
        assert_eq!(
            vocabulary(&again, case),
            reported,
            "case {case:?}: two reads must report one entity, got {again:?}"
        );
        let modified = timestamp_at(&reported, "lastModifiedTime", case);
        assert!(
            modified.0 > 0,
            "case {case:?}: the entity carries the instant its ingestion completed, got \
             {reported:?}"
        );
    }
}

/// RULE: a later `UPSERT` of the same id REPLACES what was stored under it, and a completed
/// `DELETE` takes it out — from which instant the read of the triple resolves to nothing.
#[tokio::test]
async fn an_upsert_replaces_and_a_delete_removes_the_entity() {
    let router = emulator();
    let library = create_library(&router, "entity-mutation-library").await;
    let entity_id = "vocab-replaced";

    submit(
        &router,
        &library,
        "the first upsert",
        &submission(
            "VOCABULARY",
            "UPSERT",
            json!({
                "upsertEntitiesInfo": [{
                    "vocabulary": {
                        "entityId": entity_id,
                        "language": "EN",
                        "phrases": [{ "text": "mockingbird" }],
                    },
                }],
            }),
        ),
    )
    .await;
    let first = poll_entity(
        &router,
        &library,
        "VOCABULARY",
        entity_id,
        200,
        "the first upsert",
    )
    .await;
    assert_eq!(
        vocabulary(&first, "the first upsert").get("language"),
        Some(&json!("EN")),
        "case \"the first upsert\": expected the language it carried, got {first:?}"
    );

    submit(
        &router,
        &library,
        "the second upsert",
        &submission(
            "VOCABULARY",
            "UPSERT",
            json!({
                "upsertEntitiesInfo": [{
                    "vocabulary": {
                        "entityId": entity_id,
                        "description": "revised",
                        "language": "FR",
                        "phrases": [{ "text": "rossignol" }, { "text": "merle" }],
                    },
                }],
            }),
        ),
    )
    .await;
    // The replacement is what the SECOND ingestion left, so the case waits for the member it
    // changed rather than for a length of time.
    let started = Instant::now();
    let replaced = loop {
        let (status, answer) = get_entity(&router, &library, "VOCABULARY", entity_id).await;
        let reported = vocabulary(&answer, "the second upsert");
        if status == 200 && reported.get("language") == Some(&json!("FR")) {
            break reported;
        }
        assert!(
            started.elapsed() < POLL_DEADLINE,
            "case \"the second upsert\": the replacement never landed; last answer was {answer:?}"
        );
        std::thread::sleep(POLL_INTERVAL);
    };
    assert_eq!(
        replaced.get("description"),
        Some(&json!("revised")),
        "case \"the second upsert\": expected the description it carried, got {replaced:?}"
    );
    assert_eq!(
        replaced.get("phrases").and_then(Value::as_array).map(Vec::len),
        Some(2),
        "case \"the second upsert\": expected the phrases it carried, got {replaced:?}"
    );

    submit(
        &router,
        &library,
        "the delete",
        &submission(
            "VOCABULARY",
            "DELETE",
            json!({ "deleteEntitiesInfo": { "entityIds": [entity_id] } }),
        ),
    )
    .await;
    let gone = poll_entity(
        &router,
        &library,
        "VOCABULARY",
        entity_id,
        404,
        "the delete",
    )
    .await;
    assert_eq!(
        error_code(&gone),
        "ResourceNotFoundException",
        "case \"the delete\": expected the modelled refusal, got {gone:?}"
    );
    assert_eq!(
        gone.get("message").and_then(Value::as_str),
        Some(state::library_entities::ENTITY_NOT_FOUND_MESSAGE),
        "case \"the delete\": expected this service's sentence for an unresolved entity, got \
         {gone:?}"
    );
}

/// RULE: the read resolves the whole triple, so everything that resolves to nothing is
/// refused — an unresolved library and an ARN of another Region on the library, and an id of
/// another library, of the other modelled entity type, or one nothing upserted on the entity.
///
/// The rows differ along WHICH part of the triple fails to resolve.
#[tokio::test]
async fn what_resolves_to_nothing_is_refused() {
    let router = emulator();
    let library = create_library(&router, "entity-refusal-library").await;
    let other = create_library(&router, "entity-refusal-other").await;
    let entity_id = "vocab-of-one-library";
    submit(
        &router,
        &other,
        "fixture",
        &submission(
            "VOCABULARY",
            "UPSERT",
            json!({
                "upsertEntitiesInfo": [{
                    "vocabulary": {
                        "entityId": entity_id,
                        "language": "EN",
                        "phrases": [{ "text": "kingfisher" }],
                    },
                }],
            }),
        ),
    )
    .await;
    // The fixture entity is there before the refusals are asserted, so "absent from the
    // other library" is not merely "not ingested yet".
    poll_entity(&router, &other, "VOCABULARY", entity_id, 200, "fixture").await;

    // (case, the library addressed, the entity type, the entity id, status, error code, the
    // message AWS answers)
    let rows: [(&str, &str, &str, &str, u16, &str, &str); 5] = [
        (
            "a library this store holds nothing under",
            UNKNOWN_LIBRARY_ARN,
            "VOCABULARY",
            entity_id,
            404,
            "ResourceNotFoundException",
            state::LIBRARY_ARN_NOT_FOUND_MESSAGE,
        ),
        (
            "a library of another Region",
            OTHER_REGION_LIBRARY_ARN,
            "VOCABULARY",
            entity_id,
            400,
            "ValidationException",
            state::library_arn::WRONG_REGION_MESSAGE,
        ),
        (
            "an entity of another library",
            &library,
            "VOCABULARY",
            entity_id,
            404,
            "ResourceNotFoundException",
            state::library_entities::ENTITY_NOT_FOUND_MESSAGE,
        ),
        (
            "the other modelled entity type",
            &other,
            "RECOGNIZED_PERSON",
            entity_id,
            404,
            "ResourceNotFoundException",
            state::library_entities::ENTITY_NOT_FOUND_MESSAGE,
        ),
        (
            "an id nothing has upserted",
            &other,
            "VOCABULARY",
            "no_such_entity",
            404,
            "ResourceNotFoundException",
            state::library_entities::ENTITY_NOT_FOUND_MESSAGE,
        ),
    ];
    for (case, library_arn, entity_type, id, status, code, message) in rows {
        let (actual, refused) = get_entity(&router, library_arn, entity_type, id).await;
        assert_eq!(
            actual, status,
            "case {case:?}: expected {status}, got {actual} with {refused:?}"
        );
        assert_eq!(
            error_code(&refused),
            code,
            "case {case:?}: expected {code}, got {refused:?}"
        );
        assert_eq!(
            refused.get("message").and_then(Value::as_str),
            Some(message),
            "case {case:?}: expected {message:?}, got {refused:?}"
        );
    }
}
