// SPDX-License-Identifier: CC0-1.0

//! What a REQUEST about an event source mapping has to state, over the three operations
//! that state it.
//!
//! Four rules, each asserted over a table whose rows span what the rule generalises over,
//! end to end through the production dispatch path:
//!
//! 1. **a queue's batch and its batching window** — `BatchSize`'s own related setting,
//!    `smithy_query(query="operation", operation="CreateEventSourceMapping")`: "When you set
//!    BatchSize to a value greater than 10, you must set MaximumBatchingWindowInSeconds to at
//!    least 1." A queue's default window is "0 seconds" ("For Kinesis, DynamoDB, and Amazon
//!    SQS event sources, the default batching window is 0 seconds"), so a mapping on a queue
//!    asking for a larger batch without one is refused; the stream sources, whose default
//!    batch is 100, are not held to it. A FIFO queue — the one whose NAME ends in the suffix
//!    Amazon SQS requires of one — hands its messages to one consumer in order, so it has no
//!    batching window at all and asking to gather one is its own refusal: the two together
//!    hold a FIFO queue's batch at ten. The same requirements are decided by an UPDATE,
//!    against the values in force after it.
//! 2. **a filter pattern has to be one** — `Filter$Pattern` is "a filter pattern", so text
//!    that is not one is refused by both calls that state `FilterCriteria`, and a mapping
//!    whose criteria were refused is left exactly as it was: the next, valid update of the
//!    same mapping is accepted. Criteria carrying no filter at all are no criteria — "to
//!    delete the filter criteria, specify an empty FilterCriteria object" — so a mapping
//!    stated with them, or updated to them, carries none and reports the member not at all.
//! 3. **a listing's filters are filters** — `ListEventSourceMappings` output
//!    `EventSourceMappings` is "a list of event source mappings", and a `FunctionName` or
//!    `EventSourceArn` that matches none of the stored mappings selects none of them: the
//!    answer is HTTP 200 with an empty list, including for a function this account does not
//!    hold.
//! 4. **but an `EventSourceArn` still has to name an event source** — the member enumerates
//!    the source services and the resource of each one a mapping is read through, so a value
//!    outside that enumeration is refused by the listing exactly as it is by the create, and
//!    a value that names one of those resources is held to the resource EXISTING: a broker
//!    Amazon MQ does not hold is answered with the modelled not-found.
//!
//! Every assertion names its case and the actual value.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::event_source_mappings::{
    call, create_function, create_mapping, create_request, get_mapping, given_broker,
    given_enabled_mapping, list_mappings, refusal_message, router_on, Answer, DYNAMODB_SOURCE,
    KINESIS_SOURCE, MAPPINGS_URI, MSK_SOURCE, QUEUE_SOURCE,
};

/// The Region the harness signs for, and the account every emulator ARN carries.
const REGION: &str = "us-east-1";
/// See [`REGION`].
const ACCOUNT: &str = "000000000000";

/// The account field of an ARN written as a FORMAT: the example id such a format carries
/// beside the placeholder names of its resource half, rather than an account anything lives
/// in.
const EXAMPLE_ACCOUNT: &str = "123456789012";

/// The broker the Amazon MQ rows map, and the ARN that names it: `broker:<name>:<id>`, the
/// shape "Amazon MQ – The ARN of the broker" is written in.
const BROKER_NAME: &str = "request-rules-broker";
/// See [`BROKER_NAME`].
const BROKER_SOURCE: &str =
    "arn:aws:mq:us-east-1:000000000000:broker:request-rules-broker:b-1111";
/// A broker of the same account that nothing created.
const ABSENT_BROKER_SOURCE: &str =
    "arn:aws:mq:us-east-1:000000000000:broker:request-rules-ghost:b-2222";

/// The modelled errors these rules are refused with.
const INVALID_PARAMETER: &str = "InvalidParameterValueException";
/// See [`INVALID_PARAMETER`].
const NOT_FOUND: &str = "ResourceNotFoundException";

/// A FIFO queue, whose NAME ends in the suffix Amazon SQS requires of one.
const FIFO_QUEUE_SOURCE: &str = "arn:aws:sqs:us-east-1:000000000000:order-intake.fifo";

/// How Lambda words the refusal of a batch past ten gathered without a window (fixed text).
const BATCH_WINDOW_MESSAGE: &str =
    "Maximum batch window in seconds must be greater than 0 if maximum batch size is greater \
     than 10";

/// How Lambda words the refusal of a batching window on a FIFO queue (fixed text).
const FIFO_WINDOW_MESSAGE: &str = "Batching window is not supported for FIFO queues";

/// How Lambda words the refusal of criteria that are not a filter pattern (fixed text).
const INVALID_FILTER_MESSAGE: &str = "Invalid filter pattern definition.";

/// `UpdateEventSourceMapping` on the mapping `uuid` names.
async fn update_mapping(router: &MultiServiceRouter, uuid: &str, body: &Value) -> Answer {
    call(router, "PUT", &format!("{MAPPINGS_URI}/{uuid}"), body).await
}

/// One row of the batch-and-window table: the source the mapping names, what else that
/// source requires, the `BatchSize` and `MaximumBatchingWindowInSeconds` the create states,
/// and the sentence the pair is refused with — `None` for a pair the queue may be polled in.
struct BatchRow {
    case: &'static str,
    event_source: &'static str,
    extras: Value,
    batch_size: i64,
    window: Option<i64>,
    refusal: Option<&'static str>,
}

/// The rows: the batch either side of the ceiling the related setting names, with no window,
/// a zero window and a window past zero, at the published `BatchSize` maximum
/// (`@range(min: 1, max: 10000)`), on a standard queue and on a FIFO one, and the same
/// batches on the sources neither requirement binds.
fn batch_rows() -> Vec<BatchRow> {
    vec![
        BatchRow {
            case: "a queue at the ceiling without a window",
            event_source: QUEUE_SOURCE,
            extras: json!({}),
            batch_size: 10,
            window: None,
            refusal: None,
        },
        BatchRow {
            case: "a queue one past the ceiling without a window",
            event_source: QUEUE_SOURCE,
            extras: json!({}),
            batch_size: 11,
            window: None,
            refusal: Some(BATCH_WINDOW_MESSAGE),
        },
        BatchRow {
            case: "a queue one past the ceiling with a zero window",
            event_source: QUEUE_SOURCE,
            extras: json!({}),
            batch_size: 11,
            window: Some(0),
            refusal: Some(BATCH_WINDOW_MESSAGE),
        },
        BatchRow {
            case: "a queue one past the ceiling with the smallest window",
            event_source: QUEUE_SOURCE,
            extras: json!({}),
            batch_size: 11,
            window: Some(1),
            refusal: None,
        },
        BatchRow {
            case: "a queue at the published batch maximum without a window",
            event_source: QUEUE_SOURCE,
            extras: json!({}),
            batch_size: 10_000,
            window: None,
            refusal: Some(BATCH_WINDOW_MESSAGE),
        },
        BatchRow {
            case: "a queue at the published batch maximum with the largest window",
            event_source: QUEUE_SOURCE,
            extras: json!({}),
            batch_size: 10_000,
            window: Some(300),
            refusal: None,
        },
        BatchRow {
            case: "a fifo queue at the ceiling without a window",
            event_source: FIFO_QUEUE_SOURCE,
            extras: json!({}),
            batch_size: 10,
            window: None,
            refusal: None,
        },
        BatchRow {
            case: "a fifo queue one past the ceiling without a window",
            event_source: FIFO_QUEUE_SOURCE,
            extras: json!({}),
            batch_size: 11,
            window: None,
            refusal: Some(BATCH_WINDOW_MESSAGE),
        },
        BatchRow {
            case: "a fifo queue one past the ceiling with the smallest window",
            event_source: FIFO_QUEUE_SOURCE,
            extras: json!({}),
            batch_size: 11,
            window: Some(1),
            refusal: Some(FIFO_WINDOW_MESSAGE),
        },
        BatchRow {
            case: "a fifo queue at the ceiling with a window",
            event_source: FIFO_QUEUE_SOURCE,
            extras: json!({}),
            batch_size: 10,
            window: Some(5),
            refusal: Some(FIFO_WINDOW_MESSAGE),
        },
        BatchRow {
            case: "a kinesis data stream past the ceiling without a window",
            event_source: KINESIS_SOURCE,
            extras: json!({ "StartingPosition": "LATEST" }),
            batch_size: 100,
            window: None,
            refusal: None,
        },
        BatchRow {
            case: "a dynamodb stream at the published batch maximum without a window",
            event_source: DYNAMODB_SOURCE,
            extras: json!({ "StartingPosition": "TRIM_HORIZON" }),
            batch_size: 10_000,
            window: None,
            refusal: None,
        },
        BatchRow {
            case: "an msk cluster past the ceiling without a window",
            event_source: MSK_SOURCE,
            extras: json!({ "Topics": ["order-events"] }),
            batch_size: 500,
            window: None,
            refusal: None,
        },
    ]
}

/// RULE: a mapping on a QUEUE whose `BatchSize` is past ten takes a
/// `MaximumBatchingWindowInSeconds` past zero, and one on a FIFO queue takes no batching
/// window at all; a pair that breaks either is refused with the modelled 400 carrying that
/// requirement's own sentence and no mapping is created, and no other kind of event source is
/// held to them.
#[tokio::test]
async fn a_queue_batch_past_ten_takes_a_batching_window() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);

    for (index, row) in batch_rows().iter().enumerate() {
        let case = row.case;
        let function = format!("esm-batch-window-{index}");
        create_function(&router, &function).await;

        let mut extras = row.extras.clone();
        extras["BatchSize"] = json!(row.batch_size);
        if let Some(window) = row.window {
            extras["MaximumBatchingWindowInSeconds"] = json!(window);
        }
        let answer = create_mapping(
            &router,
            &create_request(&function, Some(row.event_source), &extras),
        )
        .await;
        let listed = list_mappings(&router, &format!("FunctionName={function}")).await;
        let held = listed.mappings().len();

        if let Some(expected) = row.refusal {
            assert_eq!(
                (answer.status, answer.error_code().contains(INVALID_PARAMETER)),
                (400, true),
                "case {case:?}: a batch of {} with window {:?} must be refused with the \
                 modelled 400, got {} {}",
                row.batch_size,
                row.window,
                answer.status,
                answer.text()
            );
            assert_eq!(
                refusal_message(&answer).as_deref(),
                Some(expected),
                "case {case:?}: the refusal states the requirement, got {}",
                answer.text()
            );
            assert_eq!(
                held, 0,
                "case {case:?}: a refused create stores no mapping, got {held} in {}",
                listed.text()
            );
            continue;
        }

        assert_eq!(
            answer.status,
            202,
            "case {case:?}: a batch of {} with window {:?} must be accepted, got {} {}",
            row.batch_size,
            row.window,
            answer.status,
            answer.text()
        );
        let uuid = answer
            .string("UUID")
            .unwrap_or_else(|| panic!("case {case:?}: the create answers a UUID"));
        let fetched = get_mapping(&router, &uuid).await;
        assert_eq!(
            (
                fetched.status,
                fetched.json().get("BatchSize").cloned().unwrap_or(Value::Null)
            ),
            (200, json!(row.batch_size)),
            "case {case:?}: the stored mapping reports the batch it was created with, got {} {}",
            fetched.status,
            fetched.text()
        );
        assert_eq!(
            held, 1,
            "case {case:?}: the accepted create stores exactly one mapping, got {held} in {}",
            listed.text()
        );
    }
}

/// RULE: the same requirements are decided by an UPDATE, against the values in force after
/// it — so an update that raises a queue mapping's batch past ten without a window is refused
/// and changes nothing, the same raise beside a window is accepted, and a window stated for a
/// FIFO queue's mapping is refused whatever its batch.
#[tokio::test]
async fn an_update_of_a_queue_batch_is_held_to_the_same_window_rule() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);

    // (case, the mapping's source, what the create states, what the update states, the
    // sentence the update is refused with)
    let rows: Vec<(&str, &str, Value, Value, Option<&str>)> = vec![
        (
            "raising the batch past the ceiling with the stored window at zero",
            QUEUE_SOURCE,
            json!({ "BatchSize": 5 }),
            json!({ "BatchSize": 11 }),
            Some(BATCH_WINDOW_MESSAGE),
        ),
        (
            "raising the batch past the ceiling and stating a window at once",
            QUEUE_SOURCE,
            json!({ "BatchSize": 5 }),
            json!({ "BatchSize": 11, "MaximumBatchingWindowInSeconds": 1 }),
            None,
        ),
        (
            "raising the batch past the ceiling with a window already stored",
            QUEUE_SOURCE,
            json!({ "BatchSize": 5, "MaximumBatchingWindowInSeconds": 2 }),
            json!({ "BatchSize": 10_000 }),
            None,
        ),
        (
            "dropping the stored window to zero under a batch past the ceiling",
            QUEUE_SOURCE,
            json!({ "BatchSize": 11, "MaximumBatchingWindowInSeconds": 2 }),
            json!({ "MaximumBatchingWindowInSeconds": 0 }),
            Some(BATCH_WINDOW_MESSAGE),
        ),
        (
            "raising the batch to the ceiling itself with no window",
            QUEUE_SOURCE,
            json!({ "BatchSize": 5 }),
            json!({ "BatchSize": 10 }),
            None,
        ),
        (
            "stating a window for a fifo queue's mapping",
            FIFO_QUEUE_SOURCE,
            json!({ "BatchSize": 5 }),
            json!({ "MaximumBatchingWindowInSeconds": 1 }),
            Some(FIFO_WINDOW_MESSAGE),
        ),
        (
            "raising a fifo queue's batch past the ceiling",
            FIFO_QUEUE_SOURCE,
            json!({ "BatchSize": 5 }),
            json!({ "BatchSize": 11 }),
            Some(BATCH_WINDOW_MESSAGE),
        ),
        (
            "raising a fifo queue's batch to the ceiling itself",
            FIFO_QUEUE_SOURCE,
            json!({ "BatchSize": 5 }),
            json!({ "BatchSize": 10 }),
            None,
        ),
    ];

    for (index, (case, event_source, create_extras, update, refusal)) in rows.iter().enumerate() {
        let function = format!("esm-batch-update-{index}");
        create_function(&router, &function).await;
        let uuid = given_enabled_mapping(
            &router,
            case,
            &create_request(&function, Some(event_source), create_extras),
        )
        .await;
        let before = get_mapping(&router, &uuid).await.json();

        let answer = update_mapping(&router, &uuid, update).await;
        if let Some(expected) = refusal {
            assert_eq!(
                (answer.status, answer.error_code().contains(INVALID_PARAMETER)),
                (400, true),
                "case {case:?}: the update must be refused with the modelled 400, got {} {}",
                answer.status,
                answer.text()
            );
            assert_eq!(
                refusal_message(&answer).as_deref(),
                Some(*expected),
                "case {case:?}: the refusal states the requirement, got {}",
                answer.text()
            );
            let after = get_mapping(&router, &uuid).await.json();
            for member in ["BatchSize", "MaximumBatchingWindowInSeconds", "State"] {
                assert_eq!(
                    after.get(member),
                    before.get(member),
                    "case {case:?}: a refused update leaves {member} as it was, got {after}"
                );
            }
            continue;
        }
        assert_eq!(
            answer.status,
            202,
            "case {case:?}: the update must be accepted, got {} {}",
            answer.status,
            answer.text()
        );
        for (member, value) in update.as_object().expect("the update is an object") {
            assert_eq!(
                answer.json().get(member),
                Some(value),
                "case {case:?}: the accepted update reports {member} as {value}, got {}",
                answer.text()
            );
        }
    }
}

/// RULE: a `Filter$Pattern` has to be a filter pattern — text that is not one is refused
/// with the modelled 400 carrying AWS's fixed sentence, by the create and by the update
/// alike, and a refused update leaves the mapping's criteria exactly as they were.
#[tokio::test]
async fn criteria_that_are_not_a_filter_pattern_are_refused_by_both_calls() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);

    // (case, the pattern text, whether it is a pattern this service admits)
    let rows: Vec<(&str, &str, bool)> = vec![
        ("one clause of candidate values", r#"{"eventName":["INSERT"]}"#, true),
        (
            "a clause nested in the record",
            r#"{"dynamodb":{"Keys":{"pk":{"S":["row-1"]}}}}"#,
            true,
        ),
        (
            "a clause written as a filter rather than a value",
            r#"{"body":{"kind":[{"prefix":"order"}]}}"#,
            true,
        ),
        ("text that is not json at all", "INSERT", false),
        ("json that is not an object", r#"["INSERT"]"#, false),
        (
            "a member holding a bare value rather than a clause",
            r#"{"eventName":"INSERT"}"#,
            false,
        ),
        ("a truncated document", r#"{"eventName":["INSERT"]"#, false),
    ];

    for (index, (case, pattern, admitted)) in rows.iter().enumerate() {
        let function = format!("esm-filter-{index}");
        create_function(&router, &function).await;
        let criteria = json!({ "Filters": [{ "Pattern": pattern }] });

        // The create states the criteria.
        let created = create_mapping(
            &router,
            &create_request(
                &function,
                Some(QUEUE_SOURCE),
                &json!({ "FilterCriteria": criteria }),
            ),
        )
        .await;
        if *admitted {
            assert_eq!(
                created.status,
                202,
                "case {case:?}: a pattern this service reads must be accepted, got {} {}",
                created.status,
                created.text()
            );
        } else {
            assert_eq!(
                (
                    created.status,
                    created.error_code().contains(INVALID_PARAMETER),
                    refusal_message(&created)
                ),
                (400, true, Some(INVALID_FILTER_MESSAGE.to_string())),
                "case {case:?}: text that is no pattern must be refused with the fixed \
                 sentence, got {} {}",
                created.status,
                created.text()
            );
            let listed = list_mappings(&router, &format!("FunctionName={function}")).await;
            assert_eq!(
                listed.mappings().len(),
                0,
                "case {case:?}: a refused create stores no mapping, got {}",
                listed.text()
            );
        }

        // And so does an update of a mapping that already carries readable criteria.
        let other = format!("esm-filter-update-{index}");
        create_function(&router, &other).await;
        let stored = json!({ "Filters": [{ "Pattern": r#"{"eventName":["REMOVE"]}"# }] });
        let uuid = given_enabled_mapping(
            &router,
            case,
            &create_request(
                &other,
                Some(QUEUE_SOURCE),
                &json!({ "FilterCriteria": stored }),
            ),
        )
        .await;
        let updated = update_mapping(&router, &uuid, &json!({ "FilterCriteria": criteria })).await;
        if *admitted {
            assert_eq!(
                (updated.status, updated.json().get("FilterCriteria").cloned()),
                (202, Some(criteria.clone())),
                "case {case:?}: a pattern this service reads must be stored, got {} {}",
                updated.status,
                updated.text()
            );
            continue;
        }
        assert_eq!(
            (
                updated.status,
                updated.error_code().contains(INVALID_PARAMETER),
                refusal_message(&updated)
            ),
            (400, true, Some(INVALID_FILTER_MESSAGE.to_string())),
            "case {case:?}: the update must be refused with the fixed sentence, got {} {}",
            updated.status,
            updated.text()
        );
        let read = get_mapping(&router, &uuid).await;
        assert_eq!(
            (read.status, read.json().get("FilterCriteria").cloned()),
            (200, Some(stored.clone())),
            "case {case:?}: a refused update leaves the stored criteria alone, got {} {}",
            read.status,
            read.text()
        );
    }
}

/// RULE: a mapping whose criteria an update refused is unchanged, so the REPAIR of that
/// update — the same mapping, with criteria this service reads — is accepted and stored.
#[tokio::test]
async fn a_refused_filter_update_leaves_the_mapping_open_to_a_valid_one() {
    let case = "a malformed filter refused, then the corrected one";
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    create_function(&router, "esm-filter-repair-fn").await;
    let uuid = given_enabled_mapping(
        &router,
        case,
        &create_request(
            "esm-filter-repair-fn",
            Some(DYNAMODB_SOURCE),
            &json!({ "StartingPosition": "LATEST", "BatchSize": 5 }),
        ),
    )
    .await;

    let malformed = update_mapping(
        &router,
        &uuid,
        &json!({ "FilterCriteria": { "Filters": [{ "Pattern": r#"{"eventName":"INSERT"}"# }] } }),
    )
    .await;
    assert_eq!(
        (malformed.status, refusal_message(&malformed)),
        (400, Some(INVALID_FILTER_MESSAGE.to_string())),
        "case {case:?}: the malformed criteria must be refused, got {} {}",
        malformed.status,
        malformed.text()
    );

    let corrected = json!({ "Filters": [{ "Pattern": r#"{"eventName":["INSERT"]}"# }] });
    let repaired = update_mapping(&router, &uuid, &json!({ "FilterCriteria": corrected })).await;
    assert_eq!(
        repaired.status,
        202,
        "case {case:?}: the corrected criteria must be accepted, got {} {}",
        repaired.status,
        repaired.text()
    );
    // The whole configuration the update answers is the mapping's, read back out of the
    // store: the members a mapping on a stream carries, and the criteria this call stated.
    for (member, expected) in [
        ("UUID", json!(uuid)),
        ("FilterCriteria", corrected.clone()),
        ("EventSourceArn", json!(DYNAMODB_SOURCE)),
        ("BatchSize", json!(5)),
        ("StartingPosition", json!("LATEST")),
        ("State", json!("Updating")),
        ("StateTransitionReason", json!("User action")),
    ] {
        assert_eq!(
            repaired.json().get(member),
            Some(&expected),
            "case {case:?}: the accepted update reports {member} as {expected}, got {}",
            repaired.text()
        );
    }
    let read = get_mapping(&router, &uuid).await;
    assert_eq!(
        (read.status, read.json().get("FilterCriteria").cloned()),
        (200, Some(corrected)),
        "case {case:?}: the corrected criteria must be stored, got {} {}",
        read.status,
        read.text()
    );
}

/// RULE: `FilterCriteria` carrying no filter at all are NO criteria — "to delete the filter
/// criteria, specify an empty FilterCriteria object" — so a create stating them stores none
/// and an update stating them deletes the criteria the mapping carried, and every reader then
/// reports the member not at all.
#[tokio::test]
async fn criteria_carrying_no_filter_are_no_criteria() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);

    // (case, the criteria carrying no filter)
    let rows: Vec<(&str, Value)> = vec![
        ("an empty criteria object", json!({})),
        ("an empty list of filters", json!({ "Filters": [] })),
        ("one filter carrying no pattern", json!({ "Filters": [{}] })),
    ];

    for (index, (case, empty)) in rows.iter().enumerate() {
        // A create stating them stores no criteria at all.
        let created_with = format!("esm-empty-criteria-{index}");
        create_function(&router, &created_with).await;
        let created = create_mapping(
            &router,
            &create_request(
                &created_with,
                Some(QUEUE_SOURCE),
                &json!({ "FilterCriteria": empty }),
            ),
        )
        .await;
        assert_eq!(
            (created.status, created.json().get("FilterCriteria").cloned()),
            (202, None),
            "case {case:?}: criteria that carry no filter are no criteria, got {} {}",
            created.status,
            created.text()
        );

        // And an update stating them deletes the criteria the mapping carried.
        let function = format!("esm-delete-criteria-{index}");
        create_function(&router, &function).await;
        let stored = json!({ "Filters": [{ "Pattern": r#"{"eventName":["INSERT"]}"# }] });
        let uuid = given_enabled_mapping(
            &router,
            case,
            &create_request(
                &function,
                Some(QUEUE_SOURCE),
                &json!({ "FilterCriteria": stored }),
            ),
        )
        .await;
        let carried = get_mapping(&router, &uuid).await;
        assert_eq!(
            carried.json().get("FilterCriteria"),
            Some(&stored),
            "case {case:?}: the mapping must carry the criteria it was created with, got {}",
            carried.text()
        );
        let cleared = update_mapping(&router, &uuid, &json!({ "FilterCriteria": empty })).await;
        assert_eq!(
            (cleared.status, cleared.json().get("FilterCriteria").cloned()),
            (202, None),
            "case {case:?}: the update must delete the criteria, got {} {}",
            cleared.status,
            cleared.text()
        );
        let read = get_mapping(&router, &uuid).await;
        assert_eq!(
            (read.status, read.json().get("FilterCriteria").cloned()),
            (200, None),
            "case {case:?}: the deleted criteria must be gone from the store, got {} {}",
            read.status,
            read.text()
        );
    }
}

/// RULE: the filters of a listing SELECT among the stored mappings — a `FunctionName` or an
/// `EventSourceArn` that matches none of them selects none, so the answer is HTTP 200 with an
/// empty `EventSourceMappings` list rather than a refusal.
#[tokio::test]
async fn a_listing_filter_that_matches_nothing_answers_an_empty_list() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    // One function with a mapping and one without, so a filter that matches nothing is told
    // apart from a filter that matches the stored mapping.
    create_function(&router, "esm-lookup-mapped").await;
    create_function(&router, "esm-lookup-bare").await;
    let mapped = create_mapping(
        &router,
        &create_request("esm-lookup-mapped", Some(QUEUE_SOURCE), &json!({})),
    )
    .await;
    assert_eq!(
        mapped.status,
        202,
        "the mapping the matching rows select must exist, got {} {}",
        mapped.status,
        mapped.text()
    );
    let mapped_uuid = mapped.string("UUID").expect("the create answers a UUID");

    // (case, the query, the UUIDs it must report)
    let rows: Vec<(&str, String, Vec<String>)> = vec![
        (
            "a function this account does not hold",
            "FunctionName=esm-lookup-ghost".to_string(),
            Vec::new(),
        ),
        (
            "the arn of a function this account does not hold",
            format!("FunctionName=arn:aws:lambda:{REGION}:{ACCOUNT}:function:esm-lookup-ghost"),
            Vec::new(),
        ),
        (
            "a version of a held function that does not exist",
            "FunctionName=esm-lookup-mapped:7".to_string(),
            Vec::new(),
        ),
        (
            "a held function with no mapping of its own",
            "FunctionName=esm-lookup-bare".to_string(),
            Vec::new(),
        ),
        (
            "an event source nothing maps",
            format!("EventSourceArn=arn:aws:sqs:{REGION}:{ACCOUNT}:esm-lookup-unmapped"),
            Vec::new(),
        ),
        (
            "a held function and the queue it maps at once",
            format!("FunctionName=esm-lookup-mapped&EventSourceArn={QUEUE_SOURCE}"),
            vec![mapped_uuid.clone()],
        ),
        (
            "a function this account does not hold beside the mapped queue",
            format!("FunctionName=esm-lookup-ghost&EventSourceArn={QUEUE_SOURCE}"),
            Vec::new(),
        ),
        (
            "the held function alone",
            "FunctionName=esm-lookup-mapped".to_string(),
            vec![mapped_uuid.clone()],
        ),
    ];

    for (case, query, expected) in rows {
        let listed = list_mappings(&router, &query).await;
        assert_eq!(
            listed.status, 200,
            "case {case:?}: a filter is not a lookup, so {query:?} must answer 200, got {} {}",
            listed.status,
            listed.text()
        );
        assert_eq!(
            listed.json().get("EventSourceMappings"),
            Some(&json!(listed.mappings())),
            "case {case:?}: the answer carries the EventSourceMappings member, got {}",
            listed.text()
        );
        assert_eq!(
            listed.listed_uuids(),
            expected,
            "case {case:?}: {query:?} must report {expected:?}, got {}",
            listed.text()
        );
    }
}

/// One row of the listing's ARN table: the `EventSourceArn` the query carries and the whole
/// refusal it answers.
struct ArnRow {
    case: &'static str,
    event_source: String,
    message: String,
}

/// The rows: a resource of another vendor, and the wrong resource type of each source vendor
/// the member enumerates — `smithy_query(query="doc",
/// shape="ListEventSourceMappingsRequest", member="EventSourceArn")`: "Amazon Kinesis – The
/// ARN of the data stream or a stream consumer. Amazon DynamoDB Streams – The ARN of the
/// stream. Amazon Simple Queue Service – The ARN of the queue. Amazon Managed Streaming for
/// Apache Kafka – The ARN of the cluster or the ARN of the VPC connection. Amazon MQ – The
/// ARN of the broker. Amazon DocumentDB – The ARN of the DocumentDB change stream."
fn arn_rows() -> Vec<ArnRow> {
    let expected_format = |vendor: &str, resource: &str| {
        format!(
            "Unrecognized event source. Expected arn of format \
             arn:aws:{vendor}:{REGION}:{EXAMPLE_ACCOUNT}:{resource}"
        )
    };
    vec![
        ArnRow {
            case: "a dynamodb table rather than its stream",
            event_source: format!("arn:aws:dynamodb:{REGION}:{ACCOUNT}:table/orders"),
            message: expected_format("dynamodb", "table/TableName/stream/StreamId"),
        },
        ArnRow {
            case: "a kinesis application rather than a data stream",
            event_source: format!("arn:aws:kinesis:{REGION}:{ACCOUNT}:application/orders"),
            message: expected_format("kinesis", "stream/StreamName"),
        },
        ArnRow {
            case: "an mq configuration rather than a broker",
            event_source: format!("arn:aws:mq:{REGION}:{ACCOUNT}:configuration:orders:c-1111"),
            message: expected_format("mq", "broker:brokerName:brokerId"),
        },
        ArnRow {
            case: "an msk configuration rather than a cluster",
            event_source: format!("arn:aws:kafka:{REGION}:{ACCOUNT}:configuration/orders/1"),
            message: expected_format("kafka", "cluster/ClusterName/ClusterId"),
        },
        ArnRow {
            case: "a database instance rather than a documentdb cluster",
            event_source: format!("arn:aws:rds:{REGION}:{ACCOUNT}:db:orders"),
            message: expected_format("rds", "cluster:ClusterName"),
        },
        ArnRow {
            case: "a topic, whose vendor is no source service at all",
            event_source: format!("arn:aws:sns:{REGION}:{ACCOUNT}:orders"),
            message: format!(
                "Unrecognized event source. Unsupported source arn : \
                 arn:aws:sns:{REGION}:{ACCOUNT}:orders"
            ),
        },
    ]
}

/// RULE: an `EventSourceArn` that names no event source is refused by the LISTING with the
/// modelled 400 — the same reading of the member the create makes — rather than answered as a
/// filter that matches nothing.
#[tokio::test]
async fn a_listing_by_an_arn_that_names_no_event_source_is_refused() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    create_function(&router, "esm-arn-filter-fn").await;

    for row in arn_rows() {
        let case = row.case;
        let listed = list_mappings(&router, &format!("EventSourceArn={}", row.event_source)).await;
        assert_eq!(
            (listed.status, listed.error_code().contains(INVALID_PARAMETER)),
            (400, true),
            "case {case:?}: {} names no event source, so the listing must be refused, got {} {}",
            row.event_source,
            listed.status,
            listed.text()
        );
        assert_eq!(
            refusal_message(&listed).as_deref(),
            Some(row.message.as_str()),
            "case {case:?}: the refusal states the ARN format the source is addressed by, got {}",
            listed.text()
        );
        // The create reads the same member the same way, so the two answers agree.
        let created = create_mapping(
            &router,
            &create_request("esm-arn-filter-fn", Some(&row.event_source), &json!({})),
        )
        .await;
        assert_eq!(
            (created.status, refusal_message(&created)),
            (400, Some(row.message.clone())),
            "case {case:?}: the create refuses the same value with the same sentence, got {} {}",
            created.status,
            created.text()
        );
    }
}

/// RULE: an `EventSourceArn` naming a BROKER is held to that broker existing — a broker
/// Amazon MQ does not hold is the modelled not-found (HTTP 404) and no mapping is created,
/// and a broker it does hold is mapped and reads back.
#[tokio::test]
async fn a_mapping_onto_a_broker_takes_a_broker_that_exists() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    create_function(&router, "esm-broker-fn").await;

    let absent = create_mapping(
        &router,
        &create_request(
            "esm-broker-fn",
            Some(ABSENT_BROKER_SOURCE),
            &json!({ "Queues": ["order-jobs"] }),
        ),
    )
    .await;
    assert_eq!(
        (absent.status, absent.error_code().contains(NOT_FOUND)),
        (404, true),
        "a broker Amazon MQ does not hold must be answered with the modelled 404, got {} {}",
        absent.status,
        absent.text()
    );
    assert_eq!(
        refusal_message(&absent).as_deref(),
        Some("The broker resource: eventSourceArn you requested does not exist."),
        "the refusal names the kind of resource that is missing, got {}",
        absent.text()
    );
    let listed = list_mappings(&router, "FunctionName=esm-broker-fn").await;
    assert_eq!(
        (listed.status, listed.listed_uuids()),
        (200, Vec::new()),
        "a refused create stores no mapping, got {} {}",
        listed.status,
        listed.text()
    );

    // The same request, once the broker exists in the service that owns brokers.
    given_broker(&router, BROKER_NAME).await;
    let created = create_mapping(
        &router,
        &create_request(
            "esm-broker-fn",
            Some(BROKER_SOURCE),
            &json!({ "Queues": ["order-jobs"] }),
        ),
    )
    .await;
    assert_eq!(
        created.status,
        202,
        "a broker Amazon MQ holds must be mapped, got {} {}",
        created.status,
        created.text()
    );
    let uuid = created.string("UUID").expect("the create answers a UUID");
    let fetched = get_mapping(&router, &uuid).await;
    assert_eq!(
        (fetched.status, fetched.string("EventSourceArn")),
        (200, Some(BROKER_SOURCE.to_string())),
        "the stored mapping reports the broker it was created against, got {} {}",
        fetched.status,
        fetched.text()
    );
}
