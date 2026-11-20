// SPDX-License-Identifier: CC0-1.0

//! What an IAM denial's WORDING is a function OF, end to end over two services: the
//! request and the principal it speaks for — and not the credential that presented it.
//!
//! RULE: an `AccessDenied` names the principal the refused REQUEST speaks for
//! (`iam_authorization_details_pointer.rs` is the table for the sentence itself), so two
//! calls that agree on the principal, the action and the resource are refused in exactly
//! the same words even when they were signed with different credentials of that principal,
//! and a call of another principal names that other principal. The message travels back to
//! an unauthenticated caller in the response body, so nothing about the credential that
//! signed — its access key id, its secret, its signature — appears in it.
//!
//! Only a cross-service suite can state it: ONE principal with TWO credentials is a role
//! session minted twice under the same `RoleSessionName`, which STS mints and records while
//! IAM holds the role and is the gate that decides the later call, so no single crate's own
//! test target can reach the pair. The rows span what the rule generalises over: two
//! credentials of one session principal, a second session principal of the same role, and a
//! second action on the same principal.

use std::sync::Arc;

use bytes::Bytes;
use smithy_server_runtime::MultiServiceRouter;

use crate::iam_authorization_details_pointer::{assert_identity_refusal, sentence_of};
use crate::query_wire::{element, elements, form_encode};

/// The account every generated emulator presents (`engine::EMULATOR_ACCOUNT_ID`).
const ACCOUNT: &str = "000000000000";

/// The Region every request here signs for.
const REGION: &str = "us-east-1";

/// The long-term key every emulator publishes, which is the account's own administrative
/// credential: what the estate below is created with.
const AMBIENT_KEY: &str = "AKIAIOSFODNN7EXAMPLE";

/// The role every session below is minted from. It carries no identity-based policy, so
/// every call its sessions make is IAM's implicit deny.
const ROLE: &str = "RefusalWordingRole";

/// IAM and STS over ONE engine, as `core/router/src/lib.rs` mounts them.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // The signatures below are not real ones; what names the caller is the access key id in
    // the credential scope, which is what this suite varies.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(engine),
    );
    router
}

/// Post one awsQuery form body to `signing_name`, presenting `access_key_id`.
async fn query(
    router: &MultiServiceRouter,
    access_key_id: &str,
    signing_name: &str,
    body: String,
) -> (u16, String) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/{REGION}/{signing_name}/\
                 aws4_request, SignedHeaders=host, Signature=0000"
            ),
        )
        .body(Bytes::from(body))
        .expect("the test request builds");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One IAM call: the action and its form parameters, presented as signed by
/// `access_key_id`.
async fn iam(
    router: &MultiServiceRouter,
    access_key_id: &str,
    action: &str,
    parameters: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-05-08");
    for (name, value) in parameters {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    query(router, access_key_id, "iam", body).await
}

/// One IAM call that is part of the fixture, which must be served.
async fn ok(router: &MultiServiceRouter, action: &str, parameters: &[(&str, &str)]) -> String {
    let (status, body) = iam(router, AMBIENT_KEY, action, parameters).await;
    assert_eq!(
        status, 200,
        "the fixture {action} must be served, got {status}: {body}"
    );
    body
}

/// A trust policy admitting the account root, which is the delegation that lets a principal
/// of this account assume the role.
fn trust_policy() -> String {
    format!(
        r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow","Principal":{{"AWS":"arn:aws:iam::{ACCOUNT}:root"}},"Action":"sts:AssumeRole"}}]}}"#
    )
}

/// Mint a session of [`ROLE`] under `session_name` and answer the access key id of that
/// credential.
///
/// Two mints under ONE `RoleSessionName` are two credentials of the same principal: STS
/// reports the same `AssumedRoleUser` ARN for both, which is the principal IAM's refusal
/// names.
async fn assume_role(router: &MultiServiceRouter, session_name: &str) -> String {
    let role_arn = format!("arn:aws:iam::{ACCOUNT}:role/{ROLE}");
    let body = format!(
        "Action=AssumeRole&Version=2011-06-15&RoleArn={}&RoleSessionName={session_name}",
        form_encode(&role_arn)
    );
    let (status, assumed) = query(router, AMBIENT_KEY, "sts", body).await;
    assert_eq!(
        status, 200,
        "the fixture AssumeRole must succeed, got {status}: {assumed}"
    );
    let reported = element(&assumed, "Arn");
    assert_eq!(
        reported,
        session_arn(session_name),
        "the fixture mint must report the session ARN this suite is about, got {reported:?}"
    );
    element(&assumed, "AccessKeyId")
}

/// The ARN STS reports for a session of [`ROLE`] under `session_name`, which is the
/// principal a refusal of that session's call names.
fn session_arn(session_name: &str) -> String {
    format!("arn:aws:sts::{ACCOUNT}:assumed-role/{ROLE}/{session_name}")
}

/// The sentence IAM refuses a listing with for `session_name`: the principal, the action,
/// the collection the decision was made on and why.
fn refusal(session_name: &str, entity: &str, iam_action: &str) -> String {
    format!(
        "User: {} is not authorized to perform: {iam_action} on resource: \
         arn:aws:iam::{ACCOUNT}:{entity}/ because no identity-based policy allows the \
         {iam_action} action",
        session_arn(session_name)
    )
}

/// Make one refused listing as `access_key_id` and answer the SENTENCE its refusal carried:
/// the refusal closes with the pointer at that one request's authorization details, whose id
/// is minted per request, so the sentence is what two refusals of one call share.
async fn refusal_of(
    router: &MultiServiceRouter,
    case: &str,
    access_key_id: &str,
    session_name: &str,
    action: &str,
    entity: &str,
) -> String {
    let (status, body) = iam(router, access_key_id, action, &[]).await;
    assert_eq!(
        status, 403,
        "case {case:?}: expected the call nothing allows to be refused, got {status}: {body}"
    );
    let code = elements(&body, "Code")
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: no <Code> in {body}"));
    assert_eq!(
        code, "AccessDenied",
        "case {case:?}: expected the error code AccessDenied, got {code:?}: {body}"
    );
    let message = elements(&body, "Message")
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: no <Message> in {body}"));
    assert_identity_refusal(
        case,
        &message,
        &refusal(session_name, entity, &format!("iam:{action}")),
    );
    sentence_of(&message)
}

/// RULE: a refusal's wording is a function of the request and of the principal it speaks
/// for — so two credentials of ONE session principal are refused in the same words for the
/// same call, while another session principal and another action are refused in words of
/// their own. Nothing about the credential that signed reaches the message.
#[tokio::test]
async fn one_principals_two_credentials_are_refused_in_the_same_words() {
    let router = build_router();
    ok(
        &router,
        "CreateRole",
        &[("RoleName", ROLE), ("AssumeRolePolicyDocument", &trust_policy())],
    )
    .await;

    // Two mints under ONE session name: two different credentials, one principal.
    let first = assume_role(&router, "doubled").await;
    let second = assume_role(&router, "doubled").await;
    assert_ne!(
        first, second,
        "the fixture must mint two different credentials, got {first:?} twice"
    );

    let case = "the same refused call signed with each of one principal's credentials";
    let by_first = refusal_of(&router, case, &first, "doubled", "ListRoles", "role").await;
    let by_second = refusal_of(&router, case, &second, "doubled", "ListRoles", "role").await;
    assert_eq!(
        by_first, by_second,
        "case {case:?}: expected one wording for one request of one principal, got \
         {by_first:?} under the first credential and {by_second:?} under the second"
    );

    // Another session of the same role: a different principal, so a different sentence.
    let elsewhere = assume_role(&router, "another").await;
    let case = "the same call made by another session principal";
    let by_other = refusal_of(&router, case, &elsewhere, "another", "ListRoles", "role").await;
    assert_ne!(
        by_other, by_first,
        "case {case:?}: expected another principal's refusal to name that principal, got \
         {by_other:?} for both"
    );

    // Another action of the same principal: a different request, so a different sentence.
    let case = "another action of the same principal";
    let by_action = refusal_of(&router, case, &first, "doubled", "ListUsers", "user").await;
    assert_ne!(
        by_action, by_first,
        "case {case:?}: expected another refused call to name that call, got {by_action:?} \
         for both"
    );
}
