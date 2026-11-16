// SPDX-License-Identifier: CC0-1.0

//! WHO may reach the operations of this cluster (data-plane) API, driven end to end over
//! the real wire path (an awsJson1_0 request through the multi-service router into the
//! mounted service), so what is asserted is what a client holding those credentials sees.
//!
//! THE RULE UNDER TEST: every operation of this API evaluates the identity-based IAM
//! policies in force on the principal its credentials name against
//! `route53-recovery-cluster:<OperationName>`, on the resource the request ADDRESSES,
//! BEFORE it reads the registry or moves a switch — so a request no statement allows
//! (IAM's default deny), or one an explicit `Deny` covers, is answered HTTP 403
//! `AccessDeniedException` whatever state the routing control is in, and nothing is read
//! or written; a request a matching `Allow` covers reaches the operation's own answer.
//!
//! The role, its policies and the session live in the IAM and STS stores of the SAME
//! shared engine — the stores those services' own handlers write — so the documents under
//! test are the ones `iam:PutRolePolicy` stores and the principal a refusal blames is the
//! one `sts:GetCallerIdentity` answers for the key the request signed with.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::SystemTime;

use serde_json::{json, Value};
use service_com_amazonaws_iam_awsidentitymanagementv20100508::state as iam;
use service_com_amazonaws_sts_awssecuritytokenservicev20110615::state as sts;
use smithy_server_runtime::MultiServiceRouter;

use super::common::{
    call, call_as, create_cluster, create_control_panel, create_routing_control,
    emulator_with_identity, error_code, error_message, unknown_arn_like, Control, Panel,
    ACCOUNT_KEY_ID,
};

/// This API's IAM action prefix: its `arnNamespace`, which is also its `sigv4Name`
/// (`smithy_query(query="service_info", model_path="services/service-com-amazonaws-route53recoverycluster-togglecustomerapi/smithy-model/com-amazonaws-route53recoverycluster-togglecustomerapi.json")`).
const ACTION_PREFIX: &str = "route53-recovery-cluster";

/// The access key id the restricted cases sign with: the assumed-role session seeded below.
const SESSION_KEY_ID: &str = "ASIAEXAMPLESESSION01";

/// The role that session is assumed from, and the session's own name.
const CALLER_ROLE: &str = "arc-operator";
const CALLER_SESSION: &str = "failover-session";

/// The name of the inline policy a case puts in force on that role.
const POLICY_NAME: &str = "inline";

/// How long a seeded session lasts: an hour, STS's own default.
const SESSION_SECONDS: i32 = 3_600;

/// The ARN the account holds the calling role under.
fn role_arn() -> String {
    format!(
        "arn:aws:iam::{}:role/{CALLER_ROLE}",
        ::engine::emulator_account_id()
    )
}

/// The principal ARN the assumed-role session speaks for — what an authorization refusal
/// names.
fn session_arn() -> String {
    format!(
        "arn:aws:sts::{}:assumed-role/{CALLER_ROLE}/{CALLER_SESSION}",
        ::engine::emulator_account_id()
    )
}

/// Record the calling role and the assumed-role session [`SESSION_KEY_ID`] speaks for, as
/// `iam:CreateRole` and `sts:AssumeRole` record them.
fn given_session(engine: &Arc<engine::InMemoryEngine>) {
    let store = iam::service_state(&**engine).expect("the IAM store");
    store.lock().roles.insert(
        iam::name_key(CALLER_ROLE),
        ::service_com_amazonaws_iam_awsidentitymanagementv20100508::ops::types::Role {
            path: "/".to_string(),
            role_name: CALLER_ROLE.to_string(),
            role_id: "AROAEXAMPLEARCOPERATOR".to_string(),
            arn: role_arn(),
            create_date: ::aws_smithy_types::DateTime::from_secs(0),
            assume_role_policy_document: None,
            description: None,
            max_session_duration: None,
            permissions_boundary: None,
            tags: None,
            role_last_used: None,
            source_role_template: None,
        },
    );

    let vend = sts::NewSession {
        role_arn: Some(role_arn()),
        ..sts::NewSession::new(
            sts::Identity {
                user_id: sts::role_id(&role_arn()),
                account: ::engine::emulator_account_id().to_string(),
                arn: session_arn(),
            },
            sts::SessionKind::AssumedRole,
            SESSION_SECONDS,
        )
    };
    let store = sts::service_state(&**engine).expect("the STS store");
    store
        .lock()
        .insert(vend.into_session(SESSION_KEY_ID, SystemTime::now()))
        .expect("the session is recorded");
}

/// Put `document` in force on the calling role as its inline policy, replacing whatever it
/// carried; `None` takes every policy off it, as `iam:DeleteRolePolicy` does.
fn given_role_policy(engine: &Arc<engine::InMemoryEngine>, document: Option<&Value>) {
    let store = iam::service_state(&**engine).expect("the IAM store");
    let mut store = store.lock();
    store.role_inline_policies.remove(&iam::name_key(CALLER_ROLE));
    if let Some(document) = document {
        store.role_inline_policies.insert(
            iam::name_key(CALLER_ROLE),
            BTreeMap::from([(
                iam::name_key(POLICY_NAME),
                iam::InlinePolicy {
                    policy_name: POLICY_NAME.to_string(),
                    policy_document: document.to_string(),
                },
            )]),
        );
    }
}

/// A policy allowing `actions` on `resource`.
fn allow(actions: Value, resource: Value) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{ "Effect": "Allow", "Action": actions, "Resource": resource }]
    })
}

/// Which of IAM's two refusals a case expects.
#[derive(Debug, Clone, Copy)]
enum Refusal {
    /// The default deny of a request no statement allows.
    DefaultDeny,
    /// The explicit deny of a request a `Deny` statement covers.
    ExplicitDeny,
}

impl Refusal {
    /// The clause IAM closes the refusal with.
    fn reason(self, action: &str) -> String {
        match self {
            Self::DefaultDeny => {
                format!("because no identity-based policy allows the {action} action")
            }
            Self::ExplicitDeny => "with an explicit deny in an identity-based policy".to_string(),
        }
    }
}

/// The sentence IAM refuses a request with: who, which action, on which resource when the
/// request addressed one, and which of its two rules refused.
///
/// Spelled out here independently of the emulator's own builder, so a change to that
/// builder has to agree with this sentence rather than redefine it.
fn not_authorized(
    principal: &str,
    action: &str,
    resource: Option<&str>,
    refusal: Refusal,
) -> String {
    let on_resource = resource.map_or_else(String::new, |arn| format!(" on resource: {arn}"));
    format!(
        "User: {principal} is not authorized to perform: {action}{on_resource} {}",
        refusal.reason(action)
    )
}

/// One control panel holding `count` routing controls, all `Off`, with the IAM and STS
/// stores carrying the calling role and its session.
async fn fixture(count: usize) -> (MultiServiceRouter, Arc<engine::InMemoryEngine>, Panel, Vec<Control>) {
    let (router, engine) = emulator_with_identity();
    given_session(&engine);
    let cluster = create_cluster(&router, "authorization").await;
    let mut panel = create_control_panel(&router, &cluster, "authorization-panel").await;
    let mut controls = Vec::new();
    for index in 0..count {
        let control = create_routing_control(&router, &panel, &format!("control-{index}")).await;
        controls.push(control.clone());
        panel.controls.push(control);
    }
    (router, engine, panel, controls)
}

/// Assert `body` is the modelled 403 refusal with IAM's sentence for `action` on `resource`.
fn assert_refused(
    case: &str,
    status: u16,
    body: &Value,
    action: &str,
    resource: Option<&str>,
    refusal: Refusal,
) {
    assert_eq!(
        (status, error_code(body).as_str()),
        (403, "AccessDeniedException"),
        "case {case:?}: expected 403 AccessDeniedException, got {status} with {body}"
    );
    let expected = not_authorized(&session_arn(), action, resource, refusal);
    let actual = error_message(body);
    assert_eq!(
        actual, expected,
        "case {case:?}: expected message {expected:?}, got {actual:?}"
    );
}

/// RULE: the documents in force on the calling principal decide the request, judged against
/// the action this operation publishes and the routing control the request addressed — an
/// explicit `Deny` refuses, a matching `Allow` serves, and anything else falls to IAM's
/// default deny.
///
/// The rows differ along what the document states: which actions it covers (this one,
/// another of this service, another service's, every one), which resources, its `Effect`,
/// and the case the action is spelled in — IAM compares action names case-insensitively, so
/// the spelling is not what decides the request.
#[tokio::test]
async fn the_identity_policy_in_force_decides_a_read_of_routing_control_state() {
    let (router, engine, _panel, controls) = fixture(2).await;
    let arn = controls[0].arn.clone();
    let elsewhere = unknown_arn_like(&arn);
    let action = format!("{ACTION_PREFIX}:GetRoutingControlState");

    // (case, the document in force, which refusal it meets — `None` when it is served)
    let rows: Vec<(&str, Option<Value>, Option<Refusal>)> = vec![
        ("no policy at all", None, Some(Refusal::DefaultDeny)),
        (
            "an allow of another action of this service, but not this one",
            Some(allow(
                json!(format!("{ACTION_PREFIX}:UpdateRoutingControlState")),
                json!("*"),
            )),
            Some(Refusal::DefaultDeny),
        ),
        (
            "an allow of every action of another service",
            Some(allow(json!("route53-recovery-control-config:*"), json!("*"))),
            Some(Refusal::DefaultDeny),
        ),
        (
            "an allow of this action on ANOTHER routing control",
            Some(allow(json!(action.clone()), json!(elsewhere.clone()))),
            Some(Refusal::DefaultDeny),
        ),
        (
            "an explicit deny of exactly this action beside an allow of this whole service",
            Some(json!({
                "Statement": [
                    { "Effect": "Allow", "Action": format!("{ACTION_PREFIX}:*"), "Resource": "*" },
                    { "Effect": "Deny", "Action": action.clone(), "Resource": "*" },
                ],
            })),
            Some(Refusal::ExplicitDeny),
        ),
        (
            "an explicit deny of this whole service beside an allow of exactly this action",
            Some(json!({
                "Statement": [
                    { "Effect": "Allow", "Action": action.clone(), "Resource": "*" },
                    { "Effect": "Deny", "Action": format!("{ACTION_PREFIX}:*"), "Resource": "*" },
                ],
            })),
            Some(Refusal::ExplicitDeny),
        ),
        (
            "a deny scoped to the routing control this request names",
            Some(json!({
                "Statement": [
                    { "Effect": "Allow", "Action": action.clone(), "Resource": "*" },
                    { "Effect": "Deny", "Action": action.clone(), "Resource": arn.clone() },
                ],
            })),
            Some(Refusal::ExplicitDeny),
        ),
        (
            // A deny about ANOTHER resource is not about this request.
            "a deny scoped to another routing control",
            Some(json!({
                "Statement": [
                    { "Effect": "Allow", "Action": action.clone(), "Resource": "*" },
                    { "Effect": "Deny", "Action": action.clone(), "Resource": elsewhere.clone() },
                ],
            })),
            None,
        ),
        (
            "an allow of exactly this action on every resource",
            Some(allow(json!(action.clone()), json!("*"))),
            None,
        ),
        (
            "an allow of this action spelled in lower case",
            Some(allow(json!(action.to_lowercase()), json!("*"))),
            None,
        ),
        (
            "an allow of this action on exactly this routing control",
            Some(allow(json!(action.clone()), json!(arn.clone()))),
            None,
        ),
        (
            "an allow of every action of this service",
            Some(allow(json!(format!("{ACTION_PREFIX}:*")), json!("*"))),
            None,
        ),
        (
            "an allow of every action",
            Some(allow(json!("*"), json!("*"))),
            None,
        ),
    ];

    for (case, document, refusal) in &rows {
        given_role_policy(&engine, document.as_ref());
        let (status, body) = call_as(
            &router,
            SESSION_KEY_ID,
            "GetRoutingControlState",
            &json!({ "RoutingControlArn": arn }),
        )
        .await;
        match refusal {
            Some(refusal) => assert_refused(case, status, &body, &action, Some(&arn), *refusal),
            None => {
                assert_eq!(
                    status, 200,
                    "case {case:?}: an authorized caller is served, got {status} with {body}"
                );
                assert_eq!(
                    body.get("RoutingControlState").and_then(Value::as_str),
                    Some("Off"),
                    "case {case:?}: expected the control's own state, got {body}"
                );
            }
        }
    }
}

/// RULE: the gate covers EVERY operation of this API, each judged against its OWN action
/// name and the resource its own input addresses — the routing control a get or an update
/// names, the control panel a listing is filtered by, and no resource at all for a listing
/// that carries no filter; and the account's own published credential, which holds no IAM
/// policies, is not bounded by it.
#[tokio::test]
async fn every_operation_takes_the_decision_against_its_own_action_and_resource() {
    let (router, engine, panel, controls) = fixture(2).await;
    let arn = controls[0].arn.clone();
    given_role_policy(&engine, None);

    // (operation, the request, the resource it addresses)
    let guarded: Vec<(&str, Value, Option<String>)> = vec![
        (
            "GetRoutingControlState",
            json!({ "RoutingControlArn": arn }),
            Some(arn.clone()),
        ),
        (
            "UpdateRoutingControlState",
            json!({ "RoutingControlArn": arn, "RoutingControlState": "On" }),
            Some(arn.clone()),
        ),
        (
            "UpdateRoutingControlStates",
            json!({
                "UpdateRoutingControlStateEntries": [
                    { "RoutingControlArn": arn, "RoutingControlState": "On" },
                ],
            }),
            Some(arn.clone()),
        ),
        (
            "ListRoutingControls",
            json!({ "ControlPanelArn": panel.arn }),
            Some(panel.arn.clone()),
        ),
        ("ListRoutingControls", json!({}), None),
    ];

    for (operation, request, resource) in &guarded {
        let action = format!("{ACTION_PREFIX}:{operation}");
        let case = format!("{operation} addressing {resource:?}");
        let (status, body) = call_as(&router, SESSION_KEY_ID, operation, request).await;
        assert_refused(
            &case,
            status,
            &body,
            &action,
            resource.as_deref(),
            Refusal::DefaultDeny,
        );

        // The account's own credential holds no IAM policies, so this gate does not bound
        // it: whatever it gets back, it is not this refusal.
        let (status, served) = call_as(&router, ACCOUNT_KEY_ID, operation, request).await;
        assert_ne!(
            (status, error_code(&served).as_str()),
            (403, "AccessDeniedException"),
            "case {case:?}: the account's own credential must not be denied, got {status} with \
             {served}"
        );
    }
}

/// RULE: the denial is scoped to the action the `Deny` names — another action of the same
/// service still goes through — it changes nothing, and it lifts again as soon as the
/// statement is taken off the same principal.
#[tokio::test]
async fn an_explicit_deny_is_scoped_to_its_action_and_lifts_when_it_is_removed() {
    let (router, engine, _panel, controls) = fixture(1).await;
    let arn = controls[0].arn.clone();
    let read_action = format!("{ACTION_PREFIX}:GetRoutingControlState");
    let allow_the_service = allow(json!(format!("{ACTION_PREFIX}:*")), json!("*"));

    // Allow of the whole service, plus a Deny of the READ alone.
    given_role_policy(
        &engine,
        Some(&json!({
            "Statement": [
                { "Effect": "Allow", "Action": format!("{ACTION_PREFIX}:*"), "Resource": "*" },
                { "Effect": "Deny", "Action": read_action.clone(), "Resource": "*" },
            ],
        })),
    );

    let (status, body) = call_as(
        &router,
        SESSION_KEY_ID,
        "GetRoutingControlState",
        &json!({ "RoutingControlArn": arn }),
    )
    .await;
    assert_refused(
        "the read is denied",
        status,
        &body,
        &read_action,
        Some(&arn),
        Refusal::ExplicitDeny,
    );

    // The UPDATE the same policy allows is served, so the denial is about one action.
    let (status, body) = call_as(
        &router,
        SESSION_KEY_ID,
        "UpdateRoutingControlState",
        &json!({ "RoutingControlArn": arn, "RoutingControlState": "On" }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"an action the deny does not name\": expected 200, got {status} with {body}"
    );

    // Taking the Deny off the same principal restores the read, which reports the state
    // the allowed update wrote: the refusal was policy-driven, not session-driven.
    given_role_policy(&engine, Some(&allow_the_service));
    let (status, body) = call_as(
        &router,
        SESSION_KEY_ID,
        "GetRoutingControlState",
        &json!({ "RoutingControlArn": arn }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"the deny is removed\": expected 200, got {status} with {body}"
    );
    assert_eq!(
        body.get("RoutingControlState").and_then(Value::as_str),
        Some("On"),
        "case \"the deny is removed\": expected the state the update wrote, got {body}"
    );
}

/// RULE: the decision is taken BEFORE the service's own preconditions, so a batch over the
/// quota from a caller no policy allows is answered the authorization refusal rather than
/// the quota refusal — and the resource it names is the one its own first entry addresses.
#[tokio::test]
async fn authorization_is_decided_before_the_batch_quota() {
    let (router, engine, _panel, controls) = fixture(1).await;
    given_role_policy(&engine, None);
    let action = format!("{ACTION_PREFIX}:UpdateRoutingControlStates");

    // Twelve entries, two over the cap of ten: well-formed ARNs of the same shape, which the
    // gate judges without resolving any of them.
    let base = controls[0].arn.clone();
    let entries: Vec<Value> = (0..12)
        .map(|index| {
            json!({
                "RoutingControlArn": format!("{base}-{index}"),
                "RoutingControlState": "On",
            })
        })
        .collect();

    let (status, body) = call_as(
        &router,
        SESSION_KEY_ID,
        "UpdateRoutingControlStates",
        &json!({ "UpdateRoutingControlStateEntries": entries }),
    )
    .await;
    assert_refused(
        "a batch over the quota from an unauthorized caller",
        status,
        &body,
        &action,
        Some(&format!("{base}-0")),
        Refusal::DefaultDeny,
    );
}

/// RULE: a refused call changes nothing — a batch an explicit `Deny` covers on ONE of its
/// entries moves no switch at all.
#[tokio::test]
async fn a_refused_batch_update_moves_no_switch() {
    let (router, engine, _panel, controls) = fixture(2).await;
    let allowed = controls[0].arn.clone();
    let denied = controls[1].arn.clone();
    let action = format!("{ACTION_PREFIX}:UpdateRoutingControlStates");

    given_role_policy(
        &engine,
        Some(&json!({
            "Statement": [
                { "Effect": "Allow", "Action": format!("{ACTION_PREFIX}:*"), "Resource": "*" },
                { "Effect": "Deny", "Action": action.clone(), "Resource": denied.clone() },
            ],
        })),
    );

    let (status, body) = call_as(
        &router,
        SESSION_KEY_ID,
        "UpdateRoutingControlStates",
        &json!({
            "UpdateRoutingControlStateEntries": [
                { "RoutingControlArn": allowed, "RoutingControlState": "On" },
                { "RoutingControlArn": denied, "RoutingControlState": "On" },
            ],
        }),
    )
    .await;
    assert_refused(
        "one entry of the batch is denied",
        status,
        &body,
        &action,
        Some(&denied),
        Refusal::ExplicitDeny,
    );

    for arn in [&allowed, &denied] {
        let (status, body) = call(
            &router,
            "GetRoutingControlState",
            &json!({ "RoutingControlArn": arn }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {arn:?}: reading back expected 200, got {status} with {body}"
        );
        assert_eq!(
            body.get("RoutingControlState").and_then(Value::as_str),
            Some("Off"),
            "case {arn:?}: a refused batch must move no switch, got {body}"
        );
    }
}
