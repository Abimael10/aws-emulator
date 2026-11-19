// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the AgentCore browser session lifecycle:
//! `StartBrowserSession`, `GetBrowserSession` and the OS-level actions `InvokeBrowser`
//! performs on a session.
//!
//! The rule under test: a start MINTS a `sessionId` matching its modelled pattern,
//! persists the configuration the request asked for (name, time-to-live, viewport,
//! extensions, enterprise policies, profile, proxy, certificates, file system
//! configurations) under the named browser together with the instant it was created, and
//! reports that stored record back; a replayed `clientToken` answers with the session
//! that token already started instead of starting a second one; a read reports the stored
//! record with the status the clock implies — `READY` until the stored time-to-live has
//! run out, `TERMINATED` from then on; and a browser is the CONTROL plane's resource, so
//! a session can only be started on, or read from, a browser that component holds.
//!
//! And the rule the action half is under: every action `InvokeBrowser` performs happens on
//! the session's own viewport — a screenshot is a real PNG of exactly those dimensions, a
//! pointer action off that screen is reported FAILED in band, and an action on a session
//! that is no longer running is refused.
//!
//! Every case drives real HTTP through the same `MultiServiceRouter` the emulator
//! serves with, so the wire shape (routes, status codes, `restJson1` bodies, modelled
//! error codes) is exercised, not just the handler functions.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;
use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use aws_smithy_types::date_time::{DateTime, Format};
use service_com_amazonaws_bedrockagentcore_amazonbedrockagentcore as agentcore;
use service_com_amazonaws_bedrockagentcorecontrol_amazonbedrockagentcorecontrol as agentcorecontrol;

use super::harness::{age_browser_session, label};

/// The browser tool the service itself provides, held by every account without anything
/// creating it: the control plane's `BrowserId` pattern reserves this id as its first
/// alternative (source: smithy_query query="shape" shape="BrowserId" on the control
/// plane's model).
const SYSTEM_BROWSER: &str = "aws.browser.v1";

/// The `name` a case creates its own browser under, through the control plane that owns
/// browsers. `CreateBrowser$name` is `SandboxName`, pattern ^[a-zA-Z][a-zA-Z0-9_]{0,47}$
/// (source: smithy_query query="operation" operation="CreateBrowser" on the control
/// plane's model).
const CUSTOM_BROWSER_NAME: &str = "demo_browser";
/// A second browser name, for a case that needs two browsers that both exist.
const OTHER_BROWSER_NAME: &str = "demo_other_browser";

/// A `browserId`-shaped identifier nothing ever created, which therefore names no
/// browser.
const MISSING_BROWSER: &str = "demo_missing_browser-0a1b2c3d4e";

/// The emulator, with request authentication off so a test signs nothing, plus the
/// shared engine so a case can age a stored session's clock without sleeping.
///
/// The AgentCore CONTROL plane is mounted beside this service because it owns the
/// browsers a session runs on: a case that needs a custom browser creates a real one
/// through `CreateBrowser` rather than naming a browser nothing holds.
fn emulator() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(agentcore::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(agentcorecontrol::EmulatedService::new(Arc::clone(&engine)));
    (router, engine)
}

/// Send one request and return its status and parsed JSON body (`Value::Null` for an
/// empty body). The SigV4 credential scope is what routes the request to this service.
async fn send(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let (status, parsed, _headers) = dispatch(router, method, path, body, &[]).await;
    (status, parsed)
}

/// Send one request carrying `headers` beyond the ones every request here carries, and
/// return its status, parsed JSON body and response headers — which is where an
/// `@httpHeader`-bound member travels.
async fn dispatch(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<Value>,
    headers: &[(&str, &str)],
) -> (u16, Value, http::HeaderMap) {
    let payload = body.map_or_else(bytes::Bytes::new, |v| bytes::Bytes::from(v.to_string()));
    let mut builder = http::Request::builder()
        .method(method)
        .uri(format!("https://bedrock-agentcore.us-east-1.amazonaws.com{path}"))
        .header(
            "authorization",
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/us-east-1/bedrock-agentcore/aws4_request, \
             SignedHeaders=host, Signature=0",
        )
        .header("content-type", "application/json");
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let request = builder.body(payload).expect("build the request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, parsed, response.headers().clone())
}

/// Create one custom browser through the control plane and return the `browserId` it
/// minted. `CreateBrowser` is `PUT /browsers`, answering 202 (source: smithy_query
/// query="operation" operation="CreateBrowser" on the control plane's model).
async fn create_browser(router: &MultiServiceRouter, name: &str) -> String {
    let (status, created) = send(
        router,
        "PUT",
        "/browsers",
        Some(json!({"name": name, "networkConfiguration": {"networkMode": "PUBLIC"}})),
    )
    .await;
    assert_eq!(
        status, 202,
        "the fixture browser must be created, got {status} with {created:?}"
    );
    created["browserId"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// `StartBrowserSession`: `PUT /browsers/{browserIdentifier}/sessions/start`.
async fn start_session(router: &MultiServiceRouter, browser: &str, body: Value) -> (u16, Value) {
    send(
        router,
        "PUT",
        &format!("/browsers/{}/sessions/start", label(browser)),
        Some(body),
    )
    .await
}

/// `GetBrowserSession`: `GET /browsers/{browserIdentifier}/sessions/get?sessionId=…`.
async fn get_session(
    router: &MultiServiceRouter,
    browser: &str,
    session_id: &str,
) -> (u16, Value) {
    send(
        router,
        "GET",
        &format!(
            "/browsers/{}/sessions/get?sessionId={}",
            label(browser),
            label(session_id)
        ),
        None,
    )
    .await
}

/// How many sessions this emulator holds for one browser — the check that a replayed
/// `clientToken` started nothing new, and that a refused start stored nothing.
fn session_count(engine: &engine::InMemoryEngine, browser: &str) -> usize {
    let state = agentcore::state::service_state(engine).expect("the service state");
    let store = state.lock();
    store
        .browsers
        .get(browser)
        .map_or(0, |held| held.sessions.len())
}

/// `BrowserSessionId`'s modelled pattern, `^[0-9a-zA-Z]{1,40}$`, checked without a regex
/// dependency.
fn is_modelled_session_id(id: &str) -> bool {
    (1..=40).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric())
}

fn error_code(body: &Value) -> &str {
    body.get("__type").and_then(Value::as_str).unwrap_or("")
}

fn message(body: &Value) -> &str {
    body.get("message").and_then(Value::as_str).unwrap_or("")
}

/// A `DateTimestamp` member carries `@timestampFormat("date-time")`, so on this
/// service's restJson1 wire it is an RFC 3339 string rather than an epoch number
/// (source: smithy_query query="shape" shape="DateTimestamp").
fn assert_date_time(case: &str, member_name: &str, value: &Value) {
    let text = value.as_str().unwrap_or_else(|| {
        panic!("case {case:?}: expected {member_name} as a date-time string, got {value:?}")
    });
    assert!(
        DateTime::from_str(text, Format::DateTime).is_ok(),
        "case {case:?}: expected {member_name} to parse as RFC 3339, got {text:?}"
    );
}

fn member<'a>(body: &'a Value, name: &str) -> &'a Value {
    body.get(name).unwrap_or(&Value::Null)
}

fn text<'a>(body: &'a Value, name: &str) -> &'a str {
    member(body, name).as_str().unwrap_or("")
}

/// One certificate, as `Certificate$location` models it: the `secretsManager` member of
/// the `CertificateLocation` union (source: smithy_query query="shape"
/// shape="CertificateLocation").
fn certificate(secret: &str) -> Value {
    json!({"location": {"secretsManager": {"secretArn": secret}}})
}

/// An Amazon S3 resource location, as `ResourceLocation`'s only member models it
/// (source: smithy_query query="shape" shape="ResourceLocation,S3Location").
fn s3_location(bucket: &str, prefix: &str) -> Value {
    json!({"s3": {"bucket": bucket, "prefix": prefix}})
}

/// Every configuration member a start accepts is reported back by a read, for rows that
/// differ along which members were supplied, their sizes and the published bounds of the
/// values they carry.
#[tokio::test]
async fn a_started_session_round_trips_through_get_browser_session() {
    let (router, _engine) = emulator();
    let custom = create_browser(&router, CUSTOM_BROWSER_NAME).await;

    // Rows differ along the browser, whether `name` was supplied and how long it is, the
    // time-to-live (omitted so the service's own default applies, both published bounds
    // of `BrowserSessionTimeout` 1..28800, and the documented recommended minimum), the
    // viewport (both published bounds of `ViewPortWidth` 320..3840 and `ViewPortHeight`
    // 240..2160), and which of the remaining optional members were supplied.
    let cases: Vec<(&str, String, Value, i64)> = vec![
        (
            "no-optional-members-runs-under-the-service-default-timeout",
            SYSTEM_BROWSER.to_string(),
            json!({}),
            900,
        ),
        (
            "named-at-the-lowest-timeout-and-the-smallest-viewport",
            SYSTEM_BROWSER.to_string(),
            json!({
                "name": "a",
                "sessionTimeoutSeconds": 1,
                "viewPort": {"width": 320, "height": 240},
            }),
            1,
        ),
        (
            "longest-name-at-the-highest-timeout-and-the-largest-viewport",
            custom.clone(),
            json!({
                "name": "n".repeat(100),
                "sessionTimeoutSeconds": 28_800,
                "viewPort": {"width": 3_840, "height": 2_160},
            }),
            28_800,
        ),
        (
            "recommended-minimum-timeout-with-two-certificates",
            custom.clone(),
            json!({
                "name": "research session",
                "sessionTimeoutSeconds": 60,
                "viewPort": {"width": 1_280, "height": 720},
                "certificates": [
                    certificate("arn:aws:secretsmanager:us-east-1:123456789012:secret:ca-one"),
                    certificate("arn:aws:secretsmanager:us-east-1:123456789012:secret:ca-two"),
                ],
            }),
            60,
        ),
        (
            "extensions-enterprise-policies-and-a-profile",
            SYSTEM_BROWSER.to_string(),
            json!({
                "sessionTimeoutSeconds": 1_800,
                "extensions": [{"location": s3_location("agent-extensions", "ad-blocker.crx")}],
                // `BrowserEnterprisePolicyType` values: MANAGED | RECOMMENDED (source:
                // smithy_query query="shape" shape="BrowserEnterprisePolicyType").
                "enterprisePolicies": [
                    {"location": s3_location("agent-policies", "managed.json"), "type": "MANAGED"},
                    {"location": s3_location("agent-policies", "recommended.json"), "type": "RECOMMENDED"},
                ],
                // `BrowserProfileId`'s pattern is
                // ^[a-zA-Z][a-zA-Z0-9_]{0,47}-[a-zA-Z0-9]{10}$ (source: smithy_query
                // query="shape" shape="BrowserProfileConfiguration").
                "profileConfiguration": {"profileIdentifier": "shopping_profile-0a1b2c3d4e"},
            }),
            1_800,
        ),
        (
            "a-proxy-with-credentials-and-a-bypass",
            custom.clone(),
            json!({
                "sessionTimeoutSeconds": 7_200,
                // `ExternalProxy`: server is a DNS hostname, port 1..65535 (source:
                // smithy_query query="shape" shape="ExternalProxy").
                "proxyConfiguration": {
                    "proxies": [{"externalProxy": {
                        "server": "proxy.example.com",
                        "port": 8_080,
                        "domainPatterns": [".example.com", "example.net"],
                        "credentials": {"basicAuth": {
                            "secretArn": "arn:aws:secretsmanager:us-east-1:123456789012:secret:proxy",
                        }},
                    }}],
                    "bypass": {"domainPatterns": ["internal.example.org"]},
                },
                "enterprisePolicies": [],
            }),
            7_200,
        ),
        (
            "both-filesystem-union-members-mounted",
            SYSTEM_BROWSER.to_string(),
            json!({
                "sessionTimeoutSeconds": 900,
                // The ARN forms `S3FilesConfiguration` and `EfsConfiguration` publish as
                // member patterns (source: smithy_query query="shape"
                // shape="S3FilesConfiguration,EfsConfiguration").
                "filesystemConfigurations": [
                    {"s3FilesConfiguration": {
                        "accessPointArn": "arn:aws:s3files:us-east-1:123456789012:file-system/fs-0123456789abcdef0/access-point/fsap-0123456789abcdef0",
                        "mountPath": "/mnt/s3",
                        "fileSystemArn": "arn:aws:s3files:us-east-1:123456789012:file-system/fs-0123456789abcdef0",
                    }},
                    {"efsConfiguration": {
                        "accessPointArn": "arn:aws:elasticfilesystem:us-east-1:123456789012:access-point/fsap-0123456789abcdef0",
                        "mountPath": "/mnt/efs",
                        "fileSystemArn": "arn:aws:elasticfilesystem:us-east-1:123456789012:file-system/fs-0123456789abcdef0",
                    }},
                ],
            }),
            900,
        ),
    ];

    let mut minted: BTreeSet<String> = BTreeSet::new();
    for (case, browser, request, expected_timeout) in cases {
        let (status, started) = start_session(&router, &browser, request.clone()).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from StartBrowserSession, got {status} with {started:?}"
        );
        assert_eq!(
            text(&started, "browserIdentifier"),
            browser,
            "case {case:?}: expected the browser the request named, got {started:?}"
        );
        let session_id = text(&started, "sessionId").to_string();
        assert!(
            is_modelled_session_id(&session_id),
            "case {case:?}: expected a sessionId matching ^[0-9a-zA-Z]{{1,40}}$, got {session_id:?}"
        );
        assert!(
            minted.insert(session_id.clone()),
            "case {case:?}: expected a sessionId no earlier start minted, got {session_id:?}"
        );
        let created_at = member(&started, "createdAt").clone();
        assert_date_time(case, "createdAt", &created_at);

        let (status, read) = get_session(&router, &browser, &session_id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from GetBrowserSession, got {status} with {read:?}"
        );
        assert_eq!(
            text(&read, "sessionId"),
            session_id,
            "case {case:?}: expected the started session back, got {read:?}"
        );
        assert_eq!(
            text(&read, "browserIdentifier"),
            browser,
            "case {case:?}: expected the browser the session was started on, got {read:?}"
        );
        // The creation instant survives the round trip character for character, so no
        // precision of the stored instant is lost on the way out.
        assert_eq!(
            member(&read, "createdAt"),
            &created_at,
            "case {case:?}: expected the creation instant the start reported, got {read:?}"
        );
        assert_date_time(case, "createdAt (read back)", member(&read, "createdAt"));
        assert_eq!(
            member(&read, "lastUpdatedAt"),
            &created_at,
            "case {case:?}: expected a session nothing has changed to report its creation instant as lastUpdatedAt, got {read:?}"
        );
        assert_eq!(
            member(&read, "name"),
            request.get("name").unwrap_or(&Value::Null),
            "case {case:?}: expected the name the request asked for, got {read:?}"
        );
        assert_eq!(
            member(&read, "sessionTimeoutSeconds").as_i64(),
            Some(expected_timeout),
            "case {case:?}: expected the stored time-to-live {expected_timeout}, got {read:?}"
        );
        // Every remaining configuration member is reported exactly as it was asked for,
        // and a member the request omitted is reported by no value at all.
        for configuration in [
            "viewPort",
            "extensions",
            "enterprisePolicies",
            "profileConfiguration",
            "proxyConfiguration",
            "certificates",
            "filesystemConfigurations",
        ] {
            assert_eq!(
                member(&read, configuration),
                request.get(configuration).unwrap_or(&Value::Null),
                "case {case:?}: expected {configuration} as the request asked for it, got {read:?}"
            );
        }
        assert_eq!(
            text(&read, "status"),
            "READY",
            "case {case:?}: expected a session inside its time-to-live to be READY, got {read:?}"
        );

        // A read changes nothing a later caller sees.
        let (status, again) = get_session(&router, &browser, &session_id).await;
        assert_eq!(
            (status, &again),
            (200, &read),
            "case {case:?}: expected a second read to report the same session, got {status} with {again:?}"
        );
    }
}

/// A session reports `READY` until the time-to-live the request asked for has run out,
/// and `TERMINATED` from that instant on — whatever that time-to-live is.
#[tokio::test]
async fn session_status_follows_the_time_to_live_the_request_asked_for() {
    let (router, engine) = emulator();

    // `BrowserSessionTimeout` is range 1..28800 (source: smithy_query query="operation"
    // operation="StartBrowserSession"): both published bounds, the documented recommended
    // minimum, and one value in between. A request that names NO timeout runs under the
    // service's own default instead, which `browser_session_streams.rs` covers.
    for timeout in [1_i64, 60, 1_200, 28_800] {
        let case = format!("timeout-{timeout}");
        let (status, started) =
            start_session(&router, SYSTEM_BROWSER, json!({"sessionTimeoutSeconds": timeout})).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from StartBrowserSession, got {status} with {started:?}"
        );
        let session_id = text(&started, "sessionId").to_string();

        // One second short of the time-to-live the session is still usable.
        let timeout_secs = u64::try_from(timeout).expect("a modelled timeout is positive");
        age_browser_session(
            &engine,
            SYSTEM_BROWSER,
            &session_id,
            Duration::from_secs(timeout_secs - 1),
        );
        let (status, read) = get_session(&router, SYSTEM_BROWSER, &session_id).await;
        assert_eq!(
            (status, text(&read, "status")),
            (200, "READY"),
            "case {case:?}: expected READY one second before the time-to-live runs out, got {status} with {read:?}"
        );

        // At the instant it runs out the session is terminated, with no call needed to
        // make it so.
        age_browser_session(&engine, SYSTEM_BROWSER, &session_id, Duration::from_secs(1));
        let (status, read) = get_session(&router, SYSTEM_BROWSER, &session_id).await;
        assert_eq!(
            (status, text(&read, "status")),
            (200, "TERMINATED"),
            "case {case:?}: expected TERMINATED once the time-to-live has run out, got {status} with {read:?}"
        );
        assert_eq!(
            member(&read, "sessionTimeoutSeconds").as_i64(),
            Some(timeout),
            "case {case:?}: expected a terminated session to still report its time-to-live, got {read:?}"
        );
    }
}

/// `clientToken` is an `@idempotencyToken` on an `@idempotent` operation: replaying one
/// answers with the session it already started, and starts nothing new. Two starts that
/// carry no token are two sessions.
#[tokio::test]
async fn a_replayed_client_token_answers_with_the_session_it_already_started() {
    let (router, engine) = emulator();
    let browser = create_browser(&router, CUSTOM_BROWSER_NAME).await;

    // `ClientToken` is len 33..256, pattern ^[a-zA-Z0-9](-*[a-zA-Z0-9]){0,256}$ (source:
    // smithy_query query="operation" operation="StartBrowserSession"). Rows differ along
    // the token's length and the body sent with the replay, which the service ignores for
    // a token it has already seen.
    let cases: Vec<(&str, String, Value, Value)> = vec![
        (
            "shortest-token-identical-replay",
            "a".repeat(33),
            json!({"name": "first", "sessionTimeoutSeconds": 120}),
            json!({"name": "first", "sessionTimeoutSeconds": 120}),
        ),
        (
            "hyphenated-token-replayed-with-a-different-body",
            "replay-token-0123456789-abcdefghij-0".to_string(),
            json!({"name": "second", "viewPort": {"width": 1_024, "height": 768}}),
            json!({"name": "ignored on a replay", "viewPort": {"width": 800, "height": 600}}),
        ),
        (
            "longest-token-replay",
            "z".repeat(256),
            json!({"sessionTimeoutSeconds": 3_600}),
            json!({"sessionTimeoutSeconds": 3_600}),
        ),
    ];

    let mut expected_sessions = 0;
    for (case, token, first_body, replay_body) in cases {
        let mut first_request = first_body.clone();
        first_request["clientToken"] = json!(token);
        let (status, started) = start_session(&router, &browser, first_request).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from the first start, got {status} with {started:?}"
        );
        expected_sessions += 1;

        let mut replay_request = replay_body;
        replay_request["clientToken"] = json!(token);
        let (status, replayed) = start_session(&router, &browser, replay_request).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from the replay, got {status} with {replayed:?}"
        );
        assert_eq!(
            replayed, started,
            "case {case:?}: expected the replay to answer with the session the token already started, got {replayed:?}"
        );
        assert_eq!(
            session_count(&engine, &browser),
            expected_sessions,
            "case {case:?}: expected the replay to start no second session"
        );

        // The configuration the replay carried is ignored: the stored session is still
        // the one the first call asked for.
        let session_id = text(&started, "sessionId").to_string();
        let (status, read) = get_session(&router, &browser, &session_id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the started session to be readable, got {status} with {read:?}"
        );
        assert_eq!(
            member(&read, "createdAt"),
            member(&started, "createdAt"),
            "case {case:?}: expected the creation instant of the first start, got {read:?}"
        );
        assert_eq!(
            member(&read, "viewPort"),
            first_body.get("viewPort").unwrap_or(&Value::Null),
            "case {case:?}: expected the viewport the FIRST start asked for, got {read:?}"
        );
    }

    // Two starts with no token at all are two sessions.
    let (_, first) = start_session(&router, &browser, json!({})).await;
    let (_, second) = start_session(&router, &browser, json!({})).await;
    assert_ne!(
        text(&first, "sessionId"),
        text(&second, "sessionId"),
        "two starts carrying no clientToken must be two sessions, got {first:?} and {second:?}"
    );
    assert_eq!(
        session_count(&engine, &browser),
        expected_sessions + 2,
        "expected every token-less start to be stored as its own session"
    );
}

/// A lookup that misses is a 404 `ResourceNotFoundException` whose message names the
/// resource that did not resolve, resolved outside-in: the BROWSER when the identifier
/// names none, and the SESSION when the browser is held but that session is not.
#[tokio::test]
async fn a_lookup_that_misses_is_refused_with_404_naming_the_resource_that_did_not_resolve() {
    let (router, _engine) = emulator();
    let held = create_browser(&router, CUSTOM_BROWSER_NAME).await;
    let other = create_browser(&router, OTHER_BROWSER_NAME).await;

    let (_, started) = start_session(&router, &held, json!({})).await;
    let known = text(&started, "sessionId").to_string();

    // (case, the browser read, the session read, the label and identifier the refusal
    // must name)
    let cases: Vec<(&str, String, String, &str, String)> = vec![
        (
            "unknown-session-on-the-service-provided-browser",
            SYSTEM_BROWSER.to_string(),
            "01abcdef0123456789".to_string(),
            "Session",
            "01abcdef0123456789".to_string(),
        ),
        (
            "unknown-session-on-a-browser-that-is-held",
            held.clone(),
            "0a1b2c3d4e5f6a7b".to_string(),
            "Session",
            "0a1b2c3d4e5f6a7b".to_string(),
        ),
        (
            "known-session-on-another-browser-that-is-held",
            other.clone(),
            known.clone(),
            "Session",
            known.clone(),
        ),
        (
            "session-on-a-browser-nothing-created",
            MISSING_BROWSER.to_string(),
            known.clone(),
            "Browser",
            MISSING_BROWSER.to_string(),
        ),
    ];

    for (case, browser, session_id, resource, identifier) in cases {
        let (status, body) = get_session(&router, &browser, &session_id).await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected HTTP 404, got {status} with {body:?}"
        );
        assert_eq!(
            error_code(&body),
            "ResourceNotFoundException",
            "case {case:?}: expected ResourceNotFoundException, got {body:?}"
        );
        let expected = format!("{resource} not found: {identifier}");
        assert_eq!(
            message(&body),
            expected,
            "case {case:?}: expected message {expected:?}, got {:?}",
            message(&body)
        );
    }

    // The session that was started is still readable after every refusal.
    let (status, read) = get_session(&router, &held, &known).await;
    assert_eq!(
        (status, text(&read, "sessionId")),
        (200, known.as_str()),
        "after every refusal the started session must still be readable, got {status} with {read:?}"
    );
}

/// A browser is the CONTROL plane's resource: a session is started only on a browser that
/// component holds, and an identifier naming none is refused with a 404 that stores
/// nothing — whatever the identifier looks like.
#[tokio::test]
async fn a_session_is_started_only_on_a_browser_the_control_plane_holds() {
    let (router, engine) = emulator();

    // Rows span the forms an identifier can take: one shaped like a `browserId` the
    // control plane mints, one shaped like the service-provided tool's reserved id but
    // not it, and one of no particular shape.
    for (case, browser) in [
        ("browser-id-shaped", MISSING_BROWSER),
        ("system-tool-shaped", "aws.browser.v2"),
        ("arbitrary-identifier", "not a browser at all"),
    ] {
        let (status, body) = start_session(&router, browser, json!({})).await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected HTTP 404 from a start on a browser nothing holds, got {status} with {body:?}"
        );
        assert_eq!(
            error_code(&body),
            "ResourceNotFoundException",
            "case {case:?}: expected ResourceNotFoundException, got {body:?}"
        );
        let expected = format!("Browser not found: {browser}");
        assert_eq!(
            message(&body),
            expected,
            "case {case:?}: expected message {expected:?}, got {:?}",
            message(&body)
        );
        assert_eq!(
            session_count(&engine, browser),
            0,
            "case {case:?}: a refused start must store nothing"
        );
    }

    // The same start on a browser the control plane really created succeeds, so the
    // refusals above are about the browser and not about the operation.
    let created = create_browser(&router, CUSTOM_BROWSER_NAME).await;
    let (status, started) = start_session(&router, &created, json!({})).await;
    assert_eq!(
        status, 200,
        "expected a start on a created browser to succeed, got {status} with {started:?}"
    );
    assert_eq!(
        session_count(&engine, &created),
        1,
        "expected the created browser to hold the one session that was started on it"
    );
}

/// Input outside a modelled constraint is refused with a 400 `ValidationException`, and
/// a violated numeric range is stated the way this service states one — the member and
/// the bound, with no echo of the value that missed it: one case on each side of every
/// range and length `StartBrowserSessionRequest` publishes for the members under test.
///
/// The clause is pinned only for the numeric-range rows: that is the one class this
/// service has its own wording for (`src/service/validation.rs`), and a refusal of
/// another class keeps the framework's sentence, which `validation_range_wording.rs`
/// owns.
#[tokio::test]
async fn input_outside_a_modelled_constraint_is_refused_with_400() {
    let (router, engine) = emulator();

    // (case, the request, the clause a numeric-range refusal must carry)
    let cases: Vec<(&str, Value, Option<&str>)> = vec![
        (
            "timeout-below-the-range",
            json!({"sessionTimeoutSeconds": 0}),
            Some(
                "Value at 'sessionTimeoutSeconds' failed to satisfy constraint: Member must have \
                 value greater than or equal to 1",
            ),
        ),
        (
            "timeout-above-the-range",
            json!({"sessionTimeoutSeconds": 28_801}),
            Some(
                "Value at 'sessionTimeoutSeconds' failed to satisfy constraint: Member must have \
                 value less than or equal to 28800",
            ),
        ),
        (
            "viewport-width-below-the-range",
            json!({"viewPort": {"width": 319, "height": 720}}),
            Some(
                "Value at 'viewPort.width' failed to satisfy constraint: Member must have value \
                 greater than or equal to 320",
            ),
        ),
        (
            "viewport-width-above-the-range",
            json!({"viewPort": {"width": 3_841, "height": 720}}),
            Some(
                "Value at 'viewPort.width' failed to satisfy constraint: Member must have value \
                 less than or equal to 3840",
            ),
        ),
        (
            "viewport-height-below-the-range",
            json!({"viewPort": {"width": 1_280, "height": 239}}),
            Some(
                "Value at 'viewPort.height' failed to satisfy constraint: Member must have value \
                 greater than or equal to 240",
            ),
        ),
        (
            "viewport-height-above-the-range",
            json!({"viewPort": {"width": 1_280, "height": 2_161}}),
            Some(
                "Value at 'viewPort.height' failed to satisfy constraint: Member must have value \
                 less than or equal to 2160",
            ),
        ),
        ("certificates-below-its-length", json!({"certificates": []}), None),
        ("extensions-below-its-length", json!({"extensions": []}), None),
        ("empty-name", json!({"name": ""}), None),
        ("name-past-its-length", json!({"name": "n".repeat(101)}), None),
        (
            "client-token-below-its-length",
            json!({"clientToken": "a".repeat(32)}),
            None,
        ),
    ];

    for (case, request, clause) in cases {
        let (status, body) = start_session(&router, SYSTEM_BROWSER, request).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {body:?}"
        );
        assert_eq!(
            error_code(&body),
            "ValidationException",
            "case {case:?}: expected ValidationException, got {body:?}"
        );
        if let Some(clause) = clause {
            let expected = format!("1 validation error detected: {clause}");
            assert_eq!(
                message(&body),
                expected,
                "case {case:?}: expected message {expected:?}, got {:?}",
                message(&body)
            );
        }
        assert_eq!(
            session_count(&engine, SYSTEM_BROWSER),
            0,
            "case {case:?}: a refused start must store nothing"
        );
    }
}

// --- InvokeBrowser: the OS-level actions performed on one session -------------------

/// Where `InvokeBrowser` carries the session: `sessionId` is
/// `@httpHeader("x-amzn-browser-session-id")` on the request AND on the answer (source:
/// smithy_query query="operation" operation="InvokeBrowser").
const BROWSER_SESSION_HEADER: &str = "x-amzn-browser-session-id";

/// The eight bytes every PNG file begins with (RFC 2083 §3.1).
const PNG_MAGIC: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];

/// The sample value an unpainted pixel of a session's screen carries on every channel: a
/// browser that has loaded no page shows a blank white viewport.
const BLANK_SAMPLE: u8 = 0xff;

/// `InvokeBrowser`: `POST /browsers/{browserIdentifier}/sessions/invoke`, with the session
/// on its header and exactly one `BrowserAction` member in the body. Answers with the
/// status, the parsed body, and the session the response's own header named.
async fn invoke(
    router: &MultiServiceRouter,
    browser: &str,
    session_id: &str,
    action: Value,
) -> (u16, Value, Option<String>) {
    let (status, body, headers) = dispatch(
        router,
        "POST",
        &format!("/browsers/{}/sessions/invoke", label(browser)),
        Some(json!({"action": action})),
        &[(BROWSER_SESSION_HEADER, session_id)],
    )
    .await;
    let answered = headers
        .get(BROWSER_SESSION_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    (status, body, answered)
}

/// `StopBrowserSession`: `PUT /browsers/{browserIdentifier}/sessions/stop?sessionId=…`.
async fn stop_session(
    router: &MultiServiceRouter,
    browser: &str,
    session_id: &str,
) -> (u16, Value) {
    send(
        router,
        "PUT",
        &format!(
            "/browsers/{}/sessions/stop?sessionId={}",
            label(browser),
            label(session_id)
        ),
        None,
    )
    .await
}

/// Start one session on the system browser and return its `sessionId`.
async fn started_session(router: &MultiServiceRouter, request: Value) -> String {
    let (status, started) = start_session(router, SYSTEM_BROWSER, request).await;
    assert_eq!(
        status, 200,
        "the fixture session must start, got {status} with {started:?}"
    );
    text(&started, "sessionId").to_string()
}

/// The `BrowserActionResult` member a 200 carries, by the name of the action performed.
fn result_member<'a>(body: &'a Value, action: &str) -> &'a Value {
    member(member(body, "result"), action)
}

/// How many OS-level actions the stored session has performed: what says a FAILED action
/// delivered nothing, read off the store the handlers share.
fn actions_performed(engine: &engine::InMemoryEngine, browser: &str, session_id: &str) -> u64 {
    let state = agentcore::state::service_state(engine).expect("the service state");
    let store = state.lock();
    store
        .browser_session(browser, session_id)
        .map_or(0, |session| session.input.actions_performed)
}

/// The image a successful screenshot answered with, base64-decoded the way an SDK decodes
/// a `Blob` on this wire.
fn screenshot_image(case: &str, body: &Value) -> Vec<u8> {
    let encoded = text(result_member(body, "screenshot"), "data");
    assert!(
        !encoded.is_empty(),
        "case {case:?}: expected base64 image data, got {body:?}"
    );
    aws_smithy_types::base64::decode(encoded)
        .unwrap_or_else(|error| panic!("case {case:?}: the data must be base64, got {error}"))
}

/// An image's dimensions and its RGB pixels, decoded with the maintained PNG decoder — a
/// real image, not bytes that merely start like one.
fn decoded(case: &str, image: &[u8]) -> (u32, u32, Vec<u8>) {
    assert_eq!(
        image.get(..PNG_MAGIC.len()),
        Some(&PNG_MAGIC[..]),
        "case {case:?}: expected the PNG signature, got {:?}",
        image.get(..PNG_MAGIC.len())
    );
    let mut reader = png::Decoder::new(std::io::Cursor::new(image))
        .read_info()
        .unwrap_or_else(|error| {
            panic!("case {case:?}: the image must decode as a PNG, got {error}")
        });
    let buffer_size = reader
        .output_buffer_size()
        .unwrap_or_else(|| panic!("case {case:?}: the image must state a buffer size"));
    let mut pixels = vec![0u8; buffer_size];
    let info = reader
        .next_frame(&mut pixels)
        .unwrap_or_else(|error| panic!("case {case:?}: the image must carry a frame, got {error}"));
    pixels.truncate(info.buffer_size());
    (info.width, info.height, pixels)
}

/// How many samples of `pixels` are not the blank surface's.
fn painted_samples(pixels: &[u8]) -> usize {
    pixels.iter().filter(|sample| **sample != BLANK_SAMPLE).count()
}

/// A screenshot is an image of the session's OWN screen: a real PNG, exactly as wide and
/// as tall as the viewport that session was started with — and, for a start that named no
/// viewport, as the surface this emulator renders such a session at.
#[tokio::test]
async fn a_screenshot_is_a_png_of_the_sessions_own_viewport() {
    let (router, _engine) = emulator();

    // Rows differ along the viewport the session was started with: both published bounds
    // of `ViewPortWidth` (320..3840) and `ViewPortHeight` (240..2160), one pair between
    // them, and a start that named no viewport at all.
    let cases: Vec<(&str, Value, u32, u32)> = vec![
        (
            "smallest-published-viewport",
            json!({"viewPort": {"width": 320, "height": 240}}),
            320,
            240,
        ),
        (
            "a-viewport-between-the-published-bounds",
            json!({"viewPort": {"width": 800, "height": 600}}),
            800,
            600,
        ),
        (
            "largest-published-viewport",
            json!({"viewPort": {"width": 3_840, "height": 2_160}}),
            3_840,
            2_160,
        ),
        (
            "no-viewport-renders-the-surface-this-emulator-assigns",
            json!({}),
            agentcore::state::DEFAULT_SURFACE_WIDTH,
            agentcore::state::DEFAULT_SURFACE_HEIGHT,
        ),
    ];

    for (case, request, width, height) in cases {
        let session_id = started_session(&router, request).await;
        // `ScreenshotFormat` publishes one value, `PNG`, which this row names explicitly.
        let (status, body, answered) = invoke(
            &router,
            SYSTEM_BROWSER,
            &session_id,
            json!({"screenshot": {"format": "PNG"}}),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} with {body:?}"
        );
        assert_eq!(
            answered.as_deref(),
            Some(session_id.as_str()),
            "case {case:?}: expected the answer to name the session it acted on, got {answered:?}"
        );
        let screenshot = result_member(&body, "screenshot");
        assert_eq!(
            text(screenshot, "status"),
            "SUCCESS",
            "case {case:?}: expected a SUCCESS screenshot, got {screenshot:?}"
        );
        assert_eq!(
            member(screenshot, "error"),
            &Value::Null,
            "case {case:?}: a performed action carries no error, got {screenshot:?}"
        );
        let image = screenshot_image(case, &body);
        let (got_width, got_height, pixels) = decoded(case, &image);
        assert_eq!(
            (got_width, got_height),
            (width, height),
            "case {case:?}: expected a {width}x{height} image, got {got_width}x{got_height}"
        );
        // Nothing has been navigated and no pointer action has been performed, so the
        // screen is the blank viewport and nothing is painted onto it.
        assert_eq!(
            painted_samples(&pixels),
            0,
            "case {case:?}: expected a blank screen, got {} painted samples",
            painted_samples(&pixels)
        );
    }
}

/// The session's viewport is the screen EVERY action happens on: a pointer action at a
/// point on it is performed, one off it is reported FAILED with the point and the screen
/// it missed, and a keyboard action naming no key is FAILED for the same reason. A FAILED
/// action delivers nothing, so it leaves the session's input state where it was.
#[tokio::test]
async fn an_action_is_performed_only_where_the_session_can_receive_it() {
    let (router, engine) = emulator();
    let session_id =
        started_session(&router, json!({"viewPort": {"width": 800, "height": 600}})).await;

    // Rows differ along the action, the point it names relative to the 800x600 screen
    // (its corners, the pixel past each edge, a negative coordinate), and the keys a
    // keyboard action sends.
    let cases: Vec<(&str, &str, Value, bool)> = vec![
        ("move-to-the-screens-origin", "mouseMove", json!({"mouseMove": {"x": 0, "y": 0}}), true),
        (
            "click-the-last-pixel-of-the-screen",
            "mouseClick",
            json!({"mouseClick": {"x": 799, "y": 599, "button": "LEFT", "clickCount": 2}}),
            true,
        ),
        (
            "click-one-pixel-past-the-right-edge",
            "mouseClick",
            json!({"mouseClick": {"x": 800, "y": 599}}),
            false,
        ),
        (
            "click-one-pixel-past-the-bottom-edge",
            "mouseClick",
            json!({"mouseClick": {"x": 799, "y": 600}}),
            false,
        ),
        (
            "move-to-a-negative-coordinate",
            "mouseMove",
            json!({"mouseMove": {"x": -1, "y": 10}}),
            false,
        ),
        (
            "scroll-at-a-point-on-the-screen",
            "mouseScroll",
            json!({"mouseScroll": {"x": 400, "y": 300, "deltaX": -1_000, "deltaY": 1_000}}),
            true,
        ),
        (
            "scroll-at-a-point-off-the-screen",
            "mouseScroll",
            json!({"mouseScroll": {"x": 400, "y": 1_200, "deltaY": 10}}),
            false,
        ),
        (
            "drag-between-two-points-on-the-screen",
            "mouseDrag",
            json!({"mouseDrag": {"startX": 10, "startY": 20, "endX": 700, "endY": 500, "button": "MIDDLE"}}),
            true,
        ),
        (
            "drag-that-ends-off-the-screen",
            "mouseDrag",
            json!({"mouseDrag": {"startX": 10, "startY": 20, "endX": 900, "endY": 500}}),
            false,
        ),
        (
            "type-text-through-the-keyboard",
            "keyType",
            json!({"keyType": {"text": "hello agentcore"}}),
            true,
        ),
        (
            "type-text-of-no-characters",
            "keyType",
            json!({"keyType": {"text": ""}}),
            true,
        ),
        (
            "press-a-key-repeatedly",
            "keyPress",
            json!({"keyPress": {"key": "Enter", "presses": 100}}),
            true,
        ),
        (
            "press-a-key-that-names-none",
            "keyPress",
            json!({"keyPress": {"key": " "}}),
            false,
        ),
        (
            "hold-a-shortcut-of-keys",
            "keyShortcut",
            json!({"keyShortcut": {"keys": ["Control", "Shift", "t"]}}),
            true,
        ),
        (
            "hold-a-shortcut-with-a-key-that-names-none",
            "keyShortcut",
            json!({"keyShortcut": {"keys": ["Control", ""]}}),
            false,
        ),
    ];

    let mut performed = actions_performed(&engine, SYSTEM_BROWSER, &session_id);
    for (case, action, request, expect_performed) in cases {
        let (status, body, answered) =
            invoke(&router, SYSTEM_BROWSER, &session_id, request).await;
        assert_eq!(
            status, 200,
            "case {case:?}: an action the session answers for is a 200, got {status} with {body:?}"
        );
        assert_eq!(
            answered.as_deref(),
            Some(session_id.as_str()),
            "case {case:?}: expected the answer to name the session, got {answered:?}"
        );
        let result = result_member(&body, action);
        assert!(
            !result.is_null(),
            "case {case:?}: expected the result to carry the {action:?} member, got {body:?}"
        );
        let expected_status = if expect_performed { "SUCCESS" } else { "FAILED" };
        assert_eq!(
            text(result, "status"),
            expected_status,
            "case {case:?}: expected status {expected_status:?}, got {result:?}"
        );
        if expect_performed {
            performed += 1;
            assert_eq!(
                member(result, "error"),
                &Value::Null,
                "case {case:?}: a performed action carries no error, got {result:?}"
            );
        } else {
            let error = text(result, "error");
            assert!(
                !error.is_empty(),
                "case {case:?}: a failed action says why, got {result:?}"
            );
        }
        assert_eq!(
            actions_performed(&engine, SYSTEM_BROWSER, &session_id),
            performed,
            "case {case:?}: expected {performed} performed action(s), got {}",
            actions_performed(&engine, SYSTEM_BROWSER, &session_id)
        );
    }
}

/// A pointer action that misses the screen says which point it was asked for and how big
/// that screen is, for sessions whose screens differ.
#[tokio::test]
async fn a_pointer_action_off_the_screen_names_the_point_and_the_screen() {
    let (router, _engine) = emulator();

    // (case, the viewport the session runs at, the point asked for)
    let cases: Vec<(&str, Value, i32, i32, u32, u32)> = vec![
        (
            "past-the-width-of-a-small-screen",
            json!({"viewPort": {"width": 320, "height": 240}}),
            320,
            0,
            320,
            240,
        ),
        (
            "past-the-height-of-a-wide-screen",
            json!({"viewPort": {"width": 1_920, "height": 1_080}}),
            100,
            1_080,
            1_920,
            1_080,
        ),
        (
            "past-the-surface-this-emulator-assigns-a-session-with-no-viewport",
            json!({}),
            i32::try_from(agentcore::state::DEFAULT_SURFACE_WIDTH).unwrap_or(i32::MAX),
            0,
            agentcore::state::DEFAULT_SURFACE_WIDTH,
            agentcore::state::DEFAULT_SURFACE_HEIGHT,
        ),
    ];

    for (case, request, x, y, width, height) in cases {
        let session_id = started_session(&router, request).await;
        let (status, body, _answered) = invoke(
            &router,
            SYSTEM_BROWSER,
            &session_id,
            json!({"mouseClick": {"x": x, "y": y}}),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} with {body:?}"
        );
        let error = text(result_member(&body, "mouseClick"), "error").to_string();
        assert!(
            error.contains(&format!("({x}, {y})")) && error.contains(&format!("{width}x{height}")),
            "case {case:?}: expected the point ({x}, {y}) and the screen {width}x{height} in the \
             error, got {error:?}"
        );
    }
}

/// An action can only be performed on a session that is still running: one a stop
/// terminated, and one whose time-to-live has run out, are both refused with the service's
/// own 400, and neither performs anything.
#[tokio::test]
async fn an_action_on_a_terminated_session_is_refused() {
    let (router, engine) = emulator();

    // (case, how this session stopped being one an action can run in)
    let cases: Vec<(&str, bool)> = vec![
        ("stopped-by-stop-browser-session", true),
        ("past-its-own-time-to-live", false),
    ];

    for (case, by_stop) in cases {
        let session_id =
            started_session(&router, json!({"sessionTimeoutSeconds": 60})).await;
        if by_stop {
            let (status, stopped) = stop_session(&router, SYSTEM_BROWSER, &session_id).await;
            assert_eq!(
                status, 200,
                "case {case:?}: the fixture stop must succeed, got {status} with {stopped:?}"
            );
        } else {
            age_browser_session(
                &engine,
                SYSTEM_BROWSER,
                &session_id,
                Duration::from_secs(61),
            );
        }

        let (status, body, _answered) = invoke(
            &router,
            SYSTEM_BROWSER,
            &session_id,
            json!({"screenshot": {}}),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {body:?}"
        );
        assert_eq!(
            error_code(&body),
            "ValidationException",
            "case {case:?}: expected ValidationException, got {body:?}"
        );
        let expected = format!(
            "Browser session {session_id} is not active. Start a new session or provide a valid \
             session ID"
        );
        assert_eq!(
            message(&body),
            expected,
            "case {case:?}: expected message {expected:?}, got {:?}",
            message(&body)
        );
        assert_eq!(
            actions_performed(&engine, SYSTEM_BROWSER, &session_id),
            0,
            "case {case:?}: a refused action performs nothing, got {}",
            actions_performed(&engine, SYSTEM_BROWSER, &session_id)
        );
    }
}

/// An action is addressed outside-in, so a browser nothing holds and a session nothing
/// started are each reported as the resource that did not resolve.
#[tokio::test]
async fn an_action_naming_no_browser_or_no_session_is_refused_with_404() {
    let (router, _engine) = emulator();
    let live = started_session(&router, json!({})).await;

    // (case, the browser, the session, the message the refusal must carry)
    let cases: Vec<(&str, String, String, String)> = vec![
        (
            "a-browser-nothing-holds",
            MISSING_BROWSER.to_string(),
            live.clone(),
            format!("Browser not found: {MISSING_BROWSER}"),
        ),
        (
            "a-session-nothing-started",
            SYSTEM_BROWSER.to_string(),
            "abc123".to_string(),
            "Session not found: abc123".to_string(),
        ),
    ];

    for (case, browser, session_id, expected) in cases {
        let (status, body, _answered) = invoke(
            &router,
            &browser,
            &session_id,
            json!({"mouseMove": {"x": 1, "y": 1}}),
        )
        .await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected HTTP 404, got {status} with {body:?}"
        );
        assert_eq!(
            error_code(&body),
            "ResourceNotFoundException",
            "case {case:?}: expected ResourceNotFoundException, got {body:?}"
        );
        assert_eq!(
            message(&body),
            expected,
            "case {case:?}: expected message {expected:?}, got {:?}",
            message(&body)
        );
    }
}

/// A screenshot shows the session's own screen: where its last pointer action left the
/// pointer, and nothing where no action has been. Two screenshots of an unchanged session
/// are the same image, and the screenshot itself changes nothing a later caller sees.
#[tokio::test]
async fn a_screenshot_shows_where_the_pointer_actions_left_the_pointer() {
    let (router, engine) = emulator();
    let session_id =
        started_session(&router, json!({"viewPort": {"width": 400, "height": 300}})).await;

    let (_status, blank, _answered) =
        invoke(&router, SYSTEM_BROWSER, &session_id, json!({"screenshot": {}})).await;
    let first = screenshot_image("a-session-with-no-pointer-action", &blank);
    let (_width, _height, blank_pixels) = decoded("a-session-with-no-pointer-action", &first);
    assert_eq!(
        painted_samples(&blank_pixels),
        0,
        "case \"a-session-with-no-pointer-action\": expected nothing painted, got {} samples",
        painted_samples(&blank_pixels)
    );

    // A screenshot REPORTS the session, so taking one twice answers with the identical
    // image and performs nothing.
    let (_status, again, _answered) =
        invoke(&router, SYSTEM_BROWSER, &session_id, json!({"screenshot": {}})).await;
    let second = screenshot_image("the-same-session-screenshotted-twice", &again);
    assert_eq!(
        second, first,
        "case \"the-same-session-screenshotted-twice\": expected the same image, got {} bytes \
         against {} bytes",
        second.len(),
        first.len()
    );
    assert_eq!(
        actions_performed(&engine, SYSTEM_BROWSER, &session_id),
        0,
        "case \"the-same-session-screenshotted-twice\": a screenshot performs no action, got {}",
        actions_performed(&engine, SYSTEM_BROWSER, &session_id)
    );

    // Rows differ along where the pointer is moved to, so what the image shows follows the
    // session's own state rather than a fixed picture.
    for (case, x, y) in [
        ("pointer-moved-into-the-middle-of-the-screen", 100_usize, 120_usize),
        ("pointer-moved-near-the-top-left", 3, 4),
    ] {
        let (status, moved, _answered) = invoke(
            &router,
            SYSTEM_BROWSER,
            &session_id,
            json!({"mouseMove": {"x": x, "y": y}}),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} with {moved:?}"
        );
        let (_status, shot, _answered) =
            invoke(&router, SYSTEM_BROWSER, &session_id, json!({"screenshot": {}})).await;
        let image = screenshot_image(case, &shot);
        let (width, _height, pixels) = decoded(case, &image);
        let samples_per_pixel = 3_usize;
        let tip = (y * usize::try_from(width).unwrap_or(0) + x) * samples_per_pixel;
        assert_eq!(
            pixels.get(tip..tip + samples_per_pixel).map(painted_samples),
            Some(samples_per_pixel),
            "case {case:?}: expected the pointer at ({x}, {y}), got {:?}",
            pixels.get(tip..tip + samples_per_pixel)
        );
        assert_eq!(
            pixels.get(..samples_per_pixel).map(painted_samples),
            Some(0),
            "case {case:?}: expected the screen's origin blank, got {:?}",
            pixels.get(..samples_per_pixel)
        );
    }
}

/// `lastUpdatedAt` is the instant the session was last CHANGED: an action that was
/// delivered moves it, and a screenshot — which only reports the screen — leaves it where
/// it was.
#[tokio::test]
async fn a_delivered_action_moves_last_updated_at_and_a_screenshot_does_not() {
    let (router, engine) = emulator();
    let session_id = started_session(&router, json!({"viewPort": {"width": 640, "height": 480}}))
        .await;
    // Move the whole session back in time, so the instant an action writes is a different
    // one however fast this host runs. The session's time-to-live is far longer, so it is
    // still a session an action can be performed on.
    age_browser_session(
        &engine,
        SYSTEM_BROWSER,
        &session_id,
        Duration::from_secs(300),
    );
    rewind_last_updated(&engine, SYSTEM_BROWSER, &session_id, Duration::from_secs(300));

    let (_status, before) = get_session(&router, SYSTEM_BROWSER, &session_id).await;
    let started_at = text(&before, "lastUpdatedAt").to_string();

    // (case, the action, whether it changes the session)
    let cases: Vec<(&str, Value, bool)> = vec![
        ("a-screenshot-reports-and-changes-nothing", json!({"screenshot": {}}), false),
        (
            "a-click-is-delivered-and-changes-the-session",
            json!({"mouseClick": {"x": 10, "y": 10}}),
            true,
        ),
    ];

    for (case, action, changes) in cases {
        let (status, body, _answered) =
            invoke(&router, SYSTEM_BROWSER, &session_id, action).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} with {body:?}"
        );
        let (_status, read) = get_session(&router, SYSTEM_BROWSER, &session_id).await;
        let now_reported = text(&read, "lastUpdatedAt").to_string();
        if changes {
            let moved = DateTime::from_str(&now_reported, Format::DateTime)
                .ok()
                .and_then(|stamp| stamp.to_millis().ok());
            let was = DateTime::from_str(&started_at, Format::DateTime)
                .ok()
                .and_then(|stamp| stamp.to_millis().ok());
            assert!(
                moved > was,
                "case {case:?}: expected lastUpdatedAt past {started_at:?}, got {now_reported:?}"
            );
        } else {
            assert_eq!(
                now_reported, started_at,
                "case {case:?}: expected lastUpdatedAt to stay {started_at:?}, got \
                 {now_reported:?}"
            );
        }
    }
}

/// Move a stored session's last-changed instant `by` further into the past, so a case can
/// tell the instant an action writes from the one that was there before it without
/// depending on how fast this host runs. The test reaching into the shared store, not an
/// emulator behaviour.
fn rewind_last_updated(
    engine: &engine::InMemoryEngine,
    browser: &str,
    session_id: &str,
    by: Duration,
) {
    let state = agentcore::state::service_state(engine).expect("the service state");
    let mut store = state.lock();
    let session = store
        .browser_session_mut(browser, session_id)
        .expect("the session that was just started");
    session.last_updated_at -= by;
}
