// SPDX-License-Identifier: CC0-1.0

//! The `Code` member of the notifications read's refusal body — one more
//! condition of the rule `refusal_error_codes.rs` states, whose body readers
//! this suite shares.
//!
//! The rule under test: when `GetBackupVaultNotifications` refuses a vault that
//! carries no notification configuration, the modelled exception's `Code` member
//! carries that error class's fixed token beside the `Message` — the token is the
//! CONDITION's, so it is the same for every vault, however the vault came to
//! carry nothing and however often the read is repeated, while a read this
//! service can answer carries no token at all and a condition this service has
//! no token for does not borrow this one.
//!
//! Every case drives real HTTP through the router, so what is asserted is the
//! body a `backup` SDK client parses; the harness is the shared one in
//! `vault_and_backup_job.rs` and the three call helpers are the shared ones in
//! `vault_notifications.rs`.

use smithy_server_runtime::MultiServiceRouter;

use crate::refusal_error_codes::{expected, member_or_unset, reported};
use crate::vault_and_backup_job::{call, create_vault, error_code, name_of_length, router};
use crate::vault_notifications::{
    delete_notifications, get_notifications, put_notifications, topic_arn, NO_NOTIFICATIONS,
};

/// The token AWS Backup carries for "the vault named carries no notification
/// configuration to report": the error class's own identifier, constant for the
/// condition.
const NO_NOTIFICATIONS_CODE: &str = "ERROR_2106";

/// How often a case repeats the read, to pin that the token is the condition's
/// answer every time and not only the first.
const REPEAT_READS: usize = 3;

/// Create the vault a case reads.
async fn vault_named(router: &MultiServiceRouter, name: &str) {
    let (status, created) = create_vault(router, name, "{}").await;
    assert_eq!(
        status, 200,
        "setup: CreateBackupVault {name} should succeed, got {status} {created:?}"
    );
}

/// The refusal carries the condition's token for every vault that carries no
/// configuration, however the vault came to carry none, and on every repeat of
/// the read.
///
/// The rows span the vault names the model's `^[a-zA-Z0-9\-\_]{2,50}$` pattern
/// admits — both published ends of its length bound, both punctuation
/// characters, digits and mixed case — crossed with the ways a vault ends up
/// carrying nothing: it was never configured, its configuration was deleted, or
/// a write of it was refused.
#[tokio::test]
async fn the_no_configuration_refusal_carries_the_conditions_token() {
    let names: Vec<(&str, String)> = vec![
        ("the shortest admitted name", name_of_length(2)),
        ("the longest admitted name", name_of_length(50)),
        ("a hyphenated name", "bare-vault".to_string()),
        ("an underscored name", "bare_vault_two".to_string()),
        ("digits only", "20261121".to_string()),
        ("mixed case", "BareVaultThree".to_string()),
    ];

    for (case, name) in &names {
        for history in [
            "never configured",
            "configured then deleted",
            "a refused write",
        ] {
            let router = router();
            vault_named(&router, name).await;

            match history {
                "configured then deleted" => {
                    let topic = topic_arn("history-topic");
                    let (status, stored) =
                        put_notifications(&router, name, &topic, &["BACKUP_JOB_FAILED"]).await;
                    assert_eq!(
                        status, 200,
                        "case {case:?}/{history}: setup write should succeed, got {status} \
                         {stored:?}"
                    );
                    let (status, cleared) = delete_notifications(&router, name).await;
                    assert_eq!(
                        status, 200,
                        "case {case:?}/{history}: setup delete should succeed, got {status} \
                         {cleared:?}"
                    );
                }
                "a refused write" => {
                    let (status, refused) =
                        put_notifications(&router, name, "not-an-arn", &["BACKUP_JOB_FAILED"])
                            .await;
                    assert_eq!(
                        status, 400,
                        "case {case:?}/{history}: setup write should be refused, got {status} \
                         {refused:?}"
                    );
                }
                _ => {}
            }

            // The token is the condition's, so it is the same answer on the
            // first read and on every repeat of it. The sentence is pinned by
            // equality rather than by what it does not contain: it is this
            // class's fixed text, which the recorded 400 body for a vault
            // carrying no configuration states unchanged.
            for attempt in 1..=REPEAT_READS {
                let (status, body) = get_notifications(&router, name).await;
                assert_eq!(
                    reported(status, &body),
                    expected(
                        400,
                        "ResourceNotFoundException",
                        NO_NOTIFICATIONS_CODE,
                        NO_NOTIFICATIONS
                    ),
                    "case {case:?}/{history} read {attempt}: expected the no-configuration \
                     refusal with Code={NO_NOTIFICATIONS_CODE}, got {status} {body:?}"
                );
            }
        }
    }
}

/// Non-vacuity: a configuration this emulator holds is SERVED with no `Code` at
/// all, and the token belongs to this condition alone — neither the access
/// refusal nor the other not-found condition of the same vault borrows it.
///
/// The rows differ along the event list, because the served answer must carry no
/// token whatever was configured.
#[tokio::test]
async fn a_served_read_carries_no_token_and_another_condition_does_not_borrow_this_one() {
    let cases: [(&str, &str, &[&str]); 3] = [
        ("one event", "served-one", &["BACKUP_JOB_COMPLETED"]),
        (
            "several events",
            "served-many",
            &["COPY_JOB_STARTED", "COPY_JOB_FAILED", "BACKUP_JOB_EXPIRED"],
        ),
        ("no events at all", "served-none", &[]),
    ];

    let router = router();
    vault_named(&router, "served-vault").await;

    for (case, topic, events) in cases {
        let topic = topic_arn(topic);
        let (status, stored) = put_notifications(&router, "served-vault", &topic, events).await;
        assert_eq!(
            status, 200,
            "case {case:?}: setup write should succeed, got {status} {stored:?}"
        );
        let (status, served) = get_notifications(&router, "served-vault").await;
        assert_eq!(
            (status, member_or_unset(&served, "Code")),
            (200, "<unset>".to_string()),
            "case {case:?}: a served read reports no Code, got {status} {served:?}"
        );
    }

    // Another condition of the same route: a vault this account does not hold
    // is refused for lack of privileges, and that refusal is not this
    // condition's token.
    let (status, denied) = get_notifications(&router, "never-created").await;
    assert_eq!(
        (status, error_code(&denied)),
        (403, "AccessDeniedException".to_string()),
        "case \"a vault the caller cannot see\": expected the access refusal, got {status} \
         {denied:?}"
    );
    assert_ne!(
        member_or_unset(&denied, "Code"),
        NO_NOTIFICATIONS_CODE,
        "case \"a vault the caller cannot see\": the access refusal must not carry this \
         condition's token, got {denied:?}"
    );

    // And the OTHER not-found condition of the same vault — the read of the
    // resource policy a vault carries none of — is a different condition: it
    // states its own sentence (which names the vault's ARN) and, having no
    // token of its own, states none rather than borrowing this one.
    let (status, policy) = call(
        &router,
        "GET",
        "/backup-vaults/served-vault/access-policy",
        None,
    )
    .await;
    assert_eq!(
        (
            status,
            error_code(&policy),
            member_or_unset(&policy, "Code")
        ),
        (
            400,
            "ResourceNotFoundException".to_string(),
            "<unset>".to_string()
        ),
        "case \"another member of the same vault\": expected that condition's own refusal \
         without this condition's token, got {status} {policy:?}"
    );
    assert_ne!(
        member_or_unset(&policy, "Message"),
        NO_NOTIFICATIONS,
        "case \"another member of the same vault\": the two conditions state different \
         sentences, got {policy:?}"
    );
}
