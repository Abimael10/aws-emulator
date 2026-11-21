// SPDX-License-Identifier: CC0-1.0

//! What a shadow's reserved `…/update/delta` topic carries across a SEQUENCE of accepted
//! updates: the whole desired state the device has not reported, recomputed at each
//! update rather than accumulated from the keys each request happened to write.
//!
//! The rule under test: on every accepted shadow update the service announces
//! `…/update/accepted` (the response document) and `…/update/documents` (the document
//! before and after the update), and announces `…/update/delta` only when the WHOLE merged
//! desired state differs from the WHOLE merged reported state at that moment — the delta
//! holding exactly those differing leaves, whichever request wrote them — with the
//! shadow's own version advancing by one per accepted update and carried on each message.
//!
//! The rows differ along what the rule generalises over: how the desired leaves arrive
//! (all at once or one update at a time), the shape of the state (scalars, a nested
//! object), whether the device converges fully or one leaf at a time, a reported leaf
//! nothing desires, and a desired write of the value already reported. The two reserved
//! namespaces (the unnamed "classic" shadow and a named one) are both exercised.
//!
//! The ABSENCE of a delta is attributed structurally, not by waiting: the following
//! update's own announcements are read off the same connection, so a delta published
//! where none was owed would arrive ahead of them and fail the ordering assertion. Every
//! row therefore ends on an update that does owe a delta.
//!
//! It needs the WHOLE router: the broker's address is published by the IoT control plane,
//! the updates go through the IoT data plane's `UpdateThingShadow`, and the broker that
//! carries the announcements is in neither. The session is spoken over a real socket with
//! the same maintained MQTT codec a client library uses — see
//! `iot_shadow_reserved_topic_push`, whose device harness this suite shares.

use serde_json::{json, Map, Value};
use service_com_amazonaws_iotdataplane_iotmoonrakerservice::state::iot_mqtt_broker::mqttbytes::{
    v4, QoS,
};

use crate::iot_shadow_reserved_topic_push::{
    discover, member, on_the_emulator, reserved_prefix, update_shadow, Device,
};

/// One accepted update of one shadow, and everything its announcements must say.
struct Step {
    /// The `state` node the update writes.
    write: Value,
    /// The merged `state` the document must stand at afterwards, as
    /// `…/update/documents` reports it for `current` — and as the NEXT step reports it
    /// for `previous`.
    document: Value,
    /// The `state` the delta must carry, or `None` when everything desired is reported
    /// and no delta is owed at all.
    delta: Option<Value>,
}

/// One shadow, walked through a sequence of updates.
struct Row {
    case: &'static str,
    thing_name: &'static str,
    /// The shadow addressed: `None` for the thing's unnamed ("classic") shadow.
    shadow_name: Option<&'static str>,
    client_id: &'static str,
    steps: Vec<Step>,
}

/// The rows. A shadow document is a JSON value, so they are built rather than declared.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "desired leaves written one update at a time",
            thing_name: "thermostat-9",
            shadow_name: None,
            client_id: "shadow-delta-probe-one-at-a-time",
            steps: vec![
                Step {
                    write: json!({"desired": {"alpha": 1}}),
                    document: json!({"desired": {"alpha": 1}}),
                    delta: Some(json!({"alpha": 1})),
                },
                // The second desired-only write: the delta holds the leaf this update
                // wrote AND the one the first update left outstanding.
                Step {
                    write: json!({"desired": {"beta": 2}}),
                    document: json!({"desired": {"alpha": 1, "beta": 2}}),
                    delta: Some(json!({"alpha": 1, "beta": 2})),
                },
                // The device reports both: nothing is outstanding, so no delta is owed.
                Step {
                    write: json!({"reported": {"alpha": 1, "beta": 2}}),
                    document: json!({
                        "desired": {"alpha": 1, "beta": 2},
                        "reported": {"alpha": 1, "beta": 2},
                    }),
                    delta: None,
                },
                // One leaf diverges again: the recomputed delta holds that leaf alone.
                Step {
                    write: json!({"desired": {"alpha": 9}}),
                    document: json!({
                        "desired": {"alpha": 9, "beta": 2},
                        "reported": {"alpha": 1, "beta": 2},
                    }),
                    delta: Some(json!({"alpha": 9})),
                },
            ],
        },
        Row {
            case: "a nested object the device converges one leaf at a time",
            thing_name: "valve-11",
            shadow_name: Some("firmware"),
            client_id: "shadow-delta-probe-nested",
            steps: vec![
                Step {
                    write: json!({"desired": {"cfg": {"a": 1, "b": 2}}}),
                    document: json!({"desired": {"cfg": {"a": 1, "b": 2}}}),
                    delta: Some(json!({"cfg": {"a": 1, "b": 2}})),
                },
                // Half of the nested object reported: the delta keeps the other half
                // only, which an accumulating implementation would re-push whole.
                Step {
                    write: json!({"reported": {"cfg": {"a": 1}}}),
                    document: json!({
                        "desired": {"cfg": {"a": 1, "b": 2}},
                        "reported": {"cfg": {"a": 1}},
                    }),
                    delta: Some(json!({"cfg": {"b": 2}})),
                },
                Step {
                    write: json!({"reported": {"cfg": {"b": 2}}}),
                    document: json!({
                        "desired": {"cfg": {"a": 1, "b": 2}},
                        "reported": {"cfg": {"a": 1, "b": 2}},
                    }),
                    delta: None,
                },
                Step {
                    write: json!({"desired": {"cfg": {"b": 5}}}),
                    document: json!({
                        "desired": {"cfg": {"a": 1, "b": 5}},
                        "reported": {"cfg": {"a": 1, "b": 2}},
                    }),
                    delta: Some(json!({"cfg": {"b": 5}})),
                },
            ],
        },
        Row {
            case: "a reported leaf nothing desires, beside an outstanding desired leaf",
            thing_name: "pump-3",
            shadow_name: None,
            client_id: "shadow-delta-probe-reported-only",
            steps: vec![
                Step {
                    write: json!({"desired": {"mode": "eco"}}),
                    document: json!({"desired": {"mode": "eco"}}),
                    delta: Some(json!({"mode": "eco"})),
                },
                // A report of something nothing desires leaves the delta as it was: the
                // delta is the desired state minus what is reported, not a diff of the
                // two documents.
                Step {
                    write: json!({"reported": {"uptime": 41}}),
                    document: json!({
                        "desired": {"mode": "eco"},
                        "reported": {"uptime": 41},
                    }),
                    delta: Some(json!({"mode": "eco"})),
                },
                Step {
                    write: json!({"reported": {"mode": "eco"}}),
                    document: json!({
                        "desired": {"mode": "eco"},
                        "reported": {"mode": "eco", "uptime": 41},
                    }),
                    delta: None,
                },
                Step {
                    write: json!({"desired": {"mode": "boost", "fan": true}}),
                    document: json!({
                        "desired": {"fan": true, "mode": "boost"},
                        "reported": {"mode": "eco", "uptime": 41},
                    }),
                    delta: Some(json!({"fan": true, "mode": "boost"})),
                },
            ],
        },
        Row {
            case: "a desired write of the value already reported",
            thing_name: "meter-5",
            shadow_name: Some("telemetry"),
            client_id: "shadow-delta-probe-rewritten",
            steps: vec![
                Step {
                    write: json!({"desired": {"rate": 10}}),
                    document: json!({"desired": {"rate": 10}}),
                    delta: Some(json!({"rate": 10})),
                },
                Step {
                    write: json!({"reported": {"rate": 10}}),
                    document: json!({"desired": {"rate": 10}, "reported": {"rate": 10}}),
                    delta: None,
                },
                // Writing the desired value the device already reports changes nothing
                // about what is outstanding, so it is announced without a delta.
                Step {
                    write: json!({"desired": {"rate": 10}}),
                    document: json!({"desired": {"rate": 10}, "reported": {"rate": 10}}),
                    delta: None,
                },
                Step {
                    write: json!({"desired": {"rate": 11}}),
                    document: json!({"desired": {"rate": 11}, "reported": {"rate": 10}}),
                    delta: Some(json!({"rate": 11})),
                },
            ],
        },
    ]
}

/// The RESPONSE document's `state` with its derived `delta` node taken out — the merged
/// `desired` and `reported` the update left behind, which is what a step's expected
/// document states. The delta it carries is asserted on its own.
///
/// Only the response document (`…/update/accepted`) carries a `delta` node at all: the
/// `…/update/documents` snapshots are the document before and after the update, so their
/// `state` is compared EXACTLY and a delta node appearing there fails the comparison.
fn state_without_delta(state: &Value) -> Value {
    let mut sections: Map<String, Value> = match state {
        Value::Object(state) => state.clone(),
        _ => Map::new(),
    };
    sections.remove("delta");
    Value::Object(sections)
}

/// Every accepted update of a shadow is announced on its reserved topics, and the delta
/// it is pushed is the whole desired state the device has not reported — recomputed at
/// each update, so a leaf stays in it until it is reported and drops out when it is.
#[test]
fn the_pushed_delta_is_the_whole_unreported_desired_state_at_each_update() {
    on_the_emulator("iot-shadow-delta-recomputed", |router, runtime| {
        runtime.block_on(async {
            let address = discover(&router).await;
            for row in rows() {
                let case = row.case;
                let prefix = reserved_prefix(row.thing_name, row.shadow_name);
                let accepted_topic = format!("{prefix}/update/accepted");
                let documents_topic = format!("{prefix}/update/documents");
                let delta_topic = format!("{prefix}/update/delta");

                let mut device = Device::connect(case, &address, row.client_id).await;
                let filters = vec![
                    delta_topic.clone(),
                    accepted_topic.clone(),
                    documents_topic.clone(),
                ];
                let granted = device.subscribe(&filters).await;
                assert_eq!(
                    granted,
                    vec![v4::SubscribeReasonCode::Success(QoS::AtLeastOnce); filters.len()],
                    "case {case:?}: expected QoS 1 granted for every reserved filter, got \
                     {granted:?}"
                );

                for (index, step) in row.steps.iter().enumerate() {
                    // The version a shadow nothing has written stands at is 0, so the
                    // version an update consumes is its position in the sequence.
                    let version = i64::try_from(index).expect("a step index is small") + 1;
                    let written = &step.write;
                    update_shadow(case, &router, row.thing_name, row.shadow_name, written).await;

                    let wanted = 2 + usize::from(step.delta.is_some());
                    let announced = device.read_announcements(wanted).await;
                    let topics: Vec<&str> = announced
                        .iter()
                        .map(|(topic, _body)| topic.as_str())
                        .collect();
                    let mut expected_topics =
                        vec![accepted_topic.as_str(), documents_topic.as_str()];
                    if step.delta.is_some() {
                        expected_topics.push(delta_topic.as_str());
                    }
                    assert_eq!(
                        topics, expected_topics,
                        "case {case:?}: update {version} of {written} must announce \
                         {expected_topics:?}, got {topics:?}"
                    );

                    // `…/update/accepted` carries the response document: the merged state,
                    // its derived delta, and the version the update consumed.
                    let (_topic, accepted) = &announced[0];
                    let state = member(case, &accepted_topic, accepted, &["state"]);
                    assert_eq!(
                        state_without_delta(state),
                        step.document,
                        "case {case:?}: update {version} must answer the merged state \
                         {}, got {accepted}",
                        step.document
                    );
                    assert_eq!(
                        state.get("delta"),
                        step.delta.as_ref(),
                        "case {case:?}: update {version} must derive the delta {:?}, got \
                         {accepted}",
                        step.delta
                    );
                    assert_eq!(
                        member(case, &accepted_topic, accepted, &["version"]),
                        &json!(version),
                        "case {case:?}: update {version} must be announced at that version, \
                         got {accepted}"
                    );

                    // `…/update/documents` carries the pair: the document this update
                    // left, and the one it replaced.
                    let (_topic, documents) = &announced[1];
                    let current = member(case, &documents_topic, documents, &["current"]);
                    assert_eq!(
                        member(case, &documents_topic, current, &["state"]),
                        &step.document,
                        "case {case:?}: update {version} must report {} as the current \
                         document, got {documents}",
                        step.document
                    );
                    assert_eq!(
                        member(case, &documents_topic, current, &["version"]),
                        &json!(version),
                        "case {case:?}: the current document must stand at version \
                         {version}, got {documents}"
                    );
                    let previous = member(case, &documents_topic, documents, &["previous"]);
                    match index.checked_sub(1).map(|earlier| &row.steps[earlier]) {
                        // Nothing stood where a created shadow is, which is what a null
                        // previous document says.
                        None => assert_eq!(
                            previous,
                            &Value::Null,
                            "case {case:?}: a created shadow has no previous document, got \
                             {documents}"
                        ),
                        Some(earlier) => {
                            assert_eq!(
                                member(case, &documents_topic, previous, &["state"]),
                                &earlier.document,
                                "case {case:?}: update {version} must report {} as the \
                                 previous document, got {documents}",
                                earlier.document
                            );
                            assert_eq!(
                                member(case, &documents_topic, previous, &["version"]),
                                &json!(version - 1),
                                "case {case:?}: the previous document must stand at the \
                                 version before {version}, got {documents}"
                            );
                        }
                    }

                    // `…/update/delta`, when one is owed: the leaves the device has not
                    // reported, whichever update wrote them.
                    if let Some(expected) = &step.delta {
                        let (_topic, delta) = &announced[2];
                        assert_eq!(
                            member(case, &delta_topic, delta, &["state"]),
                            expected,
                            "case {case:?}: update {version} must push the delta \
                             {expected}, got {delta}"
                        );
                        assert_eq!(
                            member(case, &delta_topic, delta, &["version"]),
                            &json!(version),
                            "case {case:?}: the delta must carry the version it was \
                             recomputed at ({version}), got {delta}"
                        );
                    }
                }
            }
        });
    });
}
