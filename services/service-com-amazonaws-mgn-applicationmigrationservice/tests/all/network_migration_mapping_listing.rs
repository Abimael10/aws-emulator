// SPDX-License-Identifier: CC0-1.0

//! `ListNetworkMigrationMappings`, end to end over HTTP.
//!
//! The rule under test: the listing reports the mapping jobs STORED for one execution of
//! one network migration definition of the caller's account, in the order they were
//! started, each with the status its stored instants give it — a definition or an
//! execution the account holds no record of is refused with 404
//! `ResourceNotFoundException` rather than answered with an empty page, `filters.jobIDs`
//! narrows the selection to the ids it names, `maxResults`/`nextToken` page it without
//! repeating or dropping a job, a token this service never issued is refused with 400
//! `ValidationException`, and reading changes nothing a later caller sees.
//!
//! The harness (`Mgn`, `error_code`, the account constants) is the one
//! `application_lifecycle.rs` defines; the definition/execution helpers are the ones
//! `network_migration_mapping_start.rs` defines.

use std::time::{Duration, SystemTime};

use serde_json::{json, Value};
use service_com_amazonaws_mgn_applicationmigrationservice::state::NetworkMigrationExecution;

use crate::application_lifecycle::{error_code, Mgn, EMULATOR_ACCOUNT};
use crate::network_migration_mapping_start::{
    create_definition, execution_id_of, is_uuid_shaped, job_id_of, staged_execution, start_mapping,
    uuid_shaped, EXECUTION_STAGE_MAPPING, JOB_STATUS_STARTED, JOB_STATUS_SUCCEEDED, STRATEGIES,
};

/// The modelled `http.uri` of `ListNetworkMigrationMappings`
/// (`smithy_query(query="operation",operation="ListNetworkMigrationMappings")`), without
/// the leading slash the harness adds.
const LIST_MAPPINGS: &str = "network-migration/ListNetworkMigrationMappings";

/// `ListNetworkMigrationMappings` with whatever members `request` carries beyond the two
/// required ids.
async fn list_mappings(
    mgn: &Mgn,
    definition_id: &str,
    execution_id: &str,
    extra: Value,
) -> (u16, Value) {
    let mut request = json!({
        "networkMigrationDefinitionID": definition_id,
        "networkMigrationExecutionID": execution_id,
    });
    if let Some(members) = extra.as_object() {
        for (member, value) in members {
            request[member] = value.clone();
        }
    }
    mgn.call(LIST_MAPPINGS, &request).await
}

/// The `items` of a listing that must succeed.
async fn items(mgn: &Mgn, case: &str, definition_id: &str, execution_id: &str, extra: Value) -> Vec<Value> {
    let (status, body) = list_mappings(mgn, definition_id, execution_id, extra).await;
    assert_eq!(
        status, 200,
        "case {case:?}: ListNetworkMigrationMappings expected 200, got {status} with {body:?}"
    );
    body["items"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: no items array in {body:?}"))
        .clone()
}

/// Move the clock past the instant a mapping job's work is due to have finished, so the
/// next start records another job. Rewinding the stored instant is the only way there:
/// waiting for it would make the test's outcome depend on how fast the host runs.
fn finish_mapping_job(mgn: &Mgn, job_id: &str) {
    let store = mgn.store();
    let mut guard = store.lock();
    let job = guard
        .network_migration_mapping_jobs
        .get_mut(job_id)
        .expect("the started mapping job is in the store");
    job.completes_at = SystemTime::now() - Duration::from_secs(1);
}

/// Start `count` mapping runs of one execution, finishing each but the last, and answer
/// their `jobID`s in the order they were started.
async fn started_jobs(mgn: &Mgn, case: &str, definition_id: &str, execution_id: &str, count: usize) -> Vec<String> {
    let mut jobs: Vec<String> = Vec::new();
    for index in 0..count {
        if let Some(previous) = jobs.last() {
            finish_mapping_job(mgn, previous);
        }
        let strategy = STRATEGIES[index % STRATEGIES.len()];
        let (status, body) = start_mapping(mgn, definition_id, execution_id, Some(strategy)).await;
        assert_eq!(
            status, 202,
            "case {case:?}: mapping start {index} expected 202, got {status} with {body:?}"
        );
        jobs.push(job_id_of(case, &body));
    }
    jobs
}

fn job_id_at(case: &str, item: &Value) -> String {
    item["jobID"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no jobID in {item:?}"))
        .to_string()
}

/// Every job the starts recorded is reported, in start order, with the members the model
/// declares and the status its stored instants give it.
#[tokio::test]
async fn the_listing_reports_the_jobs_recorded_for_the_execution() {
    let mgn = Mgn::new();
    let case = "three jobs of one execution";
    let definition_id = create_definition(&mgn, case, "mapping-listing-plan").await;
    let execution_id = execution_id_of(case, &staged_execution(&mgn, case, &definition_id).await);
    let jobs = started_jobs(&mgn, case, &definition_id, &execution_id, 3).await;

    let listed = items(&mgn, case, &definition_id, &execution_id, json!({})).await;
    assert_eq!(
        listed.iter().map(|item| job_id_at(case, item)).collect::<Vec<_>>(),
        jobs,
        "case {case:?}: the jobs must be listed in the order they were started, got {listed:?}"
    );

    for (index, item) in listed.iter().enumerate() {
        let job_id = job_id_at(case, item);
        assert!(
            is_uuid_shaped(&job_id),
            "case {case:?}: jobID must match the modelled NetworkMigrationJobID pattern, got {job_id:?}"
        );
        assert_eq!(
            item["networkMigrationExecutionID"].as_str(),
            Some(execution_id.as_str()),
            "case {case:?}: the item must name the execution the job ran for, got {item:?}"
        );
        assert_eq!(
            item["networkMigrationDefinitionID"].as_str(),
            Some(definition_id.as_str()),
            "case {case:?}: the item must name the definition the execution belongs to, got {item:?}"
        );
        assert!(
            item["createdAt"].as_f64().is_some(),
            "case {case:?}: createdAt must be an epoch-seconds number, got {item:?}"
        );
        // The earlier jobs' work is done; only the last one started is still running, so
        // the status and `endedAt` follow from the stored instants rather than from the
        // order of the reads.
        let last = index + 1 == listed.len();
        assert_eq!(
            item["status"].as_str(),
            Some(if last { JOB_STATUS_STARTED } else { JOB_STATUS_SUCCEEDED }),
            "case {case:?} job {index}: status must follow the job's instants, got {item:?}"
        );
        assert_eq!(
            item["endedAt"].as_f64().is_some(),
            !last,
            "case {case:?} job {index}: endedAt is reported exactly for a job whose work is done, got {item:?}"
        );
    }

    // A read changes nothing a later caller sees.
    let again = items(&mgn, case, &definition_id, &execution_id, json!({})).await;
    assert_eq!(
        again, listed,
        "case {case:?}: a second listing must answer the same page, got {again:?}"
    );

    // The jobs of ANOTHER execution of the same definition are not this execution's.
    let other = uuid_shaped(0xc0de);
    let stored = mgn
        .store()
        .lock()
        .insert_network_migration_execution(NetworkMigrationExecution {
            network_migration_execution_id: other.clone(),
            network_migration_definition_id: definition_id.clone(),
            account_id: EMULATOR_ACCOUNT.to_string(),
            status: Some("PENDING".to_string()),
            stage: Some(EXECUTION_STAGE_MAPPING.to_string()),
            created_at: Some(SystemTime::now()),
            updated_at: Some(SystemTime::now()),
            ..NetworkMigrationExecution::default()
        });
    assert!(stored, "seeding a second execution must store it");
    assert!(
        items(&mgn, "a second execution", &definition_id, &other, json!({}))
            .await
            .is_empty(),
        "an execution nothing ran for must be listed no jobs"
    );
}

/// `filters.jobIDs` narrows the selection to the ids it names; an absent filter and a
/// present-but-empty list select everything.
#[tokio::test]
async fn the_listing_honours_the_modelled_job_id_filter() {
    let mgn = Mgn::new();
    let case = "filters";
    let definition_id = create_definition(&mgn, case, "mapping-filter-plan").await;
    let execution_id = execution_id_of(case, &staged_execution(&mgn, case, &definition_id).await);
    let jobs = started_jobs(&mgn, case, &definition_id, &execution_id, 3).await;

    // Source of the filter member: `smithy_query(query="shape",
    // shape="ListNetworkMigrationMappingsFilters")` → `jobIDs`.
    let cases: Vec<(String, Value, Vec<String>)> = vec![
        ("no filters".to_string(), json!({}), jobs.clone()),
        (
            "an empty filter structure".to_string(),
            json!({"filters": {}}),
            jobs.clone(),
        ),
        (
            "an empty jobIDs list".to_string(),
            json!({"filters": {"jobIDs": []}}),
            jobs.clone(),
        ),
        (
            "one job named".to_string(),
            json!({"filters": {"jobIDs": [jobs[1]]}}),
            vec![jobs[1].clone()],
        ),
        (
            "two jobs named, in the other order".to_string(),
            json!({"filters": {"jobIDs": [jobs[2], jobs[0]]}}),
            vec![jobs[0].clone(), jobs[2].clone()],
        ),
        (
            "a job of no execution".to_string(),
            json!({"filters": {"jobIDs": [uuid_shaped(0xfeed)]}}),
            Vec::new(),
        ),
    ];

    for (case, extra, expected) in cases {
        let case = case.as_str();
        let listed = items(&mgn, case, &definition_id, &execution_id, extra).await;
        assert_eq!(
            listed.iter().map(|item| job_id_at(case, item)).collect::<Vec<_>>(),
            expected,
            "case {case:?}: the filter must select exactly the named jobs, in start order, got {listed:?}"
        );
    }
}

/// Following `nextToken` sees every job exactly once, whatever page size the caller asks
/// for, and a token this service never issued is refused.
#[tokio::test]
async fn the_listing_pages_without_repeating_or_dropping_jobs() {
    let mgn = Mgn::new();
    let case = "paging";
    let definition_id = create_definition(&mgn, case, "mapping-paging-plan").await;
    let execution_id = execution_id_of(case, &staged_execution(&mgn, case, &definition_id).await);
    let jobs = started_jobs(&mgn, case, &definition_id, &execution_id, 4).await;

    for page_size in 1..=5usize {
        let case = format!("maxResults {page_size}");
        let case = case.as_str();
        let mut seen: Vec<String> = Vec::new();
        let mut token: Option<String> = None;
        // One request per page plus one: a page size that covers the selection answers
        // no token at all, so the loop cannot run away.
        for _ in 0..=jobs.len() {
            let mut extra = json!({"maxResults": page_size});
            if let Some(token) = token.as_deref() {
                extra["nextToken"] = json!(token);
            }
            let (status, body) =
                list_mappings(&mgn, &definition_id, &execution_id, extra).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected 200, got {status} with {body:?}"
            );
            let page = body["items"]
                .as_array()
                .unwrap_or_else(|| panic!("case {case:?}: no items array in {body:?}"))
                .clone();
            assert!(
                page.len() <= page_size,
                "case {case:?}: a page must hold at most maxResults jobs, got {page:?}"
            );
            seen.extend(page.iter().map(|item| job_id_at(case, item)));
            token = body["nextToken"].as_str().map(str::to_string);
            if token.is_none() {
                break;
            }
        }
        assert!(
            token.is_none(),
            "case {case:?}: following the tokens must end, got {token:?}"
        );
        assert_eq!(
            seen, jobs,
            "case {case:?}: paging must see every job exactly once, in start order, got {seen:?}"
        );
    }

    // Each published bound of the modelled `MaxResultsType` range (1..1000) is accepted:
    // the lower one is a page of one, the upper one more than the selection holds, so it
    // answers every job at once with no token.
    for page_size in [1usize, 1000] {
        let case = format!("published bound maxResults {page_size}");
        let case = case.as_str();
        let (status, body) = list_mappings(
            &mgn,
            &definition_id,
            &execution_id,
            json!({"maxResults": page_size}),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: a maxResults on a published bound must be accepted, got {status} with {body:?}"
        );
        let page = body["items"]
            .as_array()
            .unwrap_or_else(|| panic!("case {case:?}: no items array in {body:?}"))
            .clone();
        assert_eq!(
            page.len(),
            page_size.min(jobs.len()),
            "case {case:?}: the page must hold as many jobs as the smaller of the page size and the selection, got {page:?}"
        );
        assert_eq!(
            body["nextToken"].as_str().is_some(),
            page_size < jobs.len(),
            "case {case:?}: a token is issued exactly when jobs remain, got {:?}",
            body["nextToken"]
        );
    }

    // A string this service never issued is not a cursor.
    for token in ["not-a-token", "mgn-network-migration-executions-whatever"] {
        let (status, body) = list_mappings(
            &mgn,
            &definition_id,
            &execution_id,
            json!({"nextToken": token}),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {token:?}: an unissued token expected 400, got {status} with {body:?}"
        );
        assert_eq!(
            error_code(&body),
            "ValidationException",
            "case {token:?}: an unissued token expected ValidationException, got {body:?}"
        );
    }
}

/// A listing that names a definition or an execution the account holds no record of is
/// refused with 404 `ResourceNotFoundException`, naming the kind and the id that was
/// missing.
#[tokio::test]
async fn the_listing_refuses_a_definition_or_execution_no_record_is_held_of() {
    let mgn = Mgn::new();
    let first = create_definition(&mgn, "first definition", "mapping-listing-a").await;
    let second = create_definition(&mgn, "second definition", "mapping-listing-b").await;
    let first_execution = execution_id_of(
        "first definition",
        &staged_execution(&mgn, "first definition", &first).await,
    );
    let second_execution = execution_id_of(
        "second definition",
        &staged_execution(&mgn, "second definition", &second).await,
    );

    let unknown_definition = "nmd-zzzzzzzzzzzzzzzzz";
    let unknown_execution = uuid_shaped(0xbeef);
    let cases = [
        (
            "a definition no account holds",
            unknown_definition.to_string(),
            first_execution.clone(),
            "NetworkMigrationDefinition",
            unknown_definition.to_string(),
        ),
        (
            "an execution no definition holds",
            first.clone(),
            unknown_execution.clone(),
            "NetworkMigrationExecution",
            unknown_execution.clone(),
        ),
        (
            "an execution of another definition of this account",
            first.clone(),
            second_execution.clone(),
            "NetworkMigrationExecution",
            second_execution.clone(),
        ),
    ];

    for (case, definition_id, execution_id, resource_type, resource_id) in cases {
        let (status, body) = list_mappings(&mgn, &definition_id, &execution_id, json!({})).await;
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
            Some(resource_type),
            "case {case:?}: resourceType must name the missing kind, got {:?}",
            body["resourceType"]
        );
        assert_eq!(
            body["message"].as_str(),
            Some(format!("{resource_type} with Id: '{resource_id}' was not found").as_str()),
            "case {case:?}: message must name the kind and the id, got {:?}",
            body["message"]
        );
    }
}
