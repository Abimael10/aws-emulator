// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the WORDING of the refusal a batch update over the service quota
//! is answered with.
//!
//! THE RULE UNDER TEST: the entry list carries no modelled `@length` bound
//! (`smithy_query(query="shape", shape="UpdateRoutingControlStateEntries", model_path="services/service-com-amazonaws-route53recoverycluster-togglecustomerapi/smithy-model/com-amazonaws-route53recoverycluster-togglecustomerapi.json")`),
//! so the cap of ten routing controls per call is a runtime service QUOTA, answered with
//! `ServiceLimitExceededException` (HTTP 402) and worded as the quota itself: the limit is
//! the only value in the sentence, the number of entries the request submitted does not
//! appear, and no control changes state.
//!
//! The rows differ along the submitted entry count — the thing the message must NOT vary
//! with — from one over the cap to twice it, and a batch at the cap is the positive
//! control.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::common::{
    call, create_cluster, create_control_panel, create_routing_control, emulator, error_code,
    error_message, get_state, Control,
};

/// The cap this API applies to one `UpdateRoutingControlStates` call: "Routing controls per
/// UpdateRoutingControlStates request" in Route 53 ARC's quotas.
const BATCH_QUOTA: usize = 10;

/// How AWS words a batch over that quota. The WHOLE string is fixed text apart from the
/// limit itself, so it is the same sentence for every batch over the cap.
///
/// Spelled out here independently of the emulator's own builder, so a change to that
/// builder has to agree with this sentence rather than redefine it.
fn expected_message() -> String {
    format!("Cannot update more than {BATCH_QUOTA} controls in a single batch")
}

/// One control panel holding `count` routing controls, all `Off`.
async fn panel_with_controls(count: usize) -> (MultiServiceRouter, Vec<Control>) {
    let router = emulator();
    let cluster = create_cluster(&router, "batch-quota").await;
    let mut panel = create_control_panel(&router, &cluster, "batch-quota-panel").await;
    let mut controls = Vec::new();
    for index in 0..count {
        let control = create_routing_control(&router, &panel, &format!("control-{index}")).await;
        controls.push(control.clone());
        panel.controls.push(control);
    }
    (router, controls)
}

/// The batch request that sets every ARN of `arns` to `On`.
fn batch(arns: &[String]) -> Value {
    json!({
        "UpdateRoutingControlStateEntries": arns
            .iter()
            .map(|arn| json!({ "RoutingControlArn": arn, "RoutingControlState": "On" }))
            .collect::<Vec<Value>>(),
    })
}

/// RULE: a batch carrying more than the quota's worth of entries is refused with HTTP 402
/// `ServiceLimitExceededException` whose message states the quota and nothing of the
/// request — the same sentence whatever the submitted count — and no routing control moves.
#[tokio::test]
async fn a_batch_over_the_quota_is_refused_with_the_quota_sentence() {
    let (router, controls) = panel_with_controls(BATCH_QUOTA + 1).await;
    let real: Vec<String> = controls.iter().map(|control| control.arn.clone()).collect();

    // Rows differ only in HOW FAR over the cap the batch is. Past the real controls the
    // extra entries are well-formed ARNs of the same shape that name nothing — the quota is
    // a request-shape rule, so it is answered before any ARN is resolved, which is itself
    // part of the rule.
    let rows: Vec<(&str, usize)> = vec![
        ("one entry over the cap", BATCH_QUOTA + 1),
        ("two entries over the cap", BATCH_QUOTA + 2),
        ("twice the cap", BATCH_QUOTA * 2),
    ];

    for (case, count) in &rows {
        let mut arns = real.clone();
        for extra in real.len()..*count {
            arns.push(format!("{}-{extra}", real[0]));
        }
        assert_eq!(
            arns.len(),
            *count,
            "case {case:?}: the fixture must submit {count} entries, got {}",
            arns.len()
        );

        let (status, body) = call(&router, "UpdateRoutingControlStates", &batch(&arns)).await;
        assert_eq!(
            status, 402,
            "case {case:?}: expected 402, got {status} with {body}"
        );
        assert_eq!(
            error_code(&body),
            "ServiceLimitExceededException",
            "case {case:?}: expected ServiceLimitExceededException, got {body}"
        );
        let actual = error_message(&body);
        assert_eq!(
            actual,
            expected_message(),
            "case {case:?}: expected message {:?}, got {actual:?}",
            expected_message()
        );

        // None of the submitted controls change state.
        for arn in &real {
            let reported = get_state(&router, arn).await;
            assert_eq!(
                reported.get("RoutingControlState").and_then(Value::as_str),
                Some("Off"),
                "case {case:?}: a refused batch must not move {arn}, got {reported}"
            );
        }
    }
}

/// RULE: the cap itself is served — a batch of exactly the quota's worth of entries moves
/// every switch it names, so the refusal above is about the bound and not about batching.
#[tokio::test]
async fn a_batch_at_the_quota_is_served() {
    let (router, controls) = panel_with_controls(BATCH_QUOTA + 1).await;
    let at_the_cap: Vec<String> = controls
        .iter()
        .take(BATCH_QUOTA)
        .map(|control| control.arn.clone())
        .collect();

    let (status, body) = call(&router, "UpdateRoutingControlStates", &batch(&at_the_cap)).await;
    assert_eq!(
        status, 200,
        "case \"a batch at the cap\": expected 200, got {status} with {body}"
    );
    for arn in &at_the_cap {
        let reported = get_state(&router, arn).await;
        assert_eq!(
            reported.get("RoutingControlState").and_then(Value::as_str),
            Some("On"),
            "case \"a batch at the cap\": expected {arn} to have moved, got {reported}"
        );
    }
    // The control the batch left out is untouched.
    let untouched = &controls[BATCH_QUOTA].arn;
    let reported = get_state(&router, untouched).await;
    assert_eq!(
        reported.get("RoutingControlState").and_then(Value::as_str),
        Some("Off"),
        "case \"a batch at the cap\": expected {untouched} to be untouched, got {reported}"
    );
}
