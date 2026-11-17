// SPDX-License-Identifier: CC0-1.0

//! `StartNetworkMigrationAnalysis`, end to end over HTTP.
//!
//! The rule under test: a job of the network-migration pipeline runs for ONE execution
//! of ONE network migration definition of the caller's account, and that execution runs
//! one job at a time — a start resolves the definition and then the execution, refusing
//! an id the account holds no record of with 404 `ResourceNotFoundException` and
//! recording nothing; refusing with 409 `ConflictException` while another stage's job of
//! that execution is still in flight; replaying the job it already started while that
//! job runs (the operation is `@idempotent` and models no idempotency token, so the
//! request's own members are all a retry can be recognised by); and otherwise recording
//! a job for the stage it starts, moving the execution onto that stage and answering the
//! job's minted `jobID` with 202.
//!
//! Every assertion goes through the router's `dispatch`, so it exercises the same
//! restJson1 deserialize → handle → serialize path a client drives, and the started
//! analysis run is read back through `ListNetworkMigrationExecutions` — the implemented
//! reader of the execution the run belongs to (`ListNetworkMigrationAnalyses`, the
//! reader of the job itself, is still a scaffold stub). The store is reached directly
//! only to seed a precondition no sequence of calls can reach (another account's
//! records, a job whose work the clock has already finished), the way the other
//! network-migration suites do.
//!
//! The harness (`Mgn`, `error_code`, the region and account constants) is the one
//! `application_lifecycle.rs` defines; the definition/execution helpers are the ones
//! `network_migration_mapping_start.rs` defines, since the two starts drive the same
//! pipeline.

use std::time::{Duration, SystemTime};

use serde_json::{json, Value};
use service_com_amazonaws_mgn_applicationmigrationservice::ops::types::{
    TargetNetwork, TargetS3Configuration,
};
use service_com_amazonaws_mgn_applicationmigrationservice::state::{
    NetworkMigrationDefinition, NetworkMigrationDefinitionConfiguration, NetworkMigrationExecution,
};

use crate::application_lifecycle::{error_code, Mgn, EMULATOR_ACCOUNT, OTHER_ACCOUNT, REGION};
use crate::network_migration_mapping_start::{
    create_definition, execution_id_of, executions, is_uuid_shaped, job_id_of, staged_execution,
    start_mapping, uuid_shaped, EXECUTION_STAGE_MAPPING, EXECUTION_STATUS_STARTED,
    JOB_STATUS_STARTED, JOB_STATUS_SUCCEEDED, STRATEGIES,
};

/// The modelled `http.uri` of `StartNetworkMigrationAnalysis`
/// (`smithy_query(query="operation",operation="StartNetworkMigrationAnalysis")`),
/// without the leading slash the harness adds.
const START_ANALYSIS: &str = "network-migration/StartNetworkMigrationAnalysis";

/// The `ExecutionStage` an analysis run belongs to. Source:
/// `smithy_query(query="shape",shape="ExecutionStage")` → `values` (`MAPPING`,
/// `CODE_GENERATION`, `DEPLOY`, `DEPLOYED_STACKS_DELETION`, `ANALYZE`).
const EXECUTION_STAGE_ANALYZE: &str = "ANALYZE";

/// The `ExecutionStageActivity` of an analysis run. Source:
/// `smithy_query(query="shape",shape="ExecutionStageActivity")` → `values`.
const EXECUTION_ACTIVITY_ANALYZE: &str = "ANALYZE";

/// The `ExecutionStatus` a definition's staged execution waits in. Source:
/// `smithy_query(query="shape",shape="ExecutionStatus")` → `values`.
const EXECUTION_STATUS_PENDING: &str = "PENDING";

/// The one sentence the service answers for a start refused because the execution is
/// already running a job: fixed text, with no part taken from the request.
pub(crate) const JOB_IN_PROGRESS_MESSAGE: &str =
    "A job cannot be started because another job for this execution is currently in progress.";

/// `StartNetworkMigrationAnalysis` for `definition_id`/`execution_id`. The operation
/// models exactly these two members, both required.
async fn start_analysis(mgn: &Mgn, definition_id: &str, execution_id: &str) -> (u16, Value) {
    mgn.call(
        START_ANALYSIS,
        &json!({
            "networkMigrationDefinitionID": definition_id,
            "networkMigrationExecutionID": execution_id,
        }),
    )
    .await
}

/// Move the clock past the instant a job's work is due to have finished, so nothing is
/// in flight for its execution any more. Rewinding the stored instant is the only way
/// there: no call can reach it, and waiting for it would make the test's outcome depend
/// on how fast the host runs.
fn finish_mapping_job(mgn: &Mgn, job_id: &str) {
    let store = mgn.store();
    let mut guard = store.lock();
    let job = guard
        .network_migration_mapping_jobs
        .get_mut(job_id)
        .expect("the started mapping job is in the store");
    job.completes_at = SystemTime::now() - Duration::from_secs(1);
}

/// Likewise for an analysis job.
fn finish_analysis_job(mgn: &Mgn, job_id: &str) {
    let store = mgn.store();
    let mut guard = store.lock();
    let job = guard
        .network_migration_analysis_jobs
        .get_mut(job_id)
        .expect("the started analysis job is in the store");
    job.completes_at = SystemTime::now() - Duration::from_secs(1);
}

/// How the execution a start acts on got into a state with no job in flight: the class
/// the accepting rule spans, of which the recorded "freshly staged execution" is one row.
#[derive(Clone, Copy, Debug)]
enum Prepared {
    /// The execution a created definition is staged with, which nothing has run for.
    FreshlyStaged,
    /// An execution whose mapping run the clock has already finished.
    AfterAFinishedMappingJob,
    /// An execution whose own analysis run the clock has already finished.
    AfterAFinishedAnalysisJob,
    /// A second execution of the definition, seeded because nothing modelled creates
    /// one, so the first execution's staged record is not the only shape of input.
    SeededSecondExecution,
}

/// Every way a start can arrive at an execution with nothing in flight.
fn prepared_cases() -> [(&'static str, Prepared); 4] {
    [
        ("a freshly staged execution", Prepared::FreshlyStaged),
        (
            "an execution whose mapping job has finished",
            Prepared::AfterAFinishedMappingJob,
        ),
        (
            "an execution whose analysis job has finished",
            Prepared::AfterAFinishedAnalysisJob,
        ),
        (
            "a seeded second execution of the definition",
            Prepared::SeededSecondExecution,
        ),
    ]
}

/// Drive `mgn` until `case` holds, answering the definition and the execution a start is
/// then made for.
async fn prepare(mgn: &Mgn, case: &str, name: &str, prepared: Prepared) -> (String, String) {
    let definition_id = create_definition(mgn, case, name).await;
    let staged = execution_id_of(case, &staged_execution(mgn, case, &definition_id).await);
    match prepared {
        Prepared::FreshlyStaged => (definition_id, staged),
        Prepared::AfterAFinishedMappingJob => {
            let (status, body) =
                start_mapping(mgn, &definition_id, &staged, Some(STRATEGIES[0])).await;
            assert_eq!(
                status, 202,
                "case {case:?}: the mapping start expected 202, got {status} with {body:?}"
            );
            finish_mapping_job(mgn, &job_id_of(case, &body));
            (definition_id, staged)
        }
        Prepared::AfterAFinishedAnalysisJob => {
            let (status, body) = start_analysis(mgn, &definition_id, &staged).await;
            assert_eq!(
                status, 202,
                "case {case:?}: the first analysis start expected 202, got {status} with {body:?}"
            );
            finish_analysis_job(mgn, &job_id_of(case, &body));
            (definition_id, staged)
        }
        Prepared::SeededSecondExecution => {
            let second = uuid_shaped(0xa11a);
            let stored = mgn.store().lock().insert_network_migration_execution(
                NetworkMigrationExecution {
                    network_migration_execution_id: second.clone(),
                    network_migration_definition_id: definition_id.clone(),
                    account_id: EMULATOR_ACCOUNT.to_string(),
                    status: Some(EXECUTION_STATUS_PENDING.to_string()),
                    stage: Some(EXECUTION_STAGE_MAPPING.to_string()),
                    created_at: Some(SystemTime::now()),
                    updated_at: Some(SystemTime::now()),
                    ..NetworkMigrationExecution::default()
                },
            );
            assert!(stored, "case {case:?}: seeding a second execution must store it");
            (definition_id, second)
        }
    }
}

/// Every start that finds nothing in flight mints a job, persists it under the execution
/// it runs, and moves that execution onto the analysis stage — and the same request
/// again, while that job is running, answers the job it started rather than a second one.
#[tokio::test]
async fn an_analysis_start_records_a_job_and_moves_its_execution_onto_the_analysis_stage() {
    // Neither operation models an `accountID` member or an
    // `UninitializedAccountException`, so no `InitializeService` runs here.
    let mgn = Mgn::new();
    let mut minted = Vec::new();

    for (index, (case, prepared)) in prepared_cases().into_iter().enumerate() {
        let (definition_id, execution_id) =
            prepare(&mgn, case, &format!("analysis-plan-{index}"), prepared).await;

        let (status, started) = start_analysis(&mgn, &definition_id, &execution_id).await;
        assert_eq!(
            status, 202,
            "case {case:?}: StartNetworkMigrationAnalysis expected 202, got {status} with {started:?}"
        );
        let job_id = job_id_of(case, &started);
        assert!(
            is_uuid_shaped(&job_id),
            "case {case:?}: jobID must match ^[0-9a-f]{{8}}-[0-9a-f]{{4}}-[0-9a-f]{{4}}-[0-9a-f]{{4}}-[0-9a-f]{{12}}$, got {job_id:?}"
        );

        // The job is state, not a value computed for the answer: it is stored under the
        // execution it runs, and its status follows from the stored instants.
        {
            let store = mgn.store();
            let guard = store.lock();
            let jobs = guard.network_migration_analysis_jobs_of(
                EMULATOR_ACCOUNT,
                &definition_id,
                &execution_id,
            );
            let job = jobs
                .last()
                .unwrap_or_else(|| panic!("case {case:?}: no analysis job recorded for the execution"));
            assert_eq!(
                job.job_id, job_id,
                "case {case:?}: the stored job must be the one the answer named, got {:?}",
                job.job_id
            );
            assert_eq!(
                job.network_migration_execution_id, execution_id,
                "case {case:?}: the job must be stored under the execution it runs, got {:?}",
                job.network_migration_execution_id
            );
            // The work the job runs is owed at a later instant, so a caller reading the
            // job straight after this call still sees it running.
            assert!(
                job.completes_at > job.created_at,
                "case {case:?}: the analysis work must take a non-zero span, got {:?} to {:?}",
                job.created_at,
                job.completes_at
            );
            assert_eq!(
                job.status(job.created_at),
                JOB_STATUS_STARTED,
                "case {case:?}: a running job's status must be STARTED, got {:?}",
                job.status(job.created_at)
            );
            assert_eq!(
                job.ended_at(job.created_at),
                None,
                "case {case:?}: a running job has not ended, got {:?}",
                job.ended_at(job.created_at)
            );
            assert_eq!(
                job.status(job.completes_at),
                JOB_STATUS_SUCCEEDED,
                "case {case:?}: a finished job's status must be SUCCEEDED, got {:?}",
                job.status(job.completes_at)
            );
            assert_eq!(
                job.ended_at(job.completes_at),
                Some(job.completes_at),
                "case {case:?}: a finished job ended when its work was due, got {:?}",
                job.ended_at(job.completes_at)
            );
        }

        // Round trip: the execution the run belongs to now reports the stage and the
        // activity this call started, through the implemented reader.
        let listed: Vec<Value> = executions(&mgn, case, &definition_id)
            .await
            .into_iter()
            .filter(|execution| {
                execution["networkMigrationExecutionID"].as_str() == Some(execution_id.as_str())
            })
            .collect();
        assert_eq!(
            listed.len(),
            1,
            "case {case:?}: the listing must report the execution the run belongs to, got {listed:?}"
        );
        let reported = listed[0].clone();
        for (member, expected) in [
            ("status", EXECUTION_STATUS_STARTED),
            ("stage", EXECUTION_STAGE_ANALYZE),
            ("activity", EXECUTION_ACTIVITY_ANALYZE),
        ] {
            assert_eq!(
                reported[member].as_str(),
                Some(expected),
                "case {case:?}: {member} must be {expected:?} once the analysis run started, got {:?}",
                reported[member]
            );
        }

        // `@idempotent` with no token: the same request again, while that job runs, is
        // the retry of the call that started it.
        let recorded = mgn.store().lock().network_migration_analysis_jobs.len();
        let (status, retried) = start_analysis(&mgn, &definition_id, &execution_id).await;
        assert_eq!(
            status, 202,
            "case {case:?}: the retry expected 202, got {status} with {retried:?}"
        );
        assert_eq!(
            job_id_of(case, &retried),
            job_id,
            "case {case:?}: a retry must answer the job the first call started, got {retried:?}"
        );
        assert_eq!(
            mgn.store().lock().network_migration_analysis_jobs.len(),
            recorded,
            "case {case:?}: a retry must not record a second job"
        );

        minted.push(job_id);
    }

    let mut unique = minted.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(
        unique.len(),
        minted.len(),
        "every start mints a distinct jobID, got {minted:?}"
    );
}

/// One row of the refusal table: which resource the request names, and what the service
/// must answer for it.
struct RefusalCase {
    case: &'static str,
    definition_id: String,
    execution_id: String,
    resource_type: &'static str,
    resource_id: String,
}

/// A start that names a definition or an execution the account holds no record of is
/// refused with 404 `ResourceNotFoundException`, naming the kind and the id that was
/// missing, and records nothing.
#[tokio::test]
async fn an_analysis_start_naming_no_recorded_definition_or_execution_is_refused() {
    let mgn = Mgn::new();

    // Two definitions of this account, each staged its own execution, so the table can
    // name an execution that exists but belongs to another definition.
    let first = create_definition(&mgn, "first definition", "analysis-plan-a").await;
    let second = create_definition(&mgn, "second definition", "analysis-plan-b").await;
    let first_execution = execution_id_of(
        "first definition",
        &staged_execution(&mgn, "first definition", &first).await,
    );
    let second_execution = execution_id_of(
        "second definition",
        &staged_execution(&mgn, "second definition", &second).await,
    );

    // A definition and an execution of ANOTHER account: a precondition no sequence of
    // this account's calls can reach.
    let foreign_definition = "nmd-aaaaaaaaaaaaaaaaa";
    let foreign_execution = uuid_shaped(0x5eed);
    {
        let store = mgn.store();
        let mut guard = store.lock();
        let stored = guard.insert_network_migration_definition(NetworkMigrationDefinition {
            arn: format!(
                "arn:aws:mgn:{REGION}:{OTHER_ACCOUNT}:network-migration-definition/{foreign_definition}"
            ),
            network_migration_definition_id: foreign_definition.to_string(),
            account_id: OTHER_ACCOUNT.to_string(),
            created_at: SystemTime::now(),
            updated_at: SystemTime::now(),
            configuration: NetworkMigrationDefinitionConfiguration {
                name: "other-account-plan".to_string(),
                target_s3_configuration: TargetS3Configuration {
                    s3_bucket: "migration-artifacts".to_string(),
                    s3_bucket_owner: OTHER_ACCOUNT.to_string(),
                },
                target_network: TargetNetwork {
                    topology: "ISOLATED_VPC".to_string(),
                    ..TargetNetwork::default()
                },
                ..NetworkMigrationDefinitionConfiguration::default()
            },
        });
        assert!(stored, "seeding another account's definition must store it");
        let stored = guard.insert_network_migration_execution(NetworkMigrationExecution {
            network_migration_execution_id: foreign_execution.clone(),
            network_migration_definition_id: foreign_definition.to_string(),
            account_id: OTHER_ACCOUNT.to_string(),
            created_at: Some(SystemTime::now()),
            updated_at: Some(SystemTime::now()),
            ..NetworkMigrationExecution::default()
        });
        assert!(stored, "seeding another account's execution must store it");
    }

    let unknown_definition = "nmd-zzzzzzzzzzzzzzzzz";
    let unknown_execution = uuid_shaped(0xfeed);
    let cases = vec![
        RefusalCase {
            case: "a definition no account holds",
            definition_id: unknown_definition.to_string(),
            execution_id: first_execution.clone(),
            resource_type: "NetworkMigrationDefinition",
            resource_id: unknown_definition.to_string(),
        },
        RefusalCase {
            case: "a definition of another account",
            definition_id: foreign_definition.to_string(),
            execution_id: foreign_execution.clone(),
            resource_type: "NetworkMigrationDefinition",
            resource_id: foreign_definition.to_string(),
        },
        RefusalCase {
            case: "an execution no definition holds",
            definition_id: first.clone(),
            execution_id: unknown_execution.clone(),
            resource_type: "NetworkMigrationExecution",
            resource_id: unknown_execution.clone(),
        },
        RefusalCase {
            case: "an execution of another definition of this account",
            definition_id: first.clone(),
            execution_id: second_execution.clone(),
            resource_type: "NetworkMigrationExecution",
            resource_id: second_execution.clone(),
        },
        RefusalCase {
            case: "an execution of another account under this account's definition",
            definition_id: second.clone(),
            execution_id: foreign_execution.clone(),
            resource_type: "NetworkMigrationExecution",
            resource_id: foreign_execution.clone(),
        },
    ];

    for refusal in cases {
        let case = refusal.case;
        let (status, body) =
            start_analysis(&mgn, &refusal.definition_id, &refusal.execution_id).await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected 404, got {status} with {body:?}"
        );
        assert_eq!(
            error_code(&body),
            "ResourceNotFoundException",
            "case {case:?}: expected ResourceNotFoundException, got {body:?}"
        );
        assert_eq!(
            body["resourceType"].as_str(),
            Some(refusal.resource_type),
            "case {case:?}: resourceType must name the missing kind, got {:?}",
            body["resourceType"]
        );
        assert_eq!(
            body["resourceId"].as_str(),
            Some(refusal.resource_id.as_str()),
            "case {case:?}: resourceId must name the id the request asked for, got {:?}",
            body["resourceId"]
        );
        assert_eq!(
            body["message"].as_str(),
            Some(
                format!(
                    "{} with Id: '{}' was not found",
                    refusal.resource_type, refusal.resource_id
                )
                .as_str()
            ),
            "case {case:?}: message must name the kind and the id, got {:?}",
            body["message"]
        );
    }

    // A refusal starts nothing: no job is recorded, and every execution still waits
    // where its definition's create staged it.
    assert!(
        mgn.store()
            .lock()
            .network_migration_analysis_jobs
            .is_empty(),
        "a refused start must record no analysis job"
    );
    for (case, definition_id) in [("first definition", &first), ("second definition", &second)] {
        let execution = staged_execution(&mgn, case, definition_id).await;
        assert_eq!(
            execution["status"].as_str(),
            Some(EXECUTION_STATUS_PENDING),
            "case {case:?}: a refused start must leave the staged execution PENDING, got {execution:?}"
        );
    }
}

/// One execution runs one job at a time: whichever stage's job is in flight, a start of
/// the other stage's work on the same execution is refused with 409 `ConflictException`
/// and records nothing, and once the clock has finished that job's work the start is
/// accepted.
#[tokio::test]
async fn a_start_conflicts_while_another_job_of_the_execution_is_in_flight() {
    // Both directions of the rule: the recorded case (a mapping job in flight refuses an
    // analysis start) and its mirror (an analysis job in flight refuses a mapping start).
    for (case, analysis_blocks) in [
        ("a mapping job blocks the analysis start", false),
        ("an analysis job blocks the mapping start", true),
    ] {
        let mgn = Mgn::new();
        let definition_id = create_definition(&mgn, case, "analysis-plan-conflict").await;
        let execution_id =
            execution_id_of(case, &staged_execution(&mgn, case, &definition_id).await);

        // A second execution of the same definition: the conflict is about the execution
        // the job runs for, not about the definition, so this one stays startable.
        let other_execution = uuid_shaped(0xb0b);
        let stored = mgn.store().lock().insert_network_migration_execution(
            NetworkMigrationExecution {
                network_migration_execution_id: other_execution.clone(),
                network_migration_definition_id: definition_id.clone(),
                account_id: EMULATOR_ACCOUNT.to_string(),
                status: Some(EXECUTION_STATUS_PENDING.to_string()),
                stage: Some(EXECUTION_STAGE_MAPPING.to_string()),
                created_at: Some(SystemTime::now()),
                updated_at: Some(SystemTime::now()),
                ..NetworkMigrationExecution::default()
            },
        );
        assert!(stored, "case {case:?}: seeding a second execution must store it");

        let (status, blocking) = if analysis_blocks {
            start_analysis(&mgn, &definition_id, &execution_id).await
        } else {
            start_mapping(&mgn, &definition_id, &execution_id, Some(STRATEGIES[1])).await
        };
        assert_eq!(
            status, 202,
            "case {case:?}: the blocking start expected 202, got {status} with {blocking:?}"
        );
        let blocking_job = job_id_of(case, &blocking);

        let (status, body) = if analysis_blocks {
            start_mapping(&mgn, &definition_id, &execution_id, Some(STRATEGIES[2])).await
        } else {
            start_analysis(&mgn, &definition_id, &execution_id).await
        };
        assert_eq!(
            status, 409,
            "case {case:?}: expected 409 while the other job runs, got {status} with {body:?}"
        );
        assert_eq!(
            error_code(&body),
            "ConflictException",
            "case {case:?}: expected ConflictException, got {body:?}"
        );
        assert_eq!(
            body["message"].as_str(),
            Some(JOB_IN_PROGRESS_MESSAGE),
            "case {case:?}: message must be the service's job-in-progress wording, got {:?}",
            body["message"]
        );
        assert_eq!(
            body["resourceId"].as_str(),
            Some(execution_id.as_str()),
            "case {case:?}: resourceId must name the execution the job runs for, got {:?}",
            body["resourceId"]
        );
        // The refusal is distinguishable from the unknown-id refusal: an id the account
        // holds no record of is 404, this one is 409.
        let (status, unknown) =
            start_analysis(&mgn, &definition_id, &uuid_shaped(0xdead)).await;
        assert_eq!(
            status, 404,
            "case {case:?}: an unregistered execution id expected 404, got {status} with {unknown:?}"
        );
        assert_eq!(
            error_code(&unknown),
            "ResourceNotFoundException",
            "case {case:?}: an unregistered execution id expected ResourceNotFoundException, got {unknown:?}"
        );

        // A refused start records nothing, and the refused execution id is not left
        // behind anywhere a later read would report it.
        {
            let store = mgn.store();
            let guard = store.lock();
            let analyses = guard.network_migration_analysis_jobs.len();
            let mappings = guard.network_migration_mapping_jobs.len();
            assert_eq!(
                analyses + mappings,
                1,
                "case {case:?}: only the blocking job may be recorded, got {analyses} analysis and {mappings} mapping jobs"
            );
        }
        let listed = executions(&mgn, case, &definition_id).await;
        let ids: Vec<&str> = listed
            .iter()
            .filter_map(|execution| execution["networkMigrationExecutionID"].as_str())
            .collect();
        assert!(
            !ids.contains(&uuid_shaped(0xdead).as_str()),
            "case {case:?}: a refused id must not appear among the executions, got {ids:?}"
        );

        // The OTHER execution of the same definition is not blocked by that job.
        let (status, other) = start_analysis(&mgn, &definition_id, &other_execution).await;
        assert_eq!(
            status, 202,
            "case {case:?}: another execution's analysis start expected 202, got {status} with {other:?}"
        );

        // The clock finishes the blocking job's work, and the start that conflicted with
        // it is accepted.
        if analysis_blocks {
            finish_analysis_job(&mgn, &blocking_job);
        } else {
            finish_mapping_job(&mgn, &blocking_job);
        }
        let (status, accepted) = if analysis_blocks {
            start_mapping(&mgn, &definition_id, &execution_id, Some(STRATEGIES[2])).await
        } else {
            start_analysis(&mgn, &definition_id, &execution_id).await
        };
        assert_eq!(
            status, 202,
            "case {case:?}: a start after the blocking job finished expected 202, got {status} with {accepted:?}"
        );
        assert_ne!(
            job_id_of(case, &accepted),
            blocking_job,
            "case {case:?}: that start must record a new job, got {accepted:?}"
        );
    }
}
