// SPDX-License-Identifier: CC0-1.0

//! What a backup-job read and a recovery-point read report, over the wire.
//!
//! The rules under test:
//!
//! * every instant these responses carry is serialised to the precision the model documents
//!   for them — "the value of `CreationDate` is accurate to milliseconds. For example, the
//!   value 1516925490.087 represents Friday, January 26, 2018 12:11:30.087 AM" — so no
//!   member reports a finer reading of the host clock than AWS ever emits;
//! * a backup-job read reports every member the stored job carries: the instant it was
//!   initiated, the deadline its start window set (the service's own 8-hour window when the
//!   start named none), the protected resource's non-unique name, the lifecycle its recovery
//!   point is kept under and the message category its terminal state puts it in;
//! * and it leaves unset what the job does NOT carry: no backup options, no vault-type
//!   classification for an ordinary backup vault, and no expected completion once the job
//!   has completed;
//! * a recovery point is encrypted with the key of the vault holding it — which every vault
//!   has — so a read of it reports `IsEncrypted` true and that vault's `EncryptionKeyArn`,
//!   and classifies the key exactly as the vault does;
//! * both reads of one job, and both reads of one recovery point, report the same body.
//!
//! Every case drives real HTTP through the router, so what is asserted is the wire behaviour
//! a `backup` SDK client sees. The request harness is the shared one in
//! `vault_and_backup_job.rs`, and the air-gapped create is the one in
//! `logically_air_gapped_vault.rs`. A transition falls due on the emulator's clock, so each
//! case looks again for the CONDITION it waits on, with a deadline far past the span.

use std::time::Instant;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::logically_air_gapped_vault::create_air_gapped;
use crate::vault_and_backup_job::{
    call, create_vault, error_code, router, settled_backup_job, start_job, string_at, ACCOUNT,
    POLL_DEADLINE, POLL_WAIT, REGION,
};

/// `VaultType` of a logically air-gapped vault, and of an ordinary one (model enum
/// `VaultType`), spelled out here rather than imported so a wrong constant in the
/// implementation cannot make a case pass.
const AIR_GAPPED: &str = "LOGICALLY_AIR_GAPPED_BACKUP_VAULT";
const BACKUP_VAULT: &str = "BACKUP_VAULT";

/// The `MessageCategory` a job that captured its resource is recorded in, spelled out here
/// for the same reason.
const SUCCESS: &str = "SUCCESS";

/// The `StartWindowMinutes` AWS Backup applies when the start names none: "this value is
/// optional, and the default is 8 hours"
/// (`smithy_query(query="doc",shape="StartBackupJobInput",member="StartWindowMinutes")`).
const DEFAULT_START_WINDOW_MINUTES: i64 = 8 * 60;

/// The role every case backs its resource up with.
fn backup_role() -> String {
    format!("arn:aws:iam::{ACCOUNT}:role/service-role/AWSBackupDefaultServiceRole")
}

/// `GET /backup-jobs/{BackupJobId}`, as the model binds `DescribeBackupJob`.
async fn describe_job(router: &MultiServiceRouter, job_id: &str) -> Value {
    let (status, body) = call(router, "GET", &format!("/backup-jobs/{job_id}"), None).await;
    assert_eq!(
        (status, error_code(&body).as_str()),
        (200, "<none>"),
        "expected DescribeBackupJob 200 for {job_id}, got {status} {body:?}"
    );
    body
}

/// The job `job_id` once it reports the terminal state the clock owes it, and that state is
/// the completion a job that captured its resource reaches.
async fn settled_job(router: &MultiServiceRouter, case: &str, job_id: &str) -> Value {
    let settled = settled_backup_job(router, case, job_id).await;
    assert_eq!(
        settled.get("State").and_then(Value::as_str),
        Some("COMPLETED"),
        "case {case:?}: expected the job to capture its resource, got {settled:?}"
    );
    settled
}

/// The vault `name` once it reports the `AVAILABLE` state the clock owes it, looked for
/// again until it does or until [`POLL_DEADLINE`] passes.
async fn available_vault(router: &MultiServiceRouter, case: &str, name: &str) {
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let (status, read) = call(router, "GET", &format!("/backup-vaults/{name}"), None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected DescribeBackupVault 200, got {status} {read:?}"
        );
        if read.get("VaultState").and_then(Value::as_str) == Some("AVAILABLE") {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the vault to become AVAILABLE within {POLL_DEADLINE:?}, \
             got {read:?}"
        );
        std::thread::sleep(POLL_WAIT);
    }
}

/// The one recovery point `vault` holds, as `ListRecoveryPointsByBackupVault` reports it.
async fn listed_point(router: &MultiServiceRouter, case: &str, vault: &str) -> Value {
    let (status, body) = call(
        router,
        "GET",
        &format!("/backup-vaults/{vault}/recovery-points"),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected ListRecoveryPointsByBackupVault 200, got {status} {body:?}"
    );
    let points = body["RecoveryPoints"].as_array().cloned().unwrap_or_default();
    assert_eq!(
        points.len(),
        1,
        "case {case:?}: expected the one recovery point the job wrote, got {points:?}"
    );
    points[0].clone()
}

/// The `EncryptionKeyArn` the vault `name` reports, which every vault has.
async fn vault_key(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let (status, read) = call(router, "GET", &format!("/backup-vaults/{name}"), None).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected DescribeBackupVault 200, got {status} {read:?}"
    );
    let key = string_at(&read, "EncryptionKeyArn");
    assert!(
        !key.is_empty(),
        "case {case:?}: a vault always reports the key it is encrypted with, got {read:?}"
    );
    key
}

/// A timestamp member as epoch seconds, or `None` when the body omits it.
fn instant_at(body: &Value, member: &str) -> Option<f64> {
    body.get(member).and_then(Value::as_f64)
}

/// One row of the job-read table: what the start carried, and what the read must report
/// back from the stored job.
struct JobCase {
    name: &'static str,
    resource_arn: String,
    /// The protected resource's non-unique name, as its ARN names it.
    resource_name: &'static str,
    /// The `Lifecycle` the start asked the recovery point to be kept under.
    lifecycle: Option<Value>,
    /// The `BackupOptions` the start carried.
    backup_options: Option<Value>,
    /// The `StartWindowMinutes` the start named, when the row names one.
    start_window_minutes: Option<i64>,
}

impl JobCase {
    fn start_body(&self) -> String {
        let mut body = json!({
            "BackupVaultName": "read-bodies",
            "ResourceArn": self.resource_arn,
            "IamRoleArn": backup_role(),
        });
        if let Some(lifecycle) = &self.lifecycle {
            body["Lifecycle"] = lifecycle.clone();
        }
        if let Some(options) = &self.backup_options {
            body["BackupOptions"] = options.clone();
        }
        if let Some(minutes) = self.start_window_minutes {
            body["StartWindowMinutes"] = json!(minutes);
        }
        body.to_string()
    }

    /// The start window in force for this job: the one the start named, or the service's
    /// own default when it named none.
    fn window_minutes(&self) -> i64 {
        self.start_window_minutes
            .unwrap_or(DEFAULT_START_WINDOW_MINUTES)
    }

    /// The `RecoveryPointLifecycle` the read must report: the one the start named, or the
    /// empty one a job started with none is kept under.
    fn reported_lifecycle(&self) -> Value {
        self.lifecycle
            .clone()
            .unwrap_or_else(|| Value::Object(serde_json::Map::new()))
    }
}

/// The rows of the job-read table. They differ along what the rule generalises over: the
/// protected resource (so the form its ARN names the resource in — a `type/name` path, a
/// `type:name` pair and a bare bucket), whether the start named a lifecycle, whether it
/// named backup options, and whether it named a start window (both published ends of the
/// window's range and no window at all).
fn job_cases() -> Vec<JobCase> {
    vec![
        JobCase {
            name: "a DynamoDB table, no lifecycle, no options, no start window",
            resource_arn: format!("arn:aws:dynamodb:{REGION}:{ACCOUNT}:table/orders"),
            resource_name: "orders",
            lifecycle: None,
            backup_options: None,
            start_window_minutes: None,
        },
        JobCase {
            name: "an RDS instance with a retention and the shortest start window",
            resource_arn: format!("arn:aws:rds:{REGION}:{ACCOUNT}:db:ledger"),
            resource_name: "ledger",
            lifecycle: Some(json!({"DeleteAfterDays": 35})),
            backup_options: None,
            // 60 minutes is the floor the member documents ("it must be at least 60
            // minutes to avoid errors").
            start_window_minutes: Some(60),
        },
        JobCase {
            name: "an S3 bucket with backup options and a cold-storage lifecycle",
            resource_arn: "arn:aws:s3:::reports-bucket".to_string(),
            resource_name: "reports-bucket",
            lifecycle: Some(json!({
                "MoveToColdStorageAfterDays": 30,
                "DeleteAfterDays": 120,
            })),
            backup_options: Some(json!({"WindowsVSS": "enabled"})),
            start_window_minutes: Some(1_440),
        },
        JobCase {
            name: "an EBS volume with the longest start window the member admits",
            resource_arn: format!("arn:aws:ec2:{REGION}:{ACCOUNT}:volume/vol-0123456789abcdef"),
            resource_name: "vol-0123456789abcdef",
            lifecycle: None,
            backup_options: None,
            // 52,560,000 minutes is the ceiling the member documents ("this parameter
            // has a maximum value of 100 years (52,560,000 minutes)").
            start_window_minutes: Some(52_560_000),
        },
    ]
}

/// A backup-job read reports every member the stored job carries, and leaves unset what it
/// does not — the same body whether the job is read on its own or in a page.
#[tokio::test]
async fn a_job_read_reports_the_members_the_stored_job_carries() {
    for case in job_cases() {
        let name = case.name;
        let router = router();
        let (status, created) = create_vault(&router, "read-bodies", "{}").await;
        assert_eq!(
            status, 200,
            "case {name:?}: setup CreateBackupVault should succeed, got {status} {created:?}"
        );

        let (status, started) = start_job(&router, &case.start_body()).await;
        assert_eq!(
            status, 200,
            "case {name:?}: expected StartBackupJob 200, got {status} {started:?}"
        );
        let job_id = string_at(&started, "BackupJobId");

        // A job on its way to a terminal state is expected to complete and is in no message
        // category yet. Whether the read catches it there is up to the clock, which owes the
        // settle a fixed span after the start, so these are asserted on the read that did
        // catch it running rather than on the assumption that it did.
        let running = describe_job(&router, &job_id).await;
        let mut expected_completion = None;
        if string_at(&running, "State") == "CREATED" {
            expected_completion = instant_at(&running, "ExpectedCompletionDate");
            assert!(
                expected_completion.is_some(),
                "case {name:?}: a job on its way to completion reports when it is expected, \
                 got {running:?}"
            );
            assert!(
                running.get("MessageCategory").is_none(),
                "case {name:?}: a job that has reached no terminal state is in no message \
                 category, got {running:?}"
            );
        }

        let settled = settled_job(&router, name, &job_id).await;

        // What the stored job carries.
        assert_eq!(
            string_at(&settled, "ResourceName"),
            case.resource_name,
            "case {name:?}: expected the protected resource's name, got {settled:?}"
        );
        let creation = instant_at(&settled, "CreationDate");
        assert!(
            creation.is_some(),
            "case {name:?}: a stored job reports its creation instant, got {settled:?}"
        );
        assert_eq!(
            instant_at(&settled, "InitiationDate"),
            creation,
            "case {name:?}: an on-demand job was initiated by the call that created it, \
             got {settled:?}"
        );
        let start_by = instant_at(&settled, "StartBy").unwrap_or_else(|| {
            panic!("case {name:?}: a job reports the deadline its start window set, got {settled:?}")
        });
        let window_seconds = (case.window_minutes() * 60) as f64;
        assert!(
            (start_by - creation.unwrap_or_default() - window_seconds).abs() < 1e-3,
            "case {name:?}: expected StartBy the start window ({} minutes) past the \
             creation instant, got {settled:?}",
            case.window_minutes()
        );
        assert_eq!(
            string_at(&settled, "MessageCategory"),
            SUCCESS,
            "case {name:?}: a job that captured its resource is recorded as a success, \
             got {settled:?}"
        );
        assert_eq!(
            settled.get("RecoveryPointLifecycle").cloned(),
            Some(case.reported_lifecycle()),
            "case {name:?}: expected the lifecycle the recovery point is kept under, \
             got {settled:?}"
        );

        // What it does not.
        assert_eq!(
            settled.get("BackupOptions").cloned(),
            case.backup_options.clone(),
            "case {name:?}: the backup options reported must be the ones the start \
             carried, got {settled:?}"
        );
        assert!(
            settled.get("VaultType").is_none(),
            "case {name:?}: a job in an ordinary backup vault reports no VaultType, \
             got {settled:?}"
        );
        assert!(
            settled.get("ExpectedCompletionDate").is_none(),
            "case {name:?}: a job that has completed is expected to complete no longer, \
             got {settled:?}"
        );
        // The completion reported is the instant that came due — the one the running job
        // named as its expected completion, when the read above caught it running.
        if let Some(expected_completion) = expected_completion {
            assert_eq!(
                instant_at(&settled, "CompletionDate"),
                Some(expected_completion),
                "case {name:?}: the completion reported is the instant it came due, \
                 got {settled:?}"
            );
        } else {
            assert!(
                instant_at(&settled, "CompletionDate").is_some(),
                "case {name:?}: a job that has completed reports when it did, \
                 got {settled:?}"
            );
        }

        // The page reports the same stored job.
        let (status, page) = call(&router, "GET", "/backup-jobs", None).await;
        assert_eq!(
            status, 200,
            "case {name:?}: expected ListBackupJobs 200, got {status} {page:?}"
        );
        let jobs = page["BackupJobs"].as_array().cloned().unwrap_or_default();
        assert_eq!(
            jobs.len(),
            1,
            "case {name:?}: expected the one started job, got {jobs:?}"
        );
        for member in [
            "ResourceName",
            "InitiationDate",
            "StartBy",
            "MessageCategory",
            "RecoveryPointLifecycle",
            "BackupOptions",
            "VaultType",
            "ExpectedCompletionDate",
            "CreationDate",
            "CompletionDate",
        ] {
            assert_eq!(
                jobs[0].get(member),
                settled.get(member),
                "case {name:?}: the listed and the described job must agree about {member}, \
                 got {:?} and {settled:?}",
                jobs[0]
            );
        }
    }
}

/// `VaultType` classifies the vault holding the job's recovery point, and is reported only
/// when there is something to classify: a job in an ordinary backup vault reports none,
/// while one in a logically air-gapped vault reports that type.
#[tokio::test]
async fn a_job_reports_the_vault_type_only_for_a_vault_that_is_not_ordinary() {
    // (case, the vault, whether it is air-gapped, the VaultType the read must report)
    let rows: Vec<(&str, &str, bool, Option<&str>)> = vec![
        ("an ordinary backup vault", "ordinary-vault", false, None),
        (
            "a logically air-gapped vault",
            "airgapped-vault",
            true,
            Some(AIR_GAPPED),
        ),
    ];

    for (case, vault, air_gapped, expected) in rows {
        let router = router();
        if air_gapped {
            let (status, created) = create_air_gapped(
                &router,
                vault,
                &json!({"MinRetentionDays": 7, "MaxRetentionDays": 365}).to_string(),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: setup CreateLogicallyAirGappedBackupVault should succeed, \
                 got {status} {created:?}"
            );
            // Such a vault receives backups only once the clock has made it available.
            available_vault(&router, case, vault).await;
        } else {
            let (status, created) = create_vault(&router, vault, "{}").await;
            assert_eq!(
                status, 200,
                "case {case:?}: setup CreateBackupVault should succeed, got {status} \
                 {created:?}"
            );
        }

        let body = json!({
            "BackupVaultName": vault,
            "ResourceArn": format!("arn:aws:dynamodb:{REGION}:{ACCOUNT}:table/orders"),
            "IamRoleArn": backup_role(),
        })
        .to_string();
        let (status, started) = start_job(&router, &body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected StartBackupJob 200, got {status} {started:?}"
        );
        let described = describe_job(&router, &string_at(&started, "BackupJobId")).await;
        assert_eq!(
            described.get("VaultType").and_then(Value::as_str),
            expected,
            "case {case:?}: expected VaultType {expected:?}, got {described:?}"
        );
        // The recovery point's own read classifies the vault it is held in the same way
        // the vault read does.
        let point = listed_point(&router, case, vault).await;
        assert_eq!(
            point.get("VaultType").and_then(Value::as_str),
            Some(if air_gapped { AIR_GAPPED } else { BACKUP_VAULT }),
            "case {case:?}: a recovery point reports the type of the vault holding it, \
             got {point:?}"
        );
    }
}

/// A recovery point is encrypted with the key of the vault holding it, and reports the
/// lifecycle it is kept under and the job that wrote it — the same body whether the point is
/// read on its own or in the vault's page.
#[tokio::test]
async fn a_recovery_point_read_reports_the_vault_key_and_its_lifecycle() {
    let customer_key =
        format!("arn:aws:kms:{REGION}:{ACCOUNT}:key/1234abcd-12ab-34cd-56ef-1234567890ab");

    // (case, the CreateBackupVault body, the Lifecycle the backup named, whether that
    // lifecycle makes a cold-storage transition due, whether it makes a deletion due)
    let rows: Vec<(&str, String, Option<Value>, bool, bool)> = vec![
        (
            "the key this service creates for a vault whose create named none",
            "{}".to_string(),
            None,
            false,
            false,
        ),
        (
            "a vault created with a customer-managed key",
            json!({"EncryptionKeyArn": customer_key}).to_string(),
            Some(json!({"DeleteAfterDays": 35})),
            false,
            true,
        ),
        (
            "a lifecycle that moves the point to cold storage and then deletes it",
            "{}".to_string(),
            Some(json!({"MoveToColdStorageAfterDays": 30, "DeleteAfterDays": 120})),
            true,
            true,
        ),
        (
            "the documented keep-indefinitely sentinel, which makes nothing due",
            "{}".to_string(),
            Some(json!({"DeleteAfterDays": -1})),
            false,
            false,
        ),
    ];

    for (case, vault_body, lifecycle, move_to_cold_storage_at, delete_at) in rows {
        let router = router();
        let (status, created) = create_vault(&router, "point-bodies", &vault_body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: setup CreateBackupVault should succeed, got {status} {created:?}"
        );
        let key = vault_key(&router, case, "point-bodies").await;

        let resource_arn = format!("arn:aws:dynamodb:{REGION}:{ACCOUNT}:table/orders");
        let mut body = json!({
            "BackupVaultName": "point-bodies",
            "ResourceArn": resource_arn,
            "IamRoleArn": backup_role(),
        });
        if let Some(lifecycle) = &lifecycle {
            body["Lifecycle"] = lifecycle.clone();
        }
        let (status, started) = start_job(&router, &body.to_string()).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected StartBackupJob 200, got {status} {started:?}"
        );
        let job_id = string_at(&started, "BackupJobId");
        let point_arn = string_at(&started, "RecoveryPointArn");
        let job = settled_job(&router, case, &job_id).await;

        let listed = listed_point(&router, case, "point-bodies").await;
        let (status, read) = call(
            &router,
            "GET",
            &format!("/backup-vaults/point-bodies/recovery-points/{point_arn}"),
            None,
        )
        .await;
        assert_eq!(
            (status, error_code(&read).as_str()),
            (200, "<none>"),
            "case {case:?}: expected DescribeRecoveryPoint 200, got {status} {read:?}"
        );

        for (reader, point) in [("the vault's page", &listed), ("the point's read", &read)] {
            assert_eq!(
                point.get("IsEncrypted").and_then(Value::as_bool),
                Some(true),
                "case {case:?} through {reader}: a point in a vault is encrypted with that \
                 vault's key, got {point:?}"
            );
            assert_eq!(
                string_at(point, "EncryptionKeyArn"),
                key,
                "case {case:?} through {reader}: expected the holding vault's key, \
                 got {point:?}"
            );
            // The key is the vault's, so the classification of it is the vault's too — and
            // an ordinary backup vault reports none.
            assert!(
                point.get("EncryptionKeyType").is_none(),
                "case {case:?} through {reader}: a point in an ordinary backup vault \
                 reports no EncryptionKeyType, got {point:?}"
            );
            assert_eq!(
                point.get("Lifecycle").cloned(),
                Some(
                    lifecycle
                        .clone()
                        .unwrap_or_else(|| Value::Object(serde_json::Map::new()))
                ),
                "case {case:?} through {reader}: expected the lifecycle the point is kept \
                 under, got {point:?}"
            );
            let calculated = point.get("CalculatedLifecycle").unwrap_or_else(|| {
                panic!(
                    "case {case:?} through {reader}: a point reports the instants its \
                     lifecycle makes due, got {point:?}"
                )
            });
            assert_eq!(
                calculated.get("MoveToColdStorageAt").is_some(),
                move_to_cold_storage_at,
                "case {case:?} through {reader}: expected MoveToColdStorageAt \
                 present={move_to_cold_storage_at}, got {point:?}"
            );
            assert_eq!(
                calculated.get("DeleteAt").is_some(),
                delete_at,
                "case {case:?} through {reader}: expected DeleteAt present={delete_at}, \
                 got {point:?}"
            );
            assert_eq!(
                instant_at(point, "InitiationDate"),
                instant_at(&job, "CreationDate"),
                "case {case:?} through {reader}: the point was written by a job initiated \
                 when that job was created, got {point:?}"
            );
            assert_eq!(
                string_at(point, "ResourceName"),
                "orders",
                "case {case:?} through {reader}: expected the protected resource's name, \
                 got {point:?}"
            );
        }
    }
}

/// Whether `member` is a timestamp member: this service's instants are named for the date,
/// the instant or the deadline they state.
fn is_instant_member(member: &str) -> bool {
    ["Date", "At", "By", "Time"]
        .iter()
        .any(|suffix| member.ends_with(suffix))
}

/// Every timestamp member of `body`, at any depth, as (path, epoch seconds).
fn reported_instants(body: &Value, path: &str, found: &mut Vec<(String, f64)>) {
    match body {
        Value::Object(members) => {
            for (member, value) in members {
                let child = format!("{path}.{member}");
                if is_instant_member(member) {
                    if let Some(seconds) = value.as_f64() {
                        found.push((child.clone(), seconds));
                    }
                }
                reported_instants(value, &child, found);
            }
        }
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                reported_instants(item, &format!("{path}[{index}]"), found);
            }
        }
        _ => {}
    }
}

/// How many fractional digits it takes to state `seconds` exactly.
///
/// The shortest rendering that reads back as the same number: a value AWS would serialise
/// with at most three of them needs at most three here, and one carrying a finer reading of
/// the clock needs more — so this counts the precision of the serialised form.
fn fractional_digits(seconds: f64) -> usize {
    let rendered = format!("{seconds}");
    rendered
        .split_once('.')
        .map_or(0, |(_, fraction)| fraction.trim_end_matches('0').len())
}

/// Every instant these responses report is serialised to the millisecond precision the
/// model documents for them.
#[tokio::test]
async fn every_instant_a_read_reports_carries_at_most_millisecond_precision() {
    let router = router();
    let (status, created) = create_vault(&router, "precision-vault", "{}").await;
    assert_eq!(
        status, 200,
        "setup: CreateBackupVault should succeed, got {status} {created:?}"
    );
    let body = json!({
        "BackupVaultName": "precision-vault",
        "ResourceArn": format!("arn:aws:dynamodb:{REGION}:{ACCOUNT}:table/orders"),
        "IamRoleArn": backup_role(),
        "StartWindowMinutes": 60,
        // A lifecycle so the calculated instants are reported too.
        "Lifecycle": {"MoveToColdStorageAfterDays": 30, "DeleteAfterDays": 120},
    })
    .to_string();
    let (status, started) = start_job(&router, &body).await;
    assert_eq!(
        status, 200,
        "setup: StartBackupJob should succeed, got {status} {started:?}"
    );
    let job_id = string_at(&started, "BackupJobId");
    let point_arn = string_at(&started, "RecoveryPointArn");

    // Every body this slice reports an instant in: the start's own answer, the job while
    // it runs and once it has settled, both job and recovery-point pages, the point's own
    // read and the vault's.
    let running = describe_job(&router, &job_id).await;
    let settled = settled_job(&router, "precision", &job_id).await;
    let mut bodies = vec![
        ("StartBackupJob", started.clone()),
        ("DescribeBackupJob while running", running),
        ("DescribeBackupJob once settled", settled),
    ];
    for (case, method, path) in [
        ("ListBackupJobs", "GET", "/backup-jobs".to_string()),
        (
            "ListRecoveryPointsByBackupVault",
            "GET",
            "/backup-vaults/precision-vault/recovery-points".to_string(),
        ),
        (
            "DescribeRecoveryPoint",
            "GET",
            format!("/backup-vaults/precision-vault/recovery-points/{point_arn}"),
        ),
        (
            "DescribeBackupVault",
            "GET",
            "/backup-vaults/precision-vault".to_string(),
        ),
    ] {
        let (status, body) = call(&router, method, &path, None).await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (200, "<none>"),
            "case {case:?}: expected 200, got {status} {body:?}"
        );
        bodies.push((case, body));
    }

    let mut checked = 0usize;
    for (case, body) in &bodies {
        let mut instants = Vec::new();
        reported_instants(body, "$", &mut instants);
        assert!(
            !instants.is_empty(),
            "case {case:?}: expected the body to report at least one instant, got {body:?}"
        );
        for (path, seconds) in instants {
            let digits = fractional_digits(seconds);
            assert!(
                digits <= 3,
                "case {case:?}: {path} must be accurate to milliseconds, got {seconds} \
                 with {digits} fractional digits"
            );
            checked += 1;
        }
    }
    assert!(
        checked >= bodies.len(),
        "expected every body to contribute an instant, checked only {checked}"
    );
}
