// SPDX-License-Identifier: CC0-1.0

//! What a job listing's `ByMessageCategory` selects, over the copy jobs and the backup
//! jobs this account holds.
//!
//! The RULE under test: a job listing's `ByMessageCategory` selects the jobs whose own
//! stored `MessageCategory` equals the value asked for — so a value no stored job was
//! recorded in selects none of them — EXCEPT the values THAT operation's own member
//! documentation names as standing for every message category, which filter nothing out.
//! The two listings document that wildcard differently, and each honours the spelling its
//! own member documents: `ANY` and `AGGREGATE_ALL` for `ListCopyJobs`, the wildcard `*`
//! and `AGGREGATE_ALL` for `ListBackupJobs`.
//!
//! The table spans what the rule generalises over rather than one request: which listing
//! is asked, whether the value asked for is a wildcard that operation documents or one of
//! the categories a job can be recorded in, and — for the wildcards — both spellings each
//! member documents. The category values are the example strings the members themselves
//! give (`smithy_query(query="doc",shape="ListCopyJobsInput",member="ByMessageCategory",
//! model_path="services/service-com-amazonaws-backup-cryocontrollerusermanager/smithy-model/com-amazonaws-backup-cryocontrollerusermanager.json")`),
//! including ones this issue's recorded call never named. Each row is asserted on the
//! exact set of job ids the page reports, against the unfiltered listing of the same jobs,
//! so neither a listing that ignores the filter nor one that answers an empty page can
//! pass.
//!
//! Every case drives real HTTP through the router: the Backup wire is the shared harness
//! in `vault_and_backup_job.rs`.

use std::time::Duration;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::vault_and_backup_job::{
    call, create_vault, router, start_job, string_at, ACCOUNT, REGION,
};

/// The vault every job of this suite is started into and copied into.
const VAULT: &str = "category-listing";

/// The `Status` a recovery point reaches once the backup that writes it has finished
/// (model enum `RecoveryPointStatus`), which is when a copy of it can be started.
const COMPLETED: &str = "COMPLETED";

/// How long a wait for a recovery point to finish lasts, in all, and how long each look
/// waits before the next. The wait is on the CONDITION — a read materialises the
/// completion the clock owes the point — with room well past the span the service settles
/// in, so a loaded host does not fail a case.
const POLLS: usize = 200;
const POLL_WAIT: Duration = Duration::from_millis(50);

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

/// The ids one listing page reports, asserted to have answered 200: `path` is the
/// listing's URI, `list_member` the output member holding the page and `id_member` the
/// member identifying one job.
async fn listed_ids(
    router: &MultiServiceRouter,
    case: &str,
    path: &str,
    list_member: &str,
    id_member: &str,
) -> Vec<String> {
    let (status, body) = call(router, "GET", path, None).await;
    assert_eq!(
        status, 200,
        "case {case:?}: GET {path} answered {status} {body}"
    );
    let mut ids: Vec<String> = body[list_member]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|job| string_at(job, id_member))
        .collect();
    ids.sort();
    ids
}

/// Wait until the recovery point `arn` of [`VAULT`] reports the completion the clock owes
/// it, so a copy of it can be started. Panics with the last body read if it never does.
async fn await_completed_point(router: &MultiServiceRouter, case: &str, arn: &str) {
    let path = format!("/backup-vaults/{VAULT}/recovery-points/{arn}");
    let mut last = Value::Null;
    for _ in 0..POLLS {
        let (status, body) = call(router, "GET", &path, None).await;
        if status == 200 && string_at(&body, "Status") == COMPLETED {
            return;
        }
        last = body;
        std::thread::sleep(POLL_WAIT);
    }
    panic!("case {case:?}: recovery point {arn} never completed, last read {last}");
}

/// Create the vault, back each of `resources` up into it and copy each finished point into
/// the same vault, so the store holds one backup job AND one copy job per resource.
/// Answers the ids of both, each sorted, as the listings report them.
async fn given_jobs(
    router: &MultiServiceRouter,
    resources: &[&str],
) -> (Vec<String>, Vec<String>) {
    let (status, created) = create_vault(router, VAULT, "{}").await;
    assert_eq!(
        status, 200,
        "setup: CreateBackupVault answered {status} {created}"
    );
    let mut backup_job_ids = Vec::new();
    let mut copy_job_ids = Vec::new();
    for resource in resources {
        let (status, backup) = start_job(
            router,
            &json!({
                "BackupVaultName": VAULT,
                "ResourceArn": table_arn(resource),
                "IamRoleArn": role(),
            })
            .to_string(),
        )
        .await;
        assert_eq!(
            status, 200,
            "setup {resource:?}: StartBackupJob answered {status} {backup}"
        );
        backup_job_ids.push(string_at(&backup, "BackupJobId"));
        let point = string_at(&backup, "RecoveryPointArn");
        // A copy reads a recovery point whose backup has finished, so the point is
        // addressable only once the clock has settled it.
        await_completed_point(router, resource, &point).await;
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
        copy_job_ids.push(string_at(&copy, "CopyJobId"));
    }
    backup_job_ids.sort();
    copy_job_ids.sort();
    (backup_job_ids, copy_job_ids)
}

/// RULE: `ListCopyJobs` answers every stored copy job for each value its own
/// `ByMessageCategory` documentation names as standing for every message category, and
/// none of them for a category value — a copy job this service records none, so no
/// category selects one.
#[tokio::test]
async fn list_copy_jobs_honours_the_wildcards_its_member_documents() {
    let router = router();
    let (_, copy_job_ids) = given_jobs(&router, &["orders", "ledger"]).await;
    let every = listed_ids(&router, "no filter", "/copy-jobs", "CopyJobs", "CopyJobId").await;
    assert_eq!(
        every, copy_job_ids,
        "case \"no filter\": expected every stored copy job, got {every:?}"
    );
    // The filter is over the job's own stored `MessageCategory`, and a copy job this
    // service stores carries none, so the listing reports the member for none of them.
    let (status, body) = call(&router, "GET", "/copy-jobs", None).await;
    assert_eq!(status, 200, "case \"no filter\": got {status} {body}");
    for job in body["CopyJobs"].as_array().cloned().unwrap_or_default() {
        assert_eq!(
            job.get("MessageCategory"),
            None,
            "case \"no filter\": a stored copy job is recorded in no message category, \
             got {job}"
        );
    }

    // (case, the value asked for, the jobs it selects)
    let rows: Vec<(&str, &str, Vec<String>)> = vec![
        (
            "ANY, the value this member documents as standing for every category",
            "ANY",
            copy_job_ids.clone(),
        ),
        (
            "AGGREGATE_ALL, which aggregates every category",
            "AGGREGATE_ALL",
            copy_job_ids.clone(),
        ),
        (
            "SUCCESS, an example category no stored copy job is recorded in",
            "SUCCESS",
            Vec::new(),
        ),
        (
            "AccessDenied, another example category none is recorded in",
            "AccessDenied",
            Vec::new(),
        ),
        (
            "INVALIDPARAMETERS, the third example category",
            "INVALIDPARAMETERS",
            Vec::new(),
        ),
    ];
    for (case, category, expected) in rows {
        let selected = listed_ids(
            &router,
            case,
            &format!("/copy-jobs?messageCategory={category}"),
            "CopyJobs",
            "CopyJobId",
        )
        .await;
        assert_eq!(
            selected, expected,
            "case {case:?}: expected {expected:?} for messageCategory={category}, got \
             {selected:?}"
        );
    }
}

/// RULE: the same for `ListBackupJobs`, whose own `ByMessageCategory` documentation names
/// the wildcard `*`; a backup job that captured its resource is recorded in no category,
/// so no category value selects one.
#[tokio::test]
async fn list_backup_jobs_honours_the_wildcards_its_member_documents() {
    let router = router();
    let (backup_job_ids, _) = given_jobs(&router, &["orders", "ledger"]).await;
    let every = listed_ids(
        &router,
        "no filter",
        "/backup-jobs",
        "BackupJobs",
        "BackupJobId",
    )
    .await;
    assert_eq!(
        every, backup_job_ids,
        "case \"no filter\": expected every stored backup job, got {every:?}"
    );

    // (case, the value asked for, the jobs it selects)
    let rows: Vec<(&str, &str, Vec<String>)> = vec![
        (
            "the wildcard this member documents",
            "*",
            backup_job_ids.clone(),
        ),
        (
            "AGGREGATE_ALL, which aggregates every category",
            "AGGREGATE_ALL",
            backup_job_ids.clone(),
        ),
        (
            "SUCCESS, an example category no stored backup job is recorded in",
            "SUCCESS",
            Vec::new(),
        ),
        (
            "AccessDenied, another example category none is recorded in",
            "AccessDenied",
            Vec::new(),
        ),
    ];
    for (case, category, expected) in rows {
        let selected = listed_ids(
            &router,
            case,
            &format!("/backup-jobs?messageCategory={category}"),
            "BackupJobs",
            "BackupJobId",
        )
        .await;
        assert_eq!(
            selected, expected,
            "case {case:?}: expected {expected:?} for messageCategory={category}, got \
             {selected:?}"
        );
    }
}
