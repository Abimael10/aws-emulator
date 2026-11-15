// SPDX-License-Identifier: CC0-1.0

//! What `ListCopyJobs` reports, over the copy jobs this account holds.
//!
//! The RULE under test: the listing reports the stored copy jobs, projected by the SAME
//! projection `DescribeCopyJob` reads through — so a listed job and a described one never
//! disagree — after applying the `By*` filters the model declares, each of which selects
//! the jobs whose own stored member equals the value asked for and nothing else; the page
//! is ordered by the instant each copy was started, `MaxResults` bounds it and the
//! `NextToken` it answers with resolves to the next page of the SAME order. Every
//! `CREATED` -> `COMPLETED` transition the clock has already made due is materialised
//! first, through the same step the describe uses.
//!
//! The filter table spans what the rule generalises over: which member the filter is over
//! (the protected resource, its Backup type, the job's state, the destination vault's ARN,
//! the source recovery point's ARN, the account, the creation window and the completion
//! window), and whether the value asked for is one a stored job carries or one no job
//! does. The paging test spans the page sizes a client asks for, and the round-trip test
//! reads one listed job back through the describe.
//!
//! Every case drives real HTTP through the router: the Backup wire is the shared harness in
//! `vault_and_backup_job.rs`.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::vault_and_backup_job::{
    call, create_vault, error_code, router, start_job, string_at, ACCOUNT, PAST_SETTLE, REGION,
};

/// The vault every case backs its resources up into and copies into.
const VAULT: &str = "copy-listing";

/// The terminal `CopyJobState` a settled copy reaches (model enum `CopyJobState`).
const COMPLETED: &str = "COMPLETED";

/// The Backup resource type of the protected resources below, as a read of the job
/// reports it.
const DYNAMODB_TYPE: &str = "DynamoDB";

/// An instant long before any case runs, in the `date-time` form this protocol binds a
/// timestamp to a query parameter as — so a window that OPENS at it holds every copy this
/// suite starts and one that CLOSES at it holds none.
const PAST_INSTANT: &str = "2000-01-01T00:00:00Z";

/// The role every job of this suite runs with: what it tests is the listing, not the
/// outcome a role decides.
fn role() -> String {
    format!("arn:aws:iam::{ACCOUNT}:role/service-role/AWSBackupDefaultServiceRole")
}

/// The ARN of the vault the cases use.
fn vault_arn() -> String {
    format!("arn:aws:backup:{REGION}:{ACCOUNT}:backup-vault:{VAULT}")
}

/// The protected resource `name` of this account in this Region.
fn table_arn(name: &str) -> String {
    format!("arn:aws:dynamodb:{REGION}:{ACCOUNT}:table/{name}")
}

/// `GET /copy-jobs`, as the model binds `ListCopyJobs`, with `query` appended.
async fn list_copy_jobs(router: &MultiServiceRouter, query: &str) -> (u16, Value) {
    let path = if query.is_empty() {
        "/copy-jobs".to_string()
    } else {
        format!("/copy-jobs?{query}")
    };
    call(router, "GET", &path, None).await
}

/// The `CopyJobs` a listing reports for `query`, asserted to have answered 200.
async fn listed(router: &MultiServiceRouter, case: &str, query: &str) -> Vec<Value> {
    let (status, body) = list_copy_jobs(router, query).await;
    assert_eq!(
        status, 200,
        "case {case:?}: ListCopyJobs({query:?}) answered {status} {body}"
    );
    body["CopyJobs"].as_array().cloned().unwrap_or_default()
}

/// Create the vault, back `resources` up into it and copy each point into the same vault,
/// so the store holds one copy job per resource. Answers the `(resource, copy job id)`
/// pairs, in the order the copies were started.
async fn given_copies(router: &MultiServiceRouter, resources: &[&str]) -> Vec<(String, String)> {
    let (status, created) = create_vault(router, VAULT, "{}").await;
    assert_eq!(
        status, 200,
        "setup: CreateBackupVault answered {status} {created}"
    );
    let mut started = Vec::new();
    for resource in resources {
        let resource_arn = table_arn(resource);
        let (status, backup) = start_job(
            router,
            &json!({
                "BackupVaultName": VAULT,
                "ResourceArn": &resource_arn,
                "IamRoleArn": role(),
            })
            .to_string(),
        )
        .await;
        assert_eq!(
            status, 200,
            "setup {resource:?}: StartBackupJob answered {status} {backup}"
        );
        let point = string_at(&backup, "RecoveryPointArn");
        // A copy reads a recovery point whose backup has finished, so the point is
        // addressable only once the clock has settled it.
        std::thread::sleep(PAST_SETTLE);
        let (status, copy) = call(
            router,
            "PUT",
            "/copy-jobs",
            Some(
                &json!({
                    "RecoveryPointArn": &point,
                    "SourceBackupVaultName": VAULT,
                    "DestinationBackupVaultArn": vault_arn(),
                    "IamRoleArn": role(),
                })
                .to_string(),
            ),
        )
        .await;
        assert_eq!(
            status, 200,
            "setup {resource:?}: StartCopyJob answered {status} {copy}"
        );
        started.push((resource_arn, string_at(&copy, "CopyJobId")));
    }
    // Every copy of this suite has settled by now, so the listing reports the terminal
    // state rather than racing the clock.
    std::thread::sleep(PAST_SETTLE);
    started
}

/// Every copy job the store holds is reported, and each listed job is the job the describe
/// reports for the same id.
#[tokio::test]
async fn every_stored_copy_job_is_listed_and_agrees_with_its_describe() {
    let case = "three copies in one vault";
    let router = router();
    let started = given_copies(&router, &["orders", "ledger", "invoices"]).await;

    let jobs = listed(&router, case, "").await;
    assert_eq!(
        jobs.len(),
        started.len(),
        "case {case:?}: expected every stored copy job, got {jobs:?}"
    );
    for (_, copy_job_id) in &started {
        let Some(listed_job) = jobs
            .iter()
            .find(|job| job["CopyJobId"] == json!(copy_job_id))
        else {
            panic!("case {case:?}: expected {copy_job_id} among {jobs:?}");
        };
        let (status, described) = call(
            &router,
            "GET",
            &format!("/copy-jobs/{copy_job_id}"),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeCopyJob answered {status} {described}"
        );
        assert_eq!(
            listed_job, &described["CopyJob"],
            "case {case:?}: expected the listed job to be the described one, got \
             {listed_job} against {described}"
        );
        assert_eq!(
            listed_job["State"],
            json!(COMPLETED),
            "case {case:?}: expected the settled state, got {listed_job}"
        );
    }
    // The order a caller pages through is the order the copies were started.
    let ids: Vec<Value> = jobs.iter().map(|job| job["CopyJobId"].clone()).collect();
    let expected: Vec<Value> = started
        .iter()
        .map(|(_, copy_job_id)| json!(copy_job_id))
        .collect();
    assert_eq!(
        ids, expected,
        "case {case:?}: expected the jobs in the order they were started, got {ids:?}"
    );
}

/// Each `By*` filter selects the jobs whose own stored member equals the value asked for.
#[tokio::test]
async fn each_filter_selects_the_jobs_whose_member_it_names() {
    let router = router();
    let started = given_copies(&router, &["orders", "ledger"]).await;
    let (first_resource, first_id) = started[0].clone();
    let first = listed(&router, "the first copy", &format!("resourceArn={first_resource}"))
        .await
        .first()
        .cloned()
        .unwrap_or(Value::Null);
    let source_point = string_at(&first, "SourceRecoveryPointArn");

    // (case, the query, how many jobs it selects)
    let rows: Vec<(&str, String, usize)> = vec![
        (
            "the protected resource of one copy",
            format!("resourceArn={first_resource}"),
            1,
        ),
        (
            "a protected resource no copy has",
            format!("resourceArn={}", table_arn("absent")),
            0,
        ),
        (
            "the Backup type both copies share",
            format!("resourceType={DYNAMODB_TYPE}"),
            2,
        ),
        ("a Backup type no copy has", "resourceType=EBS".to_string(), 0),
        ("the state both copies settled in", format!("state={COMPLETED}"), 2),
        ("a state no copy is in", "state=FAILED".to_string(), 0),
        (
            "the destination vault both copies were written to",
            format!("destinationVaultArn={}", vault_arn()),
            2,
        ),
        (
            "a destination vault no copy names",
            format!(
                "destinationVaultArn=arn:aws:backup:{REGION}:{ACCOUNT}:backup-vault:elsewhere"
            ),
            0,
        ),
        (
            "the source recovery point of one copy",
            format!("sourceRecoveryPointArn={source_point}"),
            1,
        ),
        ("the account that holds them", format!("accountId={ACCOUNT}"), 2),
        (
            "another account, which holds none of them",
            "accountId=210987654321".to_string(),
            0,
        ),
        (
            "a creation window the copies are inside",
            format!("createdAfter={PAST_INSTANT}"),
            2,
        ),
        (
            "a creation window closing before they were started",
            format!("createdBefore={PAST_INSTANT}"),
            0,
        ),
        (
            "a completion window the copies are inside",
            format!("completeAfter={PAST_INSTANT}"),
            2,
        ),
        (
            "a message category no copy was recorded in",
            "messageCategory=AccessDenied".to_string(),
            0,
        ),
        (
            // The two values the member documents as standing for every category filter
            // nothing out.
            "the category wildcard, which filters nothing out",
            "messageCategory=AGGREGATE_ALL".to_string(),
            2,
        ),
    ];
    for (case, query, expected) in rows {
        let jobs = listed(&router, case, &query).await;
        assert_eq!(
            jobs.len(),
            expected,
            "case {case:?}: expected {expected} job(s) for {query:?}, got {jobs:?}"
        );
    }
    // A filter over a member of ONE job selects that job and no other.
    let selected = listed(
        &router,
        "the first copy by its source point",
        &format!("sourceRecoveryPointArn={source_point}"),
    )
    .await;
    assert_eq!(
        selected.first().map(|job| job["CopyJobId"].clone()),
        Some(json!(first_id)),
        "case \"the first copy by its source point\": expected that job, got {selected:?}"
    );
}

/// `MaxResults` bounds the page and the `NextToken` the answer carries resolves to the
/// next page of the same order; a page size outside the model's range, and a token this
/// result set never issued, are refused as bad parameter values.
#[tokio::test]
async fn the_page_is_bounded_and_its_token_resolves_to_the_next_page() {
    let router = router();
    let started = given_copies(&router, &["orders", "ledger", "invoices"]).await;
    let every: Vec<Value> = listed(&router, "every job", "")
        .await
        .iter()
        .map(|job| job["CopyJobId"].clone())
        .collect();

    for page_size in 1..=started.len() {
        let case = format!("pages of {page_size}");
        let mut seen: Vec<Value> = Vec::new();
        let mut query = format!("maxResults={page_size}");
        loop {
            let (status, body) = list_copy_jobs(&router, &query).await;
            assert_eq!(
                status, 200,
                "case {case:?}: ListCopyJobs({query:?}) answered {status} {body}"
            );
            let page = body["CopyJobs"].as_array().cloned().unwrap_or_default();
            assert!(
                page.len() <= page_size,
                "case {case:?}: expected at most {page_size} job(s), got {page:?}"
            );
            seen.extend(page.iter().map(|job| job["CopyJobId"].clone()));
            match body["NextToken"].as_str() {
                Some(token) => query = format!("maxResults={page_size}&nextToken={token}"),
                None => break,
            }
        }
        assert_eq!(
            seen, every,
            "case {case:?}: expected the same order as the unpaged listing, got {seen:?}"
        );
    }

    // (case, the query, the member the refusal is about)
    for (case, query) in [
        ("a page size below the model's range", "maxResults=0"),
        ("a page size above the model's range", "maxResults=1001"),
        ("a token this result set never issued", "nextToken=not-a-token"),
        ("a malformed resource filter", "resourceArn=not-an-arn"),
    ] {
        let (status, refused) = list_copy_jobs(&router, query).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected 400 for {query:?}, got {status} {refused}"
        );
        assert_eq!(
            error_code(&refused),
            "InvalidParameterValueException",
            "case {case:?}: expected the modelled refusal for {query:?}, got {refused}"
        );
    }
}
