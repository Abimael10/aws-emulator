// SPDX-License-Identifier: CC0-1.0

//! What a LISTING of resource operation requests reports about each request it reports.
//!
//! The rule every case here holds the service to: **a listing of resource operation
//! requests reports, for each request, the record `GetResourceRequestStatus` resolves that
//! request's `RequestToken` to — the type, the primary identifier of the resource the
//! request acts on (`ProgressEvent$Identifier` is "The primary identifier for the
//! resource", which for a type whose registry schema points `primaryIdentifier` at an ARN
//! the owning service assigns is that ARN), the token, the operation, the operation status,
//! when the request was initiated and whatever that status says — and NO resource model:
//! the model of the resource a request left behind is surfaced by the per-request reads,
//! `GetResourceRequestStatus` for the request and `GetResource` for the resource, and never
//! by an enumeration of requests.**
//!
//! The rows differ along what the rule generalises over: the operation (`CREATE`, `UPDATE`,
//! `DELETE`), the status the request has reached (succeeded, failed, canceled, still
//! running), whether the request left a resource model behind at all, and which store the
//! type's resources live in — this service's own for `AWS::Kinesis::Stream`, and the owning
//! service's for `AWS::SNS::Topic`, whose primary identifier that service assigns as an ARN.
//!
//! `request_listing` holds the order, the filter criteria and the paging of the same
//! listing, and `arn_identified_resources` holds where an ARN-valued primary identifier
//! comes from; what is asserted here is the SHAPE of one summary.

use serde_json::{json, Value};

use super::emulator::{
    listing_shaped, progress, text, Emulator, RESOURCE_MODEL, SIGNING_REGION,
    STATUS_CANCEL_COMPLETE, STATUS_FAILED, STATUS_IN_PROGRESS, STATUS_SUCCESS,
};

/// A type whose resources live in this service's own store, so a case over it is about the
/// request record alone.
const STREAM: &str = "AWS::Kinesis::Stream";

/// A type whose resources live in the OWNING service's store and whose registry schema
/// points `primaryIdentifier` at the read-only ARN that service assigns
/// (`/properties/TopicArn`).
const TOPIC: &str = "AWS::SNS::Topic";

/// How many summaries one listing call here reports: far more than any case initiates, so
/// the request a case looks for is on the first page.
const EVERY_REQUEST: i64 = 100;

/// The members a summary of a request reports whatever status that request is in.
const ALWAYS_REPORTED: [&str; 6] = [
    "TypeName",
    "Identifier",
    "RequestToken",
    "Operation",
    "OperationStatus",
    "EventTime",
];

/// Every summary one `ListResourceRequests` page reports.
async fn summaries(emulator: &Emulator) -> Vec<Value> {
    let listed = emulator
        .cc_ok(
            "ListResourceRequests",
            json!({ "MaxResults": EVERY_REQUEST }),
        )
        .await;
    listed["ResourceRequestStatusSummaries"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// The summary the listing reports for `token`, required to be there.
async fn summary(emulator: &Emulator, case: &str, token: &str) -> Value {
    let reported = summaries(emulator).await;
    reported
        .iter()
        .find(|summary| text(summary, "RequestToken") == token)
        .cloned()
        .unwrap_or_else(|| {
            panic!("case {case:?}: the listing must report the request {token:?}, got {reported:?}")
        })
}

/// Assert `summary` reports the members every summary carries, and no resource model.
fn assert_summary_shape(case: &str, summary: &Value, operation: &str, status: &str) {
    assert!(
        summary.get(RESOURCE_MODEL).is_none(),
        "case {case:?}: a listing summary must carry no {RESOURCE_MODEL}, got {summary:?}"
    );
    for member in ALWAYS_REPORTED {
        assert!(
            summary.get(member).is_some_and(|value| !value.is_null()),
            "case {case:?}: the summary must report {member}, got {summary:?}"
        );
    }
    assert_eq!(
        text(summary, "Operation"),
        operation,
        "case {case:?}: expected the {operation} operation, got {summary:?}"
    );
    assert_eq!(
        text(summary, "OperationStatus"),
        status,
        "case {case:?}: expected the status {status}, got {summary:?}"
    );
}

/// The `ProgressEvent` `GetResourceRequestStatus` reports for `token`, required to succeed.
async fn status_event(emulator: &Emulator, case: &str, token: &str) -> Value {
    let (status, body) = emulator.status(token).await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetResourceRequestStatus expected HTTP 200, got {status} with body \
         {body:?}"
    );
    progress(&body)
}

/// Accept one create without waiting for it, answering its `RequestToken`.
async fn accept_create(emulator: &Emulator, type_name: &str, desired: &Value) -> String {
    let accepted = emulator.create_ok(type_name, desired, None).await;
    text(&accepted, "RequestToken")
}

/// Accept one patch without waiting for it, answering its `RequestToken`.
async fn accept_update(
    emulator: &Emulator,
    type_name: &str,
    identifier: &str,
    patch: &Value,
) -> String {
    let (status, body) = emulator.update(type_name, identifier, patch, None).await;
    assert_eq!(
        status, 200,
        "UpdateResource {identifier:?}: expected HTTP 200, got {status} with body {body:?}"
    );
    text(&progress(&body), "RequestToken")
}

/// Accept one delete without waiting for it, answering its `RequestToken`.
async fn accept_delete(emulator: &Emulator, type_name: &str, identifier: &str) -> String {
    let (status, body) = emulator.delete(type_name, identifier).await;
    assert_eq!(
        status, 200,
        "DeleteResource {identifier:?}: expected HTTP 200, got {status} with body {body:?}"
    );
    text(&progress(&body), "RequestToken")
}

/// One row of the table: what the case is, the request's token, the operation it ran, the
/// status it reached, the resource it acts on, and whether the record the status call
/// resolves carries a resource model.
struct Row {
    case: &'static str,
    token: String,
    operation: &'static str,
    status: &'static str,
    identifier: String,
    model: bool,
}

#[tokio::test]
async fn every_summary_a_listing_reports_omits_the_resource_model_and_nothing_else() {
    let emulator = Emulator::with_sns();

    // The independent requests of each phase are started together and waited for once, so
    // the suite waits one operation span per dependency level rather than one per request.
    let topic_created = accept_create(
        &emulator,
        TOPIC,
        &json!({ "TopicName": "orders", "DisplayName": "Before" }),
    )
    .await;
    let feed_created = accept_create(&emulator, STREAM, &json!({ "StreamName": "feed" })).await;
    let ledger_created = accept_create(&emulator, STREAM, &json!({ "StreamName": "ledger" })).await;
    let topic = text(&emulator.settled(&topic_created).await, "Identifier");
    let feed = text(&emulator.settled(&feed_created).await, "Identifier");
    let ledger = text(&emulator.settled(&ledger_created).await, "Identifier");

    // A patch on each store's type, a create the owning handler fails because the resource
    // is already there, and a create a cancel abandons — all against different resources,
    // so none of them contends with another.
    let topic_patched = accept_update(
        &emulator,
        TOPIC,
        &topic,
        &json!([{ "op": "replace", "path": "/DisplayName", "value": "After" }]),
    )
    .await;
    let feed_patched = accept_update(
        &emulator,
        STREAM,
        &feed,
        &json!([{ "op": "add", "path": "/Stage", "value": "prod" }]),
    )
    .await;
    let duplicate = accept_create(&emulator, STREAM, &json!({ "StreamName": "ledger" })).await;
    let abandoned = accept_create(&emulator, STREAM, &json!({ "StreamName": "never-made" })).await;
    let (cancelled, body) = emulator.cancel(&abandoned).await;
    assert_eq!(
        cancelled, 200,
        "CancelResourceRequest: expected HTTP 200, got {cancelled} with body {body:?}"
    );
    emulator.settled(&topic_patched).await;
    emulator.settled(&feed_patched).await;
    let failed = emulator.terminal(&duplicate).await;
    assert_eq!(
        text(&failed, "OperationStatus"),
        STATUS_FAILED,
        "a second create of the same resource must fail, got {failed:?}"
    );
    emulator
        .reaching(&abandoned, &[STATUS_CANCEL_COMPLETE])
        .await;

    let feed_deleted = accept_delete(&emulator, STREAM, &feed).await;
    emulator.settled(&feed_deleted).await;

    // The rows span the three operations, the three terminal statuses, both stores, and
    // both a request that left a resource model behind and one that left none.
    let rows = [
        Row {
            case: "a create of a type whose owning service assigns an ARN identifier",
            token: topic_created,
            operation: "CREATE",
            status: STATUS_SUCCESS,
            identifier: topic.clone(),
            model: true,
        },
        Row {
            case: "a patch of that same type",
            token: topic_patched,
            operation: "UPDATE",
            status: STATUS_SUCCESS,
            identifier: topic.clone(),
            model: true,
        },
        Row {
            case: "a create of a type this service's own store holds",
            token: feed_created,
            operation: "CREATE",
            status: STATUS_SUCCESS,
            identifier: feed.clone(),
            model: true,
        },
        Row {
            case: "a patch of that same type",
            token: feed_patched,
            operation: "UPDATE",
            status: STATUS_SUCCESS,
            identifier: feed.clone(),
            model: true,
        },
        Row {
            case: "a delete, which leaves no resource behind to report a model of",
            token: feed_deleted,
            operation: "DELETE",
            status: STATUS_SUCCESS,
            identifier: feed,
            model: false,
        },
        Row {
            case: "a create the owning handler failed",
            token: duplicate,
            operation: "CREATE",
            status: STATUS_FAILED,
            identifier: ledger,
            model: false,
        },
        Row {
            case: "a create a cancel abandoned",
            token: abandoned,
            operation: "CREATE",
            status: STATUS_CANCEL_COMPLETE,
            identifier: "never-made".to_string(),
            model: false,
        },
    ];

    for row in rows {
        let summary = summary(&emulator, row.case, &row.token).await;
        assert_summary_shape(row.case, &summary, row.operation, row.status);
        assert_eq!(
            text(&summary, "Identifier"),
            row.identifier,
            "case {:?}: expected the resource the request acts on, got {summary:?}",
            row.case
        );
        // Every member of the record EXCEPT the resource model is the one the status call
        // reports, so the listing drops that member and nothing else — a failed request's
        // `StatusMessage` and `ErrorCode` included.
        let event = status_event(&emulator, row.case, &row.token).await;
        assert_eq!(
            summary,
            listing_shaped(&event),
            "case {:?}: the listing must report the status call's record without its \
             {RESOURCE_MODEL}, got {summary:?} and {event:?}",
            row.case
        );
        // And the model itself is still there to be read for a request that left one: the
        // listing projects it away rather than the record having lost it.
        let reported = event.get(RESOURCE_MODEL).cloned().unwrap_or(Value::Null);
        if row.model {
            let model: Value =
                serde_json::from_str(reported.as_str().unwrap_or_default()).unwrap_or(Value::Null);
            assert!(
                model.is_object(),
                "case {:?}: the status call must still surface the resource model, got \
                 {reported:?}",
                row.case
            );
        } else {
            assert!(
                reported.is_null(),
                "case {:?}: this request left no resource model behind, got {reported:?}",
                row.case
            );
        }
    }

    // The ARN-valued identifier the summaries above report is the primary identifier the
    // type's schema points at: the ARN the owning service assigns, built from this
    // emulator's own partition and account, the Region the call was signed for, and the
    // topic name the create chose.
    assert_eq!(
        topic,
        engine::emulator_arn("sns", SIGNING_REGION, "orders"),
        "the topic's primary identifier must be the ARN form, got {topic:?}"
    );
}

#[tokio::test]
async fn a_summary_of_a_request_that_is_still_running_carries_no_resource_model_either() {
    let emulator = Emulator::with_sns();
    // A request is answered while the operation it started is still running, so it is
    // summarised before any resource model exists at all — and the listing of such a
    // request carries none either.
    let running = accept_create(&emulator, TOPIC, &json!({ "TopicName": "in-flight" })).await;
    let summary = summary(&emulator, "a running create", &running).await;
    // The status this summary reports is whichever one the request has reached by now, so
    // the case asserts the SHAPE rather than racing the clock that moves it.
    let reached = text(&summary, "OperationStatus");
    assert!(
        [STATUS_IN_PROGRESS, STATUS_SUCCESS].contains(&reached.as_str()),
        "case a running create: expected a create that is running or has completed, got \
         {summary:?}"
    );
    assert_summary_shape(
        &format!("a running create ({reached})"),
        &summary,
        "CREATE",
        &reached,
    );
    assert_eq!(
        text(&summary, "Identifier"),
        engine::emulator_arn("sns", SIGNING_REGION, "in-flight"),
        "case a running create: expected the ARN the owning service assigns, got {summary:?}"
    );
}
