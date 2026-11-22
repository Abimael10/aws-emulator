// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for the TTL a record of a feature group is stored under: the
//! duration the write states when it states one, and otherwise the DEFAULT the group's
//! `OnlineStoreConfig.TtlDuration` configures — and what every later call that moves the
//! record's event time, an update or a soft delete, re-derives its `ExpiresAt` to.
//!
//! Every case goes through the real front door — the multi-service router, the restJson1
//! protocol, the modelled constraint validation — so what is asserted is the wire
//! behaviour a client sees, not a handler's return value.

use std::sync::Arc;

use aws_smithy_types::date_time::{DateTime, Format};
use serde_json::{json, Value};
use service_common::feature_store_groups::{FeatureGroupDirectory, FeatureGroupRecord};
use smithy_server_runtime::{http, MultiServiceRouter};

use service_com_amazonaws_sagemakerfeaturestoreruntime_amazonsagemakerfeaturestoreruntime::state::StoredRecord;
use service_com_amazonaws_sagemakerfeaturestoreruntime_amazonsagemakerfeaturestoreruntime::{
    ComAmazonawsSagemakerfeaturestoreruntimeAmazonsagemakerfeaturestoreruntimeState as OnlineStore,
    EmulatedService,
};

/// The feature whose value identifies a record in every group these suites create.
const IDENTIFIER_FEATURE: &str = "customer_id";

/// The feature carrying a record's event time in every group these suites create.
const EVENT_TIME_FEATURE: &str = "event_time";

/// A booted emulator holding this service only.
struct Emulator {
    engine: Arc<engine::InMemoryEngine>,
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        Self { engine, router }
    }

    /// Create a feature group with an OnlineStore and the default TTL
    /// `default_ttl_seconds`, the way the SageMaker control plane's `CreateFeatureGroup`
    /// does: by registering its definition in the ONE shared feature-group directory this
    /// data plane resolves names against. `None` is a group configured with no
    /// `OnlineStoreConfig.TtlDuration` at all.
    fn create_feature_group(&self, name: &str, default_ttl_seconds: Option<f64>) {
        self.engine
            .state::<FeatureGroupDirectory>()
            .lock()
            .register(FeatureGroupRecord {
                name: name.to_owned(),
                arn: format!("arn:aws:sagemaker:us-east-1:123456789012:feature-group/{name}"),
                record_identifier_feature_name: IDENTIFIER_FEATURE.to_owned(),
                event_time_feature_name: EVENT_TIME_FEATURE.to_owned(),
                online_store_enabled: true,
                // Online-only feature groups: no offline store to replicate into.
                offline_store: None,
                default_ttl_seconds,
            });
    }

    /// Send one request and return its status and parsed JSON body (`Value::Null` for an
    /// empty body).
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

    async fn batch_write_record(&self, body: Value) -> (u16, Value) {
        self.send("POST", "/BatchWriteRecord", Some(body)).await
    }

    async fn update_record(&self, group: &str, body: Value) -> (u16, Value) {
        self.send(
            "POST",
            &format!("/FeatureGroup/{group}/Record"),
            Some(body),
        )
        .await
    }

    async fn get_record(&self, group: &str, identifier: &str) -> (u16, Value) {
        self.send(
            "GET",
            &format!(
                "/FeatureGroup/{group}?RecordIdentifierValueAsString={identifier}\
                 &ExpirationTimeResponse=Enabled"
            ),
            None,
        )
        .await
    }

    async fn list_records(&self, group: &str) -> (u16, Value) {
        self.send(
            "POST",
            &format!("/FeatureGroup/{group}/ListRecords"),
            Some(json!({})),
        )
        .await
    }

    /// `ListRecords` asking for the soft-deleted records too, which is the one call that
    /// reports a tombstone a `SoftDelete` left behind.
    async fn list_records_including_soft_deleted(&self, group: &str) -> (u16, Value) {
        self.send(
            "POST",
            &format!("/FeatureGroup/{group}/ListRecords"),
            Some(json!({ "IncludeSoftDeletedRecords": true })),
        )
        .await
    }

    async fn delete_record(&self, group: &str, query: &str) -> (u16, Value) {
        self.send("DELETE", &format!("/FeatureGroup/{group}?{query}"), None)
            .await
    }

    /// The record `identifier` names in `group`'s online store, read through the store's
    /// OWN API ([`OnlineStore`]) rather than off the wire.
    ///
    /// A soft-deleted record is served by no operation of this data plane — `GetRecord`
    /// and `BatchGetRecord` withhold it and `ListRecords` reports only its identifier — so
    /// the TTL state a tombstone keeps is read here, from the same store the handlers
    /// write. That this IS that store is shown by every other assertion in these suites,
    /// each of which goes through the front door.
    fn stored_record(&self, group: &str, identifier: &str) -> Option<StoredRecord> {
        let state = self.engine.state::<OnlineStore>();
        let mut store = state.lock();
        store.group_mut(group)?.record(identifier).cloned()
    }
}

/// One `FeatureValue` with a scalar value.
fn feature(name: &str, value: &str) -> Value {
    json!({ "FeatureName": name, "ValueAsString": value })
}

/// The instant an event time or an `ExpiresAt` denotes, in epoch seconds, in either
/// format a Feature Store event time takes.
fn secs(iso_or_epoch: &str) -> f64 {
    DateTime::from_str(iso_or_epoch, Format::DateTime)
        .map(|parsed| parsed.as_secs_f64())
        .unwrap_or_else(|_| iso_or_epoch.parse::<f64>().expect("a parsable event time"))
}

/// An instant one day from now, so a record written under a TTL of any size in these
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

/// [`tomorrow`] as the epoch-seconds string an event-time feature of type `Fractional`
/// carries.
fn tomorrow_epoch() -> String {
    format!("{}", tomorrow() as i64)
}

/// The `ExpiresAt` a `GetRecord` response carries, or `None` when it carries none.
fn expires_at(body: &Value) -> Option<String> {
    body.get("ExpiresAt")
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// Assert that `body`'s `ExpiresAt` is `expected_delta` seconds after `event_time`, or
/// absent when `expected_delta` is `None`.
fn assert_expiry(case: &str, body: &Value, event_time: &str, expected_delta: Option<f64>) {
    let actual = expires_at(body);
    match (expected_delta, actual.as_deref()) {
        (None, actual) => assert!(
            actual.is_none(),
            "case {case:?}: expected no ExpiresAt, got {actual:?}"
        ),
        (Some(expected), Some(at)) => {
            assert!(
                DateTime::from_str(at, Format::DateTime).is_ok(),
                "case {case:?}: expected an ISO-8601 ExpiresAt, got {at:?}"
            );
            let delta = secs(at) - secs(event_time);
            assert!(
                (delta - expected).abs() < 0.001,
                "case {case:?}: expected ExpiresAt {expected} s after the event time, got \
                 {delta} s (ExpiresAt {at:?})"
            );
        }
        (Some(expected), None) => panic!(
            "case {case:?}: expected ExpiresAt {expected} s after the event time, got no \
             ExpiresAt at all"
        ),
    }
}

/// Every unit the model's `TtlDurationUnit` enum declares, with its span in seconds (see
/// `TTL_DURATION_UNIT_VALUES` in `ops::types::shared_types`, from the model's
/// `TtlDurationUnit` shape): the default a feature group can be configured with is one of
/// these durations, so the table spans them all, with the published lower bound of
/// `TtlDurationValue` (range 1..) and larger values.
///
/// `(case, unit, value, the seconds the duration is)`
const DEFAULT_TTL_ROWS: [(&str, &str, i32, f64); 7] = [
    ("Seconds, the lower bound of TtlDurationValue", "Seconds", 1, 1.0),
    ("Seconds", "Seconds", 45, 45.0),
    ("Minutes", "Minutes", 15, 900.0),
    ("Hours", "Hours", 36, 129_600.0),
    ("Days", "Days", 2, 172_800.0),
    ("Weeks", "Weeks", 4, 2_419_200.0),
    ("Weeks, a large value", "Weeks", 52, 31_449_600.0),
];

#[tokio::test]
async fn an_ingestion_with_no_request_ttl_takes_the_feature_groups_default() {
    let emulator = Emulator::new();
    let iso = tomorrow_iso();
    let epoch = tomorrow_epoch();

    for (index, (case, unit, value, expected)) in DEFAULT_TTL_ROWS.iter().enumerate() {
        // Each row needs its own feature group: the default TTL is the GROUP's.
        let group = format!("defaultttl{index}");
        // The control plane records the group's `OnlineStoreConfig.TtlDuration` in
        // seconds, which is what this row's unit and value convert to.
        emulator.create_feature_group(&group, Some(*expected));
        // Both formats a Feature Store event time takes, alternating over the rows, so
        // the default applies whichever one the record carries.
        let event_time = if index % 2 == 0 {
            iso.as_str()
        } else {
            epoch.as_str()
        };

        // PutRecord with NO TtlDuration member at all.
        let (status, body) = emulator
            .put_record(
                &group,
                json!({ "Record": [
                    feature(IDENTIFIER_FEATURE, "C-put"),
                    feature(EVENT_TIME_FEATURE, event_time),
                    feature("city", "Seattle")
                ] }),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected PutRecord 200, got {status} ({body})"
        );
        let (status, body) = emulator.get_record(&group, "C-put").await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected GetRecord 200, got {status} ({body})"
        );
        assert_expiry(
            &format!("{case} via PutRecord with no TtlDuration ({unit} {value})"),
            &body,
            event_time,
            Some(*expected),
        );

        // The same ingestion as one entry of a batch, with no TtlDuration on the entry
        // and none on the request.
        let (status, body) = emulator
            .batch_write_record(json!({ "Entries": [{
                "FeatureGroupName": group,
                "Record": [
                    feature(IDENTIFIER_FEATURE, "C-batch"),
                    feature(EVENT_TIME_FEATURE, event_time),
                    feature("city", "Denver")
                ]
            }] }))
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected BatchWriteRecord 200, got {status} ({body})"
        );
        assert_eq!(
            body["Errors"],
            json!([]),
            "case {case:?}: expected no entry errors, got {}",
            body["Errors"]
        );
        let (_, body) = emulator.get_record(&group, "C-batch").await;
        assert_expiry(
            &format!("{case} via BatchWriteRecord with no TtlDuration ({unit} {value})"),
            &body,
            event_time,
            Some(*expected),
        );
    }

    // A group configured with NO default TtlDuration leaves a record written without one
    // under no TTL at all, so it reports no ExpiresAt.
    emulator.create_feature_group("nodefaultttl", None);
    let (status, _) = emulator
        .put_record(
            "nodefaultttl",
            json!({ "Record": [
                feature(IDENTIFIER_FEATURE, "C-none"),
                feature(EVENT_TIME_FEATURE, &iso)
            ] }),
        )
        .await;
    assert_eq!(status, 200, "case \"no group default\": expected PutRecord 200, got {status}");
    let (_, body) = emulator.get_record("nodefaultttl", "C-none").await;
    assert_expiry("no group default, no request TtlDuration", &body, &iso, None);
}

#[tokio::test]
async fn a_record_level_ttl_supersedes_the_feature_groups_default() {
    let emulator = Emulator::new();
    let iso = tomorrow_iso();
    // The group's default is a week; every row below states its own duration, which must
    // be the one in force.
    const GROUP_DEFAULT_SECS: f64 = 604_800.0;
    emulator.create_feature_group("supersede", Some(GROUP_DEFAULT_SECS));

    for (index, (case, unit, value, expected)) in DEFAULT_TTL_ROWS.iter().enumerate() {
        let ttl = json!({ "Unit": unit, "Value": value });

        // PutRecord's own `TtlDuration`.
        let identifier = format!("C-put-{index}");
        let (status, body) = emulator
            .put_record(
                "supersede",
                json!({
                    "Record": [
                        feature(IDENTIFIER_FEATURE, &identifier),
                        feature(EVENT_TIME_FEATURE, &iso)
                    ],
                    "TtlDuration": ttl
                }),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected PutRecord 200, got {status} ({body})"
        );
        let (_, body) = emulator.get_record("supersede", &identifier).await;
        assert_expiry(
            &format!("a record-level {unit} {value} over the group default, via PutRecord"),
            &body,
            &iso,
            Some(*expected),
        );

        // A batch entry's own `TtlDuration`, and the request-level one that applies to an
        // entry naming none: both are record-level durations, so both supersede the
        // group's default.
        let entry_identifier = format!("C-entry-{index}");
        let request_identifier = format!("C-request-{index}");
        let (status, body) = emulator
            .batch_write_record(json!({
                "TtlDuration": ttl,
                "Entries": [
                    {
                        "FeatureGroupName": "supersede",
                        "Record": [
                            feature(IDENTIFIER_FEATURE, &entry_identifier),
                            feature(EVENT_TIME_FEATURE, &iso)
                        ],
                        "TtlDuration": ttl
                    },
                    {
                        "FeatureGroupName": "supersede",
                        "Record": [
                            feature(IDENTIFIER_FEATURE, &request_identifier),
                            feature(EVENT_TIME_FEATURE, &iso)
                        ]
                    }
                ]
            }))
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected BatchWriteRecord 200, got {status} ({body})"
        );
        for (sub_case, identifier) in [
            ("an entry-level duration", entry_identifier),
            ("a request-level duration", request_identifier),
        ] {
            let (_, body) = emulator.get_record("supersede", &identifier).await;
            assert_expiry(
                &format!("{sub_case} of {unit} {value} over the group default"),
                &body,
                &iso,
                Some(*expected),
            );
        }
    }
}

#[tokio::test]
async fn a_record_under_the_groups_default_ttl_is_hard_deleted_once_it_elapses() {
    let emulator = Emulator::new();
    // A one-second default TTL and an event time long past: the expiry is already due by
    // the time anything reads the group, so the record is hard-deleted — no waiting on a
    // clock.
    emulator.create_feature_group("expiringdefault", Some(1.0));
    let live_event_time = tomorrow_iso();

    // (case, identifier, event time, whether the record outlives the read)
    let rows = [
        ("an expiry already due", "C-expired", "2000-01-01T00:00:00Z", false),
        ("an expiry a day out", "C-live", live_event_time.as_str(), true),
    ];
    for (case, identifier, event_time, _) in rows {
        let (status, body) = emulator
            .put_record(
                "expiringdefault",
                json!({ "Record": [
                    feature(IDENTIFIER_FEATURE, identifier),
                    feature(EVENT_TIME_FEATURE, event_time)
                ] }),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected PutRecord 200, got {status} ({body})"
        );
    }

    for (case, identifier, _, expected_live) in rows {
        let (status, body) = emulator.get_record("expiringdefault", identifier).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected GetRecord 200, got {status} ({body})"
        );
        let actual = body.get("Record").cloned().unwrap_or(Value::Null);
        assert_eq!(
            !actual.is_null(),
            expected_live,
            "case {case:?}: expected the record present={expected_live}, got {actual:?}"
        );
    }

    let (_, body) = emulator.list_records("expiringdefault").await;
    let listed = body["RecordIdentifiers"].clone();
    assert_eq!(
        listed,
        json!(["C-live"]),
        "case \"the group listed\": expected only the unexpired record, got {listed}"
    );
}

#[tokio::test]
async fn an_update_re_derives_the_expiry_from_the_ttl_in_force() {
    let emulator = Emulator::new();
    let iso = tomorrow_iso();
    let later = DateTime::from_secs_f64(tomorrow() + 3_600.0)
        .fmt(Format::DateTime)
        .expect("format an ISO-8601 event time");
    const GROUP_DEFAULT_SECS: f64 = 172_800.0;
    emulator.create_feature_group("updatedefault", Some(GROUP_DEFAULT_SECS));

    // (case, the TtlDuration the update states, the expiry the updated record must carry
    //  relative to its new event time)
    let rows: [(&str, Option<Value>, f64); 3] = [
        // "If you omit this parameter, the record's existing TTL setting remains
        // unchanged" — the setting here is the group's default, under which the record
        // was ingested, so the expiry moves with the event time.
        ("an update naming no TtlDuration", None, GROUP_DEFAULT_SECS),
        (
            "an update naming its own TtlDuration",
            Some(json!({ "Unit": "Hours", "Value": 3 })),
            10_800.0,
        ),
        (
            "an update naming a TtlDuration in another unit",
            Some(json!({ "Unit": "Weeks", "Value": 1 })),
            604_800.0,
        ),
    ];

    for (index, (case, ttl, expected)) in rows.into_iter().enumerate() {
        let identifier = format!("C-update-{index}");
        // Ingested with NO request-level TtlDuration, so the record is under the group's
        // default.
        let (status, body) = emulator
            .put_record(
                "updatedefault",
                json!({ "Record": [
                    feature(IDENTIFIER_FEATURE, &identifier),
                    feature(EVENT_TIME_FEATURE, &iso),
                    feature("city", "Seattle")
                ] }),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected PutRecord 200, got {status} ({body})"
        );
        let (_, body) = emulator.get_record("updatedefault", &identifier).await;
        assert_expiry(
            &format!("{case}: the ingested record"),
            &body,
            &iso,
            Some(GROUP_DEFAULT_SECS),
        );

        let mut request = json!({
            "RecordIdentifierValueAsString": identifier,
            "Features": [
                feature(EVENT_TIME_FEATURE, &later),
                feature("city", "Portland")
            ]
        });
        if let Some(ttl) = ttl {
            request["TtlDuration"] = ttl;
        }
        let (status, body) = emulator.update_record("updatedefault", request).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected UpdateRecord 200, got {status} ({body})"
        );

        let (_, body) = emulator.get_record("updatedefault", &identifier).await;
        assert_expiry(
            &format!("{case}: after the update"),
            &body,
            &later,
            Some(expected),
        );
    }
}

#[tokio::test]
async fn a_soft_delete_re_derives_the_expiry_from_the_ttl_the_record_was_written_under() {
    let emulator = Emulator::new();
    let ingested_at = tomorrow_iso();
    // The delete's own `EventTime`, an hour after the record's: a `SoftDelete` moves the
    // record's event time to it, so it is the instant the expiry is measured from.
    let deleted_at = DateTime::from_secs_f64(tomorrow() + 3_600.0)
        .fmt(Format::DateTime)
        .expect("format an ISO-8601 event time");

    for (index, (case, unit, value, expected)) in DEFAULT_TTL_ROWS.iter().enumerate() {
        // The same duration from each of the two sources a record can be under one from,
        // so neither the write's own `TtlDuration` nor the group's default is lost.
        let sources: [(&str, Option<f64>, Option<Value>); 2] = [
            (
                "a record-level TtlDuration",
                None,
                Some(json!({ "Unit": unit, "Value": value })),
            ),
            ("the feature group's default TTL", Some(*expected), None),
        ];
        for (source, group_default, request_ttl) in sources {
            let group = format!("softdelete{index}{}", group_default.is_some() as u8);
            let case = format!("{case} as {source}");
            emulator.create_feature_group(&group, group_default);

            let mut request = json!({ "Record": [
                feature(IDENTIFIER_FEATURE, "C1"),
                feature(EVENT_TIME_FEATURE, &ingested_at),
                feature("city", "Seattle")
            ] });
            if let Some(ttl) = request_ttl {
                request["TtlDuration"] = ttl;
            }
            let (status, body) = emulator.put_record(&group, request).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected PutRecord 200, got {status} ({body})"
            );
            let (_, body) = emulator.get_record(&group, "C1").await;
            assert_expiry(
                &format!("{case}: the ingested record"),
                &body,
                &ingested_at,
                Some(*expected),
            );

            let (status, body) = emulator
                .delete_record(
                    &group,
                    &format!(
                        "RecordIdentifierValueAsString=C1&EventTime={deleted_at}\
                         &DeletionMode=SoftDelete"
                    ),
                )
                .await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected DeleteRecord 200, got {status} ({body})"
            );

            let stored = emulator
                .stored_record(&group, "C1")
                .unwrap_or_else(|| panic!("case {case:?}: the tombstone must still be held"));
            assert!(
                stored.soft_deleted,
                "case {case:?}: expected a soft-deleted record, got {stored:?}"
            );
            assert_eq!(
                stored.ttl_secs,
                Some(*expected),
                "case {case:?}: expected the tombstone to keep the duration of {expected} s, \
                 got {:?}",
                stored.ttl_secs
            );
            let at = stored.expires_at_secs.unwrap_or_else(|| {
                panic!("case {case:?}: expected the tombstone to carry an expiry, got {stored:?}")
            });
            let delta = at - secs(&deleted_at);
            assert!(
                (delta - *expected).abs() < 0.001,
                "case {case:?}: expected the expiry {expected} s after the delete's event \
                 time, got {delta} s"
            );
            // The ISO-8601 member and the seconds one are the same instant.
            let iso = stored
                .expires_at
                .clone()
                .unwrap_or_else(|| panic!("case {case:?}: expected an ExpiresAt, got {stored:?}"));
            assert!(
                (secs(&iso) - at).abs() < 0.001,
                "case {case:?}: expected ExpiresAt {iso:?} to be the stored instant {at}"
            );
        }
    }
}

/// One row of the tombstone-expiry table: which of the two TTL sources the deleted record
/// is under, the `EventTime` the delete states, and whether the tombstone the delete
/// leaves outlives the next read of its feature group.
struct TombstoneRow {
    case: &'static str,
    /// The `OnlineStoreConfig.TtlDuration` the feature group is created with, in seconds.
    group_default_secs: Option<f64>,
    /// The `TtlDuration` the write of the record states, when it states one.
    request_ttl: Option<Value>,
    deleted_at: String,
    outlives: bool,
}

#[tokio::test]
async fn a_soft_deleted_record_is_hard_deleted_once_its_re_derived_expiry_falls_due() {
    // An instant long past: a tombstone whose event time is this one has an expiry
    // already due under any duration, so the hard delete the clock owes is materialised
    // by the next read — no waiting on a clock.
    const LONG_PAST: &str = "2000-01-01T00:00:00Z";
    let a_day_out = tomorrow_iso();

    // Each record below is written with NO value for the group's event-time feature, so
    // the record itself is under a duration but at no instant, and the first event time
    // it has is the one the delete states: that is what makes the re-derived expiry of a
    // tombstone observable without waiting for one to fall due.
    let rows = vec![
        TombstoneRow {
            case: "a one-second record TTL, deleted long past",
            group_default_secs: None,
            request_ttl: Some(json!({ "Unit": "Seconds", "Value": 1 })),
            deleted_at: LONG_PAST.to_owned(),
            outlives: false,
        },
        TombstoneRow {
            case: "an hour's record TTL, deleted long past",
            group_default_secs: None,
            request_ttl: Some(json!({ "Unit": "Hours", "Value": 1 })),
            deleted_at: LONG_PAST.to_owned(),
            outlives: false,
        },
        TombstoneRow {
            case: "a 52-week record TTL, deleted long past",
            group_default_secs: None,
            request_ttl: Some(json!({ "Unit": "Weeks", "Value": 52 })),
            deleted_at: LONG_PAST.to_owned(),
            outlives: false,
        },
        TombstoneRow {
            case: "the feature group's default TTL, deleted long past",
            group_default_secs: Some(1.0),
            request_ttl: None,
            deleted_at: LONG_PAST.to_owned(),
            outlives: false,
        },
        TombstoneRow {
            case: "a 52-week record TTL, deleted a day out",
            group_default_secs: None,
            request_ttl: Some(json!({ "Unit": "Weeks", "Value": 52 })),
            deleted_at: a_day_out,
            outlives: true,
        },
        TombstoneRow {
            case: "no TTL in force at all",
            group_default_secs: None,
            request_ttl: None,
            deleted_at: LONG_PAST.to_owned(),
            outlives: true,
        },
    ];

    for (index, row) in rows.into_iter().enumerate() {
        let TombstoneRow {
            case,
            group_default_secs,
            request_ttl,
            deleted_at,
            outlives,
        } = row;
        let emulator = Emulator::new();
        let group = format!("tombstone{index}");
        emulator.create_feature_group(&group, group_default_secs);

        let mut request = json!({ "Record": [
            feature(IDENTIFIER_FEATURE, "C1"),
            feature("city", "Seattle")
        ] });
        if let Some(ttl) = request_ttl {
            request["TtlDuration"] = ttl;
        }
        let (status, body) = emulator.put_record(&group, request).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected PutRecord 200, got {status} ({body})"
        );
        let (_, body) = emulator.list_records_including_soft_deleted(&group).await;
        assert_eq!(
            body["RecordIdentifiers"],
            json!(["C1"]),
            "case {case:?}: expected the written identifier to be listed, got {}",
            body["RecordIdentifiers"]
        );

        let (status, body) = emulator
            .delete_record(
                &group,
                &format!(
                    "RecordIdentifierValueAsString=C1&EventTime={deleted_at}\
                     &DeletionMode=SoftDelete"
                ),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected DeleteRecord 200, got {status} ({body})"
        );

        let (status, body) = emulator.list_records_including_soft_deleted(&group).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected ListRecords 200, got {status} ({body})"
        );
        let expected = if outlives { json!(["C1"]) } else { json!([]) };
        assert_eq!(
            body["RecordIdentifiers"], expected,
            "case {case:?}: expected the soft-deleted identifiers {expected}, got {}",
            body["RecordIdentifiers"]
        );
    }
}
