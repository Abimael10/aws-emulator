// SPDX-License-Identifier: CC0-1.0

//! Cover for where the batch build options a `StartBuildBatch` runs under COME FROM,
//! driven over the wire the router serves: `POST /` with
//! `X-Amz-Target: CodeBuild_20161006.StartBuildBatch` on awsJson1_1.
//!
//! RULE: a batch build runs under batch build options, and EITHER source states them —
//! the project's own `buildBatchConfig` ("the object that defines the batch build options
//! for the project") or the `buildBatchConfigOverride` the start call carries, which states
//! them for this one batch the way every other `*Override` member does: it stands in place
//! of the stored object rather than merging member by member, so a member it leaves unstated
//! resolves to the value the service documents for an unstated one. A start whose options
//! either source states is accepted and mints a batch build that reports the configuration it
//! RUNS WITH; only a start where NEITHER source states options is refused, with the
//! operation's modelled `InvalidInputException` (client, HTTP 400).
//!
//! The suite that owns the refusal's wording and the timeout window is
//! `start_build_batch_validation`; this one is about what a started batch CARRIES, read
//! back through the implemented readers:
//!
//! * the batch build the start minted is what `BatchGetBuildBatches` reports and what
//!   `ListBuildBatchesForProject` lists;
//! * starting a batch is not an update of the project: `BatchGetProjects` reports the same
//!   `buildBatchConfig` afterwards as before — a project that stores none still stores
//!   none, however the batch it was started for got its options;
//! * the environment variables the start overrode are the ones the batch keeps, for every
//!   list form and count the service accepts;
//! * an idempotency token replay of such a start resolves to the batch it started rather
//!   than being judged again against the project alone.

use serde_json::{json, Value};
use service_com_amazonaws_codebuild_codebuild_20161006::state;

use crate::project_build_lifecycle::{
    call, create_project, emulator, error_type, listed_batches, stored_build_batch, stored_project,
};
// The refusal's wording is owned by the suite that covers the refusal itself, so it is
// stated once for the whole crate.
use crate::start_build_batch_validation::NOT_BATCH_ENABLED_SENTENCE;

/// A batch service role ARN, for the rows whose configuration states one. Its value is
/// immaterial to the rule — what matters is that the member is stated.
const BATCH_SERVICE_ROLE: &str = "arn:aws:iam::000000000000:role/service-role/batch";

/// What a row expects of the `timeoutInMins` the started batch runs under.
#[derive(Debug, Clone, Copy)]
enum Timeout {
    /// The configuration the batch runs under states this many minutes, so the batch runs
    /// under exactly them.
    Stated(i32),
    /// The configuration the batch runs under states no timeout, so the batch runs under
    /// whatever an unstated member resolves to. The model states no default for
    /// `ProjectBuildBatchConfig$timeoutInMins`, so the value itself is not what these rows
    /// pin: they pin that it is the SAME however the project is configured, and that it is
    /// never `not` — the timeout the project states, which the override stands in place of.
    Unstated { not: Option<i32> },
    /// Neither source states options, so there is no batch to run and the start is refused.
    Refused,
}

/// The environment variables a batch build record carries.
fn environment_variables(record: &Value) -> &Value {
    &record["environment"]["environmentVariables"]
}

/// RULE: a batch build runs under the options EITHER source states, so a start carrying a
/// `buildBatchConfigOverride` is accepted for a project that stores no `buildBatchConfig`,
/// and the batch reports the configuration it runs with — the override standing in place of
/// the stored object, not merged into it; only a start where neither source states options is
/// refused.
///
/// The table spans which source states the options — the project alone, the request alone,
/// both (where the request's own configuration is the one the batch runs under), a request
/// configuration stating no timeout over a project that states one (whose resolved timeout is
/// therefore not the project's, which is replacement told apart from a member-by-member
/// merge), the same over a project that stores nothing (which must resolve to the same
/// timeout as the row before it), and neither source at all.
#[tokio::test]
async fn a_batch_runs_under_the_options_whichever_source_states_them() {
    // (case, the project's CreateProject members, the request's buildBatchConfigOverride,
    //  what the started batch's timeoutInMins must be)
    let cases: Vec<(&str, Value, Value, Timeout)> = vec![
        (
            "the project alone states the options",
            json!({"buildBatchConfig": {"serviceRole": BATCH_SERVICE_ROLE, "timeoutInMins": 60}}),
            Value::Null,
            Timeout::Stated(60),
        ),
        (
            "the request alone states the options",
            json!({}),
            json!({"serviceRole": BATCH_SERVICE_ROLE, "timeoutInMins": 30}),
            Timeout::Stated(30),
        ),
        (
            "both state options, and the request's are the ones the batch runs under",
            json!({"buildBatchConfig": {"serviceRole": BATCH_SERVICE_ROLE, "timeoutInMins": 60}}),
            json!({"serviceRole": BATCH_SERVICE_ROLE, "timeoutInMins": 480}),
            Timeout::Stated(480),
        ),
        (
            "an override stating no timeout stands in place of the project's configuration",
            json!({"buildBatchConfig": {"serviceRole": BATCH_SERVICE_ROLE, "timeoutInMins": 60}}),
            json!({"serviceRole": BATCH_SERVICE_ROLE}),
            Timeout::Unstated { not: Some(60) },
        ),
        (
            "the request states options carrying no timeout",
            json!({}),
            json!({"serviceRole": BATCH_SERVICE_ROLE}),
            Timeout::Unstated { not: None },
        ),
        (
            "neither source states options",
            json!({}),
            Value::Null,
            Timeout::Refused,
        ),
    ];

    // What the rows whose configuration states no timeout resolved to, so they can be held to
    // one another rather than to a value written here.
    let mut unstated: Vec<(&str, Value)> = Vec::new();

    for (index, (case, project_members, config_override, expected_timeout)) in
        cases.into_iter().enumerate()
    {
        // A fresh emulator per row: a row's state must not depend on the one before.
        let router = emulator();
        let project = format!("batch-options-{index}");
        let created = create_project(&router, &project, project_members).await;

        let mut request = json!({"projectName": project.clone()});
        if !config_override.is_null() {
            request["buildBatchConfigOverride"] = config_override;
        }
        let (status, body) = call(&router, "StartBuildBatch", request.clone()).await;

        match expected_timeout {
            Timeout::Stated(_) | Timeout::Unstated { .. } => {
                assert_eq!(
                    status, 200,
                    "case {case:?}: a batch whose options either source states must be \
                     startable, got {status} with {body}"
                );
                let batch = body["buildBatch"].clone();
                let ran_under = batch["buildBatchConfig"]["timeoutInMins"].clone();
                match expected_timeout {
                    Timeout::Stated(minutes) => assert_eq!(
                        ran_under,
                        json!(minutes),
                        "case {case:?}: expected the batch to run under {minutes} minutes, got \
                         {batch}"
                    ),
                    Timeout::Unstated { not } => {
                        if let Some(stated_by_the_project) = not {
                            assert_ne!(
                                ran_under,
                                json!(stated_by_the_project),
                                "case {case:?}: the configuration the batch runs under states no \
                                 timeout, so the project's {stated_by_the_project} must not show \
                                 through it, got {batch}"
                            );
                        }
                        assert!(
                            ran_under.is_i64(),
                            "case {case:?}: a batch runs under a resolved timeout even when its \
                             configuration states none, got {batch}"
                        );
                        unstated.push((case, ran_under.clone()));
                    }
                    Timeout::Refused => unreachable!("refused rows take the other arm"),
                }
                assert_eq!(
                    batch["buildBatchStatus"],
                    json!("IN_PROGRESS"),
                    "case {case:?}: the started batch must be in flight, got {batch}"
                );
                let id = batch["id"]
                    .as_str()
                    .unwrap_or_else(|| panic!("case {case:?}: the started batch must carry an id, got {batch}"))
                    .to_string();

                // The batch the start minted is the one the readers report.
                let fetched = stored_build_batch(&router, &id).await;
                assert_eq!(
                    fetched["buildBatchConfig"], batch["buildBatchConfig"],
                    "case {case:?}: BatchGetBuildBatches must report the configuration the \
                     batch was started with, got {fetched}"
                );
                assert_eq!(
                    listed_batches(&router, &project).await,
                    vec![id.clone()],
                    "case {case:?}: the started batch must be the one listed for the project"
                );
            }
            Timeout::Refused => {
                assert_eq!(
                    status, 400,
                    "case {case:?}: a batch no source states options for must be refused, got \
                     {status} with {body}"
                );
                assert_eq!(
                    error_type(&body),
                    "InvalidInputException",
                    "case {case:?}: expected InvalidInputException, got {body}"
                );
                assert_eq!(
                    body["message"],
                    json!(NOT_BATCH_ENABLED_SENTENCE),
                    "case {case:?}: expected CodeBuild's not-batch-enabled sentence, got {body}"
                );
                assert!(
                    listed_batches(&router, &project).await.is_empty(),
                    "case {case:?}: a refused start must mint no batch build"
                );
            }
        }

        // Starting a batch is not an update of the project: whatever the batch got its
        // options from, the project stores exactly what it was created with.
        let stored = stored_project(&router, &project).await;
        assert_eq!(
            stored["buildBatchConfig"], created["buildBatchConfig"],
            "case {case:?}: StartBuildBatch must leave the project's own buildBatchConfig \
             alone, got {stored}"
        );
        assert_eq!(
            stored, created,
            "case {case:?}: StartBuildBatch must leave the stored project exactly as it was"
        );
    }

    // Every row whose configuration states no timeout runs under the same resolved one,
    // however the project it started from was configured: that is the override standing in
    // place of the stored object rather than merging into it.
    if let Some(((first_case, first), rest)) = unstated.split_first() {
        for (case, ran_under) in rest {
            assert_eq!(
                ran_under, first,
                "case {case:?}: a configuration stating no timeout must resolve the same one as \
                 case {first_case:?} did ({first}), got {ran_under}"
            );
        }
    }
}

/// `count` environment variables, cycling the three modelled
/// `EnvironmentVariableType` values (smithy_query shape=EnvironmentVariableType:
/// PLAINTEXT | PARAMETER_STORE | SECRETS_MANAGER) so the list form varies with its size.
fn variables(count: usize) -> Value {
    // The modelled value set, read from the model rather than written from memory.
    const TYPES: [&str; 3] = ["PLAINTEXT", "PARAMETER_STORE", "SECRETS_MANAGER"];
    Value::Array(
        (0..count)
            .map(|index| {
                json!({
                    "name": format!("VAR_{index}"),
                    "value": format!("value-{index}"),
                    "type": TYPES[index % TYPES.len()],
                })
            })
            .collect(),
    )
}

/// RULE: the `environmentVariablesOverride` a batch start carries states the environment
/// the batch runs with, and the batch KEEPS it — the model puts no `@length` on
/// `EnvironmentVariables`, so every count the service accepts is reported back in full.
///
/// Driven on the shape this rule is reached by for a project that stores no batch options
/// of its own: the request states both the batch configuration and the variables. The rows
/// differ in the size of the override list — none, one, a few, and the largest count the
/// service accepts.
#[tokio::test]
async fn a_batch_keeps_the_environment_variables_it_was_started_with() {
    // (case, how many variables the start overrides)
    let counts: [(&str, usize); 4] = [
        ("no variables at all", 0),
        ("a single variable", 1),
        ("a few variables", 3),
        (
            "the largest count the service accepts",
            state::MAX_ENVIRONMENT_VARIABLES,
        ),
    ];

    for (index, (case, count)) in counts.into_iter().enumerate() {
        let router = emulator();
        let project = format!("batch-env-source-{index}");
        // No buildBatchConfig: the start below is what states the batch options.
        create_project(&router, &project, json!({})).await;

        let overrides = variables(count);
        let (status, body) = call(
            &router,
            "StartBuildBatch",
            json!({
                "projectName": project.clone(),
                "buildBatchConfigOverride": {"timeoutInMins": 60},
                "environmentVariablesOverride": overrides.clone(),
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: {count} overridden variable(s) is a count the service accepts, \
             got {status} with {body}"
        );
        let batch = body["buildBatch"].clone();
        // The project defines none of its own, so the resolved list is the override as
        // listed; `environment_variable_overrides_are_merged_by_name_for_a_batch_build`
        // owns the merge against a project that does define some.
        assert_eq!(
            environment_variables(&batch),
            &overrides,
            "case {case:?}: the started batch must carry the variables it was started with"
        );

        let id = batch["id"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: expected a batch id, got {batch}"))
            .to_string();
        let fetched = stored_build_batch(&router, &id).await;
        assert_eq!(
            environment_variables(&fetched),
            &overrides,
            "case {case:?}: BatchGetBuildBatches must report the variables the batch was \
             started with"
        );
    }
}

/// RULE: an idempotency token in force resolves to the batch build the first request with
/// it started — including a start whose batch options the REQUEST stated, which is judged
/// the same way on the replay as it was on the first call.
#[tokio::test]
async fn a_token_replay_of_a_request_stated_batch_resolves_to_the_same_batch() {
    let router = emulator();
    let project = "batch-options-token";
    // No buildBatchConfig: the options come from the request alone, on both calls.
    create_project(&router, project, json!({})).await;

    let request = json!({
        "projectName": project,
        "idempotencyToken": "token-1",
        "buildBatchConfigOverride": {"serviceRole": BATCH_SERVICE_ROLE, "timeoutInMins": 45},
        "environmentVariablesOverride": [{"name": "K", "value": "v", "type": "PLAINTEXT"}],
    });

    let (status, first) = call(&router, "StartBuildBatch", request.clone()).await;
    assert_eq!(
        status, 200,
        "the first start: expected 200, got {status} with {first}"
    );
    let (status, replay) = call(&router, "StartBuildBatch", request).await;
    assert_eq!(
        status, 200,
        "the token replay: expected 200, got {status} with {replay}"
    );
    assert_eq!(
        replay["buildBatch"]["id"], first["buildBatch"]["id"],
        "a replayed token must resolve to the batch it started, got {replay}"
    );
    assert_eq!(
        listed_batches(&router, project).await.len(),
        1,
        "a replayed token must mint no second batch build"
    );
}
