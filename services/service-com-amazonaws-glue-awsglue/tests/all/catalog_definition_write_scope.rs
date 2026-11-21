// SPDX-License-Identifier: CC0-1.0

//! What an `UpdateCatalog` write TOUCHES, driven end to end over the wire through the shared
//! [`crate::wire`] harness.
//!
//! RULE under test: `UpdateCatalog` states the addressed catalog's client-settable definition
//! and nothing beside it — its request models exactly a `CatalogId` and a `CatalogInput`
//! (`smithy_query operation=UpdateCatalog`), so the members of that input are written, the
//! members only the server owns are left as the create made them, the catalog's tag set —
//! which no `CatalogInput` carries and which the tagging operations write — is untouched, and
//! a member `CatalogInput` declares but the `Catalog` shape does not
//! (`OverwriteChildResourcePermissionsWithDefault`, from `smithy_query shape=CatalogInput`
//! against `smithy_query shape=Catalog`) is accepted without being reported back. Stating the
//! same definition twice therefore leaves one definition, and reading it back changes nothing.
//!
//! Pinned HERE: the tag set, read back through `GetTags`; the input-only member; each
//! published value of the two `CatalogInput` enums; and the restate and read-again invariants.
//! WHICH definition a write leaves, member for member, and the refusals it earns are pinned
//! beside this suite in [`crate::catalog_property_updates`], whose create, read and update
//! helpers this suite shares rather than copying.

use serde_json::{json, Value};

use crate::catalog_property_updates::{
    assert_server_owned_members_unchanged, get_catalog, given_linked_catalog_with_tags,
    resource_link, update,
};
use crate::resource_tagging::{read_tags, tags_of};
use crate::wire::{assert_echoed, emulator};

/// The one member a `CatalogInput` declares that the `Catalog` shape does not: a directive
/// about the catalog's CHILD resources, not a property of the catalog, so a read has nothing
/// to report for it. Sources: `smithy_query shape=CatalogInput` declares it,
/// `smithy_query shape=Catalog` does not.
const INPUT_ONLY_MEMBER: &str = "OverwriteChildResourcePermissionsWithDefault";

/// `stated` without the member no read reports, which is what a read is checked against.
fn reported_definition(stated: &Value) -> Value {
    let mut members = stated.clone();
    members
        .as_object_mut()
        .expect("a CatalogInput is a structure")
        .remove(INPUT_ONLY_MEMBER);
    members
}

/// One row: a catalog, the tag set its create recorded, and the definition an update states
/// over it.
struct Row {
    case: &'static str,
    name: &'static str,
    tags: Value,
    stated: Value,
}

/// The definitions an update states, spanning the forms a `CatalogInput` takes: how many
/// parameters it carries, whether it describes the catalog, each published value of the
/// external-data-access enum, the property map, the two Lake Formation default-permission
/// lists, and each published value of the input-only overwrite directive (the value lists of
/// `smithy_query shape=AllowFullTableExternalDataAccessEnum` and
/// `shape=OverwriteChildResourcePermissionsWithDefaultEnum`). Every row pairs that with a tag
/// set of its own size, because the tag set is what the write must leave alone.
fn rows() -> Vec<Row> {
    vec![
        Row {
            // The link states a target OTHER than the one the create stored, so the row turns
            // on the write landing and not on the create's own definition.
            case: "a resource link alone, replacing the one the create stored",
            name: "link-only",
            tags: tags_of(0, "link-only"),
            stated: json!({ "TargetRedshiftCatalog": resource_link("link-only-target") }),
        },
        Row {
            case: "a description and one parameter, over a catalog with one tag",
            name: "described",
            tags: tags_of(1, "described"),
            stated: json!({
                "Description": "stated by the update",
                "Parameters": { "owner": "platform" },
                "TargetRedshiftCatalog": resource_link("described"),
            }),
        },
        Row {
            case: "three parameters and external data access allowed",
            name: "wide-parameters",
            tags: tags_of(3, "wide-parameters"),
            stated: json!({
                "Parameters": { "owner": "platform", "tier": "gold", "classification": "open" },
                "TargetRedshiftCatalog": resource_link("wide-parameters"),
                "AllowFullTableExternalDataAccess": "True",
            }),
        },
        Row {
            case: "external data access refused, the enum's other published value",
            name: "closed-access",
            tags: tags_of(2, "closed-access"),
            stated: json!({
                "TargetRedshiftCatalog": resource_link("closed-access"),
                "AllowFullTableExternalDataAccess": "False",
            }),
        },
        Row {
            case: "custom properties beside the data lake access properties",
            name: "propertied",
            tags: tags_of(1, "propertied"),
            stated: json!({
                "Description": "with properties",
                "TargetRedshiftCatalog": resource_link("propertied"),
                "CatalogProperties": {
                    "DataLakeAccessProperties": { "DataLakeAccess": false },
                    "CustomProperties": { "lineage": "enabled" },
                },
            }),
        },
        Row {
            case: "both default-permission lists, and the overwrite directive accepted",
            name: "granting",
            tags: tags_of(2, "granting"),
            stated: json!({
                "TargetRedshiftCatalog": resource_link("granting"),
                "CreateDatabaseDefaultPermissions": [{
                    "Principal": { "DataLakePrincipalIdentifier": "IAM_ALLOWED_PRINCIPALS" },
                    "Permissions": ["ALL"],
                }],
                "CreateTableDefaultPermissions": [{
                    "Principal": { "DataLakePrincipalIdentifier": "IAM_ALLOWED_PRINCIPALS" },
                    // Two of the published `Permission` values
                    // (`smithy_query shape=Permission`), so the row states a grant of more
                    // than one permission.
                    "Permissions": ["SELECT", "ALTER"],
                }],
                "OverwriteChildResourcePermissionsWithDefault": "Accept",
            }),
        },
        Row {
            case: "empty permission lists, and the overwrite directive denied",
            name: "ungranting",
            tags: tags_of(3, "ungranting"),
            stated: json!({
                "TargetRedshiftCatalog": resource_link("ungranting"),
                "CreateDatabaseDefaultPermissions": [],
                "CreateTableDefaultPermissions": [],
                "OverwriteChildResourcePermissionsWithDefault": "Deny",
            }),
        },
    ]
}

/// An update writes the definition its input states, and touches nothing beside it: the
/// catalog's identity and creation instant stay as the create left them, its tag set is
/// untouched, and the member no `Catalog` carries is accepted without being reported back.
#[tokio::test]
async fn an_update_writes_the_definition_and_nothing_beside_it() {
    let router = emulator();

    for Row {
        case,
        name,
        tags,
        stated,
    } in rows()
    {
        given_linked_catalog_with_tags(&router, name, &tags).await;
        let created = get_catalog(&router, name).await;
        // The ARN the tag set is read under is the one the emulator minted for the catalog,
        // taken off the create's own read rather than spelled again here.
        let arn = created
            .get("ResourceArn")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("case {case}: a Catalog reports its ResourceArn, got {created}"))
            .to_string();
        let tags_before = read_tags(&router, case, &arn).await;

        update(&router, name, &stated).await;
        let read = get_catalog(&router, name).await;

        // Every member the input states, the read reports.
        assert_echoed(case, &reported_definition(&stated), &read);
        // The member only the input declares is accepted, and reported by nothing.
        assert!(
            read.get(INPUT_ONLY_MEMBER).is_none(),
            "case {case}: {INPUT_ONLY_MEMBER} is no member of a Catalog, got {read}"
        );
        // The members no `CatalogInput` carries are not this request's to change.
        assert_server_owned_members_unchanged(case, &created, &read);
        // The tag set is written through the tagging operations, so a definition write leaves
        // it exactly as the create recorded it.
        let tags_after = read_tags(&router, case, &arn).await;
        assert_eq!(
            tags_after, tags_before,
            "case {case}: expected the create's tags {tags_before:?}, got {tags_after:?}"
        );
    }
}

/// Stating the same definition twice leaves one definition — only "the time at which the
/// catalog was last updated" may move — and reading the catalog back changes nothing a later
/// caller sees.
#[tokio::test]
async fn stating_one_definition_twice_leaves_one_definition() {
    let router = emulator();
    let name = "restated";
    given_linked_catalog_with_tags(&router, name, &tags_of(1, "restated")).await;
    let stated = json!({
        "Description": "stated twice",
        "Parameters": { "owner": "platform", "tier": "gold" },
        "TargetRedshiftCatalog": resource_link(name),
        "CreateTableDefaultPermissions": [{
            "Principal": { "DataLakePrincipalIdentifier": "IAM_ALLOWED_PRINCIPALS" },
            "Permissions": ["SELECT"],
        }],
    });

    update(&router, name, &stated).await;
    let first = get_catalog(&router, name).await;
    update(&router, name, &stated).await;
    let second = get_catalog(&router, name).await;

    let updated_first = first
        .get("UpdateTime")
        .and_then(Value::as_f64)
        .unwrap_or_else(|| panic!("the first update stamped an UpdateTime, got {first}"));
    let updated_second = second
        .get("UpdateTime")
        .and_then(Value::as_f64)
        .unwrap_or_else(|| panic!("the second update stamped an UpdateTime, got {second}"));
    assert!(
        updated_second >= updated_first,
        "case restating the definition: expected an UpdateTime at or after {updated_first}, got \
         {updated_second}"
    );

    let mut first_definition = first.clone();
    let mut second_definition = second.clone();
    for definition in [&mut first_definition, &mut second_definition] {
        definition
            .as_object_mut()
            .expect("a Catalog is a structure")
            .remove("UpdateTime");
    }
    assert_eq!(
        first_definition, second_definition,
        "case restating the definition: expected {first_definition}, got {second_definition}"
    );

    // A read reports; it writes nothing a later caller could see, the update instant included.
    for attempt in ["the first read after the write", "the read after that one"] {
        let again = get_catalog(&router, name).await;
        assert_eq!(
            again, second,
            "case {attempt}: a read must change nothing, expected {second}, got {again}"
        );
    }
}
