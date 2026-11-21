// SPDX-License-Identifier: CC0-1.0

//! The `VersionId` a stored backup plan version is addressed by, driven as real
//! HTTP through the router.
//!
//! The rule under test: a plan version's id is an OPAQUE token this service
//! draws from entropy — "unique, randomly generated, Unicode, UTF-8 encoded
//! strings that are at most 1,024 bytes long" (the model's doc for every
//! `VersionId` member) — so a token
//!
//! * is one fixed-width run of letters and digits, safe to carry in the
//!   `versionId` query a plan read pins a version with;
//! * carries nothing of the plan it versions: neither the plan's id, nor any
//!   field of it, nor its name, nor a position in the account's history, so no
//!   two tokens share a prefix and a holder of one cannot predict another;
//! * is unique over every version of every plan, and the same token keeps
//!   reading back (the create and every later read report ONE stored value);
//! * is the only way to pin that version: a token this plan never had is refused
//!   with the modelled 400 `ResourceNotFoundException` for an unreadable
//!   version.
//!
//! The rows differ in what the plan's body carries (how many rules, the
//! schedules, the advanced settings, a `CreatorRequestId`) and in how many
//! versions the plan has been given, which is what the rule generalises over:
//! the behavioural instance — the token one create of a one-rule plan answers —
//! is one row.
//!
//! The request harness (`router`, `call`, `error_code`) is the one
//! `vault_and_backup_job.rs` exposes to this test binary, so there is ONE wire
//! path for the service's suites.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::vault_and_backup_job::{call, create_vault, error_code, router};

/// The width of every token the service mints, read from the service's own
/// published constant so this suite cannot drift from what the wire carries.
const TOKEN_LENGTH: usize =
    service_com_amazonaws_backup_cryocontrollerusermanager::state::PLAN_VERSION_ID_LENGTH;

/// The error shape a version that cannot be read names (`smithy_query`
/// operation `GetBackupPlan`: `ResourceNotFoundException`, `httpError` 400).
const NOT_FOUND: &str = "ResourceNotFoundException";

/// The sentence and `Code` this service states for "the version of the plan this
/// read asks for cannot be read".
const VERSION_UNREADABLE: &str = "Failed reading Backup plan with provided version";
const VERSION_UNREADABLE_CODE: &str = "ERROR_2202";

/// How many leading characters of two tokens are compared when checking that
/// one token says nothing about another: a token drawn from entropy shares no
/// such run with another, while an id built from a counter or from the plan's
/// own id repeats one.
const SHARED_PREFIX: usize = 8;

/// `PUT /backup/plans` — `CreateBackupPlan`'s HTTP binding.
async fn create_plan(router: &MultiServiceRouter, request: &Value) -> (u16, Value) {
    call(router, "PUT", "/backup/plans", Some(&request.to_string())).await
}

/// `POST /backup/plans/{BackupPlanId}` — `UpdateBackupPlan`'s HTTP binding.
async fn update_plan(router: &MultiServiceRouter, plan_id: &str, plan: &Value) -> (u16, Value) {
    call(
        router,
        "POST",
        &format!("/backup/plans/{plan_id}"),
        Some(&json!({"BackupPlan": plan}).to_string()),
    )
    .await
}

/// `GET /backup/plans/{BackupPlanId}` — `GetBackupPlan`'s HTTP binding, with the
/// optional `versionId` query parameter.
async fn get_plan(
    router: &MultiServiceRouter,
    plan_id: &str,
    version_id: Option<&str>,
) -> (u16, Value) {
    let path = match version_id {
        Some(version) => format!("/backup/plans/{plan_id}?versionId={version}"),
        None => format!("/backup/plans/{plan_id}"),
    };
    call(router, "GET", &path, None).await
}

/// `DELETE /backup/plans/{BackupPlanId}` — `DeleteBackupPlan`'s HTTP binding.
async fn delete_plan(router: &MultiServiceRouter, plan_id: &str) -> (u16, Value) {
    call(router, "DELETE", &format!("/backup/plans/{plan_id}"), None).await
}

/// A string member of a JSON object, or the empty string.
fn text(body: &Value, member: &str) -> String {
    body.get(member)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// What a token has to be, whatever call answered it: one fixed-width run of
/// letters and digits — nothing a URL has to escape, so the token a read pins a
/// version with travels as it was answered.
fn assert_token_shape(case: &str, token: &str) {
    assert_eq!(
        token.chars().count(),
        TOKEN_LENGTH,
        "case {case:?}: expected a {TOKEN_LENGTH}-character version token, got {token:?}"
    );
    assert!(
        token.chars().all(|character| character.is_ascii_alphanumeric()),
        "case {case:?}: a version token carries only letters and digits, got {token:?}"
    );
}

/// What a token must NOT carry: any part of the plan it versions. A token drawn
/// from entropy contains neither the plan's id nor any field of that id, nor the
/// plan's name — so reading a token tells a caller nothing about the resource.
fn assert_token_is_unrelated_to_plan(case: &str, token: &str, plan_id: &str, plan_name: &str) {
    assert!(
        !token.contains(plan_id),
        "case {case:?}: the version token {token:?} carries the plan id {plan_id:?}"
    );
    for field in plan_id.split('-').filter(|field| field.len() >= 4) {
        assert!(
            !token.contains(field),
            "case {case:?}: the version token {token:?} carries {field:?} of the plan id \
             {plan_id:?}"
        );
    }
    assert!(
        !token.contains(plan_name),
        "case {case:?}: the version token {token:?} carries the plan name {plan_name:?}"
    );
}

/// The plan bodies the table walks, each with the name its row creates it under.
fn plan_cases() -> Vec<(&'static str, Value)> {
    vec![
        (
            "one rule carrying only its required members",
            json!({"BackupPlan": {
                "BackupPlanName": "minimal-plan",
                "Rules": [{"RuleName": "daily", "TargetBackupVaultName": "plan-vault"}],
            }}),
        ),
        (
            "one scheduled rule, as the behavioural instance carries",
            json!({"BackupPlan": {
                "BackupPlanName": "scheduled-plan",
                "Rules": [{
                    "RuleName": "daily",
                    "TargetBackupVaultName": "plan-vault",
                    "ScheduleExpression": "cron(0 5 * * ? *)",
                    "StartWindowMinutes": 480,
                    "CompletionWindowMinutes": 10080,
                }],
            }}),
        ),
        (
            "three rules on two vaults",
            json!({"BackupPlan": {
                "BackupPlanName": "multi-rule-plan",
                "Rules": [
                    {"RuleName": "hourly", "TargetBackupVaultName": "plan-vault"},
                    {"RuleName": "daily", "TargetBackupVaultName": "plan-vault"},
                    {"RuleName": "weekly", "TargetBackupVaultName": "other-vault"},
                ],
            }}),
        ),
        (
            "advanced backup settings and a creator request id",
            json!({"BackupPlan": {
                "BackupPlanName": "advanced-plan",
                "Rules": [{"RuleName": "daily", "TargetBackupVaultName": "plan-vault"}],
                "AdvancedBackupSettings": [{
                    "ResourceType": "EC2",
                    "BackupOptions": {"WindowsVSS": "enabled"},
                }],
            }, "CreatorRequestId": "version-token-row"}),
        ),
    ]
}

/// Every token this service mints is an opaque fixed-width run of letters and
/// digits that carries nothing of the plan it versions, is unique across plans,
/// and pins exactly its own version on a read.
#[tokio::test]
async fn a_minted_plan_version_token_is_opaque_and_unrelated_to_its_plan() {
    // One router for the whole table, so the uniqueness check spans every plan
    // the account holds rather than one plan at a time.
    let router = router();
    let mut minted: Vec<(&str, String)> = Vec::new();

    for (case, request) in plan_cases() {
        let plan_name = text(&request["BackupPlan"], "BackupPlanName");
        let (status, created) = create_plan(&router, &request).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected CreateBackupPlan 200, got {status} {created:?}"
        );
        let plan_id = text(&created, "BackupPlanId");
        let token = text(&created, "VersionId");

        assert_token_shape(case, &token);
        assert_token_is_unrelated_to_plan(case, &token, &plan_id, &plan_name);

        // The token is the ONE stored value: the read that names it, the read
        // that names none and the version listing all report it, and reading it
        // twice reports it again.
        for attempt in 0..2 {
            let (status, read) = get_plan(&router, &plan_id, Some(&token)).await;
            assert_eq!(
                (status, text(&read, "VersionId").as_str()),
                (200, token.as_str()),
                "case {case:?} / read {attempt}: the version token must pin its own version, \
                 got {status} {read:?}"
            );
        }
        let (status, latest) = get_plan(&router, &plan_id, None).await;
        assert_eq!(
            (status, text(&latest, "VersionId").as_str()),
            (200, token.as_str()),
            "case {case:?}: the plan's only version is its current one, got {status} {latest:?}"
        );
        // A token of the right width that this service did not mint for the
        // plan names no version of it.
        let unheld: String = "A".repeat(TOKEN_LENGTH);
        let (status, refused) = get_plan(&router, &plan_id, Some(&unheld)).await;
        assert_eq!(
            (
                status,
                error_code(&refused).as_str(),
                text(&refused, "Message").as_str(),
                text(&refused, "Code").as_str(),
            ),
            (400, NOT_FOUND, VERSION_UNREADABLE, VERSION_UNREADABLE_CODE),
            "case {case:?}: a token the plan never had must be refused, got {status} {refused:?}"
        );

        minted.push((case, token));
    }

    assert_tokens_are_independent(&minted);
}

/// Each version an update stores gets its OWN token, drawn independently: the
/// plan keeps its id while every token it has answered with is distinct, says
/// nothing about the next one, and still reads back the body stored under it.
#[tokio::test]
async fn every_version_of_one_plan_gets_an_independently_drawn_token() {
    let router = router();
    // The vaults the rules target: an update resolves every rule's
    // `TargetBackupVaultName` in the caller's Region before it stores a version,
    // so the account holds them first.
    for name in ["plan-vault", "other-vault"] {
        let (status, vault) = create_vault(&router, name, "{}").await;
        assert_eq!(
            status, 200,
            "setup: expected CreateBackupVault 200 for {name}, got {status} {vault:?}"
        );
    }
    let (status, created) = create_plan(
        &router,
        &json!({"BackupPlan": {
            "BackupPlanName": "versioned-plan",
            "Rules": [{"RuleName": "daily", "TargetBackupVaultName": "plan-vault"}],
        }}),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateBackupPlan should succeed, got {status} {created:?}"
    );
    let plan_id = text(&created, "BackupPlanId");
    let mut minted: Vec<(&str, String)> = vec![("the version the create stored", text(&created, "VersionId"))];

    // The rows differ in what each update changes about the stored body — the
    // token is drawn per version whatever the body says.
    let updates: Vec<(&str, Value)> = vec![
        (
            "an update that renames the plan",
            json!({
                "BackupPlanName": "versioned-plan-renamed",
                "Rules": [{"RuleName": "daily", "TargetBackupVaultName": "plan-vault"}],
            }),
        ),
        (
            "an update that adds a second rule",
            json!({
                "BackupPlanName": "versioned-plan-renamed",
                "Rules": [
                    {"RuleName": "daily", "TargetBackupVaultName": "plan-vault"},
                    {"RuleName": "weekly", "TargetBackupVaultName": "other-vault"},
                ],
            }),
        ),
        (
            // A body an EARLIER version held, which is not the one in force, so
            // it is another document than the plan's: the version it stores
            // draws its own token rather than reusing the one that body was
            // stored under before. (A body the plan holds NOW stores no version
            // at all — `plan_update_restated_body.rs`.)
            "an update that returns the plan to a body an earlier version held",
            json!({
                "BackupPlanName": "versioned-plan-renamed",
                "Rules": [{"RuleName": "daily", "TargetBackupVaultName": "plan-vault"}],
            }),
        ),
    ];
    for (case, plan) in updates {
        let (status, updated) = update_plan(&router, &plan_id, &plan).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected UpdateBackupPlan 200, got {status} {updated:?}"
        );
        assert_eq!(
            text(&updated, "BackupPlanId"),
            plan_id,
            "case {case:?}: an update keeps the plan's id, got {updated:?}"
        );
        let token = text(&updated, "VersionId");
        assert_token_shape(case, &token);
        assert_token_is_unrelated_to_plan(case, &token, &plan_id, "versioned-plan");
        minted.push((case, token));
    }

    assert_tokens_are_independent(&minted);

    // Every token still reads back the version stored under it, so the tokens
    // address versions rather than only differing from one another.
    for (case, token) in &minted {
        let (status, read) = get_plan(&router, &plan_id, Some(token)).await;
        assert_eq!(
            (status, text(&read, "VersionId").as_str()),
            (200, token.as_str()),
            "case {case:?}: the token {token:?} must still pin its version, got {status} {read:?}"
        );
    }
    // The plan's current version is the one the last update stored, so the
    // token a read that names no version reports is that update's.
    let (status, latest) = get_plan(&router, &plan_id, None).await;
    let (last_case, last_token) = minted.last().expect("the create minted one token");
    assert_eq!(
        (status, text(&latest, "VersionId").as_str()),
        (200, last_token.as_str()),
        "case {last_case:?}: the plan's current version is the last one stored, \
         got {status} {latest:?}"
    );
}

/// The plan's own delete reports the token the service HOLDS for it, and once
/// the deletion is recorded that token pins nothing: the version it named is no
/// longer there to read, so the pinned read is refused like a token the plan
/// never had.
#[tokio::test]
async fn the_delete_of_a_plan_reports_the_token_it_held_and_then_pins_nothing() {
    for (case, request) in plan_cases() {
        let router = router();
        let (status, created) = create_plan(&router, &request).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected CreateBackupPlan 200, got {status} {created:?}"
        );
        let plan_id = text(&created, "BackupPlanId");
        let token = text(&created, "VersionId");

        let (status, deleted) = delete_plan(&router, &plan_id).await;
        assert_eq!(
            (
                status,
                text(&deleted, "BackupPlanId").as_str(),
                text(&deleted, "BackupPlanArn").as_str(),
                text(&deleted, "VersionId").as_str(),
            ),
            (
                200,
                plan_id.as_str(),
                text(&created, "BackupPlanArn").as_str(),
                token.as_str(),
            ),
            "case {case:?}: the delete reports the plan and the token it held, \
             got {status} {deleted:?}"
        );
        assert!(
            deleted["DeletionDate"].is_number(),
            "case {case:?}: the delete reports the instant it recorded, got {deleted:?}"
        );

        let (status, refused) = get_plan(&router, &plan_id, Some(&token)).await;
        assert_eq!(
            (
                status,
                error_code(&refused).as_str(),
                text(&refused, "Message").as_str(),
                text(&refused, "Code").as_str(),
            ),
            (400, NOT_FOUND, VERSION_UNREADABLE, VERSION_UNREADABLE_CODE),
            "case {case:?}: the token of a deleted plan pins no version, got {status} {refused:?}"
        );
    }
}

/// No token repeats another, and none shares a leading run with another: a token
/// drawn from entropy is independent of every token drawn before it, so neither a
/// counter nor the plan's own id can be read out of the sequence.
fn assert_tokens_are_independent(minted: &[(&str, String)]) {
    for (index, (case, token)) in minted.iter().enumerate() {
        for (other_case, other) in &minted[index + 1..] {
            assert_ne!(
                token, other,
                "case {case:?} vs {other_case:?}: two versions must not share a token, \
                 got {token:?}"
            );
            assert_ne!(
                &token[..SHARED_PREFIX],
                &other[..SHARED_PREFIX],
                "case {case:?} vs {other_case:?}: tokens drawn independently share no leading \
                 run, got {token:?} and {other:?}"
            );
        }
    }
}
