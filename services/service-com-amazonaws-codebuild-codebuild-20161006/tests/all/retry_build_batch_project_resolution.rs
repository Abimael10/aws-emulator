// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the ORDER in which `RetryBuildBatch` resolves the resources a
//! request addresses, driven over the wire the router serves (`POST /` with
//! `X-Amz-Target: CodeBuild_20161006.<Operation>` on awsJson1_1) through the harness in
//! `project_build_lifecycle`.
//!
//! RULE under test: a retry addressed by a per-project resource id (`<projectName>:<uuid>`)
//! resolves the PROJECT that id names before the record the id addresses, because what a
//! retry runs again is that project's work — so with the project absent the refusal reports
//! the project by its ARN, whether or not a record is held under the id, and the batch half
//! of the id is never named. A value that carries no uuid half names no project, so such a
//! request is answered on the record it addresses; a project that IS held reports the batch
//! build as the absent resource.
//!
//! Every refusal is read back against the store: what a refused call must not change is
//! asserted through the implemented readers.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::project_build_lifecycle::{
    batch_enabled, call, create_project, emulator, error_type, expected_arn, listed_batches,
    merged, settled_build_batch, stored_build_batch,
};

/// A well-formed uuid half for a batch build id this service never minted: every id it
/// mints derives its uuid from the project, the sequence number and the start instant, so
/// a uuid built from `nth` alone is never one of them.
fn synthetic_uuid(nth: usize) -> String {
    format!("00000000-0000-8000-8000-{nth:012}")
}

/// A `CreateProject` body fragment for a batch-enabled project whose inline buildspec
/// declares one build group running `command` — `false` fails that group's build, which is
/// what makes the batch a FAILED one a retry may restart.
fn batch_project_running(command: &str) -> Value {
    let buildspec = format!(
        "version: 0.2\nbatch:\n  build-list:\n    - identifier: only\n      \
         buildspec: |\n        version: 0.2\n        phases:\n          build:\n            \
         commands:\n              - {command}\nphases:\n  build:\n    commands:\n      \
         - echo base\n"
    );
    merged(
        json!({"source": {"type": "NO_SOURCE", "buildspec": buildspec}}),
        batch_enabled(),
    )
}

/// Start one batch build of `project` and hand back its id.
async fn start_build_batch(router: &MultiServiceRouter, project: &str) -> String {
    let (status, body) = call(router, "StartBuildBatch", json!({"projectName": project})).await;
    assert_eq!(
        status, 200,
        "StartBuildBatch {project:?}: expected 200, got {status} with {body}"
    );
    body["buildBatch"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("StartBuildBatch {project:?} must report an id, got {body}"))
        .to_string()
}

/// Delete `project`, asserting it succeeded — the batch builds it started survive it,
/// which is what makes the ordering rows below decidable.
async fn delete_project(router: &MultiServiceRouter, project: &str) {
    let (status, body) = call(router, "DeleteProject", json!({"name": project})).await;
    assert_eq!(
        status, 200,
        "DeleteProject {project:?}: expected 200, got {status} with {body}"
    );
}

/// How the project a batch build id names came to be absent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Absence {
    /// No project of that name was ever created, so no batch build was ever started under
    /// it either: the id is well formed and denotes nothing at all.
    NeverCreated,
    /// The project was created, a batch build of it started, and the project then deleted:
    /// the BATCH record is still held under the id, so only the order the preconditions
    /// are resolved in decides which resource the refusal names.
    DeletedUnderItsBatch,
}

/// The RULE: `RetryBuildBatch` resolves the project named by the batch build id's project
/// half before the batch registry, so a retry whose project is absent is refused on the
/// PROJECT — named by its ARN — and nothing is restarted.
///
/// The table spans how the project came to be absent (never created, deleted while its
/// batch build stayed), the project-name forms the model's `ProjectName` pattern allows
/// (the two-character minimum, a long name, and names carrying `-` and `_`) and the hex
/// case of the id's uuid half.
#[tokio::test]
async fn a_batch_retry_whose_project_is_absent_is_refused_on_the_project() {
    // (project name, how it is absent, the uuid half to request instead of a derived one)
    // — the names span the pattern `^[A-Za-z0-9][A-Za-z0-9\-_]{1,149}$` on
    // `CreateProjectInput$name` (smithy_query(query="shape",shape="ProjectName")): the
    // shortest allowed name, a long one, and one of each permitted punctuation.
    let cases: &[(&str, Absence, Option<&str>)] = &[
        ("ab", Absence::NeverCreated, None),
        ("retry-batch-gone-project", Absence::NeverCreated, None),
        ("retry_batch_gone_project", Absence::NeverCreated, None),
        (
            "r0123456789012345678901234567890123456789012345678901234567890123456789",
            Absence::NeverCreated,
            None,
        ),
        // The uuid half's hex digits are case-insensitive, so an id carrying upper-case
        // ones names its project just as one carrying lower-case digits does.
        (
            "retry-batch-upper-case-uuid",
            Absence::NeverCreated,
            Some("ABCDEF00-0000-8000-8000-0000000000AB"),
        ),
        ("ab", Absence::DeletedUnderItsBatch, None),
        ("retry-batch-deleted", Absence::DeletedUnderItsBatch, None),
        ("retry_batch_deleted", Absence::DeletedUnderItsBatch, None),
    ];

    for (nth, (project, absence, uuid)) in cases.iter().enumerate() {
        let case = format!("{project:?} {absence:?} {uuid:?}");
        // A fresh emulator per row: a row's state must not depend on the one before.
        let router = emulator();
        // A live project with a batch build of its own, so every row also proves the
        // refusal is about the project the id names and not about the account holding none.
        create_project(&router, "retry-batch-live", batch_project_running("echo ok")).await;
        let live_batch = start_build_batch(&router, "retry-batch-live").await;

        let requested = match absence {
            Absence::NeverCreated => {
                let uuid = uuid.map_or_else(|| synthetic_uuid(nth + 1), str::to_string);
                format!("{project}:{uuid}")
            }
            Absence::DeletedUnderItsBatch => {
                create_project(&router, project, batch_project_running("false")).await;
                let orphan = start_build_batch(&router, project).await;
                delete_project(&router, project).await;
                orphan
            }
        };

        let (status, body) = call(&router, "RetryBuildBatch", json!({"id": requested})).await;
        assert_eq!(
            status, 400,
            "case {case}: expected 400, got {status} with {body}"
        );
        assert_eq!(
            error_type(&body),
            "ResourceNotFoundException",
            "case {case}: expected ResourceNotFoundException, got {body}"
        );
        // Built here from the region the call is signed for and the emulator's account, so
        // the assertion pins the ARN the message must carry rather than echoing whatever
        // the handler produced.
        assert_eq!(
            body["message"],
            json!(format!(
                "Project cannot be found: {}",
                expected_arn(&format!("project/{project}"))
            )),
            "case {case}: the refusal must name the absent PROJECT by its ARN, got {body}"
        );
        let stated = body["message"].as_str().unwrap_or_default();
        let uuid_half = requested.split_once(':').map(|(_, uuid)| uuid);
        assert!(
            !stated.contains("build-batch")
                && uuid_half.is_none_or(|uuid| !stated.contains(uuid)),
            "case {case}: the refusal must not point the caller at a batch build, got {body}"
        );

        // Nothing was restarted: the live project still reports the one batch build it
        // started, read back through the store the handlers write.
        assert_eq!(
            listed_batches(&router, "retry-batch-live").await,
            vec![live_batch.clone()],
            "case {case}: a refused retry must restart nothing"
        );
    }
}

/// The boundary of the same rule: a value that carries no uuid half names no project, and
/// a project that IS held has nothing absent but the batch build — so both are answered on
/// the BATCH, by the identifier the request gave exactly as given.
///
/// The table spans the forms such a value takes: a well-formed id of a held project
/// denoting no batch build, a bare project name, a second half that is not a uuid, one a
/// hex digit short of one, and the batch build's ARN form.
#[tokio::test]
async fn an_id_whose_project_is_held_is_answered_on_the_batch_build() {
    let router = emulator();
    create_project(&router, "retry-batch-held", batch_project_running("false")).await;
    let held = start_build_batch(&router, "retry-batch-held").await;

    let cases: &[(&str, String)] = &[
        (
            "a well-formed id of a held project denoting no batch build",
            format!("retry-batch-held:{}", synthetic_uuid(1)),
        ),
        ("a bare project name", "retry-batch-held".to_string()),
        (
            "a second half that is not a uuid",
            "retry-batch-held:not-a-uuid".to_string(),
        ),
        (
            "a second half one hex digit short of a uuid",
            "retry-batch-held:00000000-0000-8000-8000-00000000000".to_string(),
        ),
        (
            "the batch build's ARN form",
            expected_arn(&format!("build-batch/{held}")),
        ),
    ];

    for (case, requested) in cases {
        let (status, body) = call(&router, "RetryBuildBatch", json!({"id": requested})).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected 400, got {status} with {body}"
        );
        assert_eq!(
            error_type(&body),
            "ResourceNotFoundException",
            "case {case:?}: expected ResourceNotFoundException, got {body}"
        );
        assert_eq!(
            body["message"],
            json!(format!("BuildBatch cannot be found: {requested}")),
            "case {case:?}: the refusal must name the batch build the id addresses, got {body}"
        );
    }

    // The batch build the account holds is untouched, and nothing was started beside it.
    assert_eq!(
        listed_batches(&router, "retry-batch-held").await,
        vec![held],
        "a refused retry must restart nothing"
    );
}

/// The control the two tables above are decided against: resolving the project first does
/// not stand between a held project and a retry of its FAILED batch build, which still
/// restarts that batch in place.
#[tokio::test]
async fn a_failed_batch_of_a_held_project_still_retries() {
    let router = emulator();
    create_project(&router, "retry-batch-control", batch_project_running("false")).await;
    let id = start_build_batch(&router, "retry-batch-control").await;

    // The transition the clock owes the batch is waited for through the shared harness,
    // which looks again until the batch reports itself complete.
    let settled = settled_build_batch(&router, &id).await;
    assert_eq!(
        settled["buildBatchStatus"],
        json!("FAILED"),
        "the control row needs a FAILED batch build, got {settled}"
    );

    let (status, body) = call(&router, "RetryBuildBatch", json!({"id": id.clone()})).await;
    assert_eq!(
        status, 200,
        "RetryBuildBatch of a failed batch of a held project: expected 200, got {status} \
         with {body}"
    );
    assert_eq!(
        body["buildBatch"]["id"],
        json!(id),
        "the retry restarts the batch in place, got {body}"
    );
    assert_eq!(
        body["buildBatch"]["buildBatchStatus"],
        json!("IN_PROGRESS"),
        "the restarted batch build is running again, got {body}"
    );
    // Read back through the implemented reader: the restart is what the store holds.
    let read = stored_build_batch(&router, &id).await;
    assert_eq!(
        read["buildBatchStatus"],
        json!("IN_PROGRESS"),
        "the store reports the restarted batch build, got {read}"
    );
    assert_eq!(
        listed_batches(&router, "retry-batch-control").await,
        vec![id],
        "a retry restarts the batch in place rather than minting a second one"
    );
}
