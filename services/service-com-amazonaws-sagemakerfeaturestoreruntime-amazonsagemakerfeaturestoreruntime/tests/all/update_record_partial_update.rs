// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for `UpdateRecord`: the PARTIAL update of a record the
//! OnlineStore already holds.
//!
//! The rules asserted here are the ones the operation documents
//! (`smithy_query(query="doc", shape="UpdateRecord")`):
//!
//! * every posted `FeatureValue` replaces the stored value of the feature it names and
//!   every feature the request does not name keeps its stored value;
//! * an `EventTime` older than the record's is refused with `ConflictException` (409),
//!   one equal to or newer than it applies, and an omitted one keeps the stored
//!   `EventTime`;
//! * a record that does not exist, or that has been soft-deleted, is `ResourceNotFound`
//!   (404) — `PutRecord`, not `UpdateRecord`, is what creates a record;
//! * a `TtlDuration` recomputes `ExpiresAt = EventTime + TtlDuration` and requires an
//!   `EventTime` in the same request;
//! * that expiry is ONE value the record holds in two spellings — the ISO-8601 string a
//!   read reports and the instant the store compares against — so a sum stating no
//!   instant an ISO-8601 string can carry leaves the record under no expiry at all,
//!   reported by neither read and enforced by nothing;
//! * a feature of a record has ONE value, so a request that names the same feature more
//!   than once — which the modelled `Features` list admits, carrying no `uniqueItems` —
//!   settles that feature on the value it names LAST and leaves no earlier value of it
//!   observable.
//!
//! Each test is a TABLE spanning the class its rule generalises over — how many
//! features are updated, which ones, the shape of their values, the two accepted
//! event-time formats, every declared TTL unit — and the recorded wire case is one row.
//! Every case goes through the real front door: the multi-service router, the restJson1
//! protocol and the modelled constraint validation, so what is asserted is the wire
//! behaviour a client sees rather than a handler's return value.

use std::sync::Arc;

use aws_smithy_types::date_time::{DateTime, Format};
use serde_json::{json, Value};
use service_common::feature_store_groups::{FeatureGroupDirectory, FeatureGroupRecord};
use smithy_server_runtime::{http, MultiServiceRouter};

use service_com_amazonaws_sagemakerfeaturestoreruntime_amazonsagemakerfeaturestoreruntime::EmulatedService;

/// A booted emulator holding this service only.
struct Emulator {
    engine: Arc<engine::InMemoryEngine>,
    router: MultiServiceRouter,
}

impl Emulator {
    /// A booted emulator with the feature groups this suite updates records in already
    /// created.
    ///
    /// Every operation of this data plane resolves its `FeatureGroupName` against the
    /// feature groups the SageMaker CONTROL plane holds, so a group has to exist before a
    /// record can be written to it — exactly as a client has to call
    /// `CreateFeatureGroup` first.
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        let emulator = Self { engine, router };
        for group in ["customers", "ttlgroup"] {
            emulator.create_feature_group(group);
        }
        emulator
    }

    /// Create a feature group with an OnlineStore, the way the SageMaker control plane's
    /// `CreateFeatureGroup` does: by registering its definition — including the two
    /// feature names the records below carry — in the ONE shared feature-group directory
    /// this data plane resolves names against.
    fn create_feature_group(&self, name: &str) {
        self.engine
            .state::<FeatureGroupDirectory>()
            .lock()
            .register(FeatureGroupRecord {
                name: name.to_owned(),
                arn: format!("arn:aws:sagemaker:us-east-1:123456789012:feature-group/{name}"),
                record_identifier_feature_name: "customer_id".to_owned(),
                event_time_feature_name: "event_time".to_owned(),
                online_store_enabled: true,
                // Online-only feature groups: no offline store to replicate into.
                offline_store: None,
                default_ttl_seconds: None,
            });
    }

    /// Send one request and return its status and parsed JSON body (`Value::Null` for
    /// an empty body).
    async fn send(&self, method: &str, uri: &str, body: Option<Value>) -> (u16, Value) {
        // The SigV4 credential scope is how the router picks the claiming service: the
        // 4th field is this service's signing name.
        let request = http::Request::builder()
            .method(method)
            .uri(uri)
            .header(
                "authorization",
                "AWS4-HMAC-SHA256 Credential=AKIDTESTONLY/20240101/us-east-1/sagemaker/aws4_request, \
                 SignedHeaders=host, Signature=0000",
            )
            .header("host", "featurestore-runtime.sagemaker.us-east-1.amazonaws.com")
            .header("content-type", "application/json")
            .body(
                body.map(|value| bytes::Bytes::from(value.to_string()))
                    .unwrap_or_default(),
            )
            .expect("build the request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let parsed = serde_json::from_slice::<Value>(response.body()).unwrap_or(Value::Null);
        (status, parsed)
    }

    async fn put_record(&self, group: &str, body: Value) -> (u16, Value) {
        self.send("PUT", &format!("/FeatureGroup/{group}"), Some(body))
            .await
    }

    async fn update_record(&self, group: &str, body: Value) -> (u16, Value) {
        self.send(
            "POST",
            &format!("/FeatureGroup/{group}/Record"),
            Some(body),
        )
        .await
    }

    async fn get_record(&self, group: &str, query: &str) -> (u16, Value) {
        self.send("GET", &format!("/FeatureGroup/{group}?{query}"), None)
            .await
    }

    async fn delete_record(&self, group: &str, query: &str) -> (u16, Value) {
        self.send("DELETE", &format!("/FeatureGroup/{group}?{query}"), None)
            .await
    }
}

/// One `(FeatureName, value)` pair of a record, as [`record_pairs`] reports it.
type Pair = (String, Value);

/// One row of a partial-update table: the case name, the `Features` the update posts,
/// and the record the merge is expected to leave behind.
type MergeCase<'a> = (&'a str, Value, Vec<(&'a str, Value)>);

/// One row of a `TargetStores` table: the case name, the list the update names, and the
/// record the OnlineStore is expected to hold afterwards.
type TargetStoresCase<'a> = (&'a str, Value, &'a [Pair]);

/// One `FeatureValue` with a scalar value.
fn feature(name: &str, value: &str) -> Value {
    json!({ "FeatureName": name, "ValueAsString": value })
}

/// The `Record` member of a `GetRecord` response, as `(FeatureName, Value)` pairs where
/// the value is the scalar or the list the response carried.
fn record_pairs(body: &Value) -> Vec<Pair> {
    body.get("Record")
        .and_then(Value::as_array)
        .map(|features| {
            features
                .iter()
                .map(|feature| {
                    let name = feature["FeatureName"].as_str().unwrap_or_default().to_owned();
                    let value = feature
                        .get("ValueAsString")
                        .or_else(|| feature.get("ValueAsStringList"))
                        .cloned()
                        .unwrap_or(Value::Null);
                    (name, value)
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The wire error code of an error response: restJson1 carries it as the body's
/// `__type`.
fn error_code(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .rsplit('#')
        .next()
        .unwrap_or_default()
        .to_owned()
}

/// An event time, as the instant an ISO-8601 or epoch-seconds value denotes.
fn secs(iso_or_epoch: &str) -> f64 {
    DateTime::from_str(iso_or_epoch, Format::DateTime)
        .map(|parsed| parsed.as_secs_f64())
        .unwrap_or_else(|_| iso_or_epoch.parse::<f64>().expect("a parsable event time"))
}

/// An event time one day from now, so a record written with a TTL of any size in these
/// tests is still live when the test reads it back.
fn tomorrow() -> f64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("a clock after the epoch")
        .as_secs() as f64;
    now + 86_400.0
}

/// [`tomorrow`] as the ISO-8601 string an event-time feature of type `String` carries.
fn tomorrow_iso() -> String {
    DateTime::from_secs_f64(tomorrow())
        .fmt(Format::DateTime)
        .expect("format an ISO-8601 event time")
}

/// Ingest the record these tests update, into group `group` under identifier `C1`.
///
/// `customer_id` is the group's own `RecordIdentifierFeatureName` and `event_time` its
/// `EventTimeFeatureName` (see `Emulator::create_feature_group`), followed by three
/// ordinary features of differing value shapes.
async fn seed(emulator: &Emulator, group: &str, event_time: &str) {
    let (status, body) = emulator
        .put_record(
            group,
            json!({ "Record": [
                feature("customer_id", "C1"),
                feature("event_time", event_time),
                feature("city", "Seattle"),
                feature("balance", "-17.5"),
                { "FeatureName": "tags", "ValueAsStringList": ["a", "b"] }
            ] }),
        )
        .await;
    assert_eq!(
        status, 200,
        "seeding {group:?} must succeed, got {status} {body:?}"
    );
}

#[tokio::test]
async fn update_record_replaces_only_the_features_the_request_names() {
    // The class the rule spans: WHICH features a request names and WHAT shape their
    // values take. One row per kind of partial update — a single feature, several at
    // once, a feature the record does not carry yet (added), a scalar replaced by a
    // collection and the reverse, an empty string, unicode, a re-posted identifier
    // feature, and a request that re-posts a value identical to the stored one.
    let cases: Vec<MergeCase> = vec![
        (
            "one feature",
            json!([feature("city", "Boston")]),
            vec![
                ("customer_id", json!("C1")),
                ("event_time", json!("2024-01-01T00:00:00Z")),
                ("city", json!("Boston")),
                ("balance", json!("-17.5")),
                ("tags", json!(["a", "b"])),
            ],
        ),
        (
            "two features at once",
            json!([feature("city", "Boston"), feature("balance", "42")]),
            vec![
                ("customer_id", json!("C1")),
                ("event_time", json!("2024-01-01T00:00:00Z")),
                ("city", json!("Boston")),
                ("balance", json!("42")),
                ("tags", json!(["a", "b"])),
            ],
        ),
        (
            "a feature the record does not carry yet",
            json!([feature("note", "new")]),
            vec![
                ("customer_id", json!("C1")),
                ("event_time", json!("2024-01-01T00:00:00Z")),
                ("city", json!("Seattle")),
                ("balance", json!("-17.5")),
                ("tags", json!(["a", "b"])),
                ("note", json!("new")),
            ],
        ),
        (
            "a scalar replaced by a collection",
            json!([{ "FeatureName": "city", "ValueAsStringList": ["Boston", "Austin"] }]),
            vec![
                ("customer_id", json!("C1")),
                ("event_time", json!("2024-01-01T00:00:00Z")),
                ("city", json!(["Boston", "Austin"])),
                ("balance", json!("-17.5")),
                ("tags", json!(["a", "b"])),
            ],
        ),
        (
            "a collection replaced by a scalar",
            json!([feature("tags", "z")]),
            vec![
                ("customer_id", json!("C1")),
                ("event_time", json!("2024-01-01T00:00:00Z")),
                ("city", json!("Seattle")),
                ("balance", json!("-17.5")),
                ("tags", json!("z")),
            ],
        ),
        (
            "an empty and a unicode value",
            json!([feature("balance", ""), feature("city", "東京 / Zürich")]),
            vec![
                ("customer_id", json!("C1")),
                ("event_time", json!("2024-01-01T00:00:00Z")),
                ("city", json!("東京 / Zürich")),
                ("balance", json!("")),
                ("tags", json!(["a", "b"])),
            ],
        ),
        (
            "the identifier feature re-posted with its own value",
            json!([feature("customer_id", "C1"), feature("city", "Boston")]),
            vec![
                ("customer_id", json!("C1")),
                ("event_time", json!("2024-01-01T00:00:00Z")),
                ("city", json!("Boston")),
                ("balance", json!("-17.5")),
                ("tags", json!(["a", "b"])),
            ],
        ),
        (
            "a value identical to the stored one",
            json!([feature("city", "Seattle")]),
            vec![
                ("customer_id", json!("C1")),
                ("event_time", json!("2024-01-01T00:00:00Z")),
                ("city", json!("Seattle")),
                ("balance", json!("-17.5")),
                ("tags", json!(["a", "b"])),
            ],
        ),
    ];

    for (case, features, expected) in cases {
        let emulator = Emulator::new();
        seed(&emulator, "customers", "2024-01-01T00:00:00Z").await;

        let (status, body) = emulator
            .update_record(
                "customers",
                json!({
                    "RecordIdentifierValueAsString": "C1",
                    "Features": features
                }),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected UpdateRecord 200, got {status} {body:?}"
        );

        let (status, body) = emulator
            .get_record("customers", "RecordIdentifierValueAsString=C1")
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected GetRecord 200, got {status}"
        );
        let actual = record_pairs(&body);
        let expected: Vec<(String, Value)> = expected
            .into_iter()
            .map(|(name, value)| (name.to_owned(), value))
            .collect();
        assert_eq!(
            actual, expected,
            "case {case:?}: expected the merged record {expected:?}, got {actual:?}"
        );
    }
}

#[tokio::test]
async fn an_omitted_event_time_keeps_the_records_event_time() {
    // "If you omit EventTime, the service keeps the record's existing EventTime and
    // applies the update." The class: the event-time formats a record can be stored
    // with — ISO-8601 and epoch seconds, past and future.
    let future_iso = tomorrow_iso();
    let future_epoch = format!("{}", tomorrow() as i64);
    let cases: Vec<(&str, &str)> = vec![
        ("ISO-8601 event time", "2024-01-01T00:00:00Z"),
        ("epoch-seconds event time", "1704067200"),
        ("fractional epoch-seconds event time", "1704067200.5"),
        ("ISO-8601 event time in the future", &future_iso),
        ("epoch-seconds event time in the future", &future_epoch),
    ];

    for (case, event_time) in cases {
        let emulator = Emulator::new();
        seed(&emulator, "customers", event_time).await;

        let (status, body) = emulator
            .update_record(
                "customers",
                json!({
                    "RecordIdentifierValueAsString": "C1",
                    "Features": [feature("city", "Boston")]
                }),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected UpdateRecord 200, got {status} {body:?}"
        );

        let (_, body) = emulator
            .get_record("customers", "RecordIdentifierValueAsString=C1")
            .await;
        let actual = record_pairs(&body);
        let expected: Vec<(String, Value)> = vec![
            ("customer_id".to_owned(), json!("C1")),
            ("event_time".to_owned(), json!(event_time)),
            ("city".to_owned(), json!("Boston")),
            ("balance".to_owned(), json!("-17.5")),
            ("tags".to_owned(), json!(["a", "b"])),
        ];
        assert_eq!(
            actual, expected,
            "case {case:?}: expected the stored event time preserved, got {actual:?}"
        );
    }
}

#[tokio::test]
async fn an_event_time_older_than_the_records_is_refused_with_a_conflict() {
    // "If you provide an EventTime that is older than the record's current EventTime,
    // the service rejects the update with a ConflictException. If the EventTime is equal
    // to or newer than the current value, the service applies the update." The class:
    // the ORDER of the supplied event time against the stored one, across both accepted
    // formats and across the boundary between them (an epoch value and an ISO value can
    // denote the very same instant).
    let stored_iso = "2024-01-01T00:00:00Z";
    let stored_epoch_of_same_instant = "1704067200";
    let cases: Vec<(&str, &str, u16)> = vec![
        ("newer, ISO-8601", "2024-01-02T00:00:00Z", 200),
        ("equal, ISO-8601", stored_iso, 200),
        ("older, ISO-8601", "2023-12-31T23:59:59Z", 409),
        ("far older, ISO-8601", "2000-01-01T00:00:00Z", 409),
        ("newer, epoch seconds", "1704067201", 200),
        ("equal instant, epoch seconds", stored_epoch_of_same_instant, 200),
        ("older, epoch seconds", "1704067199", 409),
        ("older by a fraction of a second", "1704067199.999", 409),
    ];

    for (case, supplied, expected_status) in cases {
        let emulator = Emulator::new();
        seed(&emulator, "customers", stored_iso).await;

        let (status, body) = emulator
            .update_record(
                "customers",
                json!({
                    "RecordIdentifierValueAsString": "C1",
                    "Features": [
                        feature("city", "Boston"),
                        feature("event_time", supplied)
                    ]
                }),
            )
            .await;
        assert_eq!(
            status, expected_status,
            "case {case:?}: expected UpdateRecord {expected_status}, got {status} {body:?}"
        );

        let (_, read_back) = emulator
            .get_record("customers", "RecordIdentifierValueAsString=C1")
            .await;
        let actual = record_pairs(&read_back);
        if expected_status == 200 {
            let expected: Vec<(String, Value)> = vec![
                ("customer_id".to_owned(), json!("C1")),
                ("event_time".to_owned(), json!(supplied)),
                ("city".to_owned(), json!("Boston")),
                ("balance".to_owned(), json!("-17.5")),
                ("tags".to_owned(), json!(["a", "b"])),
            ];
            assert_eq!(
                actual, expected,
                "case {case:?}: expected the update applied, got {actual:?}"
            );
        } else {
            let code = error_code(&body);
            assert_eq!(
                code, "ConflictException",
                "case {case:?}: expected the ConflictException error code, got {code:?}"
            );
            let expected: Vec<(String, Value)> = vec![
                ("customer_id".to_owned(), json!("C1")),
                ("event_time".to_owned(), json!(stored_iso)),
                ("city".to_owned(), json!("Seattle")),
                ("balance".to_owned(), json!("-17.5")),
                ("tags".to_owned(), json!(["a", "b"])),
            ];
            assert_eq!(
                actual, expected,
                "case {case:?}: expected a refused update to persist nothing, got {actual:?}"
            );
        }
    }
}

#[tokio::test]
async fn updating_a_record_the_online_store_does_not_hold_is_not_found() {
    // "The record must already exist. If the record does not exist or has been
    // soft-deleted, the operation returns a ResourceNotFound error. To create a record,
    // use PutRecord." The class: every way the OnlineStore can fail to hold a live
    // record under the requested identifier.
    let cases: Vec<&str> = vec![
        "a feature group with no OnlineStore",
        "an identifier never written",
        "a soft-deleted record",
        "a hard-deleted record",
        "a record whose TTL has passed",
    ];

    for case in cases {
        let emulator = Emulator::new();
        let identifier = match case {
            "a feature group with no OnlineStore" => "C1",
            "an identifier never written" => {
                seed(&emulator, "customers", "2024-01-01T00:00:00Z").await;
                "C-absent"
            }
            "a soft-deleted record" => {
                seed(&emulator, "customers", "2024-01-01T00:00:00Z").await;
                emulator
                    .delete_record(
                        "customers",
                        "RecordIdentifierValueAsString=C1&EventTime=2024-01-02T00:00:00Z\
                         &DeletionMode=SoftDelete",
                    )
                    .await;
                "C1"
            }
            "a hard-deleted record" => {
                seed(&emulator, "customers", "2024-01-01T00:00:00Z").await;
                emulator
                    .delete_record(
                        "customers",
                        "RecordIdentifierValueAsString=C1&EventTime=2024-01-02T00:00:00Z\
                         &DeletionMode=HardDelete",
                    )
                    .await;
                "C1"
            }
            // An event time long past plus a one-second TTL: the expiry is already due,
            // so the record is gone by the time the update arrives.
            _ => {
                emulator
                    .put_record(
                        "customers",
                        json!({
                            "Record": [
                                feature("customer_id", "C1"),
                                feature("event_time", "2000-01-01T00:00:00Z")
                            ],
                            "TtlDuration": { "Unit": "Seconds", "Value": 1 }
                        }),
                    )
                    .await;
                "C1"
            }
        };

        let (status, body) = emulator
            .update_record(
                "customers",
                json!({
                    "RecordIdentifierValueAsString": identifier,
                    "Features": [feature("city", "Boston")]
                }),
            )
            .await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected UpdateRecord 404, got {status} {body:?}"
        );
        let code = error_code(&body);
        assert_eq!(
            code, "ResourceNotFound",
            "case {case:?}: expected the ResourceNotFound error code, got {code:?}"
        );

        // The refusal creates nothing: UpdateRecord never brings a record into being.
        let (_, read_back) = emulator
            .get_record(
                "customers",
                &format!("RecordIdentifierValueAsString={identifier}"),
            )
            .await;
        let served = read_back.get("Record").cloned().unwrap_or(Value::Null);
        assert!(
            served.is_null(),
            "case {case:?}: expected no record after the refusal, got {served:?}"
        );
    }
}

#[tokio::test]
async fn a_ttl_duration_sets_expires_at_to_the_supplied_event_time_plus_the_duration() {
    // Every unit the model's `TtlDurationUnit` enum declares (see
    // `TTL_DURATION_UNIT_VALUES` in `ops::types::shared_types`), with the published
    // lower bound of `TtlDurationValue` (range 1..) and larger values, over both
    // accepted event-time formats. The event time is a day ahead, so no row's expiry is
    // already due.
    let iso = tomorrow_iso();
    let epoch = format!("{}", tomorrow() as i64);
    let cases: Vec<(&str, &str, &str, i64, f64)> = vec![
        ("Seconds, lower bound", &iso, "Seconds", 1, 1.0),
        ("Seconds", &iso, "Seconds", 90, 90.0),
        ("Minutes", &iso, "Minutes", 90, 5_400.0),
        ("Hours", &iso, "Hours", 36, 129_600.0),
        ("Days", &iso, "Days", 1, 86_400.0),
        ("Weeks", &iso, "Weeks", 2, 1_209_600.0),
        ("Days on an epoch event time", &epoch, "Days", 3, 259_200.0),
        ("Weeks, large value", &iso, "Weeks", 52, 31_449_600.0),
    ];

    for (case, event_time, unit, value, expected_delta) in cases {
        let emulator = Emulator::new();
        // Seeded a second BEFORE the row's event time, so the update's own event time is
        // newer and the expiry is computed from the value this request supplies.
        let seeded = DateTime::from_secs_f64(secs(event_time) - 1.0)
            .fmt(Format::DateTime)
            .expect("format an ISO-8601 event time");
        seed(&emulator, "ttlgroup", &seeded).await;

        let (status, body) = emulator
            .update_record(
                "ttlgroup",
                json!({
                    "RecordIdentifierValueAsString": "C1",
                    "Features": [
                        feature("city", "Boston"),
                        feature("event_time", event_time)
                    ],
                    "TtlDuration": { "Unit": unit, "Value": value }
                }),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected UpdateRecord 200, got {status} {body:?}"
        );

        let (status, body) = emulator
            .get_record(
                "ttlgroup",
                "RecordIdentifierValueAsString=C1&ExpirationTimeResponse=Enabled",
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected GetRecord 200, got {status}"
        );
        let expires_at = body
            .get("ExpiresAt")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        assert!(
            DateTime::from_str(&expires_at, Format::DateTime).is_ok(),
            "case {case:?}: expected an ISO-8601 ExpiresAt, got {expires_at:?}"
        );
        let delta = secs(&expires_at) - secs(event_time);
        assert!(
            (delta - expected_delta).abs() < 0.001,
            "case {case:?}: expected ExpiresAt {expected_delta} s after the supplied event \
             time, got {delta} s (ExpiresAt {expires_at:?})"
        );
    }
}

#[tokio::test]
async fn an_update_the_operations_documented_limits_refuse_answers_400() {
    // The documented refusals that are this handler's own, each with the request that
    // satisfies the same rule on the other side of the boundary:
    //
    // * "If you specify a TtlDuration, you must also provide an EventTime in the
    //   request. Otherwise, the operation returns a ValidationError."
    // * "You can update up to 100 features per call" — the model bounds `Features` at
    //   `len 1..` only, so 100 passes and 101 is refused.
    // * `RecordIdentifierValueAsString` "must match the value defined by the feature
    //   group's record identifier feature": a record's identity is not updatable.
    // * an event time the update supplies has to be a timestamp, in one of the two
    //   formats a Feature Store event time takes.
    let many = |count: usize| -> Value {
        Value::Array(
            (0..count)
                .map(|n| feature(&format!("f{n}"), "v"))
                .collect(),
        )
    };
    let cases: Vec<(&str, Value, u16)> = vec![
        (
            "TtlDuration with no EventTime",
            json!({
                "RecordIdentifierValueAsString": "C1",
                "Features": [feature("city", "Boston")],
                "TtlDuration": { "Unit": "Days", "Value": 1 }
            }),
            400,
        ),
        (
            "TtlDuration with an EventTime",
            json!({
                "RecordIdentifierValueAsString": "C1",
                "Features": [
                    feature("city", "Boston"),
                    feature("event_time", "2024-06-01T00:00:00Z")
                ],
                "TtlDuration": { "Unit": "Days", "Value": 1 }
            }),
            200,
        ),
        (
            "100 features, the documented maximum",
            json!({
                "RecordIdentifierValueAsString": "C1",
                "Features": many(100)
            }),
            200,
        ),
        (
            "101 features, one past the documented maximum",
            json!({
                "RecordIdentifierValueAsString": "C1",
                "Features": many(101)
            }),
            400,
        ),
        (
            "the identifier feature carrying another record's value",
            json!({
                "RecordIdentifierValueAsString": "C1",
                "Features": [feature("customer_id", "C2")]
            }),
            400,
        ),
        (
            "an event time that is not a timestamp",
            json!({
                "RecordIdentifierValueAsString": "C1",
                "Features": [feature("event_time", "last Tuesday")]
            }),
            400,
        ),
    ];

    for (case, body, expected_status) in cases {
        let emulator = Emulator::new();
        seed(&emulator, "customers", "2024-01-01T00:00:00Z").await;

        let (status, response) = emulator.update_record("customers", body).await;
        assert_eq!(
            status, expected_status,
            "case {case:?}: expected UpdateRecord {expected_status}, got {status} {response:?}"
        );
        if expected_status == 400 {
            let code = error_code(&response);
            assert!(
                matches!(code.as_str(), "ValidationError" | "ValidationException"),
                "case {case:?}: expected a 400 client refusal code, got {code:?}"
            );
        }
    }
}

#[tokio::test]
async fn an_update_aimed_at_the_offline_store_alone_leaves_the_online_record_alone() {
    // "The target stores for the record update. By default, Amazon SageMaker Feature
    // Store updates the record in all stores associated with the FeatureGroup." Only the
    // OnlineStore is modelled here, so the rows that name it update the record and the
    // row that names only the OfflineStore does not.
    let stored: Vec<Pair> = vec![
        ("customer_id".to_owned(), json!("C1")),
        ("event_time".to_owned(), json!("2024-01-01T00:00:00Z")),
        ("city".to_owned(), json!("Seattle")),
        ("balance".to_owned(), json!("-17.5")),
        ("tags".to_owned(), json!(["a", "b"])),
    ];
    let updated: Vec<Pair> = stored
        .iter()
        .map(|(name, value)| {
            if name == "city" {
                (name.clone(), json!("Boston"))
            } else {
                (name.clone(), value.clone())
            }
        })
        .collect();

    let cases: Vec<TargetStoresCase> = vec![
        ("no TargetStores", Value::Null, &updated),
        ("OnlineStore", json!(["OnlineStore"]), &updated),
        (
            "both stores",
            json!(["OnlineStore", "OfflineStore"]),
            &updated,
        ),
        ("OfflineStore alone", json!(["OfflineStore"]), &stored),
    ];

    for (case, target_stores, expected) in cases {
        let emulator = Emulator::new();
        seed(&emulator, "customers", "2024-01-01T00:00:00Z").await;

        let mut body = json!({
            "RecordIdentifierValueAsString": "C1",
            "Features": [feature("city", "Boston")]
        });
        if !target_stores.is_null() {
            body["TargetStores"] = target_stores;
        }
        let (status, response) = emulator.update_record("customers", body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected UpdateRecord 200, got {status} {response:?}"
        );

        let (_, read_back) = emulator
            .get_record("customers", "RecordIdentifierValueAsString=C1")
            .await;
        let actual = record_pairs(&read_back);
        assert_eq!(
            actual.as_slice(),
            expected,
            "case {case:?}: expected the record {expected:?}, got {actual:?}"
        );
    }
}

#[tokio::test]
async fn a_target_store_outside_the_enum_is_refused_with_400() {
    // A value outside the model's `TargetStore` enum (`TARGET_STORE_VALUES`) would
    // otherwise read as "not the OnlineStore" and turn the update into a silent no-op
    // success, so it is refused instead.
    let emulator = Emulator::new();
    seed(&emulator, "customers", "2024-01-01T00:00:00Z").await;

    for case in ["OnlineStores", "onlinestore", ""] {
        let (status, body) = emulator
            .update_record(
                "customers",
                json!({
                    "RecordIdentifierValueAsString": "C1",
                    "Features": [feature("city", "Boston")],
                    "TargetStores": [case]
                }),
            )
            .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected UpdateRecord 400, got {status} {body:?}"
        );
        let code = error_code(&body);
        assert!(
            matches!(code.as_str(), "ValidationError" | "ValidationException"),
            "case {case:?}: expected a 400 client refusal code, got {code:?}"
        );
    }

    // And the record is untouched by every refusal above.
    let (_, read_back) = emulator
        .get_record("customers", "RecordIdentifierValueAsString=C1")
        .await;
    let city = record_pairs(&read_back)
        .into_iter()
        .find(|(name, _)| name == "city")
        .map(|(_, value)| value)
        .unwrap_or(Value::Null);
    assert_eq!(
        city,
        json!("Seattle"),
        "case \"after the refusals\": expected the stored city, got {city:?}"
    );
}

#[tokio::test]
async fn a_merge_past_the_stores_per_record_feature_bound_is_refused() {
    use service_com_amazonaws_sagemakerfeaturestoreruntime_amazonsagemakerfeaturestoreruntime::state::MAX_FEATURES_PER_RECORD;

    // `UpdateRecord` is the one operation that grows a stored record, so it is the one
    // that has to respect the store's per-record capacity bound. The rows walk up to the
    // bound in calls of the documented per-call maximum, then one call past it.
    let emulator = Emulator::new();
    seed(&emulator, "customers", "2024-01-01T00:00:00Z").await;
    // What `seed` wrote: customer_id, event_time, city, balance, tags.
    let seeded = 5_usize;
    // "You can update up to 100 features per call", so a request adds at most this many.
    let per_call = 100_usize;
    let calls_within_the_bound = (MAX_FEATURES_PER_RECORD - seeded) / per_call;

    let fresh = |call: usize| -> Value {
        Value::Array(
            (0..per_call)
                .map(|n| feature(&format!("f{}", call * per_call + n), "v"))
                .collect(),
        )
    };

    for call in 0..calls_within_the_bound {
        let (status, body) = emulator
            .update_record(
                "customers",
                json!({
                    "RecordIdentifierValueAsString": "C1",
                    "Features": fresh(call)
                }),
            )
            .await;
        assert_eq!(
            status, 200,
            "case \"call {call} of {calls_within_the_bound} within the bound\": expected \
             UpdateRecord 200, got {status} {body:?}"
        );
    }
    let at_the_bound = seeded + calls_within_the_bound * per_call;

    // One call of fresh names past the bound is refused as a server-side capacity limit
    // (503), not as the caller's mistake (400) — the request is well-formed.
    let (status, body) = emulator
        .update_record(
            "customers",
            json!({
                "RecordIdentifierValueAsString": "C1",
                "Features": fresh(calls_within_the_bound)
            }),
        )
        .await;
    assert_eq!(
        status, 503,
        "case \"one call past the bound\": expected 503, got {status} {body:?}"
    );
    let code = error_code(&body);
    assert_eq!(
        code, "ServiceUnavailable",
        "case \"one call past the bound\": expected ServiceUnavailable, got {code:?}"
    );

    // The refused merge persisted NOTHING — not even the features it could have added
    // before reaching the bound.
    let (_, read_back) = emulator
        .get_record("customers", "RecordIdentifierValueAsString=C1")
        .await;
    let held = record_pairs(&read_back).len();
    assert_eq!(
        held, at_the_bound,
        "case \"after the refusal\": expected the record to still hold {at_the_bound} \
         feature values, got {held}"
    );

    // A record at the bound still takes an update that only REPLACES feature values it
    // already carries: the bound is on residency, not on updating.
    let (status, body) = emulator
        .update_record(
            "customers",
            json!({
                "RecordIdentifierValueAsString": "C1",
                "Features": [feature("city", "Boston")]
            }),
        )
        .await;
    assert_eq!(
        status, 200,
        "case \"a replacing update at the bound\": expected UpdateRecord 200, got \
         {status} {body:?}"
    );
    let (_, read_back) = emulator
        .get_record("customers", "RecordIdentifierValueAsString=C1")
        .await;
    let city = record_pairs(&read_back)
        .into_iter()
        .find(|(name, _)| name == "city")
        .map(|(_, value)| value)
        .unwrap_or(Value::Null);
    assert_eq!(
        city,
        json!("Boston"),
        "case \"a replacing update at the bound\": expected the updated city, got {city:?}"
    );
}

#[tokio::test]
async fn a_feature_group_arn_and_its_name_reach_one_record() {
    // `FeatureGroupNameOrArn` is "the feature group name" or "the feature group Amazon
    // Resource Name (ARN)", so an update addressed by ARN and a read addressed by name
    // must be the same record. On the wire the ARN is one `@httpLabel` segment, so its
    // `:` and `/` arrive percent-encoded.
    let emulator = Emulator::new();
    seed(&emulator, "customers", "2024-01-01T00:00:00Z").await;

    let arn = "arn%3Aaws%3Asagemaker%3Aus-east-1%3A123456789012%3Afeature-group%2Fcustomers";
    let (status, body) = emulator
        .update_record(
            arn,
            json!({
                "RecordIdentifierValueAsString": "C1",
                "Features": [feature("city", "Boston")]
            }),
        )
        .await;
    assert_eq!(
        status, 200,
        "case \"update by ARN\": expected UpdateRecord 200, got {status} {body:?}"
    );

    for (case, group) in [("by name", "customers"), ("by ARN", arn)] {
        let (_, read_back) = emulator
            .get_record(group, "RecordIdentifierValueAsString=C1")
            .await;
        let actual = record_pairs(&read_back);
        let expected: Vec<(String, Value)> = vec![
            ("customer_id".to_owned(), json!("C1")),
            ("event_time".to_owned(), json!("2024-01-01T00:00:00Z")),
            ("city".to_owned(), json!("Boston")),
            ("balance".to_owned(), json!("-17.5")),
            ("tags".to_owned(), json!(["a", "b"])),
        ];
        assert_eq!(
            actual, expected,
            "case {case:?}: expected the updated record, got {actual:?}"
        );
    }
}

// A feature named MORE THAN ONCE in one request.
//
// `Features` is modelled `list<FeatureValue>` with `length min 1` and NO `uniqueItems`
// (`smithy_query(query="shape", shape="FeatureValues,Record")`), so a request may
// legitimately name one feature twice and the constraint walk passes it to the handler.
// A feature of a record nevertheless HAS one value — a `FeatureValue` is "The value
// associated with a feature" and a read reports "the latest value for all the Features"
// (`GetRecord$FeatureNames`) — so the value a request settles a repeated name on is the
// LAST it names for it, and the record the OnlineStore holds afterwards carries that
// feature exactly once. Everything the operation derives from a posted feature (the
// `EventTime` it orders by, the identifier it checks) is derived from that same held
// value, so a read can never disagree with what the merge stored.

/// The event time the record these tests update is stored with.
const HELD_EVENT_TIME: &str = "2025-06-01T00:00:00Z";

/// An event time NEWER than [`HELD_EVENT_TIME`], which an update may apply.
const NEWER_EVENT_TIME: &str = "2026-01-01T00:00:00Z";

/// An event time newer still, for a request that names the event-time feature twice.
const NEWEST_EVENT_TIME: &str = "2027-02-03T04:05:06Z";

/// An event time OLDER than [`HELD_EVENT_TIME`], which an update is refused for.
const OLDER_EVENT_TIME: &str = "2024-01-01T00:00:00Z";

/// One `FeatureValue` carrying a list, which is what a collection-typed feature carries.
fn list_feature(name: &str, values: &[&str]) -> Value {
    json!({ "FeatureName": name, "ValueAsStringList": values })
}

/// The record the repeated-name tests update: the group's own record-identifier and
/// event-time features followed by two ordinary features, one scalar and one collection.
///
/// Every value is a word that occurs in no other value of the record, so a value a later
/// occurrence of the same name superseded can be looked for in the whole rendered
/// response rather than in one member of it.
fn held_record() -> Vec<Value> {
    vec![
        feature("customer_id", "C1"),
        feature("event_time", HELD_EVENT_TIME),
        feature("city", "Seattle"),
        list_feature("tags", &["tagone", "tagtwo"]),
    ]
}

/// The pairs [`held_record`] reads back as, i.e. the record an update must leave
/// unchanged when it is refused.
fn held_pairs() -> Vec<(String, Value)> {
    vec![
        ("customer_id".to_owned(), json!("C1")),
        ("event_time".to_owned(), json!(HELD_EVENT_TIME)),
        ("city".to_owned(), json!("Seattle")),
        ("tags".to_owned(), json!(["tagone", "tagtwo"])),
    ]
}

/// Ingest `record` into `group` through `PutRecord`, which must be served.
async fn seed_record(emulator: &Emulator, case: &str, group: &str, record: Vec<Value>) {
    let (status, body) = emulator
        .put_record(group, json!({ "Record": record }))
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: seeding {group:?} must succeed, got {status} {body:?}"
    );
}

/// How many times `name` occurs among the `(FeatureName, value)` pairs of a read record.
fn occurrences(pairs: &[Pair], name: &str) -> usize {
    pairs.iter().filter(|(held, _)| held == name).count()
}

/// One row of the event-time ordering table: the case, the `Features` the update posts,
/// the status and error code it is answered with (an empty code for a served update), and
/// the record it leaves behind.
type OrderingCase<'a> = (&'a str, Vec<Value>, u16, &'a str, Vec<Pair>);

/// One row of the identity-check table: the case, the `Features` the update posts, the
/// status it is answered with and the record it leaves behind.
type IdentityCase<'a> = (&'a str, Vec<Value>, u16, Vec<Pair>);

/// One row of the repeated-name merge table: the case, the `Features` the update posts,
/// the record it must leave behind, and the values it must leave nowhere.
struct RepeatedCase<'a> {
    case: &'a str,
    features: Vec<Value>,
    expected: Vec<(&'a str, Value)>,
    superseded: Vec<&'a str>,
}

#[tokio::test]
async fn an_update_that_names_a_feature_twice_settles_on_the_last_value_named() {
    // The class the rule spans: HOW MANY times a name is repeated, WHICH feature is
    // repeated (an ordinary one, one the record does not carry yet, the group's
    // event-time feature, its record-identifier feature), and the FORM of the values the
    // repeated name carries (`ValueAsString`, `ValueAsStringList`, one after the other) —
    // with an update whose names are all distinct as the control row.
    let rows = vec![
        RepeatedCase {
            case: "an ordinary feature named twice",
            features: vec![feature("city", "Boston"), feature("city", "Austin")],
            expected: vec![
                ("customer_id", json!("C1")),
                ("event_time", json!(HELD_EVENT_TIME)),
                ("city", json!("Austin")),
                ("tags", json!(["tagone", "tagtwo"])),
            ],
            superseded: vec!["Seattle", "Boston"],
        },
        RepeatedCase {
            case: "an ordinary feature named three times",
            features: vec![
                feature("city", "Boston"),
                feature("city", "Austin"),
                feature("city", "Denver"),
            ],
            expected: vec![
                ("customer_id", json!("C1")),
                ("event_time", json!(HELD_EVENT_TIME)),
                ("city", json!("Denver")),
                ("tags", json!(["tagone", "tagtwo"])),
            ],
            superseded: vec!["Seattle", "Boston", "Austin"],
        },
        RepeatedCase {
            case: "a feature the record does not carry yet, named twice",
            features: vec![
                feature("note", "notefirst"),
                feature("note", "notelast"),
            ],
            expected: vec![
                ("customer_id", json!("C1")),
                ("event_time", json!(HELD_EVENT_TIME)),
                ("city", json!("Seattle")),
                ("tags", json!(["tagone", "tagtwo"])),
                ("note", json!("notelast")),
            ],
            superseded: vec!["notefirst"],
        },
        RepeatedCase {
            case: "a scalar value superseded by a list value",
            features: vec![
                feature("tags", "tagscalar"),
                list_feature("tags", &["taglistone", "taglisttwo"]),
            ],
            expected: vec![
                ("customer_id", json!("C1")),
                ("event_time", json!(HELD_EVENT_TIME)),
                ("city", json!("Seattle")),
                ("tags", json!(["taglistone", "taglisttwo"])),
            ],
            superseded: vec!["tagscalar", "tagone", "tagtwo"],
        },
        RepeatedCase {
            case: "a list value superseded by a scalar value",
            features: vec![
                list_feature("tags", &["taglistone", "taglisttwo"]),
                feature("tags", "tagscalar"),
            ],
            expected: vec![
                ("customer_id", json!("C1")),
                ("event_time", json!(HELD_EVENT_TIME)),
                ("city", json!("Seattle")),
                ("tags", json!("tagscalar")),
            ],
            superseded: vec!["taglistone", "taglisttwo", "tagone", "tagtwo"],
        },
        RepeatedCase {
            case: "the group's event-time feature named twice",
            features: vec![
                feature("event_time", NEWER_EVENT_TIME),
                feature("event_time", NEWEST_EVENT_TIME),
                feature("city", "Boston"),
            ],
            expected: vec![
                ("customer_id", json!("C1")),
                ("event_time", json!(NEWEST_EVENT_TIME)),
                ("city", json!("Boston")),
                ("tags", json!(["tagone", "tagtwo"])),
            ],
            superseded: vec![HELD_EVENT_TIME, NEWER_EVENT_TIME, "Seattle"],
        },
        RepeatedCase {
            case: "the group's record-identifier feature named twice",
            features: vec![
                feature("customer_id", "C1"),
                feature("customer_id", "C1"),
                feature("city", "Boston"),
            ],
            expected: vec![
                ("customer_id", json!("C1")),
                ("event_time", json!(HELD_EVENT_TIME)),
                ("city", json!("Boston")),
                ("tags", json!(["tagone", "tagtwo"])),
            ],
            superseded: vec!["Seattle"],
        },
        RepeatedCase {
            case: "every feature named once",
            features: vec![feature("city", "Boston"), feature("tags", "tagscalar")],
            expected: vec![
                ("customer_id", json!("C1")),
                ("event_time", json!(HELD_EVENT_TIME)),
                ("city", json!("Boston")),
                ("tags", json!("tagscalar")),
            ],
            superseded: vec!["Seattle", "tagone", "tagtwo"],
        },
    ];

    for row in rows {
        let case = row.case;
        let emulator = Emulator::new();
        seed_record(&emulator, case, "customers", held_record()).await;

        let (status, body) = emulator
            .update_record(
                "customers",
                json!({
                    "RecordIdentifierValueAsString": "C1",
                    "Features": row.features
                }),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected UpdateRecord 200, got {status} {body:?}"
        );

        let (status, body) = emulator
            .get_record("customers", "RecordIdentifierValueAsString=C1")
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected GetRecord 200, got {status} {body:?}"
        );
        let actual = record_pairs(&body);
        let expected: Vec<Pair> = row
            .expected
            .iter()
            .map(|(name, value)| ((*name).to_owned(), value.clone()))
            .collect();
        assert_eq!(
            actual, expected,
            "case {case:?}: expected the merged record {expected:?}, got {actual:?}"
        );

        // Each feature of the record is reported ONCE: a repeated name updates the one
        // value the record holds for it instead of adding a second.
        for (name, _) in &expected {
            let held = occurrences(&actual, name);
            assert_eq!(
                held, 1,
                "case {case:?}: expected exactly one {name:?} value in the record, got \
                 {held} in {actual:?}"
            );
        }

        // A value a later occurrence of the same name superseded is held nowhere, so no
        // pre-update value of an updated feature stays observable.
        let rendered = body.to_string();
        for gone in &row.superseded {
            assert!(
                !rendered.contains(gone),
                "case {case:?}: expected the record not to carry the superseded value \
                 {gone:?}, got {rendered}"
            );
        }
    }
}

#[tokio::test]
async fn the_last_event_time_an_update_names_is_the_one_the_update_is_ordered_by() {
    // The `EventTime` of an update is the value the group's own `EventTimeFeatureName`
    // carries among the posted features, and a feature named twice carries the value
    // named LAST — so that value, and not the one named first, is what the ordering rule
    // ("If you provide an EventTime that is older than the record's current EventTime,
    // the service rejects the update with a ConflictException") is applied to.
    let rows: Vec<OrderingCase> = vec![
        (
            "the newer of two event times named last",
            vec![
                feature("event_time", OLDER_EVENT_TIME),
                feature("event_time", NEWER_EVENT_TIME),
                feature("city", "Boston"),
            ],
            200,
            "",
            vec![
                ("customer_id".to_owned(), json!("C1")),
                ("event_time".to_owned(), json!(NEWER_EVENT_TIME)),
                ("city".to_owned(), json!("Boston")),
                ("tags".to_owned(), json!(["tagone", "tagtwo"])),
            ],
        ),
        (
            "the older of two event times named last",
            vec![
                feature("event_time", NEWER_EVENT_TIME),
                feature("event_time", OLDER_EVENT_TIME),
                feature("city", "Boston"),
            ],
            409,
            "ConflictException",
            held_pairs(),
        ),
    ];

    for (case, features, expected_status, expected_code, expected_record) in rows {
        let emulator = Emulator::new();
        seed_record(&emulator, case, "customers", held_record()).await;

        let (status, body) = emulator
            .update_record(
                "customers",
                json!({
                    "RecordIdentifierValueAsString": "C1",
                    "Features": features
                }),
            )
            .await;
        assert_eq!(
            status, expected_status,
            "case {case:?}: expected HTTP {expected_status}, got {status} {body:?}"
        );
        if !expected_code.is_empty() {
            let code = error_code(&body);
            assert_eq!(
                code, expected_code,
                "case {case:?}: expected the {expected_code} error code, got {code:?} in \
                 {body:?}"
            );
        }

        let (_, read_back) = emulator
            .get_record("customers", "RecordIdentifierValueAsString=C1")
            .await;
        let actual = record_pairs(&read_back);
        assert_eq!(
            actual, expected_record,
            "case {case:?}: expected the record {expected_record:?}, got {actual:?}"
        );
    }
}

#[tokio::test]
async fn the_last_identifier_value_an_update_names_is_the_one_it_is_checked_against() {
    // `RecordIdentifierValueAsString` "must match the value defined by the feature
    // group's record identifier feature", and the value a feature named twice carries is
    // the one named LAST — so the check is made against that value: an update whose last
    // occurrence carries the record's own identifier is served, and one whose last
    // occurrence carries another value is refused and changes nothing.
    let rows: Vec<IdentityCase> = vec![
        (
            "the record's own identifier value named last",
            vec![
                feature("customer_id", "C9"),
                feature("customer_id", "C1"),
                feature("city", "Boston"),
            ],
            200,
            vec![
                ("customer_id".to_owned(), json!("C1")),
                ("event_time".to_owned(), json!(HELD_EVENT_TIME)),
                ("city".to_owned(), json!("Boston")),
                ("tags".to_owned(), json!(["tagone", "tagtwo"])),
            ],
        ),
        (
            "another record's identifier value named last",
            vec![
                feature("customer_id", "C1"),
                feature("customer_id", "C9"),
                feature("city", "Boston"),
            ],
            400,
            held_pairs(),
        ),
    ];

    for (case, features, expected_status, expected_record) in rows {
        let emulator = Emulator::new();
        seed_record(&emulator, case, "customers", held_record()).await;

        let (status, body) = emulator
            .update_record(
                "customers",
                json!({
                    "RecordIdentifierValueAsString": "C1",
                    "Features": features
                }),
            )
            .await;
        assert_eq!(
            status, expected_status,
            "case {case:?}: expected HTTP {expected_status}, got {status} {body:?}"
        );
        if expected_status == 400 {
            let code = error_code(&body);
            assert!(
                matches!(code.as_str(), "ValidationError" | "ValidationException"),
                "case {case:?}: expected the modelled validation error code, got {code:?} \
                 in {body:?}"
            );
        }

        let (_, read_back) = emulator
            .get_record("customers", "RecordIdentifierValueAsString=C1")
            .await;
        let actual = record_pairs(&read_back);
        assert_eq!(
            actual, expected_record,
            "case {case:?}: expected the record {expected_record:?}, got {actual:?}"
        );
    }
}

#[tokio::test]
async fn a_feature_written_twice_by_one_write_is_updated_as_one_value() {
    // A whole-record write that names a feature twice stores that feature ONCE, so the
    // record an update merges onto holds one value per name whichever write ingested it —
    // and after an update of that feature neither read of this data plane serves a
    // pre-update value beside the new one. The class: the two whole-record writes that
    // ingest a record, and the two reads that serve one.
    for (case, via) in [
        ("written by PutRecord", "PutRecord"),
        ("written by BatchWriteRecord", "BatchWriteRecord"),
    ] {
        let emulator = Emulator::new();
        let posted = vec![
            feature("customer_id", "C1"),
            feature("event_time", HELD_EVENT_TIME),
            feature("city", "Seattle"),
            feature("city", "Boston"),
            list_feature("tags", &["tagone", "tagtwo"]),
        ];
        if via == "PutRecord" {
            seed_record(&emulator, case, "customers", posted).await;
        } else {
            let (status, body) = emulator
                .send(
                    "POST",
                    "/BatchWriteRecord",
                    Some(json!({ "Entries": [
                        { "FeatureGroupName": "customers", "Record": posted }
                    ] })),
                )
                .await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected BatchWriteRecord 200, got {status} {body:?}"
            );
            let errors = body
                .get("Errors")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            assert!(
                errors.is_empty(),
                "case {case:?}: expected the batch entry to be written, got {errors:?}"
            );
        }

        // The ingested record already holds the repeated feature once, under the value
        // the write named last.
        let (_, body) = emulator
            .get_record("customers", "RecordIdentifierValueAsString=C1")
            .await;
        let written = record_pairs(&body);
        let expected_written: Vec<Pair> = vec![
            ("customer_id".to_owned(), json!("C1")),
            ("event_time".to_owned(), json!(HELD_EVENT_TIME)),
            ("city".to_owned(), json!("Boston")),
            ("tags".to_owned(), json!(["tagone", "tagtwo"])),
        ];
        assert_eq!(
            written, expected_written,
            "case {case:?}: expected the written record {expected_written:?}, got \
             {written:?}"
        );

        let (status, body) = emulator
            .update_record(
                "customers",
                json!({
                    "RecordIdentifierValueAsString": "C1",
                    "Features": [
                        feature("event_time", NEWER_EVENT_TIME),
                        feature("city", "Denver")
                    ]
                }),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected UpdateRecord 200, got {status} {body:?}"
        );

        let expected_merged: Vec<Pair> = vec![
            ("customer_id".to_owned(), json!("C1")),
            ("event_time".to_owned(), json!(NEWER_EVENT_TIME)),
            ("city".to_owned(), json!("Denver")),
            ("tags".to_owned(), json!(["tagone", "tagtwo"])),
        ];

        // GetRecord: the updated feature has one value, the updated one.
        let (_, body) = emulator
            .get_record("customers", "RecordIdentifierValueAsString=C1")
            .await;
        let merged = record_pairs(&body);
        assert_eq!(
            merged, expected_merged,
            "case {case:?}: expected GetRecord to serve {expected_merged:?}, got {merged:?}"
        );
        let rendered = body.to_string();
        for gone in ["Seattle", "Boston", HELD_EVENT_TIME] {
            assert!(
                !rendered.contains(gone),
                "case {case:?}: expected GetRecord not to serve the pre-update value \
                 {gone:?}, got {rendered}"
            );
        }

        // BatchGetRecord: the same record, read through the other reader of this plane.
        let (status, body) = emulator
            .send(
                "POST",
                "/BatchGetRecord",
                Some(json!({ "Identifiers": [
                    {
                        "FeatureGroupName": "customers",
                        "RecordIdentifiersValueAsString": ["C1"]
                    }
                ] })),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected BatchGetRecord 200, got {status} {body:?}"
        );
        let details = body
            .get("Records")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            details.len(),
            1,
            "case {case:?}: expected one record detail, got {details:?}"
        );
        let batched = record_pairs(&details[0]);
        assert_eq!(
            batched, expected_merged,
            "case {case:?}: expected BatchGetRecord to serve {expected_merged:?}, got \
             {batched:?}"
        );
    }
}

/// The last instant an `ExpiresAt` can state, in epoch seconds: the end of year 9999,
/// the greatest year the ISO-8601 string that member carries admits ("The `ExpiresAt`
/// ISO string of the requested record",
/// `smithy_query(query="shape", shape="GetRecordResponse")`).
const LAST_STATABLE_SECS: i64 = 253_402_300_799;

/// The largest value a `TtlDuration` can carry: `TtlDuration$Value` targets
/// `TtlDurationValue`, an `integer` modelled `range 1..` with no maximum, so the type's
/// own upper bound is what a request may state
/// (`smithy_query(query="shape", shape="TtlDuration")`).
const LARGEST_TTL_VALUE: i64 = i32::MAX as i64;

/// What a read of the record a row wrote is expected to answer about its expiry.
#[derive(Debug, Clone, Copy)]
enum Expiry {
    /// Reported as an ISO-8601 string this many seconds after the row's event time, and
    /// enforced from that same instant.
    Reported(f64),
    /// None at all: the sum states no instant an ISO-8601 string can carry, so the
    /// record holds the expiry in NEITHER spelling — no `ExpiresAt` is reported and
    /// nothing hard-deletes the record.
    Unstatable,
    /// Reported and already reached, so the record is hard-deleted and the read serves
    /// nothing at all.
    Due,
}

/// Store the record one row reads back, under its event time and `TtlDuration`, through
/// `write` — the two writes that derive a record's `ExpiresAt`.
async fn write_under_ttl(
    emulator: &Emulator,
    write: &str,
    event_time: &str,
    unit: &str,
    value: i64,
) -> (u16, Value) {
    let ttl = json!({ "Unit": unit, "Value": value });
    if write == "PutRecord" {
        return emulator
            .put_record(
                "ttlgroup",
                json!({
                    "Record": [
                        feature("customer_id", "C1"),
                        feature("event_time", event_time),
                        feature("city", "Seattle")
                    ],
                    "TtlDuration": ttl
                }),
            )
            .await;
    }
    // `UpdateRecord` is a PARTIAL update, so the record has to exist first. It is seeded
    // under the row's OWN event time and no TTL duration, so the update's event time is
    // not older than the stored one (no `ConflictException`) and the expiry it reads back
    // is the one this request's duration derives.
    seed(emulator, "ttlgroup", event_time).await;
    emulator
        .update_record(
            "ttlgroup",
            json!({
                "RecordIdentifierValueAsString": "C1",
                "Features": [
                    feature("event_time", event_time),
                    feature("city", "Tacoma")
                ],
                "TtlDuration": ttl
            }),
        )
        .await
}

#[tokio::test]
async fn an_expiry_no_iso_8601_string_can_state_leaves_the_record_under_no_expiry() {
    // `ExpiresAt = EventTime + TtlDuration` is one value held in two spellings, so the
    // class the rule spans is WHERE that sum falls: inside the range an ISO-8601 string
    // states (reported, and enforced once reached) or outside it on either side (no
    // expiry at all). The rows differ in the event time's form (ISO-8601, epoch seconds)
    // and magnitude and in the size of the duration — the largest `TtlDuration` the model
    // admits under an ordinary event time reaches the far side of the bound on its own —
    // and the published bound is tested from both sides. Each row is written through BOTH
    // writes that derive an expiry.
    let iso = tomorrow_iso();
    let epoch = format!("{}", tomorrow() as i64);
    let inside_bound = format!("{}", LAST_STATABLE_SECS - 1);
    let at_bound = format!("{LAST_STATABLE_SECS}");
    let milliseconds = format!("{}", tomorrow() as i64 * 1_000);
    let cases: Vec<(&str, &str, &str, i64, Expiry)> = vec![
        ("an ISO-8601 event time", &iso, "Hours", 1, Expiry::Reported(3_600.0)),
        ("an epoch-seconds event time", &epoch, "Days", 1, Expiry::Reported(86_400.0)),
        (
            "a sum one second inside the last statable instant",
            &inside_bound,
            "Seconds",
            1,
            Expiry::Reported(1.0),
        ),
        (
            "a sum one second past the last statable instant",
            &at_bound,
            "Seconds",
            1,
            Expiry::Unstatable,
        ),
        (
            "the largest duration the model admits",
            &iso,
            "Weeks",
            LARGEST_TTL_VALUE,
            Expiry::Unstatable,
        ),
        (
            "an event time passed in milliseconds",
            &milliseconds,
            "Hours",
            1,
            Expiry::Unstatable,
        ),
        (
            "an event time above every statable instant",
            "1e300",
            "Hours",
            1,
            Expiry::Unstatable,
        ),
        (
            "an event time below every statable instant",
            "-1e300",
            "Days",
            1,
            Expiry::Unstatable,
        ),
        (
            "an expiry already reached",
            "2024-01-01T00:00:00Z",
            "Seconds",
            1,
            Expiry::Due,
        ),
    ];

    for (case, event_time, unit, value, expected) in cases {
        for write in ["PutRecord", "UpdateRecord"] {
            let emulator = Emulator::new();
            let (status, body) = write_under_ttl(&emulator, write, event_time, unit, value).await;
            assert_eq!(
                status, 200,
                "case {case:?} through {write}: expected 200, got {status} {body:?}"
            );

            let (status, body) = emulator
                .get_record(
                    "ttlgroup",
                    "RecordIdentifierValueAsString=C1&ExpirationTimeResponse=Enabled",
                )
                .await;
            assert_eq!(
                status, 200,
                "case {case:?} through {write}: expected GetRecord 200, got {status} {body:?}"
            );
            let served = record_pairs(&body);
            let reported = body
                .get("ExpiresAt")
                .and_then(Value::as_str)
                .map(str::to_owned);

            match expected {
                Expiry::Reported(delta) => {
                    assert!(
                        !served.is_empty(),
                        "case {case:?} through {write}: expected GetRecord to serve the \
                         record, got {body:?}"
                    );
                    let at = reported.clone().unwrap_or_default();
                    assert!(
                        DateTime::from_str(&at, Format::DateTime).is_ok(),
                        "case {case:?} through {write}: expected an ISO-8601 ExpiresAt, got \
                         {reported:?}"
                    );
                    let actual = secs(&at) - secs(event_time);
                    assert!(
                        (actual - delta).abs() < 0.001,
                        "case {case:?} through {write}: expected ExpiresAt {delta} s after the \
                         event time, got {actual} s (ExpiresAt {reported:?})"
                    );
                }
                Expiry::Unstatable => {
                    assert!(
                        !served.is_empty(),
                        "case {case:?} through {write}: expected GetRecord to serve the record, \
                         since an expiry no ISO-8601 string states is no expiry the store \
                         enforces, got {body:?}"
                    );
                    assert!(
                        reported.is_none(),
                        "case {case:?} through {write}: expected no ExpiresAt, since the sum \
                         states no instant an ISO-8601 string can carry, got {reported:?}"
                    );
                }
                Expiry::Due => {
                    assert!(
                        served.is_empty() && reported.is_none(),
                        "case {case:?} through {write}: expected an expiry already reached to \
                         have hard-deleted the record, got {served:?} with ExpiresAt \
                         {reported:?}"
                    );
                }
            }
        }
    }
}
