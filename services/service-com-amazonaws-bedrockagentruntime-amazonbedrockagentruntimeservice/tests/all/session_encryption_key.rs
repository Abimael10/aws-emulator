// SPDX-License-Identifier: CC0-1.0

//! The customer managed key a `CreateSession` names, resolved in the service that OWNS keys.
//!
//! THE RULE UNDER TEST: **a session is minted only under a key AWS KMS resolves to a key this
//! account and Region hold; a reference it resolves no such key for is refused `403`
//! `AccessDeniedException`, with the sentence this service words for a key it cannot use, and
//! no session is created.** The service cannot encrypt with a key it cannot reach, so the
//! refusal belongs on the create — a session encrypted under a key that is not there could
//! never be read back.
//!
//! What the rows generalise over is the FORM of the reference and where the key is: no key
//! named at all, two different keys KMS really issued in this Region, a well-formed key ARN of
//! this account and Region whose uuid KMS never issued, a key ARN of another account, and the
//! ARN of a key KMS DID issue but at another Region — a KMS key being a regional resource.
//!
//! Every reference is resolved through KMS's own endpoint on the same engine, so no row
//! restates what that service holds: the accepted rows name a key `kms:CreateKey` answered
//! with, and the refused ones name keys no `kms:CreateKey` of this suite ever produced.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::sessions::{error_code, is_model_uuid, label, ACCOUNT};

/// The Region the sessions are created in, and so the Region a key named by a bare ARN has to
/// live in.
const REGION: &str = "us-west-2";

/// A second Region, for the row whose key KMS issued somewhere else.
const OTHER_REGION: &str = "eu-west-1";

/// A 12-digit account that is NOT the one this emulator presents, for the cross-account row.
const FOREIGN_ACCOUNT: &str = "111122223333";

/// A uuid no `kms:CreateKey` issues, for the key ARN that names nothing.
const UNISSUED_KEY_ID: &str = "11111111-2222-3333-4444-555555555555";

/// The status and code of each outcome, from the model:
/// `smithy_query(query="operation",operation="CreateSession")` -> `AccessDeniedException`
/// httpError 403, and the operation's own success status 201.
const CREATED: u16 = 201;
const DENIED: (u16, &str) = (403, "AccessDeniedException");

/// The sentence a key this service cannot use is refused with, which is fixed text for the
/// whole class: it names the two policies the key's access is granted in and interpolates
/// nothing.
const KEY_ACCESS_DENIED: &str =
    "Check that your IAM role has the necessary KMS permissions in both the key policy and \
     IAM policy, then retry your request.";

/// A router carrying this service and the AWS KMS one over ONE engine, so a key created
/// through KMS is the very key a session's reference resolves against.
fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_kms_trentservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_bedrockagentruntime_amazonbedrockagentruntimeservice::EmulatedService::new(
            engine,
        ),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service` and `region`, which
/// is what routes the request and what a minted ARN takes its Region from.
fn authorization(service: &str, region: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDTESTONLY/20261021/{region}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000"
    )
}

/// One Bedrock Agent runtime request, on the URI its own `@http` binding names.
async fn bedrock(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> (u16, Value) {
    let payload = body
        .map(|document| Bytes::from(serde_json::to_vec(document).expect("the body serializes")))
        .unwrap_or_default();
    let request = http::Request::builder()
        .method(method)
        .uri(path)
        .header("authorization", authorization("bedrock", REGION))
        .header("content-type", "application/json")
        .body(payload)
        .expect("the test request builds");
    answered(router, request).await
}

/// One KMS request over the awsJson1_1 wire that model declares, signed for `region` — the
/// Region the key it creates lives in.
async fn kms(
    router: &MultiServiceRouter,
    region: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("authorization", authorization("kms", region))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("TrentService.{operation}"))
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the body serializes"),
        ))
        .expect("the test request builds");
    answered(router, request).await
}

/// The `(status, body)` one request is answered with.
async fn answered(router: &MultiServiceRouter, request: http::Request<Bytes>) -> (u16, Value) {
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, value)
}

/// The ARN of a key KMS really issued at `region`.
async fn created_key_arn(router: &MultiServiceRouter, case: &str, region: &str) -> String {
    let (status, created) = kms(router, region, "CreateKey", &json!({})).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from kms:CreateKey, got {status} with {created}"
    );
    created
        .get("KeyMetadata")
        .and_then(|metadata| metadata.get("Arn"))
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: kms:CreateKey answered no ARN: {created}"))
        .to_string()
}

/// A key ARN of the form `KmsKeyArn`'s own pattern admits
/// (`^arn:aws(|-cn|-us-gov):kms:[a-zA-Z0-9-]*:[0-9]{12}:key/[a-zA-Z0-9-]{36}$`), for the rows
/// that name a key nothing issued here.
fn key_arn(region: &str, account: &str, key_id: &str) -> String {
    format!("arn:aws:kms:{region}:{account}:key/{key_id}")
}

/// How many sessions this service holds, as `ListSessions` reports them: what a refused create
/// must leave unchanged.
async fn session_count(router: &MultiServiceRouter) -> usize {
    let (_, listed) = bedrock(router, "POST", "/sessions/", Some(&json!({}))).await;
    listed
        .get("sessionSummaries")
        .and_then(Value::as_array)
        .map_or(0, Vec::len)
}

/// What one row expects of the create.
enum Expected {
    /// The session is minted, and `GetSession` reports the key the row named (none for a row
    /// that names no key at all).
    Created,
    /// The create is refused and nothing is created.
    Denied,
}

/// Where the key the row names comes from.
enum Key {
    /// No key at all: the member is absent.
    None,
    /// A key KMS issued at this Region, named by the ARN that service answered.
    Held,
    /// A second such key, so the accepted case is not one special value.
    SecondHeld,
    /// A well-formed key ARN of this account and Region whose uuid KMS never issued.
    Unissued,
    /// A key ARN of another account.
    ForeignAccount,
    /// The ARN of a key KMS issued at ANOTHER Region.
    OtherRegion,
}

#[tokio::test]
async fn a_session_is_minted_only_under_a_key_kms_resolves() {
    // (case, which key the declaration names, the outcome)
    let rows: [(&str, Key, Expected); 6] = [
        ("no encryption key at all", Key::None, Expected::Created),
        (
            "a key KMS issued in this Region",
            Key::Held,
            Expected::Created,
        ),
        (
            "a second key KMS issued in this Region",
            Key::SecondHeld,
            Expected::Created,
        ),
        (
            "a key ARN whose uuid KMS never issued",
            Key::Unissued,
            Expected::Denied,
        ),
        (
            "a key ARN of another account",
            Key::ForeignAccount,
            Expected::Denied,
        ),
        (
            "the ARN of a key KMS issued at another Region",
            Key::OtherRegion,
            Expected::Denied,
        ),
    ];

    for (case, key, expected) in rows {
        let router = router();
        // Both keys exist for every row, so the rows differ only in WHICH reference the
        // create names.
        let held = created_key_arn(&router, case, REGION).await;
        let second_held = created_key_arn(&router, case, REGION).await;
        let elsewhere = created_key_arn(&router, case, OTHER_REGION).await;
        let reference = match key {
            Key::None => None,
            Key::Held => Some(held),
            Key::SecondHeld => Some(second_held),
            Key::Unissued => Some(key_arn(REGION, ACCOUNT, UNISSUED_KEY_ID)),
            Key::ForeignAccount => Some(key_arn(REGION, FOREIGN_ACCOUNT, UNISSUED_KEY_ID)),
            Key::OtherRegion => Some(elsewhere),
        };
        let request = match &reference {
            Some(arn) => json!({ "encryptionKeyArn": arn }),
            None => json!({}),
        };

        let before = session_count(&router).await;
        let (status, answer) = bedrock(&router, "PUT", "/sessions/", Some(&request)).await;

        match expected {
            Expected::Created => {
                assert_eq!(
                    status, CREATED,
                    "case {case:?}: expected HTTP {CREATED} from CreateSession, got {status} \
                     with {answer}"
                );
                let session_id = answer
                    .get("sessionId")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                assert!(
                    is_model_uuid(session_id),
                    "case {case:?}: expected a minted sessionId, got {answer}"
                );
                // The key the create accepted is the one the session is read back under: the
                // create stored the reference rather than only admitting it.
                let (status, session) = bedrock(
                    &router,
                    "GET",
                    &format!("/sessions/{}/", label(session_id)),
                    None,
                )
                .await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: expected HTTP 200 from GetSession, got {status} with \
                     {session}"
                );
                assert_eq!(
                    session.get("encryptionKeyArn").and_then(Value::as_str),
                    reference.as_deref(),
                    "case {case:?}: expected the read to report the key the create took, got \
                     {session}"
                );
                assert_eq!(
                    session_count(&router).await,
                    before + 1,
                    "case {case:?}: expected the created session to be held"
                );
            }
            Expected::Denied => {
                assert_eq!(
                    (status, error_code(&answer)),
                    DENIED,
                    "case {case:?}: expected {DENIED:?}, got {status} with {answer}"
                );
                assert_eq!(
                    answer.get("message").and_then(Value::as_str),
                    Some(KEY_ACCESS_DENIED),
                    "case {case:?}: expected the key-access sentence, got {answer}"
                );
                assert_eq!(
                    session_count(&router).await,
                    before,
                    "case {case:?}: expected a refused create to hold no session"
                );
            }
        }
    }
}

#[tokio::test]
async fn a_key_kms_no_longer_resolves_stops_minting_sessions() {
    // The same reference, before and after KMS stops resolving it: a key scheduled for
    // deletion is still resolvable, so what this row varies is the key's EXISTENCE in that
    // service — the resolution is KMS's own at the moment of the call, not a value cached when
    // the first session was created.
    let case = "a key that was deleted between two creates";
    let router = router();
    let key_arn = created_key_arn(&router, case, REGION).await;

    let (status, created) = bedrock(
        &router,
        "PUT",
        "/sessions/",
        Some(&json!({ "encryptionKeyArn": key_arn })),
    )
    .await;
    assert_eq!(
        status, CREATED,
        "case {case:?}: expected the first create to be accepted, got {status} with {created}"
    );

    // KMS is asked to forget the key outright, so the next resolution of the same reference
    // finds nothing.
    let key_id = key_arn
        .rsplit_once(":key/")
        .map(|(_, id)| id.to_string())
        .unwrap_or_default();
    let (status, deleted) = kms(
        &router,
        REGION,
        "ScheduleKeyDeletion",
        &json!({ "KeyId": key_id, "PendingWindowInDays": 7 }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from kms:ScheduleKeyDeletion, got {status} with \
         {deleted}"
    );

    // A key PENDING deletion is still a key KMS resolves, so the create still goes through:
    // what this service refuses is a reference KMS resolves no key for, which is the one thing
    // it can establish.
    let (status, again) = bedrock(
        &router,
        "PUT",
        "/sessions/",
        Some(&json!({ "encryptionKeyArn": key_arn })),
    )
    .await;
    assert_eq!(
        status, CREATED,
        "case {case:?}: expected a key KMS still resolves to be accepted, got {status} with \
         {again}"
    );
}
