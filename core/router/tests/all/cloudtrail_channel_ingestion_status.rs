// SPDX-License-Identifier: CC0-1.0

//! A CloudTrail Lake channel's INGESTION TABLE — `GetChannel$IngestionStatus`, "A table
//! showing information about the most recent successful and failed attempts to ingest
//! events" — as a view of what the channel's data plane recorded.
//!
//! Ingestion happens in `cloudtrail-data` (`PutAuditEvents`) while the channel RESOURCE
//! belongs to the CloudTrail control plane: two service crates, two stores in the one
//! shared engine. A service-level suite mounts one of them, so neither can see the step
//! these tests are about — that an ingest attempt the data plane decided is what the
//! control plane's report of that channel then states, and that a channel nothing has
//! attempted to ingest onto has no table at all.
//!
//! Each test is a TABLE whose rows differ along what the rule generalises over: how many
//! events were accepted, over how many calls, whether the most recent attempt was accepted
//! or refused, which refusal class refused it, and in which order acceptances and refusals
//! arrived. The case the issue cited is one row.
//!
//! The wire helpers are the ones `cloudtrail_lake_custom_event_ingestion` already states
//! for this service pair, so all three suites drive one client.
//!
//! Assembling the router costs stack (see `boot_serve.rs`), so every test runs on an
//! explicitly sized thread.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_cloudtraildata_cloudtraildataservice::state::MAX_EVENT_DATA_BYTES;

use crate::cloudtrail_lake_custom_event_ingestion::{
    authorization, control_plane, create_channel, create_event_data_store, entries, error_code,
    on_a_router, DATA_PLANE_SIGNING_NAME,
};

/// How many characters a `UUID` is written with (`com.amazonaws.cloudtrail#UUID`:
/// `@length 36`, `@pattern ^[a-f0-9\-]+$`), which is the shape
/// `IngestionStatus$LatestIngestionSuccessEventID` and
/// `IngestionStatus$LatestIngestionAttemptEventID` carry.
const UUID_LEN: usize = 36;

/// One event a row submits: `AuditEvent$id`, its `eventData`, the
/// `eventDataChecksum` it carries (when it carries one), and the per-event outcome the
/// data plane's validation gives it — the empty string for an acceptable event, else the
/// `ResultErrorEntry$errorCode` it is refused under. Those codes are the published
/// catalog of `ResultErrorEntry$errorCode` ("Possible error codes include: FieldTooLong,
/// FieldNotFound, InvalidChecksum, InvalidData, InvalidRecipient, InvalidEventSource,
/// AccountNotSubscribed, Throttling, and InternalFailure").
#[derive(Debug, Clone)]
struct Submitted {
    id: String,
    data: String,
    checksum: Option<String>,
    expected_error_code: &'static str,
}

/// An acceptable event: a JSON audit-event object under `id`, carrying no checksum.
fn accepted(id: &str) -> Submitted {
    Submitted {
        id: id.to_string(),
        data: r#"{"eventSource":"custom.app","eventName":"ReproAction"}"#.to_string(),
        checksum: None,
        expected_error_code: "",
    }
}

/// An event refused because its `eventDataChecksum` is not the base64-SHA256 of its own
/// `eventData` — the case the issue's reproduction cites.
fn refused_checksum(id: &str) -> Submitted {
    Submitted {
        id: id.to_string(),
        data: r#"{"eventSource":"custom.app","eventName":"Checksum"}"#.to_string(),
        // Well-formed base64 of 32 bytes, so it is a checksum in form and simply not
        // this payload's.
        checksum: Some("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=".to_string()),
        expected_error_code: "InvalidChecksum",
    }
}

/// An event refused because its `eventData` is not a JSON object carrying event content.
fn refused_data(id: &str) -> Submitted {
    Submitted {
        id: id.to_string(),
        data: "not an audit event at all".to_string(),
        checksum: None,
        expected_error_code: "InvalidData",
    }
}

/// An event refused because its `eventData` alone is past the documented 1 MB an event
/// may carry (`MAX_EVENT_DATA_BYTES`, this emulator's one statement of that bound).
fn refused_too_long(id: &str) -> Submitted {
    Submitted {
        id: id.to_string(),
        data: payload_of_len(MAX_EVENT_DATA_BYTES + 1),
        checksum: None,
        expected_error_code: "FieldTooLong",
    }
}

/// A JSON audit event payload of the documented shape, `len` bytes long.
fn payload_of_len(len: usize) -> String {
    let prefix = r#"{"eventSource":"custom.app","eventName":"Login","pad":""#;
    let suffix = r#""}"#;
    assert!(
        len >= prefix.len() + suffix.len(),
        "payload_of_len({len}): too short to build a JSON object"
    );
    format!(
        "{prefix}{}{suffix}",
        "p".repeat(len - prefix.len() - suffix.len())
    )
}

/// `PutAuditEvents` with a batch that may carry per-event checksums — the sibling suite's
/// client sends `id` and `eventData` only, and a refused CHECKSUM is one of the outcomes
/// this suite's rule has to cover.
async fn ingest(
    router: &MultiServiceRouter,
    region: &str,
    channel_arn: &str,
    events: &[Submitted],
) -> (u16, Value) {
    let body = json!({
        "auditEvents": events
            .iter()
            .map(|event| {
                let mut entry = json!({ "id": event.id, "eventData": event.data });
                if let Some(checksum) = &event.checksum {
                    entry["eventDataChecksum"] = Value::String(checksum.clone());
                }
                entry
            })
            .collect::<Vec<_>>(),
    });
    let request = http::Request::builder()
        .method("POST")
        .uri(format!(
            "https://127.0.0.1:9297/PutAuditEvents?channelArn={channel_arn}"
        ))
        .header("host", "127.0.0.1:9297")
        .header("content-type", "application/json")
        .header(
            "authorization",
            authorization(region, DATA_PLANE_SIGNING_NAME),
        )
        .body(bytes::Bytes::from(
            serde_json::to_vec(&body).expect("serialize the request body"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let body = serde_json::from_slice(response.body().as_ref()).unwrap_or(Value::Null);
    (status, body)
}

/// Create an event data store and a `Custom` channel logging to it in `region`, and answer
/// the channel's ARN. One Region holds at most one channel per source, so each row homes
/// its channel in a Region of its own.
async fn channel_in(router: &MultiServiceRouter, region: &str, name: &str, case: &str) -> String {
    let (status, store) = create_event_data_store(router, region, name, true).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateEventDataStore must succeed, got {status} with {store}"
    );
    let store_arn = store["EventDataStoreArn"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    create_channel(router, region, name, &store_arn, case).await
}

/// `GetChannel`'s answer for `channel_arn`, which must report the channel.
async fn get_channel(
    router: &MultiServiceRouter,
    region: &str,
    channel_arn: &str,
    case: &str,
) -> Value {
    let (status, body) =
        control_plane(router, region, "GetChannel", &json!({ "Channel": channel_arn })).await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetChannel must report the channel, got {status} with {body}"
    );
    body
}

/// The `IngestionStatus` table a `GetChannel` answer carries, or `None` when it carries
/// none at all.
fn ingestion_status(body: &Value) -> Option<Value> {
    match body.get("IngestionStatus") {
        None | Some(Value::Null) => None,
        Some(table) => Some(table.clone()),
    }
}

/// One member of the ingestion table as a string, or `None` when the table omits it.
fn member(table: &Value, name: &str) -> Option<String> {
    match table.get(name) {
        None | Some(Value::Null) => None,
        Some(value) => Some(
            value
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| value.to_string()),
        ),
    }
}

/// A timestamp member as the seconds-since-epoch an `awsJson1_1` body carries it as.
fn time_member(table: &Value, name: &str) -> Option<f64> {
    table.get(name).and_then(Value::as_f64)
}

/// Whether `value` is written the way the model writes a `UUID`
/// (`@length 36`, `@pattern ^[a-f0-9\-]+$`), which is what an ingestion event id is.
fn is_modelled_uuid(value: &str) -> bool {
    value.len() == UUID_LEN
        && value
            .chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c) || c == '-')
}

/// What one row expects of the channel's table, accumulated from the answers
/// `PutAuditEvents` gave for the events the row submitted.
#[derive(Debug, Default)]
struct Recorded {
    /// The `eventID` of the most recently ACCEPTED event, if any was accepted.
    success_event_id: Option<String>,
    /// The `errorCode` of the most recently REFUSED event, if any was refused.
    error_code: Option<String>,
    /// Whether the most recent attempt of all was accepted.
    latest_attempt_accepted: bool,
    /// Whether anything was attempted at all.
    attempted: bool,
}

/// Submit one batch, assert every event got the outcome its row states, and fold those
/// outcomes into `recorded`.
async fn submit(
    router: &MultiServiceRouter,
    region: &str,
    channel_arn: &str,
    events: &[Submitted],
    case: &str,
    call: usize,
    recorded: &mut Recorded,
) {
    let (status, reply) = ingest(router, region, channel_arn, events).await;
    assert_eq!(
        status, 200,
        "case {case:?} call {call}: a batch of per-event outcomes is HTTP 200, got {status} \
         with {reply}"
    );
    assert_eq!(
        error_code(&reply),
        "",
        "case {case:?} call {call}: expected no operation error, got {reply}"
    );
    let successful = entries(&reply, "successful", "eventID");
    let failed = entries(&reply, "failed", "errorCode");
    for event in events {
        let accepted = successful
            .iter()
            .find(|(id, _)| *id == event.id)
            .map(|(_, event_id)| event_id.clone());
        let refused = failed
            .iter()
            .find(|(id, _)| *id == event.id)
            .map(|(_, code)| code.clone());
        if event.expected_error_code.is_empty() {
            let Some(event_id) = accepted else {
                panic!(
                    "case {case:?} call {call}: the event {:?} is acceptable, so it must be \
                     listed in successful, got successful {successful:?} and failed {failed:?}",
                    event.id
                );
            };
            recorded.success_event_id = Some(event_id);
            recorded.latest_attempt_accepted = true;
        } else {
            assert_eq!(
                refused.as_deref(),
                Some(event.expected_error_code),
                "case {case:?} call {call}: the event {:?} must be refused under \
                 {:?}, got failed {failed:?}",
                event.id,
                event.expected_error_code
            );
            recorded.error_code = Some(event.expected_error_code.to_string());
            recorded.latest_attempt_accepted = false;
        }
        recorded.attempted = true;
    }
}

/// One row of the ingestion-table test: a Region and name of its own, and the batches it
/// submits in order.
struct Row {
    case: &'static str,
    region: &'static str,
    name: &'static str,
    batches: Vec<Vec<Submitted>>,
}

/// RULE: a channel's ingestion table reports the attempts its data plane recorded for that
/// channel — the time and assigned event id of the most recently ACCEPTED event, the time
/// of the most recent attempt of any outcome together with that attempt's event id where
/// one was assigned, and the error code of the most recently REFUSED attempt — and no
/// member states a fact the data plane did not record.
///
/// Rows differ along how many events were accepted, over how many calls, whether the most
/// recent attempt was accepted or refused, which refusal class refused it, and the order
/// acceptances and refusals arrived in.
#[test]
fn a_channels_ingestion_table_reports_what_its_data_plane_recorded() {
    on_a_router("cloudtrail-channel-ingestion-status", |router| {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("a current-thread runtime");
        runtime.block_on(async {
            let rows: Vec<Row> = vec![
                Row {
                    case: "one accepted event",
                    region: "us-west-2",
                    name: "ingest-one",
                    batches: vec![vec![accepted("evt-1")]],
                },
                Row {
                    case: "three accepted events over two calls",
                    region: "us-east-1",
                    name: "ingest-many",
                    batches: vec![
                        vec![accepted("evt-2"), accepted("evt-3")],
                        vec![accepted("evt-4")],
                    ],
                },
                Row {
                    case: "a refused checksum after a success",
                    region: "eu-central-1",
                    name: "ingest-then-refuse",
                    batches: vec![
                        vec![accepted("evt-5")],
                        vec![refused_checksum("evt-6")],
                    ],
                },
                Row {
                    case: "a success after a refused payload",
                    region: "ap-southeast-2",
                    name: "refuse-then-ingest",
                    batches: vec![vec![refused_data("evt-7")], vec![accepted("evt-8")]],
                },
                Row {
                    case: "nothing but a refusal, on size",
                    region: "eu-west-1",
                    name: "refuse-only",
                    batches: vec![vec![refused_too_long("evt-9")]],
                },
                Row {
                    case: "one mixed batch: accepted, refused, accepted",
                    region: "sa-east-1",
                    name: "ingest-mixed",
                    batches: vec![vec![
                        accepted("evt-10"),
                        refused_data("evt-11"),
                        accepted("evt-12"),
                    ]],
                },
            ];

            for row in &rows {
                let Row {
                    case,
                    region,
                    name,
                    batches,
                } = row;
                let channel_arn = channel_in(&router, region, name, case).await;

                // Before anything is attempted there is no table to report.
                let fetched = get_channel(&router, region, &channel_arn, case).await;
                assert!(
                    ingestion_status(&fetched).is_none(),
                    "case {case:?}: a channel nothing has attempted to ingest onto has no \
                     ingestion table, got {fetched}"
                );

                let mut recorded = Recorded::default();
                for (call, events) in batches.iter().enumerate() {
                    submit(
                        &router,
                        region,
                        &channel_arn,
                        events,
                        case,
                        call + 1,
                        &mut recorded,
                    )
                    .await;
                }

                let fetched = get_channel(&router, region, &channel_arn, case).await;
                let Some(table) = ingestion_status(&fetched) else {
                    panic!(
                        "case {case:?}: {} attempt(s) were decided on this channel, so \
                         GetChannel must report an ingestion table, got {fetched}",
                        batches.iter().map(Vec::len).sum::<usize>()
                    );
                };

                // Reading the table twice reports the same facts: a report changes nothing.
                let again = get_channel(&router, region, &channel_arn, case).await;
                assert_eq!(
                    ingestion_status(&again),
                    Some(table.clone()),
                    "case {case:?}: reading the channel must not change what it reports, got \
                     {again}"
                );

                assert!(
                    recorded.attempted,
                    "case {case:?}: every row submits at least one event"
                );

                // The most recent SUCCESS: the event id the data plane assigned the last
                // event it accepted, and a time it reports alongside it.
                let reported_success = member(&table, "LatestIngestionSuccessEventID");
                assert_eq!(
                    reported_success, recorded.success_event_id,
                    "case {case:?}: LatestIngestionSuccessEventID must be the eventID \
                     assigned to the most recently ingested event, got {reported_success:?}"
                );
                let success_time = time_member(&table, "LatestIngestionSuccessTime");
                assert_eq!(
                    success_time.is_some(),
                    recorded.success_event_id.is_some(),
                    "case {case:?}: LatestIngestionSuccessTime is stated exactly when an \
                     event was ingested, got {success_time:?} beside \
                     {reported_success:?}"
                );
                if let Some(event_id) = &reported_success {
                    assert!(
                        is_modelled_uuid(event_id),
                        "case {case:?}: the reported success eventID must be within the \
                         modelled UUID form, got {event_id:?}"
                    );
                }

                // The most recent FAILURE: the per-event error code the last refused
                // attempt carried, and nothing when nothing was refused.
                let reported_error = member(&table, "LatestIngestionErrorCode");
                assert_eq!(
                    reported_error, recorded.error_code,
                    "case {case:?}: LatestIngestionErrorCode must be the errorCode of the \
                     most recently refused attempt, got {reported_error:?}"
                );

                // The most recent ATTEMPT, whatever its outcome: always timed, and
                // carrying an event id only where the data plane assigned one.
                let attempt_time = time_member(&table, "LatestIngestionAttemptTime");
                assert!(
                    attempt_time.is_some(),
                    "case {case:?}: an attempt was decided, so \
                     LatestIngestionAttemptTime must be stated, got {table}"
                );
                let reported_attempt = member(&table, "LatestIngestionAttemptEventID");
                let expected_attempt = if recorded.latest_attempt_accepted {
                    recorded.success_event_id.clone()
                } else {
                    None
                };
                assert_eq!(
                    reported_attempt, expected_attempt,
                    "case {case:?}: LatestIngestionAttemptEventID must be the eventID of \
                     the most recent attempt, which exists only where that attempt was \
                     accepted, got {reported_attempt:?}"
                );
                if let (Some(attempted), Some(ingested)) = (attempt_time, success_time) {
                    assert!(
                        attempted >= ingested,
                        "case {case:?}: the most recent attempt cannot predate the most \
                         recent ingest, got attempt {attempted} and success {ingested}"
                    );
                }
            }
        });
    });
}

/// RULE: the ingestion table of a channel reports that channel's own attempts only — an
/// ingest onto one channel states nothing about another, and a channel whose only
/// attempts were refused reports a failure with no success beside it.
#[test]
fn one_channels_ingests_say_nothing_about_another_channel() {
    on_a_router("cloudtrail-channel-ingestion-isolation", |router| {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("a current-thread runtime");
        runtime.block_on(async {
            const INGESTED_REGION: &str = "us-east-2";
            const REFUSED_REGION: &str = "ca-central-1";
            const UNTOUCHED_REGION: &str = "ap-northeast-1";

            let ingested =
                channel_in(&router, INGESTED_REGION, "isolation-ingested", "ingested").await;
            let refused =
                channel_in(&router, REFUSED_REGION, "isolation-refused", "refused").await;
            let untouched =
                channel_in(&router, UNTOUCHED_REGION, "isolation-untouched", "untouched").await;

            let mut on_ingested = Recorded::default();
            submit(
                &router,
                INGESTED_REGION,
                &ingested,
                &[accepted("iso-1")],
                "ingested",
                1,
                &mut on_ingested,
            )
            .await;
            let mut on_refused = Recorded::default();
            submit(
                &router,
                REFUSED_REGION,
                &refused,
                &[refused_checksum("iso-2")],
                "refused",
                1,
                &mut on_refused,
            )
            .await;

            // The channel that ingested: a success, no error code.
            let table = ingestion_status(
                &get_channel(&router, INGESTED_REGION, &ingested, "ingested").await,
            )
            .expect("the channel that ingested reports a table");
            assert_eq!(
                member(&table, "LatestIngestionSuccessEventID"),
                on_ingested.success_event_id,
                "case \"ingested\": the table must carry this channel's own assigned \
                 eventID, got {table}"
            );
            assert_eq!(
                member(&table, "LatestIngestionErrorCode"),
                None,
                "case \"ingested\": nothing was refused on this channel, so no error code \
                 may be stated, got {table}"
            );

            // The channel whose one attempt was refused: an error code, no success.
            let table = ingestion_status(
                &get_channel(&router, REFUSED_REGION, &refused, "refused").await,
            )
            .expect("the channel whose attempt was refused reports a table");
            assert_eq!(
                member(&table, "LatestIngestionErrorCode"),
                on_refused.error_code,
                "case \"refused\": the table must carry the error code that attempt was \
                 refused under, got {table}"
            );
            assert_eq!(
                member(&table, "LatestIngestionSuccessEventID"),
                None,
                "case \"refused\": nothing was ingested on this channel, so no success \
                 eventID may be stated, got {table}"
            );
            assert_eq!(
                time_member(&table, "LatestIngestionSuccessTime"),
                None,
                "case \"refused\": nothing was ingested on this channel, so no success time \
                 may be stated, got {table}"
            );

            // The channel nothing touched: no table at all, however much its neighbours
            // ingested.
            let fetched = get_channel(&router, UNTOUCHED_REGION, &untouched, "untouched").await;
            assert!(
                ingestion_status(&fetched).is_none(),
                "case \"untouched\": no attempt was ever made on this channel, so it must \
                 report no ingestion table, got {fetched}"
            );
        });
    });
}
