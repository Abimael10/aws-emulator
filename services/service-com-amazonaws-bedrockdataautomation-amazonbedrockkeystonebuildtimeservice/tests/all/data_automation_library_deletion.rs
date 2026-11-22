// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of a DataAutomationLibrary's TEARDOWN.
//!
//! THE RULE under test: `DeleteDataAutomationLibrary` ACCEPTS the deletion of the library
//! its `libraryArn` names and answers the status that leaves it in — the modelled
//! `DELETING` — and the removal the call owes settles on its own: from the instant it falls
//! due the library resolves to nothing, so a read of it is the modelled
//! `ResourceNotFoundException` (HTTP 404) with the service's fixed sentence, a listing no
//! longer enumerates it, the ingestion jobs submitted against it are gone with it, and the
//! `libraryName` it held is free for another create. A delete of a library that is not there
//! is that same 404. A library a DataAutomationProject still references is not released at
//! all: that delete is the modelled `ValidationException` (HTTP 400) and writes nothing.
//!
//! The suites are TABLE-driven along what the rule generalises over: the operation a
//! released library is addressed through, the number of libraries in the store, whether the
//! library carried ingestion jobs, how many times the `@idempotent` delete is called, and
//! which library each project of the store references. Nothing here asserts on elapsed time:
//! a case that waits for the settling polls the CONDITION — the 404 — under a deadline far
//! larger than the span.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_bedrockdataautomation_amazonbedrockkeystonebuildtimeservice::state;
use service_com_amazonaws_bedrockdataautomation_amazonbedrockkeystonebuildtimeservice::state::ComAmazonawsBedrockdataautomationAmazonbedrockkeystonebuildtimeserviceState as ServiceState;
use service_com_amazonaws_bedrockdataautomation_amazonbedrockkeystonebuildtimeservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

use crate::support::{encode_label, error_code, string_at};

/// The Region every case signs for.
const REGION: &str = "us-east-1";

/// How long a case is willing to wait for the removal an accepted delete owes, and how
/// often it looks again. The deadline is far larger than
/// [`state::library_deletion::LIBRARY_DELETION_SPAN`] so a loaded host cannot fail a case
/// that a fast one passes; what is asserted is the CONDITION, never the time it took.
const SETTLING_DEADLINE: Duration = Duration::from_secs(60);
const LOOK_AGAIN_AFTER: Duration = Duration::from_millis(25);

fn authorization() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDTESTKEY/20240101/{REGION}/bedrock/aws4_request, \
         SignedHeaders=host, Signature=0123456789abcdef"
    )
}

/// The emulator under test, with a handle on the shared engine so a case can assert what the
/// store HOLDS and not only what it answers.
fn emulator() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(Arc::clone(&engine)));
    (router, engine)
}

async fn call(router: &MultiServiceRouter, method: &str, uri: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "bedrock-data-automation.amazonaws.com")
        .header("content-type", "application/json")
        .header("authorization", authorization())
        .body(Bytes::from(
            serde_json::to_vec(body).expect("serializable body"),
        ))
        .expect("a valid test request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let decoded = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, decoded)
}

/// One library's path, as every operation of the library's own URI space binds it.
fn library_uri(arn: &str) -> String {
    format!("/data-automation-libraries/{}/", encode_label(arn))
}

/// The ingestion-job collection's path for one library.
fn jobs_uri(arn: &str) -> String {
    format!(
        "/data-automation-libraries/{}/library-ingestion-jobs/",
        encode_label(arn)
    )
}

/// `CreateDataAutomationLibrary`; returns the `libraryArn` it minted.
async fn create_library(router: &MultiServiceRouter, name: &str) -> String {
    let (status, payload) = call(
        router,
        "PUT",
        "/data-automation-libraries/",
        &json!({ "libraryName": name, "libraryDescription": "to be released" }),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {name:?}: expected CreateDataAutomationLibrary 201, got {status} with {payload:?}"
    );
    string_at(&payload, "libraryArn", name)
}

/// `GetDataAutomationLibrary`.
async fn get_library(router: &MultiServiceRouter, arn: &str) -> (u16, Value) {
    call(router, "POST", &library_uri(arn), &json!({})).await
}

/// One project's path, as `GetDataAutomationProject` (POST), `UpdateDataAutomationProject`
/// (PUT) and `DeleteDataAutomationProject` (DELETE) all bind it.
fn project_uri(arn: &str) -> String {
    format!("/data-automation-projects/{}/", encode_label(arn))
}

/// A project request body: the members `CreateDataAutomationProject` and
/// `UpdateDataAutomationProject` both require, plus the library references handed in. A body
/// for no reference at all carries no `dataAutomationLibraryConfiguration` member, which is
/// what a project that references nothing is created — and updated — with.
fn project_body(name: Option<&str>, libraries: &[String]) -> Value {
    let mut body = json!({ "standardOutputConfiguration": {} });
    if let Some(name) = name {
        body["projectName"] = json!(name);
    }
    if !libraries.is_empty() {
        body["dataAutomationLibraryConfiguration"] = json!({
            "libraries": libraries
                .iter()
                .map(|arn| json!({ "libraryArn": arn }))
                .collect::<Vec<Value>>(),
        });
    }
    body
}

/// `CreateDataAutomationProject` referencing `libraries`; returns the `projectArn` it minted.
async fn create_project(
    router: &MultiServiceRouter,
    name: &str,
    libraries: &[String],
) -> String {
    let (status, payload) = call(
        router,
        "PUT",
        "/data-automation-projects/",
        &project_body(Some(name), libraries),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {name:?}: expected CreateDataAutomationProject 201, got {status} with {payload:?}"
    );
    string_at(&payload, "projectArn", name)
}

/// `UpdateDataAutomationProject`, re-specifying the project's library references as
/// `libraries` — an update that names none leaves the project referencing nothing.
async fn update_project(
    router: &MultiServiceRouter,
    arn: &str,
    libraries: &[String],
    case: &str,
) {
    let (status, payload) = call(
        router,
        "PUT",
        &project_uri(arn),
        &project_body(None, libraries),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected UpdateDataAutomationProject 200, got {status} with {payload:?}"
    );
}

/// `DeleteDataAutomationProject`.
async fn delete_project(router: &MultiServiceRouter, arn: &str, case: &str) {
    let (status, payload) = call(router, "DELETE", &project_uri(arn), &json!({})).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected DeleteDataAutomationProject 200, got {status} with {payload:?}"
    );
}

/// Assert that a delete of `arn` was refused as the in-use ValidationException, and that the
/// refusal performed NOTHING: the library still reads ACTIVE and no removal is owed for it.
async fn assert_referenced_refusal(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    arn: &str,
    case: &str,
) {
    let (status, payload) = delete_library(router, arn).await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400, got {status} with {payload:?}"
    );
    assert_eq!(
        error_code(&payload),
        "ValidationException",
        "case {case:?}: expected a ValidationException, got {payload:?}"
    );
    assert_eq!(
        payload.get("message").and_then(Value::as_str),
        Some(state::library_deletion::LIBRARY_REFERENCED_MESSAGE),
        "case {case:?}: expected {:?}, got {payload:?}",
        state::library_deletion::LIBRARY_REFERENCED_MESSAGE
    );

    let (status, read) = get_library(router, arn).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the refused library must still read back, got {status} with {read:?}"
    );
    assert_eq!(
        read.get("library")
            .and_then(|library| library.get("status"))
            .and_then(Value::as_str),
        Some(state::LIBRARY_ACTIVE),
        "case {case:?}: expected the library left at {:?}, got {read:?}",
        state::LIBRARY_ACTIVE
    );

    let store = engine.state::<ServiceState>();
    let held = store.lock();
    let owed = held
        .libraries
        .get(arn)
        .unwrap_or_else(|| panic!("case {case:?}: the refused library must still be stored"))
        .deletion_due;
    assert!(
        owed.is_none(),
        "case {case:?}: a refused delete must owe no removal, got {owed:?}"
    );
}

/// Assert that a delete of `arn` was accepted and answers the modelled DELETING status.
async fn assert_delete_accepted(router: &MultiServiceRouter, arn: &str, case: &str) {
    let (status, payload) = delete_library(router, arn).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200, got {status} with {payload:?}"
    );
    assert_eq!(
        payload.get("libraryArn").and_then(Value::as_str),
        Some(arn),
        "case {case:?}: expected the ARN it was handed, got {payload:?}"
    );
    assert_eq!(
        payload.get("status").and_then(Value::as_str),
        Some(state::library_deletion::LIBRARY_DELETING),
        "case {case:?}: expected {:?}, got {payload:?}",
        state::library_deletion::LIBRARY_DELETING
    );
}

/// `DeleteDataAutomationLibrary`.
async fn delete_library(router: &MultiServiceRouter, arn: &str) -> (u16, Value) {
    call(router, "DELETE", &library_uri(arn), &json!({})).await
}

/// `ListDataAutomationLibraries`; returns the ARNs it enumerates.
async fn listed_libraries(router: &MultiServiceRouter, case: &str) -> Vec<String> {
    let (status, payload) = call(router, "POST", "/data-automation-libraries/", &json!({})).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected ListDataAutomationLibraries 200, got {status} with {payload:?}"
    );
    payload
        .get("libraries")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("case {case:?}: no libraries list in {payload:?}"))
        .iter()
        .map(|item| string_at(item, "libraryArn", case))
        .collect()
}

/// `InvokeDataAutomationLibraryIngestionJob`; returns the `jobArn` it minted.
async fn submit_job(router: &MultiServiceRouter, library_arn: &str, entity_id: &str) -> String {
    let body = json!({
        "entityType": "VOCABULARY",
        "operationType": "UPSERT",
        "inputConfiguration": {
            "inlinePayload": {
                "upsertEntitiesInfo": [{
                    "vocabulary": {
                        "entityId": entity_id,
                        "language": "EN",
                        "phrases": [{ "text": "mockingbird" }],
                    },
                }],
            },
        },
        "outputConfiguration": { "s3Uri": format!("s3://bda-ingestion/output/{entity_id}") },
    });
    let (status, payload) = call(router, "PUT", &jobs_uri(library_arn), &body).await;
    assert_eq!(
        status, 201,
        "case {entity_id:?}: expected InvokeDataAutomationLibraryIngestionJob 201, got \
         {status} with {payload:?}"
    );
    string_at(&payload, "jobArn", entity_id)
}

/// Look again until the library `arn` names resolves to nothing, and answer the body the
/// 404 carried. Panics once [`SETTLING_DEADLINE`] has passed without that having happened:
/// eventual settling is allowed, never settling is not.
async fn poll_until_released(router: &MultiServiceRouter, arn: &str, case: &str) -> Value {
    let deadline = Instant::now() + SETTLING_DEADLINE;
    let mut last = (0u16, Value::Null);
    while Instant::now() < deadline {
        let (status, payload) = get_library(router, arn).await;
        if status == 404 {
            return payload;
        }
        last = (status, payload);
        std::thread::sleep(LOOK_AGAIN_AFTER);
    }
    panic!(
        "case {case:?}: the accepted deletion never settled; the last read answered {} with \
         {:?}",
        last.0, last.1
    )
}

/// RULE: the delete answers the modelled DELETING status, and the removal it owes settles
/// into the modelled 404 with the service's fixed sentence — on the library's read and on
/// every other operation that resolves its ARN.
#[tokio::test]
async fn an_accepted_delete_settles_into_the_modeled_not_found() {
    let (router, engine) = emulator();
    let arn = create_library(&router, "released_library").await;

    let (status, deleted) = delete_library(&router, &arn).await;
    assert_eq!(
        status, 200,
        "the delete must be accepted, got {status} with {deleted:?}"
    );
    assert_eq!(
        deleted.get("libraryArn").and_then(Value::as_str),
        Some(arn.as_str()),
        "the delete must answer the ARN it was handed, got {deleted:?}"
    );
    assert_eq!(
        deleted.get("status").and_then(Value::as_str),
        Some(state::library_deletion::LIBRARY_DELETING),
        "the delete must answer the modelled {:?} status, got {deleted:?}",
        state::library_deletion::LIBRARY_DELETING
    );

    let refusal = poll_until_released(&router, &arn, "a released library").await;
    assert_eq!(
        error_code(&refusal),
        "ResourceNotFoundException",
        "case \"a released library\": expected a ResourceNotFoundException, got {refusal:?}"
    );
    assert_eq!(
        refusal.get("message").and_then(Value::as_str),
        Some(state::LIBRARY_ARN_NOT_FOUND_MESSAGE),
        "case \"a released library\": expected {:?}, got {refusal:?}",
        state::LIBRARY_ARN_NOT_FOUND_MESSAGE
    );

    // (case, the operation a released library is addressed through, its method, its URI,
    // its body) — every one of them answers the same refusal, so the library is released
    // for the whole surface and not only for its read.
    let rows = [
        (
            "the read again",
            "POST",
            library_uri(&arn),
            json!({}),
            "ResourceNotFoundException",
        ),
        (
            "the update",
            "PUT",
            library_uri(&arn),
            json!({ "libraryDescription": "after the release" }),
            "ResourceNotFoundException",
        ),
        (
            "the delete again",
            "DELETE",
            library_uri(&arn),
            json!({}),
            "ResourceNotFoundException",
        ),
        (
            "the ingestion-job listing",
            "POST",
            jobs_uri(&arn),
            json!({}),
            "ResourceNotFoundException",
        ),
    ];
    for (case, method, uri, body, expected) in rows {
        let (status, payload) = call(&router, method, &uri, &body).await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected HTTP 404, got {status} with {payload:?}"
        );
        assert_eq!(
            error_code(&payload),
            expected,
            "case {case:?}: expected a {expected}, got {payload:?}"
        );
        assert_eq!(
            payload.get("message").and_then(Value::as_str),
            Some(state::LIBRARY_ARN_NOT_FOUND_MESSAGE),
            "case {case:?}: expected {:?}, got {payload:?}",
            state::LIBRARY_ARN_NOT_FOUND_MESSAGE
        );
    }

    let store = engine.state::<ServiceState>();
    let held = store.lock();
    assert!(
        held.libraries.is_empty(),
        "case \"the store\": a released library must leave no record, store holds {:?}",
        held.libraries.keys().collect::<Vec<_>>()
    );
}

/// RULE: a release is of ONE library — the other libraries of the store keep answering and
/// keep being enumerated — and the released library's name is free for another create, which
/// mints a library of its own.
#[tokio::test]
async fn releasing_one_library_leaves_the_others_and_frees_its_name() {
    let (router, _engine) = emulator();
    let released = create_library(&router, "released_one").await;
    let kept = create_library(&router, "kept_one").await;

    let before = listed_libraries(&router, "before the delete").await;
    assert_eq!(
        before.len(),
        2,
        "case \"before the delete\": expected both libraries listed, got {before:?}"
    );

    let (status, _) = delete_library(&router, &released).await;
    assert_eq!(status, 200, "the delete must be accepted, got {status}");
    poll_until_released(&router, &released, "one of two libraries").await;

    let after = listed_libraries(&router, "after the delete").await;
    assert_eq!(
        after,
        vec![kept.clone()],
        "case \"after the delete\": expected only the kept library listed, got {after:?}"
    );
    let (status, read) = get_library(&router, &kept).await;
    assert_eq!(
        status, 200,
        "case \"the kept library\": expected HTTP 200, got {status} with {read:?}"
    );

    // The released name is free: a create under it succeeds and mints its OWN library.
    let recreated = create_library(&router, "released_one").await;
    assert_ne!(
        recreated, released,
        "case \"re-creating under the released name\": expected a library of its own, got the \
         released ARN back"
    );
    let (status, read) = get_library(&router, &recreated).await;
    assert_eq!(
        status, 200,
        "case \"the re-created library\": expected HTTP 200, got {status} with {read:?}"
    );
    let (status, gone) = get_library(&router, &released).await;
    assert_eq!(
        status, 404,
        "case \"the released library after the re-create\": expected HTTP 404, got {status} \
         with {gone:?}"
    );
}

/// RULE: a released library takes the ingestion jobs submitted against it with it — a read
/// of one is the library's own refusal, because the library the job belongs to is gone — and
/// leaves the jobs of every other library alone.
#[tokio::test]
async fn a_released_library_releases_the_jobs_submitted_against_it() {
    let (router, engine) = emulator();
    let released = create_library(&router, "released_with_jobs").await;
    let kept = create_library(&router, "kept_with_jobs").await;
    let released_job = submit_job(&router, &released, "released-entity").await;
    let kept_job = submit_job(&router, &kept, "kept-entity").await;

    let (status, _) = delete_library(&router, &released).await;
    assert_eq!(status, 200, "the delete must be accepted, got {status}");
    poll_until_released(&router, &released, "a library carrying jobs").await;

    let (status, payload) = call(
        &router,
        "POST",
        &format!("{}{}", jobs_uri(&released), encode_label(&released_job)),
        &json!({}),
    )
    .await;
    assert_eq!(
        status, 404,
        "case \"the released library's job\": expected HTTP 404, got {status} with {payload:?}"
    );
    assert_eq!(
        payload.get("message").and_then(Value::as_str),
        Some(state::LIBRARY_ARN_NOT_FOUND_MESSAGE),
        "case \"the released library's job\": expected {:?}, got {payload:?}",
        state::LIBRARY_ARN_NOT_FOUND_MESSAGE
    );

    let (status, read) = call(
        &router,
        "POST",
        &format!("{}{}", jobs_uri(&kept), encode_label(&kept_job)),
        &json!({}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"the kept library's job\": expected HTTP 200, got {status} with {read:?}"
    );

    let store = engine.state::<ServiceState>();
    let held = store.lock();
    let jobs: Vec<&String> = held.ingestion_jobs.keys().collect();
    assert_eq!(
        jobs,
        vec![&kept_job],
        "case \"the store's jobs\": expected only the kept library's job to remain, got \
         {jobs:?}"
    );
}

/// RULE: a delete of a `libraryArn` that names no library of this store is the modelled 404
/// with the service's fixed sentence, over a set of well-formed ARNs that name nothing.
#[tokio::test]
async fn deleting_a_library_that_is_not_there_is_the_modeled_not_found() {
    let (router, _engine) = emulator();

    // The rows span the resource-id space the modelled pattern admits (`[a-zA-Z0-9-]{12,36}`)
    // in the Region the request is signed for: an id of the form this service mints, one at
    // the 12-character lower bound, one at the 36-character upper bound, and one carrying
    // the pattern's hyphen.
    let unknown = [
        (
            "an id of the form this service mints",
            "arn:aws:bedrock:us-east-1:000000000000:data-automation-library/0000000a-00000000000000ff",
        ),
        (
            "an id at the 12-character lower bound",
            "arn:aws:bedrock:us-east-1:000000000000:data-automation-library/abcdefabcdef",
        ),
        (
            "an id at the 36-character upper bound",
            "arn:aws:bedrock:us-east-1:000000000000:data-automation-library/abcdefabcdefabcdefabcdefabcdefabcdef",
        ),
        (
            "an id carrying the pattern's hyphen",
            "arn:aws:bedrock:us-east-1:000000000000:data-automation-library/dead-beef-1234",
        ),
    ];
    for (case, arn) in unknown {
        let (status, payload) = delete_library(&router, arn).await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected HTTP 404, got {status} with {payload:?}"
        );
        assert_eq!(
            error_code(&payload),
            "ResourceNotFoundException",
            "case {case:?}: expected a ResourceNotFoundException, got {payload:?}"
        );
        assert_eq!(
            payload.get("message").and_then(Value::as_str),
            Some(state::LIBRARY_ARN_NOT_FOUND_MESSAGE),
            "case {case:?}: expected {:?}, got {payload:?}",
            state::LIBRARY_ARN_NOT_FOUND_MESSAGE
        );
    }
}

/// RULE: the delete is `@idempotent` — repeating it while the removal is still settling
/// answers the same ARN and the same DELETING status, and the library is released exactly
/// once.
#[tokio::test]
async fn repeating_the_delete_answers_the_same_acceptance() {
    let (router, engine) = emulator();
    let arn = create_library(&router, "idempotent_release").await;

    for attempt in 1..=3 {
        let (status, payload) = delete_library(&router, &arn).await;
        // The removal may already have settled under a slow host, in which case the
        // `@idempotent` delete of a library that is no longer there is its 404 — the same
        // answer every other operation gives for it. Either way the acceptance is the same
        // one, never a second deletion of a second library.
        if status == 404 {
            assert_eq!(
                payload.get("message").and_then(Value::as_str),
                Some(state::LIBRARY_ARN_NOT_FOUND_MESSAGE),
                "attempt #{attempt}: expected {:?}, got {payload:?}",
                state::LIBRARY_ARN_NOT_FOUND_MESSAGE
            );
            break;
        }
        assert_eq!(
            status, 200,
            "attempt #{attempt}: expected HTTP 200, got {status} with {payload:?}"
        );
        assert_eq!(
            payload.get("libraryArn").and_then(Value::as_str),
            Some(arn.as_str()),
            "attempt #{attempt}: expected the ARN it was handed, got {payload:?}"
        );
        assert_eq!(
            payload.get("status").and_then(Value::as_str),
            Some(state::library_deletion::LIBRARY_DELETING),
            "attempt #{attempt}: expected {:?}, got {payload:?}",
            state::library_deletion::LIBRARY_DELETING
        );
    }

    poll_until_released(&router, &arn, "a repeatedly deleted library").await;
    let store = engine.state::<ServiceState>();
    let held = store.lock();
    assert!(
        held.libraries.is_empty(),
        "case \"the store\": expected no record left, store holds {:?}",
        held.libraries.keys().collect::<Vec<_>>()
    );
}

/// RULE: a library a DataAutomationProject still names in its stored configuration is IN USE
/// — the delete is refused with the modelled `ValidationException` (HTTP 400) and the
/// service's fixed sentence, and performs nothing: the library still reads `ACTIVE` and owes
/// no removal. Once nothing refers to it any more the identical delete is accepted.
///
/// The rows differ along WHICH library each project of the store references and HOW MANY
/// projects reference the target: the target from one project, from two, beside a project
/// that references another library, another library only, no library at all, and no project
/// at all — so the refusal follows from "some project references it" rather than from one
/// configuration's shape. A project references at most ONE library
/// (`smithy_query(query="shape",shape="DataAutomationLibraryConfiguration")`: `libraries` is
/// `@length(min: 0, max: 1)`), which is why the rows vary the projects rather than the list.
#[tokio::test]
async fn deleting_a_library_a_project_references_is_refused() {
    // (case, how many libraries the store holds, the library indices each project references,
    //  whether anything still references the target — library #0)
    let rows: [(&str, usize, &[&[usize]], bool); 6] = [
        ("one project naming it", 1, &[&[0]], true),
        ("two projects both naming it", 1, &[&[0], &[0]], true),
        (
            "one project naming it beside one naming another library",
            2,
            &[&[0], &[1]],
            true,
        ),
        ("one project naming another library only", 2, &[&[1]], false),
        ("one project naming no library at all", 1, &[&[]], false),
        ("no project at all", 1, &[], false),
    ];
    for (case, libraries, references, referenced) in rows {
        let (router, engine) = emulator();
        let mut arns: Vec<String> = Vec::new();
        for index in 0..libraries {
            arns.push(create_library(&router, &format!("in_use_library_{index}")).await);
        }
        let mut projects: Vec<String> = Vec::new();
        for (index, named) in references.iter().enumerate() {
            let named_arns: Vec<String> = named.iter().map(|at| arns[*at].clone()).collect();
            projects
                .push(create_project(&router, &format!("in-use-project-{index}"), &named_arns).await);
        }

        let target = arns[0].clone();
        if referenced {
            assert_referenced_refusal(&router, &engine, &target, case).await;
            // The reference is let go of on the PROJECT side, and from that instant the
            // identical delete is accepted.
            for (index, project) in projects.iter().enumerate() {
                delete_project(&router, project, &format!("{case}: referrer #{index}")).await;
            }
        }
        assert_delete_accepted(&router, &target, case).await;
        // Every other library of the store is untouched by the delete of this one.
        for (index, other) in arns.iter().enumerate().skip(1) {
            let (status, read) = get_library(&router, other).await;
            assert_eq!(
                status, 200,
                "case {case:?}: library #{index} must be untouched, got {status} with {read:?}"
            );
        }
    }
}

/// RULE: the references that decide the refusal are the ones the project's stored
/// configuration carries RIGHT NOW, so an `UpdateDataAutomationProject` that acquires a
/// reference starts refusing the delete and one that lets go of it stops.
#[tokio::test]
async fn an_update_of_the_project_decides_whether_the_library_can_be_deleted() {
    // (case, the library the project references before the update, the one it references
    //  after it — by index, the target being library #0 — and whether the target is still
    //  referenced once the update has been applied)
    let rows: [(&str, &[usize], &[usize], bool); 4] = [
        ("the update acquires the reference", &[1], &[0], true),
        ("the update keeps the reference", &[0], &[0], true),
        (
            "the update hands the reference to another library",
            &[0],
            &[1],
            false,
        ),
        ("the update names no library at all", &[0], &[], false),
    ];
    for (case, before, after, referenced) in rows {
        let (router, engine) = emulator();
        let target = create_library(&router, "update_referenced_target").await;
        let other = create_library(&router, "update_referenced_other").await;
        let arns = [target.clone(), other.clone()];
        let referenced_before: Vec<String> = before.iter().map(|at| arns[*at].clone()).collect();
        let project = create_project(&router, "update-referrer", &referenced_before).await;
        if before.contains(&0) {
            assert_referenced_refusal(&router, &engine, &target, &format!("{case}: before it"))
                .await;
        }

        let referenced_after: Vec<String> = after.iter().map(|at| arns[*at].clone()).collect();
        update_project(&router, &project, &referenced_after, case).await;

        if referenced {
            assert_referenced_refusal(&router, &engine, &target, case).await;
        } else {
            assert_delete_accepted(&router, &target, case).await;
        }
    }
}
