// SPDX-License-Identifier: CC0-1.0

//! The `Code` member of an AWS Backup refusal body.
//!
//! The rule under test: a refusal this service has a token for carries that
//! token in the modelled exception's `Code` member, constant for the condition
//! and stated the same way by EVERY operation that states it — so a client that
//! branches on the Backup error code reads the same token wherever the condition
//! arises. The token is the CONDITION's, not the call's: two different
//! conditions never answer with one token.
//!
//! Every case drives real HTTP through the router, so what is asserted is the
//! body a `backup` SDK client parses; the harness is the shared one in
//! `vault_and_backup_job.rs`. Each table spans the forms of input the condition
//! generalises over, and every table is paired with the SERVED answer for an
//! input the emulator does hold, so a token is never asserted against a blanket
//! refusal.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::logically_air_gapped_vault::create_air_gapped;
use crate::recovery_point_deletion::completed_point;
use crate::tiering_configuration::{
    create_body, create_tiering, delete_tiering, get_tiering, selection, MIN_TIERING_DAYS,
};
use crate::vault_and_backup_job::{
    call, create_vault, error_code, name_of_length, router, string_at, ACCOUNT, REGION,
};

/// The token AWS Backup carries for "the tiering configuration asked about is
/// not one this account holds", with the sentence it states for it.
const TIERING_NOT_FOUND_CODE: &str = "ERROR_2C01";
const TIERING_NOT_FOUND_MESSAGE: &str = "Cannot find tiering configuration";

/// The token for "a tiering configuration of this name already exists", with
/// its sentence.
const TIERING_EXISTS_CODE: &str = "ERROR_1C01";
const TIERING_EXISTS_MESSAGE: &str = "Tiering configuration with the same name already exists";

/// The token for "the restore job asked about is not one this account holds",
/// with its sentence.
const RESTORE_JOB_NOT_FOUND_CODE: &str = "ERROR_2801";
const RESTORE_JOB_NOT_FOUND_MESSAGE: &str = "Cannot find restore job";

/// The token for "this vault's type supports no multi-party approval", with its
/// sentence.
const UNSUPPORTED_VAULT_TYPE_CODE: &str = "ERROR_3935";
const UNSUPPORTED_VAULT_TYPE_MESSAGE: &str = "Operation not supported for the provided vault type";

/// The retention window the air-gapped vault here is created with: the
/// documented floor of `MinRetentionDays` and a year.
const AIR_GAPPED_WINDOW: &str = r#"{"MinRetentionDays": 7, "MaxRetentionDays": 365}"#;

/// `VaultState` the clock owes a freshly created air-gapped vault.
const VAULT_AVAILABLE: &str = "AVAILABLE";

/// How long [`available_air_gapped`] looks for that state in all, and how long
/// it waits between looks. Generous room past the span the clock owes, so the
/// case turns on the CONDITION rather than on how fast the host runs.
const VAULT_POLLS: usize = 300;
const VAULT_POLL_WAIT: std::time::Duration = std::time::Duration::from_millis(100);

/// A body member as a string, or `"<unset>"` when the member is absent — the
/// distinction this suite exists to pin.
///
/// Shared with the sibling suite that pins one more condition's token
/// (`vault_notifications_refusal_code.rs`), so this crate has ONE reader of a
/// refusal body's `Code` rather than one per file.
pub(crate) fn member_or_unset(body: &Value, name: &str) -> String {
    body.get(name)
        .and_then(Value::as_str)
        .unwrap_or("<unset>")
        .to_string()
}

/// The (status, `__type`, `Code`, `Message`) a refusal reports, as one tuple so
/// an assertion names all four at once.
pub(crate) fn reported(status: u16, body: &Value) -> (u16, String, String, String) {
    (
        status,
        error_code(body),
        member_or_unset(body, "Code"),
        member_or_unset(body, "Message"),
    )
}

/// The expected tuple for a refusal class.
pub(crate) fn expected(
    status: u16,
    shape: &str,
    code: &str,
    message: &str,
) -> (u16, String, String, String) {
    (
        status,
        shape.to_string(),
        code.to_string(),
        message.to_string(),
    )
}

/// A `TieringConfigurationName` of `length` characters drawn from the alphabet
/// the model's pattern admits (`^[a-zA-Z0-9_]{1,200}$`).
fn tiering_name_of_length(length: usize) -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_";
    (0..length)
        .map(|i| char::from(ALPHABET[(i * 5 + length) % ALPHABET.len()]))
        .collect()
}

/// `POST /backup-vaults/{BackupVaultName}/mpaApprovalTeam?delete`, as the model
/// binds the disassociation.
async fn disassociate_team(router: &MultiServiceRouter, vault: &str) -> (u16, Value) {
    call(
        router,
        "POST",
        &format!("/backup-vaults/{vault}/mpaApprovalTeam?delete"),
        Some(&json!({"RequesterComment": "removing the team"}).to_string()),
    )
    .await
}

/// Create an air-gapped vault and wait until the vault read reports the state
/// the clock owes it. The wait is on that CONDITION, looked at again until it
/// holds or a deadline far past the span passes.
async fn available_air_gapped(router: &MultiServiceRouter, name: &str) {
    let (status, created) = create_air_gapped(router, name, AIR_GAPPED_WINDOW).await;
    assert_eq!(
        status, 200,
        "setup {name}: CreateLogicallyAirGappedBackupVault should succeed, got {status} {created:?}"
    );
    for _ in 0..VAULT_POLLS {
        let (status, described) = call(router, "GET", &format!("/backup-vaults/{name}"), None).await;
        if status == 200 && string_at(&described, "VaultState") == VAULT_AVAILABLE {
            return;
        }
        ::tokio::time::sleep(VAULT_POLL_WAIT).await;
    }
    panic!("setup {name}: the air-gapped vault never became {VAULT_AVAILABLE}");
}

/// Every call that addresses a tiering configuration by name reports the ONE
/// not-found token for a name the account holds none under.
///
/// The rows span the forms the `TieringConfigurationName` pattern admits — both
/// published ends of its `{1,200}` length bound, digits, underscores and mixed
/// case — and each is asked of both calls that resolve one; the configuration
/// the same emulator does hold is served instead, so the token is the stored
/// state's answer and not the route's.
#[tokio::test]
async fn every_call_addressing_a_tiering_configuration_reports_the_not_found_token() {
    let router = router();
    let (status, created) = create_vault(&router, "tiering-vault", "{}").await;
    assert_eq!(
        status, 200,
        "setup: CreateBackupVault should succeed, got {status} {created:?}"
    );

    let longest = tiering_name_of_length(200);
    let absent: Vec<(&str, String)> = vec![
        ("the shortest admitted name", tiering_name_of_length(1)),
        ("the longest admitted name", longest),
        ("digits only", "20261110".to_string()),
        ("underscored words", "tier_never_created".to_string()),
        ("mixed case", "TierNeverCreated".to_string()),
    ];

    for (case, name) in &absent {
        for (operation, answer) in [
            ("GetTieringConfiguration", get_tiering(&router, name).await),
            (
                "DeleteTieringConfiguration",
                delete_tiering(&router, name).await,
            ),
        ] {
            let (status, refused) = answer;
            assert_eq!(
                reported(status, &refused),
                expected(
                    400,
                    "ResourceNotFoundException",
                    TIERING_NOT_FOUND_CODE,
                    TIERING_NOT_FOUND_MESSAGE
                ),
                "case {case:?} on {operation} ({name:?}): expected the not-found refusal with \
                 Code={TIERING_NOT_FOUND_CODE}, got {status} {refused:?}"
            );
        }
    }

    // Non-vacuity: a name this emulator does hold is SERVED, and the served
    // answer carries no `Code` at all.
    let stored = "tier_served";
    let (status, answered) = create_tiering(
        &router,
        &create_body(
            stored,
            "tiering-vault",
            json!([selection(json!(["*"]), MIN_TIERING_DAYS)]),
        ),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateTieringConfiguration should succeed, got {status} {answered:?}"
    );
    for (operation, answer) in [
        ("GetTieringConfiguration", get_tiering(&router, stored).await),
        (
            "DeleteTieringConfiguration",
            delete_tiering(&router, stored).await,
        ),
    ] {
        let (status, served) = answer;
        assert_eq!(
            (status, member_or_unset(&served, "Code")),
            (200, "<unset>".to_string()),
            "case \"a stored configuration\" on {operation}: expected it served with no Code, \
             got {status} {served:?}"
        );
    }
}

/// One row of the duplicate-create table: the case name, the vault the first
/// create applies to, its selections, the creator token it carries (if any) and
/// the tags it carries (if any).
type DuplicateRow = (
    &'static str,
    &'static str,
    Value,
    Option<&'static str>,
    Option<Value>,
);

/// A create naming a tiering configuration that already exists reports the
/// already-exists token, and still discloses nothing about the stored
/// configuration.
///
/// The rows differ in what the FIRST create carried — a bare body, a creator
/// token, tags, a wildcard vault, two selections — because that is the state
/// the refusal must not leak and must not depend on.
#[tokio::test]
async fn a_duplicate_tiering_configuration_create_reports_the_already_exists_token() {
    let bucket = "arn:aws:s3:::demo-bucket";
    let cases: Vec<DuplicateRow> = vec![
        (
            "a bare create",
            "tiering-vault",
            json!([selection(json!(["*"]), MIN_TIERING_DAYS)]),
            None,
            None,
        ),
        (
            "a create carrying a CreatorRequestId",
            "tiering-vault",
            json!([selection(json!(["*"]), MIN_TIERING_DAYS)]),
            Some("first-call"),
            None,
        ),
        (
            "a create carrying tags",
            "tiering-vault",
            json!([selection(json!([bucket]), 90)]),
            None,
            Some(json!({"team": "platform"})),
        ),
        (
            "a create applying to every vault",
            "*",
            json!([selection(json!(["*"]), MIN_TIERING_DAYS)]),
            None,
            None,
        ),
        (
            "a create with two selections",
            "tiering-vault",
            json!([
                selection(json!([bucket]), 120),
                selection(json!(["*"]), 365),
            ]),
            None,
            None,
        ),
    ];

    for (index, (case, vault, selections, token, tags)) in cases.into_iter().enumerate() {
        let router = router();
        let (status, created) = create_vault(&router, "tiering-vault", "{}").await;
        assert_eq!(
            status, 200,
            "case {case:?}: setup CreateBackupVault should succeed, got {status} {created:?}"
        );

        let name = format!("tier_duplicate_{index}");
        let mut first = create_body(&name, vault, selections);
        if let Some(token) = token {
            first["CreatorRequestId"] = json!(token);
        }
        if let Some(tags) = tags {
            first["TieringConfigurationTags"] = tags;
        }
        let (status, stored) = create_tiering(&router, &first).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the first create should succeed, got {status} {stored:?}"
        );

        // A second create of the same name carrying a DIFFERENT creator token
        // is not a retry of the first, so it collides.
        let mut second = create_body(
            &name,
            "tiering-vault",
            json!([selection(json!(["*"]), MIN_TIERING_DAYS)]),
        );
        second["CreatorRequestId"] = json!("second-call");
        let (status, refused) = create_tiering(&router, &second).await;
        assert_eq!(
            reported(status, &refused),
            expected(
                400,
                "AlreadyExistsException",
                TIERING_EXISTS_CODE,
                TIERING_EXISTS_MESSAGE
            ),
            "case {case:?}: expected the already-exists refusal with Code={TIERING_EXISTS_CODE}, \
             got {status} {refused:?}"
        );
        assert_eq!(
            (
                member_or_unset(&refused, "Arn"),
                member_or_unset(&refused, "CreatorRequestId")
            ),
            ("<unset>".to_string(), "<unset>".to_string()),
            "case {case:?}: the refusal must disclose neither Arn nor CreatorRequestId, got \
             {refused:?}"
        );

        // Non-vacuity: ANOTHER name is still created, so what is refused is the
        // collision and not the second call.
        let other = create_body(
            &format!("{name}_other"),
            "tiering-vault",
            json!([selection(json!(["*"]), MIN_TIERING_DAYS)]),
        );
        let (status, accepted) = create_tiering(&router, &other).await;
        assert_eq!(
            status, 200,
            "case {case:?}: a create of another name must still be served, got {status} \
             {accepted:?}"
        );
    }
}

/// All three calls that address a restore job by id report the ONE not-found
/// token for an id the account holds no job under.
///
/// The rows span the forms a `RestoreJobId` can take (the minted form never
/// minted, a bare word, a long hex string, a single digit, a lower-case
/// spelling), and the job the same emulator did mint is served by all three, so
/// the token is the stored state's answer and not the route's.
#[tokio::test]
async fn every_call_addressing_a_restore_job_reports_the_not_found_token() {
    let router = router();
    let (status, created) = create_vault(&router, "restore-source", "{}").await;
    assert_eq!(
        status, 200,
        "setup: CreateBackupVault should succeed, got {status} {created:?}"
    );

    let absent = [
        ("the minted form, never minted", "A1B2C3D4-0000-4000-8000-000000000001"),
        ("a bare word", "no-such-restore-job"),
        ("a long hex string", "0123456789abcdef0123456789abcdef"),
        ("a single digit", "7"),
        ("a lower-case spelling", "a1b2c3d4"),
    ];
    for (case, job_id) in absent {
        let answers = [
            (
                "DescribeRestoreJob",
                call(&router, "GET", &format!("/restore-jobs/{job_id}"), None).await,
            ),
            (
                "GetRestoreJobMetadata",
                call(
                    &router,
                    "GET",
                    &format!("/restore-jobs/{job_id}/metadata"),
                    None,
                )
                .await,
            ),
            (
                "PutRestoreValidationResult",
                call(
                    &router,
                    "PUT",
                    &format!("/restore-jobs/{job_id}/validations"),
                    Some(&json!({"ValidationStatus": "SUCCESSFUL"}).to_string()),
                )
                .await,
            ),
        ];
        for (operation, (status, refused)) in answers {
            assert_eq!(
                reported(status, &refused),
                expected(
                    400,
                    "ResourceNotFoundException",
                    RESTORE_JOB_NOT_FOUND_CODE,
                    RESTORE_JOB_NOT_FOUND_MESSAGE
                ),
                "case {case:?} on {operation} ({job_id:?}): expected the not-found refusal with \
                 Code={RESTORE_JOB_NOT_FOUND_CODE}, got {status} {refused:?}"
            );
            assert!(
                !member_or_unset(&refused, "Message").contains(job_id),
                "case {case:?} on {operation}: the message must not echo the requested id, got \
                 {refused:?}"
            );
        }
    }

    // Non-vacuity: a job this emulator minted is served by all three calls.
    let resource = format!("arn:aws:dynamodb:{REGION}:{ACCOUNT}:table/demo");
    let point = completed_point(&router, "restore-source", &resource).await;
    let (status, started) = call(
        &router,
        "PUT",
        "/restore-jobs",
        Some(
            &json!({
                "RecoveryPointArn": point,
                "ResourceType": "DynamoDB",
                "Metadata": {"targetTableName": "restored"},
                "IamRoleArn":
                    format!("arn:aws:iam::{ACCOUNT}:role/service-role/AWSBackupDefaultServiceRole"),
            })
            .to_string(),
        ),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: StartRestoreJob should succeed, got {status} {started:?}"
    );
    let minted = string_at(&started, "RestoreJobId");
    for (operation, method, path, body) in [
        (
            "DescribeRestoreJob",
            "GET",
            format!("/restore-jobs/{minted}"),
            None,
        ),
        (
            "GetRestoreJobMetadata",
            "GET",
            format!("/restore-jobs/{minted}/metadata"),
            None,
        ),
        (
            "PutRestoreValidationResult",
            "PUT",
            format!("/restore-jobs/{minted}/validations"),
            Some(json!({"ValidationStatus": "SUCCESSFUL"}).to_string()),
        ),
    ] {
        let (status, served) = call(&router, method, &path, body.as_deref()).await;
        assert!(
            (status == 200 || status == 204) && member_or_unset(&served, "Code") == "<unset>",
            "case \"the job this emulator minted\" on {operation}: expected it served with no \
             Code, got {status} {served:?}"
        );
    }
}

/// A multi-party approval call on a vault whose type supports none reports the
/// unsupported-vault-type token.
///
/// The rows span the vault names the pattern admits at both published ends of
/// its length bound and both punctuation characters, so the token is the
/// condition's answer rather than one name's; the one vault type that does
/// support multi-party approval answers the modelled 204 with no `Code`, which
/// is what makes the refusal about the type.
#[tokio::test]
async fn an_unsupported_vault_type_refusal_reports_its_token() {
    let router = router();
    let unsupported: Vec<(&str, String)> = vec![
        ("a standard vault", "plain-vault".to_string()),
        ("the shortest admitted name", name_of_length(2)),
        ("the longest admitted name", name_of_length(50)),
        ("an underscored name", "plain_vault_two".to_string()),
    ];
    for (case, name) in &unsupported {
        let (status, created) = create_vault(&router, name, "{}").await;
        assert_eq!(
            status, 200,
            "case {case:?}: setup CreateBackupVault should succeed, got {status} {created:?}"
        );
        let (status, refused) = disassociate_team(&router, name).await;
        assert_eq!(
            reported(status, &refused),
            expected(
                400,
                "InvalidParameterValueException",
                UNSUPPORTED_VAULT_TYPE_CODE,
                UNSUPPORTED_VAULT_TYPE_MESSAGE
            ),
            "case {case:?} ({name:?}): expected the unsupported-vault-type refusal with \
             Code={UNSUPPORTED_VAULT_TYPE_CODE}, got {status} {refused:?}"
        );
    }

    // Non-vacuity: the one type that does support multi-party approval answers
    // the modelled 204, so the token above is the TYPE's refusal and not the
    // route's.
    available_air_gapped(&router, "air-gapped-vault").await;
    let (status, answered) = disassociate_team(&router, "air-gapped-vault").await;
    assert_eq!(
        (status, member_or_unset(&answered, "Code")),
        (204, "<unset>".to_string()),
        "case \"the type that supports multi-party approval\": expected the modelled 204, got \
         {status} {answered:?}"
    );
}
