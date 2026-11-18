// SPDX-License-Identifier: CC0-1.0

//! What AWS Backup answers a call that is ADDRESSED by a value which is not an
//! identifier of one of its resources at all, driven as real HTTP through the
//! router.
//!
//! The rule under test: an identifier bound into the path of a call that
//! addresses a backup plan or one of its resource selections is read BEFORE the
//! store is asked about it, so a value that is not of the form this service
//! gives such a resource is refused `InvalidParameterValueException` (HTTP 400)
//! with that member's own token and its own fixed sentence — a condition
//! distinct from a WELL-FORMED identifier the account holds nothing under, which
//! stays the not-found refusal with its own token and sentence. A client reads
//! the two apart: one says "correct the argument", the other "the resource is
//! not there".
//!
//! The form is the one the service mints
//! (`ComAmazonawsBackupCryocontrollerusermanagerState::next_id`): the hyphenated
//! 8-4-4-4-12 hex identifier AWS Backup documents its plan ids in
//! (`arn:aws:backup:us-east-1:123456789012:plan:8F81F553-3A74-4A3F-B93D-B3360DC80C50`,
//! the model doc for `BackupPlanArn`), read case-insensitively.
//!
//! Both halves of the (`BackupPlanId`, `SelectionId`) pair are asserted over a
//! TABLE whose rows differ along what the rule generalises over — the KIND of
//! value supplied (an ARN of the resource itself, an ARN of another resource, a
//! bare word, a digit, the unhyphenated spelling, a group of the wrong length, a
//! non-hex character, one too many groups) — and each table is paired with the
//! identifiers the same emulator DID mint, which are served, so a refusal is
//! never a blanket answer.
//!
//! The request harness (`call_reported`, `router`, `ACCOUNT`, `REGION`) is the
//! one `vault_and_backup_job.rs` exposes to this test binary, so there is ONE
//! wire path for the service's suites.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::vault_and_backup_job::{call_reported, router, ACCOUNT, REGION};

/// The token and sentence a `BackupPlanId` that is not a plan identifier at all
/// is refused with (`crate::state`'s `ERROR_CODE_BACKUP_PLAN_ID_INVALID` /
/// `MESSAGE_BACKUP_PLAN_ID_INVALID`).
const PLAN_ID_INVALID_CODE: &str = "ERROR_3208";
const PLAN_ID_INVALID_MESSAGE: &str = "Invalid Backup plan ID";

/// The token and sentence a `SelectionId` that is not a selection identifier at
/// all is refused with (`ERROR_CODE_BACKUP_SELECTION_ID_INVALID` /
/// `MESSAGE_BACKUP_SELECTION_ID_INVALID`).
const SELECTION_ID_INVALID_CODE: &str = "ERROR_3314";
const SELECTION_ID_INVALID_MESSAGE: &str = "Invalid Backup selection ID";

/// The token and sentence a plan READ answers for a WELL-FORMED id whose plan or
/// version is not there to read (`ERROR_CODE_BACKUP_PLAN_VERSION_UNREADABLE` /
/// `MESSAGE_BACKUP_PLAN_VERSION_UNREADABLE`).
const VERSION_UNREADABLE_CODE: &str = "ERROR_2202";
const VERSION_UNREADABLE_MESSAGE: &str = "Failed reading Backup plan with provided version";

/// The token and sentence a selection READ answers for a WELL-FORMED id the plan
/// does not hold (`ERROR_CODE_BACKUP_SELECTION_NOT_FOUND` /
/// `MESSAGE_BACKUP_SELECTION_NOT_FOUND`).
const SELECTION_NOT_FOUND_CODE: &str = "ERROR_2303";
const SELECTION_NOT_FOUND_MESSAGE: &str = "Cannot find Backup selection";

/// Identifiers of the form this service mints that a fresh store holds nothing
/// under — one in each casing, since the form is read case-insensitively.
const UNHELD_ID: &str = "11111111-2222-3333-4444-555555555555";
const UNHELD_ID_UPPER: &str = "A1B2C3D4-0000-4000-8000-00000000000F";

/// What a refused call said about itself: the status, the error shape named by
/// `x-amzn-errortype`, and the `Code` and `Message` members of the body, each
/// `"<unset>"` when absent.
#[derive(Debug, PartialEq, Eq)]
struct Refusal {
    status: u16,
    errortype: String,
    code: String,
    message: String,
}

impl Refusal {
    /// The refusal a response reports.
    fn of(status: u16, errortype: &str, body: &Value) -> Self {
        Self {
            status,
            errortype: errortype.to_string(),
            code: member(body, "Code"),
            message: member(body, "Message"),
        }
    }

    /// The refusal a row expects.
    fn expected(errortype: &str, code: &str, message: &str) -> Self {
        Self {
            status: 400,
            errortype: errortype.to_string(),
            code: code.to_string(),
            message: message.to_string(),
        }
    }
}

/// A body member as a string, or `"<unset>"` when the member is absent.
fn member(body: &Value, name: &str) -> String {
    body.get(name)
        .and_then(Value::as_str)
        .unwrap_or("<unset>")
        .to_string()
}

/// `PUT /backup/plans` — `CreateBackupPlan`'s HTTP binding — for a plan of one
/// rule, answering the id this service minted for it.
async fn plan_id_of(router: &MultiServiceRouter, name: &str) -> String {
    let body = json!({
        "BackupPlan": {
            "BackupPlanName": name,
            "Rules": [{"RuleName": "daily", "TargetBackupVaultName": "identifier-vault"}],
        }
    });
    let (status, _, created) =
        call_reported(router, "PUT", "/backup/plans", Some(&body.to_string())).await;
    assert_eq!(
        status, 200,
        "setup: CreateBackupPlan {name:?} expected 200, got {status} {created:?}"
    );
    member(&created, "BackupPlanId")
}

/// `PUT /backup/plans/{BackupPlanId}/selections` — `CreateBackupSelection`'s —
/// for a selection that names one resource, answering the id minted for it.
async fn selection_id_of(router: &MultiServiceRouter, plan_id: &str, name: &str) -> String {
    let body = json!({
        "BackupSelection": {
            "SelectionName": name,
            "IamRoleArn": format!("arn:aws:iam::{ACCOUNT}:role/AWSBackupDefaultServiceRole"),
            "Resources": ["arn:aws:s3:::identifier-probe"],
        }
    });
    let (status, _, created) = call_reported(
        router,
        "PUT",
        &format!("/backup/plans/{plan_id}/selections"),
        Some(&body.to_string()),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateBackupSelection {name:?} expected 200, got {status} {created:?}"
    );
    member(&created, "SelectionId")
}

/// `GET /backup/plans/{BackupPlanId}` — `GetBackupPlan`'s.
async fn get_plan(router: &MultiServiceRouter, plan_id: &str) -> (u16, String, Value) {
    call_reported(router, "GET", &format!("/backup/plans/{plan_id}"), None).await
}

/// `GET /backup/plans/{BackupPlanId}/selections/{SelectionId}` —
/// `GetBackupSelection`'s.
async fn get_selection(
    router: &MultiServiceRouter,
    plan_id: &str,
    selection_id: &str,
) -> (u16, String, Value) {
    call_reported(
        router,
        "GET",
        &format!("/backup/plans/{plan_id}/selections/{selection_id}"),
        None,
    )
    .await
}

/// The values that are NOT an identifier of a backup resource, with what each row
/// is about.
///
/// The rows span the ways a value can fail to be one: the ARN of the resource
/// itself (what a client passes when it confuses the two), the ARN of another
/// resource of this service, a bare word, a single digit, the unhyphenated
/// 32-character spelling of the same hex, a group of the wrong length, a
/// non-hex character in a group, one group too many, and a well-formed
/// identifier with something appended.
fn malformed_identifiers() -> Vec<(&'static str, String)> {
    vec![
        (
            "a plan ARN where the bare id is required",
            format!("arn:aws:backup:{REGION}:{ACCOUNT}:backup-plan:{UNHELD_ID}"),
        ),
        (
            "a vault ARN, which names another resource entirely",
            format!("arn:aws:backup:{REGION}:{ACCOUNT}:backup-vault:identifier-vault"),
        ),
        ("a bare word", "not-an-identifier".to_string()),
        ("a single digit", "7".to_string()),
        (
            "the unhyphenated spelling of the same hex",
            "11111111222233334444555555555555".to_string(),
        ),
        (
            "a group one character short",
            "11111111-2222-3333-4444-55555555555".to_string(),
        ),
        (
            "a non-hex character in the first group",
            "1111111g-2222-3333-4444-555555555555".to_string(),
        ),
        (
            "one group too many",
            "11111111-2222-3333-4444-5555-55555555".to_string(),
        ),
        (
            "a well-formed identifier with a suffix appended",
            format!("{UNHELD_ID}-extra"),
        ),
    ]
}

/// A `BackupPlanId` that is not a plan identifier is refused as THAT member,
/// ahead of the store, while a well-formed id the account holds no plan under
/// stays the unreadable-version refusal.
#[tokio::test]
async fn a_plan_read_refuses_an_identifier_no_plan_could_have() {
    let router = router();
    // A plan the account DOES hold, so none of the rows below is answered by an
    // empty store.
    let plan_id = plan_id_of(&router, "identifier-plan").await;

    for (case, asked) in malformed_identifiers() {
        let (status, errortype, refused) = get_plan(&router, &asked).await;
        assert_eq!(
            Refusal::of(status, &errortype, &refused),
            Refusal::expected(
                "InvalidParameterValueException",
                PLAN_ID_INVALID_CODE,
                PLAN_ID_INVALID_MESSAGE,
            ),
            "case {case:?} ({asked:?}): expected the invalid-plan-id refusal, got {status} \
             {errortype} {refused:?}"
        );
        assert!(
            !member(&refused, "Message").contains(&asked),
            "case {case:?}: the sentence must not echo the value it refused, got {refused:?}"
        );
    }

    // A WELL-FORMED id is a different condition: the plan or the version it asks
    // for is simply not there to read, in either casing of the form.
    for (case, asked) in [
        ("the minted form, never minted", UNHELD_ID),
        ("the minted form in upper case", UNHELD_ID_UPPER),
    ] {
        let (status, errortype, refused) = get_plan(&router, asked).await;
        assert_eq!(
            Refusal::of(status, &errortype, &refused),
            Refusal::expected(
                "ResourceNotFoundException",
                VERSION_UNREADABLE_CODE,
                VERSION_UNREADABLE_MESSAGE,
            ),
            "case {case:?} ({asked:?}): expected the unreadable-version refusal, got {status} \
             {errortype} {refused:?}"
        );
    }

    // Non-vacuity: the id this emulator minted is served.
    let (status, _, read) = get_plan(&router, &plan_id).await;
    assert_eq!(
        (status, member(&read, "BackupPlanId")),
        (200, plan_id.clone()),
        "case \"the id this emulator minted\": expected the plan to be served, got {status} \
         {read:?}"
    );
}

/// A `SelectionId` that is not a selection identifier is refused as THAT member,
/// ahead of the store and ahead of the plan half of the pair, while a
/// well-formed id the plan does not hold stays the absent selection.
#[tokio::test]
async fn a_selection_read_refuses_an_identifier_no_selection_could_have() {
    let router = router();
    let plan_id = plan_id_of(&router, "identifier-plan").await;
    let selection_id = selection_id_of(&router, &plan_id, "assigned").await;

    for (case, asked) in malformed_identifiers() {
        let (status, errortype, refused) = get_selection(&router, &plan_id, &asked).await;
        assert_eq!(
            Refusal::of(status, &errortype, &refused),
            Refusal::expected(
                "InvalidParameterValueException",
                SELECTION_ID_INVALID_CODE,
                SELECTION_ID_INVALID_MESSAGE,
            ),
            "case {case:?} ({asked:?}): expected the invalid-selection-id refusal, got {status} \
             {errortype} {refused:?}"
        );
        assert!(
            !member(&refused, "Message").contains(&asked),
            "case {case:?}: the sentence must not echo the value it refused, got {refused:?}"
        );

        // The identifier is read before the store, so the SAME refusal answers a
        // pair whose plan half names no plan either: what the caller is told to
        // correct is the value the service could not read.
        let (status, errortype, refused) = get_selection(&router, UNHELD_ID, &asked).await;
        assert_eq!(
            Refusal::of(status, &errortype, &refused),
            Refusal::expected(
                "InvalidParameterValueException",
                SELECTION_ID_INVALID_CODE,
                SELECTION_ID_INVALID_MESSAGE,
            ),
            "case {case:?} ({asked:?}) under an unheld plan: expected the invalid-selection-id \
             refusal, got {status} {errortype} {refused:?}"
        );
    }

    // A WELL-FORMED id the plan does not hold is the absent selection instead,
    // in either casing of the form.
    for (case, asked) in [
        ("the minted form, never minted", UNHELD_ID),
        ("the minted form in upper case", UNHELD_ID_UPPER),
    ] {
        let (status, errortype, refused) = get_selection(&router, &plan_id, asked).await;
        assert_eq!(
            Refusal::of(status, &errortype, &refused),
            Refusal::expected(
                "ResourceNotFoundException",
                SELECTION_NOT_FOUND_CODE,
                SELECTION_NOT_FOUND_MESSAGE,
            ),
            "case {case:?} ({asked:?}): expected the absent-selection refusal, got {status} \
             {errortype} {refused:?}"
        );
    }

    // Non-vacuity: the pair this emulator minted is served.
    let (status, _, read) = get_selection(&router, &plan_id, &selection_id).await;
    assert_eq!(
        (status, member(&read, "SelectionId")),
        (200, selection_id.clone()),
        "case \"the pair this emulator minted\": expected the selection to be served, got \
         {status} {read:?}"
    );
}
