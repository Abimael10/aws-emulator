// SPDX-License-Identifier: CC0-1.0

//! What deleting a ROLE does to the temporary credentials already issued for it, on an
//! emulator that VERIFIES request signatures.
//!
//! The rule under test: **deleting a role invalidates the temporary credentials issued for
//! it immediately, and re-creating a role of the same name does not restore them.** "Deleting
//! a role or instance profile that is associated with a running instance will break any
//! applications running on the instance" (`DeleteRole`, IAM's model) is the operation's own
//! warning about exactly that: the role is the principal those credentials speak for, so once
//! it is gone they speak for nobody and no service is to serve a call made with one. A role
//! is identified by its id — "the stable and unique string identifying the role"
//! (`Role$RoleId`) — so a role created again under the same name is another principal behind
//! the same ARN, and the credentials of the first one stay withdrawn.
//!
//! Withdrawn where it counts: the credential itself is given up, so the EMULATOR's own
//! authentication layer stops accepting it whichever service it is presented to, rather than
//! each service having to notice that the role behind it is gone. That is why this suite
//! drives a router with request authentication ON and signs every call: the refusal a
//! withdrawn credential earns is the service's unknown-credential answer (IAM's
//! `InvalidClientTokenId`, HTTP 403), not an authorization denial — and a client tells the
//! two apart to decide whether to refresh its credentials or to fix a policy.
//!
//! The stages differ along what has happened to the role since the session was minted: the
//! role still there (the positive control), the role deleted, and a role of the same name
//! created again. The last stage mints a FRESH session of the re-created role, because what
//! the delete withdraws is the credentials it issued and not the role's access.
//!
//! Both services on one shared engine: IAM holds the role, its documents and the record of
//! the session, STS mints the credential, and the emulator's credential store is what the
//! signature is verified against.

use std::sync::Arc;

use emulator_server::request_authentication::SigV4Authenticator;
use service_com_amazonaws_iam_awsidentitymanagementv20100508 as iam;
use service_com_amazonaws_sts_awssecuritytokenservicev20110615 as sts;
use smithy_server_runtime::MultiServiceRouter;

use super::sts_sessions::{element, emulator_account, form_value, member};

/// The Region every call below is signed for.
const REGION: &str = "us-east-1";

/// The role every stage is about, and the session name its credentials carry.
const ROLE_NAME: &str = "WithdrawnRole";
const SESSION_NAME: &str = "withdrawal";

/// The IAM operation the session calls, and the action it is authorized under.
const LIST_USERS: &str = "ListUsers";

/// What IAM answers a request whose access key id resolves to no credential this emulator
/// holds, and the fixed sentence that code carries
/// (`::service_common::authentication_refusal::UNRESOLVED_CREDENTIAL_MESSAGE`).
const UNKNOWN_CREDENTIAL_CODE: &str = "InvalidClientTokenId";
const UNKNOWN_CREDENTIAL_MESSAGE: &str = "The security token included in the request is invalid.";

/// The credential one call is signed with: the key pair this process publishes, or a session
/// a mint handed back — which is that pair plus the session token travelling with it.
#[derive(Clone)]
struct Signer {
    access_key_id: String,
    secret_access_key: String,
    session_token: Option<String>,
}

impl Signer {
    /// The key pair the router publishes once request authentication is on, which the setup
    /// calls are made with: it is the account's own credential.
    fn published() -> Self {
        let minted = SigV4Authenticator::minted_credentials()
            .expect("turning request authentication on publishes this process's keys");
        Self {
            access_key_id: minted.access_key_id.clone(),
            secret_access_key: minted.secret_access_key.clone(),
            session_token: None,
        }
    }
}

/// One awsQuery call to `signing_name`, SIGNED with `signer` the way a client signs it: the
/// shared in-process signer writes the `Authorization` header and the `x-amz-date` the
/// emulator's verifier re-signs over, and a session credential carries its token beside them.
async fn signed_query(
    router: &MultiServiceRouter,
    signer: &Signer,
    signing_name: &str,
    version: &str,
    action: &str,
    params: &[(&str, String)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={version}");
    for (name, value) in params {
        body.push('&');
        body.push_str(name);
        body.push('=');
        body.push_str(&form_value(value));
    }
    let mut builder = http::Request::builder()
        .method("POST")
        .uri(format!("https://{signing_name}.amazonaws.com/"))
        .header("host", format!("{signing_name}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded");
    if let Some(token) = &signer.session_token {
        builder = builder.header("x-amz-security-token", token.as_str());
    }
    let mut request = builder
        .body(body.into_bytes())
        .expect("a well-formed awsQuery request");
    ::service_common::in_process_sigv4::sign_in_process_request_as(
        &mut request,
        signing_name,
        REGION,
        &signer.access_key_id,
        &signer.secret_access_key,
    )
    .await
    .expect("the request signs");
    let (parts, payload) = request.into_parts();
    let response = router
        .dispatch(http::Request::from_parts(parts, bytes::Bytes::from(payload)))
        .await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One signed IAM call.
async fn iam_call(
    router: &MultiServiceRouter,
    signer: &Signer,
    action: &str,
    params: &[(&str, String)],
) -> (u16, String) {
    signed_query(router, signer, "iam", "2010-05-08", action, params).await
}

/// One signed STS call.
async fn sts_call(
    router: &MultiServiceRouter,
    signer: &Signer,
    action: &str,
    params: &[(&str, String)],
) -> (u16, String) {
    signed_query(router, signer, "sts", "2011-06-15", action, params).await
}

/// Create the role, trusted by the account's root and allowed to list the account's users,
/// and answer its `RoleId`.
async fn given_role(router: &MultiServiceRouter, signer: &Signer, case: &str) -> String {
    let account = emulator_account();
    let trust = format!(
        r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow","Principal":{{"AWS":"arn:aws:iam::{account}:root"}},"Action":"sts:AssumeRole"}}]}}"#
    );
    let (status, body) = iam_call(
        router,
        signer,
        "CreateRole",
        &[
            ("RoleName", ROLE_NAME.to_string()),
            ("AssumeRolePolicyDocument", trust),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateRole must succeed, got {status}: {body}"
    );
    let role_id = member(case, &body, "RoleId");
    let (status, body) = iam_call(
        router,
        signer,
        "PutRolePolicy",
        &[
            ("RoleName", ROLE_NAME.to_string()),
            ("PolicyName", "may-list".to_string()),
            (
                "PolicyDocument",
                r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"iam:*","Resource":"*"}]}"#
                    .to_string(),
            ),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: PutRolePolicy must succeed, got {status}: {body}"
    );
    role_id
}

/// Assume the role as `signer` and answer a signer holding the session's own credentials.
async fn assumed_session(router: &MultiServiceRouter, signer: &Signer, case: &str) -> Signer {
    let role_arn = format!("arn:aws:iam::{}:role/{ROLE_NAME}", emulator_account());
    let (status, body) = sts_call(
        router,
        signer,
        "AssumeRole",
        &[
            ("RoleArn", role_arn),
            ("RoleSessionName", SESSION_NAME.to_string()),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: AssumeRole must succeed, got {status}: {body}"
    );
    Signer {
        access_key_id: member(case, &body, "AccessKeyId"),
        secret_access_key: member(case, &body, "SecretAccessKey"),
        session_token: Some(member(case, &body, "SessionToken")),
    }
}

/// RULE: deleting a role gives up every temporary credential issued for it, so a call made
/// with one is refused by the emulator's authentication layer — and creating a role of the
/// same name does not revive it, while a session of the NEW role is served normally.
#[tokio::test]
async fn deleting_a_role_withdraws_the_credentials_it_issued() {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(true);
    router.register_service(iam::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(sts::EmulatedService::new(Arc::clone(&engine)));
    let account = Signer::published();

    let case = "a session of a role that is then deleted";
    let first_id = given_role(&router, &account, case).await;
    let session = assumed_session(&router, &account, case).await;

    // The positive control: while the role is there, the session's own credential
    // authenticates and the role's document allows the call.
    let (status, body) = iam_call(&router, &session, LIST_USERS, &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the session must be served while the role exists, got {status}: {body}"
    );

    let (status, body) = iam_call(
        &router,
        &account,
        "DeleteRolePolicy",
        &[
            ("RoleName", ROLE_NAME.to_string()),
            ("PolicyName", "may-list".to_string()),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DeleteRolePolicy must succeed, got {status}: {body}"
    );
    let (status, body) = iam_call(
        &router,
        &account,
        "DeleteRole",
        &[("RoleName", ROLE_NAME.to_string())],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DeleteRole must succeed, got {status}: {body}"
    );

    for stage in ["the role is deleted", "a role of the same name is created again"] {
        let (status, body) = iam_call(&router, &session, LIST_USERS, &[]).await;
        assert_eq!(
            status, 403,
            "case {stage:?}: the withdrawn credential must be refused, got {status}: {body}"
        );
        let code = element(&body, "Code").unwrap_or_default();
        assert_eq!(
            code, UNKNOWN_CREDENTIAL_CODE,
            "case {stage:?}: the credential must be refused at authentication, got {code:?}: \
             {body}"
        );
        let message = element(&body, "Message").unwrap_or_default();
        assert_eq!(
            message, UNKNOWN_CREDENTIAL_MESSAGE,
            "case {stage:?}: expected the fixed sentence of that code, got {message:?}"
        );
        if stage == "the role is deleted" {
            let second_id = given_role(&router, &account, case).await;
            assert_ne!(
                second_id, first_id,
                "case {case:?}: a role created again must carry a new RoleId, got {second_id:?}"
            );
        }
    }

    // And a session of the RE-CREATED role is served: what the delete withdrew is the
    // credentials it had issued, not the role's access.
    let fresh = assumed_session(&router, &account, case).await;
    let (status, body) = iam_call(&router, &fresh, LIST_USERS, &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: a session of the re-created role must be served, got {status}: {body}"
    );
}
