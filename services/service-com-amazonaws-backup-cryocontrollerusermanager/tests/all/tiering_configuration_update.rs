// SPDX-License-Identifier: CC0-1.0

//! `UpdateTieringConfiguration` over what `CreateTieringConfiguration` stored.
//!
//! The rules under test:
//!
//! * an update REPLACES the mutable body of the configuration its name
//!   addresses — the vault it applies to and its selections — and every later
//!   read reports the submitted body;
//! * the identity the create minted (the name, its ARN and the creation
//!   instant) survives every update, and the update instant is what the call
//!   rewrites: `LastUpdatedTime` is the create's own instant until an update has
//!   run, then the update's, reported by the update, the read and the listing
//!   alike;
//! * a name the account holds no configuration under — one never created, or
//!   one a delete removed — is the one not-found refusal, so an update never
//!   stores a configuration;
//! * however many updates of one name arrive together, each applies its WHOLE
//!   body: the configuration the reads report is one submitted body, never a
//!   mixture, and at least one of the racing calls succeeds.
//!
//! Every case drives real HTTP through the router; the harness is the shared one
//! in `vault_and_backup_job.rs`, and the route bindings, the create body and the
//! not-found sentence are the ones the sibling tiering suite already states.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::tiering_configuration::{
    create_body, create_tiering, delete_tiering, get_tiering, list_tiering, selection,
    MAX_TIERING_DAYS, MIN_TIERING_DAYS, NOT_FOUND,
};
use crate::vault_and_backup_job::{call, create_vault, error_code, router, string_at, ACCOUNT, REGION};

/// `PUT /tiering-configurations/{TieringConfigurationName}`, as the model binds
/// the update: the name is a label, so the body carries only what may change.
pub(crate) async fn update_tiering(
    router: &MultiServiceRouter,
    name: &str,
    body: &Value,
) -> (u16, Value) {
    call(
        router,
        "PUT",
        &format!("/tiering-configurations/{name}"),
        Some(&body.to_string()),
    )
    .await
}

/// An update body: the vault the configuration applies to and its selections,
/// the two members `TieringConfigurationInputForUpdate` declares.
pub(crate) fn update_body(vault: &str, selections: Value) -> Value {
    json!({
        "TieringConfiguration": {
            "BackupVaultName": vault,
            "ResourceSelection": selections,
        },
    })
}

/// A timestamp member as the wire carries it (epoch seconds, so a NUMBER), or
/// `Value::Null` when the answer carries none.
pub(crate) fn instant_at(body: &Value, member: &str) -> Value {
    body.get(member).cloned().unwrap_or(Value::Null)
}

/// The stored configuration as the read reports it.
pub(crate) async fn read_configuration(router: &MultiServiceRouter, name: &str) -> (u16, Value) {
    let (status, read) = get_tiering(router, name).await;
    let configuration = read
        .get("TieringConfiguration")
        .cloned()
        .unwrap_or(Value::Null);
    (status, if configuration.is_null() { read } else { configuration })
}

/// The listing entry for one name, if the listing reports it.
pub(crate) async fn listed_entry(router: &MultiServiceRouter, name: &str) -> Option<Value> {
    let (_, listed) = list_tiering(router, "").await;
    listed
        .get("TieringConfigurations")
        .and_then(Value::as_array)
        .and_then(|entries| {
            entries
                .iter()
                .find(|entry| string_at(entry, "TieringConfigurationName") == name)
                .cloned()
        })
}

/// An update replaces the stored body and leaves the created identity alone;
/// the read and the listing report the submitted body and the update instant.
///
/// The rows differ along what the rule generalises over: the vault the updated
/// configuration applies to (a named one and the documented `*` wildcard),
/// whether the update widens or narrows the number of selections, how many
/// resources each selection names, and the transition day count — including
/// both published ends of its range.
#[tokio::test]
async fn an_update_replaces_the_stored_body_and_keeps_the_created_identity() {
    let bucket = "arn:aws:s3:::demo-bucket".to_string();
    let other_bucket = "arn:aws:s3:::other-bucket".to_string();
    let cases: Vec<(&str, &str, Value, &str, Value)> = vec![
        (
            "a named vault becomes every vault, at the floor",
            "tiering-vault",
            json!([selection(json!([bucket.clone()]), 90)]),
            "*",
            json!([selection(json!(["*"]), MIN_TIERING_DAYS)]),
        ),
        (
            "every vault becomes a named one, at the ceiling",
            "*",
            json!([selection(json!(["*"]), MIN_TIERING_DAYS)]),
            "tiering-vault",
            json!([selection(json!([bucket.clone()]), MAX_TIERING_DAYS)]),
        ),
        (
            "one selection becomes two",
            "tiering-vault",
            json!([selection(json!([bucket.clone()]), 120)]),
            "tiering-vault",
            json!([
                selection(json!([bucket.clone()]), 365),
                selection(json!([other_bucket.clone()]), 730),
            ]),
        ),
        (
            "two selections become one naming two buckets",
            "tiering-vault",
            json!([
                selection(json!([bucket.clone()]), 365),
                selection(json!([other_bucket.clone()]), 730),
            ]),
            "other-vault",
            json!([selection(json!([bucket.clone(), other_bucket.clone()]), 180)]),
        ),
        (
            "only the day count changes",
            "tiering-vault",
            json!([selection(json!([bucket.clone()]), 90)]),
            "tiering-vault",
            json!([selection(json!([bucket.clone()]), 91)]),
        ),
    ];

    for (case, created_vault, created_selections, updated_vault, updated_selections) in cases {
        let router = router();
        for vault in ["tiering-vault", "other-vault"] {
            let (status, created) = create_vault(&router, vault, "{}").await;
            assert_eq!(
                status, 200,
                "case {case:?}: setup CreateBackupVault {vault} should succeed, got {status} \
                 {created:?}"
            );
        }

        let name = "tier_updated";
        let (status, created) = create_tiering(
            &router,
            &create_body(name, created_vault, created_selections.clone()),
        )
        .await;
        assert_eq!(
            (status, error_code(&created).as_str()),
            (200, "<none>"),
            "case {case:?}: setup CreateTieringConfiguration should succeed, got {status} \
             {created:?}"
        );
        let expected_arn =
            format!("arn:aws:backup:{REGION}:{ACCOUNT}:tiering-configuration:{name}");
        let creation_time = instant_at(&created, "CreationTime");
        assert!(
            creation_time.is_number(),
            "case {case:?}: the create reports when it stored the configuration, got {created:?}"
        );

        // No update has rewritten the configuration yet, so the instant it last
        // changed is the instant the create stored it.
        let (_, before) = read_configuration(&router, name).await;
        assert_eq!(
            instant_at(&before, "LastUpdatedTime"),
            creation_time.clone(),
            "case {case:?}: a configuration no update has rewritten reports the create's own \
             instant, got {before:?}"
        );

        let (status, updated) = update_tiering(
            &router,
            name,
            &update_body(updated_vault, updated_selections.clone()),
        )
        .await;
        assert_eq!(
            (status, error_code(&updated).as_str()),
            (200, "<none>"),
            "case {case:?}: expected UpdateTieringConfiguration 200, got {status} {updated:?}"
        );
        assert_eq!(
            (
                string_at(&updated, "TieringConfigurationName").as_str(),
                string_at(&updated, "TieringConfigurationArn").as_str(),
                instant_at(&updated, "CreationTime")
            ),
            (name, expected_arn.as_str(), creation_time.clone()),
            "case {case:?}: an update must keep the created identity, got {updated:?}"
        );
        let last_updated = instant_at(&updated, "LastUpdatedTime");
        assert!(
            last_updated.is_number(),
            "case {case:?}: an update must report when it rewrote the configuration, got \
             {updated:?}"
        );

        let (status, read) = read_configuration(&router, name).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the updated configuration must be readable, got {status} {read:?}"
        );
        assert_eq!(
            (
                string_at(&read, "TieringConfigurationName").as_str(),
                string_at(&read, "TieringConfigurationArn").as_str(),
                string_at(&read, "BackupVaultName").as_str(),
                read.get("ResourceSelection").cloned().unwrap_or(Value::Null),
                instant_at(&read, "CreationTime"),
                instant_at(&read, "LastUpdatedTime")
            ),
            (
                name,
                expected_arn.as_str(),
                updated_vault,
                updated_selections.clone(),
                creation_time.clone(),
                last_updated.clone()
            ),
            "case {case:?}: the read must report the body the update stored, got {read:?}"
        );

        let listed = listed_entry(&router, name)
            .await
            .unwrap_or_else(|| panic!("case {case:?}: the listing must still report {name}"));
        assert_eq!(
            (
                string_at(&listed, "BackupVaultName").as_str(),
                instant_at(&listed, "CreationTime"),
                instant_at(&listed, "LastUpdatedTime")
            ),
            (updated_vault, creation_time, last_updated),
            "case {case:?}: the listing must agree with the update, got {listed:?}"
        );
    }
}

/// A second update replaces the body the first one stored, and each update
/// reports an instant no earlier than the one before it: the stored body is
/// always the LAST one submitted, never a merge of the updates.
#[tokio::test]
async fn each_update_replaces_the_body_the_previous_one_stored() {
    let router = router();
    let (status, created) = create_vault(&router, "tiering-vault", "{}").await;
    assert_eq!(
        status, 200,
        "setup: CreateBackupVault should succeed, got {status} {created:?}"
    );
    let name = "tier_rewritten";
    let (status, created) = create_tiering(
        &router,
        &create_body(
            name,
            "tiering-vault",
            json!([selection(json!(["*"]), MIN_TIERING_DAYS)]),
        ),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateTieringConfiguration should succeed, got {status} {created:?}"
    );

    let bodies = [
        (
            "first update",
            "*",
            json!([selection(json!(["arn:aws:s3:::first-bucket"]), 100)]),
        ),
        (
            "second update",
            "tiering-vault",
            json!([
                selection(json!(["arn:aws:s3:::second-bucket"]), 200),
                selection(json!(["arn:aws:s3:::third-bucket"]), 300),
            ]),
        ),
        (
            "third update",
            "tiering-vault",
            json!([selection(json!(["*"]), MAX_TIERING_DAYS)]),
        ),
    ];
    let mut previous_update: Option<f64> = None;
    for (case, vault, selections) in bodies {
        let (status, updated) = update_tiering(&router, name, &update_body(vault, selections.clone()))
            .await;
        assert_eq!(
            (status, error_code(&updated).as_str()),
            (200, "<none>"),
            "case {case:?}: expected UpdateTieringConfiguration 200, got {status} {updated:?}"
        );
        let instant = updated
            .get("LastUpdatedTime")
            .and_then(Value::as_f64)
            .unwrap_or_else(|| panic!("case {case:?}: an update reports its instant, got {updated:?}"));
        if let Some(earlier) = previous_update {
            assert!(
                instant >= earlier,
                "case {case:?}: a later update's instant must not precede an earlier one's, got \
                 {instant} after {earlier}"
            );
        }
        previous_update = Some(instant);

        let (_, read) = read_configuration(&router, name).await;
        assert_eq!(
            (
                string_at(&read, "BackupVaultName").as_str(),
                read.get("ResourceSelection").cloned().unwrap_or(Value::Null)
            ),
            (vault, selections.clone()),
            "case {case:?}: the read must report only the last submitted body, got {read:?}"
        );
    }
}

/// A name the account holds no configuration under is the one not-found
/// refusal, and a refused update stores nothing.
///
/// The rows differ along how the name came to hold no configuration: never
/// created, created under another name, and created then deleted.
#[tokio::test]
async fn an_update_of_a_name_no_configuration_holds_is_refused_and_stores_nothing() {
    let cases: [(&str, Option<&str>, bool); 3] = [
        ("no configuration was ever created", None, false),
        ("another name holds the configuration", Some("tier_other"), false),
        ("the configuration was deleted", Some("tier_absent"), true),
    ];
    for (case, created, delete_it) in cases {
        let router = router();
        let (status, vault) = create_vault(&router, "tiering-vault", "{}").await;
        assert_eq!(
            status, 200,
            "case {case:?}: setup CreateBackupVault should succeed, got {status} {vault:?}"
        );
        if let Some(created) = created {
            let (status, answered) = create_tiering(
                &router,
                &create_body(
                    created,
                    "tiering-vault",
                    json!([selection(json!(["*"]), MIN_TIERING_DAYS)]),
                ),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: setup create should succeed, got {status} {answered:?}"
            );
            if delete_it {
                let (status, deleted) = delete_tiering(&router, created).await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: setup delete should succeed, got {status} {deleted:?}"
                );
            }
        }

        let (status, refused) = update_tiering(
            &router,
            "tier_absent",
            &update_body("tiering-vault", json!([selection(json!(["*"]), 365)])),
        )
        .await;
        assert_eq!(
            (
                status,
                error_code(&refused).as_str(),
                string_at(&refused, "Message").as_str()
            ),
            (400, "ResourceNotFoundException", NOT_FOUND),
            "case {case:?}: an update of a name nothing holds is refused, got {status} {refused:?}"
        );
        assert!(
            listed_entry(&router, "tier_absent").await.is_none(),
            "case {case:?}: a refused update must store no configuration"
        );
        let (status, read) = get_tiering(&router, "tier_absent").await;
        assert_eq!(
            (status, error_code(&read).as_str()),
            (400, "ResourceNotFoundException"),
            "case {case:?}: the name must still hold no configuration, got {status} {read:?}"
        );
    }
}

/// However many updates of one name arrive together, every one of them applies
/// its whole body: at least one succeeds, and the configuration the reads
/// report is one of the submitted bodies rather than a mixture of several.
///
/// The rows differ in how many updates race for the one configuration.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_updates_of_one_name_each_apply_a_whole_body() {
    for racers in [2_usize, 5, 9] {
        let router = Arc::new(router());
        let (status, vault) = create_vault(&router, "tiering-vault", "{}").await;
        assert_eq!(
            status, 200,
            "case {racers} racers: setup CreateBackupVault should succeed, got {status} {vault:?}"
        );
        let name = "tier_contended_update";
        let (status, created) = create_tiering(
            &router,
            &create_body(
                name,
                "tiering-vault",
                json!([selection(json!(["*"]), MIN_TIERING_DAYS)]),
            ),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {racers} racers: setup create should succeed, got {status} {created:?}"
        );

        // Each racer submits a body only it submits, so the stored body
        // identifies which update the reads report.
        let bodies: Vec<Value> = (0..racers)
            .map(|racer| {
                update_body(
                    if racer % 2 == 0 { "*" } else { "tiering-vault" },
                    json!([selection(
                        json!([format!("arn:aws:s3:::bucket-{racer}")]),
                        MIN_TIERING_DAYS + racer as i64
                    )]),
                )
            })
            .collect();

        let mut pending = Vec::new();
        for body in bodies.clone() {
            let router = Arc::clone(&router);
            pending.push(tokio::spawn(async move {
                update_tiering(&router, name, &body).await
            }));
        }
        let mut answers: Vec<(u16, Value)> = Vec::with_capacity(racers);
        for task in pending {
            answers.push(task.await.expect("a racing update to finish"));
        }

        let winners: Vec<&(u16, Value)> =
            answers.iter().filter(|(status, _)| *status == 200).collect();
        assert!(
            !winners.is_empty(),
            "case {racers} racers: at least one concurrent update must succeed, got {answers:?}"
        );
        // A guard, not an exercised case: every racer applies its body under
        // the state lock, so none is refused. It pins the only refusal this
        // operation models for a contended update, so a later change that
        // starts losing writers has to lose them as the modelled conflict.
        for (status, answer) in answers.iter().filter(|(status, _)| *status != 200) {
            assert_eq!(
                (*status, error_code(answer).as_str()),
                (409, "ConflictException"),
                "case {racers} racers: an update that did not apply may only be refused as a \
                 conflicting action, got {status} {answer:?}"
            );
        }

        // The stored configuration is ONE submitted body: its vault and its
        // selections come from the same update.
        let (status, read) = read_configuration(&router, name).await;
        assert_eq!(
            status, 200,
            "case {racers} racers: the configuration must be readable, got {status} {read:?}"
        );
        let stored = update_body(
            &string_at(&read, "BackupVaultName"),
            read.get("ResourceSelection").cloned().unwrap_or(Value::Null),
        );
        assert!(
            bodies.contains(&stored),
            "case {racers} racers: the stored body must be one submitted body, got {read:?}"
        );
        assert!(
            instant_at(&read, "LastUpdatedTime").is_number(),
            "case {racers} racers: the applied update must be recorded, got {read:?}"
        );
    }
}
