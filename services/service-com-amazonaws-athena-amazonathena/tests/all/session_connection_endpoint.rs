// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for `GetSessionEndpoint`, driven over the same router the emulator boots
//! (awsJson1_1, `x-amz-target: AmazonAthena.<Operation>`): it "Gets a connection endpoint and
//! authentication token for a given session Id".
//!
//! THE RULE UNDER TEST: for a session this account holds that is still running, the call
//! answers a grant computed from that stored session — an address of THIS emulator naming the
//! session, a fresh authentication token only this store could have minted for it, and the
//! instant that token expires — and asking for it changes nothing a later caller sees. A
//! session id this account holds no session under is refused with the one sentence every
//! session-addressed call of this service states for that miss, and a session whose resources
//! are no longer running has no connection to hand out and is refused for that.
//!
//! The rows differ along what the rule generalises over: how many times the grant is asked
//! for, which notebook the session was started in (one, or none at all), which session of
//! several the request names, the shape of an id that names no session, and which
//! non-running state the session is in.
//!
//! Nothing here waits for a fixed span: a test that needs a transition the clock owes POLLS
//! for the state it is waiting on, under a deadline far above the span that transition is due
//! after.

use std::time::{Duration, SystemTime};

use serde_json::{json, Value};
use service_com_amazonaws_athena_amazonathena::state::{
    self, ERROR_CODE_INVALID_INPUT, ERROR_CODE_UNAUTHORIZED, NOTEBOOK_ID_CONFIG,
    PYSPARK_ENGINE_VERSION, SESSION_ENDPOINT_UNAVAILABLE, SESSION_NOT_FOUND,
    SESSION_STATE_TERMINATED,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{assert_refusal, call, engine_and_router};

/// The Spark enabled workgroup every session below runs in.
const WORK_GROUP: &str = "spark-session-endpoint";

/// How long a poll for a transition the clock owes keeps looking, and how long it waits
/// between looks. The deadline is far above the span a session's shutdown is due after, so the
/// CONDITION is what is waited on rather than a span of the host's own speed.
const POLL_INTERVAL: Duration = Duration::from_millis(50);
/// See [`POLL_INTERVAL`].
const POLL_LOOKS: u32 = 1_200;

/// The longest the token's expiry may be reported beyond the call: the ten minutes this
/// service publishes for the other grant it hands out over a session, which this one shares.
/// Written here rather than read off the implementation, so a different span fails.
const EXPECTED_TOKEN_LIFETIME_SECONDS: f64 = 10.0 * 60.0;

/// How far either way the reported expiry may land from that span: the resolution the wire
/// carries a timestamp at is coarser than the clock the instant was read off, and the two
/// readings around the call are of the host's own clock. Generous, because the span above is
/// what this assertion is about rather than the host's speed.
const EXPIRY_TOLERANCE_SECONDS: f64 = 1.0;

/// Create the Spark enabled workgroup every session below is started in.
async fn given_spark_work_group(router: &MultiServiceRouter) {
    let (status, created) = call(
        router,
        "CreateWorkGroup",
        &json!({
            "Name": WORK_GROUP,
            "Configuration": {
                "EngineVersion": { "SelectedEngineVersion": PYSPARK_ENGINE_VERSION },
                "ExecutionRole": "arn:aws:iam::000000000000:role/AthenaSparkExecution",
                "ResultConfiguration": { "OutputLocation": "s3://athena-spark/sessions/" }
            }
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the Spark workgroup must be created, got {status} with {created:?}"
    );
}

/// Create the notebook `name` in [`WORK_GROUP`] and hand back its `NotebookId`.
async fn given_notebook(router: &MultiServiceRouter, name: &str) -> String {
    let (status, created) = call(
        router,
        "CreateNotebook",
        &json!({ "WorkGroup": WORK_GROUP, "Name": name }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the notebook {name} must be created, got {status} with {created:?}"
    );
    created["NotebookId"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateNotebook must answer with an id, got {created:?}"))
        .to_string()
}

/// Start one session in [`WORK_GROUP`], bound to `notebook_id` when one is given, and hand
/// back its `SessionId`.
async fn given_session(router: &MultiServiceRouter, notebook_id: Option<&str>) -> String {
    let engine_configuration = match notebook_id {
        Some(notebook_id) => json!({
            "CoordinatorDpuSize": 1,
            "MaxConcurrentDpus": 4,
            "DefaultExecutorDpuSize": 1,
            "AdditionalConfigs": { NOTEBOOK_ID_CONFIG: notebook_id }
        }),
        None => json!({
            "CoordinatorDpuSize": 1,
            "MaxConcurrentDpus": 4,
            "DefaultExecutorDpuSize": 1
        }),
    };
    let (status, started) = call(
        router,
        "StartSession",
        &json!({ "WorkGroup": WORK_GROUP, "EngineConfiguration": engine_configuration }),
    )
    .await;
    assert_eq!(
        status, 200,
        "StartSession must be accepted, got {status} with {started:?}"
    );
    started["SessionId"]
        .as_str()
        .unwrap_or_else(|| panic!("StartSession must answer with a SessionId, got {started:?}"))
        .to_string()
}

/// One `GetSessionEndpoint` call for `session_id`.
async fn endpoint_of(router: &MultiServiceRouter, session_id: &str) -> (u16, Value) {
    call(
        router,
        "GetSessionEndpoint",
        &json!({ "SessionId": session_id }),
    )
    .await
}

/// Poll until the session `session_id` reports `wanted`.
async fn settled_in(router: &MultiServiceRouter, session_id: &str, wanted: &str) {
    for _ in 0..POLL_LOOKS {
        let (status, body) = call(
            router,
            "GetSessionStatus",
            &json!({ "SessionId": session_id }),
        )
        .await;
        assert_eq!(
            status, 200,
            "GetSessionStatus({session_id}) must answer 200 while polling, got {status} with \
             {body:?}"
        );
        if body["Status"]["State"].as_str() == Some(wanted) {
            return;
        }
        ::tokio::time::sleep(POLL_INTERVAL).await;
    }
    panic!("the session {session_id} never reached {wanted}");
}

/// Epoch seconds now, for bounding the instant the answered token expires without waiting for
/// any span to pass.
fn epoch_seconds_now() -> f64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .expect("the host clock is at or after the epoch")
        .as_secs_f64()
}

/// The grant a running session answers with: an address of this emulator naming that session,
/// a token this store minted for it, and the instant that token expires — a new token per
/// call, and nothing about the session changed by the asking.
#[tokio::test]
async fn a_running_session_answers_an_endpoint_and_a_token_of_its_own() {
    let (engine, router) = engine_and_router();
    given_spark_work_group(&router).await;
    let notebook_id = given_notebook(&router, "endpoint.ipynb").await;

    // The rows differ along which notebook the session was started in: the grant is about the
    // SESSION, so a session bound to no notebook has one too.
    let cases: Vec<(&str, Option<String>)> = vec![
        ("a session started in a notebook", Some(notebook_id)),
        ("a session bound to no notebook", None),
    ];

    let mut endpoints: Vec<(String, String)> = Vec::new();
    for (case, notebook) in &cases {
        let session_id = given_session(&router, notebook.as_deref()).await;

        // Three issues of the grant for the same session: each is served, each carries a
        // token of its own, and each reports the same address.
        let mut tokens: Vec<String> = Vec::new();
        for issue in 1..=3 {
            let before = epoch_seconds_now();
            let (status, granted) = endpoint_of(&router, &session_id).await;
            assert_eq!(
                status, 200,
                "case {case:?}: issue {issue} must be served, got {status} with {granted:?}"
            );
            let after = epoch_seconds_now();

            let url = granted["EndpointUrl"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            assert!(
                url.starts_with(smithy_server_runtime::UNSERVED_URL_BASE),
                "case {case:?}: issue {issue}: the endpoint must lead back to this emulator, \
                 got {url:?}"
            );
            assert!(
                url.ends_with(&format!("/{session_id}")),
                "case {case:?}: issue {issue}: the endpoint must name the session, got {url:?}"
            );

            // The token is this store's own: it resolves to the session it was issued for.
            let token = granted["AuthToken"].as_str().unwrap_or_default().to_string();
            assert!(
                !token.is_empty(),
                "case {case:?}: issue {issue} must carry a token, got {granted:?}"
            );
            let store = state::service_state(&*engine).expect("the Athena store is reachable");
            assert_eq!(
                store
                    .lock()
                    .session_endpoint_auth_token_session(&token)
                    .as_deref(),
                Some(session_id.as_str()),
                "case {case:?}: issue {issue}: the token must name the session it was issued \
                 for"
            );

            // The expiry is the INSTANT the token stops working, the published lifetime after
            // the call that minted it — bounded by the two readings around that call, so
            // nothing here depends on how fast the host runs.
            let expires = granted["AuthTokenExpirationTime"]
                .as_f64()
                .unwrap_or_default();
            let earliest = before + EXPECTED_TOKEN_LIFETIME_SECONDS - EXPIRY_TOLERANCE_SECONDS;
            let latest = after + EXPECTED_TOKEN_LIFETIME_SECONDS + EXPIRY_TOLERANCE_SECONDS;
            assert!(
                expires >= earliest && expires <= latest,
                "case {case:?}: issue {issue}: expected the token to expire \
                 {EXPECTED_TOKEN_LIFETIME_SECONDS} seconds after the call (between \
                 {earliest} and {latest}), got {expires}"
            );

            tokens.push(token);
            endpoints.push((session_id.clone(), url));
        }
        assert!(
            tokens.windows(2).all(|pair| pair[0] != pair[1]),
            "case {case:?}: each issue must mint another token, got {tokens:?}"
        );

        // Asking for the endpoint changed nothing a later caller sees: the session still
        // reads, under its own id.
        let (status, read) = call(&router, "GetSession", &json!({ "SessionId": session_id })).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the session must still read, got {status} with {read:?}"
        );
        assert_eq!(
            read["SessionId"].as_str(),
            Some(session_id.as_str()),
            "case {case:?}: the read must be of that same session, got {read:?}"
        );
    }

    // One session has ONE endpoint, and two sessions never share one: the address is the
    // session's, not the request's.
    for (session_id, url) in &endpoints {
        for (other_session, other_url) in &endpoints {
            if session_id == other_session {
                assert_eq!(
                    url, other_url,
                    "every call about the session {session_id} must report one endpoint"
                );
            } else {
                assert_ne!(
                    url, other_url,
                    "the sessions {session_id} and {other_session} must not share an endpoint"
                );
            }
        }
    }
}

/// The grant is refused for a session this account holds none of, and for a session whose
/// resources are no longer running — each with the sentence Athena words that class in.
#[tokio::test]
async fn the_endpoint_is_refused_for_an_unknown_and_for_a_closed_session() {
    let (_engine, router) = engine_and_router();
    given_spark_work_group(&router).await;
    let session_id = given_session(&router, None).await;

    // Rows differ along the shape of an id that names no session: a UUID nothing minted, a
    // single character, the longest `SessionId` the model admits, and one character away from
    // a session this account does hold.
    let flipped = format!(
        "{}{}",
        if session_id.starts_with('a') { "b" } else { "a" },
        &session_id[1..]
    );
    let unknown: Vec<(&str, String)> = vec![
        (
            "a UUID nothing minted",
            "00000000-0000-4000-8000-000000000000".to_string(),
        ),
        ("a single character", "s".to_string()),
        ("the longest id the model admits", "s".repeat(256)),
        ("one character from a held id", flipped),
    ];
    for (case, id) in unknown {
        let (status, refused) = endpoint_of(&router, &id).await;
        assert_refusal(
            case,
            status,
            &refused,
            ERROR_CODE_UNAUTHORIZED,
            SESSION_NOT_FOUND,
        );
    }

    // The session that IS held is served while it runs.
    let (status, granted) = endpoint_of(&router, &session_id).await;
    assert_eq!(
        status, 200,
        "the running session must be served, got {status} with {granted:?}"
    );

    // Once it has been asked to shut down there is nothing left to connect to, from the
    // instant the shutdown is asked for through the state it settles in.
    let (status, terminated) = call(
        &router,
        "TerminateSession",
        &json!({ "SessionId": session_id }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the terminate must be accepted, got {status} with {terminated:?}"
    );
    let (status, refused) = endpoint_of(&router, &session_id).await;
    assert_refusal(
        "a session that is shutting down",
        status,
        &refused,
        ERROR_CODE_INVALID_INPUT,
        SESSION_ENDPOINT_UNAVAILABLE,
    );

    settled_in(&router, &session_id, SESSION_STATE_TERMINATED).await;
    let (status, refused) = endpoint_of(&router, &session_id).await;
    assert_refusal(
        "a session whose resources are gone",
        status,
        &refused,
        ERROR_CODE_INVALID_INPUT,
        SESSION_ENDPOINT_UNAVAILABLE,
    );

    // The two refusals are different sentences: a client branching on them can tell a session
    // it must not ask about again from one it closed itself.
    assert_ne!(
        SESSION_NOT_FOUND, SESSION_ENDPOINT_UNAVAILABLE,
        "the unknown-session and the closed-session refusals must not read alike"
    );
}
