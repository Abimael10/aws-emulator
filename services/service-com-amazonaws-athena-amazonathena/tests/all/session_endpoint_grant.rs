// SPDX-License-Identifier: CC0-1.0

//! The TWO grants Athena hands out over one session, driven end to end over the same router
//! the emulator boots (awsJson1_1, `x-amz-target: AmazonAthena.<Operation>`):
//! `GetSessionEndpoint` "gets a connection endpoint and authentication token for a given
//! session Id", and `CreatePresignedNotebookUrl` "gets an authentication token and the URL at
//! which the notebook can be accessed".
//!
//! THE RULE UNDER TEST: they are two grants, not one. Each call mints a token of its own kind,
//! and a token minted for one grant resolves as that grant's token and as no other — so a
//! client holding a notebook address cannot present its token as a connection token, or the
//! other way round — while both name the session they were issued for. The addresses differ
//! too: one is the session's connection endpoint, the other the notebook file's own address.
//!
//! The rows differ along which grant minted the token and which resolver is asked, so the
//! whole cross-product of the two kinds is asserted rather than one pairing.
//!
//! Nothing here waits for a fixed span: the one transition the clock owes is waited on by
//! POLLING for the state itself, under a deadline far above the span it is due after.

use std::time::Duration;

use serde_json::{json, Value};
use service_com_amazonaws_athena_amazonathena::state::{
    self, NOTEBOOK_ID_CONFIG, PYSPARK_ENGINE_VERSION, SESSION_STATE_IDLE,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{call, engine_and_router};

/// The Spark enabled workgroup the session below runs in.
const WORK_GROUP: &str = "spark-session-grants";

/// How long a poll for the transition a start owes keeps looking, and how long it waits
/// between looks. The deadline is far above the span a session's resources are due after, so
/// the CONDITION is what is waited on rather than a span of the host's own speed.
const POLL_INTERVAL: Duration = Duration::from_millis(50);
/// See [`POLL_INTERVAL`].
const POLL_LOOKS: u32 = 1_200;

/// Create the Spark enabled workgroup and the notebook the session is started in, and hand
/// back the `NotebookId`.
async fn given_notebook_work_group(router: &MultiServiceRouter) -> String {
    let (status, created) = call(
        router,
        "CreateWorkGroup",
        &json!({
            "Name": WORK_GROUP,
            "Configuration": {
                "EngineVersion": { "SelectedEngineVersion": PYSPARK_ENGINE_VERSION },
                "ExecutionRole": "arn:aws:iam::000000000000:role/AthenaSparkExecution",
                "ResultConfiguration": { "OutputLocation": "s3://athena-spark/grants/" }
            }
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the Spark workgroup must be created, got {status} with {created:?}"
    );
    let (status, created) = call(
        router,
        "CreateNotebook",
        &json!({ "WorkGroup": WORK_GROUP, "Name": "grants.ipynb" }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the notebook must be created, got {status} with {created:?}"
    );
    created["NotebookId"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateNotebook must answer with an id, got {created:?}"))
        .to_string()
}

/// Start one session in that workgroup, bound to `notebook_id`, and hand back its `SessionId`
/// once its resources are up.
async fn given_running_session(router: &MultiServiceRouter, notebook_id: &str) -> String {
    let (status, started) = call(
        router,
        "StartSession",
        &json!({
            "WorkGroup": WORK_GROUP,
            "EngineConfiguration": {
                "CoordinatorDpuSize": 1,
                "MaxConcurrentDpus": 4,
                "DefaultExecutorDpuSize": 1,
                "AdditionalConfigs": { NOTEBOOK_ID_CONFIG: notebook_id }
            }
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "StartSession must be accepted, got {status} with {started:?}"
    );
    let session_id = started["SessionId"]
        .as_str()
        .unwrap_or_else(|| panic!("StartSession must answer with a SessionId, got {started:?}"))
        .to_string();
    for _ in 0..POLL_LOOKS {
        let (status, read) = call(
            router,
            "GetSessionStatus",
            &json!({ "SessionId": session_id }),
        )
        .await;
        assert_eq!(
            status, 200,
            "GetSessionStatus must answer 200 while polling, got {status} with {read:?}"
        );
        if read["Status"]["State"].as_str() == Some(SESSION_STATE_IDLE) {
            return session_id;
        }
        ::tokio::time::sleep(POLL_INTERVAL).await;
    }
    panic!("the session {session_id} never reached {SESSION_STATE_IDLE}");
}

/// One grant's answer, which must be served.
async fn granted(router: &MultiServiceRouter, operation: &str, session_id: &str) -> Value {
    let (status, answer) = call(
        router,
        operation,
        &json!({ "SessionId": session_id }),
    )
    .await;
    assert_eq!(
        status, 200,
        "{operation} must be served for a running session, got {status} with {answer:?}"
    );
    answer
}

/// RULE: each grant mints a token of its OWN kind — one resolves as a connection token and the
/// other as a notebook address's authenticator, and neither resolves as the other — and the
/// two addresses are not the same address.
#[tokio::test]
async fn the_two_grants_over_one_session_mint_tokens_of_their_own_kinds() {
    let (engine, router) = engine_and_router();
    let notebook_id = given_notebook_work_group(&router).await;
    let session_id = given_running_session(&router, &notebook_id).await;

    let endpoint = granted(&router, "GetSessionEndpoint", &session_id).await;
    let notebook = granted(&router, "CreatePresignedNotebookUrl", &session_id).await;
    let connection_token = endpoint["AuthToken"].as_str().unwrap_or_default().to_string();
    let notebook_token = notebook["AuthToken"].as_str().unwrap_or_default().to_string();
    assert!(
        !connection_token.is_empty() && !notebook_token.is_empty(),
        "both grants must carry a token, got {endpoint:?} and {notebook:?}"
    );
    assert_ne!(
        connection_token, notebook_token,
        "the two grants must not hand out one token"
    );

    // (case, the token, which resolver must name the session, which must name nothing)
    let rows: Vec<(&str, &String, bool)> = vec![
        ("the connection token", &connection_token, true),
        ("the notebook address's token", &notebook_token, false),
    ];
    let store = state::service_state(&*engine).expect("the Athena store is reachable");
    for (case, token, is_connection) in rows {
        let held = store.lock();
        let as_connection = held.session_endpoint_auth_token_session(token);
        let as_notebook = held.notebook_auth_token_session(token);
        drop(held);
        let (names_session, names_nothing) = if is_connection {
            (as_connection, as_notebook)
        } else {
            (as_notebook, as_connection)
        };
        assert_eq!(
            names_session.as_deref(),
            Some(session_id.as_str()),
            "case {case:?}: it must name the session it was issued for"
        );
        assert_eq!(
            names_nothing, None,
            "case {case:?}: it must resolve as no grant of the other kind"
        );
    }

    // The addresses are two addresses: the session's connection endpoint, and the notebook
    // file the session serves.
    let endpoint_url = endpoint["EndpointUrl"].as_str().unwrap_or_default();
    let notebook_url = notebook["NotebookUrl"].as_str().unwrap_or_default();
    assert_ne!(
        endpoint_url, notebook_url,
        "the connection endpoint and the notebook address must not be one address"
    );
    assert!(
        endpoint_url.contains(&session_id) && notebook_url.contains(&session_id),
        "each address must name the session it is for, got {endpoint_url:?} and {notebook_url:?}"
    );
}
