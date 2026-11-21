// SPDX-License-Identifier: CC0-1.0

//! Athena's PRESIGNED NOTEBOOK URL, driven end to end over the same router the emulator
//! boots (awsJson1_1, `x-amz-target: AmazonAthena.<Operation>`):
//! `CreatePresignedNotebookUrl` "Gets an authentication token and the URL at which the
//! notebook can be accessed".
//!
//! THE RULE UNDER TEST: for a session this account holds that is still running, the call
//! mints a grant — an address of this emulator carrying a fresh authentication token, and the
//! lifetime that token is published with ("`CreatePresignedNotebookUrl` must be called every
//! 10 minutes to refresh the authentication token", so 600 seconds) — and every re-issue
//! mints another token with that same lifetime. A session id this account holds no session
//! under, and a session whose resources are no longer running, are each refused with
//! `InvalidRequestException` (HTTP 400) carrying Athena's own sentence for that class.
//!
//! The rule is asserted over TABLES whose rows differ along what it generalises over: which
//! notebook the session was started in (one, another, none at all), how many times the grant
//! is re-issued, the shape of an id that names no session, and which non-running state the
//! session is in.
//!
//! Nothing here waits for a fixed span: a test that needs a transition the clock owes POLLS
//! the service for the state it is waiting on, under a deadline far above the span that
//! transition is due after.

use std::time::Duration;

use serde_json::{json, Value};
use service_com_amazonaws_athena_amazonathena::state::{
    self, ERROR_CODE_INVALID_INPUT, ERROR_CODE_UNAUTHORIZED, NOTEBOOK_ID_CONFIG,
    NOTEBOOK_SESSION_NOT_OPEN, NOTEBOOK_TYPE_IPYNB, PYSPARK_ENGINE_VERSION, SESSION_NOT_FOUND,
    SESSION_STATE_TERMINATED,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{assert_refusal, call, engine_and_router, fetch};

/// The Spark enabled workgroup the sessions below run in.
const WORK_GROUP: &str = "spark-presigned-url";

/// How long a poll for a transition the clock owes keeps looking, and how long it waits
/// between looks. The deadline is far above the span a session's shutdown is due after, so
/// the CONDITION is what is waited on rather than a span of the host's own speed.
const POLL_INTERVAL: Duration = Duration::from_millis(50);
/// See [`POLL_INTERVAL`].
const POLL_LOOKS: u32 = 1_200;

/// Create the Spark enabled workgroup every session below is started in.
async fn given_spark_work_group(router: &MultiServiceRouter, name: &str) {
    let (status, created) = call(
        router,
        "CreateWorkGroup",
        &json!({
            "Name": name,
            "Configuration": {
                "EngineVersion": { "SelectedEngineVersion": PYSPARK_ENGINE_VERSION },
                "ExecutionRole": "arn:aws:iam::000000000000:role/AthenaSparkExecution",
                "ResultConfiguration": { "OutputLocation": "s3://athena-spark/notebooks/" }
            }
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the Spark workgroup {name} must be created, got {status} with {created:?}"
    );
}

/// Create the notebook `name` in `WORK_GROUP` and hand back its `NotebookId`.
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

/// Start one session in `WORK_GROUP`, bound to `notebook_id` when one is given, and hand back
/// its `SessionId`.
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
        &json!({
            "WorkGroup": WORK_GROUP,
            "EngineConfiguration": engine_configuration
        }),
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

/// Import the notebook `name` into `WORK_GROUP` carrying `payload`, and hand back its
/// `NotebookId`.
///
/// Beside [`given_notebook`] so one row of the table below owns content no constant of this
/// service carries: an address that served that content read it out of the store.
async fn given_imported_notebook(
    router: &MultiServiceRouter,
    name: &str,
    payload: &str,
) -> String {
    let (status, imported) = call(
        router,
        "ImportNotebook",
        &json!({
            "WorkGroup": WORK_GROUP,
            "Name": name,
            "Type": NOTEBOOK_TYPE_IPYNB,
            "Payload": payload,
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the notebook {name} must be imported, got {status} with {imported:?}"
    );
    imported["NotebookId"]
        .as_str()
        .unwrap_or_else(|| panic!("ImportNotebook must answer with an id, got {imported:?}"))
        .to_string()
}

/// One `CreatePresignedNotebookUrl` call for `session_id`.
async fn presign(router: &MultiServiceRouter, session_id: &str) -> (u16, Value) {
    call(
        router,
        "CreatePresignedNotebookUrl",
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

/// The number of seconds `AuthTokenExpirationTime` must report: the ten minutes the
/// operation's own documentation publishes for refreshing the token ("During programmatic
/// access, `CreatePresignedNotebookUrl` must be called every 10 minutes to refresh the
/// authentication token"), which is what the recorded response of this operation carries.
///
/// Written here rather than read off the implementation's own constant, so the assertions
/// below fail for any other number — and, because it is a LIFETIME, they also fail for an
/// answer that reported the instant the token expires instead.
const PUBLISHED_TOKEN_LIFETIME_SECONDS: i64 = 10 * 60;

/// How far from now a value would have to be to be an INSTANT rather than a lifetime: a
/// UTC epoch second of this decade is more than this many seconds, so a reported value below
/// it cannot be one.
const SMALLEST_PLAUSIBLE_EPOCH_SECOND: i64 = 1_600_000_000;

/// The ipynb content the imported row of the table below carries: a notebook with a cell in
/// it, so the content the address serves is this document and not any constant this service
/// holds.
const IMPORTED_IPYNB: &str = "{\"cells\": [{\"cell_type\": \"code\", \"source\": \"1 + 1\", \
     \"metadata\": {}, \"outputs\": []}], \"metadata\": {}, \"nbformat\": 4, \
     \"nbformat_minor\": 5}";

/// One row of the grant table: which notebook the session is started in, the file name its
/// address must carry, and the content following that address must serve.
struct GrantCase {
    case: &'static str,
    notebook_id: Option<String>,
    notebook_name: Option<&'static str>,
    /// The ipynb content the address serves, or `None` for a session with no notebook to
    /// open.
    opened: Option<&'static str>,
}

/// A grant is minted for a running session, carries its token inside the address it signs,
/// publishes the token's lifetime, and a re-issue mints another token with that same
/// lifetime.
#[tokio::test]async fn a_grant_carries_a_fresh_token_inside_the_address_it_signs() {
    let (engine, router) = engine_and_router();
    given_spark_work_group(&router, WORK_GROUP).await;
    let created_notebook = given_notebook(&router, "first.ipynb").await;
    let imported_notebook =
        given_imported_notebook(&router, "second notebook.ipynb", IMPORTED_IPYNB).await;

    // Rows differ along which notebook the session serves and what that notebook holds: a
    // created notebook (the empty document a create writes), an imported one carrying content
    // of its own, and a session bound to no notebook at all.
    let cases: Vec<GrantCase> = vec![
        GrantCase {
            case: "a session started in a created notebook",
            notebook_id: Some(created_notebook.clone()),
            notebook_name: Some("first.ipynb"),
            opened: Some(state::EMPTY_IPYNB),
        },
        GrantCase {
            case: "a session started in an imported notebook",
            notebook_id: Some(imported_notebook.clone()),
            notebook_name: Some("second notebook.ipynb"),
            opened: Some(IMPORTED_IPYNB),
        },
        GrantCase {
            case: "a session bound to no notebook",
            notebook_id: None,
            notebook_name: None,
            opened: None,
        },
    ];

    for GrantCase {
        case,
        notebook_id,
        notebook_name,
        opened,
    } in cases
    {
        let session_id = given_session(&router, notebook_id.as_deref()).await;

        // Three issues of the grant for the same session: each one is served, each carries a
        // token of its own, and each publishes the same lifetime.
        let mut tokens: Vec<String> = Vec::new();
        for issue in 1..=3 {
            let (status, granted) = presign(&router, &session_id).await;
            assert_eq!(
                status, 200,
                "case {case:?}: issue {issue} must be served, got {status} with {granted:?}"
            );
            let token = granted["AuthToken"].as_str().unwrap_or_default().to_string();
            let url = granted["NotebookUrl"].as_str().unwrap_or_default().to_string();
            assert!(
                !token.is_empty(),
                "case {case:?}: issue {issue} must carry a token, got {granted:?}"
            );
            assert_eq!(
                granted["AuthTokenExpirationTime"].as_i64(),
                Some(PUBLISHED_TOKEN_LIFETIME_SECONDS),
                "case {case:?}: issue {issue} must publish the token's lifetime in seconds, got \
                 {granted:?}"
            );
            // It is that LIFETIME and not the instant the token expires: a value an
            // implementation computed as `now + lifetime` would be an epoch second of this
            // decade, far above anything a span of minutes can reach.
            assert!(
                granted["AuthTokenExpirationTime"]
                    .as_i64()
                    .is_some_and(|reported| reported < SMALLEST_PLAUSIBLE_EPOCH_SECOND),
                "case {case:?}: issue {issue} must report a lifetime rather than an instant, \
                 got {granted:?}"
            );
            assert!(
                url.contains(&token),
                "case {case:?}: issue {issue}: the address must carry the token, got {url:?}"
            );
            assert!(
                url.contains(&session_id),
                "case {case:?}: issue {issue}: the address must name the session, got {url:?}"
            );
            // The address is signed by this emulator rather than built by hand, which is
            // what makes it lead back here.
            for field in ["X-Amz-Algorithm", "X-Amz-Signature", "X-Amz-Expires"] {
                assert!(
                    url.contains(field),
                    "case {case:?}: issue {issue}: the address must be signed ({field}), got \
                     {url:?}"
                );
            }
            // ... and following it opens the notebook it points directly to: the ipynb file
            // that notebook holds, served by this emulator at the address it handed out. A
            // session with no notebook has no file to open, so its address serves none.
            let served = fetch(&router, &url).await;
            match opened {
                Some(content) => {
                    assert_eq!(
                        served.status, 200,
                        "case {case:?}: issue {issue}: following the address must open the \
                         notebook, got HTTP {} with {:?}",
                        served.status,
                        String::from_utf8_lossy(&served.body)
                    );
                    assert_eq!(
                        served.content_type.as_deref(),
                        Some(state::NOTEBOOK_MEDIA_TYPE),
                        "case {case:?}: issue {issue}: the notebook must be served as an ipynb \
                         file, got {:?}",
                        served.content_type
                    );
                    assert_eq!(
                        String::from_utf8_lossy(&served.body),
                        content,
                        "case {case:?}: issue {issue}: the address must serve the content that \
                         notebook holds"
                    );
                }
                None => assert_eq!(
                    served.status, 404,
                    "case {case:?}: issue {issue}: a session with no notebook has none to \
                     open, got HTTP {}",
                    served.status
                ),
            }
            // "The URL includes the authentication token and notebook file name": the name
            // comes off the notebook the session was STARTED in.
            match notebook_name {
                Some(name) => {
                    let encoded = name.replace(' ', "%20");
                    assert!(
                        url.contains(name) || url.contains(&encoded),
                        "case {case:?}: issue {issue}: the address must carry the notebook's \
                         name {name:?}, got {url:?}"
                    );
                }
                None => assert!(
                    !url.contains("ipynb"),
                    "case {case:?}: issue {issue}: a session bound to no notebook names none, \
                     got {url:?}"
                ),
            }

            // The token is this store's own: it resolves to the session it was issued for.
            let state = state::service_state(&*engine).expect("the Athena store is reachable");
            let resolved = state.lock().notebook_auth_token_session(&token);
            assert_eq!(
                resolved.as_deref(),
                Some(session_id.as_str()),
                "case {case:?}: issue {issue}: the token must name the session it was issued \
                 for"
            );
            tokens.push(token);
        }
        assert!(
            tokens.windows(2).all(|pair| pair[0] != pair[1]),
            "case {case:?}: each issue must mint another token, got {tokens:?}"
        );

        // Handing out the grant changed nothing about the session: the state a later read
        // reports is still the one the clock puts it in.
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

    // A value this store did not mint names no session, however exactly it has a token's
    // shape — so the token is checked rather than merely carried.
    let state = state::service_state(&*engine).expect("the Athena store is reachable");
    for (case, forged) in [
        ("not hexadecimal at all", "not-a-token".to_string()),
        ("hexadecimal of a token's shape", "00".repeat(96)),
        ("empty", String::new()),
    ] {
        assert_eq!(
            state.lock().notebook_auth_token_session(&forged),
            None,
            "case {case:?}: a value this store did not mint must name no session"
        );
    }
}

/// The grant is refused for a session this account holds none of, and for a session whose
/// resources are no longer running — each with the sentence Athena words that class in.
#[tokio::test]
async fn the_grant_is_refused_for_an_unknown_and_for_a_closed_session() {
    let (_engine, router) = engine_and_router();
    given_spark_work_group(&router, WORK_GROUP).await;
    let notebook_id = given_notebook(&router, "closing.ipynb").await;
    let session_id = given_session(&router, Some(&notebook_id)).await;

    // Rows differ along the shape of an id that names no session: a UUID nothing minted, a
    // single character, the longest `SessionId` the model admits, and one character away
    // from a session this account does hold.
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
        let (status, refused) = presign(&router, &id).await;
        assert_refusal(
            case,
            status,
            &refused,
            ERROR_CODE_UNAUTHORIZED,
            SESSION_NOT_FOUND,
        );
    }

    // The session that IS held is served while it runs.
    let (status, granted) = presign(&router, &session_id).await;
    assert_eq!(
        status, 200,
        "the running session must be served, got {status} with {granted:?}"
    );
    let address = granted["NotebookUrl"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let served = fetch(&router, &address).await;
    assert_eq!(
        served.status, 200,
        "the address of a running session must open its notebook, got HTTP {}",
        served.status
    );

    // An address whose token this emulator did not mint opens nothing, however exactly it
    // has a minted one's shape: the token is CHECKED where the store is, not merely carried.
    let token = granted["AuthToken"].as_str().unwrap_or_default().to_string();
    let forged = address.replace(&token, &"00".repeat(token.len() / 2));
    let refused = fetch(&router, &forged).await;
    assert_eq!(
        refused.status, 404,
        "an address carrying a token this emulator did not mint must open nothing, got HTTP {}",
        refused.status
    );

    // Once it has been asked to shut down there is no notebook server left to address, from
    // the instant the shutdown is asked for through the state it settles in.
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
    let (status, refused) = presign(&router, &session_id).await;
    assert_refusal(
        "a session that is shutting down",
        status,
        &refused,
        ERROR_CODE_INVALID_INPUT,
        NOTEBOOK_SESSION_NOT_OPEN,
    );

    settled_in(&router, &session_id, SESSION_STATE_TERMINATED).await;
    let (status, refused) = presign(&router, &session_id).await;
    assert_refusal(
        "a session whose resources are gone",
        status,
        &refused,
        ERROR_CODE_INVALID_INPUT,
        NOTEBOOK_SESSION_NOT_OPEN,
    );
    // The address that WAS handed out opens nothing either: there is no notebook server
    // behind it once the session's resources are gone.
    let closed = fetch(&router, &address).await;
    assert_eq!(
        closed.status, 404,
        "the address of a session whose resources are gone must open nothing, got HTTP {}",
        closed.status
    );

    // The two refusals are different sentences: a client branching on them can tell a
    // session it must not ask about again from one it closed itself.
    assert_ne!(
        SESSION_NOT_FOUND, NOTEBOOK_SESSION_NOT_OPEN,
        "the unknown-session and the closed-session refusals must not read alike"
    );
}
