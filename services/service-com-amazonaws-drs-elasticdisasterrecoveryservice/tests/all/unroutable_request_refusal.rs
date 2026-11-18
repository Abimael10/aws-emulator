// SPDX-License-Identifier: CC0-1.0

//! What this endpoint answers a request it cannot place on one of its operations.
//!
//! Elastic Disaster Recovery authorizes a request before it dispatches it, and the action
//! it authorizes comes from the request's method and URI. A request those cannot place on
//! an operation of the service's model therefore names no action to authorize, and the
//! layer ahead of the service refuses it as an authorization failure — HTTP 403, error
//! class `AccessDeniedException`, and one fixed sentence that names neither the method,
//! the URI nor any operation — rather than reporting on what the service implements.
//!
//! The rule under test, asserted over a TABLE of requests rather than one recorded
//! request:
//!
//! 1. Every request this endpoint claims whose method and path name no operation of the
//!    model gets that ONE answer, whatever method and path it carried: an operation name
//!    in the path of a route the model does not bind, a method no route of that path
//!    binds, a path of no route at all.
//! 2. It says nothing about which operations this emulator has implemented: a request
//!    that DOES name a modelled route is routed to that operation and answered by its
//!    handler.
//!
//! Every assertion drives the real `restJson1` front door: the request is built the way a
//! client builds it, dispatched through [`MultiServiceRouter`], and judged on the HTTP
//! status, the `x-amzn-errortype` header and the JSON body.

use serde_json::{json, Value};

use crate::replication_configuration_template_lifecycle::{call_in, drs, Answer};
use crate::resource_tags::{call_path, encoded_label};

/// The Region every request here signs for.
const REGION: &str = "eu-west-1";

/// The error class the authorization layer's refusal carries, which is what `restJson1`
/// resolves an error shape from, and the status it carries.
const NOT_AUTHORIZED_CODE: &str = "AccessDeniedException";
const NOT_AUTHORIZED_STATUS: u16 = 403;

/// The whole sentence that refusal states. Entirely fixed text: no part of it comes from
/// the request or from state. Written out here as the literal a client reads, so a
/// wording that drifts fails this suite.
const NOT_AUTHORIZED: &str = "Unable to determine service/operation name to be authorized";

/// A string member of an answer's body.
fn text(answer: &Answer, name: &str) -> String {
    answer
        .member(name)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// Every request whose method and path name no operation of this model is refused as the
/// request the layer could not place on an action, in one wording. The rows differ along
/// what the rule generalises over: an operation of this service named in a path no route
/// binds (which is how the tagging operations are addressed by a caller that assumes this
/// service's RPC-style routes), a name no operation has, a method the addressed path does
/// not bind, the tagging route without its resource label, and the endpoint's root.
#[tokio::test]
async fn a_request_naming_no_operation_is_refused_as_unauthorizable() {
    let router = drs();
    let arn = format!(
        "arn:aws:drs:{REGION}:{}:recovery-plan/plan-00000000000000000",
        engine::EMULATOR_ACCOUNT_ID
    );
    let tagging_path = format!("/tags/{}", encoded_label(&arn));

    let rows: Vec<(&str, &str, String)> = vec![
        (
            "a tagging operation named in the path, which the model binds to /tags",
            "POST",
            "/ListTagsForResource".to_string(),
        ),
        (
            "another tagging operation named in the path",
            "POST",
            "/TagResource".to_string(),
        ),
        (
            "a third tagging operation named in the path",
            "POST",
            "/UntagResource".to_string(),
        ),
        (
            "a name no operation of this model has",
            "POST",
            "/CreateRecoveryPlanDraft".to_string(),
        ),
        (
            "a modelled operation name under a method it does not bind",
            "DELETE",
            "/CreateRecoveryPlan".to_string(),
        ),
        (
            "the tagging route without its resource label",
            "GET",
            "/tags".to_string(),
        ),
        (
            "a method the tagging route does not bind",
            "PATCH",
            tagging_path.clone(),
        ),
        ("the endpoint's root", "GET", "/".to_string()),
        (
            "a path of no route at all",
            "GET",
            "/recovery-plans/plan-00000000000000000".to_string(),
        ),
    ];

    for (case, method, path) in &rows {
        let answer = call_path(&router, REGION, method, path, b"{}").await;
        assert_eq!(
            answer.status, NOT_AUTHORIZED_STATUS,
            "case {case:?} ({method} {path}): expected {NOT_AUTHORIZED_STATUS}, got {answer:?}"
        );
        assert_eq!(
            answer.error_code.as_deref(),
            Some(NOT_AUTHORIZED_CODE),
            "case {case:?} ({method} {path}): expected {NOT_AUTHORIZED_CODE}, got {answer:?}"
        );
        assert_eq!(
            text(&answer, "message"),
            NOT_AUTHORIZED,
            "case {case:?} ({method} {path}): wrong message in {answer:?}"
        );
    }
}

/// The refusal above is about the ROUTE, not about what this emulator implements: a
/// request that names a modelled route reaches that operation. The rows are the routes
/// this model binds in each of its two shapes — an operation named in the path, and the
/// tagging route — so neither shape is answered by the authorization layer.
#[tokio::test]
async fn a_request_naming_a_modelled_route_reaches_its_operation() {
    let router = drs();

    // A modelled route whose operation is implemented: it answers that operation's own
    // modelled success, so the refusal above cannot be standing in front of it.
    let created = call_in(&router, REGION, "CreateRecoveryPlan", &json!({"name": "Routed"})).await;
    assert_eq!(
        created.status, 201,
        "case \"an implemented operation named in the path\": expected 201, got {created:?}"
    );
    let arn = created
        .member("recoveryPlan")
        .and_then(|plan| plan.get("recoveryPlanArn"))
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("expected the created plan's ARN, got {created:?}"))
        .to_string();
    let tagging_path = format!("/tags/{}", encoded_label(&arn));

    // The tagging route in all three the shapes this model binds, each as a client sends
    // it — `UntagResource` binds `tagKeys` to the query string and the model makes it
    // required, so a request for it carries one. What each operation answers is its own
    // business — one is implemented, the others are not yet — so what is asserted here is
    // only that the authorization layer did not answer for them.
    let untagging_path = format!("{tagging_path}?tagKeys=env");
    let rows: Vec<(&str, &str, &str)> = vec![
        ("the tagging route's read", "GET", &tagging_path),
        ("the tagging route's write", "POST", &tagging_path),
        ("the tagging route's delete", "DELETE", &untagging_path),
    ];
    for (case, method, path) in &rows {
        let answer = call_path(&router, REGION, method, path, b"{\"tags\":{}}").await;
        assert_ne!(
            answer.status, NOT_AUTHORIZED_STATUS,
            "case {case:?} ({method} {path}): a modelled route must reach its operation, \
             got {answer:?}"
        );
        assert_ne!(
            text(&answer, "message"),
            NOT_AUTHORIZED,
            "case {case:?} ({method} {path}): a modelled route must reach its operation, \
             got {answer:?}"
        );
    }
}
