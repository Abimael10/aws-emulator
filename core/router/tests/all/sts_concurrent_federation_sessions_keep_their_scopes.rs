// SPDX-License-Identifier: CC0-1.0

//! Federation sessions minted AT THE SAME INSTANT each keep the session policy its own mint
//! passed, at every service that accepts the credential.
//!
//! RULE under test: **the session policies one mint passed scope exactly the credential that
//! mint vended** — "when you pass session policies, the session permissions are the
//! intersection of the IAM user policies and the session policies that you pass"
//! (`GetFederationTokenRequest$Policy`, the STS model), where "the session" is the one being
//! minted. So a scope is neither shared with nor overwritten by a sibling mint, however many
//! mints are in flight together: an action a session's OWN scope allows is served for that
//! credential and refused for a sibling that was not granted it, and every service handed the
//! credential reads the same scope. A refusal whose deciding boundary is the scope says so —
//! `because no session policy allows the <action> action` — and names the federated user of
//! the credential presented, never a sibling's.
//!
//! The rows span what the rule generalises over: the FORM of the scope's grant (one action by
//! name, a whole service by wildcard, two actions at once, no session policy at all), WHICH
//! service's gate the grant belongs to (Amazon S3 and AWS Secrets Manager, two gates of their
//! own, each asked for an action that addresses no resource of its own), and whether a session
//! is granted one service, both or neither. Six mints run together, each on its own task of a
//! multi-threaded runtime, as many as the recorded federation flow issues at once.
//!
//! The intersection rule itself — both halves of it, and the resource-scoped decision site of
//! each service — is pinned by [`super::sts_federation_session_policy_intersection`], whose
//! harness this suite drives rather than standing up a second one.

use std::sync::Arc;

use serde_json::json;
use smithy_server_runtime::MultiServiceRouter;

use crate::sts_federation_session_policy_intersection::{
    build_router, given_federation_session, given_secret, json_error_code, json_error_message,
    no_session_grant, not_authorized_account_wide, secrets_manager, LIST_ALL_MY_BUCKETS,
    LIST_SECRETS,
};
use crate::sts_session_credential_refusals::{code_of, message_of, s3, seed_bucket, AMBIENT_KEY};

/// The bucket the account's own credential seeds, so the bucket listing a granted session is
/// served has something in it.
const BUCKET: &str = "concurrent-federation-subject";

/// The code Amazon S3 answers an authorization refusal with, and the status it carries
/// (Amazon S3 "Error responses": `AccessDenied`, HTTP 403).
const S3_ACCESS_DENIED: &str = "AccessDenied";
const S3_ACCESS_DENIED_STATUS: u16 = 403;

/// The code AWS Secrets Manager answers one with: the AWS common `AccessDeniedException`,
/// which its awsJson1_1 wire form carries in the body's `__type`.
const JSON_ACCESS_DENIED: &str = "AccessDeniedException";

/// The status that refusal travels with: awsJson1_1 discriminates a failure by the error type
/// in the body rather than by the transport status, so a denial carries the ordinary
/// client-error status.
const JSON_ACCESS_DENIED_STATUS: u16 = 400;

/// One minted federation session: the access key id its requests are signed with, and the ARN
/// a refusal of them names as the principal.
struct Session {
    access_key_id: String,
    federated_user_arn: String,
}

impl From<(String, String)> for Session {
    /// What the shared harness answers for one mint.
    fn from((access_key_id, federated_user_arn): (String, String)) -> Self {
        Self {
            access_key_id,
            federated_user_arn,
        }
    }
}

/// One mint of the batch: the scope it passes, and what that scope leaves each service's
/// account-wide listing answering.
struct Mint {
    case: &'static str,
    /// The `Name` the mint asks for, which is what the federated-user ARN ends in.
    name: &'static str,
    /// The `Action` elements this mint's inline session policy allows over every resource, or
    /// an empty slice for a mint that passes no session policy at all.
    allows: &'static [&'static str],
    /// Whether `s3:ListAllMyBuckets` is inside this session's permissions.
    buckets: bool,
    /// Whether `secretsmanager:ListSecrets` is.
    secrets: bool,
}

/// The batch every test here mints: six scopes, spanning the forms a grant takes and which
/// service each grant belongs to.
fn batch() -> [Mint; 6] {
    [
        Mint {
            case: "a scope granting the bucket listing by name",
            name: "fed-buckets-named",
            allows: &[LIST_ALL_MY_BUCKETS],
            buckets: true,
            secrets: false,
        },
        Mint {
            case: "a scope granting the secret listing by name",
            name: "fed-secrets-named",
            allows: &[LIST_SECRETS],
            buckets: false,
            secrets: true,
        },
        Mint {
            case: "a scope granting the whole of one service",
            name: "fed-buckets-wildcard",
            allows: &["s3:*"],
            buckets: true,
            secrets: false,
        },
        Mint {
            case: "a scope granting the whole of the other service",
            name: "fed-secrets-wildcard",
            allows: &["secretsmanager:*"],
            buckets: false,
            secrets: true,
        },
        Mint {
            case: "a scope granting both listings at once",
            name: "fed-both-listings",
            allows: &[LIST_ALL_MY_BUCKETS, LIST_SECRETS],
            buckets: true,
            secrets: true,
        },
        Mint {
            case: "a mint passing no session policy at all",
            name: "fed-unscoped",
            allows: &[],
            buckets: false,
            secrets: false,
        },
    ]
}

/// Assert what `session` may do at Amazon S3's gate: its account-wide bucket listing is served
/// when its OWN scope grants that action, and refused with the session-policy clause otherwise.
async fn assert_bucket_listing(router: &MultiServiceRouter, mint: &Mint, session: &Session) {
    let (status, body) = s3(router, &session.access_key_id, "GET", "/", "").await;
    if mint.buckets {
        assert_eq!(
            status, 200,
            "case {:?}: this session's own scope grants {LIST_ALL_MY_BUCKETS}, so its bucket \
             listing must be served, got {status}: {body}",
            mint.case
        );
        assert!(
            body.contains("<Buckets>"),
            "case {:?}: expected a Buckets container in the listing, got {body:?}",
            mint.case
        );
        return;
    }
    assert_eq!(
        status, S3_ACCESS_DENIED_STATUS,
        "case {:?}: this session's own scope does not grant {LIST_ALL_MY_BUCKETS}, so its \
         bucket listing must be refused, got {status}: {body}",
        mint.case
    );
    let code = code_of(&body);
    assert_eq!(
        code, S3_ACCESS_DENIED,
        "case {:?}: expected {S3_ACCESS_DENIED}, got {code:?}: {body}",
        mint.case
    );
    let message = message_of(&body);
    assert_eq!(
        message,
        not_authorized_account_wide(
            &session.federated_user_arn,
            LIST_ALL_MY_BUCKETS,
            &no_session_grant(LIST_ALL_MY_BUCKETS)
        ),
        "case {:?}: expected the refusal to blame this session's own scope and name its own \
         federated user, got {message:?}",
        mint.case
    );
}

/// The same, at AWS Secrets Manager's gate — the other service handed the same credential.
async fn assert_secret_listing(router: &MultiServiceRouter, mint: &Mint, session: &Session) {
    let (status, answer) =
        secrets_manager(router, &session.access_key_id, "ListSecrets", &json!({})).await;
    if mint.secrets {
        assert_eq!(
            status, 200,
            "case {:?}: this session's own scope grants {LIST_SECRETS}, so its secret listing \
             must be served, got {status}: {answer}",
            mint.case
        );
        assert!(
            answer.get("SecretList").is_some(),
            "case {:?}: expected a SecretList in the listing, got {answer}",
            mint.case
        );
        return;
    }
    let code = json_error_code(&answer);
    assert_eq!(
        (status, code.as_str()),
        (JSON_ACCESS_DENIED_STATUS, JSON_ACCESS_DENIED),
        "case {:?}: this session's own scope does not grant {LIST_SECRETS}, so its secret \
         listing must be refused, got {status} {code:?}: {answer}",
        mint.case
    );
    let message = json_error_message(&answer);
    assert_eq!(
        message,
        not_authorized_account_wide(
            &session.federated_user_arn,
            LIST_SECRETS,
            &no_session_grant(LIST_SECRETS)
        ),
        "case {:?}: expected the refusal to blame this session's own scope and name its own \
         federated user, got {message:?}",
        mint.case
    );
}

/// An emulator holding the four services this suite exercises, seeded with the bucket and the
/// secret the listings report, shared by every task that mints.
async fn seeded_emulator(case: &str) -> Arc<MultiServiceRouter> {
    let router = Arc::new(build_router());
    seed_bucket(&router, case, BUCKET).await;
    given_secret(&router, case).await;
    router
}

/// RULE: six mints running together vend six credentials, each scoped by the session policy
/// ITS OWN mint passed — a sibling's grant neither leaks in nor withholds one — and both
/// services handed a credential read that one scope.
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn concurrent_federation_mints_each_keep_their_own_scope() {
    let batch = batch();
    let router = seeded_emulator(batch[0].case).await;

    // Every mint of the batch runs on its own task, so the six are in flight at the same
    // instant — which is what would let one mint's scope reach another's credential.
    let mut minting = Vec::with_capacity(batch.len());
    for mint in &batch {
        let router = Arc::clone(&router);
        let (case, name, allows) = (mint.case, mint.name, mint.allows);
        minting.push(tokio::spawn(async move {
            given_federation_session(&router, case, AMBIENT_KEY, name, allows).await
        }));
    }
    let mut sessions: Vec<Session> = Vec::with_capacity(batch.len());
    for task in minting {
        sessions.push(Session::from(task.await.expect("the mint answered")));
    }

    // Six mints are six credentials: a scope that decided a sibling's request would be reading
    // a credential it was not minted for.
    for (index, session) in sessions.iter().enumerate() {
        for (other, sibling) in sessions.iter().enumerate() {
            if index == other {
                continue;
            }
            assert_ne!(
                session.access_key_id, sibling.access_key_id,
                "case {:?}: expected a credential of its own, got the one {:?} holds",
                batch[index].case, batch[other].case
            );
        }
    }

    for (mint, session) in batch.iter().zip(sessions.iter()) {
        assert_bucket_listing(&router, mint, session).await;
        assert_secret_listing(&router, mint, session).await;
    }
}

/// RULE: a scope belongs to the credential its mint vended, so a LATER mint — one passing a
/// scope that grants something else, or none at all — leaves an earlier session answering
/// exactly as it did before.
#[tokio::test]
async fn a_later_mint_does_not_rescope_an_earlier_session() {
    let batch = batch();
    let router = seeded_emulator(batch[0].case).await;

    let mut sessions: Vec<Session> = Vec::new();
    for mint in &batch {
        sessions.push(Session::from(
            given_federation_session(&router, mint.case, AMBIENT_KEY, mint.name, mint.allows).await,
        ));
        // After each further mint, EVERY session already vended still answers by its own scope.
        for (earlier, session) in batch.iter().zip(sessions.iter()) {
            assert_bucket_listing(&router, earlier, session).await;
            assert_secret_listing(&router, earlier, session).await;
        }
    }
}
