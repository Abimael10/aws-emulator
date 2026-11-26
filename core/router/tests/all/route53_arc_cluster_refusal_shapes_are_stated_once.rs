// SPDX-License-Identifier: CC0-1.0

//! Route 53 ARC's cluster (data-plane) API states each of its refusals ONCE, whichever
//! operation answers it.
//!
//! The rule under test: **a refusal more than one operation of this API can answer is ONE
//! refusal — same modelled class, same status, same `Reason`, same named members and the
//! same words — so two sibling operations handed the same unanswerable request answer
//! identically.** Two of this API's refusals are reachable from more than one operation:
//!
//! * the safety-rule refusal, which both `UpdateRoutingControlState` and
//!   `UpdateRoutingControlStates` answer when a deployed rule of the control panel would be
//!   left unsatisfied;
//! * the routing-control not-found refusal, which `GetRoutingControlState` and both state
//!   updates answer for a `RoutingControlArn` the control plane minted nothing for.
//!
//! Why the suite lives here: every row needs BOTH planes of Route 53 ARC over one engine —
//! the recovery-control config plane mints the cluster, the control panel, the routing
//! controls and the safety rule that the cluster data plane then refuses a change to — so it
//! is the assembled pair that shows the rule, as `core/router/src/lib.rs` assembles it.
//!
//! The rows span what the rule generalises over: the operation answering the refusal (the
//! read and both updates), the state the request asked for, which routing control of the
//! panel it named, and the form of an ARN that names nothing.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// This test target's ONE percent-encoder for a URI path segment: an ARN sent as a
// non-greedy `@httpLabel` arrives with its `:` and `/` escaped, as an AWS SDK sends it.
use crate::cloudfront_key_value_store_handshake::segment as percent_encode;

use service_com_amazonaws_route53recoverycluster_togglecustomerapi::EmulatedService as ClusterDataPlane;
use service_com_amazonaws_route53recoverycontrolconfig_route53recoverycontrolconfig::EmulatedService as RecoveryControlConfig;

/// The Region these requests are signed for. Recovery-control resource ARNs are
/// account-wide, so it appears in none of them.
const REGION: &str = "us-west-2";

/// The signing name that routes a request to the recovery-control config plane.
const CONFIG_SIGNING_NAME: &str = "route53-recovery-control-config";

/// The signing name that routes a request to the cluster data plane.
const DATA_PLANE_SIGNING_NAME: &str = "route53-recovery-cluster";

/// The `X-Amz-Target` prefix the cluster data plane's model fixes (service shape
/// `ToggleCustomerAPI`, awsJson1.0).
const TARGET_PREFIX: &str = "ToggleCustomerAPI";

/// The HTTP status this API states these refusals in.
///
/// Spelled out here independently of the emulator's own constant, so a change to that
/// constant has to agree with this figure rather than redefine it.
const REFUSAL_STATUS: u16 = 400;

/// The modelled classes the two refusals are answered with.
const VALIDATION_EXCEPTION: &str = "ValidationException";
const RESOURCE_NOT_FOUND_EXCEPTION: &str = "ResourceNotFoundException";

/// The `ResourceNotFoundException$ResourceType` a routing control lookup reports: the KIND
/// of resource a `RoutingControlArn` addresses.
const ROUTING_CONTROL_RESOURCE_TYPE: &str = "RoutingControl";

/// How often a status that is still settling is looked at again, and how many times: the
/// product is a generous bound on a status that never settles, not a measurement of the
/// host.
const POLL_INTERVAL: Duration = Duration::from_millis(25);
const SETTLE_LOOKS: usize = 2_000;

/// One emulator with both planes of Route 53 ARC mounted over ONE engine, exactly as
/// `core/router/src/lib.rs` mounts them.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(ClusterDataPlane::new(Arc::clone(&engine)));
    router.register_service(RecoveryControlConfig::new(engine));
    router
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name`, which is how
/// every signed SDK request routes. The signature is never checked here.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host;x-amz-target, \
         Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// Send one request and read its status and JSON body, naming `case` when the body is not
/// JSON at all.
async fn dispatch(
    router: &MultiServiceRouter,
    request: http::Request<Bytes>,
    case: &str,
) -> (u16, Value) {
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let body = response.into_body();
    let parsed: Value = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&body).unwrap_or_else(|error| {
            panic!(
                "case {case}: the response body is not JSON ({error}): {:?}",
                String::from_utf8_lossy(&body)
            )
        })
    };
    (status, parsed)
}

/// Call one config-plane operation (a REST route) and return its status and body.
async fn config(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let payload = body.map_or_else(Bytes::new, |value| {
        Bytes::from(serde_json::to_vec(&value).expect("the request body serializes"))
    });
    let request = http::Request::builder()
        .method(method)
        .uri(format!(
            "https://{CONFIG_SIGNING_NAME}.{REGION}.amazonaws.com{path}"
        ))
        .header("content-type", "application/json")
        .header("authorization", authorization(CONFIG_SIGNING_NAME))
        .body(payload)
        .expect("a well-formed request");
    dispatch(router, request, path).await
}

/// Call one cluster data-plane operation and return its status and body.
async fn data_plane(
    router: &MultiServiceRouter,
    operation: &str,
    body: &Value,
    case: &str,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header(
            "host",
            format!("{DATA_PLANE_SIGNING_NAME}.{REGION}.amazonaws.com"),
        )
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", format!("{TARGET_PREFIX}.{operation}"))
        .header("authorization", authorization(DATA_PLANE_SIGNING_NAME))
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    dispatch(router, request, case).await
}

/// The `__type` of a failure body, without any `namespace#` prefix.
fn error_code(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .map(|value| value.rsplit('#').next().unwrap_or(value).to_string())
        .unwrap_or_default()
}

/// A string member of a body, looked up under the member name the model declares and under
/// its capitalised form, so the assertion is about the VALUE and not about which casing the
/// protocol renders.
fn string_member(body: &Value, name: &str) -> String {
    let capitalised: String = name
        .char_indices()
        .map(|(index, c)| {
            if index == 0 {
                c.to_ascii_uppercase()
            } else {
                c
            }
        })
        .collect();
    body.get(name)
        .or_else(|| body.get(&capitalised))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// An ARN of the same shape as `arn` that names no resource: its last character is replaced,
/// which keeps it a well-formed ARN of the same kind.
fn unknown_like(arn: &str) -> String {
    let last = arn.chars().next_back().unwrap_or('0');
    let mut shifted: String = arn.chars().take(arn.chars().count() - 1).collect();
    shifted.push(if last == 'f' { 'e' } else { 'f' });
    shifted
}

/// The fixture every row addresses: one cluster, one control panel on it, and `controls`
/// routing controls in that panel. Answers `(the panel's ARN, the controls' ARNs)`.
async fn fleet(router: &MultiServiceRouter, controls: usize) -> (String, Vec<String>) {
    let (status, cluster) = config(
        router,
        "POST",
        "/cluster",
        Some(json!({ "ClusterName": "one-wording" })),
    )
    .await;
    assert_eq!(
        status, 200,
        "case CreateCluster: expected 200, got {status} with {cluster}"
    );
    let cluster_arn = cluster["Cluster"]["ClusterArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case CreateCluster: a created cluster has an ARN, got {cluster}"))
        .to_string();

    let (status, panel) = config(
        router,
        "POST",
        "/controlpanel",
        Some(json!({ "ClusterArn": cluster_arn, "ControlPanelName": "one-wording-panel" })),
    )
    .await;
    assert_eq!(
        status, 200,
        "case CreateControlPanel: expected 200, got {status} with {panel}"
    );
    let panel_arn = panel["ControlPanel"]["ControlPanelArn"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case CreateControlPanel: a created panel has an ARN, got {panel}")
        })
        .to_string();

    let mut arns = Vec::with_capacity(controls);
    for index in 0..controls {
        let name = format!("one-wording-control-{index}");
        let (status, control) = config(
            router,
            "POST",
            "/routingcontrol",
            Some(json!({
                "ClusterArn": cluster_arn,
                "ControlPanelArn": panel_arn,
                "RoutingControlName": name,
            })),
        )
        .await;
        assert_eq!(
            status, 200,
            "case CreateRoutingControl({name}): expected 200, got {status} with {control}"
        );
        arns.push(
            control["RoutingControl"]["RoutingControlArn"]
                .as_str()
                .unwrap_or_else(|| {
                    panic!(
                        "case CreateRoutingControl({name}): a created control has an ARN, got \
                         {control}"
                    )
                })
                .to_string(),
        );
    }

    (panel_arn, arns)
}

/// Create one ASSERTION safety rule over `asserted` whose configuration NO transaction can
/// satisfy (at least one more control `On` than the rule evaluates), and answer its
/// `SafetyRuleArn` once the config plane reports it `DEPLOYED` — the point from which it
/// guards state updates.
async fn blocking_rule(
    router: &MultiServiceRouter,
    panel_arn: &str,
    asserted: &[String],
) -> String {
    let threshold = asserted.len() + 1;
    let (status, created) = config(
        router,
        "POST",
        "/safetyrule",
        Some(json!({
            "AssertionRule": {
                "AssertedControls": asserted,
                "ControlPanelArn": panel_arn,
                "Name": "one-wording-rule",
                "RuleConfig": { "Type": "ATLEAST", "Threshold": threshold, "Inverted": false },
                "WaitPeriodMs": 5000,
            }
        })),
    )
    .await;
    assert_eq!(
        status, 200,
        "case CreateSafetyRule: expected 200, got {status} with {created}"
    );
    let rule_arn = created["AssertionRule"]["SafetyRuleArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case CreateSafetyRule: a created rule has an ARN, got {created}"))
        .to_string();

    // The config plane deploys a created rule asynchronously, so the rule's status is WAITED
    // FOR: the bound is generous and makes a rule that never deploys fail the test rather
    // than timing the host.
    for _ in 0..SETTLE_LOOKS {
        let (status, described) = config(
            router,
            "GET",
            &format!("/safetyrule/{}", percent_encode(&rule_arn)),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "case DescribeSafetyRule: expected 200, got {status} with {described}"
        );
        if described["AssertionRule"]["Status"].as_str() == Some("DEPLOYED") {
            return rule_arn;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    panic!("case DescribeSafetyRule: the rule never reached Status DEPLOYED");
}

/// The message Route 53 ARC states when `safety_rule_arn` blocks a change to
/// `routing_control_arns`, with `condition` the rule's condition as the service words it.
///
/// Spelled out here independently of the emulator's own builder, so a change to that builder
/// has to agree with this sentence rather than redefine it.
fn safety_rule_message(
    condition: &str,
    routing_control_arns: &[&str],
    safety_rule_arn: &str,
) -> String {
    format!(
        "Can't update the routing control state because of a configured safety rule: \
         \"{condition}.\" To override the rule and change the state, see the guidance for \
         AllowSafetyRulesOverride in the Route 53 ARC Developer Guide. \nRouting control: \
         [{controls}] \nSafety rule: {safety_rule_arn}",
        controls = routing_control_arns.join(", "),
    )
}

/// The state `arn` reports, through `GetRoutingControlState`.
async fn reported_state(router: &MultiServiceRouter, arn: &str) -> String {
    let (status, body) = data_plane(
        router,
        "GetRoutingControlState",
        &json!({ "RoutingControlArn": arn }),
        arn,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {arn}: expected 200 from GetRoutingControlState, got {status} with {body}"
    );
    body.get("RoutingControlState")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// RULE: the safety-rule refusal is ONE refusal of this API — a single update and a batch
/// proposing the same change are answered with the same status, the same modelled class and
/// the same sentence — and it is the standing rule that refuses them, so overriding it lets
/// the identical request through.
#[tokio::test]
async fn both_state_updates_state_the_safety_rule_refusal_identically() {
    let router = emulator();
    let (panel_arn, controls) = fleet(&router, 2).await;
    let rule_arn = blocking_rule(&router, &panel_arn, &controls).await;
    // The rule asks for one more control `On` than it evaluates, so no transaction over its
    // controls satisfies it.
    let condition = format!("At least {} routing controls must be On", controls.len() + 1);

    // (case, the control the request names, the state it asks for)
    let rows = [
        ("the first control, switched on", 0, "On"),
        ("the first control, switched off", 0, "Off"),
        ("the second control, switched on", 1, "On"),
    ];

    for (case, index, state) in rows {
        let arn = controls[index].clone();
        let (single_status, single_body) = data_plane(
            &router,
            "UpdateRoutingControlState",
            &json!({ "RoutingControlArn": arn, "RoutingControlState": state }),
            case,
        )
        .await;
        let (batch_status, batch_body) = data_plane(
            &router,
            "UpdateRoutingControlStates",
            &json!({
                "UpdateRoutingControlStateEntries": [
                    { "RoutingControlArn": arn, "RoutingControlState": state },
                ],
            }),
            case,
        )
        .await;

        let expected = safety_rule_message(&condition, &[arn.as_str()], &rule_arn);
        for (operation, status, body) in [
            ("UpdateRoutingControlState", single_status, &single_body),
            ("UpdateRoutingControlStates", batch_status, &batch_body),
        ] {
            assert_eq!(
                (status, error_code(body).as_str()),
                (REFUSAL_STATUS, VALIDATION_EXCEPTION),
                "case {case:?} {operation}: expected {REFUSAL_STATUS} {VALIDATION_EXCEPTION}, \
                 got {status} with {body}"
            );
            let actual = string_member(body, "message");
            assert_eq!(
                actual, expected,
                "case {case:?} {operation}: expected message {expected:?}, got {actual:?}"
            );
        }
        assert_eq!(
            (single_status, &single_body),
            (batch_status, &batch_body),
            "case {case:?}: one refusal, so both updates answer the same status and body; got \
             {single_status} with {single_body} and {batch_status} with {batch_body}"
        );

        // Nothing moved: the refusal is the whole answer.
        let reported = reported_state(&router, &arn).await;
        assert_eq!(
            reported, "Off",
            "case {case:?}: a refused update must leave {arn} Off, got {reported:?}"
        );
    }

    // The positive control: the rows above are refused by the RULE, so the identical requests
    // naming it in `SafetyRulesToOverride` are served by both operations.
    let (status, body) = data_plane(
        &router,
        "UpdateRoutingControlState",
        &json!({
            "RoutingControlArn": controls[0],
            "RoutingControlState": "On",
            "SafetyRulesToOverride": [rule_arn],
        }),
        "the rule overridden, single update",
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"the rule overridden, single update\": expected 200, got {status} with {body}"
    );
    let (status, body) = data_plane(
        &router,
        "UpdateRoutingControlStates",
        &json!({
            "UpdateRoutingControlStateEntries": [
                { "RoutingControlArn": controls[1], "RoutingControlState": "On" },
            ],
            "SafetyRulesToOverride": [rule_arn],
        }),
        "the rule overridden, batch update",
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"the rule overridden, batch update\": expected 200, got {status} with {body}"
    );
    for arn in &controls {
        let reported = reported_state(&router, arn).await;
        assert_eq!(
            reported, "On",
            "case \"the rule overridden\": {arn} must report On, got {reported:?}"
        );
    }
}

/// RULE: the routing-control not-found refusal is ONE refusal of this API — the read and
/// both state updates answer a `RoutingControlArn` the control plane minted nothing for with
/// the same status, the same modelled class, the same ARN as the resource that was not found,
/// the same resource type and the same sentence.
#[tokio::test]
async fn the_read_and_both_updates_state_the_not_found_refusal_identically() {
    let router = emulator();
    let (panel_arn, controls) = fleet(&router, 1).await;
    let served = controls[0].clone();

    // The rows differ in the form of an ARN that names nothing: an unknown control in a
    // known panel, a control of an unknown panel, and an id of another form altogether.
    let rows = [
        ("an unknown control in a known panel", unknown_like(&served)),
        (
            "a control of a panel that was never created",
            format!(
                "{}/routingcontrol/aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
                unknown_like(&panel_arn)
            ),
        ),
        (
            "an id of another form",
            format!("{panel_arn}/routingcontrol/0"),
        ),
    ];

    for (case, arn) in &rows {
        let answers = [
            (
                "GetRoutingControlState",
                data_plane(
                    &router,
                    "GetRoutingControlState",
                    &json!({ "RoutingControlArn": arn }),
                    case,
                )
                .await,
            ),
            (
                "UpdateRoutingControlState",
                data_plane(
                    &router,
                    "UpdateRoutingControlState",
                    &json!({ "RoutingControlArn": arn, "RoutingControlState": "On" }),
                    case,
                )
                .await,
            ),
            (
                "UpdateRoutingControlStates",
                data_plane(
                    &router,
                    "UpdateRoutingControlStates",
                    &json!({
                        "UpdateRoutingControlStateEntries": [
                            { "RoutingControlArn": arn, "RoutingControlState": "On" },
                        ],
                    }),
                    case,
                )
                .await,
            ),
        ];

        let expected_message = format!("Routing Control '{arn}' not found");
        for (operation, (status, body)) in &answers {
            assert_eq!(
                (*status, error_code(body).as_str()),
                (REFUSAL_STATUS, RESOURCE_NOT_FOUND_EXCEPTION),
                "case {case:?} {operation}: expected {REFUSAL_STATUS} \
                 {RESOURCE_NOT_FOUND_EXCEPTION}, got {status} with {body}"
            );
            let actual = string_member(body, "message");
            assert_eq!(
                actual, expected_message,
                "case {case:?} {operation}: expected message {expected_message:?}, got \
                 {actual:?}"
            );
            let reported_id = string_member(body, "resourceId");
            assert_eq!(
                &reported_id, arn,
                "case {case:?} {operation}: the refusal reports the ARN the request carried, \
                 got {reported_id:?} in {body}"
            );
            let reported_type = string_member(body, "resourceType");
            assert_eq!(
                reported_type, ROUTING_CONTROL_RESOURCE_TYPE,
                "case {case:?} {operation}: expected resource type \
                 {ROUTING_CONTROL_RESOURCE_TYPE:?}, got {reported_type:?} in {body}"
            );
        }

        let (first_operation, first_answer) = &answers[0];
        for (operation, answer) in &answers[1..] {
            assert_eq!(
                answer, first_answer,
                "case {case:?}: one refusal, so {operation} answers exactly what \
                 {first_operation} answers; got {answer:?} against {first_answer:?}"
            );
        }
    }

    // The positive control: the control that WAS minted is still served, so the rows above
    // refuse their own ARNs and not the operations.
    let reported = reported_state(&router, &served).await;
    assert_eq!(
        reported, "Off",
        "case \"the minted control\": expected Off, got {reported:?}"
    );
}
