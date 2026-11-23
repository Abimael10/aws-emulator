// SPDX-License-Identifier: CC0-1.0

//! `UpdateNotebook`, driven end to end over the same router the emulator boots (awsJson1_1,
//! `x-amz-target: AmazonAthena.UpdateNotebook`), and read back through the readers that
//! resolve a `NotebookId`.
//!
//! THE RULE UNDER TEST: an update REPLACES the stored content of the notebook the request
//! names with the document the request carried and moves that notebook's
//! `LastModifiedTime` to the instant of the write, leaving everything that identifies the
//! notebook — its id, its name, the workgroup it lives in and when it was created — as it
//! was; a `NotebookId` no create or import minted names nothing to write, a notebook a
//! session is still serving is written only through that session, and a repeat under one
//! `ClientRequestToken` writes nothing further.
//!
//! Each rule is asserted over a TABLE rather than one request: the rows differ along the
//! form, the size and the character set of the ipynb document written, the shape of an id
//! that names no notebook, which session (or none) the request names and where in its
//! lifecycle that session sits, and whether an idempotency token was reused for the same
//! content or for other content.
//!
//! Nothing here waits on the wall clock: a session that must be at a given point of its
//! lifecycle is seeded with the instants that put it there, through this service's own
//! [`SessionRecord`], and every call is read back over the wire.

use std::time::{Duration, SystemTime};

use serde_json::{json, Value};
use service_com_amazonaws_athena_amazonathena::state::{
    self, SessionRecord, ERROR_CODE_INVALID_INPUT, ERROR_CODE_NOTEBOOK_NOT_FOUND,
    ERROR_CODE_UNAUTHORIZED, NOTEBOOK_SESSION_ID_REQUIRED, NOTEBOOK_TYPE_IPYNB,
    SESSION_NOT_FOUND,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{assert_refusal, call, engine_and_router, message};

/// The Spark enabled workgroup the notebooks below live in.
const WORK_GROUP: &str = "spark-notebooks";

/// The PySpark engine version a Spark enabled workgroup selects, which is what makes the
/// workgroup one a notebook can live in.
const PYSPARK_ENGINE_VERSION: &str = "PySpark engine version 3";

/// A span long enough that a session seeded as started that long ago is past every span
/// this emulator owes, for as long as any test run takes: no assertion here depends on how
/// fast the host runs.
const WELL_PAST_ANY_RUN: Duration = Duration::from_secs(3_600);

/// Create the Spark enabled workgroup the notebooks are created in.
async fn given_spark_work_group(router: &MultiServiceRouter) {
    let (status, created) = call(
        router,
        "CreateWorkGroup",
        &json!({
            "Name": WORK_GROUP,
            "Configuration": {
                "EngineVersion": { "SelectedEngineVersion": PYSPARK_ENGINE_VERSION },
                "ExecutionRole": "arn:aws:iam::000000000000:role/AthenaSparkExecution"
            }
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the Spark workgroup {WORK_GROUP} must be created, got {status} with {created:?}"
    );
}

/// Create a notebook called `name` and hand back the `NotebookId` it minted.
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
        .unwrap_or_else(|| panic!("a create must answer with a NotebookId, got {created:?}"))
        .to_string()
}

/// One `UpdateNotebook` call, as a client sends it.
async fn update(router: &MultiServiceRouter, request: &Value) -> (u16, Value) {
    call(router, "UpdateNotebook", request).await
}

/// The update request that writes `payload` into the notebook `notebook_id`.
fn write_of(notebook_id: &str, payload: &str) -> Value {
    json!({
        "NotebookId": notebook_id,
        "Payload": payload,
        "Type": NOTEBOOK_TYPE_IPYNB,
    })
}

/// `ExportNotebook`'s view of one notebook, which must be readable.
async fn exported(router: &MultiServiceRouter, notebook_id: &str) -> Value {
    let (status, body) = call(
        router,
        "ExportNotebook",
        &json!({ "NotebookId": notebook_id }),
    )
    .await;
    assert_eq!(
        status, 200,
        "ExportNotebook({notebook_id}) must answer 200, got {status} with {body:?}"
    );
    body
}

/// The content the notebook `notebook_id` holds, as an export answers with it.
async fn content(router: &MultiServiceRouter, notebook_id: &str) -> String {
    let body = exported(router, notebook_id).await;
    body["Payload"]
        .as_str()
        .unwrap_or_else(|| panic!("an export must carry the Payload, got {body:?}"))
        .to_string()
}

/// `GetNotebookMetadata`'s view of one notebook, which must be readable.
async fn metadata(router: &MultiServiceRouter, notebook_id: &str) -> Value {
    let (status, body) = call(
        router,
        "GetNotebookMetadata",
        &json!({ "NotebookId": notebook_id }),
    )
    .await;
    assert_eq!(
        status, 200,
        "GetNotebookMetadata({notebook_id}) must answer 200, got {status} with {body:?}"
    );
    body["NotebookMetadata"].clone()
}

/// Seed one session into `engine`'s store, started in `notebook_id` at `started_at`, shut
/// down at that same instant when `terminated`, and hand back its `SessionId`.
///
/// Seeding is what lets a row place a session at an exact point of its lifecycle, which
/// starting one over the wire cannot; the update itself is always driven over the wire.
fn given_session(
    engine: &engine::InMemoryEngine,
    notebook_id: Option<&str>,
    started_at: SystemTime,
    terminated: bool,
) -> String {
    let id = state::new_session_id();
    let mut record = SessionRecord::started(
        id.clone(),
        WORK_GROUP.to_string(),
        notebook_id.map(str::to_string),
        started_at,
    );
    if terminated {
        record.terminate(started_at);
    }
    // The seeded instants must really put the session where the row claims, so a mis-seeded
    // row cannot pass by accident.
    assert_eq!(
        record.is_active_at(SystemTime::now()),
        !terminated,
        "a session seeded as terminated={terminated} must be active={}, got the state {:?}",
        !terminated,
        record.state_at(SystemTime::now())
    );
    let store = state::service_state(engine).expect("this service's state");
    store.lock().sessions.insert(record);
    id
}

/// RULE: an update replaces the content the notebook holds, whatever its form or size, and
/// moves only that content and the instant it was last modified at.
#[tokio::test]
async fn an_update_replaces_the_content_a_read_answers_with() {
    let (_engine, router) = engine_and_router();
    given_spark_work_group(&router).await;
    let notebook_id = given_notebook(&router, "analysis").await;

    let created = metadata(&router, &notebook_id).await;
    let creation_time = created["CreationTime"].as_f64();
    assert!(
        creation_time.is_some(),
        "the created notebook carries a CreationTime, got {created:?}"
    );
    assert_eq!(
        created["LastModifiedTime"].as_f64(),
        creation_time,
        "a notebook nothing has updated was last modified when it was created, got {created:?}"
    );

    // The rows differ along the FORM, the SIZE and the character set of the document
    // written: the modelled `Payload` is a string of 1 to 10485760 bytes with no further
    // constraint, so the shortest it admits is one byte.
    let documents = [
        (
            "an empty ipynb document",
            "{\"cells\": [], \"metadata\": {}, \"nbformat\": 4, \"nbformat_minor\": 5}"
                .to_string(),
        ),
        (
            "a notebook with a code cell",
            "{\"cells\": [{\"cell_type\": \"code\", \"source\": [\"spark.sql('SELECT 1')\"]}], \
             \"nbformat\": 4, \"nbformat_minor\": 5}"
                .to_string(),
        ),
        (
            "content outside ASCII",
            "{\"cells\": [{\"cell_type\": \"markdown\", \"source\": [\"# 分析ノート\"]}]}".to_string(),
        ),
        ("the shortest payload the model admits", "{".to_string()),
        (
            "a document of a hundred thousand bytes",
            format!("{{\"cells\": [\"{}\"]}}", "a".repeat(100_000)),
        ),
    ];

    let mut previous = state::EMPTY_IPYNB.to_string();
    for (case, document) in &documents {
        assert_eq!(
            content(&router, &notebook_id).await,
            previous,
            "case {case:?}: the notebook must still hold what the last write left"
        );

        let (status, answered) = update(&router, &write_of(&notebook_id, document)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected UpdateNotebook to answer 200, got {status} with \
             {answered:?}"
        );

        let read_back = content(&router, &notebook_id).await;
        assert_eq!(
            read_back, *document,
            "case {case:?}: expected the export to answer with the written document ({} \
             bytes), got {} bytes",
            document.len(),
            read_back.len()
        );

        // Nothing that identifies the notebook moved with its content, and the instant it
        // was last modified at is no earlier than the instant it was created at.
        let after = metadata(&router, &notebook_id).await;
        assert_eq!(
            after["NotebookId"].as_str(),
            Some(notebook_id.as_str()),
            "case {case:?}: the update names the same notebook, got {after:?}"
        );
        assert_eq!(
            after["Name"].as_str(),
            Some("analysis"),
            "case {case:?}: an update of the contents does not rename the notebook, got \
             {after:?}"
        );
        assert_eq!(
            after["WorkGroup"].as_str(),
            Some(WORK_GROUP),
            "case {case:?}: the notebook stays in its workgroup, got {after:?}"
        );
        assert_eq!(
            after["Type"].as_str(),
            Some(NOTEBOOK_TYPE_IPYNB),
            "case {case:?}: the only notebook type is {NOTEBOOK_TYPE_IPYNB}, got {after:?}"
        );
        assert_eq!(
            after["CreationTime"].as_f64(),
            creation_time,
            "case {case:?}: an update does not move the creation instant, got {after:?}"
        );
        let last_modified = after["LastModifiedTime"].as_f64();
        assert!(
            last_modified >= creation_time,
            "case {case:?}: expected a LastModifiedTime at or after the CreationTime \
             {creation_time:?}, got {last_modified:?}"
        );
        // The two readers of one notebook answer with the same record.
        assert_eq!(
            exported(&router, &notebook_id).await["NotebookMetadata"],
            after,
            "case {case:?}: the export and the metadata read report one record"
        );
        previous = document.clone();
    }
}

/// RULE: a `NotebookId` this account minted no notebook under names nothing whose content
/// could be replaced, whatever the shape of the id.
#[tokio::test]
async fn an_update_needs_a_notebook_this_account_holds() {
    let (_engine, router) = engine_and_router();
    given_spark_work_group(&router).await;
    let held = given_notebook(&router, "analysis").await;

    // The rows differ along the SHAPE of an id of the modelled form
    // (`^[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}$`) that names no
    // notebook: a freshly minted one, the all-zero one and the all-f one.
    let misses = [
        ("an id no notebook was minted under", state::new_notebook_id()),
        (
            "the all-zero id of the modelled form",
            "00000000-0000-0000-0000-000000000000".to_string(),
        ),
        (
            "the all-f id of the modelled form",
            "ffffffff-ffff-ffff-ffff-ffffffffffff".to_string(),
        ),
    ];

    for (case, missing) in &misses {
        let (status, refused) = update(&router, &write_of(missing, "{\"cells\": []}")).await;
        assert_refusal(
            case,
            status,
            &refused,
            ERROR_CODE_NOTEBOOK_NOT_FOUND,
            &state::notebook_not_found(missing),
        );
    }

    // No refusal wrote anything: the notebook this account does hold still holds the
    // document its create left.
    assert_eq!(
        content(&router, &held).await,
        state::EMPTY_IPYNB,
        "a refused update must write nothing, and the created notebook holds the empty \
         ipynb document"
    );
}

/// RULE: while a session is serving a notebook, its content is written through THAT session:
/// a request naming no session is refused, and one naming a session that is not an active
/// session of this notebook names nothing to write through.
#[tokio::test]
async fn a_notebook_an_active_session_serves_is_written_through_that_session() {
    let (engine, router) = engine_and_router();
    given_spark_work_group(&router).await;
    let served = given_notebook(&router, "served").await;
    let other = given_notebook(&router, "other").await;
    let dormant = given_notebook(&router, "dormant").await;
    let quiet = given_notebook(&router, "quiet").await;

    let long_ago = SystemTime::now() - WELL_PAST_ANY_RUN;
    // One session per row: still running in the notebook under test, still running in
    // ANOTHER notebook, shut down in a third, and one bound to no notebook at all.
    let active = given_session(&engine, Some(&served), long_ago, false);
    let elsewhere = given_session(&engine, Some(&other), long_ago, false);
    let shut_down = given_session(&engine, Some(&dormant), long_ago, true);
    let unbound = given_session(&engine, None, long_ago, false);

    // A notebook a session is serving, with no session named: refused for the session the
    // request does not name.
    let (status, refused) = update(&router, &write_of(&served, "{\"cells\": [1]}")).await;
    assert_refusal(
        "a served notebook with no SessionId",
        status,
        &refused,
        ERROR_CODE_INVALID_INPUT,
        NOTEBOOK_SESSION_ID_REQUIRED,
    );

    // The rows differ along WHICH session the request names and where that session sits:
    // one serving another notebook, one whose resources are no longer running, one bound to
    // no notebook, and an id naming no session at all.
    let not_a_session_of_this_notebook = [
        ("a session serving another notebook", elsewhere.clone()),
        ("a session that was shut down", shut_down),
        ("a session bound to no notebook", unbound),
        ("an id naming no session", state::new_session_id()),
    ];
    for (case, session_id) in &not_a_session_of_this_notebook {
        let mut request = write_of(&served, "{\"cells\": [2]}");
        request["SessionId"] = json!(session_id);
        let (status, refused) = update(&router, &request).await;
        assert_refusal(
            case,
            status,
            &refused,
            ERROR_CODE_UNAUTHORIZED,
            SESSION_NOT_FOUND,
        );
    }

    // Nothing above wrote: the served notebook still holds what its create left.
    assert_eq!(
        content(&router, &served).await,
        state::EMPTY_IPYNB,
        "a refused update must write nothing into the served notebook"
    );

    // The session that IS serving the notebook writes it.
    let written = "{\"cells\": [\"through the session\"]}";
    let mut request = write_of(&served, written);
    request["SessionId"] = json!(active);
    let (status, answered) = update(&router, &request).await;
    assert_eq!(
        status, 200,
        "case \"the notebook's own active session\": expected 200, got {status} with \
         {answered:?}"
    );
    assert_eq!(
        content(&router, &served).await,
        written,
        "case \"the notebook's own active session\": expected the written document"
    );

    // A notebook no ACTIVE session serves takes an update that names none: one whose only
    // session has shut down, and one no session was ever started in.
    let unserved = [
        ("a notebook whose only session shut down", dormant),
        ("a notebook no session was started in", quiet),
    ];
    for (case, notebook_id) in &unserved {
        let document = format!("{{\"cells\": [\"{case}\"]}}");
        let (status, answered) = update(&router, &write_of(notebook_id, &document)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected an update with no SessionId to answer 200, got {status} \
             with {answered:?}"
        );
        assert_eq!(
            content(&router, notebook_id).await,
            document,
            "case {case:?}: expected the written document"
        );
    }

    // The session serving another notebook still serves it: that notebook takes its own
    // update through its own session.
    let mut request = write_of(&other, "{\"cells\": [\"elsewhere\"]}");
    request["SessionId"] = json!(elsewhere);
    let (status, answered) = update(&router, &request).await;
    assert_eq!(
        status, 200,
        "case \"another notebook through its own session\": expected 200, got {status} with \
         {answered:?}"
    );
}

/// RULE: `ClientRequestToken` makes an update execute only once: a repeat under the token
/// the stored content already stands under writes nothing further, and the same token
/// carrying other content describes another update, which is refused.
#[tokio::test]
async fn one_client_request_token_updates_once() {
    let (_engine, router) = engine_and_router();
    given_spark_work_group(&router).await;

    // The modelled `ClientRequestToken` is a hyphenated hex token
    // (`^[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}$`); the rows differ
    // along the document written under each.
    let rows = [
        (
            "an ipynb document",
            "11111111-1111-4111-8111-111111111111",
            "{\"cells\": [{\"cell_type\": \"code\", \"source\": [\"1\"]}]}",
        ),
        (
            "a document outside ASCII",
            "22222222-2222-4222-8222-222222222222",
            "{\"cells\": [{\"cell_type\": \"markdown\", \"source\": [\"冪等性\"]}]}",
        ),
    ];

    for (case, token, document) in rows {
        let notebook_id = given_notebook(&router, &format!("idempotent {case}")).await;
        let mut request = write_of(&notebook_id, document);
        request["ClientRequestToken"] = json!(token);

        let (status, first) = update(&router, &request).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the first update to answer 200, got {status} with \
             {first:?}"
        );
        let after_first = metadata(&router, &notebook_id).await;

        let (status, again) = update(&router, &request).await;
        assert_eq!(
            status, 200,
            "case {case:?}: an update repeated under one token executes once and answers the \
             same way, got {status} with {again:?}"
        );
        assert_eq!(
            metadata(&router, &notebook_id).await,
            after_first,
            "case {case:?}: the repeat must write nothing, so the notebook keeps the instant \
             of the one write it was modified by"
        );
        assert_eq!(
            content(&router, &notebook_id).await,
            document,
            "case {case:?}: the repeat must leave the written document in place"
        );

        // The same token carrying OTHER content describes another update.
        let mut changed = write_of(&notebook_id, "{\"cells\": [\"changed\"]}");
        changed["ClientRequestToken"] = json!(token);
        let (status, mismatched) = update(&router, &changed).await;
        assert_refusal(
            case,
            status,
            &mismatched,
            ERROR_CODE_INVALID_INPUT,
            state::IDEMPOTENT_PARAMETERS_DO_NOT_MATCH,
        );
        assert_eq!(
            content(&router, &notebook_id).await,
            document,
            "case {case:?}: the refused repeat must write nothing, got {:?}",
            message(&mismatched)
        );

        // ANOTHER token is another update, however alike the two requests are.
        let mut second = write_of(&notebook_id, "{\"cells\": [\"changed\"]}");
        second["ClientRequestToken"] = json!("33333333-3333-4333-8333-333333333333");
        let (status, accepted) = update(&router, &second).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected an update under another token to answer 200, got \
             {status} with {accepted:?}"
        );
        assert_eq!(
            content(&router, &notebook_id).await,
            "{\"cells\": [\"changed\"]}",
            "case {case:?}: expected the document the second token wrote"
        );
    }
}
