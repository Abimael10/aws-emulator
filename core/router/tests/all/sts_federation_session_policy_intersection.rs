// SPDX-License-Identifier: CC0-1.0

//! What a federated-user session credential MAY do, at every service that accepts it: the
//! intersection of the documents in force on the IAM identity whose long-term key minted the
//! session and the session policies that mint passed.
//!
//! "When you pass session policies, the session permissions are the intersection of the IAM
//! user policies and the session policies that you pass" and "though the session policy
//! parameters are optional, if you do not pass a policy, then the resulting federated user
//! session has no permissions" (`GetFederationTokenRequest$Policy`, the STS model). So an
//! action BOTH halves allow is served, and an action either half withholds is refused 403
//! `AccessDenied` naming the half that withheld it: the session-policy clause when the scope
//! is the deciding boundary, the identity clause only when nothing identity-based granted the
//! action. The federated user is no IAM identity and carries no document of its own, which is
//! why the documents in force are the MINTING identity's.
//!
//! The rows span what the rule generalises over: the FORM of the session policy's grant (the
//! exact action, the service wildcard, the all-action wildcard, another service's action,
//! none at all), WHICH identity minted the session (the account's own credential, which no
//! document bounds, and an IAM user carrying documents of its own), WHICH service decides
//! (Amazon S3 and AWS Secrets Manager, two gates of their own), and WHICH decision site
//! answers inside each — an action that addresses no resource of its own
//! (`s3:ListAllMyBuckets`, `secretsmanager:ListSecrets`) and a resource-scoped one
//! (`s3:GetObject`, `secretsmanager:GetSecretValue`).

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::element;
use crate::sts_session_credential_refusals::{
    code_of, document, get_object, message_of, object_arn, s3, seed_bucket, statement, sts,
    user_with_key, ACCOUNT, AMBIENT_KEY, ASCII_KEY, GET_OBJECT,
};

/// The IAM action the account's own bucket listing is authorized under, as a refusal names it
/// ("to use this operation, you must add the `s3:ListAllMyBuckets` policy action",
/// `ListBuckets`).
pub(crate) const LIST_ALL_MY_BUCKETS: &str = "s3:ListAllMyBuckets";

/// The Secrets Manager actions this suite drives: the account-wide listing, which addresses
/// no secret, and the read of one secret.
///
/// `pub(crate)`, like the harness below it: the suite that pins what CONCURRENTLY minted
/// sessions may do ([`super::sts_concurrent_federation_sessions_keep_their_scopes`]) drives
/// the same two gates through these rather than spelling a second harness for them.
pub(crate) const LIST_SECRETS: &str = "secretsmanager:ListSecrets";
const GET_SECRET_VALUE: &str = "secretsmanager:GetSecretValue";

/// The name of the secret every row reads, created by the account's own credential.
const SECRET_NAME: &str = "federation/secret";

/// The `ClientRequestToken` the create of that secret stakes its first version under: a
/// version-creating write must carry one ("if you don't include this value, then Secrets
/// Manager generates a random UUID for you", `CreateSecretRequest$ClientRequestToken`, whose
/// modelled length is 32..64), and this suite spells one so the create is reproducible.
const SECRET_VERSION_TOKEN: &str = "11111111-2222-3333-4444-555555555555";

/// The Region every request here is signed for.
const REGION: &str = "us-east-1";

/// The four services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them: IAM holds the users and their documents, STS mints
/// the federated session, and Amazon S3 and Secrets Manager each decide a request signed with
/// it at their own gate.
pub(crate) fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_secretsmanager_secretsmanager::EmulatedService::new(engine),
    );
    router
}

/// Call one Secrets Manager operation, presented as signed by `access_key_id`, over this
/// service's awsJson1_1 wire form.
pub(crate) async fn secrets_manager(
    router: &MultiServiceRouter,
    access_key_id: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("secretsmanager.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("secretsmanager.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 \
                 Credential={access_key_id}/20260101/{REGION}/secretsmanager/aws4_request, \
                 SignedHeaders=host, Signature=0000"
            ),
        )
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the request body is serializable"),
        ))
        .expect("the test request builds");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// The `__type` discriminator of an awsJson1_1 error body, without its namespace.
pub(crate) fn json_error_code(parsed: &Value) -> String {
    parsed
        .get("__type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .rsplit('#')
        .next()
        .unwrap_or_default()
        .to_string()
}

/// The `message` of an awsJson1_1 error body, whichever casing it carries.
pub(crate) fn json_error_message(parsed: &Value) -> String {
    for member in ["message", "Message"] {
        if let Some(text) = parsed.get(member).and_then(Value::as_str) {
            return text.to_string();
        }
    }
    format!("<no message> in {parsed}")
}

/// The clause a refusal carries when the SESSION POLICIES are the boundary that withheld the
/// grant.
pub(crate) fn no_session_grant(action: &str) -> String {
    format!("because no session policy allows the {action} action")
}

/// The clause a refusal carries when nothing identity-based granted the action at all.
fn no_identity_grant(action: &str) -> String {
    format!("because no identity-based policy allows the {action} action")
}

/// The refusal sentence an action that addresses NO resource of its own is worded in: it
/// names no resource, because no resource was addressed.
pub(crate) fn not_authorized_account_wide(principal: &str, action: &str, reason: &str) -> String {
    format!("User: {principal} is not authorized to perform: {action} {reason}")
}

/// The refusal sentence Secrets Manager words for a request addressed to one secret, which
/// names the identifier the request supplied for it verbatim.
fn not_authorized_on(principal: &str, action: &str, resource: &str, reason: &str) -> String {
    format!(
        "User: {principal} is not authorized to perform: {action} on resource: {resource} \
         {reason}"
    )
}

/// WHICH identity's long-term credentials mint the session a row drives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Minter {
    /// The account's own published credential, which no identity-based document bounds.
    AccountCredential,
    /// An IAM user whose inline document allows federating and every action this suite asks
    /// about.
    PermissiveUser,
    /// An IAM user whose inline document allows federating and NOTHING else, so no action
    /// here is inside the intersection whatever the session policy says.
    FederateOnlyUser,
}

/// What a row expects of one call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expect {
    /// Both halves allow the action.
    Served,
    /// Refused because the SESSION POLICIES withheld the grant.
    NoSessionGrant,
    /// Refused because nothing identity-based granted the action.
    NoIdentityGrant,
}

impl Expect {
    /// The diagnosis clause a refused row's message carries for `action`.
    fn reason(self, action: &str) -> String {
        match self {
            Self::NoSessionGrant => no_session_grant(action),
            _ => no_identity_grant(action),
        }
    }
}

struct Row {
    case: &'static str,
    minter: Minter,
    /// The `Action` elements the row's inline session policy allows over `*`, or an empty
    /// slice for a mint that passes no session policy at all.
    allows: &'static [&'static str],
    /// What the account-wide bucket listing answers.
    listing: Expect,
    /// What the resource-scoped object read answers.
    read: Expect,
    /// What the account-wide secret listing answers.
    secret_listing: Expect,
    /// What the resource-scoped secret read answers.
    secret_read: Expect,
}

/// RULE: a federated-user session's permissions are the intersection of the minting IAM
/// identity's documents and the session policies the mint passed — an action both halves
/// allow is served by whichever service decides it, and one either half withholds is refused
/// naming that half.
#[tokio::test]
async fn a_federation_session_may_do_the_intersection_of_both_halves() {
    let rows = [
        Row {
            case: "the session policy allows each listing action by name",
            minter: Minter::AccountCredential,
            allows: &[LIST_ALL_MY_BUCKETS, LIST_SECRETS],
            listing: Expect::Served,
            read: Expect::NoSessionGrant,
            secret_listing: Expect::Served,
            secret_read: Expect::NoSessionGrant,
        },
        Row {
            case: "the session policy allows every action",
            minter: Minter::AccountCredential,
            allows: &["*"],
            listing: Expect::Served,
            read: Expect::Served,
            secret_listing: Expect::Served,
            secret_read: Expect::Served,
        },
        Row {
            case: "the session policy allows one whole service",
            minter: Minter::AccountCredential,
            allows: &["s3:*"],
            listing: Expect::Served,
            read: Expect::Served,
            secret_listing: Expect::NoSessionGrant,
            secret_read: Expect::NoSessionGrant,
        },
        Row {
            case: "the session policy allows the other whole service",
            minter: Minter::AccountCredential,
            allows: &["secretsmanager:*"],
            listing: Expect::NoSessionGrant,
            read: Expect::NoSessionGrant,
            secret_listing: Expect::Served,
            secret_read: Expect::Served,
        },
        Row {
            case: "the mint passes no session policy at all",
            minter: Minter::AccountCredential,
            allows: &[],
            listing: Expect::NoSessionGrant,
            read: Expect::NoSessionGrant,
            secret_listing: Expect::NoSessionGrant,
            secret_read: Expect::NoSessionGrant,
        },
        Row {
            case: "an IAM user allowed everything mints a listing-only session",
            minter: Minter::PermissiveUser,
            allows: &[LIST_ALL_MY_BUCKETS, LIST_SECRETS],
            listing: Expect::Served,
            read: Expect::NoSessionGrant,
            secret_listing: Expect::Served,
            secret_read: Expect::NoSessionGrant,
        },
        Row {
            case: "an IAM user allowed everything mints an allow-all session",
            minter: Minter::PermissiveUser,
            allows: &["*"],
            listing: Expect::Served,
            read: Expect::Served,
            secret_listing: Expect::Served,
            secret_read: Expect::Served,
        },
        Row {
            case: "an IAM user allowed nothing but federating mints an allow-all session",
            minter: Minter::FederateOnlyUser,
            allows: &["*"],
            listing: Expect::NoIdentityGrant,
            read: Expect::NoIdentityGrant,
            secret_listing: Expect::NoIdentityGrant,
            secret_read: Expect::NoIdentityGrant,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let router = build_router();
        let bucket = format!("intersection-bucket-{index}");
        seed_bucket(&router, row.case, &bucket).await;
        let secret_arn = given_secret(&router, row.case).await;

        let minting_key = match row.minter {
            Minter::AccountCredential => AMBIENT_KEY.to_string(),
            Minter::PermissiveUser => user_with_key(
                &router,
                row.case,
                &format!("PermissiveFederator{index}"),
                &document(&[
                    statement("Allow", "sts:GetFederationToken", "*", None),
                    statement("Allow", LIST_ALL_MY_BUCKETS, "*", None),
                    statement("Allow", GET_OBJECT, "*", None),
                    statement("Allow", "secretsmanager:*", "*", None),
                ]),
            )
            .await,
            Minter::FederateOnlyUser => user_with_key(
                &router,
                row.case,
                &format!("BareFederator{index}"),
                &document(&[statement("Allow", "sts:GetFederationToken", "*", None)]),
            )
            .await,
        };

        let name = format!("fed-{index}");
        let (federated, principal) =
            given_federation_session(&router, row.case, &minting_key, &name, row.allows).await;

        // An action that addresses no resource at all: only the identity's documents and the
        // scope can allow it, at either service's gate.
        let (status, body) = s3(&router, &federated, "GET", "/", "").await;
        match row.listing {
            Expect::Served => {
                assert_eq!(
                    status, 200,
                    "case {:?}: the bucket listing is inside the intersection, got {status}: \
                     {body}",
                    row.case
                );
                assert!(
                    body.contains("<Buckets>"),
                    "case {:?}: expected a Buckets container, got {body:?}",
                    row.case
                );
            }
            refused => {
                assert_eq!(
                    status, 403,
                    "case {:?}: the bucket listing is outside the intersection, got {status}: \
                     {body}",
                    row.case
                );
                let code = code_of(&body);
                assert_eq!(
                    code, "AccessDenied",
                    "case {:?}: expected AccessDenied, got {code:?}: {body}",
                    row.case
                );
                let message = message_of(&body);
                assert_eq!(
                    message,
                    not_authorized_account_wide(
                        &principal,
                        LIST_ALL_MY_BUCKETS,
                        &refused.reason(LIST_ALL_MY_BUCKETS)
                    ),
                    "case {:?}: expected the refusal to name the deciding half, got {message:?}",
                    row.case
                );
            }
        }

        // The resource-scoped read, decided by the same rule at the other decision site of
        // the same service.
        let (status, body) = get_object(&router, &federated, &bucket, ASCII_KEY).await;
        match row.read {
            Expect::Served => assert_eq!(
                status, 200,
                "case {:?}: the object read is inside the intersection, got {status}: {body}",
                row.case
            ),
            refused => {
                assert_eq!(
                    status, 403,
                    "case {:?}: the object read is outside the intersection, got {status}: \
                     {body}",
                    row.case
                );
                let message = message_of(&body);
                assert_eq!(
                    message,
                    format!(
                        "User: {principal} is not authorized to perform: {GET_OBJECT} on \
                         resource: \"{}\" {}",
                        object_arn(&bucket, ASCII_KEY),
                        refused.reason(GET_OBJECT)
                    ),
                    "case {:?}: expected the refusal to name the deciding half, got {message:?}",
                    row.case
                );
            }
        }

        // The SAME rule at ANOTHER service's gate: the account-wide secret listing.
        let (status, answer) =
            secrets_manager(&router, &federated, "ListSecrets", &json!({})).await;
        match row.secret_listing {
            Expect::Served => {
                assert_eq!(
                    status, 200,
                    "case {:?}: the secret listing is inside the intersection, got {status}: \
                     {answer}",
                    row.case
                );
                assert!(
                    answer.get("SecretList").is_some(),
                    "case {:?}: expected a SecretList, got {answer}",
                    row.case
                );
            }
            refused => {
                let code = json_error_code(&answer);
                assert_eq!(
                    code, "AccessDeniedException",
                    "case {:?}: the secret listing is outside the intersection, got {status} \
                     {code:?}: {answer}",
                    row.case
                );
                let message = json_error_message(&answer);
                assert_eq!(
                    message,
                    not_authorized_account_wide(
                        &principal,
                        LIST_SECRETS,
                        &refused.reason(LIST_SECRETS)
                    ),
                    "case {:?}: expected the refusal to name the deciding half, got {message:?}",
                    row.case
                );
            }
        }

        // ... and that service's resource-scoped read.
        let (status, answer) = secrets_manager(
            &router,
            &federated,
            "GetSecretValue",
            &json!({ "SecretId": secret_arn }),
        )
        .await;
        match row.secret_read {
            Expect::Served => assert_eq!(
                status, 200,
                "case {:?}: the secret read is inside the intersection, got {status}: {answer}",
                row.case
            ),
            refused => {
                let code = json_error_code(&answer);
                assert_eq!(
                    code, "AccessDeniedException",
                    "case {:?}: the secret read is outside the intersection, got {status} \
                     {code:?}: {answer}",
                    row.case
                );
                let message = json_error_message(&answer);
                assert_eq!(
                    message,
                    not_authorized_on(
                        &principal,
                        GET_SECRET_VALUE,
                        &secret_arn,
                        &refused.reason(GET_SECRET_VALUE)
                    ),
                    "case {:?}: expected the refusal to name the deciding half, got {message:?}",
                    row.case
                );
            }
        }
    }
}

/// RULE: session policies are per-SESSION — one mint's scope decides only the credential that
/// mint vended, so a sibling session's grant neither leaks in nor out, at either service.
#[tokio::test]
async fn sibling_federation_sessions_keep_their_own_scopes() {
    let router = build_router();
    let case = "two sessions minted with different scopes";
    let bucket = "sibling-scope-bucket";
    seed_bucket(&router, case, bucket).await;
    given_secret(&router, case).await;

    // (the federated user's name, the one action its session policy allows)
    let mints = [
        ("fed-buckets", LIST_ALL_MY_BUCKETS),
        ("fed-secrets", LIST_SECRETS),
    ];
    let mut minted: Vec<(&str, &str, String)> = Vec::new();
    for (name, action) in mints {
        let (key, _) =
            given_federation_session(&router, case, AMBIENT_KEY, name, &[action]).await;
        minted.push((name, action, key));
    }

    for (name, action, key) in &minted {
        let (status, body) = s3(&router, key, "GET", "/", "").await;
        let expected = if *action == LIST_ALL_MY_BUCKETS { 200 } else { 403 };
        assert_eq!(
            status, expected,
            "case {case:?}: session {name:?} allows {action:?}, so its bucket listing must \
             answer HTTP {expected}, got {status}: {body}"
        );
        let (status, answer) = secrets_manager(&router, key, "ListSecrets", &json!({})).await;
        let expected = if *action == LIST_SECRETS { 200 } else { 400 };
        assert_eq!(
            status, expected,
            "case {case:?}: session {name:?} allows {action:?}, so its secret listing must \
             answer HTTP {expected}, got {status}: {answer}"
        );
    }
}

/// Mint a federated-user session with `minting_key`, passing an inline session policy that
/// allows each of `allows` over every resource — and none at all where `allows` is empty.
///
/// Answers the session's access key id and the federated-user ARN a refusal of its requests
/// names as the principal.
pub(crate) async fn given_federation_session(
    router: &MultiServiceRouter,
    case: &str,
    minting_key: &str,
    name: &str,
    allows: &[&str],
) -> (String, String) {
    let session_policy = (!allows.is_empty()).then(|| {
        let statements: Vec<String> = allows
            .iter()
            .map(|action| statement("Allow", action, "*", None))
            .collect();
        document(&statements)
    });
    let mut members = vec![("Name", name)];
    if let Some(policy) = &session_policy {
        members.push(("Policy", policy.as_str()));
    }
    let (status, body) = sts(router, minting_key, "GetFederationToken", &members).await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetFederationToken({name:?}) must succeed, got {status}: {body}"
    );
    (
        element(&body, "AccessKeyId"),
        format!("arn:aws:sts::{ACCOUNT}:federated-user/{name}"),
    )
}

/// Create the secret every row reads with the account's own credential, and answer the ARN
/// the account holds it under — which is the resource a read of it is decided over.
pub(crate) async fn given_secret(router: &MultiServiceRouter, case: &str) -> String {
    let (status, answer) = secrets_manager(
        router,
        AMBIENT_KEY,
        "CreateSecret",
        &json!({
            "Name": SECRET_NAME,
            "SecretString": "the-stored-credential",
            "ClientRequestToken": SECRET_VERSION_TOKEN,
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateSecret must succeed, got {status}: {answer}"
    );
    answer["ARN"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no ARN in {answer}"))
        .to_string()
}
