// SPDX-License-Identifier: CC0-1.0

//! Cover for what a batch build started through the request's own configuration KEEPS,
//! driven over the wire the router serves: `POST /` with
//! `X-Amz-Target: CodeBuild_20161006.StartBuildBatch` on awsJson1_1.
//!
//! Where the batch build options come from — either the project's stored `buildBatchConfig`
//! or the `buildBatchConfigOverride` the start call carries, which stands in place of it for
//! this one batch — is the rule `build_batch_configuration_source` owns, and the refusal's
//! wording is `start_build_batch_validation`'s. This suite starts from the case those leave
//! open: a project that stores NO batch configuration, batch built through the request's
//! override alone, and reads what that batch then carries.
//!
//! Two rules, both about such a batch:
//!
//! 1. `environmentVariablesOverride` members "override, or add to, the environment variables
//!    defined in the batch build project", and the batch reports each as GIVEN — value byte
//!    for byte, whatever the value's bytes are. The value targets the unconstrained `String`,
//!    so nothing in it is the service's to alter: `build_batch_configuration_source` owns the
//!    rule itself (and the forms of list and the types a variable may carry); what is read
//!    here is the one axis it does not vary, the bytes of a value.
//! 2. Such a batch is a batch build like any other, so everything downstream of the start is
//!    reachable for it: `ListBuildBatchesForProject` lists it, `StopBuildBatch` ends it and
//!    `DeleteBuildBatch` removes it, after which `BatchGetBuildBatches` reports it as not
//!    found by its ARN.
//!
//! Each row is read back through the implemented readers, so what is asserted is what the
//! start PERSISTED rather than what its answer happened to echo.

use serde_json::{json, Value};
use service_com_amazonaws_codebuild_codebuild_20161006::state;

use crate::project_build_lifecycle::{
    call, create_project, emulator, listed_batches, stored_build_batch, REGION,
};

/// A buildspec whose `batch` section declares one `build-list` entry — the shape of spec a
/// batch build is started with.
fn batch_buildspec() -> String {
    String::from(
        "version: 0.2\n\
         batch:\n  \
           fast-fail: false\n  \
           build-list:\n    \
             - identifier: build1\n\
         phases:\n  \
           build:\n    \
             commands:\n      \
               - echo building\n",
    )
}

/// `CreateProject` members for a project that stores NO `buildBatchConfig` and whose inline
/// buildspec declares a batch.
fn project_without_batch_config() -> Value {
    json!({"source": {"type": "NO_SOURCE", "buildspec": batch_buildspec()}})
}

/// Start a batch of `project` through the request's own configuration, with `extra` merged
/// into the request, and hand back the batch the answer carries.
///
/// The override states no member: what makes the batch buildable is that the REQUEST states
/// the batch build options at all, so the suite that owns the configuration rule keeps the
/// rows about what such an object may state.
async fn start_through_the_override(
    router: &smithy_server_runtime::MultiServiceRouter,
    project: &str,
    extra: Value,
) -> Value {
    let mut request = json!({
        "projectName": project,
        "buildspecOverride": batch_buildspec(),
        "buildBatchConfigOverride": {},
    });
    if let (Value::Object(target), Value::Object(members)) = (&mut request, extra) {
        target.extend(members);
    }
    let (status, body) = call(router, "StartBuildBatch", request.clone()).await;
    assert_eq!(
        status, 200,
        "StartBuildBatch {request}: expected 200, got {status} with {body}"
    );
    body["buildBatch"].clone()
}

/// The id a started batch reports.
fn batch_id(batch: &Value) -> String {
    batch["id"]
        .as_str()
        .unwrap_or_else(|| panic!("a started batch must report an id, got {batch}"))
        .to_string()
}

/// RULE 1: the environment variables a start overrode are the ones the batch keeps, value
/// byte for byte.
///
/// The rows differ along the ONE axis the suite that owns this rule does not vary: the bytes
/// of a value. Which forms of list, which modelled types and how many variables a request
/// may carry are that suite's rows.
#[tokio::test]
async fn a_batch_started_through_an_override_keeps_every_variable_value_byte_for_byte() {
    let long_value = "v".repeat(512);
    // (case, the variable the request overrides with)
    let rows: Vec<(&str, Value)> = vec![
        (
            "an empty value",
            json!({"name": "EMPTY_VAR", "value": "", "type": "PLAINTEXT"}),
        ),
        (
            "a plain ASCII value",
            json!({"name": "PLAIN_VAR", "value": "plain-value", "type": "PLAINTEXT"}),
        ),
        (
            "a unicode value carrying an accent, CJK and an astral emoji",
            json!({"name": "UNICODE_VAR", "value": "héllo-世界-🚀", "type": "PLAINTEXT"}),
        ),
        (
            "a value of whitespace and newlines",
            json!({"name": "SPACE_VAR", "value": " a\tb\nc \r\n", "type": "PLAINTEXT"}),
        ),
        (
            "a long value",
            json!({"name": "LONG_VAR", "value": long_value, "type": "PLAINTEXT"}),
        ),
        (
            "a value that looks like JSON",
            json!({"name": "JSON_VAR", "value": "{\"k\": [1, 2]}", "type": "PLAINTEXT"}),
        ),
    ];

    let router = emulator();
    create_project(&router, "batch-env-override", project_without_batch_config()).await;

    for (case, variable) in rows {
        let overrides = json!([variable]);
        let batch = start_through_the_override(
            &router,
            "batch-env-override",
            json!({"environmentVariablesOverride": overrides.clone()}),
        )
        .await;
        assert_eq!(
            batch["environment"]["environmentVariables"], overrides,
            "case {case:?}: expected the started batch to carry {overrides}, got {batch}"
        );

        let fetched = stored_build_batch(&router, &batch_id(&batch)).await;
        assert_eq!(
            fetched["environment"]["environmentVariables"], overrides,
            "case {case:?}: BatchGetBuildBatches must report {overrides} byte for byte, got \
             {fetched}"
        );
    }
}

/// RULE 2: a batch the request's own configuration enabled is listed for its project, ends
/// when stopped and is gone once deleted — the surface a refused start made unreachable.
#[tokio::test]
async fn a_batch_started_through_an_override_is_listed_stoppable_and_deletable() {
    let router = emulator();
    create_project(
        &router,
        "batch-override-lifecycle",
        project_without_batch_config(),
    )
    .await;

    let batch = start_through_the_override(&router, "batch-override-lifecycle", json!({})).await;
    let id = batch_id(&batch);
    assert_eq!(
        batch["buildBatchStatus"],
        json!("IN_PROGRESS"),
        "the started batch must be in flight, got {batch}"
    );
    assert_eq!(
        listed_batches(&router, "batch-override-lifecycle").await,
        vec![id.clone()],
        "the started batch must be listed for its project"
    );

    let (status, stopped) = call(&router, "StopBuildBatch", json!({"id": id.clone()})).await;
    assert_eq!(
        status, 200,
        "StopBuildBatch: expected 200, got {status} with {stopped}"
    );
    assert_eq!(
        stopped["buildBatch"]["buildBatchStatus"],
        json!("STOPPED"),
        "a stopped batch reports STOPPED, got {stopped}"
    );
    let read = stored_build_batch(&router, &id).await;
    assert_eq!(
        read["buildBatchStatus"],
        json!("STOPPED"),
        "the stored batch reads as STOPPED, got {read}"
    );

    let (status, deleted) = call(&router, "DeleteBuildBatch", json!({"id": id.clone()})).await;
    assert_eq!(
        status, 200,
        "DeleteBuildBatch: expected 200, got {status} with {deleted}"
    );
    let (status, after) = call(&router, "BatchGetBuildBatches", json!({"ids": [id.clone()]})).await;
    assert_eq!(
        status, 200,
        "BatchGetBuildBatches after the delete: expected 200, got {status} with {after}"
    );
    assert_eq!(
        after["buildBatchesNotFound"],
        json!([state::build_batch_arn(REGION, &id)]),
        "the deleted batch reads as not found by its ARN, got {after}"
    );
}
