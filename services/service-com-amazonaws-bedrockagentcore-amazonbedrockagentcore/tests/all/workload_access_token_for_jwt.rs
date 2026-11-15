// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for `GetWorkloadAccessTokenForJWT`: the mint that acts on behalf of the
//! user a presented bearer token speaks for.
//!
//! The rule under test, one level above the recorded case (one expired token refused):
//!
//! > The `userToken` a call presents is VALIDATED before anything is minted. A token this
//! > plane cannot accept — one whose own validity window has run out, one not yet valid, one
//! > naming no expiry, one naming no subject, one nothing signed, and one that is three
//! > base64url-shaped runs but not a compact JWS carrying a JSON claim set — is refused
//! > `UnauthorizedException` (HTTP 401) with the fixed sentence of its class (`Token has
//! > expired` for the expiry class), and mints nothing and records nothing. A token inside
//! > its window naming a subject is accepted, and the workload access token minted from it
//! > names that subject as the user it acts for, in the issuance this service records.
//!
//! Every case drives real HTTP through the same `MultiServiceRouter` the emulator serves
//! with, so the wire shape (route, status code, `restJson1` body, modelled error code) is
//! exercised, not just the handler function.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;
use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use service_com_amazonaws_bedrockagentcore_amazonbedrockagentcore as agentcore;

/// `GetWorkloadAccessTokenForJWT`: `POST /identities/GetWorkloadAccessTokenForJWT` (source:
/// smithy_query query="operation" operation="GetWorkloadAccessTokenForJWT": `http`).
const ROUTE: &str = "/identities/GetWorkloadAccessTokenForJWT";

/// The region every call is signed for.
const REGION: &str = "us-east-1";

/// The canonical account every ARN this emulator emits carries.
const ACCOUNT: &str = "000000000000";

/// The emulator, with request authentication off so a test signs nothing, plus the shared
/// engine so a case can read the issuance the handler recorded.
fn emulator() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(agentcore::EmulatedService::new(Arc::clone(&engine)));
    (router, engine)
}

/// Send one request and return its status and parsed JSON body.
async fn get_token(router: &MultiServiceRouter, body: Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri(format!(
            "https://bedrock-agentcore.{REGION}.amazonaws.com{ROUTE}"
        ))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/bedrock-agentcore/\
                 aws4_request, SignedHeaders=host, Signature=0"
            ),
        )
        .header("content-type", "application/json")
        .body(bytes::Bytes::from(body.to_string()))
        .expect("build the request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, parsed)
}

fn error_code(body: &Value) -> &str {
    body.get("__type").and_then(Value::as_str).unwrap_or("")
}

fn text<'a>(body: &'a Value, name: &str) -> &'a str {
    body.get(name).and_then(Value::as_str).unwrap_or("")
}

/// `time` as RFC 7519 §2's NumericDate — whole seconds since the epoch, which is how a claim
/// carries an instant.
fn numeric_date(time: SystemTime) -> i64 {
    i64::try_from(
        time.duration_since(SystemTime::UNIX_EPOCH)
            .expect("the clock is past the epoch")
            .as_secs(),
    )
    .expect("the instant fits an i64")
}

/// A compact JWS carrying `claims`, with a JOSE header naming a signing algorithm and a
/// non-empty signature segment: the form a caller presents as its identity provider's token.
/// Its segments are base64url, so the value is inside `UserTokenType`'s `@pattern`.
fn presented(claims: &Value) -> String {
    format!(
        "{}.{}.{}",
        service_common::jwt::encode_base64url(br#"{"alg":"RS256","typ":"JWT","kid":"k1"}"#),
        service_common::jwt::encode_base64url(claims.to_string().as_bytes()),
        service_common::jwt::encode_base64url(b"presented-signature")
    )
}

/// The claims of the compact JWS `token`, read back through the tree's own reader of a token
/// this service minted.
fn claims_of(case: &str, token: &str) -> Value {
    let read = service_common::jwt::read_unsecured(token).unwrap_or_else(|defect| {
        panic!("case {case:?}: expected a minted compact JWS, got {token:?} ({defect})")
    });
    Value::Object(read.claims)
}

/// What the emulator recorded for `token` at `now`: the workload and user it names.
fn issuance(
    engine: &engine::InMemoryEngine,
    token: &str,
    now: SystemTime,
) -> Option<(String, Option<String>)> {
    let state = agentcore::state::service_state(engine).expect("the service state");
    let store = state.lock();
    store
        .workload_access_tokens
        .issued(token, now)
        .map(|held| (held.workload_name.clone(), held.user_id.clone()))
}

/// How many issuances the emulator holds — the check that a refused request recorded nothing.
fn issuance_count(engine: &engine::InMemoryEngine) -> usize {
    let state = agentcore::state::service_state(engine).expect("the service state");
    let store = state.lock();
    store.workload_access_tokens.len()
}

/// A token this plane accepts mints one for the workload the request names, acting for the
/// SUBJECT the presented token claims, and the issuance records that user.
///
/// Rows differ along the subject the presented token names (an identifier, an opaque one, an
/// address), the claims it carries beside the window (`iss`, `aud`, `nbf` already passed)
/// and the workload name (both published bounds of `WorkloadIdentityNameType`'s 3..255
/// length and every character class its `^[A-Za-z0-9_.-]+$` pattern admits; source:
/// smithy_query query="operation" operation="GetWorkloadAccessTokenForJWT").
#[tokio::test]
async fn a_token_minted_for_a_presented_jwt_acts_for_that_jwt_s_subject() {
    let (router, engine) = emulator();
    let now = SystemTime::now();
    let seconds = numeric_date(now);

    let cases: Vec<(&str, String, &str, Value)> = vec![
        (
            "the shortest workload name and a plain subject",
            "abc".to_string(),
            "user-1",
            json!({ "sub": "user-1", "exp": seconds + 600 }),
        ),
        (
            "every character class the workload pattern admits",
            "Agent_0.workload-9".to_string(),
            "a3f1c0de-0000-4a1b-9c2d-5e6f70819203",
            json!({
                "sub": "a3f1c0de-0000-4a1b-9c2d-5e6f70819203",
                "iss": "https://idp.example.com/",
                "aud": "agentcore-client",
                "iat": seconds - 30,
                "exp": seconds + 600,
            }),
        ),
        (
            "the longest workload name and a subject that is an address",
            "w".repeat(255),
            "user@example.com",
            // The window is as generous as the other rows': what this row is about is the
            // workload NAME and the subject, so its presented token must still be inside its
            // window however long the rows before it took. A token at the edge of its window
            // — expiring at this very instant, or a moment ago — is the refusal suite's own
            // row, where the edge is what is being tested.
            json!({ "sub": "user@example.com", "exp": seconds + 600 }),
        ),
        (
            "a token already inside its not-before",
            "support-agent".to_string(),
            "operator-7",
            json!({
                "sub": "operator-7",
                "nbf": seconds - 60,
                "exp": seconds + 600,
            }),
        ),
    ];

    let mut minted: BTreeSet<String> = BTreeSet::new();
    for (case, workload, subject, claims) in cases {
        let (status, body) = get_token(
            &router,
            json!({ "workloadName": workload, "userToken": presented(&claims) }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from GetWorkloadAccessTokenForJWT, got {status} \
             with {body:?}"
        );
        let token = text(&body, "workloadAccessToken").to_string();
        // `WorkloadIdentityTokenType` is len 1..131072.
        assert!(
            (1..=131_072).contains(&token.len()),
            "case {case:?}: expected a token inside the modelled 1..131072 length, got {} \
             characters",
            token.len()
        );
        assert!(
            minted.insert(token.clone()),
            "case {case:?}: expected a token no earlier call minted, got one that repeats"
        );

        // The minted token is this service's own: its subject is the user the PRESENTED token
        // named, and its audience the workload identity the request addressed.
        let claims = claims_of(case, &token);
        assert_eq!(
            text(&claims, "sub"),
            subject,
            "case {case:?}: expected the presented token's subject as the user, got {claims:?}"
        );
        assert_eq!(
            text(&claims, "aud"),
            format!(
                "arn:aws:bedrock-agentcore:{REGION}:{ACCOUNT}:workload-identity-directory/default/\
                 workload-identity/{workload}"
            ),
            "case {case:?}: expected the workload identity the request named, got {claims:?}"
        );
        assert_eq!(
            text(&claims, "workloadName"),
            workload,
            "case {case:?}: expected the workload the request named, got {claims:?}"
        );

        // The issuance was recorded against that same user, and resolves until its own
        // expiry and no longer.
        let held = issuance(&engine, &token, now);
        assert_eq!(
            held,
            Some((workload.clone(), Some(subject.to_string()))),
            "case {case:?}: expected the recorded issuance to name the workload and the \
             presented token's subject, got {held:?}"
        );
        let past_expiry = issuance(
            &engine,
            &token,
            now + agentcore::state::WORKLOAD_ACCESS_TOKEN_TTL + Duration::from_secs(1),
        );
        assert_eq!(
            past_expiry, None,
            "case {case:?}: expected the issuance to stop resolving once its window has run \
             out, got {past_expiry:?}"
        );
    }
}

/// A presented token this plane cannot accept is refused `UnauthorizedException` (401) with
/// the fixed sentence of its class, and mints and records nothing.
///
/// The rows span every way a presented token can fail: its window (expired a moment ago,
/// expiring at this very instant, not yet valid, naming no expiry), what it names (no
/// subject, an empty subject), what signed it (the unsecured form of RFC 7515 §6, whose
/// signature segment is empty), and whether it is a compact JWS at all (three
/// base64url-shaped runs that are not base64url-encoded JSON).
#[tokio::test]
async fn a_presented_token_this_plane_cannot_accept_mints_nothing() {
    let (router, engine) = emulator();
    let seconds = numeric_date(SystemTime::now());
    // The one sentence AWS words the expiry class with, from the recorded refusal; every
    // other class carries the invalid-token sentence of this plane.
    let expired = "Token has expired";

    // The unsecured form of RFC 7515 §6: the header names `alg` `none`, so nothing signed
    // it however its signature segment is filled. (An EMPTY third segment is outside
    // `UserTokenType`'s own `@pattern`, and the request boundary refuses that before the
    // handler runs.)
    let unsecured = format!(
        "{}.{}.{}",
        service_common::jwt::encode_base64url(br#"{"alg":"none","typ":"JWT"}"#),
        service_common::jwt::encode_base64url(
            json!({ "sub": "user-1", "exp": seconds + 600 })
                .to_string()
                .as_bytes()
        ),
        service_common::jwt::encode_base64url(b"looks-like-a-signature")
    );

    let cases: Vec<(&str, String, bool)> = vec![
        (
            "expired a moment ago",
            presented(&json!({ "sub": "user-1", "exp": seconds - 1 })),
            true,
        ),
        (
            "expiring at this very instant",
            presented(&json!({ "sub": "user-1", "exp": seconds })),
            true,
        ),
        (
            "expired long ago, as a stale cached token is",
            presented(&json!({ "sub": "user-1", "iat": seconds - 7_200, "exp": seconds - 3_600 })),
            true,
        ),
        (
            "not yet valid",
            presented(&json!({
                "sub": "user-1",
                "nbf": seconds + 600,
                "exp": seconds + 1_200,
            })),
            false,
        ),
        (
            "naming no expiry at all",
            presented(&json!({ "sub": "user-1" })),
            false,
        ),
        (
            "naming no subject",
            presented(&json!({ "exp": seconds + 600 })),
            false,
        ),
        (
            "naming an empty subject",
            presented(&json!({ "sub": "", "exp": seconds + 600 })),
            false,
        ),
        ("unsecured, so nothing signed it", unsecured, false),
        (
            "three runs that are not base64url-encoded JSON",
            "abc.def.ghi".to_string(),
            false,
        ),
        (
            "a value that is no token at all",
            "not-three-segments".to_string(),
            false,
        ),
    ];

    for (case, user_token, is_expiry_class) in cases {
        let (status, body) = get_token(
            &router,
            json!({ "workloadName": "demo.agent", "userToken": user_token }),
        )
        .await;
        assert_eq!(
            status, 401,
            "case {case:?}: expected HTTP 401, got {status} with {body:?}"
        );
        assert_eq!(
            error_code(&body),
            "UnauthorizedException",
            "case {case:?}: expected UnauthorizedException, got {body:?}"
        );
        let message = text(&body, "message");
        if is_expiry_class {
            assert_eq!(
                message, expired,
                "case {case:?}: expected the fixed text of the expiry class, got {message:?}"
            );
        } else {
            assert!(
                !message.is_empty() && message != expired,
                "case {case:?}: expected the invalid-token sentence of its own class, got \
                 {message:?}"
            );
            assert!(
                !message.contains("abc.def.ghi") && !message.contains("eyJ"),
                "case {case:?}: a refusal must not quote the presented token back, got \
                 {message:?}"
            );
        }
        assert!(
            body.get("workloadAccessToken").is_none(),
            "case {case:?}: a refused call must mint nothing, got {body:?}"
        );
        assert_eq!(
            issuance_count(&engine),
            0,
            "case {case:?}: a refused call must record no issuance"
        );
    }
}

/// Input outside a modelled constraint is refused with a 400 `ValidationException` before the
/// presented token is read at all: one case on each side of `WorkloadIdentityNameType`'s
/// 3..255 length, one outside its `^[A-Za-z0-9_.-]+$` pattern, one empty `userToken` (below
/// `UserTokenType`'s 1..131072 length) and one whose last segment is empty (outside that
/// member's `@pattern`), plus a request missing each required member.
#[tokio::test]
async fn input_outside_a_modelled_constraint_is_refused_with_400() {
    let (router, engine) = emulator();
    let token = presented(&json!({
        "sub": "user-1",
        "exp": numeric_date(SystemTime::now()) + 600,
    }));

    let cases: Vec<(&str, Value)> = vec![
        (
            "workload-name-below-its-length",
            json!({ "workloadName": "ab", "userToken": token }),
        ),
        (
            "workload-name-past-its-length",
            json!({ "workloadName": "w".repeat(256), "userToken": token }),
        ),
        (
            "workload-name-outside-its-pattern",
            json!({ "workloadName": "demo agent!", "userToken": token }),
        ),
        (
            "empty-user-token",
            json!({ "workloadName": "demo.agent", "userToken": "" }),
        ),
        (
            "user-token-whose-last-segment-is-empty",
            json!({ "workloadName": "demo.agent", "userToken": "aGVhZGVy.Y2xhaW1z." }),
        ),
        (
            "no-user-token-at-all",
            json!({ "workloadName": "demo.agent" }),
        ),
        ("no-workload-name-at-all", json!({ "userToken": token })),
    ];

    for (case, request) in cases {
        let (status, body) = get_token(&router, request).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {body:?}"
        );
        assert_eq!(
            error_code(&body),
            "ValidationException",
            "case {case:?}: expected ValidationException, got {body:?}"
        );
        assert_eq!(
            issuance_count(&engine),
            0,
            "case {case:?}: a refused request must mint and record nothing"
        );
    }
}
