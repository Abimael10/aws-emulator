// SPDX-License-Identifier: CC0-1.0

//! Athena's PRESIGNED NOTEBOOK URL, asserted as a SCOPED grant, driven end to end over the
//! same router the emulator boots (awsJson1_1, `x-amz-target: AmazonAthena.<Operation>`).
//!
//! THE RULE UNDER TEST: `CreatePresignedNotebookUrl` "Gets an authentication token and the
//! URL at which the notebook can be accessed", and that URL "includes the authentication
//! token and notebook file name and points directly to the opened notebook" — so one grant
//! opens exactly the notebook the session it was issued for was started in, and nothing
//! else. Every part of the grant is therefore checked where the store is: an address opens a
//! notebook only when the token it carries is one this store minted FOR that session and the
//! file name it carries is that session's notebook's own; any other combination leads to no
//! notebook of this emulator's.
//!
//! The rule is asserted over a TABLE of sessions whose rows differ along what it generalises
//! over: a session in a created notebook, a second session in that SAME notebook, a session
//! in an imported notebook carrying content of its own, and a session bound to no notebook —
//! and then over every CROSS pairing of those rows, which is where a grant that was not
//! scoped to its own session would open another session's notebook.
//!
//! Each cross pairing is asserted BOTH ways round: through the address, which is how a client
//! meets the rule, and against the store that minted the grant
//! (`ComAmazonawsAthenaAmazonathenaState::notebook_auth_token_session`), which names the reason
//! the pairing opens nothing — the token names the session it was issued for and no other.
//!
//! The single-grant rule itself (a fresh token per issue, the signed address, the content each
//! address serves, the session left as a later caller saw it) is owned by
//! `presigned_notebook_url.rs`; what each row asserts here is what a cross pairing has to be
//! read against — that the row WAS granted, under the lifetime the operation publishes.
//!
//! Nothing here waits for a span: every assertion is about the answer to a call.

use std::time::SystemTime;

use serde_json::{json, Value};
use service_com_amazonaws_athena_amazonathena::state::{
    self, NOTEBOOK_ID_CONFIG, NOTEBOOK_TYPE_IPYNB, PYSPARK_ENGINE_VERSION,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{call, engine_and_router, fetch};

/// The Spark enabled workgroup every session below runs in.
const WORK_GROUP: &str = "spark-grant-scope";

/// The seconds `AuthTokenExpirationTime` carries, written here rather than read off the
/// implementation's own constant.
///
/// A LIFETIME rather than an instant: the RECORDED response of this operation carries the ten
/// minutes its prose publishes for refreshing the token ("During programmatic access,
/// `CreatePresignedNotebookUrl` must be called every 10 minutes to refresh the authentication
/// token") for every issue, early or late, where the member's own documentation calls it "The
/// UTC epoch time when the authentication token expires". The recorded wire form governs where
/// the two disagree, and `presigned_notebook_url.rs` is where that form is pinned on its own;
/// it is asserted per row here only so a pairing below is read against a row that WAS granted.
const PUBLISHED_TOKEN_LIFETIME_SECONDS: i64 = 10 * 60;

/// The ipynb document the imported notebook below carries, so the content one address serves
/// is this document rather than any constant this service holds.
const IMPORTED_IPYNB: &str = "{\"cells\": [{\"cell_type\": \"code\", \"source\": \"2 * 21\", \
     \"metadata\": {}, \"outputs\": []}], \"metadata\": {}, \"nbformat\": 4, \
     \"nbformat_minor\": 5}";

/// Create the Spark enabled workgroup the sessions below are started in.
async fn given_spark_work_group(router: &MultiServiceRouter) {
    let (status, created) = call(
        router,
        "CreateWorkGroup",
        &json!({
            "Name": WORK_GROUP,
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
        "the Spark workgroup must be created, got {status} with {created:?}"
    );
}

/// Create the notebook `name` and hand back its `NotebookId`.
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
    id_of(&created, "CreateNotebook", "NotebookId")
}

/// Import the notebook `name` carrying `payload`, and hand back its `NotebookId`.
async fn given_imported_notebook(router: &MultiServiceRouter, name: &str, payload: &str) -> String {
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
    id_of(&imported, "ImportNotebook", "NotebookId")
}

/// Start one session, bound to `notebook_id` when one is given, and hand back its `SessionId`.
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
    id_of(&started, "StartSession", "SessionId")
}

/// What the store serves at the address naming `session_id` and `notebook_name` against
/// `token`: the predicate the route serving a notebook address consults
/// (`state::notebook_at_address`), asked here directly.
///
/// Beside each pairing below, so a refusal is attributed to the STORE's own check of the grant
/// rather than to anything the transport did with a doctored address.
fn opened_at(
    engine: &engine::InMemoryEngine,
    session_id: &str,
    notebook_name: Option<&str>,
    token: &str,
) -> Option<String> {
    state::notebook_at_address(engine, session_id, notebook_name, token, SystemTime::now())
}

/// One identifier an answer must carry.
fn id_of(body: &Value, operation: &str, member: &str) -> String {
    body[member]
        .as_str()
        .unwrap_or_else(|| panic!("{operation} must answer with a {member}, got {body:?}"))
        .to_string()
}

/// One row of the table: a session, the notebook file name its address must carry and the
/// content that address must serve.
struct SessionCase {
    case: &'static str,
    notebook_id: Option<String>,
    notebook_name: Option<&'static str>,
    opened: Option<&'static str>,
}

/// One grant handed out for one row, as the row's session was answered it.
struct Grant {
    case: &'static str,
    session_id: String,
    notebook_name: Option<&'static str>,
    opened: Option<&'static str>,
    url: String,
    token: String,
}

/// A grant opens the notebook of the session it was issued for, and no grant of any other
/// session — neither with another session's token nor pointed at another notebook's file.
#[tokio::test]
async fn a_grant_opens_only_the_notebook_of_the_session_it_was_issued_for() {
    let (engine, router) = engine_and_router();
    given_spark_work_group(&router).await;
    let created = given_notebook(&router, "alpha.ipynb").await;
    let imported = given_imported_notebook(&router, "beta.ipynb", IMPORTED_IPYNB).await;

    let cases: Vec<SessionCase> = vec![
        SessionCase {
            case: "a session in a created notebook",
            notebook_id: Some(created.clone()),
            notebook_name: Some("alpha.ipynb"),
            opened: Some(state::EMPTY_IPYNB),
        },
        SessionCase {
            case: "a second session in that same notebook",
            notebook_id: Some(created.clone()),
            notebook_name: Some("alpha.ipynb"),
            opened: Some(state::EMPTY_IPYNB),
        },
        SessionCase {
            case: "a session in an imported notebook",
            notebook_id: Some(imported.clone()),
            notebook_name: Some("beta.ipynb"),
            opened: Some(IMPORTED_IPYNB),
        },
        SessionCase {
            case: "a session bound to no notebook",
            notebook_id: None,
            notebook_name: None,
            opened: None,
        },
    ];

    // One grant per row, each asserted to be the grant of its OWN session.
    let mut grants: Vec<Grant> = Vec::new();
    for SessionCase {
        case,
        notebook_id,
        notebook_name,
        opened,
    } in cases
    {
        let session_id = given_session(&router, notebook_id.as_deref()).await;
        let (status, granted) = call(
            &router,
            "CreatePresignedNotebookUrl",
            &json!({ "SessionId": session_id }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the grant must be served, got {status} with {granted:?}"
        );
        let token = id_of(&granted, "CreatePresignedNotebookUrl", "AuthToken");
        let url = id_of(&granted, "CreatePresignedNotebookUrl", "NotebookUrl");
        assert_eq!(
            granted["AuthTokenExpirationTime"].as_i64(),
            Some(PUBLISHED_TOKEN_LIFETIME_SECONDS),
            "case {case:?}: the published token lifetime must be reported, got {granted:?}"
        );
        assert!(
            url.contains(&session_id),
            "case {case:?}: the address must name its own session, got {url:?}"
        );
        assert!(
            url.contains(&token),
            "case {case:?}: the address must carry its own token, got {url:?}"
        );

        // The address opens the notebook that session was started in, with its content read
        // out of the store; a session with none to open has no notebook at its address.
        let served = fetch(&router, &url).await;
        match opened {
            Some(content) => {
                assert_eq!(
                    served.status, 200,
                    "case {case:?}: the address must open its notebook, got HTTP {} with {:?}",
                    served.status,
                    String::from_utf8_lossy(&served.body)
                );
                assert_eq!(
                    String::from_utf8_lossy(&served.body),
                    content,
                    "case {case:?}: the address must serve the content that notebook holds"
                );
            }
            None => assert_eq!(
                served.status, 404,
                "case {case:?}: a session with no notebook has none to open, got HTTP {}",
                served.status
            ),
        }

        // The token is the store's own tag over the session it was issued for, which is what
        // the open above resolved it through: asserted against the store directly, so the
        // cross pairings below are read as the token-to-session check failing rather than as
        // anything the transport did with a doctored address.
        let store = state::service_state(&*engine).expect("the Athena store is reachable");
        assert_eq!(
            store.lock().notebook_auth_token_session(&token).as_deref(),
            Some(session_id.as_str()),
            "case {case:?}: the token must name the session it was issued for"
        );

        grants.push(Grant {
            case,
            session_id,
            notebook_name,
            opened,
            url,
            token,
        });
    }

    // No two sessions share a grant: each token was minted for its own session, so a token
    // names the one session whose notebook it opens.
    for held in &grants {
        for other in &grants {
            if held.session_id == other.session_id {
                continue;
            }
            assert_ne!(
                held.token, other.token,
                "case {:?} vs {:?}: two sessions must not be handed the same token",
                held.case, other.case
            );
            assert_ne!(
                held.url, other.url,
                "case {:?} vs {:?}: two sessions must not be handed the same address",
                held.case, other.case
            );

            // The other row's token names the OTHER session, so it authenticates nothing at
            // this session's address — the store-level reason the cross pairing below opens
            // nothing.
            let store = state::service_state(&*engine).expect("the Athena store is reachable");
            assert_eq!(
                store
                    .lock()
                    .notebook_auth_token_session(&other.token)
                    .as_deref(),
                Some(other.session_id.as_str()),
                "case {:?} vs {:?}: the other row's token must name its own session",
                held.case,
                other.case
            );

            // The CROSS pairing: this session's own address, carrying the other session's
            // token. The token is checked against the session the address names, so this
            // opens nothing — even where both sessions were started in the SAME notebook.
            //
            // Only for a row that HAS a notebook to open: the address of a session bound to
            // none opens nothing under its own token either, so swapping one in could not
            // fail. Such a row still takes part as the OTHER side of every pairing.
            let Some(held_notebook) = held.notebook_name else {
                continue;
            };
            let swapped = held.url.replace(&held.token, &other.token);
            assert_ne!(
                swapped, held.url,
                "case {:?} vs {:?}: the swap must change the address it is made from",
                held.case, other.case
            );
            let refused = fetch(&router, &swapped).await;
            assert_eq!(
                refused.status, 404,
                "case {:?} vs {:?}: an address carrying another session's token must open \
                 nothing, got HTTP {}",
                held.case,
                other.case,
                refused.status
            );
            assert_eq!(
                opened_at(&engine, &held.session_id, held.notebook_name, &other.token),
                None,
                "case {:?} vs {:?}: the store must serve nothing for this session against \
                 another session's token",
                held.case,
                other.case
            );
            assert!(
                opened_at(&engine, &held.session_id, held.notebook_name, &held.token).is_some(),
                "case {:?} vs {:?}: the store must still serve this session's own notebook \
                 against its own token",
                held.case,
                other.case
            );

            // ... and this session's own grant, pointed at the other row's notebook file:
            // the name is the one the session's OWN notebook carries, so no other name opens
            // a notebook at that address.
            let Some(other_name) = other.notebook_name else {
                continue;
            };
            if held_notebook == other_name {
                continue;
            }
            let renamed = held.url.replace(held_notebook, other_name);
            assert_ne!(
                renamed, held.url,
                "case {:?} vs {:?}: the rename must change the address it is made from",
                held.case, other.case
            );
            let refused = fetch(&router, &renamed).await;
            assert_eq!(
                refused.status, 404,
                "case {:?} vs {:?}: an address naming another notebook's file must open \
                 nothing, got HTTP {}",
                held.case,
                other.case,
                refused.status
            );
            // ... and the store is where that comes from: against this session's OWN token,
            // the other row's file name serves nothing.
            assert_eq!(
                opened_at(&engine, &held.session_id, Some(other_name), &held.token),
                None,
                "case {:?} vs {:?}: the store must serve nothing for this session under \
                 another notebook's file name",
                held.case,
                other.case
            );
        }
    }

    // Nothing rightly granted was broken by the pairings above: each session's OWN address
    // still opens its own notebook, so the 404s came from the pairing and not from the grant.
    for Grant {
        case, url, opened, ..
    } in &grants
    {
        let served = fetch(&router, url).await;
        match opened {
            Some(content) => {
                assert_eq!(
                    served.status, 200,
                    "case {case:?}: the address must still open its notebook, got HTTP {}",
                    served.status
                );
                assert_eq!(
                    String::from_utf8_lossy(&served.body),
                    *content,
                    "case {case:?}: the address must still serve that notebook's content"
                );
            }
            None => assert_eq!(
                served.status, 404,
                "case {case:?}: a session with no notebook still has none to open, got HTTP {}",
                served.status
            ),
        }
    }
}
