// SPDX-License-Identifier: CC0-1.0

//! `UpdateRecord` through the whole router: what its documented per-call feature ceiling
//! COUNTS, and the one statement this data plane refuses an unreadable key feature with.
//!
//! RULE 1: a limit documented in FEATURES bounds the features a call updates — one value
//! per feature name, because the modelled `Features` list carries no `uniqueItems` and a
//! name a call repeats names ONE feature, whose value is the one named last — and not the
//! entries of the list the wire carries. So a call naming 51 features in 102 entries
//! updates 51 features and is served, and the count a refusal reports is the count of
//! features, the same count the store's own per-record bound is read on.
//!
//! RULE 2: the two features a feature group's definition fixes — its record identifier and
//! its event time — each hold ONE value, read from `ValueAsString` ("Used when your
//! CollectionType is None", `FeatureValue$ValueAsString`), so a write that posts either
//! with its value anywhere else states no value this plane can read and is refused with
//! that feature's own statement of the rule, identically by every write of this data plane:
//! `PutRecord`, `BatchWriteRecord` and `UpdateRecord` alike. A value the plane CAN read is
//! then checked against the record being updated, which is a different verdict and states
//! itself differently.
//!
//! Both halves are driven through the SAME router the emulator mounts, with the feature
//! group created by the SageMaker CONTROL plane's `CreateFeatureGroup` — the only thing
//! that fixes those two feature names — so what is asserted is the wire behaviour a client
//! sees.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::sagemaker_feature_store_control_plane_handoff::{
    build_router, create_feature_group, dispatch, feature, record, Answer,
};

/// The feature group every row below writes to.
const GROUP: &str = "updatecount";

/// The feature names the created group fixes (`sagemaker_feature_store_control_plane_handoff`
/// creates it with these), which are the two features RULE 2 is about.
const IDENTIFIER_FEATURE: &str = "record_id";
const EVENT_TIME_FEATURE: &str = "event_time";

/// The event time the seeded record is written with, so an update that posts none keeps
/// it and one that posts an unreadable one is refused for that alone.
const SEEDED_EVENT_TIME: &str = "2026-01-01T00:00:00Z";

/// The most FEATURES one `UpdateRecord` call may update: "You can update up to 100
/// features per call" (`smithy_query(query="doc", shape="UpdateRecord")`).
const DOCUMENTED_FEATURE_CEILING: usize = 100;

/// A booted router holding the SageMaker control plane and the FeatureStore runtime, with
/// [`GROUP`] created and one record seeded in its OnlineStore.
async fn seeded() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let router = build_router(&engine);
    let created = create_feature_group(&router, GROUP, true).await;
    assert_eq!(
        created.status, 200,
        "case \"CreateFeatureGroup\": expected HTTP 200, got {} with body {}",
        created.status, created.body
    );
    let put = dispatch(
        &router,
        "PUT",
        &format!("/FeatureGroup/{GROUP}"),
        &[],
        Some(json!({ "Record": record("R1", SEEDED_EVENT_TIME, "original") })),
    )
    .await;
    assert_eq!(
        put.status, 200,
        "case \"PutRecord seeding the record\": expected HTTP 200, got {} with body {}",
        put.status, put.body
    );
    router
}

/// `UpdateRecord` on the seeded record, with the `Features` given.
async fn update(router: &MultiServiceRouter, identifier: &str, features: Value) -> Answer {
    dispatch(
        router,
        "POST",
        &format!("/FeatureGroup/{GROUP}/Record"),
        &[],
        Some(json!({
            "RecordIdentifierValueAsString": identifier,
            "Features": features
        })),
    )
    .await
}

/// `PutRecord` of a whole record.
async fn put(router: &MultiServiceRouter, posted: Value) -> Answer {
    dispatch(
        router,
        "PUT",
        &format!("/FeatureGroup/{GROUP}"),
        &[],
        Some(json!({ "Record": posted })),
    )
    .await
}

/// `BatchWriteRecord` carrying that one record as its one entry.
async fn batch_write(router: &MultiServiceRouter, posted: Value) -> Answer {
    dispatch(
        router,
        "POST",
        "/BatchWriteRecord",
        &[],
        Some(json!({ "Entries": [ { "FeatureGroupName": GROUP, "Record": posted } ] })),
    )
    .await
}

/// `GetRecord` of the seeded record.
async fn get(router: &MultiServiceRouter) -> Answer {
    dispatch(
        router,
        "GET",
        &format!("/FeatureGroup/{GROUP}?RecordIdentifierValueAsString=R1"),
        &[],
        None,
    )
    .await
}

/// One `FeatureValue` whose value sits in `ValueAsStringList`.
fn list_feature(name: &str, values: &[&str]) -> Value {
    json!({ "FeatureName": name, "ValueAsStringList": values })
}

/// One `FeatureValue` carrying neither value member.
fn feature_without_value(name: &str) -> Value {
    json!({ "FeatureName": name })
}

/// The message an error response carries: this service's modelled errors declare a
/// `Message` member, while the framework's own constraint refusal carries `message`.
fn message(body: &Value) -> String {
    body.get("message")
        .or_else(|| body.get("Message"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// The `Errors` a `BatchWriteRecord` answer reports, as `(ErrorCode, ErrorMessage)`.
fn reported_errors(body: &Value) -> Vec<(String, String)> {
    body.get("Errors")
        .and_then(Value::as_array)
        .map(|errors| {
            errors
                .iter()
                .map(|error| {
                    (
                        error["ErrorCode"].as_str().unwrap_or_default().to_owned(),
                        error["ErrorMessage"].as_str().unwrap_or_default().to_owned(),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The features a `GetRecord` answer serves, as `(FeatureName, value)` pairs where the
/// value is the scalar or the list the answer carried.
fn served(body: &Value) -> Vec<(String, Value)> {
    body.get("Record")
        .and_then(Value::as_array)
        .map(|features| {
            features
                .iter()
                .map(|held| {
                    (
                        held["FeatureName"].as_str().unwrap_or_default().to_owned(),
                        held.get("ValueAsString")
                            .or_else(|| held.get("ValueAsStringList"))
                            .cloned()
                            .unwrap_or(Value::Null),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The value the served record holds for `name` (`Value::Null` when it holds none).
fn served_value(body: &Value, name: &str) -> Value {
    served(body)
        .into_iter()
        .find(|(held, _)| held == name)
        .map(|(_, value)| value)
        .unwrap_or(Value::Null)
}

/// A `Features` list naming `distinct` features, each named `repetitions` times: the
/// occurrences of one name are interleaved round by round, and the value of round `round`
/// is `v{round}`, so the value every feature is named LAST with is
/// `v{repetitions - 1}`.
fn posted_features(distinct: usize, repetitions: usize) -> Vec<Value> {
    let mut features = Vec::with_capacity(distinct * repetitions);
    for round in 0..repetitions {
        for index in 0..distinct {
            features.push(feature(&format!("feature_{index}"), &format!("v{round}")));
        }
    }
    features
}

#[tokio::test]
async fn the_documented_update_ceiling_counts_features_not_posted_list_entries() {
    // The class the rule spans: how many DISTINCT features the call updates, against how
    // many ENTRIES it spells them in. The rows pair each side of the ceiling with a list
    // that names every feature once, twice and three times — so the verdict turns on the
    // feature count alone, and the recorded shape (every name once) is one row.
    //
    // (case, distinct features, times each is named, expected status)
    let rows: [(&str, usize, usize, u16); 9] = [
        ("one feature, named once", 1, 1, 200),
        ("one feature, named four times", 1, 4, 200),
        ("the documented ceiling, each name once", 100, 1, 200),
        ("half the ceiling, every name twice", 51, 2, 200),
        ("the ceiling, every name twice", 100, 2, 200),
        ("the ceiling, every name three times", 100, 3, 200),
        ("one feature past the ceiling", 101, 1, 400),
        ("one feature past the ceiling, every name twice", 101, 2, 400),
        ("well past the ceiling", 250, 1, 400),
    ];

    for (case, distinct, repetitions, expected_status) in rows {
        let router = seeded().await;
        let features = posted_features(distinct, repetitions);
        let entries = features.len();
        let answered = update(&router, "R1", json!(features)).await;
        assert_eq!(
            answered.status, expected_status,
            "case {case:?}: expected HTTP {expected_status} for {distinct} features in \
             {entries} entries, got {} with body {}",
            answered.status, answered.body
        );

        let read_back = get(&router).await;
        if expected_status == 200 {
            // The update applied: each named feature holds the value it was named LAST
            // with, and the record carries each of them ONCE beside the three features it
            // was seeded with.
            let held = served(&read_back.body);
            assert_eq!(
                held.len(),
                distinct + 3,
                "case {case:?}: expected the record to hold {} features ({distinct} updated \
                 beside the three it was seeded with), got {held:?}",
                distinct + 3
            );
            let last = json!(format!("v{}", repetitions - 1));
            for index in [0, distinct - 1] {
                let name = format!("feature_{index}");
                let actual = served_value(&read_back.body, &name);
                assert_eq!(
                    actual, last,
                    "case {case:?}: expected {name} to hold the value it was named last \
                     with, {last:?}, got {actual:?}"
                );
            }
        } else {
            // The refusal states the modelled error, and reports the FEATURES the call
            // updates rather than the entries it spelled them in.
            let code = answered.error_code();
            let text = message(&answered.body);
            assert_eq!(
                (answered.status, code.as_str()),
                (400, "ValidationError"),
                "case {case:?}: expected HTTP 400 ValidationError, got {} {code:?} with \
                 message {text:?}",
                answered.status
            );
            assert_eq!(
                text,
                format!(
                    "Features: at most {DOCUMENTED_FEATURE_CEILING} features can be updated \
                     per call, but {distinct} were supplied"
                ),
                "case {case:?}: expected the refusal to report the {distinct} features the \
                 call updates, got {text:?}"
            );
            let held = served(&read_back.body);
            assert_eq!(
                held.len(),
                3,
                "case {case:?}: expected the refused update to leave the seeded record \
                 standing, got {held:?}"
            );
        }
    }
}

/// One row of the unreadable-key-feature table: the case, the key feature under test,
/// and the form the value posted for it takes.
type UnreadableForm = (&'static str, &'static str, fn(&str) -> Value);

#[tokio::test]
async fn a_key_feature_with_no_readable_value_is_refused_the_same_way_by_every_write() {
    // The class the rule spans: WHICH of the two features the group's definition fixes
    // carries the unreadable value, WHICH form that value arrives in, and WHICH write of
    // this data plane posts it.
    //
    // (case, the key feature under test, the value posted for it)
    let forms: [UnreadableForm; 4] = [
        (
            "the record identifier feature in ValueAsStringList",
            IDENTIFIER_FEATURE,
            |name| list_feature(name, &["R1"]),
        ),
        (
            "the record identifier feature with neither value member",
            IDENTIFIER_FEATURE,
            feature_without_value,
        ),
        (
            "the event time feature in ValueAsStringList",
            EVENT_TIME_FEATURE,
            |name| list_feature(name, &[SEEDED_EVENT_TIME]),
        ),
        (
            "the event time feature with neither value member",
            EVENT_TIME_FEATURE,
            feature_without_value,
        ),
    ];

    for (case, key_feature, form) in forms {
        let posted = form(key_feature);
        // The statement is the one the key feature's own rule states, so the two features
        // state the member their value has to be in, each in its own wording.
        let expected_message = if key_feature == IDENTIFIER_FEATURE {
            format!(
                "the record identifier feature '{IDENTIFIER_FEATURE}' must carry its value in \
                 ValueAsString"
            )
        } else {
            format!(
                "EventTime: the event time feature '{EVENT_TIME_FEATURE}' must carry its value \
                 in ValueAsString"
            )
        };

        // The whole record a `PutRecord`/`BatchWriteRecord` posts: the key feature under
        // test in the form of the row, the other one readable.
        let whole_record = if key_feature == IDENTIFIER_FEATURE {
            json!([
                posted.clone(),
                feature(EVENT_TIME_FEATURE, SEEDED_EVENT_TIME),
                feature("feature_a", "written")
            ])
        } else {
            json!([
                feature(IDENTIFIER_FEATURE, "R1"),
                posted.clone(),
                feature("feature_a", "written")
            ])
        };

        let router = seeded().await;

        let refused = put(&router, whole_record.clone()).await;
        let code = refused.error_code();
        let text = message(&refused.body);
        assert_eq!(
            (refused.status, code.as_str(), text.as_str()),
            (400, "ValidationError", expected_message.as_str()),
            "case {case:?} (PutRecord): expected HTTP 400 ValidationError stating the rule, \
             got {} {code:?} with message {text:?}",
            refused.status
        );

        let reported = batch_write(&router, whole_record).await;
        assert_eq!(
            reported.status, 200,
            "case {case:?} (BatchWriteRecord): expected HTTP 200 reporting the entry, got {} \
             with body {}",
            reported.status, reported.body
        );
        let errors = reported_errors(&reported.body);
        assert_eq!(
            errors,
            vec![("ValidationError".to_owned(), expected_message.clone())],
            "case {case:?} (BatchWriteRecord): expected the entry's own failure to state the \
             same rule, got {errors:?}"
        );

        // The PARTIAL update posts the key feature under test and one ordinary feature,
        // and answers that very statement — the same posted value refused the same way.
        let updated = update(
            &router,
            "R1",
            json!([posted, feature("feature_a", "updated")]),
        )
        .await;
        let code = updated.error_code();
        let text = message(&updated.body);
        assert_eq!(
            (updated.status, code.as_str(), text.as_str()),
            (400, "ValidationError", expected_message.as_str()),
            "case {case:?} (UpdateRecord): expected HTTP 400 ValidationError stating the same \
             rule as the whole-record writes, got {} {code:?} with message {text:?}",
            updated.status
        );

        let read_back = get(&router).await;
        let feature_a = served_value(&read_back.body, "feature_a");
        assert_eq!(
            feature_a,
            json!("original"),
            "case {case:?}: expected every refused write to leave the seeded record \
             standing, got feature_a {feature_a:?}"
        );
    }
}

#[tokio::test]
async fn the_identifier_feature_an_update_posts_is_read_for_the_one_value_it_states() {
    // The class the rule spans: what an update states for the group's record-identifier
    // feature — nothing, the record's own value, another record's value, or no value this
    // plane can read as one — where the last is the posted value's own defect and states
    // itself as such, not as a value that fails to match.
    let unreadable = format!(
        "the record identifier feature '{IDENTIFIER_FEATURE}' must carry its value in \
         ValueAsString"
    );
    let mismatch = format!(
        "Features: the record identifier feature '{IDENTIFIER_FEATURE}' must carry the \
         RecordIdentifierValueAsString of the record being updated ('R1')"
    );

    // (case, what the update posts for the identifier feature, expected status, expected
    //  message, the value feature_a must hold afterwards)
    let rows: Vec<(&str, Option<Value>, u16, String, &str)> = vec![
        ("the identifier feature not posted at all", None, 200, String::new(), "updated"),
        (
            "the record's own identifier value",
            Some(feature(IDENTIFIER_FEATURE, "R1")),
            200,
            String::new(),
            "updated",
        ),
        (
            "another record's identifier value",
            Some(feature(IDENTIFIER_FEATURE, "R9")),
            400,
            mismatch,
            "original",
        ),
        (
            "the identifier value in ValueAsStringList",
            Some(list_feature(IDENTIFIER_FEATURE, &["R1"])),
            400,
            unreadable.clone(),
            "original",
        ),
        (
            "the identifier feature with neither value member",
            Some(feature_without_value(IDENTIFIER_FEATURE)),
            400,
            unreadable,
            "original",
        ),
    ];

    for (case, posted, expected_status, expected_message, expected_a) in rows {
        let router = seeded().await;
        let mut features = vec![feature("feature_a", "updated")];
        if let Some(posted) = posted {
            features.insert(0, posted);
        }
        let answered = update(&router, "R1", json!(features)).await;
        let text = message(&answered.body);
        assert_eq!(
            answered.status, expected_status,
            "case {case:?}: expected HTTP {expected_status}, got {} with body {}",
            answered.status, answered.body
        );
        if expected_status == 400 {
            let code = answered.error_code();
            assert_eq!(
                (code.as_str(), text.as_str()),
                ("ValidationError", expected_message.as_str()),
                "case {case:?}: expected the modelled ValidationError stating the rule, got \
                 {code:?} with message {text:?}"
            );
        }

        let read_back = get(&router).await;
        let feature_a = served_value(&read_back.body, "feature_a");
        assert_eq!(
            feature_a,
            json!(expected_a),
            "case {case:?}: expected feature_a {expected_a:?} afterwards, got {feature_a:?}"
        );
    }
}
