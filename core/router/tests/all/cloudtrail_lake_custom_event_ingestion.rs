// SPDX-License-Identifier: CC0-1.0

//! The documented CloudTrail Lake CUSTOM-EVENT pipeline, end to end through the whole
//! router: create an event data store, create a `Custom` channel that logs to it, and
//! ingest a batch onto that channel with `cloudtrail-data`'s `PutAuditEvents`.
//!
//! The channel RESOURCE belongs to the CloudTrail control plane and its ingest endpoint
//! belongs to the cloudtrail-data data plane — two service crates, two stores in the one
//! shared engine. A service-level suite mounts one of them, so neither can see the step
//! this suite is about: that the call which MINTS the channel is what makes it usable in
//! the component serving its data plane, in that same call, and the call which RETIRES it
//! is what makes it unusable there.
//!
//! Each test is a TABLE whose rows differ along what the rule generalises over — the
//! Region the channel is homed in, how the destination store was created, the size of the
//! ingested batch and the shape of each payload — not only over the one case the issue
//! cited. The recorded case is one row.
//!
//! Assembling the router costs stack (see `boot_serve.rs`), so every test runs on an
//! explicitly sized thread.

use std::collections::HashSet;
use std::sync::Arc;

use aws_emulator_router::{build_router, InMemoryEngine};
use smithy_server_runtime::MultiServiceRouter;

/// Keep this at or above `boot_serve.rs`'s stack: the same assembly runs here.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The `X-Amz-Target` prefix every AWS SDK sends for the CloudTrail control plane: the
/// fully qualified Coral service name.
const CONTROL_PLANE_TARGET_PREFIX: &str = "com.amazonaws.cloudtrail.v20131101.CloudTrail_20131101.";

/// The signing name the control plane claims requests by.
const CONTROL_PLANE_SIGNING_NAME: &str = "cloudtrail";

/// The signing name the data plane claims requests by (`cloudtrail-data`'s `sigv4Name`),
/// which is what routes `PutAuditEvents` to it.
const DATA_PLANE_SIGNING_NAME: &str = "cloudtrail-data";

/// The account every ARN this emulator mints names.
pub(crate) const ACCOUNT_ID: &str = "000000000000";

/// The largest `AuditEventResultEntry$eventID` the model admits (`@length 1..128`).
const MAX_EVENT_ID_LEN: usize = 128;

/// A SigV4 `Authorization` header whose credential scope names `region` and
/// `signing_name` — the scope the router resolves the route key from. Request
/// authentication is off in these tests, so the signature value is never checked, and the
/// key id names no principal IAM or STS holds a record for, which is the position the
/// account's own published credential is in.
fn authorization(region: &str, signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 \
         Credential=AKIDEMULATOR/20260101/{region}/{signing_name}/aws4_request, \
         SignedHeaders=host;x-amz-date, Signature=0123456789abcdef"
    )
}

/// The answer the router gave, as a client sees it: the status line and the parsed body.
async fn dispatch(
    router: &MultiServiceRouter,
    request: http::Request<bytes::Bytes>,
) -> (u16, serde_json::Value) {
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let body = serde_json::from_slice(response.body().as_ref()).unwrap_or(serde_json::Value::Null);
    (status, body)
}

/// One CloudTrail control-plane call, under the dotted `awsJson1_1` target an SDK sends.
pub(crate) async fn control_plane(
    router: &MultiServiceRouter,
    region: &str,
    operation: &str,
    body: &serde_json::Value,
) -> (u16, serde_json::Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("https://127.0.0.1:9297/")
        .header("host", "127.0.0.1:9297")
        .header("content-type", "application/x-amz-json-1.1")
        .header(
            "authorization",
            authorization(region, CONTROL_PLANE_SIGNING_NAME),
        )
        .header(
            "x-amz-target",
            format!("{CONTROL_PLANE_TARGET_PREFIX}{operation}"),
        )
        .body(bytes::Bytes::from(
            serde_json::to_vec(body).expect("serialize the request body"),
        ))
        .expect("a well-formed request");
    dispatch(router, request).await
}

/// `cloudtrail-data`'s `PutAuditEvents`: `POST /PutAuditEvents?channelArn=…` with the
/// batch in the body, signed for the data plane's own signing name.
pub(crate) async fn put_audit_events(
    router: &MultiServiceRouter,
    region: &str,
    channel_arn: &str,
    external_id: Option<&str>,
    events: &[(String, String)],
) -> (u16, serde_json::Value) {
    let body = serde_json::json!({
        "auditEvents": events
            .iter()
            .map(|(id, data)| serde_json::json!({ "id": id, "eventData": data }))
            .collect::<Vec<_>>(),
    });
    // `externalId` is an optional `httpQuery` member, so a request that states none
    // carries no such parameter at all.
    let external_id = external_id
        .map(|value| format!("&externalId={value}"))
        .unwrap_or_default();
    let request = http::Request::builder()
        .method("POST")
        .uri(format!(
            "https://127.0.0.1:9297/PutAuditEvents?channelArn={channel_arn}{external_id}"
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
    dispatch(router, request).await
}

/// The error code a `restJson1` body carries in `__type`, empty on a success.
pub(crate) fn error_code(body: &serde_json::Value) -> String {
    body.get("__type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The sentence a refusal carries, under either spelling of the member.
pub(crate) fn message_of(body: &serde_json::Value) -> String {
    for member in ["message", "Message"] {
        if let Some(stated) = body.get(member).and_then(serde_json::Value::as_str) {
            return stated.to_string();
        }
    }
    String::new()
}

/// `(id, eventID)` pairs out of an output member, in the order the response listed them.
pub(crate) fn entries(body: &serde_json::Value, member: &str, value: &str) -> Vec<(String, String)> {
    body.get(member)
        .and_then(serde_json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(|item| {
                    let field = |name: &str| {
                        item.get(name)
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or_default()
                            .to_string()
                    };
                    (field("id"), field(value))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Whether `value` has the form the model gives `AuditEventResultEntry$eventID`
/// (`@pattern ^[-_A-Za-z0-9]+$`, `@length 1..128`).
fn is_modelled_event_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_EVENT_ID_LEN
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// An event data store the channels of a row log to, created over the wire. `documented`
/// adds the members the published ingestion pipeline passes — the `ActivityAuditLog`
/// advanced event selector a custom-event store collects under, a Region scope, a
/// retention period and termination protection — so the table spans both the minimal
/// create and the documented one.
pub(crate) async fn create_event_data_store(
    router: &MultiServiceRouter,
    region: &str,
    name: &str,
    documented: bool,
) -> (u16, serde_json::Value) {
    let mut request = serde_json::json!({ "Name": name });
    if documented {
        request["AdvancedEventSelectors"] = serde_json::json!([{
            "Name": "select-activity-audit-log",
            "FieldSelectors": [{ "Field": "eventCategory", "Equals": ["ActivityAuditLog"] }],
        }]);
        request["MultiRegionEnabled"] = serde_json::json!(false);
        request["RetentionPeriod"] = serde_json::json!(7);
        request["TerminationProtectionEnabled"] = serde_json::json!(false);
    }
    control_plane(router, region, "CreateEventDataStore", &request).await
}

/// Create a `Custom` channel logging to `store_arn`, and answer the ARN it minted.
pub(crate) async fn create_channel(
    router: &MultiServiceRouter,
    region: &str,
    name: &str,
    store_arn: &str,
    case: &str,
) -> String {
    let request = serde_json::json!({
        "Name": name,
        "Source": "Custom",
        "Destinations": [{ "Type": "EVENT_DATA_STORE", "Location": store_arn }],
    });
    let (status, body) = control_plane(router, region, "CreateChannel", &request).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateChannel must succeed, got {status} with {body}"
    );
    let channel_arn = body["ChannelArn"].as_str().unwrap_or_default().to_string();
    let expected_prefix = format!("arn:aws:cloudtrail:{region}:{ACCOUNT_ID}:channel/");
    assert!(
        channel_arn.starts_with(&expected_prefix),
        "case {case:?}: expected a channel ARN under {expected_prefix:?}, got \
         {channel_arn:?}"
    );
    channel_arn
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

/// Run `body` against a freshly assembled router, on a thread sized for that assembly.
pub(crate) fn on_a_router<F>(name: &'static str, body: F)
where
    F: FnOnce(MultiServiceRouter) + Send + 'static,
{
    std::thread::Builder::new()
        .name(name.to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(move || {
            let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
            let mut router = build_router(engine);
            // Signature verification is the emulator's concern, not a service's: pinned
            // off here rather than depending on the process-wide default.
            router.set_request_authentication(false);
            body(router);
        })
        .expect("spawn the router thread")
        .join()
        .unwrap_or_else(|_| panic!("{name} must pass"));
}

/// One row of the ingestion table: a Region of its own (a Region holds at most one
/// channel per source, so each row homes its channel in another one), the name its
/// resources carry, whether the destination store is created the documented way, and the
/// batch the row ingests as `(AuditEvent$id, AuditEvent$eventData)` pairs.
struct Row {
    case: &'static str,
    region: &'static str,
    name: &'static str,
    documented_store: bool,
    batch: Vec<(String, String)>,
}

/// RULE: a channel the CloudTrail control plane just created is one its data plane can
/// ingest onto in the very next call — `PutAuditEvents` answers HTTP 200 with `failed`
/// empty and one `successful` entry per submitted event, echoing that event's own `id`
/// beside the `eventID` CloudTrail assigned.
///
/// Rows differ along the Region the channel is homed in, whether its destination store was
/// created with the documented selectors or the minimal request, the batch size, and the
/// payload shape (flat, nested, unicode, large).
#[test]
fn a_channel_the_control_plane_created_ingests_audit_events_at_once() {
    on_a_router("cloudtrail-lake-ingestion", |router| {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("a current-thread runtime");
        runtime.block_on(async {
            let rows: Vec<Row> = vec![
                Row {
                    case: "one flat event, the documented store create",
                    region: "us-west-2",
                    name: "pipeline-west",
                    documented_store: true,
                    batch: vec![(
                        "sim2real-1".to_string(),
                        r#"{"eventSource":"custom.app","eventName":"ReproAction"}"#.to_string(),
                    )],
                },
                Row {
                    case: "two events, one of them nested",
                    region: "us-east-1",
                    name: "pipeline-east",
                    documented_store: false,
                    batch: vec![
                        (
                            "evt_2".to_string(),
                            r#"{"eventSource":"custom.app","eventName":"Logout"}"#.to_string(),
                        ),
                        (
                            "evt-3".to_string(),
                            r#"{"eventSource":"custom.app","userIdentity":{"type":"CustomUserIdentity","principalId":"p-1"}}"#
                                .to_string(),
                        ),
                    ],
                },
                Row {
                    case: "three events, unicode and a large payload",
                    region: "eu-central-1",
                    name: "pipeline-central",
                    documented_store: true,
                    batch: vec![
                        (
                            "evt-4".to_string(),
                            r#"{"eventSource":"custom.app","note":"héllo wörld"}"#.to_string(),
                        ),
                        ("evt-5".to_string(), payload_of_len(32 * 1024)),
                        (
                            "0123456789".to_string(),
                            r#"{"eventSource":"custom.app","eventName":"Purchase","amount":42}"#
                                .to_string(),
                        ),
                    ],
                },
            ];

            // Every event id this emulator assigned, across every row: an id names ONE
            // ingested event, so no two may repeat.
            let mut assigned: HashSet<String> = HashSet::new();

            for row in &rows {
                let Row {
                    case,
                    region,
                    name,
                    documented_store,
                    batch,
                } = row;
                let (status, store) =
                    create_event_data_store(&router, region, name, *documented_store).await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: CreateEventDataStore must succeed, got {status} with \
                     {store}"
                );
                let store_arn = store["EventDataStoreArn"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                assert!(
                    !store_arn.is_empty(),
                    "case {case:?}: CreateEventDataStore must report an ARN, got {store}"
                );

                let channel_arn =
                    create_channel(&router, region, name, &store_arn, case).await;

                // The control plane reports the channel it just minted, so the ARN the
                // data plane is about to be asked about is one this account really holds.
                let (status, fetched) = control_plane(
                    &router,
                    region,
                    "GetChannel",
                    &serde_json::json!({ "Channel": channel_arn }),
                )
                .await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: GetChannel must report the channel just created, got \
                     {status} with {fetched}"
                );

                // Two calls, so the second proves the ingest is not a one-shot: the batch
                // under test, then the same batch under fresh ids.
                let second: Vec<(String, String)> = batch
                    .iter()
                    .map(|(id, data)| (format!("{id}-again"), data.clone()))
                    .collect();
                for (call, events) in [("the first call", batch), ("a later call", &second)] {
                    let (status, reply) =
                        put_audit_events(&router, region, &channel_arn, None, events).await;
                    assert_eq!(
                        status, 200,
                        "case {case:?} on {call}: PutAuditEvents onto the channel the \
                         control plane just created must ingest, got {status} with {reply}"
                    );
                    assert_eq!(
                        error_code(&reply),
                        "",
                        "case {case:?} on {call}: expected no error code, got {reply}"
                    );
                    let failed = entries(&reply, "failed", "errorCode");
                    assert!(
                        failed.is_empty(),
                        "case {case:?} on {call}: every submitted event is acceptable, so \
                         failed must be empty, got {failed:?}"
                    );
                    let successful = entries(&reply, "successful", "eventID");
                    assert_eq!(
                        successful.len(),
                        events.len(),
                        "case {case:?} on {call}: expected one successful entry per \
                         submitted event ({}), got {successful:?}",
                        events.len()
                    );
                    for ((submitted, _), (echoed, event_id)) in events.iter().zip(&successful) {
                        assert_eq!(
                            echoed, submitted,
                            "case {case:?} on {call}: successful must echo the submitted \
                             event id, got {echoed:?}"
                        );
                        assert!(
                            is_modelled_event_id(event_id),
                            "case {case:?} on {call}: the assigned eventID must be within \
                             the modelled form, got {event_id:?}"
                        );
                        assert_ne!(
                            event_id, submitted,
                            "case {case:?} on {call}: the assigned eventID is CloudTrail's \
                             own, not the submitted id, got {event_id:?}"
                        );
                        assert!(
                            assigned.insert(event_id.clone()),
                            "case {case:?} on {call}: the eventID {event_id:?} was already \
                             assigned to another ingested event"
                        );
                    }
                }
            }
        });
    });
}

/// RULE: the data plane answers `ChannelNotFound` (HTTP 400, "The channel <reference>
/// could not be found.") for exactly the channels this account does not hold — one no call
/// ever created, and one the control plane's `DeleteChannel` has retired — and never for a
/// channel it does hold.
#[test]
fn only_a_channel_the_control_plane_does_not_hold_is_refused_as_absent() {
    on_a_router("cloudtrail-lake-absent-channel", |router| {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("a current-thread runtime");
        runtime.block_on(async {
            const REGION: &str = "us-west-2";
            let batch = vec![(
                "repro-1".to_string(),
                r#"{"eventSource":"repro.example.com","eventName":"ReproAction"}"#.to_string(),
            )];

            // Two channels, so the retired one is refused while its neighbour keeps
            // ingesting: a channel per Region, because one Region holds at most one
            // channel per source.
            let (status, store) =
                create_event_data_store(&router, REGION, "lifecycle-store", true).await;
            assert_eq!(
                status, 200,
                "CreateEventDataStore must succeed, got {status} with {store}"
            );
            let store_arn = store["EventDataStoreArn"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            let retired =
                create_channel(&router, REGION, "lifecycle-channel", &store_arn, "the retired channel")
                    .await;

            const OTHER_REGION: &str = "us-east-1";
            let (_, other_store) =
                create_event_data_store(&router, OTHER_REGION, "kept-store", false).await;
            let other_store_arn = other_store["EventDataStoreArn"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            let kept = create_channel(
                &router,
                OTHER_REGION,
                "kept-channel",
                &other_store_arn,
                "the channel that stays",
            )
            .await;

            // The channel is retired through the control plane, which is what takes it out
            // of the data plane's reach too.
            let (status, body) = control_plane(
                &router,
                REGION,
                "DeleteChannel",
                &serde_json::json!({ "Channel": retired }),
            )
            .await;
            assert_eq!(
                status, 200,
                "DeleteChannel must retire the channel, got {status} with {body}"
            );

            // A well-formed channel ARN of this account that no call ever created. Its id
            // is written the way the control plane mints one.
            let never_created = format!(
                "arn:aws:cloudtrail:{REGION}:{ACCOUNT_ID}:channel/\
                 39032a83-30fe-4581-8bd3-351632c2be72"
            );

            // (case, the channel the call names, the Region it is signed for, the status
            // and the error code expected)
            let rows: Vec<(&str, &str, &str, u16, &str)> = vec![
                (
                    "a channel the control plane retired",
                    retired.as_str(),
                    REGION,
                    400,
                    "ChannelNotFound",
                ),
                (
                    "a well-formed channel ARN no call ever created",
                    never_created.as_str(),
                    REGION,
                    400,
                    "ChannelNotFound",
                ),
                (
                    "a channel the control plane still holds",
                    kept.as_str(),
                    OTHER_REGION,
                    200,
                    "",
                ),
            ];

            for (case, channel, region, expected_status, expected_code) in &rows {
                let (status, reply) =
                    put_audit_events(&router, region, channel, None, &batch).await;
                assert_eq!(
                    status, *expected_status,
                    "case {case:?}: expected HTTP {expected_status}, got {status} with \
                     {reply}"
                );
                assert_eq!(
                    error_code(&reply),
                    *expected_code,
                    "case {case:?}: expected the error code {expected_code:?}, got {reply}"
                );
                if *expected_code == "ChannelNotFound" {
                    let stated = message_of(&reply);
                    let expected = format!("The channel {channel} could not be found.");
                    assert_eq!(
                        stated, expected,
                        "case {case:?}: expected the sentence AWS words for this refusal \
                         ({expected:?}), got {stated:?}"
                    );
                } else {
                    let failed = entries(&reply, "failed", "errorCode");
                    let successful = entries(&reply, "successful", "eventID");
                    assert!(
                        failed.is_empty() && successful.len() == batch.len(),
                        "case {case:?}: the channel is still held, so the batch must \
                         ingest, got successful {successful:?} and failed {failed:?}"
                    );
                }
            }
        });
    });
}
