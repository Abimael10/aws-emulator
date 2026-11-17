// SPDX-License-Identifier: CC0-1.0

//! The BODY of the authorization refusal a call that addresses a RECOVERY POINT
//! by ARN answers.
//!
//! The rule under test: when such a call is refused because the recovery point
//! its ARN names belongs to another account, the refusal's `Message` is IAM's
//! resource-based-policy denial — "User: `<principal>` is not authorized to
//! perform: `<action>` on resource: `<resource>` because no resource-based policy
//! allows the `<action>` action" — whose variable parts come from the request: the
//! principal the credentials it signed with speak for, the action it asked for
//! (`backup:<Operation>`) and the `RecoveryPointArn` it carried, verbatim. Only a
//! resource-based policy of the account that HOLDS the point could have allowed
//! the call, and none does, which is the clause the sentence closes on.
//!
//! The table spans every implemented operation that addresses a recovery point by
//! the ARN the request supplies — so the action varies — crossed with ARNs that
//! differ in every part the sentence quotes back: the account id, the Region, the
//! partition, the spelling of the point id, and an ARN with no account field at
//! all. A second table varies the remaining part, the PRINCIPAL: the account's own
//! published credential and an assumed-role session, so the name in the sentence
//! is shown to be read off the credentials rather than fixed. The controls are the
//! other half of the rule: a refusal this decision did not take (an own-account
//! ARN that names no stored point) is NOT this sentence, and the account's own
//! stored point is still answered.
//!
//! Every case drives real HTTP through the router, so what is asserted is the wire
//! behaviour a `backup` SDK client sees. The request harness and the operation
//! table are the shared ones in `recovery_point_cross_account_authorization.rs`,
//! and the three-service emulator with its seeded session is
//! `caller_authorization.rs`'s, so this suite drives the same wire paths rather
//! than growing a second copy of them.

use std::collections::BTreeSet;

use crate::caller_authorization::{
    call_as, emulator, given_session, session_arn, ACCOUNT_KEY_ID, SESSION_KEY_ID,
};
use crate::recovery_point_cross_account_authorization::{
    address, addressed_operations, caller_principal, denial, stored_point, ACTION_NAMESPACE,
};
use crate::vault_and_backup_job::{error_code, string_at, ACCOUNT, REGION};

/// Recovery-point ARNs of accounts other than the one serving the request, each
/// differing in a part the refusal quotes back.
///
/// The id spellings are the ones the model's own `RecoveryPointArn` examples use
/// (`smithy_query(query="shape", shape="DescribeRecoveryPointInput")`:
/// "arn:aws:backup:us-east-1:123456789012:recovery-point:1EB3B5E7-9EB0-435A-A80B-108B488B0D45"),
/// plus the lowercase uuid form a started backup mints.
fn foreign_arns() -> Vec<(&'static str, String)> {
    vec![
        (
            "another account, a lowercase uuid",
            format!("arn:aws:backup:{REGION}:111122223333:recovery-point:fb698c18-a985-4704-b4fb-0c14fa8cfaa3"),
        ),
        (
            "another account, the model's uppercase id",
            "arn:aws:backup:us-east-1:123456789012:recovery-point:1EB3B5E7-9EB0-435A-A80B-108B488B0D45"
                .to_string(),
        ),
        (
            "another Region and another account",
            "arn:aws:backup:eu-central-1:999988887777:recovery-point:aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"
                .to_string(),
        ),
        (
            "another partition",
            "arn:aws-us-gov:backup:us-gov-west-1:210987654321:recovery-point:7c1b0f42"
                .to_string(),
        ),
        (
            "an account id one digit off this account's",
            format!("arn:aws:backup:{REGION}:000000000001:recovery-point:1EB3B5E7"),
        ),
        (
            "no account field at all",
            format!("arn:aws:backup:{REGION}::recovery-point:1EB3B5E7"),
        ),
    ]
}

/// The refusal's `Message` is IAM's resource-based-policy denial, for every
/// operation that addresses a recovery point by ARN and every foreign-account ARN
/// form.
#[tokio::test]
async fn the_refusal_states_the_authorization_sentence() {
    for operation in addressed_operations() {
        let router = crate::vault_and_backup_job::router();
        let _stored = stored_point(&router).await;

        for (case, arn) in foreign_arns() {
            let (status, error_type, refused) = address(&router, &operation, &arn).await;
            assert_eq!(
                (status, error_type.as_str(), error_code(&refused).as_str()),
                (403, "AccessDeniedException", "AccessDeniedException"),
                "case {case:?} on {}: expected the authorization refusal, got {status} \
                 {error_type} {refused:?}",
                operation.operation
            );

            let message = string_at(&refused, "Message");
            assert_eq!(
                message,
                denial(operation.operation, &arn),
                "case {case:?} on {}: expected IAM's resource-based-policy denial, got \
                 {message:?}",
                operation.operation
            );
        }
    }
}

/// The sentence is built FROM the request: it names the principal that signed, the
/// action asked for and the ARN carried — so every row of the table answers its
/// own message rather than one shared sentence.
#[tokio::test]
async fn the_sentence_names_the_principal_the_action_and_the_arn() {
    let mut messages: BTreeSet<String> = BTreeSet::new();
    let mut rows = 0usize;

    for operation in addressed_operations() {
        let router = crate::vault_and_backup_job::router();
        let _stored = stored_point(&router).await;
        let action = format!("{ACTION_NAMESPACE}:{}", operation.operation);

        for (case, arn) in foreign_arns() {
            let (status, _, refused) = address(&router, &operation, &arn).await;
            let message = string_at(&refused, "Message");
            assert_eq!(
                status, 403,
                "case {case:?} on {}: expected the authorization refusal, got {status} \
                 {refused:?}",
                operation.operation
            );
            for quoted in [caller_principal(), action.clone(), arn.clone()] {
                assert!(
                    message.contains(&quoted),
                    "case {case:?} on {}: the refusal must name {quoted:?}, got {message:?}",
                    operation.operation
                );
            }
            messages.insert(message);
            rows += 1;
        }
    }

    assert_eq!(
        messages.len(),
        rows,
        "case the whole table: expected one sentence per (action, resource) pair, got \
         {} distinct for {rows} rows: {messages:?}",
        messages.len()
    );
}

/// The PRINCIPAL the sentence names is the one the request's credentials speak
/// for, as `sts:GetCallerIdentity` reports it: the account's own published
/// credential and an assumed-role session are each blamed by their own ARN for the
/// same call on the same ARN.
///
/// The two operations driven here are the ones that address the point by ARN alone
/// (the delete and the read); the two job starts are judged on the CALLER's own
/// policies first, which `caller_authorization.rs` owns.
#[tokio::test]
async fn the_sentence_names_the_principal_the_credentials_speak_for() {
    let foreign =
        format!("arn:aws:backup:{REGION}:111122223333:recovery-point:fb698c18-a985-4704-b4fb-0c14fa8cfaa3");
    // (case, the access key id the request signs with, the principal it speaks for)
    let principals: Vec<(&str, &str, String)> = vec![
        (
            "the account's own published credential",
            ACCOUNT_KEY_ID,
            caller_principal(),
        ),
        ("an assumed-role session", SESSION_KEY_ID, session_arn()),
    ];
    // (the operation, its method, the path that addresses `foreign` through it)
    let addressed: Vec<(&str, &str)> = vec![("DeleteRecoveryPoint", "DELETE"), ("DescribeRecoveryPoint", "GET")];

    for (case, access_key_id, principal) in principals {
        let (router, engine) = emulator();
        given_session(&engine);

        for (operation, method) in &addressed {
            let path = format!("/backup-vaults/sentence-vault/recovery-points/{foreign}");
            let (status, refused) = call_as(&router, access_key_id, method, &path, None).await;
            let action = format!("{ACTION_NAMESPACE}:{operation}");
            let expected = format!(
                "User: {principal} is not authorized to perform: {action} on resource: \
                 {foreign} because no resource-based policy allows the {action} action"
            );
            assert_eq!(
                (status, error_code(&refused).as_str(), refused["Message"].as_str()),
                (403, "AccessDeniedException", Some(expected.as_str())),
                "case {case:?} on {operation}: expected the refusal to name {principal:?}, \
                 got {status} {refused:?}"
            );
        }
    }
}

/// The other half of the rule: a refusal this decision did NOT take is not this
/// sentence. An own-account ARN that names no stored point is still each
/// operation's own 400 refusal, and the account's own stored point is answered.
#[tokio::test]
async fn an_own_account_refusal_is_not_the_authorization_sentence() {
    for operation in addressed_operations() {
        let router = crate::vault_and_backup_job::router();
        let stored = stored_point(&router).await;

        let absent = format!("arn:aws:backup:{REGION}:{ACCOUNT}:recovery-point:nothing-here");
        let (status, _, refused) = address(&router, &operation, &absent).await;
        let message = string_at(&refused, "Message");
        assert_eq!(
            (status, message == denial(operation.operation, &absent)),
            (400, false),
            "case an own-account ARN naming nothing on {}: expected a 400 refusal that is \
             not the authorization sentence, got {status} {refused:?}",
            operation.operation
        );

        let (status, _, answered) = address(&router, &operation, &stored).await;
        assert_eq!(
            (status, error_code(&answered).as_str()),
            (200, "<none>"),
            "case the account's own stored point on {}: expected it to be answered, got \
             {status} {answered:?}",
            operation.operation
        );
    }
}
