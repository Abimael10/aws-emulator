// SPDX-License-Identifier: CC0-1.0

//! A restore job's state change reaches a rule's SQS target only where that QUEUE's own
//! resource policy admits the delivery — and the envelope it hands over is the job's own
//! stored state.
//!
//! The RULE under test: every status a stored restore job MOVES INTO after the call that
//! started it is published onto the account's default EventBridge bus as `source`
//! `aws.backup` with `detail-type` `Restore Job State Change`, carrying that job's own stored
//! members as the event `detail` — the values `DescribeRestoreJob` reports for the same job —
//! and the recovery point the restore read FROM in `resources`; an ENABLED rule whose
//! `EventPattern` matches hands that envelope to EVERY one of its targets, and for an SQS
//! queue target EventBridge makes the `sqs:SendMessage` as its own service principal and
//! relies on the queue's resource policy for it, so the delivery happens exactly where that
//! document states an `Allow` about it: a grant scoped by `aws:SourceArn` authorizes the
//! deliveries of the rule it names and no other, an explicit `Deny` refuses whatever a grant
//! allows, and a queue carrying no policy at all has granted nothing.
//!
//! Sources. The envelope is in no Smithy model — AWS Backup's EventBridge notifications carry
//! `"source": "aws.backup"` with `"detail-type": "Restore Job State Change"` — so every value
//! asserted here is compared against what the control plane answers for the same job
//! (`smithy_query(query="operation", operation="DescribeRestoreJob",
//! model_path="services/service-com-amazonaws-backup-cryocontrollerusermanager/smithy-model/com-amazonaws-backup-cryocontrollerusermanager.json")`,
//! whose `Status` is the enum `RestoreJobStatus`: `PENDING`/`RUNNING`/`COMPLETED`/`ABORTED`/
//! `FAILED`). That a queue target's delivery is decided by the queue's own resource policy is
//! what `PutTargets` documents ("for Lambda functions, Amazon SNS topics and Amazon SQS
//! queues, EventBridge relies on resource-based policies").
//!
//! What the rows differ along is the FORM that grant takes on the target queue, which is the
//! dimension `restore_job_state_change_events.rs` holds fixed while it spans the rule patterns
//! instead: an unconditioned grant, a grant scoped by `aws:SourceArn` to the delivering rule,
//! a grant scoped by a wildcard over this account's rules, a grant scoped to ANOTHER rule, a
//! grant beside an explicit deny of the same delivery, and no policy at all. Every case's rule
//! also targets a CONTROL queue whose grant is unconditioned, so a row's own queue holding
//! nothing is the policy withholding the delivery rather than a look taken before the job
//! moved.
//!
//! What the cases select on the bus is the envelope carrying a TERMINAL `RestoreJobStatus`,
//! which is what a client watching a restore does: a lifecycle that publishes a non-terminal
//! status first is looked past rather than asserted about.
//!
//! Every case drives real HTTP through the router: the Backup wire is the shared harness in
//! `vault_and_backup_job.rs`, and DynamoDB, EventBridge and SQS are mounted beside Backup on
//! the one shared engine (`restore_job_state_change_events.rs` owns that mounting), because
//! the restore creates its table through DynamoDB's own API and the publication reaches
//! EventBridge through the in-process callback, whose rule delivers to the queues.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::job_state_change_events::{
    delivered, events_in, given_queue, given_rule, given_unpolicied_queue, set_queue_policy,
};
use crate::recovery_point_deletion::completed_point;
use crate::restore_job::source_table;
use crate::restore_job_state_change_events::{describe_restore, emulator_with_engine, start_restore};
use crate::vault_and_backup_job::{create_vault, ACCOUNT, REGION};

/// The `source` and `detail-type` AWS Backup publishes a restore job's moves under, spelled
/// out here rather than imported, so a wrong constant in the implementation cannot make a
/// test pass.
const EVENT_SOURCE: &str = "aws.backup";
const RESTORE_JOB_STATE_CHANGE: &str = "Restore Job State Change";

/// The `RestoreJobStatus` values that END a restore job's lifecycle (model enum
/// `RestoreJobStatus`), and the one a performed restore reaches: a client watching the bus
/// selects the envelope carrying a terminal status, so these are what a case waits for.
const TERMINAL_STATUSES: [&str; 3] = ["COMPLETED", "ABORTED", "FAILED"];
const COMPLETED: &str = "COMPLETED";

/// The service principal EventBridge delivers a rule's target as, and the condition key a
/// grant written for it is scoped by: the ARN of the RULE the delivery was made for.
const EVENTS_PRINCIPAL: &str = "events.amazonaws.com";
const SOURCE_ARN_KEY: &str = "aws:SourceArn";

/// The vault every case backs its table up into.
const VAULT: &str = "restore-grant-source";

/// The rule every case installs, and the rule a grant scoped to ANOTHER rule names. The
/// second need not exist: `aws:SourceArn` is a value a statement is written against, not a
/// resource the policy resolves.
const RULE: &str = "restore-grant-events";
const ANOTHER_RULE: &str = "some-other-rule";

/// The table a case backs up, and the table its restore creates.
const SOURCE_TABLE: &str = "orders";
const RESTORED_TABLE: &str = "orders-restored";

/// The Backup resource type of the recovery point every case restores.
const DYNAMODB: &str = "DynamoDB";

/// How long a case looks for the envelope the clock owes before failing, and how long each
/// look waits before the next.
///
/// Generous on purpose: the transition falls due on the emulator's own clock, so the wait is
/// for the ENVELOPE and this deadline only bounds a move that is never published at all — a
/// loaded or a slow host still sees one that is.
const POLL_DEADLINE: Duration = Duration::from_secs(60);
const POLL_WAIT: Duration = Duration::from_millis(100);

/// The pattern the rule filters on: the restore-job envelopes of AWS Backup.
fn pattern() -> Value {
    json!({ "source": [EVENT_SOURCE], "detail-type": [RESTORE_JOB_STATE_CHANGE] })
}

/// A queue policy granting the delivering service the send on `queue_arn`, under `condition`
/// when one is given — the document a client writes for a rule's queue target.
fn grant(queue_arn: &str, condition: Option<Value>) -> Value {
    let mut statement = json!({
        "Effect": "Allow",
        "Principal": { "Service": EVENTS_PRINCIPAL },
        "Action": "sqs:SendMessage",
        "Resource": queue_arn,
    });
    if let Some(condition) = condition {
        statement["Condition"] = condition;
    }
    json!({ "Version": "2012-10-17", "Statement": [statement] })
}

/// An `aws:SourceArn` condition under `operator` naming `arn`.
fn scoped(operator: &str, arn: &str) -> Value {
    json!({ operator: { SOURCE_ARN_KEY: arn } })
}

/// The ARN of a rule of this account in this Region, as `PutRule` answers it for a rule of
/// the default bus.
fn rule_arn_of(name: &str) -> String {
    format!("arn:aws:events:{REGION}:{ACCOUNT}:rule/{name}")
}

/// One row: the document the target queue carries, and whether it admits this rule's
/// delivery.
struct Row {
    case: &'static str,
    /// The policy to put on the row's queue, given that queue's ARN and the ARN EventBridge
    /// minted for the delivering rule — `None` for a queue that carries no policy at all.
    policy: fn(&str, &str) -> Option<Value>,
    admitted: bool,
}

/// The rows: the grant forms a client writes on a queue a rule targets.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "an unconditioned grant to the delivering service",
            policy: |queue_arn, _rule_arn| Some(grant(queue_arn, None)),
            admitted: true,
        },
        Row {
            case: "a grant scoped by aws:SourceArn to the delivering rule",
            policy: |queue_arn, rule_arn| {
                Some(grant(queue_arn, Some(scoped("ArnEquals", rule_arn))))
            },
            admitted: true,
        },
        Row {
            case: "a grant scoped by a wildcard over this account's rules",
            policy: |queue_arn, _rule_arn| {
                Some(grant(
                    queue_arn,
                    Some(scoped("ArnLike", &rule_arn_of("*"))),
                ))
            },
            admitted: true,
        },
        Row {
            case: "a grant scoped by aws:SourceArn to another rule",
            policy: |queue_arn, _rule_arn| {
                Some(grant(
                    queue_arn,
                    Some(scoped("ArnEquals", &rule_arn_of(ANOTHER_RULE))),
                ))
            },
            admitted: false,
        },
        Row {
            case: "a grant beside an explicit deny of the same delivery",
            policy: |queue_arn, _rule_arn| {
                let mut document = grant(queue_arn, None);
                document["Statement"]
                    .as_array_mut()
                    .expect("the grant states its Statement as an array")
                    .push(json!({
                        "Effect": "Deny",
                        "Principal": { "Service": EVENTS_PRINCIPAL },
                        "Action": "sqs:SendMessage",
                        "Resource": queue_arn,
                    }));
                Some(document)
            },
            admitted: false,
        },
        Row {
            case: "no policy at all",
            policy: |_queue_arn, _rule_arn| None,
            admitted: false,
        },
    ]
}

/// A queue target whose own policy admits this rule's delivery is handed the restore job's
/// envelope, which is the one the control queue was handed.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_queue_target_granting_the_delivery_is_handed_the_restore_envelope() {
    for row in rows().into_iter().filter(|row| row.admitted) {
        observed(&row).await;
    }
}

/// A queue target whose own policy does not admit this rule's delivery is handed nothing,
/// while the control queue of the same rule is handed the envelope.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_queue_target_withholding_the_delivery_is_handed_nothing() {
    for row in rows().into_iter().filter(|row| !row.admitted) {
        observed(&row).await;
    }
}

/// Drive one row: back a table up, restore it, and compare what each of the rule's two
/// queue targets was handed with what the control plane reports for the same job.
async fn observed(row: &Row) {
    let case = row.case;
    let (router, _engine) = emulator_with_engine();
    let table_arn = source_table(&router, SOURCE_TABLE, false).await;
    let (status, created) = create_vault(&router, VAULT, "{}").await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateBackupVault answered {status} {created}"
    );

    // Both queues are targets of the SAME rule: the control's grant is unconditioned, and
    // the row's is whatever the row states — so the two differ in the document alone.
    let (control_url, control_arn) = given_queue(&router, case, "restore-grant-control").await;
    let (row_url, row_queue_arn) = given_unpolicied_queue(&router, case, "restore-grant-row").await;
    given_rule(
        &router,
        case,
        RULE,
        &pattern(),
        &[control_arn, row_queue_arn.clone()],
    )
    .await;
    // The grant is written against the ARN EventBridge itself minted for the rule, read back
    // through that service's own read rather than spelled a second time here.
    let rule_arn = installed_rule_arn(&router, case).await;
    assert_eq!(
        rule_arn,
        rule_arn_of(RULE),
        "case {case:?}: expected the rule ARN of this account and Region, got {rule_arn:?}"
    );
    if let Some(policy) = (row.policy)(&row_queue_arn, &rule_arn) {
        set_queue_policy(&router, case, &row_url, &policy.to_string()).await;
    }

    let point = completed_point(&router, VAULT, &table_arn).await;
    let job_id = start_restore(&router, case, &point, DYNAMODB, RESTORED_TABLE).await;

    // The move falls due on the emulator's own clock, so the bus is read with no further
    // Backup call — the wait is for the ENVELOPE, not for a span.
    let (envelope, control) = terminal_envelope(&router, case, &control_url).await;
    assert_eq!(
        envelope["source"],
        json!(EVENT_SOURCE),
        "case {case:?}: the envelope names AWS Backup as its source, got {envelope}"
    );
    assert_eq!(
        envelope["resources"],
        json!([point]),
        "case {case:?}: the envelope names the recovery point the restore read, got {envelope}"
    );

    // The detail is the job's own stored state: every member is the one the read of the same
    // job reports.
    let read = describe_restore(&router, case, &job_id).await;
    let detail = &envelope["detail"];
    for (member, reported) in [
        ("restoreJobId", "RestoreJobId"),
        ("status", "Status"),
        ("recoveryPointArn", "RecoveryPointArn"),
        ("sourceResourceArn", "SourceResourceArn"),
        ("createdResourceArn", "CreatedResourceArn"),
        ("resourceType", "ResourceType"),
    ] {
        assert_eq!(
            detail[member], read[reported],
            "case {case:?}: detail.{member} must be what DescribeRestoreJob reports as \
             {reported}, got {detail} against {read}"
        );
    }
    assert_eq!(
        (detail["restoreJobId"].clone(), detail["status"].clone()),
        (json!(job_id), json!(COMPLETED)),
        "case {case:?}: a performed restore ends in the completed status of the job the start \
         answered with, got {detail}"
    );
    assert_eq!(
        detail["sourceResourceArn"],
        json!(table_arn),
        "case {case:?}: detail.sourceResourceArn must be the resource the point was taken \
         of, got {detail}"
    );
    assert_eq!(
        detail["resourceType"],
        json!(DYNAMODB),
        "case {case:?}: detail.resourceType must be the type the restore was of, got {detail}"
    );
    assert!(
        detail["createdResourceArn"].is_string(),
        "case {case:?}: a completed restore reports the resource it created, got {detail}"
    );

    if row.admitted {
        let (same, _) = terminal_envelope(&router, case, &row_url).await;
        for member in ["source", "detail-type", "resources", "detail"] {
            assert_eq!(
                same[member], envelope[member],
                "case {case:?}: every target of the rule is handed the envelope unchanged, \
                 so {member} must agree, got {same} against {envelope}"
            );
        }
    } else {
        // The control's envelope above is the positive control for this silence: the move WAS
        // published and dispatched, and this queue's own document withheld the delivery.
        let leaked = delivered(&router, case, &row_url).await;
        assert!(
            leaked.is_empty(),
            "case {case:?}: a queue whose policy does not admit this delivery is handed \
             nothing, got {leaked:?} (control: {control:?})"
        );
    }
}

/// The restore-job envelope carrying a TERMINAL status on the queue at `url`, looked for
/// again until one arrives or until [`POLL_DEADLINE`] passes, with every envelope the queue
/// handed over beside it.
///
/// What a client watching a restore selects: an envelope of a non-terminal status is looked
/// past, so a lifecycle that publishes one first is not what this waits on. Each attempt
/// awaits, which is what lets the in-process clock pump make progress beside the test, and a
/// drain takes the messages it reports off the queue, so what each attempt collected is kept.
async fn terminal_envelope(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
) -> (Value, Vec<Value>) {
    let deadline = Instant::now() + POLL_DEADLINE;
    let mut bodies: Vec<Value> = Vec::new();
    loop {
        bodies.extend(delivered(router, case, url).await);
        if let Some(found) = bodies.iter().find(|event| is_terminal_restore(event)) {
            let found = found.clone();
            return (found, bodies);
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected a terminal {RESTORE_JOB_STATE_CHANGE:?} envelope on \
             {url:?} within {POLL_DEADLINE:?}, got {bodies:?}"
        );
        ::tokio::time::sleep(POLL_WAIT).await;
    }
}

/// Whether `event` is a restore job's envelope reporting a status that ends its lifecycle.
fn is_terminal_restore(event: &Value) -> bool {
    event["detail-type"] == json!(RESTORE_JOB_STATE_CHANGE)
        && TERMINAL_STATUSES
            .iter()
            .any(|status| event["detail"]["status"] == json!(*status))
}

/// The ARN EventBridge reports for the installed rule, read through `DescribeRule`.
async fn installed_rule_arn(router: &MultiServiceRouter, case: &str) -> String {
    let (status, described) =
        events_in(router, REGION, "DescribeRule", json!({ "Name": RULE })).await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeRule answered {status} {described}"
    );
    described["Arn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no Arn in {described}"))
        .to_string()
}
