// SPDX-License-Identifier: CC0-1.0

//! The RESERVED shadow-name namespace, end to end over HTTP through the router this
//! service mounts in production.
//!
//! The rule these cases assert: the `$` namespace is the SERVICE's, not an unaddressable
//! one. A name the service reserves for a feature it SERVES is addressed through the
//! ordinary shadow operations — written, read, listed and deleted like any other named
//! shadow, and isolated from the classic shadow and from the thing's other named shadows
//! — while every other name in that namespace is refused `InvalidRequestException` (400)
//! with the service's fixed "Invalid shadow name : <name>" sentence on every path that
//! addresses a shadow. The served name here is `$package`, the named shadow AWS IoT
//! reserves for a thing's software package version reporting, which the Software Package
//! Catalog's `versionUpdateByJobsConfig` names as the shadow a job's version reporting
//! targets.
//!
//! The table spans what the rule generalises over: the operation that addresses the
//! shadow, and the form of the name — the served reserved name, names that merely look
//! like it (a different case, a longer name sharing its prefix, a plural), and reserved
//! names of other shapes. A reserved name that holds no document is a not-found and not
//! a name refusal, which is what keeps the two conditions distinguishable.

use serde_json::{json, Value};

use crate::shadow_and_retained_message::{Answer, Harness};

/// The reserved shadow name this service serves, as `state::SERVED_RESERVED_SHADOW_NAMES`
/// holds it.
const SERVED: &str = "$package";

/// Names in the reserved namespace the service serves nothing under — including the ones
/// that only LOOK like the served name, since the reserved name is exact. Every one is
/// inside the modelled `shadowName` bounds (length 1..64, pattern `^[$a-zA-Z0-9:_-]+$`,
/// `smithy_query(query="shape",shape="ShadowName")`), so each reaches the handler and the
/// namespace rule is what refuses it.
const UNSERVED_RESERVED_NAMES: &[&str] = &[
    "$packages",
    "$Package",
    "$PACKAGE",
    "$package-2",
    "$package:1",
    "$aws",
    "$aws-fleet-telemetry",
    "$",
];

/// A shadow document a device reports.
fn reported(state: Value) -> Vec<u8> {
    serde_json::to_vec(&json!({ "state": { "reported": state } })).expect("a JSON document")
}

/// The `state.reported` the answer reports, or `Value::Null` when it reports none.
fn reported_state(answer: &Answer) -> Value {
    answer
        .json()
        .get("state")
        .and_then(|state| state.get("reported"))
        .cloned()
        .unwrap_or(Value::Null)
}

/// The served reserved name is addressed like any other named shadow: a write is
/// accepted, the read reports what was written, the listing names it, and a delete
/// removes it.
#[tokio::test]
async fn the_served_reserved_shadow_name_round_trips_like_any_named_shadow() {
    let harness = Harness::new();
    let thing = "reporting-thing";

    let written = harness
        .update(
            thing,
            Some(SERVED),
            reported(json!({ "samplePackage": { "version": "1.0.0" } })),
        )
        .await;
    assert_eq!(
        written.status,
        200,
        "case {SERVED:?}: a write of the served reserved shadow must be accepted, got {} {}",
        written.status,
        String::from_utf8_lossy(&written.body)
    );

    let read = harness.get_shadow(thing, Some(SERVED)).await;
    assert_eq!(
        read.status,
        200,
        "case {SERVED:?}: the read must answer 200, got {} {}",
        read.status,
        String::from_utf8_lossy(&read.body)
    );
    assert_eq!(
        reported_state(&read),
        json!({ "samplePackage": { "version": "1.0.0" } }),
        "case {SERVED:?}: the read must report what was written, got {}",
        String::from_utf8_lossy(&read.body)
    );

    let listed = harness.list_named_shadows(thing, "").await;
    assert_eq!(
        listed.status,
        200,
        "case {SERVED:?}: the listing must answer 200, got {} {}",
        listed.status,
        String::from_utf8_lossy(&listed.body)
    );
    assert_eq!(
        listed.json().get("results").cloned(),
        Some(json!([SERVED])),
        "case {SERVED:?}: the listing must name the served reserved shadow, got {}",
        String::from_utf8_lossy(&listed.body)
    );

    let deleted = harness.delete_shadow(thing, Some(SERVED)).await;
    assert_eq!(
        deleted.status,
        200,
        "case {SERVED:?}: the delete must answer 200, got {} {}",
        deleted.status,
        String::from_utf8_lossy(&deleted.body)
    );
    let gone = harness.get_shadow(thing, Some(SERVED)).await;
    assert_eq!(
        gone.status,
        404,
        "case {SERVED:?}: a deleted shadow must be a not-found, got {} {}",
        gone.status,
        String::from_utf8_lossy(&gone.body)
    );
}

/// The served reserved shadow is its own document: it neither reads nor is read by the
/// classic shadow or another named shadow of the same thing.
#[tokio::test]
async fn the_served_reserved_shadow_is_isolated_from_the_thing_s_other_shadows() {
    let harness = Harness::new();
    let thing = "isolated-thing";

    // (case, the shadow written, the document written)
    let writes = [
        ("the classic shadow", None, json!({ "classic": true })),
        (
            "an ordinary named shadow",
            Some("sensor"),
            json!({ "sensor": 1 }),
        ),
        (
            "the served reserved shadow",
            Some(SERVED),
            json!({ "samplePackage": { "version": "2.3.4" } }),
        ),
    ];
    for (label, shadow, document) in &writes {
        let written = harness
            .update(thing, *shadow, reported(document.clone()))
            .await;
        assert_eq!(
            written.status,
            200,
            "case {label:?}: the write must be accepted, got {} {}",
            written.status,
            String::from_utf8_lossy(&written.body)
        );
    }

    for (label, shadow, document) in &writes {
        let read = harness.get_shadow(thing, *shadow).await;
        assert_eq!(
            reported_state(&read),
            *document,
            "case {label:?}: each shadow keeps its own document, got {}",
            String::from_utf8_lossy(&read.body)
        );
    }
}

/// A thing that has had nothing reported for it holds no such shadow: the read is the
/// not-found, naming the shadow the request addressed, and NOT the name refusal — so a
/// client can tell "nothing reported yet" from "that name is not addressable".
#[tokio::test]
async fn the_served_reserved_shadow_of_a_thing_without_one_is_a_not_found() {
    let harness = Harness::new();

    for thing in ["never-reported", "a", "fleet:1_edge-9"] {
        let read = harness.get_shadow(thing, Some(SERVED)).await;
        assert_eq!(
            read.status,
            404,
            "case {thing:?}: expected 404, got {} {}",
            read.status,
            String::from_utf8_lossy(&read.body)
        );
        assert_eq!(
            read.error_code(),
            "ResourceNotFoundException",
            "case {thing:?}: expected ResourceNotFoundException, got {}",
            String::from_utf8_lossy(&read.body)
        );
        assert_eq!(
            read.json().get("message").and_then(Value::as_str),
            Some(format!("No shadow exists with name: '{SERVED}'").as_str()),
            "case {thing:?}: the not-found must name the shadow addressed, got {}",
            String::from_utf8_lossy(&read.body)
        );
    }
}

/// Every other name in the reserved namespace stays unaddressable, on every path that
/// addresses a shadow — including the names that only look like the served one.
#[tokio::test]
async fn a_reserved_name_the_service_serves_nothing_under_is_still_refused() {
    let harness = Harness::new();

    for name in UNSERVED_RESERVED_NAMES {
        let expected = format!("Invalid shadow name : {name}");
        for (operation, answer) in [
            (
                "UpdateThingShadow",
                harness
                    .update("reserved-thing", Some(name), reported(json!({ "a": 1 })))
                    .await,
            ),
            (
                "GetThingShadow",
                harness.get_shadow("reserved-thing", Some(name)).await,
            ),
            (
                "DeleteThingShadow",
                harness.delete_shadow("reserved-thing", Some(name)).await,
            ),
        ] {
            assert_eq!(
                answer.status,
                400,
                "case {name:?}/{operation}: expected 400, got {} {}",
                answer.status,
                String::from_utf8_lossy(&answer.body)
            );
            assert_eq!(
                answer.error_code(),
                "InvalidRequestException",
                "case {name:?}/{operation}: expected InvalidRequestException, got {}",
                String::from_utf8_lossy(&answer.body)
            );
            assert_eq!(
                answer.json().get("message").and_then(Value::as_str),
                Some(expected.as_str()),
                "case {name:?}/{operation}: expected message {expected:?}, got {}",
                String::from_utf8_lossy(&answer.body)
            );
        }
    }
}
