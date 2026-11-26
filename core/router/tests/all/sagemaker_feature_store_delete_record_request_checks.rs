// SPDX-License-Identifier: CC0-1.0

//! A Feature Store `DeleteRecord` judges the REQUEST before it decides which store the
//! deletion is applied to.
//!
//! The RULE pinned here: **a check a Feature Store write makes on the REQUEST is made
//! whatever `TargetStores` the call names and whichever stores the feature group has —
//! those select which store the write is APPLIED to, never whether the request is allowed
//! — so a `DeleteRecord` whose `@required` `EventTime` states no instant is refused with
//! the modelled `ValidationError` (400) in every one of those combinations, a `HardDelete`
//! whose `EventTime` is before the stored record's is refused in every one of them too
//! ("Note that the `EventTime` specified in `DeleteRecord` should be set later than the
//! `EventTime` of the existing record in the `OnlineStore` for that `RecordIdentifer`. If
//! it is not, the deletion does not occur … `HardDelete` returns `EventTime`:
//! `400 ValidationException`", `smithy_query(query="doc",shape="DeleteRecord")`), and only
//! the WRITE is conditioned on the stores: an accepted deletion that does not name the
//! OnlineStore leaves the record there to be read back.**
//!
//! An `EventTime` is a Feature Store event time, so the two forms that state an instant are
//! an ISO-8601 timestamp and seconds since the epoch; the member itself is a `ValueAsString`
//! (`len 0..358400`, `pattern .*`,
//! `smithy_query(query="operation",operation="DeleteRecord")`), which admits any text, so
//! the rows below span the forms a request can send that name no instant at all — prose, a
//! timestamp-shaped value naming no date, a number that is no finite instant, and a number
//! with trailing text — beside the forms that do.
//!
//! TWO crates have to agree for the rule to hold, which is why the suite is here: the
//! SageMaker control plane is what DEFINES a feature group — its two feature names and
//! which stores it has — and the FeatureStore runtime is the data plane that serves the
//! deletion. The rows that need a group with no OnlineStore therefore need the control
//! plane's own `CreateFeatureGroup` to make one. They also live in this crate rather than
//! beside the runtime crate's own suites because that crate still roots its integration
//! tests at a hand-declared `tests/all.rs`, which a fix may not edit; the router's
//! `tests/all/` picks a new suite up with nothing to declare.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The SigV4 `Authorization` header of this crate's shared signer, written once for the
// whole test crate; it signs for `us-east-1`, which is the Region every call below is in.
use crate::rds_docdb_neptune_signing_name_routing::authorization;

/// The host the calls name; the emulator serves every service on one endpoint.
const HOST: &str = "featurestore-runtime.sagemaker.us-east-1.amazonaws.com";

/// The feature whose value identifies a record, as each group below is defined.
const IDENTIFIER_FEATURE: &str = "customer_id";

/// The feature carrying a record's event time, as each group below is defined.
const EVENT_TIME_FEATURE: &str = "event_time";

/// A third feature, so a record read back is told apart from an empty answer.
const PAYLOAD_FEATURE: &str = "payload";

/// The instant every record below is ingested at, in the two spellings an event time is
/// stated in: the ISO-8601 timestamp and the same instant in seconds since the epoch.
const STORED_EVENT_TIME: &str = "2026-10-23T18:20:13Z";
const STORED_EVENT_TIME_SECS: f64 = 1_792_779_613.0;

/// The deletion mode that removes the record outright, and so the one whose out-of-order
/// refusal the caller sees (`smithy_query(query="shape",shape="DeletionMode")`:
/// HardDelete | SoftDelete).
const HARD_DELETE: &str = "HardDelete";

/// How the runtime states a `DeleteRecord` whose `EventTime` is before the stored record's.
const OUT_OF_ORDER: &str =
    "Validation Error: Delete event time must be same or after latest record event time.";

/// The offline location a group that has an OfflineStore writes into, and the role it writes
/// under: the control plane records both, and a create naming an `OfflineStoreConfig` carries
/// a `RoleArn` ("The Amazon Resource Name (ARN) of the IAM execution role used to persist
/// data into the OfflineStore if an OfflineStoreConfig is provided").
const OFFLINE_S3_URI: &str = "s3://feature-store-offline/records";
const OFFLINE_ROLE_ARN: &str = "arn:aws:iam::123456789012:role/offline-store-writer";

/// One store shape a feature group can be created with: a create that names NEITHER store
/// is refused by the control plane, so these three are the shapes that exist.
///
/// (case, the group's name, whether it has an OnlineStore, whether it has an OfflineStore)
const SHAPES: [(&str, &str, bool, bool); 3] = [
    ("a group with both stores", "both-stores", true, true),
    ("a group with an OnlineStore only", "online-only", true, false),
    ("a group with an OfflineStore only", "offline-only", false, true),
];

/// The forms a deletion names its target stores in: none at all ("By default, Feature Store
/// deletes the record from all of the stores that you're using for the FeatureGroup"), each
/// store alone, and both in either order — `TargetStores` is a `@httpQuery` list, so a value
/// per repetition of the parameter.
///
/// (case, the query-string tail, whether the form names the OnlineStore)
const TARGET_FORMS: [(&str, &str, bool); 5] = [
    ("naming no TargetStores", "", true),
    ("naming TargetStores=OnlineStore", "&TargetStores=OnlineStore", true),
    (
        "naming TargetStores=OfflineStore",
        "&TargetStores=OfflineStore",
        false,
    ),
    (
        "naming both stores, online first",
        "&TargetStores=OnlineStore&TargetStores=OfflineStore",
        true,
    ),
    (
        "naming both stores, offline first",
        "&TargetStores=OfflineStore&TargetStores=OnlineStore",
        true,
    ),
];

/// `EventTime` values that state NO instant, differing along the form the request sends:
/// prose, a timestamp-shaped value naming no date, a number that is no finite instant (in
/// both spellings), a number with trailing text, and a bare sign.
const UNREADABLE_EVENT_TIMES: [&str; 6] = [
    "banana",
    "2026-13-45T99:61:61Z",
    "NaN",
    "inf",
    "1792779613abc",
    "-",
];

/// `EventTime` values that DO state an instant, in both forms a Feature Store event time
/// takes and at and after the stored record's own instant.
const READABLE_EVENT_TIMES: [&str; 4] = [
    STORED_EVENT_TIME,
    "1792779613",
    "1792779613.5",
    "2030-01-01T00:00:00Z",
];

/// The SageMaker control plane and the FeatureStore runtime over ONE engine, as
/// `core/router/src/lib.rs` mounts them: the group a deletion names is the control plane's
/// own resource, and the record is the runtime's.
struct FeatureStore {
    router: MultiServiceRouter,
}

impl FeatureStore {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(
            service_com_amazonaws_sagemakerfeaturestoreruntime_amazonsagemakerfeaturestoreruntime
                ::EmulatedService::new(Arc::clone(&engine)),
        );
        router.register_service(
            service_com_amazonaws_sagemaker_sagemaker::EmulatedService::new(engine),
        );
        Self { router }
    }

    /// One dispatched request, as its status and parsed body (`Value::Null` for an empty or
    /// unparseable body — a `DeleteRecord` that is served answers an empty 200).
    async fn answered(&self, request: http::Request<Bytes>) -> (u16, Value) {
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let parsed = if response.body().is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(response.body()).unwrap_or(Value::Null)
        };
        (status, parsed)
    }

    /// One SageMaker control-plane call (`awsJson1_1`, `X-Amz-Target: SageMaker.<Operation>`),
    /// which must answer 200.
    async fn control_plane(&self, case: &str, target: &str, body: &Value) -> Value {
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("host", HOST)
            .header("content-type", "application/x-amz-json-1.1")
            .header("x-amz-target", format!("SageMaker.{target}"))
            .header("authorization", authorization("sagemaker"))
            .body(Bytes::from(body.to_string()))
            .expect("a well-formed test request");
        let (status, answered) = self.answered(request).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from SageMaker {target}, got {status} with \
             {answered}"
        );
        answered
    }

    /// One runtime data-plane call (`restJson1`: a method and a URI of its own).
    async fn data_plane(&self, method: &str, uri: &str, body: Option<Value>) -> (u16, Value) {
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://{HOST}{uri}"))
            .header("host", HOST)
            .header("content-type", "application/json")
            .header("authorization", authorization("sagemaker"))
            .body(
                body.map(|value| Bytes::from(value.to_string()))
                    .unwrap_or_default(),
            )
            .expect("a well-formed test request");
        self.answered(request).await
    }

    /// Create the feature group `group` with the stores the shape names, and read it back
    /// through the control plane's own `DescribeFeatureGroup` so the shape under test is the
    /// one that service really holds.
    async fn create_group(&self, case: &str, group: &str, online: bool, offline: bool) {
        let mut body = json!({
            "FeatureGroupName": group,
            "RecordIdentifierFeatureName": IDENTIFIER_FEATURE,
            "EventTimeFeatureName": EVENT_TIME_FEATURE,
            "FeatureDefinitions": [
                { "FeatureName": IDENTIFIER_FEATURE, "FeatureType": "String" },
                { "FeatureName": EVENT_TIME_FEATURE, "FeatureType": "String" },
                { "FeatureName": PAYLOAD_FEATURE, "FeatureType": "String" },
            ],
        });
        let object = body.as_object_mut().expect("a JSON object body");
        if online {
            object.insert(
                "OnlineStoreConfig".to_string(),
                json!({ "EnableOnlineStore": true }),
            );
        }
        if offline {
            object.insert(
                "OfflineStoreConfig".to_string(),
                // `DisableGlueTableCreation`: this suite manages no Glue table.
                json!({
                    "S3StorageConfig": { "S3Uri": OFFLINE_S3_URI },
                    "DisableGlueTableCreation": true,
                }),
            );
            object.insert("RoleArn".to_string(), json!(OFFLINE_ROLE_ARN));
        }
        self.control_plane(case, "CreateFeatureGroup", &body).await;

        let described = self
            .control_plane(
                case,
                "DescribeFeatureGroup",
                &json!({ "FeatureGroupName": group }),
            )
            .await;
        let has_online = described
            .pointer("/OnlineStoreConfig/EnableOnlineStore")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let has_offline = described.get("OfflineStoreConfig").is_some();
        assert_eq!(
            (has_online, has_offline),
            (online, offline),
            "case {case:?}: expected the control plane to hold a group with \
             (OnlineStore, OfflineStore) = ({online}, {offline}), got {described}"
        );
    }

    /// Ingest one record of `group` at [`STORED_EVENT_TIME`].
    async fn put_record(&self, case: &str, group: &str, identifier: &str) {
        let (status, answered) = self
            .data_plane(
                "PUT",
                &format!("/FeatureGroup/{group}"),
                Some(json!({ "Record": [
                    { "FeatureName": IDENTIFIER_FEATURE, "ValueAsString": identifier },
                    { "FeatureName": EVENT_TIME_FEATURE, "ValueAsString": STORED_EVENT_TIME },
                    { "FeatureName": PAYLOAD_FEATURE, "ValueAsString": "ingested" },
                ] })),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected an empty HTTP 200 from PutRecord, got {status} with \
             {answered}"
        );
    }

    /// One `DeleteRecord`, with the query string the row builds.
    async fn delete_record(&self, group: &str, query: &str) -> (u16, Value) {
        self.data_plane("DELETE", &format!("/FeatureGroup/{group}?{query}"), None)
            .await
    }

    /// Whether `GetRecord` still serves the record `identifier` of `group`.
    async fn record_is_served(&self, group: &str, identifier: &str) -> (bool, Value) {
        let (_, body) = self
            .data_plane(
                "GET",
                &format!("/FeatureGroup/{group}?RecordIdentifierValueAsString={identifier}"),
                None,
            )
            .await;
        let served = body.get("Record").and_then(Value::as_array).is_some();
        (served, body)
    }
}

/// The wire error code of an error response: restJson1 carries it as the body's `__type`.
fn error_code(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .rsplit('#')
        .next()
        .unwrap_or_default()
        .to_owned()
}

/// The message an error response carries: this service's modelled errors declare a `Message`
/// member, while the framework's own pre-handler refusal carries `message`, so both
/// spellings are read.
fn message(body: &Value) -> String {
    body.get("message")
        .or_else(|| body.get("Message"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// How the runtime states a `DeleteRecord` `EventTime` that names no instant: the member,
/// the two forms that state one, and the value the request sent.
fn unreadable_event_time(value: &str) -> String {
    format!("EventTime: expected an ISO-8601 timestamp or seconds since the epoch, not '{value}'")
}

/// The three groups of [`SHAPES`], created through the control plane.
async fn with_every_shape() -> FeatureStore {
    let aws = FeatureStore::new();
    for (case, group, online, offline) in SHAPES {
        aws.create_group(case, group, online, offline).await;
    }
    aws
}

/// RULE: the `EventTime` check is on the REQUEST, so a value naming no instant is refused
/// whatever stores the deletion names and whichever stores the group has.
#[tokio::test]
async fn an_event_time_that_states_no_instant_is_refused_whatever_stores_the_deletion_names() {
    let aws = with_every_shape().await;

    // Each row deletes its OWN record identifier, so no row is answered differently for
    // what another row left behind.
    let mut row = 0usize;
    for (shape, group, online, _offline) in SHAPES {
        for (form, targets, _names_online) in TARGET_FORMS {
            for event_time in UNREADABLE_EVENT_TIMES {
                row += 1;
                let identifier = format!("C{row}");
                let case = format!("{shape}, {form}, EventTime {event_time:?}");
                if online {
                    aws.put_record(&case, group, &identifier).await;
                }

                let (status, body) = aws
                    .delete_record(
                        group,
                        &format!(
                            "RecordIdentifierValueAsString={identifier}\
                             &EventTime={event_time}{targets}"
                        ),
                    )
                    .await;
                let code = error_code(&body);
                assert_eq!(
                    (status, code.as_str()),
                    (400, "ValidationError"),
                    "case {case:?}: expected HTTP 400 ValidationError for an EventTime that \
                     states no instant, got {status} {code:?} with {body}"
                );
                let answered = message(&body);
                assert_eq!(
                    answered,
                    unreadable_event_time(event_time),
                    "case {case:?}: expected the refusal to state the two forms an EventTime \
                     takes and the value sent, got {answered:?}"
                );

                // A refused request wrote nothing: the record it named is still there.
                if online {
                    let (served, read) = aws.record_is_served(group, &identifier).await;
                    assert!(
                        served,
                        "case {case:?}: expected the refused deletion to leave the record \
                         standing, got {read}"
                    );
                }
            }
        }
    }
}

/// RULE: an `EventTime` that states an instant is accepted in the same combinations — and
/// only the WRITE follows the stores, so the record stays readable where the deletion did
/// not name the OnlineStore.
#[tokio::test]
async fn an_event_time_that_states_an_instant_is_served_whatever_stores_the_deletion_names() {
    let aws = with_every_shape().await;

    let mut row = 0usize;
    for (shape, group, online, _offline) in SHAPES {
        for (form, targets, names_online) in TARGET_FORMS {
            for event_time in READABLE_EVENT_TIMES {
                row += 1;
                let identifier = format!("C{row}");
                let case = format!("{shape}, {form}, EventTime {event_time:?}");
                if online {
                    aws.put_record(&case, group, &identifier).await;
                }

                let (status, body) = aws
                    .delete_record(
                        group,
                        &format!(
                            "RecordIdentifierValueAsString={identifier}\
                             &EventTime={event_time}{targets}"
                        ),
                    )
                    .await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: expected an empty HTTP 200 for a deletion at or after the \
                     record's own event time, got {status} with {body}"
                );

                // The online store is written only where the request named it: that is the
                // one thing `TargetStores` decides.
                if online {
                    let (served, read) = aws.record_is_served(group, &identifier).await;
                    assert_eq!(
                        served, !names_online,
                        "case {case:?}: expected GetRecord to serve the record only where the \
                         deletion did not name the OnlineStore (named: {names_online}), got \
                         {read}"
                    );
                }
            }
        }
    }
}

/// RULE: the documented ordering against the stored record's `EventTime` is decided on the
/// REQUEST too, so a `HardDelete` before it is refused whatever stores the deletion names.
#[tokio::test]
async fn a_hard_delete_before_the_records_event_time_is_refused_whatever_stores_it_names() {
    let aws = with_every_shape().await;

    // The class the rule spans: how far before the record's instant the deletion is, in both
    // forms an event time takes.
    let earlier: [(&str, String); 3] = [
        (
            "one second earlier, as epoch seconds",
            format!("{}", STORED_EVENT_TIME_SECS - 1.0),
        ),
        (
            "much earlier, as epoch seconds",
            format!("{}", STORED_EVENT_TIME_SECS - 500.0),
        ),
        ("earlier, as ISO-8601", "2020-01-01T00:00:00Z".to_owned()),
    ];

    let mut row = 0usize;
    for (shape, group, online, _offline) in SHAPES {
        // The ordering is stated over "the EventTime of the existing record in the
        // OnlineStore", and a group with no OnlineStore holds no such record to order
        // against.
        if !online {
            continue;
        }
        for (form, targets, _names_online) in TARGET_FORMS {
            for (when, event_time) in &earlier {
                row += 1;
                let identifier = format!("H{row}");
                let case = format!("{shape}, {form}, {when}");
                aws.put_record(&case, group, &identifier).await;

                let (status, body) = aws
                    .delete_record(
                        group,
                        &format!(
                            "RecordIdentifierValueAsString={identifier}\
                             &EventTime={event_time}&DeletionMode={HARD_DELETE}{targets}"
                        ),
                    )
                    .await;
                let code = error_code(&body);
                assert_eq!(
                    (status, code.as_str()),
                    (400, "ValidationError"),
                    "case {case:?}: expected HTTP 400 ValidationError for a {HARD_DELETE} \
                     before the record's event time, got {status} {code:?} with {body}"
                );
                let answered = message(&body);
                assert_eq!(
                    answered, OUT_OF_ORDER,
                    "case {case:?}: expected the refusal to state the ordering rule, got \
                     {answered:?}"
                );
                let (served, read) = aws.record_is_served(group, &identifier).await;
                assert!(
                    served,
                    "case {case:?}: expected the refused deletion to leave the record standing, \
                     got {read}"
                );
            }
        }
    }
}
