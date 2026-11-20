// SPDX-License-Identifier: CC0-1.0

//! `TagResource` (`POST /tags/{resourceArn}`) is served by its own handler, off this
//! service's state, for EVERY request the modelled route can carry — never by the
//! emulator's unimplemented-operation channel.
//!
//! What the route answers each class of request with is pinned by the suites that own
//! those classes: the served write and its merge rule by [`crate::resource_tags`], and the
//! two refusal classes of an ARN the route cannot serve by
//! [`crate::tagging_resource_arn_faults`]. This suite asserts the one property that holds
//! across all of them and belongs to none: the answer came from the operation. A
//! scaffolded operation answers HTTP 501 with the error code `NotImplemented` whatever it
//! is handed, so the rows below are the ways the route can be addressed — the resource
//! types it addresses and holds, a resource type it does not address, ARNs of addressed
//! types that name no held resource, a value that is not an ARN, and a write the service
//! refuses on its size — and every one of them must be answered by the handler instead:
//! served off state, or refused on the client's side of the contract, with a refused write
//! leaving the resource's tag set exactly as it was.
//!
//! The exact status of a served write and the exact class of each refusal are deliberately
//! NOT re-asserted here, so this suite keeps saying what it is about when those values
//! change in the suites that own them.

use serde_json::{json, Map, Value};

use crate::replication_configuration_template_lifecycle::{drs_with_engine, REGION};
use crate::resource_tags::{
    launch_template_with_tags, list_tags, plan_with_tags, reported_tags, tag_resource,
    template_with_tags, ACCOUNT, TAGS_PER_RESOURCE_LIMIT,
};

/// The status and the error code the emulator answers an operation it does not implement
/// with (`crate::scaffolded`) — the answer no request to this route may get.
const UNIMPLEMENTED_STATUS: u16 = 501;
const UNIMPLEMENTED_CODE: &str = "NotImplemented";

/// A tag map holding one more key than a resource may carry, so the write that sends it is
/// past the documented maximum however empty the resource's set is.
fn map_past_the_limit() -> Value {
    let mut map = Map::new();
    for index in 0..=TAGS_PER_RESOURCE_LIMIT {
        map.insert(format!("k{index}"), Value::String(index.to_string()));
    }
    Value::Object(map)
}

/// Every request the modelled route can carry is answered by the operation itself — served
/// off state, or refused on the client's side of the contract — and never as an operation
/// this emulator does not implement.
#[tokio::test]
async fn no_request_to_the_tagging_route_is_answered_as_unimplemented() {
    let (router, _engine) = drs_with_engine();

    // The resource the one served row writes to, created with no tags at all: what it
    // carries at the end is what that write put there and nothing else.
    let plan = plan_with_tags(&router, REGION, Some(&json!({}))).await;
    let plan_id = plan
        .rsplit_once('/')
        .expect("a recovery-plan ARN")
        .1
        .to_string();
    let template = template_with_tags(&router, &json!({})).await;
    let launch_template = launch_template_with_tags(&router, &json!({"env": "prod"})).await;

    let written_on_the_plan = json!({"env": "prod"});
    let rows: Vec<(&str, String, Value, bool)> = vec![
        (
            "a held recovery plan",
            plan.clone(),
            written_on_the_plan.clone(),
            true,
        ),
        (
            "a held replication configuration template",
            template,
            json!({"env": "prod", "team": "dr"}),
            true,
        ),
        (
            "a launch configuration template, a resource type the route does not address",
            launch_template,
            json!({"env": "staging"}),
            false,
        ),
        (
            "a recovery plan id nothing minted",
            format!("arn:aws:drs:{REGION}:{ACCOUNT}:recovery-plan/plan-00000000000000000"),
            json!({"env": "staging"}),
            false,
        ),
        (
            "a source server nothing registered",
            format!("arn:aws:drs:{REGION}:{ACCOUNT}:source-server/s-00000000000000009"),
            json!({"env": "staging"}),
            false,
        ),
        (
            "the held plan's id under another account",
            format!("arn:aws:drs:{REGION}:123456789012:recovery-plan/{plan_id}"),
            json!({"env": "staging"}),
            false,
        ),
        (
            "a value that is not laid out as an ARN at all",
            "arn:not-an-arn-but-long-enough-to-pass-the-boundary".to_string(),
            json!({"env": "staging"}),
            false,
        ),
        (
            "a map holding one key more than a resource may carry",
            plan.clone(),
            map_past_the_limit(),
            false,
        ),
    ];

    for (case, arn, tags, served) in &rows {
        let answer = tag_resource(&router, REGION, arn, tags).await;
        assert_ne!(
            answer.status, UNIMPLEMENTED_STATUS,
            "case {case:?}: the route must be answered by its handler, got {answer:?}"
        );
        assert_ne!(
            answer.error_code.as_deref(),
            Some(UNIMPLEMENTED_CODE),
            "case {case:?}: the route must be answered by its handler, got {answer:?}"
        );
        if *served {
            assert!(
                (200..300).contains(&answer.status),
                "case {case:?}: expected the write to be served, got {answer:?}"
            );
        } else {
            assert!(
                (400..500).contains(&answer.status),
                "case {case:?}: expected the write to be refused on the client's side of \
                 the contract, got {answer:?}"
            );
        }
    }

    // Every refused row left the resource it addressed alone: the plan carries exactly
    // what the one served write of this sweep put there, so neither the write past the
    // maximum nor any refused ARN wrote anything.
    let read = list_tags(&router, REGION, &plan).await;
    assert_eq!(
        reported_tags(&read),
        Some(&written_on_the_plan),
        "expected the plan to carry {written_on_the_plan} after every refusal, got {read:?}"
    );
}
