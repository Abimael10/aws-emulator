// SPDX-License-Identifier: CC0-1.0

//! The INSTANTS a tiering configuration carries, over the calls that report one
//! (`CreateTieringConfiguration`, `GetTieringConfiguration`,
//! `ListTieringConfigurations`, `UpdateTieringConfiguration`).
//!
//! The rule under test: a stored tiering configuration carries a last-change
//! instant from the moment it is stored — "the date and time a tiering
//! configuration was updated, in Unix format and Coordinated Universal Time
//! (UTC)… accurate to milliseconds" (model doc for
//! `TieringConfiguration$LastUpdatedTime`, declared on
//! `TieringConfigurationsListMember` too) — so every read of a configuration
//! reports one: the create's own instant until an update rewrites it, and the
//! update's instant afterwards. The single-resource read and the listing project
//! the same stored record, so the two never disagree about either instant.
//!
//! Every case drives real HTTP through the router; the harness is the shared one
//! in `vault_and_backup_job.rs`, and the route bindings and bodies are the ones
//! the sibling tiering suites already state.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::tiering_configuration::{
    create_body, create_tiering, delete_tiering, get_tiering, list_tiering, selection,
    MAX_TIERING_DAYS, MIN_TIERING_DAYS,
};
use crate::tiering_configuration_update::{
    instant_at, listed_entry, read_configuration, update_body, update_tiering,
};
use crate::vault_and_backup_job::{create_vault, error_code, router};

/// The vault every configuration in this suite applies to.
const VAULT: &str = "tiering-vault";

/// Create the vault the configurations here apply to.
async fn vaulted_router() -> MultiServiceRouter {
    let router = router();
    let (status, created) = create_vault(&router, VAULT, "{}").await;
    assert_eq!(
        status, 200,
        "setup: CreateBackupVault {VAULT} should succeed, got {status} {created:?}"
    );
    router
}

/// A freshly created configuration reports a last-change instant — the instant
/// the create stored it — through the single-resource read and the listing
/// alike, and an update moves it on while leaving the creation instant alone.
///
/// The rows differ along what the rule generalises over: the vault the
/// configuration applies to (a named one and the documented `*` wildcard), the
/// number of selections, the resources each names, and the transition day count
/// — including both published ends of its range.
#[tokio::test]
async fn a_created_configuration_reports_the_instant_it_was_stored() {
    let bucket = "arn:aws:s3:::demo-bucket";
    let other_bucket = "arn:aws:s3:::other-bucket";
    let cases: Vec<(&str, &str, &str, Value)> = vec![
        (
            "every vault, every resource, at the floor",
            "tier_instant_all",
            "*",
            json!([selection(json!(["*"]), MIN_TIERING_DAYS)]),
        ),
        (
            "one vault, one bucket, at the ceiling",
            "tier_instant_one",
            VAULT,
            json!([selection(json!([bucket]), MAX_TIERING_DAYS)]),
        ),
        (
            "two selections with different day counts",
            "tier_instant_two",
            VAULT,
            json!([
                selection(json!([bucket]), 90),
                selection(json!([other_bucket]), 365),
            ]),
        ),
        (
            "one selection naming two buckets",
            "tier_instant_pair",
            VAULT,
            json!([selection(json!([bucket, other_bucket]), 180)]),
        ),
    ];

    for (case, name, vault, selections) in cases {
        let router = vaulted_router().await;
        let (status, created) =
            create_tiering(&router, &create_body(name, vault, selections.clone())).await;
        assert_eq!(
            (status, error_code(&created).as_str()),
            (200, "<none>"),
            "case {case:?}: expected CreateTieringConfiguration 200, got {status} {created:?}"
        );
        let creation_time = instant_at(&created, "CreationTime");
        assert!(
            creation_time.is_number(),
            "case {case:?}: the create reports when it stored the configuration, got {created:?}"
        );

        // The read reports BOTH instants, and the configuration nothing has
        // rewritten last changed when it was stored.
        let (status, read) = read_configuration(&router, name).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the configuration must be readable, got {status} {read:?}"
        );
        assert_eq!(
            (
                instant_at(&read, "CreationTime"),
                instant_at(&read, "LastUpdatedTime")
            ),
            (creation_time.clone(), creation_time.clone()),
            "case {case:?}: the read must report the create's instant as both instants, got \
             {read:?}"
        );

        // The listing projects the same stored record, so it reports the same
        // two instants.
        let listed = listed_entry(&router, name)
            .await
            .unwrap_or_else(|| panic!("case {case:?}: the listing must report {name}"));
        assert_eq!(
            (
                instant_at(&listed, "CreationTime"),
                instant_at(&listed, "LastUpdatedTime")
            ),
            (creation_time.clone(), creation_time.clone()),
            "case {case:?}: the listing must agree with the read, got {listed:?}"
        );

        // An update rewrites the last-change instant and leaves the creation
        // instant alone; both reads follow.
        let (status, updated) = update_tiering(
            &router,
            name,
            &update_body(VAULT, json!([selection(json!(["*"]), MIN_TIERING_DAYS)])),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected UpdateTieringConfiguration 200, got {status} {updated:?}"
        );
        let last_updated = instant_at(&updated, "LastUpdatedTime");
        let (created_seconds, updated_seconds) = (
            creation_time.as_f64().unwrap_or_default(),
            last_updated.as_f64().unwrap_or_default(),
        );
        assert!(
            updated_seconds >= created_seconds,
            "case {case:?}: the update's instant must not precede the create's, got \
             {updated_seconds} after {created_seconds}"
        );
        let (_, read) = read_configuration(&router, name).await;
        let listed = listed_entry(&router, name)
            .await
            .unwrap_or_else(|| panic!("case {case:?}: the listing must still report {name}"));
        assert_eq!(
            (
                instant_at(&read, "CreationTime"),
                instant_at(&read, "LastUpdatedTime"),
                instant_at(&listed, "CreationTime"),
                instant_at(&listed, "LastUpdatedTime")
            ),
            (
                creation_time.clone(),
                last_updated.clone(),
                creation_time,
                last_updated
            ),
            "case {case:?}: both reads must report the update's instant beside the create's, got \
             {read:?} and {listed:?}"
        );
    }
}

/// Every entry of a listing page carries both instants, however many
/// configurations the page reports and whichever of them an update has
/// rewritten.
#[tokio::test]
async fn every_listed_configuration_carries_both_instants() {
    let router = vaulted_router().await;
    let names = ["tier_listed_a", "tier_listed_b", "tier_listed_c"];
    for name in names {
        let (status, created) = create_tiering(
            &router,
            &create_body(
                name,
                VAULT,
                json!([selection(json!(["*"]), MIN_TIERING_DAYS)]),
            ),
        )
        .await;
        assert_eq!(
            status, 200,
            "setup {name}: the create should succeed, got {status} {created:?}"
        );
    }
    // One of the three has been rewritten, so the page reports both an
    // untouched configuration and an updated one.
    let (status, updated) = update_tiering(
        &router,
        names[1],
        &update_body("*", json!([selection(json!(["*"]), MAX_TIERING_DAYS)])),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup {}: the update should succeed, got {status} {updated:?}",
        names[1]
    );

    let (status, page) = list_tiering(&router, "").await;
    assert_eq!(status, 200, "expected the listing to answer 200, got {page:?}");
    let entries = page
        .get("TieringConfigurations")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        entries.len(),
        names.len(),
        "the listing must report every stored configuration, got {page:?}"
    );
    for entry in &entries {
        assert!(
            instant_at(entry, "CreationTime").is_number()
                && instant_at(entry, "LastUpdatedTime").is_number(),
            "every listed configuration carries both instants, got {entry:?}"
        );
        // The listing entry agrees with the read of the same configuration.
        let name = entry
            .get("TieringConfigurationName")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let (_, read) = read_configuration(&router, &name).await;
        assert_eq!(
            (
                instant_at(entry, "CreationTime"),
                instant_at(entry, "LastUpdatedTime")
            ),
            (
                instant_at(&read, "CreationTime"),
                instant_at(&read, "LastUpdatedTime")
            ),
            "the listing and the read of {name} must agree, got {entry:?} and {read:?}"
        );
    }

    // A configuration a delete removed is reported by neither, so the instants
    // above belong to stored configurations.
    let (status, deleted) = delete_tiering(&router, names[0]).await;
    assert_eq!(
        status, 200,
        "expected DeleteTieringConfiguration 200, got {status} {deleted:?}"
    );
    assert!(
        listed_entry(&router, names[0]).await.is_none(),
        "a deleted configuration must leave the listing"
    );
    let (status, read) = get_tiering(&router, names[0]).await;
    assert_eq!(
        (status, error_code(&read).as_str()),
        (400, "ResourceNotFoundException"),
        "a deleted configuration must be gone from the read too, got {status} {read:?}"
    );
}
