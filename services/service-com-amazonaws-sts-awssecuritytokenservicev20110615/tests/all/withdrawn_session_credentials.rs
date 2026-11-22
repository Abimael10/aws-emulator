// SPDX-License-Identifier: CC0-1.0

//! What a session credential this emulator minted still ANSWERS once the role behind it is
//! gone, on an emulator that VERIFIES request signatures.
//!
//! The rule under test: **a credential this emulator minted authenticates every request it
//! signs until its own expiry, whatever has happened to the principal behind it — so a call
//! that needs no principal resolved is SERVED, and a call that needs one is refused at
//! credential resolution, never as a signature failure.** `GetCallerIdentity` "returns
//! details about the IAM user or role whose credentials are used to call the operation. No
//! permissions are required to perform this operation" (this service's model, which declares
//! no error for it at all), so it answers from the credential itself: HTTP 200 with the
//! session's `Arn`, its `UserId` and the caller's `Account`. A signature failure is reserved
//! for a request whose computed signature genuinely differs from the one presented, which is
//! why it must never be the answer to a request signed correctly with a credential the
//! emulator itself vended: that code tells an operator their signer, secret or clock is
//! wrong, and no client retries or re-signs around it.
//!
//! The two halves of the answer, for the same credential:
//!
//! * `sts:GetCallerIdentity` reports the identity the session carries — the assumed-role ARN
//!   `arn:aws:sts::<account>:assumed-role/<role name>/<session name>`, with the role named
//!   WITHOUT the path it is held under ("the ARN of the temporary security credentials that
//!   are returned from the AssumeRole action", `AssumedRoleUser$Arn`), and the `UserId`
//!   `<RoleId>:<session name>` built from the id IAM issued for the role; and
//! * IAM, which has to resolve the principal whose policies judge the call, refuses it with
//!   its unknown-credential code (`InvalidClientTokenId`, HTTP 403) — the class a client's
//!   credential-refresh path keys on.
//!
//! The rows differ along what the rule generalises over: the SHAPE of the session (a session
//! of a role at the default path, of a role under a nested path, and a CHAINED session — the
//! session of a role assumed from another session), and the STAGE the call is made at (while
//! the role is still there, and after it has been deleted). Each row's identity is read back
//! before and after the delete, so the served answer is the same one either side of it.
//!
//! Both services on one shared engine, and every call SIGNED through the shared in-process
//! signer: IAM holds the roles and their documents, STS mints each session, and the
//! emulator's credential store is what the signature is verified against — so a credential
//! whose material had been given up would show here as the signature failure this rule
//! forbids.

use std::sync::Arc;

use service_com_amazonaws_iam_awsidentitymanagementv20100508 as iam;
use service_com_amazonaws_sts_awssecuritytokenservicev20110615 as sts;
use service_common::authentication_refusal::{
    INVALID_SECURITY_TOKEN_MESSAGE, SIGNATURE_MISMATCH_MESSAGE,
};
use smithy_server_runtime::MultiServiceRouter;

use super::role_deletion_withdraws_its_credentials::{iam_call, sts_call, Signer};
use super::sts_sessions::{element, emulator_account, member};

/// What IAM answers a request whose credential resolves to no principal this account holds,
/// and the one code that must never be the answer to a correctly signed request.
const UNKNOWN_CREDENTIAL_CODE: &str = "InvalidClientTokenId";
const SIGNATURE_MISMATCH_CODE: &str = "SignatureDoesNotMatch";

/// The IAM operation each row's session calls: the one that forces IAM to resolve the
/// principal whose policies judge the call.
const LIST_USERS: &str = "ListUsers";

/// A document allowing every IAM and STS action, so nothing a row observes is decided by a
/// missing grant.
const ALLOW_EVERYTHING: &str =
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["iam:*","sts:*"],"Resource":"*"}]}"#;

/// One role of this account, trusted by `principal` and allowed every call, created through
/// IAM's own operations: its ARN and the `RoleId` IAM issued for it.
async fn given_role(
    router: &MultiServiceRouter,
    signer: &Signer,
    case: &str,
    role_name: &str,
    path: &str,
    principal: &str,
) -> (String, String) {
    let trust = format!(
        r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow","Principal":{{"AWS":"{principal}"}},"Action":"sts:AssumeRole"}}]}}"#
    );
    let (status, body) = iam_call(
        router,
        signer,
        "CreateRole",
        &[
            ("RoleName", role_name.to_string()),
            ("Path", path.to_string()),
            ("AssumeRolePolicyDocument", trust),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateRole({role_name:?}) must succeed, got {status}: {body}"
    );
    let (arn, role_id) = (member(case, &body, "Arn"), member(case, &body, "RoleId"));
    let (status, body) = iam_call(
        router,
        signer,
        "PutRolePolicy",
        &[
            ("RoleName", role_name.to_string()),
            ("PolicyName", "granted".to_string()),
            ("PolicyDocument", ALLOW_EVERYTHING.to_string()),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: PutRolePolicy must succeed, got {status}: {body}"
    );
    (arn, role_id)
}

/// Assume `role_arn` as `signer` under `session_name`, and answer a signer holding the
/// session's own credentials.
async fn assumed(
    router: &MultiServiceRouter,
    signer: &Signer,
    case: &str,
    role_arn: &str,
    session_name: &str,
) -> Signer {
    let (status, body) = sts_call(
        router,
        signer,
        "AssumeRole",
        &[
            ("RoleArn", role_arn.to_string()),
            ("RoleSessionName", session_name.to_string()),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: AssumeRole({role_arn:?}) must succeed, got {status}: {body}"
    );
    Signer {
        access_key_id: member(case, &body, "AccessKeyId"),
        secret_access_key: member(case, &body, "SecretAccessKey"),
        session_token: Some(member(case, &body, "SessionToken")),
    }
}

/// `GetCallerIdentity` signed with `signer`, asserted SERVED, and the identity it reports.
async fn served_identity(
    router: &MultiServiceRouter,
    signer: &Signer,
    case: &str,
    stage: &str,
) -> (String, String, String) {
    let (status, body) = sts_call(router, signer, "GetCallerIdentity", &[]).await;
    let code = element(&body, "Code").unwrap_or_default();
    assert_ne!(
        code, SIGNATURE_MISMATCH_CODE,
        "case {case:?} ({stage}): a credential this emulator minted must never be answered \
         as a signature failure, got {body}"
    );
    assert_eq!(
        status, 200,
        "case {case:?} ({stage}): GetCallerIdentity must be served, got {status}: {body}"
    );
    (
        member(case, &body, "Arn"),
        member(case, &body, "UserId"),
        member(case, &body, "Account"),
    )
}

/// One row: how the session is minted, and the identity `GetCallerIdentity` must report for
/// it either side of the delete.
struct Row {
    case: &'static str,
    /// The role the session is OF, and the path IAM holds it under.
    role_name: &'static str,
    path: &'static str,
    /// Set for a CHAINED session: the session is minted from a session of this other role,
    /// which is itself assumed by the account.
    chained_from: Option<(&'static str, &'static str)>,
    session_name: &'static str,
}

/// RULE: a session credential this emulator minted is still authentic after the role behind
/// it is deleted, so `GetCallerIdentity` answers for it with the identity it carries, while
/// IAM — which must resolve the principal whose policies judge the call — refuses it at
/// credential resolution and never as a signature mismatch.
#[tokio::test]
async fn a_session_whose_role_is_deleted_still_authenticates_and_reports_its_identity() {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(true);
    router.register_service(iam::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(sts::EmulatedService::new(Arc::clone(&engine)));
    let account = Signer::published();
    let account_id = emulator_account();
    let root = format!("arn:aws:iam::{account_id}:root");

    let rows = vec![
        Row {
            case: "a session of a role at the default path",
            role_name: "PlainWithdrawn",
            path: "/",
            chained_from: None,
            session_name: "plain",
        },
        Row {
            case: "a session of a role under a nested path",
            role_name: "PathedWithdrawn",
            path: "/teams/platform/",
            chained_from: None,
            session_name: "pathed",
        },
        Row {
            case: "a chained session: the session of a role assumed from another session",
            role_name: "ChainedWithdrawn",
            path: "/",
            chained_from: Some(("ChainingWithdrawn", "chaining")),
            session_name: "chained",
        },
    ];

    for Row {
        case,
        role_name,
        path,
        chained_from,
        session_name,
    } in rows
    {
        // The session is minted by whoever is allowed to assume the role: the account for a
        // plain session, and a session of the FIRST role for a chained one — whose role
        // therefore trusts that first role's sessions.
        let (caller, trusted_principal) = match chained_from {
            None => (account.clone(), root.clone()),
            Some((first_role, first_session)) => {
                let (first_arn, _) =
                    given_role(&router, &account, case, first_role, "/", &root).await;
                let first = assumed(&router, &account, case, &first_arn, first_session).await;
                (
                    first,
                    format!("arn:aws:iam::{account_id}:role/{first_role}"),
                )
            }
        };
        let (role_arn, role_id) =
            given_role(&router, &account, case, role_name, path, &trusted_principal).await;
        let session = assumed(&router, &caller, case, &role_arn, session_name).await;

        // What the credential carries, which is what the identity must read as at BOTH
        // stages: the session's ARN names the role WITHOUT the path it is held under.
        let expected_arn =
            format!("arn:aws:sts::{account_id}:assumed-role/{role_name}/{session_name}");
        let expected_user_id = format!("{role_id}:{session_name}");

        // The positive control: while the role is there, the session reports its identity
        // and IAM serves the call its policies allow.
        let identity = served_identity(&router, &session, case, "the role still exists").await;
        assert_eq!(
            identity,
            (
                expected_arn.clone(),
                expected_user_id.clone(),
                account_id.clone()
            ),
            "case {case:?}: the session must report the identity it was minted for"
        );
        let (status, body) = iam_call(&router, &session, LIST_USERS, &[]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the session must be served while the role exists, got {status}: \
             {body}"
        );

        let (status, body) = iam_call(
            &router,
            &account,
            "DeleteRolePolicy",
            &[
                ("RoleName", role_name.to_string()),
                ("PolicyName", "granted".to_string()),
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
            &[("RoleName", role_name.to_string())],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: DeleteRole must succeed, got {status}: {body}"
        );

        // The identity is still the session's own: nothing about the credential changed, and
        // this call needs no principal resolved.
        let identity = served_identity(&router, &session, case, "the role is deleted").await;
        assert_eq!(
            identity,
            (expected_arn, expected_user_id, account_id.clone()),
            "case {case:?}: the deleted role's session must report the same identity"
        );

        // And the call that DOES need a principal is refused at credential resolution, in
        // the class and the sentence AWS's front door answers an unknown credential with —
        // this request presented a security token, so it is that stage's sentence.
        let (status, body) = iam_call(&router, &session, LIST_USERS, &[]).await;
        assert_eq!(
            status, 403,
            "case {case:?}: the withdrawn credential must be refused, got {status}: {body}"
        );
        let code = element(&body, "Code").unwrap_or_default();
        let message = element(&body, "Message").unwrap_or_default();
        assert_eq!(
            (code.as_str(), message.as_str()),
            (UNKNOWN_CREDENTIAL_CODE, INVALID_SECURITY_TOKEN_MESSAGE),
            "case {case:?}: expected the unknown-credential refusal, got {code:?}: {message:?}"
        );
        assert_ne!(
            message, SIGNATURE_MISMATCH_MESSAGE,
            "case {case:?}: a withdrawn credential is not a signature failure, got {body}"
        );
    }
}
