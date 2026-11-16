// SPDX-License-Identifier: CC0-1.0

//! What AWS Backup answers, stores and publishes for a restore it REFUSES.
//!
//! The RULE under test: `StartRestoreJob` resolves its `RecoveryPointArn` against this
//! account's recovery points DURING the synchronous call, so a value that resolves to no
//! stored point — however it fails to resolve — is refused on the spot with one of that
//! operation's modelled client errors (HTTP 400), stores no restore job, and therefore
//! announces no `Restore Job State Change` on the account's default event bus; only a state
//! a STORED job moves into is published. The refusal is a value refusal
//! (`InvalidParameterValueException`) carrying the condition's fixed sentence, so a client
//! can branch on "the recovery point is gone" rather than on an opaque failure.
//!
//! This suite is the two halves of that rule asserted TOGETHER, which neither sibling does:
//! `restore_job.rs` pins the refusal classes and the empty store, and
//! `restore_job_state_change_events.rs` pins what an ACCEPTED restore publishes, but a
//! client that watches the bus for restore events needs the refused call to be a 400 AND the
//! bus to stay silent for it. The negative half is only worth anything beside a live
//! positive half, so the same rule, the same target and the same queue are then handed an
//! accepted restore's own envelope: the silence above is a publication withheld, not a
//! pattern filtering or a bus nothing ever reaches.
//!
//! The rows span the ways a `RecoveryPointArn` can fail to resolve — an ARN of the right
//! form that names no point, one of another partition, a point this account held and
//! deleted, and a value that is not an ARN at all — rather than one fabricated ARN written
//! four times.
//!
//! Every case drives real HTTP through the router: Backup, DynamoDB, EventBridge and SQS on
//! the one shared engine of `restore_job_state_change_events.rs`, because the accepted
//! restore creates its table through DynamoDB's own API and the publication reaches
//! EventBridge through the in-process callback, whose rule delivers to an SQS queue target.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::job_state_change_events::{
    delivered, delivered_of_type, given_queue, given_rule, of_type, role,
    BACKUP_JOB_STATE_CHANGE, EVENT_SOURCE,
};
use crate::recovery_point_deletion::completed_point;
use crate::restore_job::{source_table, stored_restore_jobs};
use crate::restore_job_state_change_events::emulator_with_engine;
use crate::vault_and_backup_job::{call, create_vault, error_code, string_at, ACCOUNT, REGION};

/// The `detail-type` AWS Backup publishes a restore job's state changes under, spelled out
/// here rather than imported, so a wrong constant in the implementation cannot make a test
/// pass. Source: the EventBridge reference for AWS Backup, whose restore job events carry
/// `"source": "aws.backup"` with `"detail-type": "Restore Job State Change"`.
const RESTORE_JOB_STATE_CHANGE: &str = "Restore Job State Change";

/// The modelled client errors `StartRestoreJob` may refuse a recovery-point reference with,
/// as the operation's own error set names them — each HTTP 400. Source: the Smithy model's
/// `StartRestoreJob` errors (`InvalidParameterValueException`, `InvalidRequestException`,
/// `MissingParameterValueException`, `ResourceNotFoundException`), less the one for a member
/// that was not sent at all.
const RECOVERY_POINT_REFUSAL_CODES: [&str; 3] = [
    "InvalidParameterValueException",
    "InvalidRequestException",
    "ResourceNotFoundException",
];

/// The sentence this service states for "the recovery point named is not one this account
/// holds" — fixed text, naming no part of the request.
const RECOVERY_POINT_NOT_FOUND: &str = "Cannot find recovery point";

/// The vaults the cases use: the one the restorable point is taken into, and the one whose
/// point is deleted again. Both inside the modelled `BackupVaultName` pattern,
/// `^[a-zA-Z0-9\-\_]{2,50}$`.
const VAULT: &str = "refused-restore-source";
const DELETED_POINT_VAULT: &str = "refused-restore-deleted";

/// The DynamoDB table the backups are taken of, and the table the accepted restore creates.
const SOURCE_TABLE: &str = "ledger";
const TARGET_TABLE: &str = "ledger-restored";

/// The rule every case reads the bus through: one rule owed BOTH job detail-types this
/// suite can see, so an absent restore envelope cannot be a pattern that filtered it.
const RULE: &str = "restore-refusal-events";

/// One way a `RecoveryPointArn` fails to resolve, and what the refusal must say.
struct Row {
    case: &'static str,
    recovery_point_arn: String,
    /// Whether this row is the "no such stored point" condition, whose sentence is fixed —
    /// a value that is not an ARN at all is the same class with its own sentence, so only
    /// its code and the empty store are asserted.
    fixed_sentence: bool,
}

/// The rows: the ways an ARN can name no stored recovery point.
fn rows(deleted_point_arn: &str) -> Vec<Row> {
    vec![
        Row {
            case: "an ARN of the right form that nothing answers to",
            recovery_point_arn: format!(
                "arn:aws:backup:{REGION}:{ACCOUNT}:recovery-point:7c1d9a52-4f03-4b7a-9e61-2d8b5fae01c4"
            ),
            fixed_sentence: true,
        },
        Row {
            case: "an ARN of another partition",
            recovery_point_arn: format!(
                "arn:aws-us-gov:backup:{REGION}:{ACCOUNT}:recovery-point:3a0f17bd-55c2-4f9e-8a74-6b1c0d2e9f88"
            ),
            fixed_sentence: true,
        },
        Row {
            case: "a point this account held and deleted",
            recovery_point_arn: deleted_point_arn.to_string(),
            fixed_sentence: true,
        },
        Row {
            case: "a value that is not an ARN at all",
            recovery_point_arn: "ledger-backup-from-yesterday".to_string(),
            fixed_sentence: false,
        },
    ]
}

/// A `StartRestoreJob` body for `recovery_point_arn`, carrying the metadata a DynamoDB
/// restore needs (the table it would create) and the role it would run as — so every refusal
/// below is about the recovery point and about nothing else in the request.
fn start_body(recovery_point_arn: &str) -> String {
    json!({
        "RecoveryPointArn": recovery_point_arn,
        "ResourceType": "DynamoDB",
        "Metadata": {"targetTableName": TARGET_TABLE},
        "IamRoleArn": role(),
    })
    .to_string()
}

/// `PUT /restore-jobs`, as the model binds `StartRestoreJob`.
async fn start_restore(router: &MultiServiceRouter, recovery_point_arn: &str) -> (u16, Value) {
    call(
        router,
        "PUT",
        "/restore-jobs",
        Some(&start_body(recovery_point_arn)),
    )
    .await
}

/// How many restore jobs `ListRestoreJobs` reports: the same "nothing was stored" read a
/// client can make, beside the store the handler writes.
async fn listed_restore_jobs(router: &MultiServiceRouter, case: &str) -> usize {
    let (status, listed) = call(router, "GET", "/restore-jobs", None).await;
    assert_eq!(
        status, 200,
        "case {case:?}: ListRestoreJobs answered {status} {listed:?}"
    );
    listed
        .get("RestoreJobs")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or_default()
}

/// A restore from a recovery point that does not resolve is refused synchronously with a
/// modelled 400, stores no restore job and publishes no `Restore Job State Change` — while
/// an accepted restore's own move reaches the very same rule and target.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_refused_restore_is_a_modelled_400_that_publishes_no_restore_event() {
    let case = "a refused restore";
    let (router, engine) = emulator_with_engine();
    let table_arn = source_table(&router, SOURCE_TABLE, false).await;
    for vault in [VAULT, DELETED_POINT_VAULT] {
        let (status, created) = create_vault(&router, vault, "{}").await;
        assert_eq!(
            status, 200,
            "setup {vault:?}: CreateBackupVault answered {status} {created:?}"
        );
    }

    // The rule and its target are installed BEFORE anything is started, so every envelope
    // either kind of job owes from here on is delivered to this one queue.
    let (url, queue_arn) = given_queue(&router, case, "refused-restore-events").await;
    given_rule(
        &router,
        case,
        RULE,
        &json!({
            "source": [EVENT_SOURCE],
            "detail-type": [RESTORE_JOB_STATE_CHANGE, BACKUP_JOB_STATE_CHANGE],
        }),
        &[queue_arn],
    )
    .await;

    // Two finished recovery points: the one the accepted restore reads, and the one that is
    // deleted again so a row can name a point this account genuinely held.
    let restorable = completed_point(&router, VAULT, &table_arn).await;
    let deleted_point = completed_point(&router, DELETED_POINT_VAULT, &table_arn).await;
    let (status, deleted) = call(
        &router,
        "DELETE",
        &format!("/backup-vaults/{DELETED_POINT_VAULT}/recovery-points/{deleted_point}"),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: the point to be deleted should be deletable, got {status} {deleted:?}"
    );

    for row in rows(&deleted_point) {
        let case = row.case;
        let (status, refused) = start_restore(&router, &row.recovery_point_arn).await;
        let code = error_code(&refused);
        assert!(
            status == 400 && RECOVERY_POINT_REFUSAL_CODES.contains(&code.as_str()),
            "case {case:?} ({}): expected one of {RECOVERY_POINT_REFUSAL_CODES:?} with HTTP \
             400, got {status} {refused:?}",
            row.recovery_point_arn
        );
        if row.fixed_sentence {
            assert_eq!(
                (code.as_str(), string_at(&refused, "Message").as_str()),
                ("InvalidParameterValueException", RECOVERY_POINT_NOT_FOUND),
                "case {case:?} ({}): expected the recovery-point sentence, got {refused:?}",
                row.recovery_point_arn
            );
        }
        assert_eq!(
            (stored_restore_jobs(&engine), listed_restore_jobs(&router, case).await),
            (0, 0),
            "case {case:?} ({}): a refused start persists no restore job",
            row.recovery_point_arn
        );
    }

    // Nothing any of those refusals published is waiting on the queue. Read BEFORE the
    // accepted restore, so an envelope a refusal had published would be queued ahead of the
    // accepted job's own.
    let mut received = delivered(&router, case, &url).await;
    assert!(
        of_type(&received, RESTORE_JOB_STATE_CHANGE).is_empty(),
        "case {case:?}: a refused restore publishes no {RESTORE_JOB_STATE_CHANGE} envelope, \
         got {received:?}"
    );

    // The positive half: the restorable point's own ARN is accepted, one job is stored, and
    // its move reaches this rule's target — so the silence above is a withheld publication.
    let (status, started) = start_restore(&router, &restorable).await;
    assert_eq!(
        (status, error_code(&started).as_str()),
        (200, "<none>"),
        "the restorable point's own ARN must be accepted, got {status} {started:?}"
    );
    let job_id = string_at(&started, "RestoreJobId");
    assert_eq!(
        (stored_restore_jobs(&engine), listed_restore_jobs(&router, case).await),
        (1, 1),
        "the accepted start must persist exactly one restore job"
    );
    received.extend(delivered_of_type(&router, case, &url, RESTORE_JOB_STATE_CHANGE, 1).await);
    let restore_envelopes = of_type(&received, RESTORE_JOB_STATE_CHANGE);
    assert!(
        !restore_envelopes.is_empty(),
        "case {case:?}: the accepted restore's move must reach the rule's target, got \
         {received:?}"
    );
    for envelope in &restore_envelopes {
        assert_eq!(
            (
                envelope["detail"]["restoreJobId"].clone(),
                envelope["resources"].clone(),
            ),
            (json!(job_id), json!([restorable.clone()])),
            "case {case:?}: every {RESTORE_JOB_STATE_CHANGE} envelope belongs to the ACCEPTED \
             job and names its recovery point, got {envelope}"
        );
    }
}
