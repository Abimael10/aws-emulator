// SPDX-License-Identifier: CC0-1.0

//! The authorization id an IAM refusal points at RESOLVES — in the service that owns the
//! authorization details of a denied request.
//!
//! The rule under test: an IAM `AccessDenied` message closes with a pointer naming an
//! authorization id minted for that one refused request, and that id names a record IAM
//! Toolbox's `GetRequestAuthorizationDetails` serves: the request context the decision was
//! made in, the evaluation of the `(action, resource)` pair it was made on with the effect
//! that decided it, and the policies that were evaluated. An id nothing was refused under
//! names no record.
//!
//! Only a cross-service suite can pin this: the denial is IAM's
//! (`service-com-amazonaws-iam-awsidentitymanagementv20100508`, awsQuery) while the record it
//! points at belongs to IAM Toolbox
//! (`service-com-amazonaws-iamtoolbox-authrequestservice`, restJson1, `GET
//! /authorization-details/{authorizationId}`), and the two meet only over the one engine the
//! assembled emulator mounts them on. The principals are made with IAM's OWN operations, so
//! the documents the decision reads are the ones those calls wrote.
//!
//! The rows vary what the rule generalises over: which operation is refused, the resource
//! form the decision is made on (a collection ARN, one entity by name), and which decision
//! refused it — nothing allowing the call, or a statement about it saying `Deny`.

use std::collections::BTreeSet;
use std::sync::Arc;

use bytes::Bytes;
use serde_json::Value;
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, form_encode};

/// The Region every call below is signed for. IAM is global, so this is the Region its
/// endpoint lives in.
const REGION: &str = "us-east-1";

/// The access-key id standing for the account's own published credential: IAM holds no
/// record for it, so it resolves to the account's ambient identity, which carries no IAM
/// policies — the position AWS account root credentials are in, and what the fixture calls
/// below are signed as.
const ACCOUNT_KEY: &str = "EMULATORTESTKEY12345";

/// The signing name IAM and IAM Toolbox share.
const SIGNING_NAME: &str = "iam";

/// IAM's API version on the awsQuery wire.
const VERSION: &str = "2010-05-08";

/// What the pointer clause of a refusal names before the authorization id, and what it names
/// after it. Written out here rather than taken from the service under test, so this suite
/// asserts AWS's text.
const POINTER_PREFIX: &str =
    ". Go to https://us-east-1.console.aws.amazon.com/iam/home?region=us-east-1\
     #/authorization-details/";
const POINTER_API: &str =
    " for complete details, or call the GetRequestAuthorizationDetails API with the \
     following authorization id: ";

/// The `EvaluatedEffect` the record of a call nothing allowed carries.
const IMPLICIT_DENY: &str = "IMPLICIT_DENY";

/// The `EvaluatedEffect` the record of a call a statement explicitly denied carries.
const EXPLICIT_DENY: &str = "EXPLICIT_DENY";

/// A SigV4 `Authorization` header whose credential scope names IAM's signing name. The
/// signature itself is never checked — the router here runs with request authentication off
/// — and the access key id is what resolves the caller's identity.
fn authorization(access_key_id: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/{REGION}/{SIGNING_NAME}/\
         aws4_request, SignedHeaders=host, Signature=0"
    )
}

/// The emulator these rows drive: IAM beside the service that owns the authorization details
/// of a refused request, on ONE engine, as `core/router/src/lib.rs` mounts them.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_iamtoolbox_authrequestservice::EmulatedService::new(engine),
    );
    router
}

/// One awsQuery call to IAM, signed with `access_key_id`.
async fn iam(
    router: &MultiServiceRouter,
    access_key_id: &str,
    action: &str,
    params: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={VERSION}");
    for (name, value) in params {
        body.push('&');
        body.push_str(name);
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("https://iam.amazonaws.com/")
        .header("host", "iam.amazonaws.com")
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(access_key_id))
        .body(Bytes::from(body))
        .expect("a well-formed awsQuery request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).to_string();
    (status, text)
}

/// One awsQuery call signed as the account itself, which must succeed: the fixture legs.
async fn fixture(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    params: &[(&str, &str)],
) -> String {
    let (status, body) = iam(router, ACCOUNT_KEY, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the fixture {action} must succeed, got {status}: {body}"
    );
    body
}

/// `GET /authorization-details/{id}`: the read IAM Toolbox serves, signed as the account.
async fn authorization_details(
    router: &MultiServiceRouter,
    authorization_id: &str,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("GET")
        .uri(format!(
            "https://iam.amazonaws.com/authorization-details/{authorization_id}"
        ))
        .header("host", "iam.amazonaws.com")
        .header("authorization", authorization(ACCOUNT_KEY))
        .body(Bytes::new())
        .expect("a well-formed GetRequestAuthorizationDetails request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let body = response.into_body();
    let parsed = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&body).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// An IAM user carrying `policies` as its inline policies, with a long-term access key:
/// answers that key id.
async fn principal(
    router: &MultiServiceRouter,
    case: &str,
    user_name: &str,
    policies: &[(&str, String)],
) -> String {
    fixture(router, case, "CreateUser", &[("UserName", user_name)]).await;
    for (policy_name, document) in policies {
        fixture(
            router,
            case,
            "PutUserPolicy",
            &[
                ("UserName", user_name),
                ("PolicyName", policy_name),
                ("PolicyDocument", document),
            ],
        )
        .await;
    }
    let minted = fixture(
        router,
        case,
        "CreateAccessKey",
        &[("UserName", user_name)],
    )
    .await;
    element(&minted, "AccessKeyId")
}

/// A one-statement document.
fn statement(effect: &str, action: &str) -> String {
    format!(
        r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"{effect}","Action":"{action}","Resource":"*"}}]}}"#
    )
}

/// The authorization id the pointer clause of `message` names, asserted to be there and to
/// be the same id in the console address and in the API pointer.
fn pointed_at(case: &str, message: &str) -> String {
    let (before, id) = message.split_once(POINTER_API).unwrap_or_else(|| {
        panic!(
            "case {case:?}: expected the refusal to point at its authorization details, got \
             {message:?}"
        )
    });
    assert!(
        before.ends_with(&format!("{POINTER_PREFIX}{id}")),
        "case {case:?}: expected the console address to name the same authorization id \
         {id:?}, got {message:?}"
    );
    id.to_string()
}

/// One row: the refused call, the action its decision is made on, the resource it is made
/// against, the policies the caller holds and the effect the record must report.
struct Row {
    case: &'static str,
    action: &'static str,
    params: Vec<(&'static str, String)>,
    iam_action: &'static str,
    resource: String,
    policies: Vec<(&'static str, String)>,
    effect: &'static str,
}

/// The rows, over the account the fixture calls run against.
fn rows(account: &str) -> Vec<Row> {
    vec![
        // Nothing allows the call: a collection read, decided on the ARN of the collection.
        Row {
            case: "a user listing nothing allows",
            action: "ListUsers",
            params: Vec::new(),
            iam_action: "iam:ListUsers",
            resource: format!("arn:aws:iam::{account}:user/"),
            policies: Vec::new(),
            effect: IMPLICIT_DENY,
        },
        // A grant for another action: a document IS in force, and it is the one the record
        // must report as evaluated.
        Row {
            case: "a role listing withheld beside a grant for another action",
            action: "ListRoles",
            params: Vec::new(),
            iam_action: "iam:ListRoles",
            resource: format!("arn:aws:iam::{account}:role/"),
            policies: vec![("allow-listusers", statement("Allow", "iam:ListUsers"))],
            effect: IMPLICIT_DENY,
        },
        // A statement about the request saying Deny, over a grant for everything: the other
        // decision, which the record reports as the explicit deny it is.
        Row {
            case: "a read of one entity a statement denies",
            action: "ListAccessKeys",
            params: vec![("UserName", "reported".to_string())],
            iam_action: "iam:ListAccessKeys",
            resource: format!("arn:aws:iam::{account}:user/reported"),
            policies: vec![
                ("allow-everything", statement("Allow", "iam:*")),
                ("deny-the-action", statement("Deny", "iam:ListAccessKeys")),
            ],
            effect: EXPLICIT_DENY,
        },
    ]
}

/// RULE: the authorization id an IAM refusal points at names the authorization details of
/// THAT refused request, which the service owning them serves: the context the decision was
/// made in, the evaluation of the action and resource it was made on with the effect that
/// decided it, and the policies in force on the caller.
#[tokio::test]
async fn an_iam_denial_points_at_a_record_the_toolbox_service_serves() {
    let router = build_router();
    let account = ::engine::emulator_account_id();
    // The entity the row that reads one entity addresses, created with the account's own
    // credentials so no row's refusal is about it.
    fixture(&router, "the fixture target", "CreateUser", &[("UserName", "reported")]).await;

    let mut minted: Vec<String> = Vec::new();
    for (index, row) in rows(account).into_iter().enumerate() {
        let user_name = format!("pointed{index}");
        let access_key_id = principal(&router, row.case, &user_name, &row.policies).await;
        let params: Vec<(&str, &str)> = row
            .params
            .iter()
            .map(|(name, value)| (*name, value.as_str()))
            .collect();

        let (status, body) = iam(&router, &access_key_id, row.action, &params).await;
        assert_eq!(
            status, 403,
            "case {:?}: expected the refused call to answer HTTP 403, got {status}: {body}",
            row.case
        );
        let message = element(&body, "Message");
        let authorization_id = pointed_at(row.case, &message);
        minted.push(authorization_id.clone());

        // The id resolves — in the service that owns the record, over its own wire.
        let (detail_status, detail) = authorization_details(&router, &authorization_id).await;
        assert_eq!(
            detail_status, 200,
            "case {:?}: expected the authorization id {authorization_id:?} to resolve, got \
             {detail_status}: {detail:?}",
            row.case
        );

        // The record is about THIS request: the action and the resource the decision was
        // made on, and the effect that decided it.
        let evaluations = detail["evaluations"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            evaluations.len(),
            1,
            "case {:?}: expected one evaluation, got {detail:?}",
            row.case
        );
        let evaluation = &evaluations[0];
        assert_eq!(
            evaluation["action"].as_str(),
            Some(row.iam_action),
            "case {:?}: expected the evaluation to name the action decided, got {evaluation:?}",
            row.case
        );
        assert_eq!(
            evaluation["resource"].as_str(),
            Some(row.resource.as_str()),
            "case {:?}: expected the evaluation to name the resource decided against, got \
             {evaluation:?}",
            row.case
        );
        assert_eq!(
            evaluation["evaluatedEffect"].as_str(),
            Some(row.effect),
            "case {:?}: expected the effect {:?}, got {evaluation:?}",
            row.case,
            row.effect
        );

        // The context the decision was made in, which names the principal that signed.
        assert_eq!(
            detail["requestContext"]["aws:username"].as_str(),
            Some(user_name.as_str()),
            "case {:?}: expected the request context to name the signing user, got {detail:?}",
            row.case
        );

        // The policies that were evaluated: one entry per document in force on the caller,
        // each identified as the inline policy it is.
        let policies = detail["policies"].as_array().cloned().unwrap_or_default();
        assert_eq!(
            policies.len(),
            row.policies.len(),
            "case {:?}: expected one entry per document in force, got {policies:?}",
            row.case
        );
        for (policy_name, _) in &row.policies {
            assert!(
                policies.iter().any(|policy| {
                    policy["type"].as_str() == Some("IDENTITY_BASED_POLICY")
                        && policy["inline"].as_bool() == Some(true)
                        && policy["uri"]
                            .as_str()
                            .is_some_and(|uri| uri.ends_with(policy_name))
                }),
                "case {:?}: expected the evaluated policies to carry {policy_name:?}, got \
                 {policies:?}",
                row.case
            );
        }
    }

    // Each refusal is pointed at a record of its own.
    let distinct: BTreeSet<&String> = minted.iter().collect();
    assert_eq!(
        distinct.len(),
        minted.len(),
        "expected every refusal to mint its own authorization id, got {minted:?}"
    );
}

/// RULE: only a refusal mints an authorization id, so an id nothing was refused under names
/// no record — whatever its form.
#[tokio::test]
async fn an_authorization_id_nothing_was_refused_under_resolves_to_nothing() {
    let router = build_router();

    // Rows spanning the forms an id of the modelled alphabet takes: the shortest one, a long
    // one, and one minted BY a refusal in another emulator (a well-formed id this engine
    // never handed out).
    for id in ["0", "zzzzzzzzzzzzzzzzzzzzzzzzz", "3x9q1p7m2k5h8d4s6g0b1n2v7"] {
        let (status, body) = authorization_details(&router, id).await;
        assert_eq!(
            status, 404,
            "case {id:?}: expected an id nothing was refused under to resolve to nothing, \
             got {status}: {body:?}"
        );
    }
}
