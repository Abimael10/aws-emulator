// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of the ORDER `ListDataAutomationLibraryEntities` serves one
//! (library, entity type) pair in, of the ids it serves entities an upsert did not name
//! under, and of the library the listing is OF.
//!
//! RULE. The listing enumerates the fold of the ingestion jobs of one (library, entity type)
//! pair whose completion the clock has made due, ordered by the instant each entity was last
//! written with the `entityId` breaking the tie between the entities one job wrote, and every
//! page of one listing follows that same order — so an entity a later ingestion wrote follows
//! the entities written before it, an upsert that rewrites an entity moves it behind them, an
//! entity an upsert named no `entityId` for is enumerated under an id this service minted
//! inside the modelled `EntityId` constraints, and the listing resolves its `libraryArn`
//! through the same lookup every other operation of this service takes, so from the instant
//! the removal a `DeleteDataAutomationLibrary` owes falls due it refuses with this service's
//! sentence for an unresolved library instead of enumerating anything.
//!
//! Every case drives the real `restJson1` wire path — `MultiServiceRouter::dispatch` with a
//! signed-looking request — so routing, HTTP binding, constraint validation, the handler and
//! response serialization are all exercised, exactly as an AWS SDK `bedrock-data-automation`
//! client would.
//!
//! The cases here are about what spans MORE than one ingestion: entities written by different
//! jobs (and so carrying different instants), a rewrite of one of them, the two kinds of
//! `entityId` an upsert can leave behind, and the pages the collection is walked in. Nothing
//! waits for a length of time: each case polls for the CONDITION it is waiting on.

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

/// How long a case waits for the condition it polls for: an ingestion's completion, and a
/// library's removal, fall due a span after the call that owes them, and this budget is
/// deliberately far wider than either span, so a loaded host only makes the loop go round
/// more often.
const POLL_DEADLINE: Duration = Duration::from_secs(120);

/// How long the poll loop parks between looks. The runtime this workspace enables carries no
/// timer, so the test thread parks; a test may wait for a due instant, a handler may not.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The characters the modelled `EntityId` admits
/// (`smithy_query(query="shape",shape="EntityId")`: `@length(min: 1, max: 128)`,
/// `^[a-zA-Z0-9-_]+$`), which an id this service mints for an unnamed entity stays inside.
fn is_modelled_entity_id(value: &str) -> bool {
    (1..=128).contains(&value.chars().count())
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

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

/// One library's own path, as `DeleteDataAutomationLibrary` (DELETE) binds it.
fn library_uri(library_arn: &str) -> String {
    format!("/data-automation-libraries/{}/", encode_label(library_arn))
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

/// The entity COLLECTION path, per `ListDataAutomationLibraryEntities`'s modelled `@http`
/// `{method: POST, uri:
/// /data-automation-libraries/{libraryArn}/entityType/{entityType}/entities/}`.
fn entities_uri(library_arn: &str, entity_type: &str) -> String {
    format!(
        "/data-automation-libraries/{}/entityType/{}/entities/",
        encode_label(library_arn),
        encode_label(entity_type)
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

/// One upsert submission carrying the entities the case describes: the `entityId` (absent
/// when the case names none), the language and how many phrases it holds.
fn upsert_submission(entity_type: &str, entities: &[(Option<&str>, &str, usize)]) -> Value {
    let entries: Vec<Value> = entities
        .iter()
        .map(|(entity_id, language, phrases)| {
            let texts: Vec<Value> = (0..*phrases)
                .map(|index| json!({ "text": format!("{language}-phrase-{index}") }))
                .collect();
            let mut info = json!({ "language": language, "phrases": texts });
            if let Some(id) = entity_id {
                info["entityId"] = json!(id);
            }
            json!({ "vocabulary": info })
        })
        .collect();
    submission(
        entity_type,
        "UPSERT",
        json!({ "upsertEntitiesInfo": entries }),
    )
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

/// `ListDataAutomationLibraryEntities` with the page the case asks for.
async fn list_entities(
    router: &MultiServiceRouter,
    library_arn: &str,
    entity_type: &str,
    page: &Value,
) -> (u16, Value) {
    call(router, "POST", &entities_uri(library_arn, entity_type), page).await
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

/// The `vocabulary` summaries a listing answered, in the order it served them.
fn listed(payload: &Value, case: &str) -> Vec<Value> {
    payload
        .get("entities")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("case {case:?}: no entities array in {payload:?}"))
        .iter()
        .map(|item| {
            item.get("vocabulary")
                .cloned()
                .unwrap_or_else(|| panic!("case {case:?}: no vocabulary summary in {item:?}"))
        })
        .collect()
}

/// The `entityId`s a listing served, in that order.
fn listed_ids(payload: &Value, case: &str) -> Vec<String> {
    listed(payload, case)
        .iter()
        .map(|summary| string_at(summary, "entityId", case))
        .collect()
}

/// Look at the whole listing until what it answered satisfies `holds`, or the generous
/// [`POLL_DEADLINE`] passes.
///
/// A submission's ingestion falls due a span after the call, so a case waits for the
/// CONDITION (the listing holds what the ingestion left) rather than for a length of time.
async fn poll_listing<F>(
    router: &MultiServiceRouter,
    library_arn: &str,
    entity_type: &str,
    case: &str,
    waiting_for: &str,
    holds: F,
) -> Value
where
    F: Fn(&Value) -> bool,
{
    let started = Instant::now();
    let mut last = (0u16, Value::Null);
    while started.elapsed() < POLL_DEADLINE {
        let answer = list_entities(router, library_arn, entity_type, &json!({})).await;
        if answer.0 == 200 && holds(&answer.1) {
            return answer.1;
        }
        last = answer;
        std::thread::sleep(POLL_INTERVAL);
    }
    panic!("case {case:?}: the listing never held {waiting_for}; last answer was {last:?}")
}

/// Assert that the summaries a listing served run from the earliest-written entity to the
/// latest, and that no entity was served twice — read off the instants the ANSWER reports, so
/// the order is checked against what the response itself says rather than against a recorded
/// value. Which entity carries which instant is what each case's expected id order pins.
fn assert_in_listing_order(served: &[Value], case: &str) {
    let keys: Vec<((i64, u32), String)> = served
        .iter()
        .map(|summary| {
            (
                timestamp_at(summary, "lastModifiedTime", case),
                string_at(summary, "entityId", case),
            )
        })
        .collect();
    for pair in keys.windows(2) {
        assert!(
            pair[0].0 <= pair[1].0,
            "case {case:?}: expected the listing ordered by lastModifiedTime, got {:?} before \
             {:?}",
            pair[0],
            pair[1]
        );
    }
    for (index, (_, entity_id)) in keys.iter().enumerate() {
        assert!(
            !keys[..index].iter().any(|(_, seen)| seen == entity_id),
            "case {case:?}: expected each entity served once, got {entity_id:?} twice in {keys:?}"
        );
    }
}

/// Walk every page of one listing with the page size the case names, and return the ids in
/// the order they were served.
async fn walk_listing(
    router: &MultiServiceRouter,
    library_arn: &str,
    entity_type: &str,
    max_results: Option<i32>,
    case: &str,
) -> Vec<String> {
    let mut ids: Vec<String> = Vec::new();
    let mut token: Option<String> = None;
    let mut pages = 0usize;
    loop {
        let mut page = json!({});
        if let Some(limit) = max_results {
            page["maxResults"] = json!(limit);
        }
        if let Some(cursor) = token.as_ref() {
            page["nextToken"] = json!(cursor);
        }
        let (status, payload) = list_entities(router, library_arn, entity_type, &page).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected ListDataAutomationLibraryEntities 200, got {status} with \
             {payload:?}"
        );
        let served = listed(&payload, case);
        if let Some(limit) = max_results {
            assert!(
                served.len() <= limit as usize,
                "case {case:?}: a page must carry at most {limit} item(s), got {payload:?}"
            );
        }
        assert_in_listing_order(&served, case);
        ids.extend(listed_ids(&payload, case));
        pages += 1;
        token = payload
            .get("nextToken")
            .and_then(Value::as_str)
            .map(str::to_string);
        if token.is_none() {
            return ids;
        }
        assert!(
            pages <= 64,
            "case {case:?}: the listing handed back a token on {pages} pages for {} id(s)",
            ids.len()
        );
    }
}

/// The summary a listing served for one `entityId`, or a panic naming the case.
fn summary_of(payload: &Value, entity_id: &str, case: &str) -> Value {
    listed(payload, case)
        .into_iter()
        .find(|summary| string_at(summary, "entityId", case) == entity_id)
        .unwrap_or_else(|| panic!("case {case:?}: {entity_id:?} is not in {payload:?}"))
}

/// RULE: the listing of a (library, entity type) pair is ordered by the instant each entity
/// was last written, the `entityId` breaking the tie between the entities ONE ingestion
/// wrote — so an entity a later ingestion wrote follows the ones written before it whatever
/// its id sorts as, an upsert that rewrites an entity moves it behind them, and every page of
/// the listing follows that same order.
///
/// The ids here deliberately run AGAINST the order the ingestions wrote them in (`alpha-1` is
/// ingested last), so an order taken from the ids alone could not pass. The pages span one at
/// a time, a size that does not divide the collection, a size equal to it and no `maxResults`
/// at all.
#[tokio::test]
async fn the_listing_follows_the_instant_each_entity_was_last_written() {
    let router = emulator();
    let library = create_library(&router, "entity-order-library").await;

    // Two entities of ONE job, then a third of a job submitted after it: the first two share
    // their job's completion instant, the third carries a later one.
    submit(
        &router,
        &library,
        "the first ingestion",
        &upsert_submission(
            "VOCABULARY",
            &[(Some("zeta-1"), "EN", 1), (Some("zeta-2"), "DE", 2)],
        ),
    )
    .await;
    submit(
        &router,
        &library,
        "the later ingestion",
        &upsert_submission("VOCABULARY", &[(Some("alpha-1"), "ES", 3)]),
    )
    .await;

    let case = "two ingestions";
    let answer = poll_listing(
        &router,
        &library,
        "VOCABULARY",
        case,
        "the three entities the two ingestions left",
        |payload| listed_ids(payload, case).len() == 3,
    )
    .await;
    assert_eq!(
        listed_ids(&answer, case),
        vec![
            "zeta-1".to_string(),
            "zeta-2".to_string(),
            "alpha-1".to_string()
        ],
        "case {case:?}: expected the later ingestion's entity last, got {answer:?}"
    );
    assert_in_listing_order(&listed(&answer, case), case);

    // A rewrite of the FIRST entity: the upsert replaces what was stored under that id, so
    // the entity is now the most recently written one and the listing serves it last.
    submit(
        &router,
        &library,
        "the rewrite",
        &upsert_submission("VOCABULARY", &[(Some("zeta-1"), "FR", 4)]),
    )
    .await;
    let case = "a rewritten entity";
    let rewritten = poll_listing(
        &router,
        &library,
        "VOCABULARY",
        case,
        "the rewrite of zeta-1",
        |payload| {
            listed_ids(payload, case).len() == 3
                && summary_of(payload, "zeta-1", case).get("language") == Some(&json!("FR"))
        },
    )
    .await;
    assert_eq!(
        listed_ids(&rewritten, case),
        vec![
            "zeta-2".to_string(),
            "alpha-1".to_string(),
            "zeta-1".to_string()
        ],
        "case {case:?}: expected the rewritten entity last, got {rewritten:?}"
    );
    assert_in_listing_order(&listed(&rewritten, case), case);
    assert_eq!(
        summary_of(&rewritten, "zeta-1", case)
            .get("numOfPhrases")
            .and_then(Value::as_i64),
        Some(4),
        "case {case:?}: expected the rewrite's phrase count, got {rewritten:?}"
    );

    let order = listed_ids(&rewritten, case);
    // (case, the maxResults asked for)
    let pages: [(&str, Option<i32>); 4] = [
        ("one at a time", Some(1)),
        ("a size that does not divide the collection", Some(2)),
        ("a size equal to the collection", Some(3)),
        ("no maxResults at all", None),
    ];
    for (case, max_results) in pages {
        let served = walk_listing(&router, &library, "VOCABULARY", max_results, case).await;
        assert_eq!(
            served, order,
            "case {case:?}: the pages must serve the listing's order exactly once, got {served:?}"
        );
    }
}

/// RULE: `VocabularyEntityInfo$entityId` is OPTIONAL
/// (`smithy_query(query="shape",shape="VocabularyEntityInfo")`), so an upsert can describe an
/// entity without naming one — and such an entity is enumerated, and resolvable, under an id
/// this service minted for it: one inside the modelled `EntityId` constraints, one per
/// described entity, and the same id however often the listing is asked.
///
/// The entries differ along whether they name an `entityId`, whether they carry a
/// description, the language named and how many phrases they hold, so the listing's summary
/// of each is tied to the entry it came from.
#[tokio::test]
async fn an_upsert_that_names_no_entity_id_is_listed_under_a_minted_id() {
    let router = emulator();
    let library = create_library(&router, "entity-minted-id-library").await;
    let case = "an upsert of named and unnamed entities";

    submit(
        &router,
        &library,
        case,
        &submission(
            "VOCABULARY",
            "UPSERT",
            json!({
                "upsertEntitiesInfo": [
                    { "vocabulary": {
                        "entityId": "named-entity",
                        "language": "EN",
                        "phrases": [{ "text": "mockingbird" }],
                    }},
                    { "vocabulary": {
                        "language": "ES",
                        "phrases": [{ "text": "ruisenor" }, { "text": "mirlo" }],
                    }},
                    { "vocabulary": {
                        "description": "described, and named by the service",
                        "language": "IT",
                        "phrases": [
                            { "text": "rondine" },
                            { "text": "merlo" },
                            { "text": "martin-pescatore" },
                        ],
                    }},
                ],
            }),
        ),
    )
    .await;

    let answer = poll_listing(
        &router,
        &library,
        "VOCABULARY",
        case,
        "the three entities the upsert described",
        |payload| listed_ids(payload, case).len() == 3,
    )
    .await;

    // (the language the entry carried, the id expected, the description, the phrase count)
    let rows: [(&str, Option<&str>, Option<&str>, i64); 3] = [
        ("EN", Some("named-entity"), None, 1),
        ("ES", None, None, 2),
        ("IT", None, Some("described, and named by the service"), 3),
    ];
    let mut minted: Vec<String> = Vec::new();
    for (language, named, description, phrases) in rows {
        let summary = listed(&answer, case)
            .into_iter()
            .find(|summary| summary.get("language") == Some(&json!(language)))
            .unwrap_or_else(|| {
                panic!("case {case:?}: no entity of language {language:?} in {answer:?}")
            });
        let entity_id = string_at(&summary, "entityId", case);
        match named {
            Some(expected) => assert_eq!(
                entity_id, expected,
                "case {case:?}: expected the id the upsert named, got {summary:?}"
            ),
            None => {
                assert!(
                    is_modelled_entity_id(&entity_id),
                    "case {case:?}: expected a minted id inside the modelled EntityId \
                     constraints, got {entity_id:?}"
                );
                assert!(
                    !minted.contains(&entity_id),
                    "case {case:?}: expected one id per described entity, got {entity_id:?} \
                     among {minted:?}"
                );
                minted.push(entity_id.clone());
            }
        }
        assert_eq!(
            summary.get("description").and_then(Value::as_str),
            description,
            "case {case:?}: expected the description the entry carried, got {summary:?}"
        );
        assert_eq!(
            summary.get("numOfPhrases").and_then(Value::as_i64),
            Some(phrases),
            "case {case:?}: expected {phrases} phrase(s) counted, got {summary:?}"
        );

        // Every id the listing serves resolves the same entity through the read of one.
        let (status, read) = get_entity(&router, &library, "VOCABULARY", &entity_id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected every listed id to resolve, got {status} with {read:?} for \
             {entity_id:?}"
        );
        let reported = read
            .get("entity")
            .and_then(|entity| entity.get("vocabulary"))
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: no entity.vocabulary in {read:?}"));
        assert_eq!(
            reported
                .get("phrases")
                .and_then(Value::as_array)
                .map(Vec::len),
            Some(phrases as usize),
            "case {case:?}: the read and the listing must agree about {entity_id:?}, got \
             {reported:?}"
        );
    }
    assert_eq!(
        minted.len(),
        2,
        "case {case:?}: expected an id minted for each unnamed entity, got {minted:?}"
    );

    // Enumerating writes nothing, so the ids a second listing serves are the same ones —
    // including the minted ones, which are derived rather than drawn afresh.
    let (status, again) = list_entities(&router, &library, "VOCABULARY", &json!({})).await;
    assert_eq!(
        status, 200,
        "case {case:?}: a second listing must answer 200, got {status} with {again:?}"
    );
    assert_eq!(
        listed_ids(&again, case),
        listed_ids(&answer, case),
        "case {case:?}: two listings must serve one set of ids, got {again:?}"
    );
}

/// RULE: the listing is OF one library, resolved through the same lookup every other
/// operation of this service takes — so from the instant the removal a
/// `DeleteDataAutomationLibrary` owes falls due, the listing refuses with the modelled
/// `ResourceNotFoundException` (HTTP 404) and this service's sentence for an unresolved
/// library, instead of enumerating the entities that library held.
#[tokio::test]
async fn a_settled_library_delete_leaves_the_listing_refusing() {
    let router = emulator();
    let library = create_library(&router, "entity-released-library").await;
    let case = "a library whose delete has settled";

    submit(
        &router,
        &library,
        case,
        &upsert_submission("VOCABULARY", &[(Some("vocab-of-a-doomed-library"), "PT", 2)]),
    )
    .await;
    let held = poll_listing(
        &router,
        &library,
        "VOCABULARY",
        case,
        "the entity the ingestion left",
        |payload| listed_ids(payload, case).len() == 1,
    )
    .await;
    assert_eq!(
        listed_ids(&held, case),
        vec!["vocab-of-a-doomed-library".to_string()],
        "case {case:?}: expected the ingested entity, got {held:?}"
    );

    let (status, accepted) = call(&router, "DELETE", &library_uri(&library), &json!({})).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected DeleteDataAutomationLibrary 200, got {status} with {accepted:?}"
    );

    // The removal falls due a span after that call, so the case polls for the refusal rather
    // than for a length of time.
    let started = Instant::now();
    let mut last = (0u16, Value::Null);
    while started.elapsed() < POLL_DEADLINE {
        let answer = list_entities(&router, &library, "VOCABULARY", &json!({})).await;
        if answer.0 == 404 {
            assert_eq!(
                error_code(&answer.1),
                "ResourceNotFoundException",
                "case {case:?}: expected the modelled refusal, got {:?}",
                answer.1
            );
            assert_eq!(
                answer.1.get("message").and_then(Value::as_str),
                Some(state::LIBRARY_ARN_NOT_FOUND_MESSAGE),
                "case {case:?}: expected this service's sentence for an unresolved library, got \
                 {:?}",
                answer.1
            );
            assert!(
                answer.1.get("entities").is_none(),
                "case {case:?}: a refusal enumerates nothing, got {:?}",
                answer.1
            );
            return;
        }
        last = answer;
        std::thread::sleep(POLL_INTERVAL);
    }
    panic!("case {case:?}: the listing never refused the released library; last answer was {last:?}")
}
