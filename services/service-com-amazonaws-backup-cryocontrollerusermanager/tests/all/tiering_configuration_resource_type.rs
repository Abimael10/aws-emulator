// SPDX-License-Identifier: CC0-1.0

//! The TYPE of resource a tiering configuration's selections may name, over the
//! two calls that store such a body (`CreateTieringConfiguration`,
//! `UpdateTieringConfiguration`).
//!
//! The rule under test: a tiering configuration tiers down ONE type of resource
//! — "the type of Amazon Web Services resource; for example, `S3` for Amazon S3.
//! For tiering configurations, this is currently limited to `S3`" (model doc for
//! `ResourceSelection$ResourceType`) — so a body carrying a selection that names
//! any other type is refused 400 `InvalidParameterValueException` with the
//! condition's own token, and the store is left exactly as it was: a refused
//! create stores no configuration under the name it asked for, and a refused
//! update leaves the stored body and its instants alone.
//!
//! The refused rows are drawn from the resource-type catalog this emulator
//! reports through `GetSupportedResourceTypes`
//! (`crate::state::SUPPORTED_RESOURCE_TYPES`, which is also what
//! `smithy_query(query="doc", shape="ResourceType")` documents), plus a type
//! outside it and the one that differs from the supported type only in case —
//! and every table is paired with the SERVED answer for the one type tiering
//! does support, so the refusal is the TYPE's and not the call's.
//!
//! Every case drives real HTTP through the router; the harness is the shared one
//! in `vault_and_backup_job.rs`, and the route bindings and bodies are the ones
//! the sibling tiering suites already state.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::tiering_configuration::{
    create_body, create_tiering, get_tiering, list_tiering, selection, selection_of_type,
    MAX_TIERING_DAYS, MIN_TIERING_DAYS, NOT_FOUND, TIERING_RESOURCE_TYPE,
};
use crate::tiering_configuration_update::{
    instant_at, read_configuration, update_body, update_tiering,
};
use crate::vault_and_backup_job::{create_vault, error_code, router, string_at};

/// The sentence AWS states for "a selection names a type of resource tiering
/// does not support" — fixed text, naming neither the type submitted nor the
/// configuration — with the token it carries for the condition.
const UNSUPPORTED_TYPE_MESSAGE: &str = "Tiering configuration is only supported for S3";
const UNSUPPORTED_TYPE_CODE: &str = "ERROR_3C05";

/// The vault every configuration in this suite applies to.
const VAULT: &str = "tiering-vault";

/// Types a selection may NOT name. The first rows are the other types this
/// emulator reports through `GetSupportedResourceTypes` and the model's
/// `ResourceType` documentation — a type AWS Backup backs up but does not tier
/// — written in the spellings that member's `@pattern`
/// (`^[a-zA-Z0-9\-\_\.]{1,50}$`) admits; the last two are a type the catalog
/// does not name at all and the supported type spelled in another case, which
/// the catalog spells `S3`.
const OTHER_RESOURCE_TYPES: [(&str, &str); 9] = [
    ("a relational database", "RDS"),
    ("an Aurora cluster", "Aurora"),
    ("a key-value table", "DynamoDB"),
    ("a block volume", "EBS"),
    ("an instance", "EC2"),
    ("a file system", "EFS"),
    ("a stack", "CloudFormation"),
    ("a type the catalog does not name", "Lightsail"),
    ("the supported type in another case", "s3"),
];

/// A body member as a string, or `"<unset>"` when the member is absent.
fn member_or_unset(body: &Value, name: &str) -> String {
    body.get(name)
        .and_then(Value::as_str)
        .unwrap_or("<unset>")
        .to_string()
}

/// The (status, `__type`, `Code`, `Message`) an answer reports, as one tuple so
/// an assertion names all four at once.
fn reported(status: u16, body: &Value) -> (u16, String, String, String) {
    (
        status,
        error_code(body),
        member_or_unset(body, "Code"),
        member_or_unset(body, "Message"),
    )
}

/// What this suite expects for the unsupported-type refusal.
fn unsupported_type_refusal() -> (u16, String, String, String) {
    (
        400,
        "InvalidParameterValueException".to_string(),
        UNSUPPORTED_TYPE_CODE.to_string(),
        UNSUPPORTED_TYPE_MESSAGE.to_string(),
    )
}

/// The names the listing reports.
fn listed_names(page: &Value) -> Vec<String> {
    page.get("TieringConfigurations")
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .map(|entry| string_at(entry, "TieringConfigurationName"))
                .collect()
        })
        .unwrap_or_default()
}

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

/// A create whose selection names a type other than the supported one is
/// refused, and nothing is stored under the name it asked for.
///
/// The rows span the types a selection may not name; the one it may is served by
/// the same emulator, under the same name, right after the refusal.
#[tokio::test]
async fn a_create_naming_another_resource_type_is_refused_and_stores_nothing() {
    for (case, resource_type) in OTHER_RESOURCE_TYPES {
        let router = vaulted_router().await;
        let name = "tier_typed";
        let body = create_body(
            name,
            VAULT,
            json!([selection_of_type(
                json!(["arn:aws:s3:::demo-bucket"]),
                MIN_TIERING_DAYS,
                resource_type
            )]),
        );
        let (status, refused) = create_tiering(&router, &body).await;
        assert_eq!(
            reported(status, &refused),
            unsupported_type_refusal(),
            "case {case:?} ({resource_type:?}): expected the unsupported-type refusal, got \
             {status} {refused:?}"
        );

        // The refused create stored nothing, so the name it asked for still
        // holds no configuration and the listing reports none.
        let (status, read) = get_tiering(&router, name).await;
        assert_eq!(
            (
                status,
                error_code(&read).as_str(),
                string_at(&read, "Message").as_str()
            ),
            (400, "ResourceNotFoundException", NOT_FOUND),
            "case {case:?}: a refused create must store no configuration, got {status} {read:?}"
        );
        let (_, listed) = list_tiering(&router, "").await;
        assert!(
            listed_names(&listed).is_empty(),
            "case {case:?}: a refused create must leave the listing empty, got {listed:?}"
        );

        // Non-vacuity: the SAME name with the supported type is stored, so what
        // was refused is the type and not the call.
        let supported = create_body(
            name,
            VAULT,
            json!([selection(
                json!(["arn:aws:s3:::demo-bucket"]),
                MIN_TIERING_DAYS
            )]),
        );
        let (status, accepted) = create_tiering(&router, &supported).await;
        assert_eq!(
            (status, member_or_unset(&accepted, "Code")),
            (200, "<unset>".to_string()),
            "case {case:?}: the supported type must still be stored, got {status} {accepted:?}"
        );
    }
}

/// An update whose selection names a type other than the supported one is
/// refused, and the stored configuration — its body and its instants — is left
/// exactly as it was.
#[tokio::test]
async fn an_update_naming_another_resource_type_is_refused_and_rewrites_nothing() {
    for (case, resource_type) in OTHER_RESOURCE_TYPES {
        let router = vaulted_router().await;
        let name = "tier_typed_update";
        let stored_selections = json!([selection(json!(["arn:aws:s3:::demo-bucket"]), 90)]);
        let (status, created) =
            create_tiering(&router, &create_body(name, VAULT, stored_selections.clone())).await;
        assert_eq!(
            status, 200,
            "case {case:?}: setup CreateTieringConfiguration should succeed, got {status} \
             {created:?}"
        );
        let (_, before) = read_configuration(&router, name).await;

        let (status, refused) = update_tiering(
            &router,
            name,
            &update_body(
                "*",
                json!([selection_of_type(json!(["*"]), MAX_TIERING_DAYS, resource_type)]),
            ),
        )
        .await;
        assert_eq!(
            reported(status, &refused),
            unsupported_type_refusal(),
            "case {case:?} ({resource_type:?}): expected the unsupported-type refusal, got \
             {status} {refused:?}"
        );

        let (status, after) = read_configuration(&router, name).await;
        assert_eq!(status, 200, "case {case:?}: the configuration must still be readable");
        assert_eq!(
            (
                string_at(&after, "BackupVaultName"),
                after.get("ResourceSelection").cloned().unwrap_or(Value::Null),
                instant_at(&after, "CreationTime"),
                instant_at(&after, "LastUpdatedTime")
            ),
            (
                VAULT.to_string(),
                stored_selections.clone(),
                instant_at(&before, "CreationTime"),
                instant_at(&before, "LastUpdatedTime")
            ),
            "case {case:?}: a refused update must rewrite nothing, got {after:?}"
        );

        // Non-vacuity: the supported type rewrites the same configuration.
        let (status, accepted) = update_tiering(
            &router,
            name,
            &update_body("*", json!([selection(json!(["*"]), MAX_TIERING_DAYS)])),
        )
        .await;
        assert_eq!(
            (status, member_or_unset(&accepted, "Code")),
            (200, "<unset>".to_string()),
            "case {case:?}: the supported type must still be stored, got {status} {accepted:?}"
        );
    }
}

/// The type is read off EVERY selection the body carries, wherever the one that
/// names another type sits: a body is accepted only when all of its selections
/// name the supported type.
///
/// The rows differ in how many selections the body carries and which of them
/// names the other type (the first, a middle one, the last).
#[tokio::test]
async fn every_selection_of_the_body_is_read_for_its_resource_type() {
    let buckets = [
        "arn:aws:s3:::first-bucket",
        "arn:aws:s3:::second-bucket",
        "arn:aws:s3:::third-bucket",
    ];
    let cases: [(&str, usize, usize); 4] = [
        ("two selections, the first names another type", 2, 0),
        ("two selections, the last names another type", 2, 1),
        ("three selections, the middle one names another type", 3, 1),
        ("three selections, the last names another type", 3, 2),
    ];

    for (case, count, offending) in cases {
        let router = vaulted_router().await;
        let name = "tier_many_selections";

        let mixed: Vec<Value> = (0..count)
            .map(|index| {
                let days = MIN_TIERING_DAYS + index as i64;
                if index == offending {
                    selection_of_type(json!([buckets[index]]), days, "DynamoDB")
                } else {
                    selection(json!([buckets[index]]), days)
                }
            })
            .collect();
        let (status, refused) =
            create_tiering(&router, &create_body(name, VAULT, json!(mixed))).await;
        assert_eq!(
            reported(status, &refused),
            unsupported_type_refusal(),
            "case {case:?}: one selection outside the supported type refuses the whole body, got \
             {status} {refused:?}"
        );

        // The same body with every selection naming the supported type is
        // stored, and the read reports all of them.
        let all_supported: Vec<Value> = (0..count)
            .map(|index| selection(json!([buckets[index]]), MIN_TIERING_DAYS + index as i64))
            .collect();
        let (status, accepted) = create_tiering(
            &router,
            &create_body(name, VAULT, json!(all_supported.clone())),
        )
        .await;
        assert_eq!(
            (status, member_or_unset(&accepted, "Code")),
            (200, "<unset>".to_string()),
            "case {case:?}: a body of {count} supported selections must be stored, got {status} \
             {accepted:?}"
        );
        let (_, read) = read_configuration(&router, name).await;
        assert_eq!(
            read.get("ResourceSelection").cloned().unwrap_or(Value::Null),
            json!(all_supported),
            "case {case:?}: the read must report every stored selection, got {read:?}"
        );
        let stored_selections_read = read
            .get("ResourceSelection")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for entry in &stored_selections_read {
            assert_eq!(
                string_at(entry, "ResourceType"),
                TIERING_RESOURCE_TYPE,
                "case {case:?}: every stored selection names the supported type, got {entry:?}"
            );
        }
    }
}
