// SPDX-License-Identifier: CC0-1.0

//! How a `CatalogNameString` this service will not take is REFUSED, driven end to end over the
//! same router the emulator boots (awsJson1_1, `x-amz-target: AmazonAthena.<Operation>`).
//!
//! THE RULE UNDER TEST: a value for a catalog name is measured against two requirements, each
//! stated in the words that requirement's own layer states it in:
//!
//! * the modelled `@pattern` on `CatalogNameString` — a rule about the REQUEST — is stated in
//!   AWS's validation envelope, over one clause naming the member and quoting the expression,
//!   exactly as every other modelled constraint of this service is
//!   (`tests/all/boundary_validation_refusal.rs`). It is the requirement the framework's
//!   boundary declares and cannot apply, the expression writing the astral plane as a surrogate
//!   pair, so a control character reaching a handler is refused there in those same words;
//! * a name the model ADMITS that Athena still will not register a catalog under is refused by
//!   the service, with its own fixed sentence around the rejected name
//!   (`data_catalog_name_invalid`).
//!
//! Each is asserted over a TABLE rather than one request: the rows differ along which character
//! class the name breaks (a space, a dot, a non-ASCII letter, a control character, the two
//! non-characters at the end of the basic plane), along the LENGTH of the name, along the
//! catalog type it is stated under — the documented length limit is the type's — and along
//! which operation carried it, since every operation that takes a catalog name applies the same
//! requirement. A refused name registers nothing: it appears in no listing afterwards.

use serde_json::{json, Value};
use service_com_amazonaws_athena_amazonathena::state::{
    data_catalog_name_invalid, catalog_name_pattern_error, DATA_CATALOG_TYPE_GLUE,
    ERROR_CODE_INVALID_INPUT, MAX_DATA_CATALOG_NAME_CHARS,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{assert_refusal, call, router};

/// The `catalog-id` parameter a GLUE catalog requires, which every row below carries so no row
/// is refused for its parameters instead of its name.
const CATALOG_ID: &str = "000000000000";

/// Ask for a GLUE data catalog called `name`.
async fn create(router: &MultiServiceRouter, name: &str) -> (u16, Value) {
    call(
        router,
        "CreateDataCatalog",
        &json!({
            "Name": name,
            "Type": DATA_CATALOG_TYPE_GLUE,
            "Parameters": { "catalog-id": CATALOG_ID }
        }),
    )
    .await
}

/// Every catalog name the account's listing reports.
async fn listed(router: &MultiServiceRouter) -> Vec<String> {
    let (status, body) = call(router, "ListDataCatalogs", &json!({ "MaxResults": 50 })).await;
    assert_eq!(
        status, 200,
        "the account's catalogs must be listable, got {status} with {body:?}"
    );
    body["DataCatalogsSummary"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|summary| summary["CatalogName"].as_str().map(ToString::to_string))
        .collect()
}

/// The validation sentence a value outside the modelled expression is refused with, for the
/// member path `member` — read from the ONE place this service states that requirement, so the
/// suite cannot drift from the expression the model declares.
fn pattern_sentence(member: &str, name: &str) -> String {
    catalog_name_pattern_error(member, name).unwrap_or_else(|| {
        panic!("the name {name:?} must break the modelled expression for {member}")
    })
}

/// RULE: a name the MODEL admits that Athena will not register a catalog under is refused with
/// the service's own fixed sentence around the rejected name, and registers nothing.
#[tokio::test]
async fn a_name_the_service_will_not_register_states_one_sentence() {
    let router = router();

    // Rows differ along the character class the name breaks and along its length: every one of
    // them is inside the modelled expression (so it reaches the service's own rule) and outside
    // the documented character set or length.
    let overlong = "l".repeat(MAX_DATA_CATALOG_NAME_CHARS + 1);
    let rows: Vec<(&str, String)> = vec![
        ("a space", "orders catalog".to_string()),
        ("a dot", "orders.catalog".to_string()),
        ("a slash", "orders/catalog".to_string()),
        ("a non-ASCII letter", "café_catalog".to_string()),
        ("a tab, which the expression itself names", "orders\tcatalog".to_string()),
        (
            "a space and a non-ASCII letter together",
            "sim2real cat café".to_string(),
        ),
        ("one character past the documented length", overlong),
    ];

    for (case, name) in rows {
        let (status, refused) = create(&router, &name).await;
        assert_refusal(
            case,
            status,
            &refused,
            ERROR_CODE_INVALID_INPUT,
            &data_catalog_name_invalid(&name),
        );
        assert!(
            !listed(&router).await.contains(&name),
            "case {case:?}: a refused name must appear in no listing"
        );
    }
}

/// RULE: a value the modelled `@pattern` on `CatalogNameString` excludes is refused as THAT
/// constraint — AWS's validation envelope, one clause, the member and the expression — rather
/// than as the service's own name rule, and registers nothing.
#[tokio::test]
async fn a_name_outside_the_modelled_expression_is_refused_as_that_constraint() {
    let router = router();

    // Rows differ along which excluded character the name carries and where: the expression
    // admits `U+0020`..`U+D7FF`, `U+E000`..`U+FFFD`, the astral planes and the tab, so what it
    // excludes is a control character below `U+0020` other than the tab and the two
    // non-characters at the end of the basic plane.
    let rows: Vec<(&str, String)> = vec![
        ("a control character at the end", "orders_catalog\u{1}".to_string()),
        ("a control character in the middle", "orders\u{7}catalog".to_string()),
        ("a newline", "orders\ncatalog".to_string()),
        ("a carriage return", "orders\rcatalog".to_string()),
        ("a NUL", "orders\u{0}catalog".to_string()),
        ("the non-character U+FFFE", "orders\u{fffe}catalog".to_string()),
        ("the non-character U+FFFF", "orders\u{ffff}catalog".to_string()),
    ];

    for (case, name) in rows {
        let (status, refused) = create(&router, &name).await;
        assert_refusal(
            case,
            status,
            &refused,
            ERROR_CODE_INVALID_INPUT,
            &pattern_sentence("name", &name),
        );
        assert!(
            !listed(&router).await.contains(&name),
            "case {case:?}: a refused name must appear in no listing"
        );
    }

    // And a name the expression ADMITS, inside the documented character set, is registered: the
    // rule refuses what it states and nothing else.
    let (status, created) = create(&router, "orders_catalog").await;
    assert_eq!(
        status, 200,
        "a name both requirements admit must be registered, got {status} with {created:?}"
    );
    assert!(
        listed(&router).await.contains(&"orders_catalog".to_string()),
        "the registered catalog must be listed"
    );
}

/// RULE: the requirement is the MEMBER's, so every operation that takes a catalog name applies
/// it — each naming that operation's own member in the clause.
#[tokio::test]
async fn every_operation_that_takes_a_catalog_name_applies_the_modelled_expression() {
    let router = router();
    let name = "orders_catalog\u{1}";

    // (case, the operation, its request, the member a clause about it names)
    let rows: [(&str, &str, Value, &str); 7] = [
        (
            "a create",
            "CreateDataCatalog",
            json!({
                "Name": name,
                "Type": DATA_CATALOG_TYPE_GLUE,
                "Parameters": { "catalog-id": CATALOG_ID }
            }),
            "name",
        ),
        (
            "an update",
            "UpdateDataCatalog",
            json!({
                "Name": name,
                "Type": DATA_CATALOG_TYPE_GLUE,
                "Parameters": { "catalog-id": CATALOG_ID }
            }),
            "name",
        ),
        ("a read", "GetDataCatalog", json!({ "Name": name }), "name"),
        (
            "a delete",
            "DeleteDataCatalog",
            json!({ "Name": name }),
            "name",
        ),
        (
            "a database read",
            "GetDatabase",
            json!({ "CatalogName": name, "DatabaseName": "default" }),
            "catalogName",
        ),
        (
            "a database listing",
            "ListDatabases",
            json!({ "CatalogName": name }),
            "catalogName",
        ),
        (
            "a table listing",
            "ListTableMetadata",
            json!({ "CatalogName": name, "DatabaseName": "default" }),
            "catalogName",
        ),
    ];

    for (case, operation, request, member) in rows {
        let (status, refused) = call(&router, operation, &request).await;
        assert_refusal(
            case,
            status,
            &refused,
            ERROR_CODE_INVALID_INPUT,
            &pattern_sentence(member, name),
        );
    }
}
