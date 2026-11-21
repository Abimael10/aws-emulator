// SPDX-License-Identifier: CC0-1.0

//! `UntagResource`: the removal drops exactly the keys the request names from
//! the stored resource the ARN resolves to, and nothing else.
//!
//! The rules under test:
//!
//! * a removal takes the named keys off the SAME stored record the resource's
//!   own create wrote, the tag write merges into and the tag read reports — so
//!   the tag read no longer reports them, whichever kind of resource the ARN
//!   names and however many keys the request names;
//! * a key the request does not name survives, and a key the resource does not
//!   carry is already absent, so naming it changes nothing and the call still
//!   succeeds: repeating a removal is a no-op, as the model's `@idempotent`
//!   says;
//! * a removal reaches only the resource its ARN names;
//! * an ARN that is not an ARN is a bad parameter value, and one that names no
//!   taggable resource of this account — including a deleted backup plan — is
//!   the one not-found refusal, and such a refusal removes nothing.
//!
//! Every case drives real HTTP through the router; the harness is the shared one
//! in `vault_and_backup_job.rs`, and the tag write and read are the ones
//! `resource_tags.rs` exposes to this test binary.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::framework::create_framework;
use crate::recovery_point_deletion::{router_with_engine, start_point};
use crate::resource_tags::{list_tags, tag_resource, NOT_FOUND};
use crate::vault_and_backup_job::{call, create_vault, error_code, reported_tags, string_at,
    vault_arn, ACCOUNT, REGION};

/// `POST /untag/{ResourceArn}`, as the model binds the removal.
async fn untag_resource(
    router: &MultiServiceRouter,
    resource_arn: &str,
    tag_key_list: Value,
) -> (u16, Value) {
    call(
        router,
        "POST",
        &format!("/untag/{resource_arn}"),
        Some(&json!({"TagKeyList": tag_key_list}).to_string()),
    )
    .await
}

/// The tag set every resource under test is seeded with, so each row starts from
/// the same stored set and the rows differ only in which keys the removal names.
fn seed() -> Value {
    json!({"team": "backups", "owner": "ops", "stage": "prod", "cost": "1"})
}

/// Write [`seed`] onto `resource_arn` and assert it is what the read reports, so
/// a failed removal case cannot be confused with a failed setup.
async fn seeded(router: &MultiServiceRouter, case: &str, resource_arn: &str) {
    let (status, written) = tag_resource(router, resource_arn, seed()).await;
    assert_eq!(
        (status, error_code(&written).as_str()),
        (200, "<none>"),
        "case {case:?}: setup TagResource should succeed, got {status} {written:?}"
    );
    let (status, read) = list_tags(router, resource_arn, "").await;
    assert_eq!(
        (status, reported_tags(&read)),
        (200, seed()),
        "case {case:?}: setup should leave the seeded set, got {status} {read:?}"
    );
}

/// The removals one resource is walked through, in order: the keys each names
/// and the set the resource must carry afterwards. The rows differ along what
/// the rule generalises over — how many keys are named, whether the resource
/// carries them, and whether any are named at all.
fn removal_steps() -> Vec<(&'static str, Value, Value)> {
    vec![
        (
            "one key the resource carries",
            json!(["owner"]),
            json!({"team": "backups", "stage": "prod", "cost": "1"}),
        ),
        (
            "two keys at once",
            json!(["team", "stage"]),
            json!({"cost": "1"}),
        ),
        (
            "a key the resource does not carry",
            json!(["never-written"]),
            json!({"cost": "1"}),
        ),
        (
            "a carried key beside one that is not",
            json!(["never-written", "also-absent"]),
            json!({"cost": "1"}),
        ),
        (
            "no keys at all",
            json!([]),
            json!({"cost": "1"}),
        ),
        (
            "a key an earlier removal already took",
            json!(["owner"]),
            json!({"cost": "1"}),
        ),
        (
            "the last key the resource carries",
            json!(["cost"]),
            json!({}),
        ),
    ]
}

/// A removal takes exactly the named keys off the stored resource, whichever
/// kind of resource the ARN names, and the tag read follows.
#[tokio::test]
async fn a_removal_drops_exactly_the_named_keys_from_the_resource_it_names() {
    let (router, _engine) = router_with_engine();
    // Four KINDS of taggable resource, which is the other axis the rule
    // generalises over: a vault the create had tagged, a recovery point, a
    // backup plan and an audit framework.
    let (status, created) = create_vault(
        &router,
        "removal-vault",
        &json!({"BackupVaultTags": {"team": "backups"}}).to_string(),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateBackupVault should succeed, got {status} {created:?}"
    );
    let (status, sibling) = create_vault(&router, "untouched-vault", "{}").await;
    assert_eq!(
        status, 200,
        "setup: the sibling vault should be created, got {status} {sibling:?}"
    );
    let point = start_point(
        &router,
        "removal-vault",
        &format!("arn:aws:dynamodb:{REGION}:{ACCOUNT}:table/demo"),
        None,
    )
    .await;
    let (status, plan) = call(
        &router,
        "PUT",
        "/backup/plans",
        Some(
            &json!({
                "BackupPlan": {
                    "BackupPlanName": "removal-plan",
                    "Rules": [{"RuleName": "daily", "TargetBackupVaultName": "removal-vault"}],
                },
                "BackupPlanTags": {"team": "backups"},
            })
            .to_string(),
        ),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateBackupPlan should succeed, got {status} {plan:?}"
    );
    let (status, framework) = create_framework(
        &router,
        &json!({
            "FrameworkName": "RemovalFramework",
            "FrameworkControls": [{"ControlName": "BACKUP_RECOVERY_POINT_ENCRYPTED"}],
            "FrameworkTags": {"team": "backups"},
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateFramework should succeed, got {status} {framework:?}"
    );

    let resources: Vec<(&str, String)> = vec![
        ("a backup vault", vault_arn(REGION, "removal-vault")),
        ("a recovery point", point),
        ("a backup plan", string_at(&plan, "BackupPlanArn")),
        ("an audit framework", string_at(&framework, "FrameworkArn")),
    ];
    let sibling_arn = vault_arn(REGION, "untouched-vault");
    seeded(&router, "the sibling vault", &sibling_arn).await;

    for (kind, resource_arn) in resources {
        assert!(
            !resource_arn.is_empty(),
            "case {kind:?}: setup should report the resource's ARN"
        );
        seeded(&router, kind, &resource_arn).await;
        for (step, keys, expected) in removal_steps() {
            let case = format!("{kind} / {step}");
            let (status, removed) = untag_resource(&router, &resource_arn, keys.clone()).await;
            assert_eq!(
                (status, error_code(&removed).as_str()),
                (200, "<none>"),
                "case {case:?}: expected UntagResource 200, got {status} {removed:?}"
            );
            let (status, read) = list_tags(&router, &resource_arn, "").await;
            assert_eq!(
                (status, error_code(&read).as_str()),
                (200, "<none>"),
                "case {case:?}: expected ListTags 200, got {status} {read:?}"
            );
            assert_eq!(
                reported_tags(&read),
                expected,
                "case {case:?}: removing {keys:?} must leave {expected:?}, got {read:?}"
            );
        }
        // The removals reached only the resource they named.
        let (_, other) = list_tags(&router, &sibling_arn, "").await;
        assert_eq!(
            reported_tags(&other),
            seed(),
            "case {kind:?}: a removal must not reach another resource's tags, got {other:?}"
        );
    }
}

/// An ARN that names no taggable resource of this account is the one not-found
/// refusal, a value that is not an ARN is a bad parameter value, and neither
/// removes anything.
#[tokio::test]
async fn a_removal_addressing_no_resource_is_refused_and_removes_nothing() {
    let (router, _engine) = router_with_engine();
    let (status, created) = create_vault(&router, "removal-vault", "{}").await;
    assert_eq!(
        status, 200,
        "setup: CreateBackupVault should succeed, got {status} {created:?}"
    );
    let kept_arn = vault_arn(REGION, "removal-vault");
    seeded(&router, "the vault the refusals must not touch", &kept_arn).await;

    // A plan the account held and then deleted: the account no longer holds it,
    // so its tags are not there to remove.
    let (status, plan) = call(
        &router,
        "PUT",
        "/backup/plans",
        Some(
            &json!({
                "BackupPlan": {
                    "BackupPlanName": "deleted-plan",
                    "Rules": [{"RuleName": "daily", "TargetBackupVaultName": "removal-vault"}],
                },
            })
            .to_string(),
        ),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateBackupPlan should succeed, got {status} {plan:?}"
    );
    let deleted_plan_arn = string_at(&plan, "BackupPlanArn");
    let plan_id = string_at(&plan, "BackupPlanId");
    let (status, deleted) = call(
        &router,
        "DELETE",
        &format!("/backup/plans/{plan_id}"),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: DeleteBackupPlan should succeed, got {status} {deleted:?}"
    );

    let absent: Vec<(&str, String)> = vec![
        (
            "a vault that was never created",
            vault_arn(REGION, "never-created"),
        ),
        (
            "a recovery point nothing answers to",
            format!("arn:aws:backup:{REGION}:{ACCOUNT}:recovery-point:does-not-exist"),
        ),
        (
            "a vault of another account",
            format!("arn:aws:backup:{REGION}:111122223333:backup-vault:removal-vault"),
        ),
        (
            // An ARN of another service, with no `/` in its resource field, for
            // the reason `resource_tags.rs` gives for the same row.
            "a resource of another service",
            format!("arn:aws:sns:{REGION}:{ACCOUNT}:backup-notifications"),
        ),
        ("a backup plan the account deleted", deleted_plan_arn),
    ];
    for (case, resource_arn) in absent {
        let (status, refused) = untag_resource(&router, &resource_arn, json!(["team"])).await;
        assert_eq!(
            (
                status,
                error_code(&refused).as_str(),
                string_at(&refused, "Message").as_str()
            ),
            (400, "ResourceNotFoundException", NOT_FOUND),
            "case {case:?}: expected the not-found refusal, got {status} {refused:?}"
        );
    }

    for value in ["not-an-arn", "arn:aws:backup"] {
        let (status, refused) = untag_resource(&router, value, json!(["team"])).await;
        assert_eq!(
            (status, error_code(&refused).as_str()),
            (400, "InvalidParameterValueException"),
            "case {value:?}: expected the parameter refusal, got {status} {refused:?}"
        );
    }

    let (_, read) = list_tags(&router, &kept_arn, "").await;
    assert_eq!(
        reported_tags(&read),
        seed(),
        "a refused removal must leave every stored tag where it was, got {read:?}"
    );
}
