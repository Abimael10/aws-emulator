// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the AgentCore Memory long-term record surface:
//! `BatchCreateMemoryRecords`, `GetMemoryRecord` and `DeleteMemoryRecord`.
//!
//! The rules under test:
//!
//! * a batch create stores one record per entry the request listed, under a server-minted
//!   `memoryRecordId`, with the content, namespaces, timestamp, metadata and strategy
//!   association that entry carried, and reports one `SUCCEEDED` outcome per entry
//!   tracked by its `requestIdentifier`; a record expiry stated in days gives every
//!   record of the request an `expiresAt` that many days after its creation, and a
//!   request that states none (or the `REMOVE` action, which "does not apply when
//!   creating records") creates records that do not expire;
//! * a read reports a stored record from what was stored, and a read of a record this
//!   memory does not hold is refused `ResourceNotFoundException` (404);
//! * a delete removes the ONE record its `memoryRecordId` addresses and leaves every
//!   other record of that memory container readable;
//! * a replayed `clientToken` answers with the records the first batch created and
//!   stores no second set.
//! * a LISTING reports the container's stored records — narrowed by the namespace scope,
//!   the strategy association and the metadata expressions the request carries, every
//!   filter narrowing together — most recently created first, in pages of `maxResults`,
//!   and reports no record of another container and none it has deleted.
//!
//! Every case drives real HTTP through the same `MultiServiceRouter` the emulator
//! serves with, so the wire shape (routes, status codes, `restJson1` bodies, modelled
//! error codes) is exercised, not just the handler functions.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;
use std::sync::Arc;

use service_com_amazonaws_bedrockagentcore_amazonbedrockagentcore as agentcore;

/// A memory id matching `MemoryId`'s pattern
/// `^(arn:...:memory/)?[a-zA-Z][a-zA-Z0-9-_]{0,99}-[a-zA-Z0-9]{10}$`.
const MEMORY: &str = "record-memory-0123456789";
/// The same memory addressed by its ARN spelling, which that same pattern admits.
const MEMORY_ARN: &str =
    "arn:aws:bedrock-agentcore:us-east-1:123456789012:memory/record-memory-0123456789";
/// A second, unrelated memory container.
const OTHER_MEMORY: &str = "other-memory-9876543210";

/// Seconds in one day, the unit `recordExpiryDurationDays` states an expiry in.
const SECONDS_PER_DAY: f64 = 86_400.0;

/// The strategy association this emulator reports for a record created with no
/// `memoryStrategyId` (`crate::state::CALLER_MANAGED_MEMORY_STRATEGY_ID`).
const CALLER_MANAGED_STRATEGY: &str = "CUSTOM";

/// The emulator, with request authentication off so a test signs nothing.
///
/// The memory container this suite writes records into is created first, through the
/// shared inventory the CONTROL plane's `CreateMemory` writes: a write into a container
/// that was never created is refused.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    super::harness::create_memory(&engine, MEMORY);
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(agentcore::EmulatedService::new(engine));
    router
}

/// Send one request and return its status and parsed JSON body (`Value::Null` for an
/// empty body). The SigV4 credential scope is what routes the request to this service.
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

/// Percent-encode one `@httpLabel` value, the way an AWS SDK does, so a memory ARN's
/// `/` reaches the server as part of the label rather than as URI syntax.
fn label(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

async fn batch_create(router: &MultiServiceRouter, memory: &str, body: Value) -> (u16, Value) {
    send(
        router,
        "POST",
        &format!("/memories/{}/memoryRecords/batchCreate", label(memory)),
        Some(body),
    )
    .await
}

async fn get_record(router: &MultiServiceRouter, memory: &str, record: &str) -> (u16, Value) {
    send(
        router,
        "GET",
        &format!(
            "/memories/{}/memoryRecord/{}",
            label(memory),
            label(record)
        ),
        None,
    )
    .await
}

async fn delete_record(router: &MultiServiceRouter, memory: &str, record: &str) -> (u16, Value) {
    send(
        router,
        "DELETE",
        &format!(
            "/memories/{}/memoryRecords/{}",
            label(memory),
            label(record)
        ),
        None,
    )
    .await
}

fn error_code(body: &Value) -> &str {
    body.get("__type").and_then(Value::as_str).unwrap_or("")
}

/// `MemoryRecordId`'s modelled pattern `^mem-[a-zA-Z0-9-_]*$` with its 40..50 length,
/// checked without a regex dependency.
fn is_modelled_record_id(id: &str) -> bool {
    (40..=50).contains(&id.len())
        && id.starts_with("mem-")
        && id[4..]
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// One record-creation entry. `requestIdentifier` matches `^[a-zA-Z0-9_-]+$`, and
/// `namespaces` holds at most one namespace (`NamespacesList` @length max 1).
fn entry(request_identifier: &str, text: &str, namespace: &str) -> Value {
    json!({
        "requestIdentifier": request_identifier,
        "namespaces": [namespace],
        "content": {"text": text},
        "timestamp": 1_700_000_000,
    })
}

/// A record the server reports back, read through `GetMemoryRecord`.
async fn stored_record(router: &MultiServiceRouter, memory: &str, id: &str) -> Value {
    let (status, body) = get_record(router, memory, id).await;
    assert_eq!(
        status, 200,
        "reading back {id:?}: expected HTTP 200 from GetMemoryRecord, got {status} with {body:?}"
    );
    body["memoryRecord"].clone()
}

/// A batch create stores every entry it was given and a later `GetMemoryRecord` reports
/// each one from what was stored: the content, the namespaces, the timestamp, the
/// metadata and the strategy association the entry carried.
///
/// Rows differ along the entry count, the `MemoryRecordMetadataValue` union member
/// (source: smithy_query shape="MemoryRecordMetadataValue": stringValue |
/// stringListValue | numberValue | dateTimeValue), whether the entry named a
/// `memoryStrategyId`, the namespace form and the content size.
#[tokio::test]
async fn created_memory_records_round_trip_through_get_memory_record() {
    let router = emulator();

    let cases: Vec<(&str, Value)> = vec![
        ("one entry, no strategy, no metadata", json!([
            entry("req-1", "the user prefers window seats", "support/facts/actor-1"),
        ])),
        ("two entries, one strategy each", json!([
            {
                "requestIdentifier": "req-2a",
                "namespaces": ["support/preferences/actor-1"],
                "content": {"text": "likes aisle seats"},
                "timestamp": 1_700_000_100,
                "memoryStrategyId": "userPreference-abc123",
            },
            {
                "requestIdentifier": "req-2b",
                "namespaces": ["support/facts/actor-2"],
                "content": {"text": "lives in Berlin"},
                "timestamp": 1_700_000_200,
                "memoryStrategyId": "semantic_strategy-9",
            },
        ])),
        ("metadata: every union member, one per entry", json!([
            {
                "requestIdentifier": "req-3a",
                "namespaces": ["support/facts/actor-3"],
                "content": {"text": "string metadata"},
                "timestamp": 1_700_000_300,
                "metadata": {"topic": {"stringValue": "travel"}},
            },
            {
                "requestIdentifier": "req-3b",
                "namespaces": ["support/facts/actor-3"],
                "content": {"text": "string-list metadata"},
                "timestamp": 1_700_000_301,
                "metadata": {"tags": {"stringListValue": ["travel", "seats", "window"]}},
            },
            {
                "requestIdentifier": "req-3c",
                "namespaces": ["support/facts/actor-3"],
                "content": {"text": "number metadata"},
                "timestamp": 1_700_000_302,
                "metadata": {"confidence": {"numberValue": 0.5}},
            },
            {
                "requestIdentifier": "req-3d",
                "namespaces": ["support/facts/actor-3"],
                "content": {"text": "date-time metadata"},
                "timestamp": 1_700_000_303,
                "metadata": {"observedAt": {"dateTimeValue": 1_699_999_999}},
            },
        ])),
        ("a long content and a wildcard namespace", json!([
            {
                "requestIdentifier": "req_4",
                "namespaces": ["*"],
                "content": {"text": "x".repeat(4_000)},
                "timestamp": 1_700_000_400,
                "metadata": {
                    "k1": {"stringValue": "v1"},
                    "k2": {"numberValue": 42.0},
                },
            },
        ])),
    ];

    for (case, records) in cases {
        let (status, created) = batch_create(
            &router,
            MEMORY,
            json!({"records": records.clone()}),
        )
        .await;
        assert_eq!(
            status, 201,
            "case {case:?}: expected HTTP 201 from BatchCreateMemoryRecords, got {status} with \
             {created:?}"
        );
        let successful = created["successfulRecords"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let entries = records.as_array().expect("the case's entries").clone();
        assert_eq!(
            successful.len(),
            entries.len(),
            "case {case:?}: expected one successful record per entry ({}), got {:?}",
            entries.len(),
            created["successfulRecords"]
        );
        assert_eq!(
            created["failedRecords"],
            json!([]),
            "case {case:?}: expected no failed records, got {:?}",
            created["failedRecords"]
        );

        for (outcome, sent) in successful.iter().zip(entries.iter()) {
            let id = outcome["memoryRecordId"].as_str().unwrap_or_default();
            assert!(
                is_modelled_record_id(id),
                "case {case:?}: expected a server-minted memoryRecordId matching \
                 ^mem-[a-zA-Z0-9-_]*$ with length 40..50, got {id:?}"
            );
            assert_eq!(
                outcome["status"], "SUCCEEDED",
                "case {case:?}: expected status SUCCEEDED for {id:?}, got {:?}",
                outcome["status"]
            );
            assert_eq!(
                outcome["requestIdentifier"], sent["requestIdentifier"],
                "case {case:?}: expected the entry's requestIdentifier back, got {:?}",
                outcome["requestIdentifier"]
            );

            let record = stored_record(&router, MEMORY, id).await;
            assert_eq!(
                record["memoryRecordId"], outcome["memoryRecordId"],
                "case {case:?}: expected the read to report the created id, got {:?}",
                record["memoryRecordId"]
            );
            assert_eq!(
                record["content"], sent["content"],
                "case {case:?}: expected the stored content back unchanged, got {:?}",
                record["content"]
            );
            assert_eq!(
                record["namespaces"], sent["namespaces"],
                "case {case:?}: expected the stored namespaces back unchanged, got {:?}",
                record["namespaces"]
            );
            assert_eq!(
                record["timestamp"], sent["timestamp"],
                "case {case:?}: expected the entry's timestamp back, got {:?}",
                record["timestamp"]
            );
            assert_eq!(
                record["metadata"],
                sent.get("metadata").cloned().unwrap_or(Value::Null),
                "case {case:?}: expected the stored metadata back unchanged, got {:?}",
                record["metadata"]
            );
            let expected_strategy = sent
                .get("memoryStrategyId")
                .and_then(Value::as_str)
                .unwrap_or(CALLER_MANAGED_STRATEGY);
            assert_eq!(
                record["memoryStrategyId"], expected_strategy,
                "case {case:?}: expected memoryStrategyId {expected_strategy:?}, got {:?}",
                record["memoryStrategyId"]
            );
            assert!(
                record["createdAt"].as_f64().is_some_and(|at| at > 0.0),
                "case {case:?}: expected a createdAt instant, got {:?}",
                record["createdAt"]
            );
            assert_eq!(
                record["expiresAt"],
                Value::Null,
                "case {case:?}: a record created with no record expiry does not expire, got {:?}",
                record["expiresAt"]
            );
        }
    }
}

/// `recordExpirySpecification` decides the records' `expiresAt`: a duration in days
/// gives every record of the request an expiry that many days after it was created,
/// while the `REMOVE` action — which "does not apply when creating records" — and an
/// absent specification leave the records without one.
///
/// Rows cover the published minimum of `recordExpiryDurationDays` (31), a value above
/// it, the one other union member (`action: REMOVE`, the sole `RecordExpiryAction`
/// value) and the absent specification.
#[tokio::test]
async fn a_record_expiry_in_days_sets_each_record_s_expiry() {
    let router = emulator();

    let cases: Vec<(&str, Value, Option<f64>)> = vec![
        ("no specification", Value::Null, None),
        ("the minimum, 31 days", json!({"recordExpiryDurationDays": 31}), Some(31.0)),
        ("90 days", json!({"recordExpiryDurationDays": 90}), Some(90.0)),
        ("3650 days", json!({"recordExpiryDurationDays": 3650}), Some(3650.0)),
        ("the REMOVE action", json!({"action": "REMOVE"}), None),
    ];

    for (case, specification, expected_days) in cases {
        let mut request = json!({
            "records": [entry("expiry-probe", "remember this", "support/facts/actor-1")],
        });
        if !specification.is_null() {
            request["recordExpirySpecification"] = specification.clone();
        }

        let (status, created) = batch_create(&router, MEMORY, request).await;
        assert_eq!(
            status, 201,
            "case {case:?}: expected HTTP 201 from BatchCreateMemoryRecords, got {status} with \
             {created:?}"
        );
        let id = created["successfulRecords"][0]["memoryRecordId"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let record = stored_record(&router, MEMORY, &id).await;
        let created_at = record["createdAt"].as_f64().unwrap_or_default();

        match expected_days {
            None => assert_eq!(
                record["expiresAt"],
                Value::Null,
                "case {case:?}: expected no expiry on the record, got {:?}",
                record["expiresAt"]
            ),
            Some(days) => {
                let expires_at = record["expiresAt"].as_f64().unwrap_or_default();
                let expected = created_at.floor() + days * SECONDS_PER_DAY;
                assert!(
                    (expires_at - expected).abs() < 1.0,
                    "case {case:?}: expected an expiry {days} days after createdAt \
                     ({expected}), got {expires_at}"
                );
            }
        }
    }
}

/// A delete removes the ONE record its `memoryRecordId` addresses: that record is gone
/// (a later read is refused 404) and every other record created beside it is still
/// readable, whatever the batch size and whichever position the deleted record held.
#[tokio::test]
async fn deleting_one_memory_record_leaves_every_other_record() {
    // Rows differ along the batch size and the position of the record deleted from it.
    let cases: Vec<(&str, usize, usize)> = vec![
        ("two records, delete the first", 2, 0),
        ("two records, delete the second", 2, 1),
        ("three records, delete the middle one", 3, 1),
        ("five records, delete the last", 5, 4),
        ("one record, delete it", 1, 0),
    ];

    for (case, count, position) in cases {
        let router = emulator();
        let records: Vec<Value> = (0..count)
            .map(|index| {
                entry(
                    &format!("req-{index}"),
                    &format!("fact number {index}"),
                    &format!("support/facts/actor-{index}"),
                )
            })
            .collect();
        let (status, created) = batch_create(&router, MEMORY, json!({"records": records})).await;
        assert_eq!(
            status, 201,
            "case {case:?}: expected HTTP 201 from BatchCreateMemoryRecords, got {status} with \
             {created:?}"
        );
        let ids: Vec<String> = created["successfulRecords"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|outcome| {
                outcome["memoryRecordId"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string()
            })
            .collect();
        assert_eq!(
            ids.len(),
            count,
            "case {case:?}: expected {count} created ids, got {ids:?}"
        );
        let mut unique = ids.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(
            unique.len(),
            ids.len(),
            "case {case:?}: expected every minted memoryRecordId to differ, got {ids:?}"
        );

        let removed = &ids[position];
        let (status, deleted) = delete_record(&router, MEMORY, removed).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DeleteMemoryRecord, got {status} with \
             {deleted:?}"
        );
        assert_eq!(
            deleted["memoryRecordId"], *removed,
            "case {case:?}: expected the deleted id back, got {:?}",
            deleted["memoryRecordId"]
        );

        let (status, body) = get_record(&router, MEMORY, removed).await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected HTTP 404 reading the deleted record, got {status} with \
             {body:?}"
        );
        assert_eq!(
            error_code(&body),
            "ResourceNotFoundException",
            "case {case:?}: expected ResourceNotFoundException reading the deleted record, got \
             {body:?}"
        );
        assert_eq!(
            body["message"], "The memory record was not found",
            "case {case:?}: expected the modelled not-found message, got {:?}",
            body["message"]
        );

        // A second delete of the same record has nothing left to remove.
        let (status, body) = delete_record(&router, MEMORY, removed).await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected HTTP 404 deleting the record twice, got {status} with \
             {body:?}"
        );

        for (index, id) in ids.iter().enumerate() {
            if index == position {
                continue;
            }
            let record = stored_record(&router, MEMORY, id).await;
            assert_eq!(
                record["content"],
                json!({"text": format!("fact number {index}")}),
                "case {case:?}: expected record {index} to survive the delete unchanged, got {record:?}"
            );
        }
    }
}

/// A record is addressed inside its own memory container: the id another container
/// minted is not found there, and the memory's ARN spelling addresses the same
/// container as its bare id.
#[tokio::test]
async fn a_memory_record_is_scoped_to_its_memory_container() {
    let router = emulator();

    let (status, created) = batch_create(
        &router,
        MEMORY,
        json!({"records": [entry("scoped", "a fact", "support/facts/actor-1")]}),
    )
    .await;
    assert_eq!(
        status, 201,
        "case \"create in the first memory\": expected HTTP 201, got {status} with {created:?}"
    );
    let id = created["successfulRecords"][0]["memoryRecordId"]
        .as_str()
        .unwrap_or_default()
        .to_string();

    // The ARN spelling of the same memory addresses the same container.
    let (status, body) = get_record(&router, MEMORY_ARN, &id).await;
    assert_eq!(
        status, 200,
        "case \"read through the memory ARN\": expected HTTP 200, got {status} with {body:?}"
    );
    assert_eq!(
        body["memoryRecord"]["memoryRecordId"], id,
        "case \"read through the memory ARN\": expected the same record, got {body:?}"
    );

    // A different container holds nothing under that id.
    let (status, body) = get_record(&router, OTHER_MEMORY, &id).await;
    assert_eq!(
        status, 404,
        "case \"read from another memory\": expected HTTP 404, got {status} with {body:?}"
    );
    let (status, body) = delete_record(&router, OTHER_MEMORY, &id).await;
    assert_eq!(
        status, 404,
        "case \"delete from another memory\": expected HTTP 404, got {status} with {body:?}"
    );

    // And an id nothing ever created is not found either.
    let unknown = format!("mem-{}", "0".repeat(40));
    for (case, status) in [
        ("read", get_record(&router, MEMORY, &unknown).await),
        ("delete", delete_record(&router, MEMORY, &unknown).await),
    ] {
        assert_eq!(
            status.0, 404,
            "case {case:?} of an id nothing created: expected HTTP 404, got {:?}",
            status
        );
        assert_eq!(
            error_code(&status.1),
            "ResourceNotFoundException",
            "case {case:?} of an id nothing created: expected ResourceNotFoundException, got {:?}",
            status.1
        );
    }
}

/// `BatchCreateMemoryRecords` is `@idempotent` with an `@idempotencyToken`: replaying a
/// token answers with the records that token already created and stores no second set,
/// while a different token creates new records.
#[tokio::test]
async fn a_replayed_client_token_creates_no_second_set_of_records() {
    let router = emulator();

    let records = json!([
        entry("token-a", "first fact", "support/facts/actor-1"),
        entry("token-b", "second fact", "support/facts/actor-2"),
    ]);
    let request = json!({"clientToken": "batch-token-1", "records": records.clone()});

    let (status, first) = batch_create(&router, MEMORY, request.clone()).await;
    assert_eq!(
        status, 201,
        "case \"first call\": expected HTTP 201, got {status} with {first:?}"
    );
    let (status, replay) = batch_create(&router, MEMORY, request).await;
    assert_eq!(
        status, 201,
        "case \"replayed token\": expected HTTP 201, got {status} with {replay:?}"
    );
    assert_eq!(
        replay["successfulRecords"], first["successfulRecords"],
        "case \"replayed token\": expected the records the first call created, got {:?}",
        replay["successfulRecords"]
    );

    let (status, fresh) = batch_create(
        &router,
        MEMORY,
        json!({"clientToken": "batch-token-2", "records": records}),
    )
    .await;
    assert_eq!(
        status, 201,
        "case \"a different token\": expected HTTP 201, got {status} with {fresh:?}"
    );
    let minted = |body: &Value| -> Vec<String> {
        body["successfulRecords"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|outcome| {
                outcome["memoryRecordId"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string()
            })
            .collect()
    };
    let first_ids = minted(&first);
    let fresh_ids = minted(&fresh);
    assert!(
        fresh_ids.iter().all(|id| !first_ids.contains(id)),
        "case \"a different token\": expected a second set of records, got {fresh_ids:?} against \
         {first_ids:?}"
    );

    // Both sets are readable, so the replay stored nothing extra and the fresh call
    // stored its own records.
    for id in first_ids.iter().chain(fresh_ids.iter()) {
        let record = stored_record(&router, MEMORY, id).await;
        assert_eq!(
            record["memoryRecordId"], *id,
            "case \"both sets readable\": expected {id:?} to be stored, got {record:?}"
        );
    }
}

/// `ListMemoryRecords`: `memoryId` in the path, every filter in the body.
async fn list_records(router: &MultiServiceRouter, memory: &str, body: Value) -> (u16, Value) {
    send(
        router,
        "POST",
        &format!("/memories/{}/memoryRecords", label(memory)),
        Some(body),
    )
    .await
}

/// One `MemoryMetadataFilterExpression`: the metadata key, the relationship, and the
/// value to compare against (absent for the two operators that take none).
fn metadata_filter(key: &str, operator: &str, value: Option<Value>) -> Value {
    let mut expression = json!({"left": {"metadataKey": key}, "operator": operator});
    if let Some(value) = value {
        expression["right"] = json!({"metadataValue": value});
    }
    expression
}

/// The records every selection case is run against, created one batch at a time so their
/// creation order — the order a listing reverses — is known.
///
/// Rows differ along the namespace depth, the strategy association (named or left to the
/// one this emulator reports for a record created with none) and the metadata they carry:
/// every `MemoryRecordMetadataValue` member a filter can compare against appears.
/// Answers the label -> `memoryRecordId` pairs, oldest first.
async fn listed_fixture(router: &MultiServiceRouter) -> Vec<(&'static str, String)> {
    let entries: Vec<(&'static str, Value)> = vec![
        (
            "travel-fact",
            json!({
                "requestIdentifier": "listing-1",
                "namespaces": ["support/facts/actor-1"],
                "content": {"text": "the user prefers window seats"},
                "timestamp": 1_700_000_000,
                "metadata": {
                    "topic": {"stringValue": "travel"},
                    "confidence": {"numberValue": 0.4},
                    "observedAt": {"dateTimeValue": 1_700_000_000},
                },
            }),
        ),
        (
            "tagged-fact",
            json!({
                "requestIdentifier": "listing-2",
                "namespaces": ["support/facts/actor-2"],
                "content": {"text": "lives in Berlin"},
                "timestamp": 1_700_000_100,
                "memoryStrategyId": "userPreference-abc123",
                "metadata": {
                    "tags": {"stringListValue": ["travel", "seats"]},
                    "confidence": {"numberValue": 0.9},
                },
            }),
        ),
        (
            "preference",
            json!({
                "requestIdentifier": "listing-3",
                "namespaces": ["support/preferences/actor-1"],
                "content": {"text": "likes aisle seats"},
                "timestamp": 1_700_000_200,
                "memoryStrategyId": "userPreference-abc123",
                "metadata": {
                    "topic": {"stringValue": "home travel plans"},
                    "observedAt": {"dateTimeValue": 1_800_000_000},
                },
            }),
        ),
        (
            "root-summary",
            json!({
                "requestIdentifier": "listing-4",
                "namespaces": ["support"],
                "content": {"text": "the account is a frequent flyer"},
                "timestamp": 1_700_000_300,
                "memoryStrategyId": "semantic_strategy-9",
            }),
        ),
    ];

    let mut created: Vec<(&'static str, String)> = Vec::new();
    for (name, entry) in entries {
        let (status, body) = batch_create(router, MEMORY, json!({"records": [entry]})).await;
        assert_eq!(
            status, 201,
            "creating the fixture record {name:?}: expected HTTP 201, got {status} with {body:?}"
        );
        created.push((
            name,
            body["successfulRecords"][0]["memoryRecordId"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
        ));
    }
    created
}

/// The fixture labels of the records one listing reported, in the order it reported them.
fn reported(listed: &Value, fixture: &[(&'static str, String)]) -> Vec<String> {
    listed["memoryRecordSummaries"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|summary| {
            let id = summary["memoryRecordId"].as_str().unwrap_or_default();
            fixture
                .iter()
                .find(|(_, created)| created == id)
                .map_or_else(
                    || format!("a record no case created ({id})"),
                    |(name, _)| (*name).to_string(),
                )
        })
        .collect()
}

/// A listing reports the container's records narrowed by what the request asks for: the
/// exact namespace, the namespace hierarchy, the strategy association and the metadata
/// expressions, each narrowing on its own and all of them narrowing together.
///
/// Rows span the filter classes the model declares: both namespace members (source:
/// smithy_query query="operation" operation="ListMemoryRecords" — `namespace` is "the
/// exact namespace", `namespacePath` returns "all memory records where namespace falls
/// under the same parent hierarchy"), `memoryStrategyId`, and one row per
/// `MemoryRecordOperatorType` value (source: smithy_query query="shape"
/// shape="MemoryRecordOperatorType": EQUALS_TO | EXISTS | NOT_EXISTS | BEFORE | AFTER |
/// CONTAINS | GREATER_THAN | GREATER_THAN_OR_EQUALS | LESS_THAN | LESS_THAN_OR_EQUALS),
/// plus the combinations of several filters and a filter nothing satisfies.
#[tokio::test]
async fn a_listing_reports_the_records_the_request_s_filters_select() {
    let router = emulator();
    let fixture = listed_fixture(&router).await;

    // (case, the request body, the fixture labels expected back in order)
    let cases: Vec<(&str, Value, Vec<&str>)> = vec![
        (
            "no filter at all",
            json!({}),
            vec!["root-summary", "preference", "tagged-fact", "travel-fact"],
        ),
        (
            "the exact namespace of one record",
            json!({"namespace": "support/facts/actor-1"}),
            vec!["travel-fact"],
        ),
        (
            "an exact namespace is not a hierarchy: a parent selects only its own record",
            json!({"namespace": "support"}),
            vec!["root-summary"],
        ),
        (
            "an exact namespace no record is stored under",
            json!({"namespace": "support/facts/actor-9"}),
            vec![],
        ),
        (
            "the namespace path of the whole hierarchy",
            json!({"namespacePath": "support"}),
            vec!["root-summary", "preference", "tagged-fact", "travel-fact"],
        ),
        (
            "the namespace path of one level of it",
            json!({"namespacePath": "support/facts"}),
            vec!["tagged-fact", "travel-fact"],
        ),
        (
            "the same namespace path written with a trailing separator",
            json!({"namespacePath": "support/facts/"}),
            vec!["tagged-fact", "travel-fact"],
        ),
        (
            "the namespace path of a leaf level",
            json!({"namespacePath": "support/preferences/actor-1"}),
            vec!["preference"],
        ),
        (
            "a namespace path beneath which nothing is stored",
            json!({"namespacePath": "support/sessions"}),
            vec![],
        ),
        (
            "the strategy two records are grouped under",
            json!({"memoryStrategyId": "userPreference-abc123"}),
            vec!["preference", "tagged-fact"],
        ),
        (
            "the strategy reported for a record created with none",
            json!({"memoryStrategyId": CALLER_MANAGED_STRATEGY}),
            vec!["travel-fact"],
        ),
        (
            "a namespace path and a strategy narrow together",
            json!({"namespacePath": "support", "memoryStrategyId": "userPreference-abc123"}),
            vec!["preference", "tagged-fact"],
        ),
        (
            "metadata EXISTS",
            json!({"metadataFilters": [metadata_filter("topic", "EXISTS", None)]}),
            vec!["preference", "travel-fact"],
        ),
        (
            "metadata NOT_EXISTS",
            json!({"metadataFilters": [metadata_filter("topic", "NOT_EXISTS", None)]}),
            vec!["root-summary", "tagged-fact"],
        ),
        (
            "metadata EQUALS_TO a string",
            json!({"metadataFilters": [
                metadata_filter("topic", "EQUALS_TO", Some(json!({"stringValue": "travel"})))
            ]}),
            vec!["travel-fact"],
        ),
        (
            "metadata EQUALS_TO a string no record carries",
            json!({"metadataFilters": [
                metadata_filter("topic", "EQUALS_TO", Some(json!({"stringValue": "weather"})))
            ]}),
            vec![],
        ),
        (
            "metadata EQUALS_TO a number",
            json!({"metadataFilters": [
                metadata_filter("confidence", "EQUALS_TO", Some(json!({"numberValue": 0.9})))
            ]}),
            vec!["tagged-fact"],
        ),
        (
            "metadata CONTAINS a substring of a string value",
            json!({"metadataFilters": [
                metadata_filter("topic", "CONTAINS", Some(json!({"stringValue": "travel"})))
            ]}),
            vec!["preference", "travel-fact"],
        ),
        (
            "metadata CONTAINS one element of a string-list value",
            json!({"metadataFilters": [
                metadata_filter("tags", "CONTAINS", Some(json!({"stringValue": "seats"})))
            ]}),
            vec!["tagged-fact"],
        ),
        (
            "metadata GREATER_THAN a number",
            json!({"metadataFilters": [
                metadata_filter("confidence", "GREATER_THAN", Some(json!({"numberValue": 0.5})))
            ]}),
            vec!["tagged-fact"],
        ),
        (
            "metadata GREATER_THAN_OR_EQUALS a number, at the value itself",
            json!({"metadataFilters": [
                metadata_filter(
                    "confidence",
                    "GREATER_THAN_OR_EQUALS",
                    Some(json!({"numberValue": 0.9})),
                )
            ]}),
            vec!["tagged-fact"],
        ),
        (
            "metadata LESS_THAN a number",
            json!({"metadataFilters": [
                metadata_filter("confidence", "LESS_THAN", Some(json!({"numberValue": 0.9})))
            ]}),
            vec!["travel-fact"],
        ),
        (
            "metadata LESS_THAN_OR_EQUALS a number, at the value itself",
            json!({"metadataFilters": [
                metadata_filter(
                    "confidence",
                    "LESS_THAN_OR_EQUALS",
                    Some(json!({"numberValue": 0.9})),
                )
            ]}),
            vec!["tagged-fact", "travel-fact"],
        ),
        (
            "metadata BEFORE an instant",
            json!({"metadataFilters": [
                metadata_filter(
                    "observedAt",
                    "BEFORE",
                    Some(json!({"dateTimeValue": 1_750_000_000_i64})),
                )
            ]}),
            vec!["travel-fact"],
        ),
        (
            "metadata AFTER an instant",
            json!({"metadataFilters": [
                metadata_filter(
                    "observedAt",
                    "AFTER",
                    Some(json!({"dateTimeValue": 1_750_000_000_i64})),
                )
            ]}),
            vec!["preference"],
        ),
        (
            "an instant comparison against a value that is not an instant selects nothing",
            json!({"metadataFilters": [
                metadata_filter("confidence", "AFTER", Some(json!({"numberValue": 0.1})))
            ]}),
            vec![],
        ),
        (
            "a comparison against a value of another kind selects nothing",
            json!({"metadataFilters": [
                metadata_filter(
                    "confidence",
                    "GREATER_THAN",
                    Some(json!({"dateTimeValue": 1_700_000_000_i64})),
                )
            ]}),
            vec![],
        ),
        (
            "EQUALS_TO against a value of another kind selects nothing",
            json!({"metadataFilters": [
                metadata_filter("tags", "EQUALS_TO", Some(json!({"stringValue": "travel"})))
            ]}),
            vec![],
        ),
        (
            "two metadata expressions narrow together",
            json!({"metadataFilters": [
                metadata_filter("topic", "EXISTS", None),
                metadata_filter(
                    "confidence",
                    "LESS_THAN_OR_EQUALS",
                    Some(json!({"numberValue": 0.5})),
                ),
            ]}),
            vec!["travel-fact"],
        ),
        (
            "a namespace and a metadata expression narrow together",
            json!({
                "namespace": "support/facts/actor-2",
                "metadataFilters": [metadata_filter("tags", "EXISTS", None)],
            }),
            vec!["tagged-fact"],
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

    for (case, request, expected) in cases {
        let (status, listed) = list_records(&router, MEMORY, request.clone()).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from ListMemoryRecords, got {status} with {listed:?}"
        );
        let seen = reported(&listed, &fixture);
        assert_eq!(
            seen,
            expected.iter().map(ToString::to_string).collect::<Vec<_>>(),
            "case {case:?}: expected the records {expected:?} in that order, got {seen:?} from \
             {listed:?}"
        );
        assert!(
            listed["nextToken"].is_null(),
            "case {case:?}: expected no nextToken on a complete page, got {:?}",
            listed["nextToken"]
        );
    }
}

/// A listing reports each record as it was stored — content, namespaces, strategy,
/// timestamp and metadata — and the same record a read reports, with no relevance score:
/// a listing selects records, it ranks nothing.
#[tokio::test]
async fn a_listed_summary_reports_what_was_stored() {
    let router = emulator();
    let fixture = listed_fixture(&router).await;

    let (status, listed) = list_records(&router, MEMORY, json!({})).await;
    assert_eq!(
        status, 200,
        "expected HTTP 200 from ListMemoryRecords, got {status} with {listed:?}"
    );
    let summaries = listed["memoryRecordSummaries"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        summaries.len(),
        fixture.len(),
        "expected one summary per stored record ({}), got {summaries:?}",
        fixture.len()
    );

    for summary in &summaries {
        let id = summary["memoryRecordId"].as_str().unwrap_or_default();
        let case = fixture
            .iter()
            .find(|(_, created)| created == id)
            .map_or_else(|| format!("unknown record {id}"), |(name, _)| (*name).to_string());
        let record = stored_record(&router, MEMORY, id).await;
        for member in ["content", "namespaces", "memoryStrategyId", "createdAt", "timestamp"] {
            assert_eq!(
                summary[member], record[member],
                "case {case:?}: expected the summary's {member} to be the stored record's {:?}, \
                 got {:?}",
                record[member], summary[member]
            );
        }
        assert_eq!(
            summary["metadata"], record["metadata"],
            "case {case:?}: expected the stored metadata back unchanged, got {:?}",
            summary["metadata"]
        );
        assert_eq!(
            summary["score"],
            Value::Null,
            "case {case:?}: a listing ranks nothing, so it reports no score, got {:?}",
            summary["score"]
        );
    }
}

/// A listing pages deterministically: whatever the page size, the pages walk the same
/// most-recently-created-first sequence exactly once and the last page carries no token.
#[tokio::test]
async fn a_listing_pages_over_every_page_size() {
    let router = emulator();
    let fixture = listed_fixture(&router).await;
    let newest_first: Vec<String> = fixture
        .iter()
        .rev()
        .map(|(name, _)| (*name).to_string())
        .collect();

    // Rows: every page size from one record per page to more than the container holds,
    // plus the published maximum of `maxResults` (100) and its minimum (1).
    for page_size in [1_usize, 2, 3, 4, 5, 100] {
        let mut collected: Vec<String> = Vec::new();
        let mut token = Value::Null;
        let mut pages = 0;
        loop {
            let mut request = json!({"maxResults": page_size});
            if let Some(value) = token.as_str() {
                request["nextToken"] = json!(value);
            }
            let (status, page) = list_records(&router, MEMORY, request).await;
            assert_eq!(
                status, 200,
                "page size {page_size}: expected HTTP 200, got {status} with {page:?}"
            );
            let reported_now = reported(&page, &fixture);
            assert!(
                reported_now.len() <= page_size,
                "page size {page_size}: expected at most {page_size} records on a page, got \
                 {reported_now:?}"
            );
            collected.extend(reported_now);
            token = page["nextToken"].clone();
            pages += 1;
            assert!(
                pages <= 10,
                "page size {page_size}: expected pagination to terminate, got {pages} pages"
            );
            if token.is_null() {
                break;
            }
        }
        assert_eq!(
            collected, newest_first,
            "page size {page_size}: expected every record once, newest first, got {collected:?}"
        );
    }
}

/// A listing reports the records of the container it addresses and nothing else: a
/// container with nothing stored in it is an empty page, a deleted record is gone from the
/// listing, and a `memoryId` naming no container at all is refused at the container level.
#[tokio::test]
async fn a_listing_is_scoped_to_its_memory_container() {
    let router = emulator();
    let fixture = listed_fixture(&router).await;

    // The ARN spelling of the same container addresses the same records.
    let (status, listed) = list_records(&router, MEMORY_ARN, json!({})).await;
    assert_eq!(
        status, 200,
        "case \"the container's ARN spelling\": expected HTTP 200, got {status} with {listed:?}"
    );
    assert_eq!(
        reported(&listed, &fixture).len(),
        fixture.len(),
        "case \"the container's ARN spelling\": expected the same records, got {listed:?}"
    );

    // A deleted record leaves the listing, and every other record stays in it.
    let (_, removed) = fixture[1].clone();
    let (status, deleted) = delete_record(&router, MEMORY, &removed).await;
    assert_eq!(
        status, 200,
        "case \"delete one record\": expected HTTP 200, got {status} with {deleted:?}"
    );
    let (status, listed) = list_records(&router, MEMORY, json!({})).await;
    assert_eq!(
        status, 200,
        "case \"list after the delete\": expected HTTP 200, got {status} with {listed:?}"
    );
    let seen = reported(&listed, &fixture);
    assert_eq!(
        seen,
        vec![
            "root-summary".to_string(),
            "preference".to_string(),
            "travel-fact".to_string()
        ],
        "case \"list after the delete\": expected the deleted record gone and the rest kept, got \
         {seen:?}"
    );

    // A container this plane holds nothing for reports nothing — the container exists, so
    // this is an empty page rather than a refusal.
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    super::harness::create_memory(&engine, OTHER_MEMORY);
    let mut empty_router = MultiServiceRouter::new();
    empty_router.set_request_authentication(false);
    empty_router.register_service(agentcore::EmulatedService::new(engine));
    let (status, listed) = list_records(&empty_router, OTHER_MEMORY, json!({})).await;
    assert_eq!(
        status, 200,
        "case \"a container with nothing stored\": expected HTTP 200, got {status} with {listed:?}"
    );
    assert_eq!(
        listed["memoryRecordSummaries"],
        json!([]),
        "case \"a container with nothing stored\": expected an empty page, got {listed:?}"
    );

    // A container the account does not hold is refused, with the sentence the memory
    // records API words its container level with.
    let (status, body) = list_records(&router, OTHER_MEMORY, json!({})).await;
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

/// A listing refuses a request it cannot answer as asked: a `nextToken` it never issued,
/// and a metadata expression whose operator compares against a value the request did not
/// give it (`right` is "required for all operators except EXISTS and NOT_EXISTS").
#[tokio::test]
async fn a_listing_refuses_a_request_it_cannot_answer() {
    let router = emulator();
    let _ = listed_fixture(&router).await;

    let other_lists_token = aws_smithy_types::base64::encode("events:1");
    let cases: Vec<(&str, Value)> = vec![
        ("a token that is not base64", json!({"nextToken": "not a token"})),
        ("base64 that is not a cursor", json!({"nextToken": "YWJjZGU="})),
        (
            "a token another listing issued",
            json!({"nextToken": other_lists_token.as_str()}),
        ),
        (
            "EQUALS_TO with no value to compare against",
            json!({"metadataFilters": [metadata_filter("topic", "EQUALS_TO", None)]}),
        ),
        (
            "CONTAINS with no value to compare against",
            json!({"metadataFilters": [metadata_filter("tags", "CONTAINS", None)]}),
        ),
        (
            "BEFORE with no value to compare against",
            json!({"metadataFilters": [metadata_filter("observedAt", "BEFORE", None)]}),
        ),
    ];

    for (case, request) in cases {
        let (status, body) = list_records(&router, MEMORY, request).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400 from ListMemoryRecords, got {status} with {body:?}"
        );
        assert_eq!(
            error_code(&body),
            "ValidationException",
            "case {case:?}: expected ValidationException, got {body:?}"
        );
    }

    // The operators that take no value are answered, not refused.
    for operator in ["EXISTS", "NOT_EXISTS"] {
        let (status, body) = list_records(
            &router,
            MEMORY,
            json!({"metadataFilters": [metadata_filter("topic", operator, None)]}),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {operator:?}: expected HTTP 200 for an operator that needs no value, got \
             {status} with {body:?}"
        );
    }
}
