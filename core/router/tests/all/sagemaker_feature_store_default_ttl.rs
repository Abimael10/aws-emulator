// SPDX-License-Identifier: CC0-1.0

//! The TTL half of the SageMaker Feature Store hand-off: the CONTROL plane configures a
//! feature group's DEFAULT `TtlDuration`, and the FeatureStore Runtime DATA plane stores
//! that group's records under it.
//!
//! RULE: a group's default `TtlDuration` is a property of the records the data plane
//! STORES — a record written with no duration of its own carries
//! `ExpiresAt = EventTime + the group's default`, and the default follows the control plane,
//! configured on the create and changed or dropped by `UpdateFeatureGroup`. It is NOT a
//! condition on a later request: `UpdateRecord` requires an `EventTime` beside a
//! `TtlDuration` the REQUEST states and refuses one that states a duration without it with
//! the modelled `ValidationError` (HTTP 400), changing nothing, while an update that states
//! no `TtlDuration` applies with the record's own event time — from which the expiry is
//! re-derived — whatever TTL the record's group configures.
//!
//! The runtime API carries no feature-group configuration — an `UpdateRecord` names a
//! group, a record and features — so the data plane can only know a group's default TTL
//! because the control plane recorded it in ONE shared directory
//! (`service_common::feature_store_groups`), on the create and on every update that
//! changes it. That is observable only where both halves run on the SAME router and the
//! SAME engine, which is what this suite drives, through the harness the hand-off suite
//! beside it declares.

use std::sync::Arc;

use aws_smithy_types::date_time::{DateTime, Format};
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::sagemaker_feature_store_control_plane_handoff::{
    build_router, dispatch, feature, record, Answer,
};

/// The two feature names every group this suite creates fixes.
const IDENTIFIER_FEATURE: &str = "record_id";
const EVENT_TIME_FEATURE: &str = "event_time";

/// The event time the seeded records carry. Well in the future, so a `TtlDuration` the row
/// applies does not put the record's `ExpiresAt` in the past, which would evict it before
/// the row reads it back.
const EVENT_TIME: &str = "2035-01-01T00:00:00Z";

/// How the data plane states an update that itself specifies a `TtlDuration` but carries no
/// `EventTime`: fixed text, as the service words this error class, naming nothing of the
/// request or the feature group.
const EVENT_TIME_REQUIRED: &str =
    "Validation Error: EventTime is required when TtlDuration is specified or when the feature \
     group has a default TTL configured. Include the EventTime feature in the Features list.";

/// `CreateFeatureGroup` on the control plane, with the `OnlineStoreConfig` the row states.
async fn create_group(router: &MultiServiceRouter, name: &str, online_store: Value) -> Answer {
    dispatch(
        router,
        "POST",
        "/",
        &[("x-amz-target", "SageMaker.CreateFeatureGroup")],
        Some(json!({
            "FeatureGroupName": name,
            "RecordIdentifierFeatureName": IDENTIFIER_FEATURE,
            "EventTimeFeatureName": EVENT_TIME_FEATURE,
            "FeatureDefinitions": [
                { "FeatureName": IDENTIFIER_FEATURE, "FeatureType": "String" },
                { "FeatureName": EVENT_TIME_FEATURE, "FeatureType": "Fractional" },
                { "FeatureName": "feature_a", "FeatureType": "String" }
            ],
            "OnlineStoreConfig": online_store
        })),
    )
    .await
}

/// `UpdateFeatureGroup` on the control plane, changing the group's online store
/// configuration.
async fn update_group(router: &MultiServiceRouter, name: &str, online_store: Value) -> Answer {
    dispatch(
        router,
        "POST",
        "/",
        &[("x-amz-target", "SageMaker.UpdateFeatureGroup")],
        Some(json!({
            "FeatureGroupName": name,
            "OnlineStoreConfig": online_store
        })),
    )
    .await
}

/// `PutRecord` on the data plane: one record of `group` at [`EVENT_TIME`], with no
/// `TtlDuration` of its own, so it is stored under whatever default the group configures.
async fn put_record(router: &MultiServiceRouter, group: &str, identifier: &str) -> Answer {
    dispatch(
        router,
        "PUT",
        &format!("/FeatureGroup/{group}"),
        &[],
        Some(json!({ "Record": record(identifier, EVENT_TIME, "original") })),
    )
    .await
}

/// `UpdateRecord` on the data plane: the posted features, and the request's own
/// `TtlDuration` when the row states one.
async fn update_record(
    router: &MultiServiceRouter,
    group: &str,
    identifier: &str,
    features: Value,
    ttl_duration: Option<Value>,
) -> Answer {
    let mut body = json!({
        "RecordIdentifierValueAsString": identifier,
        "Features": features
    });
    if let Some(ttl) = ttl_duration {
        body["TtlDuration"] = ttl;
    }
    dispatch(
        router,
        "POST",
        &format!("/FeatureGroup/{group}/Record"),
        &[],
        Some(body),
    )
    .await
}

/// `GetRecord` for one record, asking for its expiration time as well.
async fn get_record(router: &MultiServiceRouter, group: &str, identifier: &str) -> Answer {
    dispatch(
        router,
        "GET",
        &format!(
            "/FeatureGroup/{group}?RecordIdentifierValueAsString={identifier}\
             &ExpirationTimeResponse=Enabled"
        ),
        &[],
        None,
    )
    .await
}

/// The value `feature_a` holds in the record the data plane serves, and `""` when it serves
/// no record at all.
async fn served_feature_a(router: &MultiServiceRouter, group: &str, identifier: &str) -> String {
    let got = get_record(router, group, identifier).await;
    got.body
        .get("Record")
        .and_then(Value::as_array)
        .and_then(|features| {
            features
                .iter()
                .find(|held| held["FeatureName"] == json!("feature_a"))
        })
        .and_then(|held| held["ValueAsString"].as_str())
        .unwrap_or_default()
        .to_owned()
}

/// The message an answer carries, under either spelling: the framework's own refusal uses
/// `message`, a modelled error's `Message` member.
fn message(answer: &Answer) -> String {
    answer
        .body
        .get("message")
        .or_else(|| answer.body.get("Message"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// The instant an ISO-8601 timestamp denotes, in epoch seconds.
fn secs(at: &str) -> f64 {
    DateTime::from_str(at, Format::DateTime)
        .map(|parsed| parsed.as_secs_f64())
        .unwrap_or_else(|_| panic!("an ISO-8601 timestamp, got {at:?}"))
}

/// Assert the record `group`/`identifier` is served with an `ExpiresAt` `expected_delta`
/// seconds after [`EVENT_TIME`], or with none at all when `expected_delta` is `None`.
async fn assert_expiry(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    identifier: &str,
    expected_delta: Option<f64>,
) {
    let got = get_record(router, group, identifier).await;
    let actual = got.body.get("ExpiresAt").and_then(Value::as_str);
    match (expected_delta, actual) {
        (None, actual) => assert!(
            actual.is_none(),
            "case {case:?}: expected no ExpiresAt, got {actual:?}"
        ),
        (Some(expected), Some(at)) => {
            let delta = secs(at) - secs(EVENT_TIME);
            assert!(
                (delta - expected).abs() < 0.001,
                "case {case:?}: expected ExpiresAt {expected} s after the record's event time, \
                 got {delta} s (ExpiresAt {at:?})"
            );
        }
        (Some(expected), None) => panic!(
            "case {case:?}: expected ExpiresAt {expected} s after the record's event time, got \
             none at all (body {})",
            got.body
        ),
    }
}

/// One row of the TTL table: the case name, the group's name and `OnlineStoreConfig`, the
/// seconds the group's default `TtlDuration` is (`None` where it configures none), the
/// `TtlDuration` the update states and the seconds that is, and the status an update
/// carrying no `EventTime` must answer.
type TtlRow<'a> = (
    &'a str,
    &'a str,
    Value,
    Option<f64>,
    Option<Value>,
    Option<f64>,
    u16,
);

#[tokio::test]
async fn only_a_request_stated_ttl_duration_makes_an_update_need_an_event_time() {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let router = build_router(&engine);

    // The class the rule spans: how the TTL governing the record comes about — the group's
    // own default, configured on the create; the request's `TtlDuration`; both; or neither
    // — crossed with the units the control plane's `TtlDuration` takes. Only a duration the
    // REQUEST states makes the update need an `EventTime`.
    let rows: Vec<TtlRow<'_>> = vec![
        (
            "a group with a default TtlDuration in hours",
            "ttl-hours",
            json!({
                "EnableOnlineStore": true,
                "TtlDuration": { "Unit": "Hours", "Value": 1 }
            }),
            Some(3_600.0),
            None,
            None,
            200,
        ),
        (
            "a group with a default TtlDuration in weeks",
            "ttl-weeks",
            json!({
                "EnableOnlineStore": true,
                "TtlDuration": { "Unit": "Weeks", "Value": 4 }
            }),
            Some(2_419_200.0),
            None,
            None,
            200,
        ),
        (
            "a group with no default TtlDuration, and a TtlDuration on the request",
            "ttl-request",
            json!({ "EnableOnlineStore": true }),
            None,
            Some(json!({ "Unit": "Days", "Value": 2 })),
            Some(172_800.0),
            400,
        ),
        (
            "a group with a default TtlDuration, and a TtlDuration on the request",
            "ttl-both",
            json!({
                "EnableOnlineStore": true,
                "TtlDuration": { "Unit": "Days", "Value": 1 }
            }),
            Some(86_400.0),
            Some(json!({ "Unit": "Hours", "Value": 12 })),
            Some(43_200.0),
            400,
        ),
        (
            "a group with no default TtlDuration, and none on the request",
            "ttl-none",
            json!({ "EnableOnlineStore": true }),
            None,
            None,
            None,
            200,
        ),
    ];

    for (case, group, online_store, default_secs, ttl_duration, ttl_secs, expected_status) in rows
    {
        let created = create_group(&router, group, online_store).await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: expected CreateFeatureGroup 200, got {} with body {}",
            created.status, created.body
        );
        let put = put_record(&router, group, "R1").await;
        assert_eq!(
            put.status, 200,
            "case {case:?}: expected PutRecord 200, got {} with body {}",
            put.status, put.body
        );
        // The hand-off itself: a record written with no `TtlDuration` of its own is stored
        // under the default the CONTROL plane configured for its group, which is what its
        // served expiry is derived from.
        assert_expiry(
            &router,
            &format!("{case}: the ingested record"),
            group,
            "R1",
            default_secs,
        )
        .await;

        // The update that carries no value for the group's `EventTimeFeatureName`.
        let answered = update_record(
            &router,
            group,
            "R1",
            json!([feature("feature_a", "no-event-time")]),
            ttl_duration.clone(),
        )
        .await;
        assert_eq!(
            answered.status, expected_status,
            "case {case:?}: expected HTTP {expected_status} for an update with no EventTime, \
             got {} with body {}",
            answered.status, answered.body
        );
        if expected_status == 400 {
            let stated = message(&answered);
            assert_eq!(
                (answered.error_code().as_str(), stated.as_str()),
                ("ValidationError", EVENT_TIME_REQUIRED),
                "case {case:?}: expected ValidationError stating {EVENT_TIME_REQUIRED:?}, got \
                 {:?} with {stated:?}",
                answered.error_code()
            );
            let served = served_feature_a(&router, group, "R1").await;
            assert_eq!(
                served, "original",
                "case {case:?}: expected the refused update to change nothing, got \
                 feature_a {served:?}"
            );
        } else {
            // An update that states no `TtlDuration` is the documented partial update: it
            // applies with the record's own event time, and the expiry stays derived from
            // that event time under the TTL the record already stands under.
            let served = served_feature_a(&router, group, "R1").await;
            assert_eq!(
                served, "no-event-time",
                "case {case:?}: expected the update with no EventTime to be applied, got \
                 feature_a {served:?}"
            );
            assert_expiry(
                &router,
                &format!("{case}: after an update stating no TtlDuration"),
                group,
                "R1",
                default_secs,
            )
            .await;
        }

        // The control, in every row: the SAME update carrying the group's event-time
        // feature is applied, and a `TtlDuration` it states replaces the duration the
        // record stood under.
        let applied = update_record(
            &router,
            group,
            "R1",
            json!([
                feature("feature_a", "with-event-time"),
                feature(EVENT_TIME_FEATURE, EVENT_TIME)
            ]),
            ttl_duration,
        )
        .await;
        assert_eq!(
            applied.status, 200,
            "case {case:?}: expected an update carrying the EventTime to be applied, got {} \
             with body {}",
            applied.status, applied.body
        );
        let served = served_feature_a(&router, group, "R1").await;
        assert_eq!(
            served, "with-event-time",
            "case {case:?}: expected the applied update to be readable, got feature_a \
             {served:?}"
        );
        assert_expiry(
            &router,
            &format!("{case}: after an update carrying the EventTime"),
            group,
            "R1",
            ttl_secs.or(default_secs),
        )
        .await;
    }
}

#[tokio::test]
async fn the_groups_default_ttl_follows_the_control_plane_and_blocks_no_update() {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let router = build_router(&engine);

    // A group created WITH a default TTL: every record of it written without a duration of
    // its own is stored under that default.
    create_group(
        &router,
        "ttl-lifted",
        json!({
            "EnableOnlineStore": true,
            "TtlDuration": { "Unit": "Hours", "Value": 6 }
        }),
    )
    .await;
    let put = put_record(&router, "ttl-lifted", "R1").await;
    assert_eq!(
        put.status, 200,
        "case \"the seeding PutRecord\": expected 200, got {} with body {}",
        put.status, put.body
    );
    assert_expiry(
        &router,
        "a record ingested under the group's default of six hours",
        "ttl-lifted",
        "R1",
        Some(21_600.0),
    )
    .await;

    // The default is not a condition on a later update: one that states no `TtlDuration`
    // applies with the record's own event time, and the expiry stays derived from it.
    let applied = update_record(
        &router,
        "ttl-lifted",
        "R1",
        json!([feature("feature_a", "while-ttl-configured")]),
        None,
    )
    .await;
    assert_eq!(
        applied.status, 200,
        "case \"while the default TTL stands\": expected the update with no EventTime to be \
         applied, got {} with body {}",
        applied.status, applied.body
    );
    let served = served_feature_a(&router, "ttl-lifted", "R1").await;
    assert_eq!(
        served, "while-ttl-configured",
        "case \"while the default TTL stands\": expected the applied update to be readable, \
         got feature_a {served:?}"
    );
    assert_expiry(
        &router,
        "the updated record, still under the group's default",
        "ttl-lifted",
        "R1",
        Some(21_600.0),
    )
    .await;

    // "To remove the default TtlDuration from an existing feature group, use the
    // UpdateFeatureGroup API and set the TtlDuration Unit and Value to null"
    // (`UpdateFeatureGroup`): a record ingested after that stands under no TTL at all, so
    // it is served with no expiry.
    let updated = update_group(&router, "ttl-lifted", json!({ "TtlDuration": {} })).await;
    assert_eq!(
        updated.status, 200,
        "case \"UpdateFeatureGroup removing the default TtlDuration\": expected 200, got {} \
         with body {}",
        updated.status, updated.body
    );
    let put = put_record(&router, "ttl-lifted", "R2").await;
    assert_eq!(
        put.status, 200,
        "case \"a PutRecord after the default TTL is removed\": expected 200, got {} with body \
         {}",
        put.status, put.body
    );
    assert_expiry(
        &router,
        "a record ingested after the default TTL is removed",
        "ttl-lifted",
        "R2",
        None,
    )
    .await;
    let applied = update_record(
        &router,
        "ttl-lifted",
        "R2",
        json!([feature("feature_a", "after-ttl-removed")]),
        None,
    )
    .await;
    assert_eq!(
        applied.status, 200,
        "case \"after the default TTL is removed\": expected the update with no EventTime to \
         be applied, got {} with body {}",
        applied.status, applied.body
    );
    let served = served_feature_a(&router, "ttl-lifted", "R2").await;
    assert_eq!(
        served, "after-ttl-removed",
        "case \"after the default TTL is removed\": expected the applied update to be \
         readable, got feature_a {served:?}"
    );

    // And configuring one again puts the new default under the records ingested from then
    // on, so the directory follows the control plane rather than being written once.
    let reconfigured = update_group(
        &router,
        "ttl-lifted",
        json!({ "TtlDuration": { "Unit": "Days", "Value": 3 } }),
    )
    .await;
    assert_eq!(
        reconfigured.status, 200,
        "case \"UpdateFeatureGroup configuring a default TtlDuration again\": expected 200, \
         got {} with body {}",
        reconfigured.status, reconfigured.body
    );
    let put = put_record(&router, "ttl-lifted", "R3").await;
    assert_eq!(
        put.status, 200,
        "case \"a PutRecord after the default TTL is configured again\": expected 200, got {} \
         with body {}",
        put.status, put.body
    );
    assert_expiry(
        &router,
        "a record ingested under the newly configured default of three days",
        "ttl-lifted",
        "R3",
        Some(259_200.0),
    )
    .await;
    let applied = update_record(
        &router,
        "ttl-lifted",
        "R3",
        json!([feature("feature_a", "after-ttl-restored")]),
        None,
    )
    .await;
    assert_eq!(
        applied.status, 200,
        "case \"after the default TTL is configured again\": expected the update with no \
         EventTime to be applied, got {} with body {}",
        applied.status, applied.body
    );
    let served = served_feature_a(&router, "ttl-lifted", "R3").await;
    assert_eq!(
        served, "after-ttl-restored",
        "case \"after the default TTL is configured again\": expected the applied update to \
         be readable, got feature_a {served:?}"
    );
    assert_expiry(
        &router,
        "the updated record, under the newly configured default",
        "ttl-lifted",
        "R3",
        Some(259_200.0),
    )
    .await;
}
