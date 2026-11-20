// SPDX-License-Identifier: CC0-1.0

//! How Route 53 ARC's cluster (data-plane) API states a refusal ON THE WIRE, over the
//! assembled emulator: the status line, the error class in the body's `__type`, and the
//! message.
//!
//! The rule under test: **on this `awsJson1_0` API the class of a failure travels in the
//! body's `__type`, and the status is the one the SERVICE answers, not the `@httpError` its
//! model documents for the shape.** Two refusal classes are what every ARN-bearing member of
//! the API can produce, and they must be told apart:
//!
//! * a value that is not the ARN of the resource the member documents is INVALID INPUT —
//!   `ValidationException` with ONE fixed sentence that echoes nothing of the request — and
//!   is decided BEFORE existence is looked at;
//! * a well-formed ARN of that resource which names nothing this endpoint serves is
//!   `ResourceNotFoundException`, worded from the ARN the request carried.
//!
//! Both are answered at HTTP [`REFUSAL_STATUS`], the client-error status this API states its
//! refusals in — the not-found class included, whose shape carries `@httpError` 404 in the
//! model. A class this API has not been observed to answer differently keeps the model's own
//! status, which the batch-quota row pins.
//!
//! Why the suite lives here: every row needs BOTH planes over one engine — the
//! recovery-control config plane mints the ARNs (a cluster, a control panel, a routing
//! control) that the cluster data plane then refuses or serves — so it is the assembled pair
//! that shows the rule, as `core/router/src/lib.rs` assembles it.
//!
//! The rows span what the rule generalises over: the operation (all four of the API's), the
//! member carrying the ARN (`RoutingControlArn` directly, inside a batch entry, and the
//! listing's `ControlPanelArn` filter), the form of the offending value (a bare word, an ARN
//! of another resource kind, a well-formed ARN of the right kind naming nothing), and a
//! refusal of another class altogether (the batch quota).

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

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

/// The HTTP status this API states a refusal in, whichever modelled class the body names.
///
/// Spelled out here independently of the emulator's own constant, so a change to that
/// constant has to agree with this figure rather than redefine it.
const REFUSAL_STATUS: u16 = 400;

/// The error class a value that is not the ARN of the documented resource is refused with,
/// and the whole sentence it carries — fixed text, with nothing of the request in it.
const VALIDATION_EXCEPTION: &str = "ValidationException";
const INVALID_RESOURCE_ARN: &str = "Missing or invalid resource arn";

/// The error class a well-formed ARN that names nothing is refused with.
const RESOURCE_NOT_FOUND_EXCEPTION: &str = "ResourceNotFoundException";

/// The error class a batch over the per-request quota is refused with, and the status its
/// shape carries in the model — the class this API has not been observed to restate.
const SERVICE_LIMIT_EXCEEDED_EXCEPTION: &str = "ServiceLimitExceededException";
const SERVICE_LIMIT_EXCEEDED_STATUS: u16 = 402;

/// The most routing controls one `UpdateRoutingControlStates` call may carry ("Routing
/// controls per UpdateRoutingControlStates request" in Route 53 ARC's quotas).
const BATCH_QUOTA: usize = 10;

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

/// The message of a failure body.
fn error_message(body: &Value) -> String {
    body.get("message")
        .or_else(|| body.get("Message"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The fleet every row addresses: one cluster, one control panel on it, one routing control
/// in that panel. Answers `(the panel's ARN, the routing control's ARN)`.
async fn fleet(router: &MultiServiceRouter) -> (String, String) {
    let (status, cluster) = config(
        router,
        "POST",
        "/cluster",
        Some(json!({ "ClusterName": "refusal-statuses" })),
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
        Some(json!({ "ClusterArn": cluster_arn, "ControlPanelName": "refusal-panel" })),
    )
    .await;
    assert_eq!(
        status, 200,
        "case CreateControlPanel: expected 200, got {status} with {panel}"
    );
    let panel_arn = panel["ControlPanel"]["ControlPanelArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case CreateControlPanel: a created panel has an ARN, got {panel}"))
        .to_string();

    let (status, control) = config(
        router,
        "POST",
        "/routingcontrol",
        Some(json!({
            "ClusterArn": cluster_arn,
            "ControlPanelArn": panel_arn,
            "RoutingControlName": "refusal-control",
        })),
    )
    .await;
    assert_eq!(
        status, 200,
        "case CreateRoutingControl: expected 200, got {status} with {control}"
    );
    let control_arn = control["RoutingControl"]["RoutingControlArn"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case CreateRoutingControl: a created control has an ARN, got {control}")
        })
        .to_string();

    (panel_arn, control_arn)
}

/// An ARN of the same shape as `arn` that names no resource: its last character is replaced,
/// which keeps it a well-formed ARN of the same kind.
fn unknown_like(arn: &str) -> String {
    let last = arn.chars().next_back().unwrap_or('0');
    let mut shifted: String = arn.chars().take(arn.chars().count() - 1).collect();
    shifted.push(if last == 'f' { 'e' } else { 'f' });
    shifted
}

/// RULE: every refusal of an ARN-bearing member is stated at this API's refusal status, with
/// the class in `__type` — the fixed invalid-ARN sentence for a value that is not the ARN of
/// the documented resource, and the not-found wording for a well-formed one that names
/// nothing.
#[tokio::test]
async fn the_cluster_data_plane_states_both_refusal_classes_at_its_own_status() {
    let router = emulator();
    let (panel_arn, control_arn) = fleet(&router).await;
    let unknown_control = unknown_like(&control_arn);
    let unknown_panel = unknown_like(&panel_arn);

    // (case, operation, the request, the class, the message)
    let rows: Vec<(&str, &str, Value, &str, String)> = vec![
        (
            "a routing control ARN naming nothing, read",
            "GetRoutingControlState",
            json!({ "RoutingControlArn": unknown_control }),
            RESOURCE_NOT_FOUND_EXCEPTION,
            format!("Routing Control '{unknown_control}' not found"),
        ),
        (
            "a routing control ARN naming nothing, updated",
            "UpdateRoutingControlState",
            json!({ "RoutingControlArn": unknown_control, "RoutingControlState": "On" }),
            RESOURCE_NOT_FOUND_EXCEPTION,
            format!("Routing Control '{unknown_control}' not found"),
        ),
        (
            "a routing control ARN naming nothing, in a batch entry",
            "UpdateRoutingControlStates",
            json!({
                "UpdateRoutingControlStateEntries": [
                    { "RoutingControlArn": unknown_control, "RoutingControlState": "On" },
                ],
            }),
            RESOURCE_NOT_FOUND_EXCEPTION,
            format!("Routing Control '{unknown_control}' not found"),
        ),
        (
            "a control panel filter naming nothing",
            "ListRoutingControls",
            json!({ "ControlPanelArn": unknown_panel }),
            RESOURCE_NOT_FOUND_EXCEPTION,
            format!("Control panel not found: '{unknown_panel}'"),
        ),
        (
            "a bare word where a routing control ARN belongs",
            "GetRoutingControlState",
            json!({ "RoutingControlArn": "not-an-arn" }),
            VALIDATION_EXCEPTION,
            INVALID_RESOURCE_ARN.to_string(),
        ),
        (
            "a control panel ARN where a routing control ARN belongs",
            "UpdateRoutingControlState",
            json!({ "RoutingControlArn": panel_arn, "RoutingControlState": "On" }),
            VALIDATION_EXCEPTION,
            INVALID_RESOURCE_ARN.to_string(),
        ),
        (
            "a bare word in a batch entry",
            "UpdateRoutingControlStates",
            json!({
                "UpdateRoutingControlStateEntries": [
                    { "RoutingControlArn": "not-an-arn", "RoutingControlState": "On" },
                ],
            }),
            VALIDATION_EXCEPTION,
            INVALID_RESOURCE_ARN.to_string(),
        ),
        (
            "a bare word where a control panel filter belongs",
            "ListRoutingControls",
            json!({ "ControlPanelArn": "not-an-arn" }),
            VALIDATION_EXCEPTION,
            INVALID_RESOURCE_ARN.to_string(),
        ),
    ];

    for (case, operation, request, expected_code, expected_message) in &rows {
        let (status, body) = data_plane(&router, operation, request, case).await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (REFUSAL_STATUS, *expected_code),
            "case {case:?}: expected {REFUSAL_STATUS} {expected_code}, got {status} with {body}"
        );
        let actual = error_message(&body);
        assert_eq!(
            &actual, expected_message,
            "case {case:?}: expected message {expected_message:?}, got {actual:?}"
        );
    }

    // The positive control: the real routing control is served, so the rows above refuse
    // their own values and not the operation.
    let (status, served) = data_plane(
        &router,
        "GetRoutingControlState",
        &json!({ "RoutingControlArn": control_arn }),
        "the routing control's own ARN",
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"the routing control's own ARN\": expected 200, got {status} with {served}"
    );
    assert_eq!(
        served.get("RoutingControlState").and_then(Value::as_str),
        Some("Off"),
        "case \"the routing control's own ARN\": a created control reads Off, got {served}"
    );
}

/// RULE: a refusal class this API has not been observed to restate keeps the status its shape
/// carries in the model — so the status above is this API's answer for those two classes and
/// not one status for every failure.
#[tokio::test]
async fn a_refusal_of_another_class_keeps_its_modelled_status() {
    let router = emulator();
    let (_panel_arn, control_arn) = fleet(&router).await;

    // One entry per routing control over the quota, each a well-formed routing control ARN
    // so the batch is refused by the quota and not by its ARNs.
    let entries: Vec<Value> = (0..=BATCH_QUOTA)
        .map(|index| {
            json!({
                "RoutingControlArn": format!("{control_arn}{index}"),
                "RoutingControlState": "On",
            })
        })
        .collect();
    let case = "a batch one entry over the quota";
    let (status, body) = data_plane(
        &router,
        "UpdateRoutingControlStates",
        &json!({ "UpdateRoutingControlStateEntries": entries }),
        case,
    )
    .await;
    assert_eq!(
        (status, error_code(&body).as_str()),
        (
            SERVICE_LIMIT_EXCEEDED_STATUS,
            SERVICE_LIMIT_EXCEEDED_EXCEPTION
        ),
        "case {case:?}: expected {SERVICE_LIMIT_EXCEEDED_STATUS} \
         {SERVICE_LIMIT_EXCEEDED_EXCEPTION}, got {status} with {body}"
    );
}
