// SPDX-License-Identifier: CC0-1.0

//! Calling the emulator AS A NAMED PRINCIPAL.
//!
//! The suites' default harness — building the emulator, sending a call, reading
//! an answer and the small value helpers — lives in
//! [`crate::repository_lifecycle`] and signs every request with
//! [`DEFAULT_ACCESS_KEY_ID`]. This module is the one place a request's SigV4
//! access key id is chosen, because a service that reads a caller's identity from
//! it (a comment's `authorArn`, the owner of an emoji reaction) can only be
//! tested by calling as two different principals. That default harness builds its
//! requests through [`call_as`] too, so there is exactly one request builder.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use serde_json::Value;
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_iam_awsidentitymanagementv20100508::ops::types::User;
use service_com_amazonaws_iam_awsidentitymanagementv20100508::state as iam;
use service_com_amazonaws_sts_awssecuritytokenservicev20110615::state as sts;

use crate::repository_lifecycle::REGION;

/// The access key id a request is signed with unless a suite is checking that the
/// service tells two callers apart. The shared harness holds it for an IAM user of
/// the account ([`hold_long_term_key`]), the position a long-term key is in on AWS.
pub(crate) const DEFAULT_ACCESS_KEY_ID: &str = "AKIDTESTKEY";

/// The ARN the service reports for a caller signing with `access_key_id` — what a
/// comment's `authorArn` and a reaction's owner are read back as: the ARN of the IAM
/// user the account holds that key for ([`hold_long_term_key`]).
pub(crate) fn caller_arn(access_key_id: &str) -> String {
    format!(
        "arn:aws:iam::{}:user/{access_key_id}",
        engine::emulator_account_id()
    )
}

/// The NAME of the principal a request signed with `access_key_id` speaks for: the
/// last segment of that principal's ARN ([`caller_arn`]), which for a long-term key
/// the account holds is the IAM user's name.
///
/// This is the identity a git commit records, so it is what a commit written with no
/// `name`/`authorName` and no `email` reads back as, for both author and committer.
pub(crate) fn caller_identity_name(access_key_id: &str) -> String {
    let arn = caller_arn(access_key_id);
    arn.rsplit_once('/')
        .map(|(_, name)| name.to_string())
        .unwrap_or(arn)
}

/// Make `access_key_id` a long-term access key the ACCOUNT HOLDS for an IAM user
/// named after it — the way `iam:CreateUser` and `iam:CreateAccessKey` record one —
/// and answer that user's ARN ([`caller_arn`]).
///
/// A long-term access key belongs to an IAM user, and the identity a request speaks
/// for is the one that key resolves to, so a suite that calls as two principals gives
/// each of them a key the account holds rather than expecting the service to invent an
/// identity for a key no store names: a key nothing holds speaks for the account's own
/// principal, which is what the credential this emulator publishes speaks for.
///
/// A suite whose principal has a NAME of its own — one not to be confused with the key
/// id — names it through [`hold_long_term_key_for`] instead; this is that function with
/// the key id as the name.
pub(crate) fn hold_long_term_key(engine: &engine::InMemoryEngine, access_key_id: &str) -> String {
    hold_long_term_key_for(engine, access_key_id, access_key_id)
}

/// The ONE place these suites record "the account holds `access_key_id` for the IAM user
/// `user_name`": the user record `iam:CreateUser` writes and the access-key record
/// `iam:CreateAccessKey` writes, in the one shared engine the resolution reads them from.
/// Answers that user's ARN.
pub(crate) fn hold_long_term_key_for(
    engine: &engine::InMemoryEngine,
    user_name: &str,
    access_key_id: &str,
) -> String {
    let arn = format!(
        "arn:aws:iam::{}:user/{user_name}",
        engine::emulator_account_id()
    );
    let state = iam::service_state(engine).expect("the in-memory engine carries the IAM store");
    let mut account = state.lock();
    account.users.insert(
        iam::name_key(user_name),
        User {
            path: "/".to_string(),
            user_name: user_name.to_string(),
            user_id: sts::unique_id("AIDA", &arn),
            arn: arn.clone(),
            create_date: ::aws_smithy_types::DateTime::from_secs(0),
            password_last_used: None,
            permissions_boundary: None,
            tags: None,
        },
    );
    account.access_keys.insert(
        access_key_id.to_string(),
        iam::AccessKeyRecord {
            user_key: iam::name_key(user_name),
            status: iam::ACCESS_KEY_ACTIVE_STATUS.to_string(),
            create_date: ::aws_smithy_types::DateTime::from_secs(0),
        },
    );
    arn
}

/// The shared harness's emulator, with a long-term key held for each caller in
/// `access_key_ids` — so each of them is a principal of the account with an identity
/// of its own, which is what a suite calling as more than one caller needs.
pub(crate) fn emulator_with_callers(
    access_key_ids: &[&str],
) -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let (router, engine) = crate::repository_lifecycle::emulator_with_engine();
    for access_key_id in access_key_ids {
        hold_long_term_key(&engine, access_key_id);
    }
    (router, engine)
}

/// The IAM user the git credentials the suites present belong to.
pub(crate) const GIT_CREDENTIAL_USER: &str = "GitCaller";

/// The `ServicePassword` the account minted with those credentials: the secret half of
/// the pair, which the git transport verifies against the verifier the account keeps of
/// it. A value of this harness's own — `CreateServiceSpecificCredential` generates one
/// no test can predict, so a suite that holds a credential ITSELF holds the verifier of
/// the password it will present.
pub(crate) const GIT_SERVICE_PASSWORD: &str = "harness-git-credential-password";

/// The `ServiceUserName` of those credentials: the generated user name
/// `CreateServiceSpecificCredential` reports for an IAM user, which is what a client
/// puts in a clone URL.
pub(crate) fn git_service_user_name() -> String {
    format!("{GIT_CREDENTIAL_USER}-at-{}", engine::emulator_account_id())
}

/// Record "the account holds an ACTIVE CodeCommit git credential for
/// [`GIT_CREDENTIAL_USER`], whose password is [`GIT_SERVICE_PASSWORD`], for a user that
/// may pull and push" — the records `iam:CreateUser`, `iam:PutUserPolicy` and
/// `iam:CreateServiceSpecificCredential` write — and answer its `ServiceUserName`.
///
/// The git transport serves only credentials the ACCOUNT ISSUED, so a suite driving it
/// holds a credential the same way the documented setup flow creates one: an IAM user,
/// a policy granting `codecommit:GitPull`/`codecommit:GitPush`, and a service-specific
/// credential for `codecommit.amazonaws.com` whose password the account keeps a verifier
/// of. One definition for every suite, so none of them invents a credential nothing
/// issued.
pub(crate) fn hold_git_credential(engine: &engine::InMemoryEngine) -> String {
    let service_user_name = git_service_user_name();
    hold_long_term_key_for(engine, GIT_CREDENTIAL_USER, GIT_CREDENTIAL_ACCESS_KEY);
    let state = iam::service_state(engine).expect("the in-memory engine carries the IAM store");
    let mut store = state.lock();
    let mut granted = std::collections::BTreeMap::new();
    granted.insert(
        iam::name_key(GIT_ACCESS_POLICY),
        iam::InlinePolicy {
            policy_name: GIT_ACCESS_POLICY.to_string(),
            policy_document: serde_json::json!({
                "Version": "2012-10-17",
                "Statement": [{
                    "Effect": "Allow",
                    "Action": ["codecommit:GitPull", "codecommit:GitPush"],
                    "Resource": "*",
                }],
            })
            .to_string(),
        },
    );
    store
        .user_inline_policies
        .insert(iam::name_key(GIT_CREDENTIAL_USER), granted);
    let held = store.hold_password(GIT_SERVICE_PASSWORD);
    store
        .service_specific_credential_secrets
        .insert(GIT_CREDENTIAL_ID.to_string(), held);
    store.service_specific_credentials.insert(
        GIT_CREDENTIAL_ID.to_string(),
        service_com_amazonaws_iam_awsidentitymanagementv20100508::ops::types::ServiceSpecificCredentialMetadata {
            user_name: GIT_CREDENTIAL_USER.to_string(),
            status: iam::CREDENTIAL_STATUS_ACTIVE.to_string(),
            service_user_name: Some(service_user_name.clone()),
            service_credential_alias: None,
            create_date: ::aws_smithy_types::DateTime::from_secs(0),
            expiration_date: None,
            service_specific_credential_id: GIT_CREDENTIAL_ID.to_string(),
            service_name: GIT_CREDENTIAL_SERVICE.to_string(),
        },
    );
    service_user_name
}

/// The access key id the harness holds for [`GIT_CREDENTIAL_USER`], so the git
/// credential belongs to a user of the account like any other principal.
const GIT_CREDENTIAL_ACCESS_KEY: &str = "AKIDGITCALLERKEY";

/// The `ServiceSpecificCredentialId` the account holds that credential under.
const GIT_CREDENTIAL_ID: &str = "ACCAHARNESSGITCREDENT";

/// The name of the inline policy that grants it the two git actions.
const GIT_ACCESS_POLICY: &str = "harness-git-access";

/// The service principal a CodeCommit git credential is created for.
const GIT_CREDENTIAL_SERVICE: &str = "codecommit.amazonaws.com";

/// The `Authorization` header a git client sends once it has been challenged: git
/// credentials as HTTP Basic ones, which is how a git client presents HTTPS git
/// credentials. `password` is the secret half it offers — the one the account minted
/// unless a suite is checking what a wrong one is answered with.
pub(crate) fn git_basic_credentials(user: &str, password: &str) -> String {
    use base64::Engine as _;
    format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"))
    )
}

/// A `clientRequestToken` no other call in this test target has used.
///
/// `CreatePullRequest` REQUIRES the member on the wire: an AWS SDK fills it in from
/// the model's `@idempotencyToken` before the request is sent, and a suite that
/// drives the wire itself fills it in itself. The token is also the idempotency key,
/// so two calls that are meant to create two pull requests carry two tokens — which
/// is what this counter guarantees, without any suite having to invent one.
///
/// A suite whose SUBJECT is the token (its bounds, its deduplication, or the refusal
/// of a request carrying none) names its own value instead.
pub(crate) fn fresh_client_request_token() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    format!(
        "idempotency-token-{}",
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

/// Send one awsJson1_1 request: `POST /` with `X-Amz-Target:
/// CodeCommit_20150413.<Operation>` (the crate's OPERATIONS.md "Wire protocol"),
/// signed with `access_key_id`, which is what the service reads a caller's
/// identity from.
pub(crate) async fn call_as(
    router: &MultiServiceRouter,
    access_key_id: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    call_as_in_region(router, access_key_id, REGION, operation, body).await
}

/// [`call_as`], addressed to `region`'s endpoint of this service.
///
/// Which Region a handler serves a request for is the Region of the SigV4
/// credential scope it was signed under, so a suite whose subject is a regional
/// value — the Region segment of an ARN the service reports, the Region a key is
/// held at — signs for the Region it means rather than building a second request
/// of its own. [`call_as`] is this bound to [`REGION`], so there is still exactly
/// one request builder.
pub(crate) async fn call_as_in_region(
    router: &MultiServiceRouter,
    access_key_id: &str,
    region: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://codecommit.{region}.amazonaws.com/"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("CodeCommit_20150413.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/{region}/codecommit/aws4_request, \
                 SignedHeaders=host;x-amz-target, Signature=0000"
            ),
        )
        .body(bytes::Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or_else(|error| {
            panic!(
                "operation {operation}: response body is not JSON ({error}): {:?}",
                String::from_utf8_lossy(response.body())
            )
        })
    };
    (status, payload)
}

/// A successful call by a named caller, with the status asserted.
pub(crate) async fn ok_as(
    router: &MultiServiceRouter,
    access_key_id: &str,
    operation: &str,
    body: &Value,
) -> Value {
    ok_as_in_region(router, access_key_id, REGION, operation, body).await
}

/// [`ok_as`], addressed to `region`'s endpoint ([`call_as_in_region`]).
pub(crate) async fn ok_as_in_region(
    router: &MultiServiceRouter,
    access_key_id: &str,
    region: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let (status, payload) = call_as_in_region(router, access_key_id, region, operation, body).await;
    assert_eq!(
        status, 200,
        "operation {operation} as {access_key_id} in {region}: expected 200, got {status} with \
         {payload}"
    );
    payload
}

/// The modeled error code of a failed call by a named caller, with the status
/// asserted. awsJson carries it in `__type`, optionally namespace-qualified.
pub(crate) async fn error_as(
    router: &MultiServiceRouter,
    access_key_id: &str,
    operation: &str,
    body: &Value,
    expected_status: u16,
) -> String {
    refusal_as(router, access_key_id, operation, body, expected_status)
        .await
        .0
}

/// The modeled error code AND message of a failed call by a named caller, with
/// the status asserted: the same parse as [`error_as`], for a suite that asserts
/// what the error SAYS as well as which one it is. awsJson carries the code in
/// `__type` (optionally namespace-qualified) and the message in `message`.
///
/// A body that carries NO message text reads as the empty string, which is what an
/// AWS SDK's parsed `Error.Message` holds for such a response — so a suite can
/// assert the absence of a message as well as its text.
pub(crate) async fn refusal_as(
    router: &MultiServiceRouter,
    access_key_id: &str,
    operation: &str,
    body: &Value,
    expected_status: u16,
) -> (String, String) {
    let (status, payload) = call_as(router, access_key_id, operation, body).await;
    assert_eq!(
        status, expected_status,
        "operation {operation} as {access_key_id}: expected HTTP {expected_status}, got {status} \
         with {payload}"
    );
    let code = payload
        .get("__type")
        .and_then(Value::as_str)
        .map(|shape| shape.rsplit('#').next().unwrap_or(shape).to_string())
        .unwrap_or_else(|| panic!("operation {operation}: no __type in {payload}"));
    let message = payload
        .get("message")
        .or_else(|| payload.get("Message"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    (code, message)
}
