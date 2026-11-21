// SPDX-License-Identifier: CC0-1.0

//! `PutBackupVaultNotifications` / `GetBackupVaultNotifications` /
//! `DeleteBackupVaultNotifications`: a vault's notification configuration is a
//! STORED member of the vault, and every one of the three calls answers from
//! that one member.
//!
//! The rules under test:
//!
//! * the write stores the topic and the event list on the named vault, and the
//!   read reports them back beside that vault's own name and ARN, in the order
//!   they were stated;
//! * a vault carries at most ONE configuration, so a later accepted write
//!   replaces the topic and the events before it;
//! * a vault carrying none has none to report: the read refuses it 400
//!   `ResourceNotFoundException`, which is also what it answers after the
//!   delete has cleared the member;
//! * the delete states the end state, so it succeeds whether or not the vault
//!   was configured;
//! * `SNSTopicArn` names a topic by ARN, so a value that is not an ARN is
//!   refused 400 `InvalidParameterValueException` and NOTHING is configured;
//! * all three decide access before existence, so a vault this account does not
//!   hold in the request's Region is refused 403 `AccessDeniedException`.
//!
//! The round-trip rows differ along what the rule generalises over — how many
//! events are named, which ones, and in what order — rather than along the name
//! of the vault. Every case drives real HTTP through the router; the harness is
//! the shared one in `vault_and_backup_job.rs`.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::vault_and_backup_job::{
    call, call_in_region, create_vault, create_vault_in, error_code, router, string_at, vault_arn,
    ACCOUNT, REGION,
};

/// The sentence AWS states for "the vault named carries no notification
/// configuration to report" — fixed text, naming neither the vault nor the
/// account.
const NO_NOTIFICATIONS: &str = "Failed reading notifications from database for Backup vault";

/// The sentence this service refuses a member whose value it cannot accept
/// with, for the member that carries the topic.
const TOPIC_INVALID: &str = "Parameter SNSTopicArn is invalid.";

/// Every token the model's `BackupVaultEvent` enum lists, in model order
/// (`smithy_query(query="shape", shape="BackupVaultEvent")`): the catalog a
/// caller may draw an event list from, so a case can be written on an entry the
/// issue never cited.
const BACKUP_VAULT_EVENTS: [&str; 30] = [
    "BACKUP_JOB_STARTED",
    "BACKUP_JOB_COMPLETED",
    "BACKUP_JOB_SUCCESSFUL",
    "BACKUP_JOB_FAILED",
    "BACKUP_JOB_EXPIRED",
    "RESTORE_JOB_STARTED",
    "RESTORE_JOB_COMPLETED",
    "RESTORE_JOB_SUCCESSFUL",
    "RESTORE_JOB_FAILED",
    "COPY_JOB_STARTED",
    "COPY_JOB_SUCCESSFUL",
    "COPY_JOB_FAILED",
    "RECOVERY_POINT_MODIFIED",
    "BACKUP_PLAN_CREATED",
    "BACKUP_PLAN_MODIFIED",
    "S3_BACKUP_OBJECT_FAILED",
    "S3_RESTORE_OBJECT_FAILED",
    "CONTINUOUS_BACKUP_INTERRUPTED",
    "RECOVERY_POINT_INDEX_COMPLETED",
    "RECOVERY_POINT_INDEX_DELETED",
    "RECOVERY_POINT_INDEXING_FAILED",
    "EKS_RESTORE_OBJECT_FAILED",
    "EKS_RESTORE_OBJECT_SKIPPED",
    "EKS_BACKUP_OBJECT_FAILED",
    "ACCESS_POINT_AVAILABLE",
    "ACCESS_POINT_CREATION_FAILED",
    "ACCESS_POINT_DELETED",
    "ACCESS_POINT_DELETION_FAILED",
    "ACCESS_POINT_EXPIRED",
    "ACCESS_POINT_DISASSOCIATED",
];

/// The ARN of an SNS topic of this account, in the form the model's own doc
/// gives for the member ("for example,
/// `arn:aws:sns:us-west-2:111122223333:MyVaultTopic`").
fn topic_arn(name: &str) -> String {
    format!("arn:aws:sns:{REGION}:{ACCOUNT}:{name}")
}

/// `PUT /backup-vaults/{BackupVaultName}/notification-configuration`, as the
/// model binds the write. The body is built here so a case can send a topic
/// that is not an ARN at all.
async fn put_notifications(
    router: &MultiServiceRouter,
    vault: &str,
    topic: &str,
    events: &[&str],
) -> (u16, Value) {
    call(
        router,
        "PUT",
        &format!("/backup-vaults/{vault}/notification-configuration"),
        Some(&json!({"SNSTopicArn": topic, "BackupVaultEvents": events}).to_string()),
    )
    .await
}

/// `GET /backup-vaults/{BackupVaultName}/notification-configuration`, as the
/// model binds the read.
async fn get_notifications(router: &MultiServiceRouter, vault: &str) -> (u16, Value) {
    call(
        router,
        "GET",
        &format!("/backup-vaults/{vault}/notification-configuration"),
        None,
    )
    .await
}

/// `DELETE /backup-vaults/{BackupVaultName}/notification-configuration`, as the
/// model binds the delete.
async fn delete_notifications(router: &MultiServiceRouter, vault: &str) -> (u16, Value) {
    call(
        router,
        "DELETE",
        &format!("/backup-vaults/{vault}/notification-configuration"),
        None,
    )
    .await
}

/// The `BackupVaultEvents` a read reports, as a list of strings.
fn reported_events(body: &Value) -> Vec<String> {
    body.get("BackupVaultEvents")
        .and_then(Value::as_array)
        .map(|events| {
            events
                .iter()
                .map(|event| event.as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// Create the vault a case configures.
async fn vault_named(router: &MultiServiceRouter, name: &str) {
    let (status, created) = create_vault(router, name, "{}").await;
    assert_eq!(
        status, 200,
        "setup: CreateBackupVault {name} should succeed, got {status} {created:?}"
    );
}

/// An accepted configuration round-trips: the read reports the topic and the
/// events that were stated, beside the vault they were stated for.
///
/// The rows differ along the event list — how many events, which of the
/// model's tokens, and in what order — because that is what the rule
/// generalises over.
#[tokio::test]
async fn a_stored_notification_configuration_round_trips_through_the_read() {
    let last_event = BACKUP_VAULT_EVENTS[BACKUP_VAULT_EVENTS.len() - 1];
    let cases: Vec<(&str, &str, Vec<&str>)> = vec![
        (
            "one event",
            "one-event-topic",
            vec!["BACKUP_JOB_COMPLETED"],
        ),
        (
            "several events",
            "several-events-topic",
            vec!["BACKUP_JOB_STARTED", "BACKUP_JOB_FAILED", "COPY_JOB_FAILED"],
        ),
        (
            "the same events in another order",
            "reordered-topic",
            vec!["COPY_JOB_FAILED", "BACKUP_JOB_FAILED", "BACKUP_JOB_STARTED"],
        ),
        (
            "an event the issue did not cite",
            "late-catalog-topic",
            vec![last_event],
        ),
        (
            "every event the model lists",
            "all-events-topic",
            BACKUP_VAULT_EVENTS.to_vec(),
        ),
        (
            "no events at all",
            "no-events-topic",
            Vec::new(),
        ),
    ];

    for (case, topic, events) in cases {
        let router = router();
        vault_named(&router, "notified-vault").await;

        let topic = topic_arn(topic);
        let (status, accepted) = put_notifications(&router, "notified-vault", &topic, &events).await;
        assert_eq!(
            (status, error_code(&accepted).as_str()),
            (200, "<none>"),
            "case {case:?}: expected PutBackupVaultNotifications 200, got {status} {accepted:?}"
        );

        let (status, read) = get_notifications(&router, "notified-vault").await;
        assert_eq!(
            (status, error_code(&read).as_str()),
            (200, "<none>"),
            "case {case:?}: expected GetBackupVaultNotifications 200, got {status} {read:?}"
        );
        assert_eq!(
            string_at(&read, "BackupVaultName"),
            "notified-vault",
            "case {case:?}: the read must name the vault it answered for, got {read:?}"
        );
        assert_eq!(
            string_at(&read, "BackupVaultArn"),
            vault_arn(REGION, "notified-vault"),
            "case {case:?}: the read must report the vault's own ARN, got {read:?}"
        );
        assert_eq!(
            string_at(&read, "SNSTopicArn"),
            topic,
            "case {case:?}: the read must report the topic that was stored, got {read:?}"
        );
        assert_eq!(
            reported_events(&read),
            events,
            "case {case:?}: the read must report the events that were stored, got {read:?}"
        );
    }
}

/// A vault carries exactly one configuration: a second accepted write replaces
/// the topic AND the event list of the first, rather than adding to them.
#[tokio::test]
async fn a_later_write_replaces_the_configuration_before_it() {
    let router = router();
    vault_named(&router, "notified-vault").await;

    let cases: [(&str, &str, Vec<&str>); 3] = [
        (
            "the first configuration",
            "first-topic",
            vec!["BACKUP_JOB_STARTED", "BACKUP_JOB_COMPLETED"],
        ),
        (
            "another topic and fewer events",
            "second-topic",
            vec!["RESTORE_JOB_FAILED"],
        ),
        (
            "another topic and more events",
            "third-topic",
            vec![
                "COPY_JOB_STARTED",
                "COPY_JOB_SUCCESSFUL",
                "COPY_JOB_FAILED",
                "RECOVERY_POINT_MODIFIED",
            ],
        ),
    ];

    for (case, topic, events) in cases {
        let topic = topic_arn(topic);
        let (status, accepted) = put_notifications(&router, "notified-vault", &topic, &events).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200, got {status} {accepted:?}"
        );
        let (status, read) = get_notifications(&router, "notified-vault").await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the notifications read to answer 200, got {read:?}"
        );
        assert_eq!(
            (string_at(&read, "SNSTopicArn"), reported_events(&read)),
            (
                topic.clone(),
                events.iter().map(|e| (*e).to_string()).collect()
            ),
            "case {case:?}: the read must report the configuration in force, got {read:?}"
        );
    }
}

/// A vault carrying no configuration has none to report, and the delete states
/// the end state: it succeeds whether or not one was configured, after which
/// the read answers exactly as it does for a vault that was never configured.
#[tokio::test]
async fn a_vault_with_no_configuration_has_none_to_report_and_the_delete_still_succeeds() {
    let router = router();
    vault_named(&router, "notified-vault").await;
    vault_named(&router, "bare-vault").await;

    let (status, read) = get_notifications(&router, "bare-vault").await;
    assert_eq!(
        (
            status,
            error_code(&read).as_str(),
            string_at(&read, "Message").as_str()
        ),
        (400, "ResourceNotFoundException", NO_NOTIFICATIONS),
        "a vault that was never configured has nothing to report, got {status} {read:?}"
    );

    let topic = topic_arn("lifecycle-topic");
    let (status, accepted) =
        put_notifications(&router, "notified-vault", &topic, &["BACKUP_JOB_FAILED"]).await;
    assert_eq!(status, 200, "setup: expected 200, got {status} {accepted:?}");

    // The delete is stated per vault AND states the end state, so the rows are
    // the vault it cleared, the same vault a second time, and a vault that was
    // never configured at all.
    for (case, vault) in [
        ("the vault that was configured", "notified-vault"),
        ("the same vault a second time", "notified-vault"),
        ("a vault that was never configured", "bare-vault"),
    ] {
        let (status, deleted) = delete_notifications(&router, vault).await;
        assert_eq!(
            (status, error_code(&deleted).as_str()),
            (200, "<none>"),
            "case {case:?}: expected DeleteBackupVaultNotifications 200, got {status} {deleted:?}"
        );
        let (status, read) = get_notifications(&router, vault).await;
        assert_eq!(
            (
                status,
                error_code(&read).as_str(),
                string_at(&read, "Message").as_str()
            ),
            (400, "ResourceNotFoundException", NO_NOTIFICATIONS),
            "case {case:?}: a cleared vault reports no configuration, got {status} {read:?}"
        );
    }
}

/// A topic that is not an ARN is refused as the value of the member that
/// carried it, and nothing is configured — the read keeps answering exactly as
/// it did before the refused call.
///
/// One row per way a value can fail to be an ARN: the field count and each
/// field the form requires.
#[tokio::test]
async fn a_topic_that_is_not_an_arn_is_refused_and_configures_nothing() {
    let cases: [(&str, &str); 6] = [
        ("not an ARN at all", "my-vault-topic"),
        ("an ARN with too few fields", "arn:aws:sns:us-west-2:000000000000"),
        ("no partition", "arn::sns:us-west-2:000000000000:MyVaultTopic"),
        ("no service", "arn:aws::us-west-2:000000000000:MyVaultTopic"),
        ("no resource", "arn:aws:sns:us-west-2:000000000000:"),
        ("an empty value", ""),
    ];

    let router = router();
    vault_named(&router, "notified-vault").await;

    for (case, topic) in cases {
        let (status, refused) =
            put_notifications(&router, "notified-vault", topic, &["BACKUP_JOB_FAILED"]).await;
        assert_eq!(
            (
                status,
                error_code(&refused).as_str(),
                string_at(&refused, "Message").as_str()
            ),
            (400, "InvalidParameterValueException", TOPIC_INVALID),
            "case {case:?} ({topic:?}): expected the parameter refusal, got {status} {refused:?}"
        );
        let (status, read) = get_notifications(&router, "notified-vault").await;
        assert_eq!(
            (status, error_code(&read).as_str()),
            (400, "ResourceNotFoundException"),
            "case {case:?}: a refused write must configure nothing, got {status} {read:?}"
        );
    }
}

/// All three calls decide access before existence: a vault this account does
/// not hold is refused for lack of privileges rather than reported absent.
#[tokio::test]
async fn a_vault_the_caller_cannot_see_is_refused_by_all_three_calls() {
    let router = router();
    let topic = topic_arn("unreachable-topic");

    let (status, refused) =
        put_notifications(&router, "never-created", &topic, &["BACKUP_JOB_FAILED"]).await;
    assert_eq!(
        (status, error_code(&refused).as_str()),
        (403, "AccessDeniedException"),
        "the write must refuse an invisible vault for lack of privileges, got {status} {refused:?}"
    );
    let (status, refused) = get_notifications(&router, "never-created").await;
    assert_eq!(
        (status, error_code(&refused).as_str()),
        (403, "AccessDeniedException"),
        "the read must refuse an invisible vault for lack of privileges, got {status} {refused:?}"
    );
    let (status, refused) = delete_notifications(&router, "never-created").await;
    assert_eq!(
        (status, error_code(&refused).as_str()),
        (403, "AccessDeniedException"),
        "the delete must refuse an invisible vault for lack of privileges, got {status} {refused:?}"
    );
}

/// The configuration belongs to ONE vault, and a vault is identified by its
/// name within one account AND one Region: a write in one Region leaves the
/// same-named vault of another Region carrying nothing.
#[tokio::test]
async fn the_configuration_is_scoped_to_the_region_the_vault_was_created_in() {
    const OTHER_REGION: &str = "eu-central-1";
    let router = router();

    for region in [REGION, OTHER_REGION] {
        let (status, created) = create_vault_in(&router, region, "shared-name", "{}").await;
        assert_eq!(
            status, 200,
            "setup: CreateBackupVault in {region} should succeed, got {status} {created:?}"
        );
    }

    let topic = topic_arn("regional-topic");
    let (status, accepted) =
        put_notifications(&router, "shared-name", &topic, &["BACKUP_JOB_STARTED"]).await;
    assert_eq!(status, 200, "expected 200, got {status} {accepted:?}");

    let (status, read) = get_notifications(&router, "shared-name").await;
    assert_eq!(
        (status, string_at(&read, "BackupVaultArn")),
        (200, vault_arn(REGION, "shared-name")),
        "the vault of the request's Region reports what was stored on it, got {read:?}"
    );

    let (status, read) = call_in_region(
        &router,
        OTHER_REGION,
        "GET",
        "/backup-vaults/shared-name/notification-configuration",
        None,
    )
    .await;
    assert_eq!(
        (status, error_code(&read).as_str()),
        (400, "ResourceNotFoundException"),
        "the same name in another Region is another vault, carrying nothing: got {status} {read:?}"
    );
}
