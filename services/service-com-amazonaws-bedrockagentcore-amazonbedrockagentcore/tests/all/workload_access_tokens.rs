// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the AgentCore identity plane's `GetWorkloadAccessTokenForUserId`
//! and `GetWorkloadAccessToken`.
//!
//! The rule under test: obtaining a workload access token MINTS one — a compact JWS whose
//! claims are computed from the request (the workload identity the `workloadName`
//! addresses, the user the call acts for, the issuing authority of the region the request
//! was signed for) and from the issuance the emulator records (the instant it was issued
//! and the instant it stops being valid) — answers it in the modelled output member, and
//! records the issuance so this service resolves a token it minted back to the workload
//! and user it names until that instant and no longer. Every call mints its own token, and
//! a request outside a modelled constraint is refused without recording anything.
//!
//! `GetWorkloadAccessToken` is the same mint for a workload acting for NO user, so its
//! token names the workload identity alone and its issuance records no user.
//!
//! The last two cases close the loop the mints exist for: a token this plane has just minted
//! is immediately usable against a FIRST-PARTY call of the same plane
//! (`GetResourceApiKey`) — which gets past the token and is refused only on the credential
//! provider it named, with the sentence AWS words that miss with — while a presented value no
//! live issuance stands behind is refused `AccessDeniedException` before anything the request
//! named is resolved.
//!
//! The last four cases are the plane's other half of the same flow:
//! `CompleteResourceTokenAuth`, which CONFIRMS a user authorization session this account had
//! issued. A session is a record of this plane's own making, held with the user the
//! authorization was started for, so only a session this plane handed out, inside its own
//! window, for the very user the confirmation acts for is confirmed; every other session URI
//! — one no initiation ever handed out included — is refused with the single fixed sentence
//! AWS words that gate with, and confirms nothing.
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

/// `GetWorkloadAccessTokenForUserId`: `POST /identities/GetWorkloadAccessTokenForUserId`
/// (source: smithy_query query="operation"
/// operation="GetWorkloadAccessTokenForUserId": `http`).
const ROUTE: &str = "/identities/GetWorkloadAccessTokenForUserId";

/// The canonical account every ARN this emulator emits carries.
const ACCOUNT: &str = "000000000000";

/// `GetWorkloadAccessToken`: `POST /identities/GetWorkloadAccessToken` (source:
/// smithy_query query="operation" operation="GetWorkloadAccessToken": `http`).
const WORKLOAD_ROUTE: &str = "/identities/GetWorkloadAccessToken";

/// The emulator, with request authentication off so a test signs nothing, plus the shared
/// engine so a case can read the issuance the handler recorded.
fn emulator() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(agentcore::EmulatedService::new(Arc::clone(&engine)));
    (router, engine)
}

/// Send one request and return its status and parsed JSON body (`Value::Null` for an empty
/// body). The SigV4 credential scope is what routes the request to this service, and its
/// region is the region the request is signed for.
async fn get_token(
    router: &MultiServiceRouter,
    region: &str,
    body: Value,
) -> (u16, Value) {
    post(router, region, ROUTE, body).await
}

/// The same for `GetWorkloadAccessToken`, the mint of a workload acting for no user.
async fn get_workload_token(
    router: &MultiServiceRouter,
    region: &str,
    body: Value,
) -> (u16, Value) {
    post(router, region, WORKLOAD_ROUTE, body).await
}

async fn post(
    router: &MultiServiceRouter,
    region: &str,
    route: &str,
    body: Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://bedrock-agentcore.{region}.amazonaws.com{route}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{region}/bedrock-agentcore/aws4_request, \
                 SignedHeaders=host, Signature=0"
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

/// One base64url segment of a compact JWS, read as JSON (RFC 7515 §2: base64url without
/// padding), through the tree's own segment decoder.
fn segment(case: &str, which: &str, encoded: &str) -> Value {
    let raw = service_common::jwt::decode_base64url(encoded).unwrap_or_else(|| {
        panic!("case {case:?}: expected the {which} segment in base64url, got {encoded:?}")
    });
    serde_json::from_slice(&raw).unwrap_or_else(|error| {
        panic!("case {case:?}: expected the {which} segment to be JSON, got {raw:?} ({error})")
    })
}

/// The JOSE header and the claim set of a compact JWS, with its three `.`-separated
/// segments checked (RFC 7515 §3.1): an unsecured JWS carries an empty signature segment.
/// The same token must also be readable by the tree's own reader of a token a service
/// minted itself, so the issued form cannot drift from the form that reader accepts.
fn read_token(case: &str, token: &str) -> (Value, Value) {
    let parts: Vec<&str> = token.split('.').collect();
    assert_eq!(
        parts.len(),
        3,
        "case {case:?}: expected three '.'-separated segments, got {token:?}"
    );
    assert_eq!(
        parts.get(2).copied().unwrap_or("x"),
        "",
        "case {case:?}: expected an unsecured JWS with an empty signature segment, got {token:?}"
    );
    let read = service_common::jwt::read_unsecured(token);
    assert!(
        read.is_ok(),
        "case {case:?}: expected the minted token to be readable as a compact JWS, got {read:?}"
    );
    (
        segment(case, "header", parts.first().copied().unwrap_or_default()),
        segment(case, "claims", parts.get(1).copied().unwrap_or_default()),
    )
}

/// What the emulator recorded for `token`, at `now`: the workload and user it names, or
/// `None` when the registry does not resolve it then. This is the test reading the store
/// the handlers share, not an emulator behaviour.
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

/// One issuance record, for the cases that drive the registry's own bounds directly.
fn issuance_record(
    workload: &str,
    user: &str,
    issued_at: SystemTime,
    expires_at: SystemTime,
) -> agentcore::state::IssuedWorkloadAccessToken {
    agentcore::state::IssuedWorkloadAccessToken {
        workload_name: workload.to_string(),
        user_id: Some(user.to_string()),
        issued_at,
        expires_at,
    }
}

/// How many issuances the emulator holds — the check that a refused request recorded
/// nothing.
fn issuance_count(engine: &engine::InMemoryEngine) -> usize {
    let state = agentcore::state::service_state(engine).expect("the service state");
    let store = state.lock();
    store.workload_access_tokens.len()
}

/// A token is minted for the workload and user the request names, in the region the
/// request was signed for, and the issuance it records resolves until its own expiry.
///
/// Rows differ along the workload name (every character class
/// `WorkloadIdentityNameType`'s pattern `^[A-Za-z0-9_.-]+$` admits, and both published
/// bounds of its 3..255 length), the user id (both published bounds of `UserIdType`'s
/// 1..128 length, and a user id that is not an identifier at all), and the signed region
/// (source: smithy_query query="operation"
/// operation="GetWorkloadAccessTokenForUserId").
#[tokio::test]
async fn a_minted_token_names_the_workload_and_user_the_request_asked_for() {
    let (router, engine) = emulator();

    let cases: Vec<(&str, &str, String, String)> = vec![
        (
            "shortest-workload-shortest-user",
            "us-east-1",
            "abc".to_string(),
            "u".to_string(),
        ),
        (
            "every-character-class-the-pattern-admits",
            "us-east-1",
            "Agent_0.workload-9".to_string(),
            "user-0123456789".to_string(),
        ),
        (
            "longest-workload-longest-user",
            "eu-west-2",
            "w".repeat(255),
            "u".repeat(128),
        ),
        (
            "another-region-signs-another-issuer",
            "ap-southeast-2",
            "payments.agent".to_string(),
            "user@example.com".to_string(),
        ),
        (
            "a-user-id-that-is-an-opaque-subject",
            "eu-west-2",
            "support-agent".to_string(),
            "a3f1c0de-0000-4a1b-9c2d-5e6f70819203".to_string(),
        ),
    ];

    let mut minted: BTreeSet<String> = BTreeSet::new();
    for (case, region, workload, user) in cases {
        let before = SystemTime::now();
        let (status, body) = get_token(
            &router,
            region,
            json!({"workloadName": workload, "userId": user}),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from GetWorkloadAccessTokenForUserId, got \
             {status} with {body:?}"
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

        let (header, claims) = read_token(case, &token);
        assert_eq!(
            text(&header, "alg"),
            "none",
            "case {case:?}: expected an unsecured JWS header, got {header:?}"
        );
        assert_eq!(
            text(&claims, "iss"),
            format!("https://bedrock-agentcore.{region}.amazonaws.com"),
            "case {case:?}: expected the issuer of the signed region, got {claims:?}"
        );
        assert_eq!(
            text(&claims, "aud"),
            format!(
                "arn:aws:bedrock-agentcore:{region}:{ACCOUNT}:workload-identity-directory/default/workload-identity/{workload}"
            ),
            "case {case:?}: expected the workload identity the request named, got {claims:?}"
        );
        assert_eq!(
            text(&claims, "sub"),
            user,
            "case {case:?}: expected the user the request named as the subject, got {claims:?}"
        );
        assert_eq!(
            text(&claims, "workloadName"),
            workload,
            "case {case:?}: expected the workload the request named, got {claims:?}"
        );

        // The validity window is the store's own time-to-live, measured from the instant
        // the token was issued.
        let ttl = i64::try_from(agentcore::state::WORKLOAD_ACCESS_TOKEN_TTL.as_secs())
            .expect("the time-to-live fits an i64");
        let issued_at = claims.get("iat").and_then(Value::as_i64).unwrap_or_default();
        let expires_at = claims.get("exp").and_then(Value::as_i64).unwrap_or_default();
        assert_eq!(
            expires_at - issued_at,
            ttl,
            "case {case:?}: expected a window of {ttl} seconds, got {claims:?}"
        );
        let signed_at = i64::try_from(
            before
                .duration_since(SystemTime::UNIX_EPOCH)
                .expect("the clock is past the epoch")
                .as_secs(),
        )
        .expect("the current instant fits an i64");
        assert!(
            (signed_at..=signed_at + 60).contains(&issued_at),
            "case {case:?}: expected the token to be issued at the instant of the call \
             ({signed_at}), got {issued_at}"
        );

        // The issuance was recorded, and it resolves to the workload and user the call
        // named until its own expiry and no longer.
        let at_issue = issuance(&engine, &token, before);
        assert_eq!(
            at_issue,
            Some((workload.to_string(), Some(user.to_string()))),
            "case {case:?}: expected the recorded issuance to name the workload and user, \
             got {at_issue:?}"
        );
        let again = issuance(&engine, &token, before);
        assert_eq!(
            again, at_issue,
            "case {case:?}: expected resolving a token twice to answer alike, got {again:?}"
        );
        let past_expiry = issuance(
            &engine,
            &token,
            before + agentcore::state::WORKLOAD_ACCESS_TOKEN_TTL + Duration::from_secs(1),
        );
        assert_eq!(
            past_expiry, None,
            "case {case:?}: expected the issuance to stop resolving once its window has \
             run out, got {past_expiry:?}"
        );
    }
}

/// Two calls naming the same workload and the same user are two tokens: a workload access
/// token is minted per call, never looked up, so neither call hands out the other's token
/// and both issuances are recorded.
#[tokio::test]
async fn every_call_mints_its_own_token() {
    let (router, engine) = emulator();

    let request = json!({"workloadName": "repeat.agent", "userId": "user-1"});
    let mut tokens: Vec<String> = Vec::new();
    for attempt in 1..=3 {
        let (status, body) = get_token(&router, "us-east-1", request.clone()).await;
        assert_eq!(
            status, 200,
            "case attempt-{attempt}: expected HTTP 200, got {status} with {body:?}"
        );
        tokens.push(text(&body, "workloadAccessToken").to_string());
    }

    let distinct: BTreeSet<&String> = tokens.iter().collect();
    assert_eq!(
        distinct.len(),
        tokens.len(),
        "expected one token per call, got {} distinct of {}",
        distinct.len(),
        tokens.len()
    );
    assert_eq!(
        issuance_count(&engine),
        tokens.len(),
        "expected every minted token to be recorded as its own issuance"
    );
    let now = SystemTime::now();
    for (index, token) in tokens.iter().enumerate() {
        let held = issuance(&engine, token, now);
        assert_eq!(
            held,
            Some(("repeat.agent".to_string(), Some("user-1".to_string()))),
            "case token-{index}: expected the issuance of the workload and user the call \
             named, got {held:?}"
        );
    }
}

/// Input outside a modelled constraint is refused with a 400 `ValidationException`, never
/// accepted and never answered with a 5xx: one case on each side of `UserIdType`'s 1..128
/// length and of `WorkloadIdentityNameType`'s 3..255 length, plus one outside that name's
/// `^[A-Za-z0-9_.-]+$` pattern and one request missing a required member. A refused
/// request mints nothing.
#[tokio::test]
async fn input_outside_a_modelled_constraint_is_refused_with_400() {
    let (router, engine) = emulator();

    let cases: Vec<(&str, Value)> = vec![
        (
            "empty-user-id",
            json!({"workloadName": "demo.agent", "userId": ""}),
        ),
        (
            "user-id-past-its-length",
            json!({"workloadName": "demo.agent", "userId": "u".repeat(129)}),
        ),
        (
            "workload-name-below-its-length",
            json!({"workloadName": "ab", "userId": "user-1"}),
        ),
        (
            "workload-name-past-its-length",
            json!({"workloadName": "w".repeat(256), "userId": "user-1"}),
        ),
        (
            "workload-name-outside-its-pattern",
            json!({"workloadName": "demo agent!", "userId": "user-1"}),
        ),
        ("no-user-id-at-all", json!({"workloadName": "demo.agent"})),
        ("no-workload-name-at-all", json!({"userId": "user-1"})),
    ];

    for (case, request) in cases {
        let (status, body) = get_token(&router, "us-east-1", request).await;
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

/// The registry's two bounds are materialised on the WRITE path, so the store a caller
/// can grow by asking for tokens cannot grow without end: a later write drops what the
/// clock has already run out on, and at the ceiling the issuance closest to its own expiry
/// gives way while the rest stay resolvable.
#[test]
fn a_write_prunes_what_expired_and_holds_the_ceiling() {
    let ttl = agentcore::state::WORKLOAD_ACCESS_TOKEN_TTL;
    let mut registry = agentcore::state::IssuedWorkloadAccessTokens::default();
    let now = SystemTime::now();

    // Recorded at an instant it was still live, so this write keeps it.
    registry.record(
        "already-expired".to_string(),
        issuance_record("expiring.agent", "user-0", now - ttl - ttl, now - ttl),
        now - ttl - ttl,
    );
    assert_eq!(
        registry.len(),
        1,
        "case nothing due yet: expected the issuance written inside its own window to be \
         held, got {} record(s)",
        registry.len()
    );

    // The next write is owed the expiry the clock already made due.
    registry.record(
        "live".to_string(),
        issuance_record("live.agent", "user-1", now, now + ttl),
        now,
    );
    assert_eq!(
        registry.len(),
        1,
        "case expired: expected the next write to drop the issuance the clock ran out on, \
         got {} record(s)",
        registry.len()
    );
    assert_eq!(
        registry.issued("already-expired", now).map(|held| held.workload_name.clone()),
        None,
        "case expired: an issuance past its window must not resolve"
    );
    assert_eq!(
        registry.issued("live", now).map(|held| held.workload_name.clone()),
        Some("live.agent".to_string()),
        "case live: the issuance inside its window must resolve"
    );

    // At the ceiling the record expiring soonest gives way, so the registry never grows
    // past it however many tokens a caller asks for.
    let ceiling = agentcore::state::MAX_ISSUED_WORKLOAD_ACCESS_TOKENS;
    for index in 0..ceiling {
        let seconds = u64::try_from(index).unwrap_or(0);
        registry.record(
            format!("token-{index}"),
            issuance_record(
                "ceiling.agent",
                "user-2",
                now,
                now + ttl + Duration::from_secs(seconds + 1),
            ),
            now,
        );
    }
    assert_eq!(
        registry.len(),
        ceiling,
        "case at the ceiling: expected at most {ceiling} records, got {}",
        registry.len()
    );
    assert_eq!(
        registry.issued("live", now).map(|held| held.workload_name.clone()),
        None,
        "case at the ceiling: expected the issuance closest to its own expiry to give way"
    );
    assert_eq!(
        registry
            .issued(&format!("token-{}", ceiling - 1), now)
            .map(|held| held.workload_name.clone()),
        Some("ceiling.agent".to_string()),
        "case at the ceiling: expected the newest issuance to stay resolvable"
    );
}

/// `GetWorkloadAccessToken` mints a token for the workload itself: its claims name the
/// workload identity the request addressed and the issuer of the region it was signed for,
/// its subject is that workload rather than any user, and the issuance it records names no
/// user and resolves until its own expiry and no longer.
///
/// Rows differ along the workload name (both published bounds of
/// `WorkloadIdentityNameType`'s 3..255 length, and every character class its
/// `^[A-Za-z0-9_.-]+$` pattern admits) and the signed region, which is what the issuing
/// authority and the resource ARN are computed from (source: smithy_query query="operation"
/// operation="GetWorkloadAccessToken").
#[tokio::test]
async fn a_token_minted_for_the_workload_itself_names_no_user() {
    let (router, engine) = emulator();

    let cases: Vec<(&str, &str, String)> = vec![
        ("shortest-workload", "us-east-1", "abc".to_string()),
        (
            "every-character-class-the-pattern-admits",
            "us-east-1",
            "Agent_0.workload-9".to_string(),
        ),
        ("longest-workload", "eu-west-2", "w".repeat(255)),
        (
            "another-region-signs-another-issuer",
            "ap-southeast-2",
            "payments.agent".to_string(),
        ),
    ];

    let mut minted: BTreeSet<String> = BTreeSet::new();
    for (case, region, workload) in cases {
        let before = SystemTime::now();
        let (status, body) =
            get_workload_token(&router, region, json!({"workloadName": workload})).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from GetWorkloadAccessToken, got {status} with \
             {body:?}"
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

        let (header, claims) = read_token(case, &token);
        assert_eq!(
            text(&header, "alg"),
            "none",
            "case {case:?}: expected an unsecured JWS header, got {header:?}"
        );
        assert_eq!(
            text(&claims, "iss"),
            format!("https://bedrock-agentcore.{region}.amazonaws.com"),
            "case {case:?}: expected the issuer of the signed region, got {claims:?}"
        );
        assert_eq!(
            text(&claims, "aud"),
            format!(
                "arn:aws:bedrock-agentcore:{region}:{ACCOUNT}:workload-identity-directory/default/workload-identity/{workload}"
            ),
            "case {case:?}: expected the workload identity the request named, got {claims:?}"
        );
        assert_eq!(
            text(&claims, "workloadName"),
            workload,
            "case {case:?}: expected the workload the request named, got {claims:?}"
        );
        // This call acts for the workload itself, so the token speaks for no user: its
        // subject is the workload, never a user id.
        assert_eq!(
            text(&claims, "sub"),
            workload,
            "case {case:?}: expected the workload itself as the subject, got {claims:?}"
        );

        let ttl = i64::try_from(agentcore::state::WORKLOAD_ACCESS_TOKEN_TTL.as_secs())
            .expect("the time-to-live fits an i64");
        let issued_at = claims.get("iat").and_then(Value::as_i64).unwrap_or_default();
        let expires_at = claims.get("exp").and_then(Value::as_i64).unwrap_or_default();
        assert_eq!(
            expires_at - issued_at,
            ttl,
            "case {case:?}: expected a window of {ttl} seconds, got {claims:?}"
        );
        let signed_at = i64::try_from(
            before
                .duration_since(SystemTime::UNIX_EPOCH)
                .expect("the clock is past the epoch")
                .as_secs(),
        )
        .expect("the current instant fits an i64");
        assert!(
            (signed_at..=signed_at + 60).contains(&issued_at),
            "case {case:?}: expected the token to be issued at the instant of the call \
             ({signed_at}), got {issued_at}"
        );

        // The issuance was recorded, naming the workload and NO user, and it resolves until
        // its own expiry and no longer.
        let at_issue = issuance(&engine, &token, before);
        assert_eq!(
            at_issue,
            Some((workload.to_string(), None)),
            "case {case:?}: expected the recorded issuance to name the workload and no user, \
             got {at_issue:?}"
        );
        let again = issuance(&engine, &token, before);
        assert_eq!(
            again, at_issue,
            "case {case:?}: expected resolving a token twice to answer alike, got {again:?}"
        );
        let past_expiry = issuance(
            &engine,
            &token,
            before + agentcore::state::WORKLOAD_ACCESS_TOKEN_TTL + Duration::from_secs(1),
        );
        assert_eq!(
            past_expiry, None,
            "case {case:?}: expected the issuance to stop resolving once its window has run \
             out, got {past_expiry:?}"
        );
    }
}

/// Two calls naming the same workload are two tokens, and the mint for the workload itself
/// records a different issuance from the mint that acts for a user, even for one workload:
/// the token is minted per call, never looked up, and whether it speaks for a user is what
/// the call asked for.
#[tokio::test]
async fn the_workload_mint_and_the_user_mint_record_different_issuances() {
    let (router, engine) = emulator();

    let mut for_workload: Vec<String> = Vec::new();
    for attempt in 1..=3 {
        let (status, body) =
            get_workload_token(&router, "us-east-1", json!({"workloadName": "repeat.agent"})).await;
        assert_eq!(
            status, 200,
            "case attempt-{attempt}: expected HTTP 200, got {status} with {body:?}"
        );
        for_workload.push(text(&body, "workloadAccessToken").to_string());
    }
    let distinct: BTreeSet<&String> = for_workload.iter().collect();
    assert_eq!(
        distinct.len(),
        for_workload.len(),
        "expected one token per call, got {} distinct of {}",
        distinct.len(),
        for_workload.len()
    );

    let (status, body) = get_token(
        &router,
        "us-east-1",
        json!({"workloadName": "repeat.agent", "userId": "user-7"}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case the-user-mint: expected HTTP 200, got {status} with {body:?}"
    );
    let for_user = text(&body, "workloadAccessToken").to_string();
    assert!(
        !distinct.contains(&for_user),
        "case the-user-mint: expected a token of its own, got one the workload mint handed out"
    );

    let now = SystemTime::now();
    for (index, token) in for_workload.iter().enumerate() {
        let held = issuance(&engine, token, now);
        assert_eq!(
            held,
            Some(("repeat.agent".to_string(), None)),
            "case token-{index}: expected an issuance naming the workload and no user, got \
             {held:?}"
        );
    }
    let held = issuance(&engine, &for_user, now);
    assert_eq!(
        held,
        Some(("repeat.agent".to_string(), Some("user-7".to_string()))),
        "case the-user-mint: expected the issuance to name the user the call acted for, got \
         {held:?}"
    );
    assert_eq!(
        issuance_count(&engine),
        for_workload.len() + 1,
        "expected every minted token to be recorded as its own issuance"
    );
}

/// A `GetWorkloadAccessToken` request outside a modelled constraint is refused with a 400
/// `ValidationException` and mints nothing: one case on each side of
/// `WorkloadIdentityNameType`'s 3..255 length, one outside its `^[A-Za-z0-9_.-]+$` pattern,
/// and one missing the required member.
#[tokio::test]
async fn the_workload_mint_refuses_input_outside_a_modelled_constraint_with_400() {
    let (router, engine) = emulator();

    let cases: Vec<(&str, Value)> = vec![
        ("workload-name-below-its-length", json!({"workloadName": "ab"})),
        (
            "workload-name-past-its-length",
            json!({"workloadName": "w".repeat(256)}),
        ),
        (
            "workload-name-outside-its-pattern",
            json!({"workloadName": "demo agent!"}),
        ),
        ("no-workload-name-at-all", json!({})),
    ];

    for (case, request) in cases {
        let (status, body) = get_workload_token(&router, "us-east-1", request).await;
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

// --- the minted token presented back to a first-party call --------------------------------

/// `GetResourceApiKey`: `POST /identities/api-key` (source: smithy_query query="operation"
/// operation="GetResourceApiKey": `http`). The first-party call a minted token is exercised
/// against here.
const API_KEY_ROUTE: &str = "/identities/api-key";

/// The sentence AWS refuses a resource credential provider this account does not hold with:
/// fixed text carrying no part of the request. Spelled out here rather than read off the
/// emulator, so the assertion states the expectation instead of restating the code.
const PROVIDER_NOT_FOUND_MESSAGE: &str = "Resource Credential Provider not found";

/// The one sentence the plane refuses a presented WORKLOAD IDENTITY token with, whichever way
/// no live issuance stands behind it.
const WORKLOAD_TOKEN_REFUSED_MESSAGE: &str = "Invalid workload access token";

/// `GetResourceApiKey` with `token` presented for the provider `provider`.
async fn get_resource_api_key(
    router: &MultiServiceRouter,
    token: &str,
    provider: &str,
) -> (u16, Value) {
    post(
        router,
        "us-east-1",
        API_KEY_ROUTE,
        json!({
            "workloadIdentityToken": token,
            "resourceCredentialProviderName": provider,
        }),
    )
    .await
}

/// Record an issuance whose window has already closed, the way a mint records one: a token
/// minted an hour ago for the plane's own time-to-live. The test reaches into the store the
/// handlers share rather than waiting for a clock, which is not an emulator behaviour.
fn aged_issuance(engine: &engine::InMemoryEngine, workload: &str, user: &str) -> String {
    let issued_at = SystemTime::now() - Duration::from_secs(3_600);
    let expires_at = issued_at + agentcore::state::WORKLOAD_ACCESS_TOKEN_TTL;
    let token = agentcore::state::mint_workload_access_token(
        "us-east-1",
        workload,
        Some(user),
        issued_at,
        expires_at,
        11,
    );
    let state = agentcore::state::service_state(engine).expect("the service state");
    state.lock().workload_access_tokens.record(
        token.clone(),
        issuance_record(workload, user, issued_at, expires_at),
        issued_at,
    );
    token
}

/// A token this service never issued, in the form the plane's own mint produces: built here
/// and recorded nowhere.
fn unissued_token(workload: &str, user: &str) -> String {
    let issued_at = SystemTime::now();
    agentcore::state::mint_workload_access_token(
        "us-east-1",
        workload,
        Some(user),
        issued_at,
        issued_at + agentcore::state::WORKLOAD_ACCESS_TOKEN_TTL,
        4_242,
    )
}

/// A token this plane has just minted is ACCEPTED by the plane's first-party
/// resource-credential call: the call gets past the token and is refused only on the
/// credential provider it named, with the fixed sentence AWS words that miss with.
///
/// This engine carries no credential provider at all — those are created by the AgentCore
/// control plane, a different service, and the key an existing one answers is proven end to
/// end over all three services in
/// `core/router/tests/all/bedrock_agentcore_resource_api_key.rs` — so every provider name
/// here is one the account does not hold. The rows span which mint the presented token came
/// from (two distinct tokens of one workload and user, and the mint for the workload itself)
/// and the form of the name asked for.
#[tokio::test]
async fn a_minted_token_is_accepted_by_the_first_party_api_key_call() {
    let (router, engine) = emulator();

    let (status, body) = get_token(
        &router,
        "us-east-1",
        json!({"workloadName": "exchange.agent", "userId": "user-1"}),
    )
    .await;
    assert_eq!(
        status, 200,
        "the first mint must answer 200, got {status} with {body:?}"
    );
    let first = text(&body, "workloadAccessToken").to_string();
    let (status, body) = get_token(
        &router,
        "us-east-1",
        json!({"workloadName": "exchange.agent", "userId": "user-1"}),
    )
    .await;
    assert_eq!(
        status, 200,
        "the second mint must answer 200, got {status} with {body:?}"
    );
    let second = text(&body, "workloadAccessToken").to_string();
    assert_ne!(
        first, second,
        "two mints must hand out two distinct tokens, got the same value twice"
    );
    let (status, body) = get_workload_token(
        &router,
        "us-east-1",
        json!({"workloadName": "exchange.agent"}),
    )
    .await;
    assert_eq!(
        status, 200,
        "the workload's own mint must answer 200, got {status} with {body:?}"
    );
    let for_the_workload = text(&body, "workloadAccessToken").to_string();

    // Rows: the form of the provider name the call asks for — every character class
    // `CredentialProviderName`'s pattern `^[a-zA-Z0-9\-_]+$` admits, and both published
    // bounds of its 1..128 length.
    let names: Vec<(&str, String)> = vec![
        ("a-name-nothing-created", "nonexistent-provider".to_string()),
        (
            "every-character-class-the-pattern-admits",
            "Partner_api-2024".to_string(),
        ),
        ("the-shortest-name-the-shape-admits", "p".to_string()),
        ("the-longest-name-the-shape-admits", "n".repeat(128)),
    ];

    for (token_case, token) in [
        ("the-first-token", &first),
        ("the-second-token", &second),
        ("the-workloads-own-token", &for_the_workload),
    ] {
        for (case, name) in &names {
            let (status, body) = get_resource_api_key(&router, token, name).await;
            assert_eq!(
                status, 404,
                "case {case:?} with {token_case:?}: expected HTTP 404, got {status} with {body:?}"
            );
            assert_eq!(
                error_code(&body),
                "ResourceNotFoundException",
                "case {case:?} with {token_case:?}: expected ResourceNotFoundException, got \
                 {body:?}"
            );
            assert_eq!(
                text(&body, "message"),
                PROVIDER_NOT_FOUND_MESSAGE,
                "case {case:?} with {token_case:?}: expected the fixed sentence, got {body:?}"
            );
            // Fixed text: the refusal quotes neither the name asked for nor the token.
            assert!(
                !text(&body, "message").contains(name.as_str())
                    && !text(&body, "message").contains(token.as_str()),
                "case {case:?} with {token_case:?}: the sentence must carry no part of the \
                 request, got {body:?}"
            );
        }
    }

    // Presenting a token is a READ: the three issuances are still the three the mints
    // recorded, and each still resolves.
    assert_eq!(
        issuance_count(&engine),
        3,
        "the exchanges must record nothing of their own"
    );
    let now = SystemTime::now();
    for (case, token) in [
        ("the-first-token", &first),
        ("the-second-token", &second),
        ("the-workloads-own-token", &for_the_workload),
    ] {
        assert!(
            issuance(&engine, token, now).is_some(),
            "case {case:?}: presenting a token must leave its issuance resolvable"
        );
    }
}

/// A presented value no LIVE issuance of this service stands behind is refused
/// `AccessDeniedException` (HTTP 403) with the fixed sentence of that gate — decided before
/// the provider the request named is looked up, so the refusal says nothing about what the
/// account holds. The workload identity token is a credential the request CARRIES; the
/// modelled `UnauthorizedException` (401) belongs to the caller's own OAuth bearer
/// credential, which this call presents none of.
///
/// The rows span why no live issuance backs the value: one whose window the clock has closed,
/// one well-formed token of the plane's own form that was never issued here, the same token
/// under the bearer scheme, and values that are no token at all.
#[tokio::test]
async fn the_api_key_call_refuses_a_token_no_live_issuance_backs() {
    let (router, engine) = emulator();
    let expired = aged_issuance(&engine, "exchange.agent", "user-1");
    let unissued = unissued_token("exchange.agent", "user-1");

    let cases: Vec<(&str, String)> = vec![
        ("an-issuance-whose-window-has-closed", expired),
        (
            "a-well-formed-token-this-service-never-issued",
            unissued.clone(),
        ),
        (
            "the-same-token-under-the-bearer-scheme",
            format!("Bearer {unissued}"),
        ),
        (
            "three-segments-that-are-not-base64url-json",
            "abc.def.ghi".to_string(),
        ),
        ("a-value-that-is-no-token-at-all", "not-a-token".to_string()),
        (
            "two-segments-rather-than-three",
            "header.claims".to_string(),
        ),
    ];

    for (case, token) in cases {
        let (status, body) = get_resource_api_key(&router, &token, "nonexistent-provider").await;
        assert_eq!(
            status, 403,
            "case {case:?}: expected HTTP 403, got {status} with {body:?}"
        );
        assert_eq!(
            error_code(&body),
            "AccessDeniedException",
            "case {case:?}: expected AccessDeniedException, got {body:?}"
        );
        assert_eq!(
            text(&body, "message"),
            WORKLOAD_TOKEN_REFUSED_MESSAGE,
            "case {case:?}: expected {WORKLOAD_TOKEN_REFUSED_MESSAGE:?}, got {body:?}"
        );
        assert!(
            !text(&body, "message").contains(&token),
            "case {case:?}: the refusal must not quote the presented token, got {body:?}"
        );
    }

    // A token this plane has just minted gets past the token check, which is what makes the
    // refusals above about the token rather than about the call.
    let (status, body) = get_token(
        &router,
        "us-east-1",
        json!({"workloadName": "exchange.agent", "userId": "user-1"}),
    )
    .await;
    assert_eq!(
        status, 200,
        "the fixture mint must answer 200, got {status} with {body:?}"
    );
    let live = text(&body, "workloadAccessToken").to_string();
    let (status, body) = get_resource_api_key(&router, &live, "nonexistent-provider").await;
    assert_eq!(
        status, 404,
        "a live token must get past the token check, got {status} with {body:?}"
    );
}

// --- confirming a user authorization session of the resource-token flow --------------------
//
// `CompleteResourceTokenAuth` is the second half of the plane's OAuth2 resource-token flow:
// the session a `sessionUri` names is a record of this plane's own making, held with the user
// the authorization was started for, and confirming one is accepted only for a session this
// account had issued, inside its own window, for the very user the confirmation acts for.
// Every other session URI is one refusal class, with the single fixed sentence AWS words it
// with, so an unrecognized session is never read as a completed authorization.

/// `CompleteResourceTokenAuth`: `POST /identities/CompleteResourceTokenAuth` (source:
/// smithy_query query="operation" operation="CompleteResourceTokenAuth": `http`).
const COMPLETE_ROUTE: &str = "/identities/CompleteResourceTokenAuth";

/// The one sentence the plane refuses a resource-token session it does not accept with: fixed
/// text carrying no part of the request. Spelled out here rather than read off the emulator,
/// so the assertion states the expectation instead of restating the code.
const SESSION_REFUSED_MESSAGE: &str = "Invalid or expired session";

/// The sentence the plane refuses an EXPIRED user bearer token with.
const EXPIRED_USER_TOKEN_MESSAGE: &str = "Token has expired";

/// The sentence the plane refuses a user bearer token it cannot accept for any other reason
/// with.
const INVALID_USER_TOKEN_MESSAGE: &str = "Invalid JWT token";

/// `CompleteResourceTokenAuth` naming `session_uri`, for the user `user_identifier` resolves
/// to.
async fn complete_resource_token_auth(
    router: &MultiServiceRouter,
    user_identifier: Value,
    session_uri: &str,
) -> (u16, Value) {
    post(
        router,
        "us-east-1",
        COMPLETE_ROUTE,
        json!({"userIdentifier": user_identifier, "sessionUri": session_uri}),
    )
    .await
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
/// non-empty signature segment: the form a caller presents as the user's own identity
/// provider token. Its segments are base64url, so the value is inside `UserTokenType`'s
/// `@pattern`.
fn presented_user_token(claims: &Value) -> String {
    format!(
        "{}.{}.{}",
        service_common::jwt::encode_base64url(br#"{"alg":"RS256","typ":"JWT","kid":"k1"}"#),
        service_common::jwt::encode_base64url(claims.to_string().as_bytes()),
        service_common::jwt::encode_base64url(b"presented-signature")
    )
}

/// A user bearer token for `subject`, inside its own window at this instant.
fn user_token_for(subject: &str) -> String {
    let seconds = numeric_date(SystemTime::now());
    presented_user_token(&json!({"sub": subject, "iat": seconds - 30, "exp": seconds + 600}))
}

/// Hand out one PENDING user authorization session for `user`, the way the initiation of the
/// flow records one: handed out `age` ago, for the plane's own window. The test reaches into
/// the store the handlers share rather than waiting for a clock, which is not an emulator
/// behaviour.
fn handed_out_session(
    engine: &engine::InMemoryEngine,
    user: &str,
    age: Duration,
    seq: u64,
) -> String {
    let issued_at = SystemTime::now() - age;
    let session_uri = agentcore::state::mint_resource_token_session_uri(issued_at, seq);
    let state = agentcore::state::service_state(engine).expect("the service state");
    state.lock().resource_token_auth_sessions.record(
        session_uri.clone(),
        agentcore::state::ResourceTokenAuthSession {
            workload_name: "exchange.agent".to_string(),
            resource_credential_provider_name: "partner-api".to_string(),
            user_id: user.to_string(),
            issued_at,
            expires_at: issued_at + agentcore::state::RESOURCE_TOKEN_AUTH_SESSION_TTL,
            confirmed_at: None,
            seq,
        },
        issued_at,
    );
    session_uri
}

/// When the session `session_uri` was confirmed, as the store holds it at this instant, and
/// `None` while it is still pending or no live session stands behind the URI at all.
fn session_confirmed_at(
    engine: &engine::InMemoryEngine,
    session_uri: &str,
) -> Option<SystemTime> {
    let state = agentcore::state::service_state(engine).expect("the service state");
    let store = state.lock();
    store
        .resource_token_auth_sessions
        .issued(session_uri, SystemTime::now())
        .and_then(|held| held.confirmed_at)
}

/// Whether the store holds a live session under `session_uri` at this instant.
fn session_is_live(engine: &engine::InMemoryEngine, session_uri: &str) -> bool {
    let state = agentcore::state::service_state(engine).expect("the service state");
    let store = state.lock();
    store
        .resource_token_auth_sessions
        .issued(session_uri, SystemTime::now())
        .is_some()
}

/// A session this account had issued is confirmed for the user it was started for: the call
/// answers the modelled empty 200 output and the confirmation is written, so a later read of
/// the session carries it.
///
/// The rows differ along the form of the `userIdentifier` (a `userId`, and the user's own
/// bearer token, whose subject is the user) and the form of the user itself (both published
/// bounds of `UserIdType`'s 1..128 length, an opaque identifier and one that is an address;
/// source: smithy_query query="shape" shape="UserIdentifier"), and along how long ago the
/// session was handed out.
#[tokio::test]
async fn a_session_this_account_issued_is_confirmed_for_its_own_user() {
    let (router, engine) = emulator();

    let long_user = "u".repeat(128);
    let cases: Vec<(&str, String, bool, Duration)> = vec![
        (
            "a user id, handed out a moment ago",
            "sim2real-test".to_string(),
            false,
            Duration::from_secs(1),
        ),
        (
            "the shortest user id the shape admits",
            "u".to_string(),
            false,
            Duration::from_secs(30),
        ),
        (
            "the longest user id the shape admits",
            long_user,
            false,
            Duration::from_secs(5),
        ),
        (
            "an opaque user id, confirmed with the user's own token",
            "a3f1c0de-0000-4a1b-9c2d-5e6f70819203".to_string(),
            true,
            Duration::from_secs(10),
        ),
        (
            "a user id that is an address, confirmed with the user's own token",
            "user@example.com".to_string(),
            true,
            Duration::from_secs(60),
        ),
    ];

    for (index, (case, user, by_token, age)) in cases.into_iter().enumerate() {
        let seq = u64::try_from(index).unwrap_or(0) + 1;
        let session_uri = handed_out_session(&engine, &user, age, seq);
        assert_eq!(
            session_confirmed_at(&engine, &session_uri),
            None,
            "case {case:?}: expected the handed-out session to be pending"
        );
        let identifier = if by_token {
            json!({"userToken": user_token_for(&user)})
        } else {
            json!({"userId": user})
        };

        let (status, body) =
            complete_resource_token_auth(&router, identifier.clone(), &session_uri).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} with {body:?}"
        );
        // The modelled output declares no member at all, so the answer carries none.
        assert!(
            body.as_object().is_none_or(|members| members.is_empty()),
            "case {case:?}: expected an empty output, got {body:?}"
        );
        let confirmed = session_confirmed_at(&engine, &session_uri);
        assert!(
            confirmed.is_some(),
            "case {case:?}: expected the confirmation to be recorded on the session, got \
             {confirmed:?}"
        );

        // Confirming the same session again states the same confirmation rather than a second
        // one: the instant the first call wrote stands.
        let (status, body) =
            complete_resource_token_auth(&router, identifier, &session_uri).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected confirming twice to answer 200, got {status} with {body:?}"
        );
        assert_eq!(
            session_confirmed_at(&engine, &session_uri),
            confirmed,
            "case {case:?}: expected the instant of the first confirmation to stand"
        );

        // Only this session was touched: a session of another user handed out beside it is
        // still pending.
        let other = handed_out_session(&engine, "someone-else", Duration::from_secs(1), seq + 100);
        assert_eq!(
            session_confirmed_at(&engine, &other),
            None,
            "case {case:?}: expected another user's session to stay pending"
        );
    }
}

/// A `sessionUri` no live authorization of this account stands behind is refused
/// `AccessDeniedException` (HTTP 403) with the one fixed sentence of that gate, and confirms
/// nothing — a well-formed URI inside the modelled pattern that no initiation ever handed out
/// included, which is the whole point of the gate: an unrecognized session is not a completed
/// authorization.
///
/// The rows span why no live session backs the URI: two well-formed URIs this plane never
/// handed out (one at the longest length the shape admits), a session whose window the clock
/// has closed, and a live session handed out for ANOTHER user, asked for both by that other
/// user's id and by that other user's own bearer token.
#[tokio::test]
async fn a_session_no_live_authorization_backs_is_refused_with_403() {
    let (router, engine) = emulator();

    let closed = handed_out_session(
        &engine,
        "sim2real-test",
        agentcore::state::RESOURCE_TOKEN_AUTH_SESSION_TTL + Duration::from_secs(60),
        1,
    );
    let someone_elses = handed_out_session(&engine, "user-1", Duration::from_secs(5), 2);
    // Inside `RequestUri`'s `^urn:ietf:params:oauth:request_uri:[a-zA-Z0-9-._~]+$`, so the
    // request reaches the handler and is refused by the gate rather than by a constraint.
    let never_issued = "urn:ietf:params:oauth:request_uri:9f8e7d6c5b4a39281706f5e4d3c2b1a0";
    let longest = format!(
        "urn:ietf:params:oauth:request_uri:{}",
        "a-b._c~0".repeat(123)
    );

    let cases: Vec<(&str, String, Value)> = vec![
        (
            "a-well-formed-uri-no-initiation-ever-handed-out",
            never_issued.to_string(),
            json!({"userId": "sim2real-test"}),
        ),
        (
            "the-longest-uri-the-shape-admits-handed-out-by-nobody",
            longest.clone(),
            json!({"userId": "sim2real-test"}),
        ),
        (
            "a-session-whose-window-the-clock-has-closed",
            closed.clone(),
            json!({"userId": "sim2real-test"}),
        ),
        (
            "a-session-handed-out-for-another-user",
            someone_elses.clone(),
            json!({"userId": "sim2real-test"}),
        ),
        (
            "another-users-session-asked-for-with-a-token-of-a-third-subject",
            someone_elses.clone(),
            json!({"userToken": user_token_for("user-2")}),
        ),
    ];

    assert!(
        longest.len() <= 1_024,
        "the longest row must stay inside the shape's 1..1024 length, got {}",
        longest.len()
    );

    for (case, session_uri, identifier) in cases {
        let (status, body) =
            complete_resource_token_auth(&router, identifier, &session_uri).await;
        assert_eq!(
            status, 403,
            "case {case:?}: expected HTTP 403, got {status} with {body:?}"
        );
        assert_eq!(
            error_code(&body),
            "AccessDeniedException",
            "case {case:?}: expected AccessDeniedException, got {body:?}"
        );
        assert_eq!(
            text(&body, "message"),
            SESSION_REFUSED_MESSAGE,
            "case {case:?}: expected {SESSION_REFUSED_MESSAGE:?}, got {body:?}"
        );
        // Fixed text: the refusal quotes neither the session asked about nor the user.
        assert!(
            !text(&body, "message").contains(session_uri.as_str()),
            "case {case:?}: the sentence must carry no part of the request, got {body:?}"
        );
    }

    // Nothing was confirmed: the other user's session is still pending, and the closed one is
    // still closed.
    assert_eq!(
        session_confirmed_at(&engine, &someone_elses),
        None,
        "a refused confirmation must leave another user's session pending"
    );
    assert!(
        !session_is_live(&engine, &closed),
        "a refused confirmation must not revive a session the clock has closed"
    );

    // The gate is about the session, not about the call: the same request for the session this
    // user really holds is confirmed.
    let own = handed_out_session(&engine, "sim2real-test", Duration::from_secs(2), 3);
    let (status, body) =
        complete_resource_token_auth(&router, json!({"userId": "sim2real-test"}), &own).await;
    assert_eq!(
        status, 200,
        "the user's own live session must be confirmed, got {status} with {body:?}"
    );
}

/// A `userIdentifier` carrying a user bearer token this plane cannot accept is refused
/// `UnauthorizedException` (HTTP 401) in the sentence of its own class — the token is a value
/// to be validated, never one to be trusted — and the session the request named stays pending.
///
/// The rows span why the token is not accepted: expired, expiring at this very instant, naming
/// no expiry at all, naming no subject, unsecured (so nothing signed it), and three segments
/// that are no compact JWS.
#[tokio::test]
async fn a_user_token_this_plane_cannot_accept_is_refused_with_401() {
    let (router, engine) = emulator();
    let seconds = numeric_date(SystemTime::now());
    let session_uri = handed_out_session(&engine, "user-1", Duration::from_secs(1), 1);

    let unsecured = format!(
        "{}.{}.",
        service_common::jwt::encode_base64url(br#"{"alg":"none","typ":"JWT"}"#),
        service_common::jwt::encode_base64url(
            json!({"sub": "user-1", "exp": seconds + 600}).to_string().as_bytes()
        )
    );

    let cases: Vec<(&str, String, &str)> = vec![
        (
            "expired-a-moment-ago",
            presented_user_token(&json!({"sub": "user-1", "exp": seconds - 1})),
            EXPIRED_USER_TOKEN_MESSAGE,
        ),
        (
            "expiring-at-this-very-instant",
            presented_user_token(&json!({"sub": "user-1", "exp": seconds})),
            EXPIRED_USER_TOKEN_MESSAGE,
        ),
        (
            "naming-no-expiry",
            presented_user_token(&json!({"sub": "user-1"})),
            INVALID_USER_TOKEN_MESSAGE,
        ),
        (
            "naming-no-subject",
            presented_user_token(&json!({"exp": seconds + 600})),
            INVALID_USER_TOKEN_MESSAGE,
        ),
        ("unsecured-so-nothing-signed-it", unsecured, INVALID_USER_TOKEN_MESSAGE),
        (
            "three-segments-that-are-not-base64url-json",
            "abc.def.ghi".to_string(),
            INVALID_USER_TOKEN_MESSAGE,
        ),
    ];

    for (case, token, expected) in cases {
        let (status, body) =
            complete_resource_token_auth(&router, json!({"userToken": token}), &session_uri).await;
        assert_eq!(
            status, 401,
            "case {case:?}: expected HTTP 401, got {status} with {body:?}"
        );
        assert_eq!(
            error_code(&body),
            "UnauthorizedException",
            "case {case:?}: expected UnauthorizedException, got {body:?}"
        );
        assert_eq!(
            text(&body, "message"),
            expected,
            "case {case:?}: expected {expected:?}, got {body:?}"
        );
        assert!(
            !text(&body, "message").contains(&token),
            "case {case:?}: the refusal must not quote the presented token, got {body:?}"
        );
        assert_eq!(
            session_confirmed_at(&engine, &session_uri),
            None,
            "case {case:?}: a refused confirmation must leave the session pending"
        );
    }
}

/// A `CompleteResourceTokenAuth` request outside a modelled constraint is refused with a 400
/// `ValidationException` and confirms nothing: `sessionUri` outside its
/// `^urn:ietf:params:oauth:request_uri:[a-zA-Z0-9-._~]+$` pattern, past its 1..1024 length,
/// and each required member missing (source: smithy_query query="operation"
/// operation="CompleteResourceTokenAuth").
#[tokio::test]
async fn the_confirmation_refuses_input_outside_a_modelled_constraint_with_400() {
    let (router, engine) = emulator();
    let session_uri = handed_out_session(&engine, "user-1", Duration::from_secs(1), 1);

    let past_length = format!("urn:ietf:params:oauth:request_uri:{}", "a".repeat(991));
    let cases: Vec<(&str, Value)> = vec![
        (
            "session-uri-outside-its-pattern",
            json!({"userIdentifier": {"userId": "user-1"}, "sessionUri": "not-a-request-uri"}),
        ),
        (
            "session-uri-carrying-a-character-the-pattern-refuses",
            json!({
                "userIdentifier": {"userId": "user-1"},
                "sessionUri": "urn:ietf:params:oauth:request_uri:has spaces",
            }),
        ),
        (
            "session-uri-past-its-length",
            json!({"userIdentifier": {"userId": "user-1"}, "sessionUri": past_length}),
        ),
        (
            "no-session-uri-at-all",
            json!({"userIdentifier": {"userId": "user-1"}}),
        ),
        ("no-user-identifier-at-all", json!({"sessionUri": session_uri})),
        ("neither-member-at-all", json!({})),
    ];

    for (case, request) in cases {
        let (status, body) = post(&router, "us-east-1", COMPLETE_ROUTE, request).await;
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
            session_confirmed_at(&engine, &session_uri),
            None,
            "case {case:?}: a refused request must confirm nothing"
        );
    }
}
