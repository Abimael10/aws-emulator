// SPDX-License-Identifier: CC0-1.0

//! What AWS Backup publishes for a job start it REFUSED: nothing.
//!
//! The RULE under test: only a state a STORED job moves into is announced on the account's
//! default EventBridge bus, so a start call this service refuses synchronously — whichever
//! kind of job it asked for and whichever modelled refusal it earned — stores no job and
//! publishes no `aws.backup` envelope at all, while a start it ACCEPTED has its terminal
//! move delivered to the same rule and the same target. The negative half of the contract
//! is only worth anything beside that positive half: a bus that published nothing ever
//! would satisfy "no envelope for a refused start" vacuously, so every case here proves
//! the path live with a job whose move IS delivered.
//!
//! The first test asserts the rule over a TABLE of refused starts, whose rows differ along
//! what the rule generalises over — the KIND of job the refused call asked for (backup,
//! copy, restore), the modelled error the service answered (an absent resource, a bad
//! parameter value, a missing required member) and therefore which `detail-type` the bus
//! would have carried had anything been stored. The second asserts it without relying on
//! the order the publications could take: the refusals are made BETWEEN two accepted
//! backups, and the only envelopes that ever arrive are those two jobs' own moves.
//!
//! Every case drives real HTTP through the router, so what is asserted is the wire
//! behaviour a `backup`, an `events` and an `sqs` client see together. The emulator, the
//! queue-and-rule setup and the drains are the shared ones in `job_state_change_events.rs`;
//! the Backup request harness is the shared one in `vault_and_backup_job.rs`.

use serde_json::{json, Value};
use service_com_amazonaws_backup_cryocontrollerusermanager::ComAmazonawsBackupCryocontrollerusermanagerState as BackupState;
use smithy_server_runtime::MultiServiceRouter;

use crate::job_state_change_events::{
    delivered, delivered_at_least, describe_backup_job, emulator_with_engine, given_queue,
    given_rule, of_type, role, vault_arn, BACKUP_JOB_STATE_CHANGE, COMPLETED, EVENT_SOURCE,
};
use crate::vault_and_backup_job::{
    call, create_vault, error_code, start_job, string_at, ACCOUNT, REGION,
};

/// The two other `detail-type` values AWS Backup publishes a job's state changes under,
/// spelled out here rather than imported, so a wrong constant in the implementation cannot
/// make a test pass. Source: the EventBridge reference for AWS Backup, whose copy and
/// restore job events carry `"detail-type": "Copy Job State Change"` and
/// `"detail-type": "Restore Job State Change"`.
const COPY_JOB_STATE_CHANGE: &str = "Copy Job State Change";
const RESTORE_JOB_STATE_CHANGE: &str = "Restore Job State Change";

/// The vault every accepted backup is taken into, and the vault a copy would be taken to.
/// Both inside the modelled `BackupVaultName` pattern, `^[a-zA-Z0-9\-\_]{2,50}$`.
const SOURCE_VAULT: &str = "refused-source";
const DESTINATION_VAULT: &str = "refused-destination";

/// The rule every case installs, which is owed EVERY kind of envelope this service
/// publishes: a rule that filtered any of them out could not tell "withheld" from
/// "filtered".
const RULE: &str = "every-backup-job-event";

/// The resource the accepted control backups are taken of: a DynamoDB table's ARN.
fn resource_arn() -> String {
    format!("arn:aws:dynamodb:{REGION}:{ACCOUNT}:table/demo")
}

/// A recovery-point ARN of the right form that names no stored point.
fn fabricated_point() -> String {
    format!("arn:aws:backup:{REGION}:{ACCOUNT}:recovery-point:does-not-exist")
}

/// The same, in another partition — a point this emulator could never hold.
fn other_partition_point() -> String {
    format!("arn:aws-cn:backup:{REGION}:{ACCOUNT}:recovery-point:1EB3B5E7")
}

/// The kind of job a refused start asked for: what its `detail-type` would have been, and
/// which of this service's three job stores it would have written to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Backup,
    Copy,
    Restore,
}

impl Kind {
    /// The `detail-type` AWS Backup publishes this kind of job's state changes under.
    fn detail_type(self) -> &'static str {
        match self {
            Self::Backup => BACKUP_JOB_STATE_CHANGE,
            Self::Copy => COPY_JOB_STATE_CHANGE,
            Self::Restore => RESTORE_JOB_STATE_CHANGE,
        }
    }

    /// How many jobs of this kind the service holds.
    ///
    /// Read off the store the handlers write rather than through a listing, because
    /// `ListCopyJobs` is scaffolded in this emulator and answers 501 for one of the three
    /// kinds: the store is the one seam every kind has.
    fn stored(self, engine: &engine::InMemoryEngine) -> usize {
        let state = engine.state::<BackupState>();
        let store = state.lock();
        match self {
            Self::Backup => store.backup_jobs.len(),
            Self::Copy => store.copy_jobs.len(),
            Self::Restore => store.restore_jobs.len(),
        }
    }
}

/// One refused start: the call, the refusal it earns, and what must still be true of the
/// state and of the bus afterwards.
struct Row {
    case: &'static str,
    /// The method and path the model binds the start to.
    method: &'static str,
    path: &'static str,
    body: Value,
    /// The modelled error the service answers, all of which the model gives HTTP 400.
    code: &'static str,
    /// The kind of job the refused start asked for — so the `detail-type` the bus would
    /// have carried, and the store that must still hold none.
    kind: Kind,
}

/// The rows: a refused start of each job kind, under each class of refusal this service
/// answers for one.
fn rows() -> Vec<Row> {
    /// The `Metadata` a DynamoDB restore carries: the table the restore would create.
    fn restore_metadata() -> Value {
        json!({"targetTableName": "restored"})
    }

    vec![
        Row {
            case: "a restore from a recovery-point ARN nothing answers to",
            method: "PUT",
            path: "/restore-jobs",
            body: json!({
                "RecoveryPointArn": fabricated_point(),
                "ResourceType": "DynamoDB",
                "Metadata": restore_metadata(),
                "IamRoleArn": role(),
            }),
            code: "InvalidParameterValueException",
            kind: Kind::Restore,
        },
        Row {
            case: "a restore from a recovery point of another partition",
            method: "PUT",
            path: "/restore-jobs",
            body: json!({
                "RecoveryPointArn": other_partition_point(),
                "ResourceType": "DynamoDB",
                "Metadata": restore_metadata(),
                "IamRoleArn": role(),
            }),
            code: "InvalidParameterValueException",
            kind: Kind::Restore,
        },
        Row {
            case: "a restore carrying no Metadata at all",
            method: "PUT",
            path: "/restore-jobs",
            body: json!({
                "RecoveryPointArn": fabricated_point(),
                "ResourceType": "DynamoDB",
                "IamRoleArn": role(),
            }),
            code: "MissingParameterValueException",
            kind: Kind::Restore,
        },
        Row {
            case: "a copy of a recovery-point ARN nothing answers to",
            method: "PUT",
            path: "/copy-jobs",
            body: json!({
                "RecoveryPointArn": fabricated_point(),
                "SourceBackupVaultName": SOURCE_VAULT,
                "DestinationBackupVaultArn": vault_arn(REGION, DESTINATION_VAULT),
                "IamRoleArn": role(),
            }),
            code: "ResourceNotFoundException",
            kind: Kind::Copy,
        },
        Row {
            case: "a copy naming no destination vault",
            method: "PUT",
            path: "/copy-jobs",
            body: json!({
                "RecoveryPointArn": fabricated_point(),
                "SourceBackupVaultName": SOURCE_VAULT,
                "IamRoleArn": role(),
            }),
            code: "MissingParameterValueException",
            kind: Kind::Copy,
        },
        Row {
            case: "a backup into a vault this account does not hold",
            method: "PUT",
            path: "/backup-jobs",
            body: json!({
                "BackupVaultName": "never-created",
                "ResourceArn": resource_arn(),
                "IamRoleArn": role(),
            }),
            code: "ResourceNotFoundException",
            kind: Kind::Backup,
        },
        Row {
            case: "a backup whose start window is below the documented minimum",
            method: "PUT",
            path: "/backup-jobs",
            body: json!({
                "BackupVaultName": SOURCE_VAULT,
                "ResourceArn": resource_arn(),
                "IamRoleArn": role(),
                // The member's doc: "if this value is included, it must be at least 60
                // minutes to avoid errors".
                "StartWindowMinutes": 1,
            }),
            code: "InvalidParameterValueException",
            kind: Kind::Backup,
        },
    ]
}

/// Both vaults a row may name, created through this service's own `CreateBackupVault`.
async fn given_vaults(router: &MultiServiceRouter, case: &str) {
    for name in [SOURCE_VAULT, DESTINATION_VAULT] {
        let (status, created) = create_vault(router, name, "{}").await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateBackupVault {name:?} answered {status} {created:?}"
        );
    }
}

/// The queue and the rule every case reads the bus through: one rule owed every kind of
/// `aws.backup` envelope, with that queue as its only target. Answers the queue's url.
async fn given_bus(router: &MultiServiceRouter, case: &str, queue: &str) -> String {
    let (url, arn) = given_queue(router, case, queue).await;
    given_rule(
        router,
        case,
        RULE,
        &json!({
            "source": [EVENT_SOURCE],
            "detail-type": [
                BACKUP_JOB_STATE_CHANGE,
                COPY_JOB_STATE_CHANGE,
                RESTORE_JOB_STATE_CHANGE,
            ],
        }),
        &[arn],
    )
    .await;
    url
}

/// Start an accepted backup of [`resource_arn`] into [`SOURCE_VAULT`], and answer the job
/// id and the recovery-point ARN the call reported.
async fn accepted_backup(router: &MultiServiceRouter, case: &str) -> (String, String) {
    let (status, started) = start_job(
        router,
        &json!({
            "BackupVaultName": SOURCE_VAULT,
            "ResourceArn": resource_arn(),
            "IamRoleArn": role(),
        })
        .to_string(),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: an accepted StartBackupJob answered {status} {started}"
    );
    (
        string_at(&started, "BackupJobId"),
        string_at(&started, "RecoveryPointArn"),
    )
}

/// Assert that `envelope` is the published terminal move of the backup job `job_id`, whose
/// recovery point is `point_arn`: the envelope AWS Backup owes an ACCEPTED start.
fn assert_control_envelope(case: &str, envelope: &Value, job_id: &str, point_arn: &str) {
    assert_eq!(
        (
            envelope["source"].clone(),
            envelope["detail-type"].clone(),
            envelope["resources"].clone(),
        ),
        (
            json!(EVENT_SOURCE),
            json!(BACKUP_JOB_STATE_CHANGE),
            json!([point_arn]),
        ),
        "case {case:?}: expected the accepted job's own envelope, got {envelope}"
    );
    assert_eq!(
        (
            envelope["detail"]["backupJobId"].clone(),
            envelope["detail"]["state"].clone(),
        ),
        (json!(job_id), json!(COMPLETED)),
        "case {case:?}: expected the accepted job's id and terminal state, got {envelope}"
    );
}

/// A start this service refuses stores no job of its kind and publishes no envelope, while
/// a start it accepted has its terminal move delivered to the very same rule and target.
#[tokio::test]
async fn a_refused_start_publishes_nothing_on_a_bus_an_accepted_one_reaches() {
    for row in rows() {
        let case = row.case;
        let (router, engine) = emulator_with_engine();
        given_vaults(&router, case).await;
        let url = given_bus(&router, case, "refused-start-events").await;

        // The refused start, made BEFORE the accepted one: anything it published would
        // therefore be queued ahead of the envelope waited for below.
        let (status, refused) =
            call(&router, row.method, row.path, Some(&row.body.to_string())).await;
        assert_eq!(
            (status, error_code(&refused).as_str()),
            (400, row.code),
            "case {case:?}: expected 400 {}, got {status} {refused:?}",
            row.code
        );
        assert_eq!(
            row.kind.stored(&engine),
            0,
            "case {case:?}: a refused start persists no {:?} job",
            row.kind
        );

        // The positive control: an accepted backup, whose terminal move the clock owes and
        // the bus is handed. Waited for by looking at the QUEUE again until it is there, so
        // the case waits for the publication rather than for a span.
        let (job_id, point_arn) = accepted_backup(&router, case).await;
        let received = delivered_at_least(&router, case, &url, 1).await;
        assert_eq!(
            received.len(),
            1,
            "case {case:?}: the accepted job's move is the ONE envelope the bus carries, got \
             {received:?}"
        );
        assert_control_envelope(case, &received[0], &job_id, &point_arn);
        // And the recovery point the envelope names is the one the job's own reader reports.
        let described = describe_backup_job(&router, &job_id).await;
        assert_eq!(
            received[0]["resources"],
            json!([described["RecoveryPointArn"].as_str().unwrap_or_default()]),
            "case {case:?}: the envelope names the recovery point DescribeBackupJob reports, \
             got {} against {described}",
            received[0]
        );
        assert!(
            of_type(&received, row.kind.detail_type())
                .iter()
                .all(|envelope| envelope["detail"]["backupJobId"] == json!(job_id)),
            "case {case:?}: no envelope of {:?} belongs to the refused start, got {received:?}",
            row.kind.detail_type()
        );
    }
}

/// The same rule, asserted without depending on the order two publications could take:
/// every refused start above is made BETWEEN two accepted backups, and the only envelopes
/// that ever arrive are those two jobs' own moves — so nothing was published for any of the
/// refusals, on a path that was live both before and after them.
#[tokio::test]
async fn nothing_arrives_between_two_accepted_jobs_for_the_starts_that_were_refused() {
    let case = "refusals between two accepted jobs";
    let (router, engine) = emulator_with_engine();
    given_vaults(&router, case).await;
    let url = given_bus(&router, case, "refusal-interleaved-events").await;

    // The first accepted job, and its envelope: the path is live before any refusal.
    let (first_id, first_point) = accepted_backup(&router, case).await;
    let mut received = delivered_at_least(&router, case, &url, 1).await;
    assert_eq!(
        received.len(),
        1,
        "case {case:?}: the first accepted job owes exactly one envelope, got {received:?}"
    );
    assert_control_envelope(case, &received[0], &first_id, &first_point);

    for row in rows() {
        let (status, refused) =
            call(&router, row.method, row.path, Some(&row.body.to_string())).await;
        assert_eq!(
            (status, error_code(&refused).as_str()),
            (400, row.code),
            "case {:?}: expected 400 {}, got {status} {refused:?}",
            row.case,
            row.code
        );
        // The accepted backups are the only jobs of any kind this emulator holds: a copy
        // and a restore store nothing at all, and the backup store holds just the first
        // accepted job.
        assert_eq!(
            (
                Kind::Copy.stored(&engine),
                Kind::Restore.stored(&engine),
                Kind::Backup.stored(&engine),
            ),
            (0, 0, 1),
            "case {:?}: a refused start persists no job",
            row.case
        );
    }

    // The second accepted job: its envelope is the NEXT thing the queue hands over, which
    // is only true if the refusals in between published nothing.
    let (second_id, second_point) = accepted_backup(&router, case).await;
    let after = delivered_at_least(&router, case, &url, 1).await;
    received.extend(after);
    assert_eq!(
        received.len(),
        2,
        "case {case:?}: the two accepted jobs' moves are the only envelopes the bus carries, \
         got {received:?}"
    );
    assert_control_envelope(case, &received[1], &second_id, &second_point);

    // Nothing else lands afterwards either: the drain that follows the second envelope is
    // empty, so no refusal's publication arrived late.
    let trailing = delivered(&router, case, &url).await;
    assert!(
        trailing.is_empty(),
        "case {case:?}: no further envelope is owed, got {trailing:?}"
    );
}
