// SPDX-License-Identifier: CC0-1.0

//! What the Data Catalog's READ projections report beyond the members a client sent.
//!
//! `GetDatabase`, `GetTable` and `GetTables` serialize members the catalog itself
//! determines — the Lake Formation default grant of a database created without one, the
//! three-state view flags, the sort order of a storage descriptor, the id of the version
//! a create made, and the principal that made it. Each is reported DEFINITELY, so a
//! client never has to read an absent member as "unknown".
//!
//! Every suite here is a table: the rows differ along what the rule generalises over
//! (the grant list sent or not sent, the shape of the storage descriptor's sort order,
//! the table type, how many tables the listed database holds), and the recorded
//! single-table case is one row among them. Calls go over the wire through the shared
//! [`crate::wire`] harness.

use serde_json::{json, Value};

use crate::wire::{
    call, emulator, error_code, error_message, ok, table_names, ACCOUNT_ID, CALLER_PRINCIPAL,
    PRINCIPAL_PERMISSIONS_TAG,
};

/// `grants` as the catalog REPORTS them: every element carries the
/// [`PRINCIPAL_PERMISSIONS_TAG`] discriminator, whatever the client sent.
fn as_reported(grants: Value) -> Value {
    let tagged: Vec<Value> = grants
        .as_array()
        .expect("a grant list is a list")
        .iter()
        .map(|grant| {
            let mut grant = grant.clone();
            grant
                .as_object_mut()
                .expect("a grant is a structure")
                .insert("__type".to_string(), json!(PRINCIPAL_PERMISSIONS_TAG));
            grant
        })
        .collect();
    Value::Array(tagged)
}

/// The `CreateTableDefaultPermissions` a database carries when its create named none:
/// the single `ALL` grant to the Lake Formation `IAM_ALLOWED_PRINCIPALS` principal.
///
/// Source: `smithy_query shape=Database,PrincipalPermissions,Permission` on this crate's
/// model — `Database$CreateTableDefaultPermissions` targets a `PrincipalPermissionsList`
/// of `PrincipalPermissions{Principal: DataLakePrincipal, Permissions: PermissionList}`,
/// and `ALL` is one of the `Permission` enum's values.
fn iam_allowed_principals_grant() -> Value {
    as_reported(json!([
        {
            "Principal": { "DataLakePrincipalIdentifier": "IAM_ALLOWED_PRINCIPALS" },
            "Permissions": [ "ALL" ]
        }
    ]))
}

/// Create `database` and read it back.
async fn create_then_get_database(
    router: &smithy_server_runtime::MultiServiceRouter,
    database_input: &Value,
) -> Value {
    let name = database_input["Name"].as_str().expect("a database name");
    ok(
        router,
        "CreateDatabase",
        &json!({ "DatabaseInput": database_input }),
    )
    .await;
    let fetched = ok(router, "GetDatabase", &json!({ "Name": name })).await;
    fetched["Database"].clone()
}

/// Create `table_input` in `database` and read it back through `GetTable`.
async fn create_then_get_table(
    router: &smithy_server_runtime::MultiServiceRouter,
    database: &str,
    table_input: &Value,
) -> Value {
    let name = table_input["Name"].as_str().expect("a table name");
    ok(
        router,
        "CreateTable",
        &json!({ "DatabaseName": database, "TableInput": table_input }),
    )
    .await;
    let fetched = ok(
        router,
        "GetTable",
        &json!({ "DatabaseName": database, "Name": name }),
    )
    .await;
    fetched["Table"].clone()
}

// ---------------------------------------------------------------------------
// Database: the Lake Formation default grant
// ---------------------------------------------------------------------------

/// A database created without `CreateTableDefaultPermissions` reports the catalog's
/// default grant; one created WITH a list — the empty list included — reports exactly
/// that list. The two are different governance decisions, so the projection may not
/// collapse them, and the recorded value is set at create, not minted per read.
///
/// The rows differ along the grant list sent: absent (with and without other members
/// beside it), explicitly empty, one grant of one permission, one grant of several
/// permissions the issue never cited, and two grants.
#[tokio::test]
async fn a_databases_create_table_default_permissions_are_reported_on_every_read() {
    let router = emulator();

    let analyst = "arn:aws:iam::000000000000:role/analyst";
    let engineer = "arn:aws:iam::000000000000:role/engineer";

    let cases: Vec<(&str, Value, Value)> = vec![
        (
            "no grants sent",
            json!({ "Name": "grants_default_db" }),
            iam_allowed_principals_grant(),
        ),
        (
            "no grants sent, other members set",
            json!({
                "Name": "grants_default_rich_db",
                "Description": "a database nobody governed",
                "LocationUri": "s3://bucket/default/",
                "Parameters": { "owner": "data-eng" }
            }),
            iam_allowed_principals_grant(),
        ),
        (
            "explicitly empty grant list",
            json!({ "Name": "grants_empty_db", "CreateTableDefaultPermissions": [] }),
            as_reported(json!([])),
        ),
        (
            "one grant of one permission",
            json!({
                "Name": "grants_one_db",
                "CreateTableDefaultPermissions": [
                    {
                        "Principal": { "DataLakePrincipalIdentifier": analyst },
                        "Permissions": [ "SELECT" ]
                    }
                ]
            }),
            as_reported(json!([
                {
                    "Principal": { "DataLakePrincipalIdentifier": analyst },
                    "Permissions": [ "SELECT" ]
                }
            ])),
        ),
        (
            // The permissions here are `Permission` enum values the issue did not cite.
            "one grant of several permissions",
            json!({
                "Name": "grants_many_permissions_db",
                "CreateTableDefaultPermissions": [
                    {
                        "Principal": { "DataLakePrincipalIdentifier": engineer },
                        "Permissions": [ "CREATE_TABLE", "ALTER", "DATA_LOCATION_ACCESS" ]
                    }
                ]
            }),
            as_reported(json!([
                {
                    "Principal": { "DataLakePrincipalIdentifier": engineer },
                    "Permissions": [ "CREATE_TABLE", "ALTER", "DATA_LOCATION_ACCESS" ]
                }
            ])),
        ),
        (
            "two grants",
            json!({
                "Name": "grants_two_db",
                "CreateTableDefaultPermissions": [
                    {
                        "Principal": { "DataLakePrincipalIdentifier": analyst },
                        "Permissions": [ "SELECT" ]
                    },
                    {
                        "Principal": { "DataLakePrincipalIdentifier": engineer },
                        "Permissions": [ "ALL" ]
                    }
                ]
            }),
            as_reported(json!([
                {
                    "Principal": { "DataLakePrincipalIdentifier": analyst },
                    "Permissions": [ "SELECT" ]
                },
                {
                    "Principal": { "DataLakePrincipalIdentifier": engineer },
                    "Permissions": [ "ALL" ]
                }
            ])),
        ),
    ];

    for (case, database_input, expected) in &cases {
        let database = create_then_get_database(&router, database_input).await;
        assert_eq!(
            &database["CreateTableDefaultPermissions"], expected,
            "case {case}: expected CreateTableDefaultPermissions {expected}, got {:?}",
            database["CreateTableDefaultPermissions"]
        );
        assert_eq!(
            database["CatalogId"],
            json!(ACCOUNT_ID),
            "case {case}: Database.CatalogId is the catalog's account id, got {:?}",
            database["CatalogId"]
        );

        // Recorded at create, so a second read reports the same grants.
        let name = database_input["Name"].as_str().expect("a database name");
        let again = ok(&router, "GetDatabase", &json!({ "Name": name })).await;
        assert_eq!(
            again["Database"], database,
            "case {case}: two reads of one database must agree, got {:?} then {:?}",
            database, again["Database"]
        );
    }
}

// ---------------------------------------------------------------------------
// Table: the members the catalog determines
// ---------------------------------------------------------------------------

/// `GetTable` reports the catalog-determined members of a table definition: the first
/// version of its history, the principal that created it, the three-state view flags as
/// explicit booleans, and its storage descriptor's sort order as a list — empty when the
/// table defines no sort order, rather than omitted.
///
/// The rows differ along the storage descriptor sent: none at all, one without a sort
/// order, one with an explicitly empty sort order, one sorted column, two sorted columns
/// with both documented `Order$SortOrder` values (0 descending, 1 ascending), and a
/// bucketed `GOVERNED` table with no sort order.
#[tokio::test]
async fn the_table_projection_reports_the_members_the_catalog_determines() {
    let router = emulator();
    ok(
        &router,
        "CreateDatabase",
        &json!({ "DatabaseInput": { "Name": "projection_db" } }),
    )
    .await;

    // (case, TableInput, expected StorageDescriptor.SortColumns — None when the whole
    // storage descriptor is expected to stay absent)
    let cases: Vec<(&str, Value, Option<Value>)> = vec![
        (
            "no storage descriptor",
            json!({ "Name": "no_sd", "TableType": "EXTERNAL_TABLE" }),
            None,
        ),
        (
            "storage descriptor without a sort order",
            json!({
                "Name": "sd_without_sort",
                "TableType": "EXTERNAL_TABLE",
                "StorageDescriptor": {
                    "Columns": [ { "Name": "id", "Type": "string" } ],
                    "Location": "s3://bucket/sd_without_sort/"
                }
            }),
            Some(json!([])),
        ),
        (
            "explicitly empty sort order",
            json!({
                "Name": "sd_empty_sort",
                "TableType": "EXTERNAL_TABLE",
                "StorageDescriptor": {
                    "Columns": [ { "Name": "id", "Type": "string" } ],
                    "SortColumns": []
                }
            }),
            Some(json!([])),
        ),
        (
            "one sorted column",
            json!({
                "Name": "sd_one_sort",
                "TableType": "EXTERNAL_TABLE",
                "StorageDescriptor": {
                    "Columns": [ { "Name": "day", "Type": "date" } ],
                    "BucketColumns": [ "day" ],
                    "NumberOfBuckets": 4,
                    "SortColumns": [ { "Column": "day", "SortOrder": 1 } ]
                }
            }),
            Some(json!([ { "Column": "day", "SortOrder": 1 } ])),
        ),
        (
            "two sorted columns, descending then ascending",
            json!({
                "Name": "sd_two_sorts",
                "TableType": "EXTERNAL_TABLE",
                "StorageDescriptor": {
                    "Columns": [
                        { "Name": "day", "Type": "date" },
                        { "Name": "amount", "Type": "double" }
                    ],
                    "BucketColumns": [ "day", "amount" ],
                    "NumberOfBuckets": 8,
                    "SortColumns": [
                        { "Column": "day", "SortOrder": 0 },
                        { "Column": "amount", "SortOrder": 1 }
                    ]
                }
            }),
            Some(json!([
                { "Column": "day", "SortOrder": 0 },
                { "Column": "amount", "SortOrder": 1 }
            ])),
        ),
        (
            "bucketed governed table with no sort order",
            json!({
                "Name": "sd_governed",
                "TableType": "GOVERNED",
                "StorageDescriptor": {
                    "Columns": [ { "Name": "id", "Type": "string" } ],
                    "BucketColumns": [ "id" ],
                    "NumberOfBuckets": 2,
                    "Compressed": true
                }
            }),
            Some(json!([])),
        ),
    ];

    for (case, table_input, expected_sort_columns) in &cases {
        let table = create_then_get_table(&router, "projection_db", table_input).await;

        assert_eq!(
            table["VersionId"],
            json!("0"),
            "case {case}: the version a create makes is the first of the table's history, \
             \"0\", got {:?}",
            table["VersionId"]
        );
        assert_eq!(
            table["CreatedBy"],
            json!(CALLER_PRINCIPAL),
            "case {case}: Table.CreatedBy is the principal ARN the request speaks for, got {:?}",
            table["CreatedBy"]
        );
        assert_eq!(
            table["CatalogId"],
            json!(ACCOUNT_ID),
            "case {case}: Table.CatalogId is the catalog's account id, got {:?}",
            table["CatalogId"]
        );
        assert_eq!(
            table["DatabaseName"],
            json!("projection_db"),
            "case {case}: Table.DatabaseName names the owning database, got {:?}",
            table["DatabaseName"]
        );
        for flag in ["IsMultiDialectView", "IsMaterializedView"] {
            assert_eq!(
                table[flag],
                json!(false),
                "case {case}: {flag} must be an explicit false for a table that is no view, \
                 got {:?}",
                table[flag]
            );
        }
        assert_eq!(
            table["IsRegisteredWithLakeFormation"],
            json!(false),
            "case {case}: IsRegisteredWithLakeFormation must be an explicit false, got {:?}",
            table["IsRegisteredWithLakeFormation"]
        );
        assert!(
            table["ViewDefinition"].is_null(),
            "case {case}: a table that is no view carries no ViewDefinition, got {:?}",
            table["ViewDefinition"]
        );

        match expected_sort_columns {
            None => assert!(
                table["StorageDescriptor"].is_null(),
                "case {case}: a storage descriptor the client never sent stays absent, got {:?}",
                table["StorageDescriptor"]
            ),
            Some(expected) => assert_eq!(
                &table["StorageDescriptor"]["SortColumns"], expected,
                "case {case}: expected StorageDescriptor.SortColumns {expected}, got {:?}",
                table["StorageDescriptor"]["SortColumns"]
            ),
        }

        // The list read serves the SAME projection, element by element, not a reduced
        // one: the element for this table is identical to what `GetTable` answered.
        let name = table_input["Name"].as_str().expect("a table name");
        let page = ok(
            &router,
            "GetTables",
            &json!({ "DatabaseName": "projection_db" }),
        )
        .await;
        let listed = page["TableList"]
            .as_array()
            .expect("TableList is a list")
            .iter()
            .find(|entry| entry["Name"] == json!(name))
            .unwrap_or_else(|| panic!("case {case}: {name} must appear in TableList"));
        assert_eq!(
            listed, &table,
            "case {case}: the TableList element must be the GetTable projection, got {listed:?} \
             against {table:?}"
        );
    }
}

/// Every element of a `GetTables` page carries the full projection, whatever the page
/// holds: the defect is per element, so a two-table database must not hide it.
///
/// The rows differ along how many tables the listed database holds.
#[tokio::test]
async fn every_element_of_a_table_list_carries_the_full_projection() {
    let router = emulator();

    for table_count in [1_usize, 2, 3] {
        let case = format!("{table_count} table(s)");
        let database = format!("list_db_{table_count}");
        ok(
            &router,
            "CreateDatabase",
            &json!({ "DatabaseInput": { "Name": database } }),
        )
        .await;

        let mut expected: Vec<Value> = Vec::new();
        for index in 0..table_count {
            let table_input = json!({
                "Name": format!("listed_table_{index}"),
                "TableType": "EXTERNAL_TABLE",
                "StorageDescriptor": {
                    "Columns": [ { "Name": "id", "Type": "string" } ],
                    "Location": format!("s3://bucket/{database}/{index}/")
                }
            });
            expected.push(create_then_get_table(&router, &database, &table_input).await);
        }

        let page = ok(&router, "GetTables", &json!({ "DatabaseName": database })).await;
        let listed = page["TableList"]
            .as_array()
            .expect("TableList is a list")
            .clone();
        assert_eq!(
            listed.len(),
            table_count,
            "case {case}: every stored table must be listed, got {listed:?}"
        );
        for (element, want) in listed.iter().zip(expected.iter()) {
            assert_eq!(
                element, want,
                "case {case}: a TableList element must be the GetTable projection, got \
                 {element:?} against {want:?}"
            );
            assert_eq!(
                element["VersionId"],
                json!("0"),
                "case {case}: a listed table that was never updated is version \"0\", got {:?}",
                element["VersionId"]
            );
            assert_eq!(
                element["CreatedBy"],
                json!(CALLER_PRINCIPAL),
                "case {case}: a listed table's CreatedBy is the caller principal, got {:?}",
                element["CreatedBy"]
            );
            assert_eq!(
                element["StorageDescriptor"]["SortColumns"],
                json!([]),
                "case {case}: a listed table with no sort order reports an empty list, got {:?}",
                element["StorageDescriptor"]["SortColumns"]
            );
            for flag in ["IsMultiDialectView", "IsMaterializedView"] {
                assert_eq!(
                    element[flag],
                    json!(false),
                    "case {case}: a listed table's {flag} is an explicit false, got {:?}",
                    element[flag]
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// A projection narrows the definition, never the identity
// ---------------------------------------------------------------------------

/// The members a `Table` carries because the CATALOG answers them about the element it is
/// serving, which every read of that table reports whatever projection the request asked
/// for.
///
/// Source: `smithy_query shape=Table` on this crate's model — `CatalogId`
/// (`CatalogIdString`, "the ID of the Data Catalog in which the table resides"),
/// `DatabaseId` ("the opaque, catalog-assigned id of the database this table belongs to"),
/// `DatabaseName` ("the name of the database where the table metadata resides") and the
/// flags `IsRowFilteringEnabled` ("whether a row-level filter is in force on this table")
/// and `IsMaterializedView` ("indicates a table is a `MaterializedView`").
/// `IsMultiDialectView` is NOT among them: it states which dialects a view's query can be
/// read in, i.e. part of the definition a projection narrows.
const TABLE_IDENTITY_MEMBERS: &[&str] = &[
    "CatalogId",
    "DatabaseName",
    "DatabaseId",
    "IsRowFilteringEnabled",
    "IsMaterializedView",
];

/// The members of a `Table` a client SENT — the definition a projection narrows. Source:
/// the same `smithy_query shape=Table`, every member a `TableInput` can set plus the
/// history members the create wrote (`VersionId`, `CreatedBy`, `CreateTime`).
const TABLE_DEFINITION_MEMBERS: &[&str] = &[
    "Owner",
    "Description",
    "Parameters",
    "StorageDescriptor",
    "CreateTime",
    "UpdateTime",
    "VersionId",
    "CreatedBy",
    "IsMultiDialectView",
    "RLSProperties",
];

/// The sentence the catalog refuses an `AttributesToGet` that names no `NAME` with —
/// entirely fixed text, echoing nothing of the request, so one wording answers every such
/// list of either projected listing.
const MUST_CONTAIN_NAME: &str = "AttributesToGet must contain NAME.";

/// The sentence the catalog refuses an `AttributesToGet` supplied as the empty list with —
/// again entirely fixed text, and spelled the way the catalog's own validator names the
/// member.
const CANNOT_BE_EMPTY: &str = "If provided, attributesToGet cannot be empty.";

/// Create `database` with three fully populated tables and answer the id the catalog
/// assigned the database.
async fn database_with_tables(
    router: &smithy_server_runtime::MultiServiceRouter,
    database: &str,
) -> Value {
    ok(
        router,
        "CreateDatabase",
        &json!({ "DatabaseInput": { "Name": database } }),
    )
    .await;
    for name in ["events_one", "events_two", "orders"] {
        ok(
            router,
            "CreateTable",
            &json!({
                "DatabaseName": database,
                "TableInput": {
                    "Name": name,
                    "TableType": "EXTERNAL_TABLE",
                    "Owner": "data-eng",
                    "Description": format!("{name} of {database}"),
                    "Parameters": { "classification": "parquet" },
                    "StorageDescriptor": {
                        "Columns": [ { "Name": "id", "Type": "string" } ],
                        "Location": format!("s3://bucket/{database}/{name}/")
                    }
                }
            }),
        )
        .await;
    }
    let read = ok(router, "GetDatabase", &json!({ "Name": database })).await;
    read["Database"]["DatabaseId"].clone()
}

/// One row of the projected-listing table: which listing to ask for, and what every entry
/// of the page it answers must report.
struct ProjectedPage<'a> {
    case: &'a str,
    /// The database listed, which every entry must be attributed to.
    database: &'a str,
    /// The id the catalog assigned that database, as its own read reports it.
    database_id: &'a Value,
    request: Value,
    /// Whether the projection named `TABLE_TYPE`, the one definition attribute it may ask
    /// for beside the name.
    wants_table_type: bool,
    /// The names the page must hold, in order.
    expected_names: Vec<&'a str>,
}

/// A PROJECTED `GetTables` page reports, on every entry, the identity the catalog wrote
/// onto that table — the catalog it resides in, the database that holds it (by name and by
/// the id the catalog assigned it) and the flags the catalog answers definitely — while
/// the definition members the projection did not name stay absent. `AttributesToGet`
/// selects which attributes of the DEFINITION come back, not whether the entry can be
/// attributed to a catalog and a database.
///
/// The rows differ along the projection asked for (`NAME`, and `NAME` with `TABLE_TYPE` in
/// both orders), the database listed (so the identity expected differs row to row), how
/// many entries the page holds, and whether the page was narrowed by `Expression` or by
/// `MaxResults`.
#[tokio::test]
async fn a_projected_table_listing_reports_every_entrys_catalog_and_database_identity() {
    let router = emulator();
    let first = "projected_identity_db";
    let second = "projected_identity_other_db";
    let first_id = database_with_tables(&router, first).await;
    let second_id = database_with_tables(&router, second).await;

    let cases: Vec<ProjectedPage> = vec![
        ProjectedPage {
            case: "NAME alone",
            database: first,
            database_id: &first_id,
            request: json!({ "DatabaseName": first, "AttributesToGet": [ "NAME" ] }),
            wants_table_type: false,
            expected_names: vec!["events_one", "events_two", "orders"],
        },
        ProjectedPage {
            case: "NAME and TABLE_TYPE",
            database: first,
            database_id: &first_id,
            request: json!({
                "DatabaseName": first,
                "AttributesToGet": [ "NAME", "TABLE_TYPE" ]
            }),
            wants_table_type: true,
            expected_names: vec!["events_one", "events_two", "orders"],
        },
        ProjectedPage {
            case: "TABLE_TYPE and NAME, the other order",
            database: second,
            database_id: &second_id,
            request: json!({
                "DatabaseName": second,
                "AttributesToGet": [ "TABLE_TYPE", "NAME" ]
            }),
            wants_table_type: true,
            expected_names: vec!["events_one", "events_two", "orders"],
        },
        ProjectedPage {
            case: "NAME over a page narrowed by Expression",
            database: second,
            database_id: &second_id,
            request: json!({
                "DatabaseName": second,
                "AttributesToGet": [ "NAME" ],
                "Expression": "events*"
            }),
            wants_table_type: false,
            expected_names: vec!["events_one", "events_two"],
        },
        ProjectedPage {
            case: "NAME and TABLE_TYPE over a page narrowed by MaxResults",
            database: first,
            database_id: &first_id,
            request: json!({
                "DatabaseName": first,
                "AttributesToGet": [ "NAME", "TABLE_TYPE" ],
                "MaxResults": 2
            }),
            wants_table_type: true,
            expected_names: vec!["events_one", "events_two"],
        },
        ProjectedPage {
            case: "NAME over a single-entry page",
            database: second,
            database_id: &second_id,
            request: json!({
                "DatabaseName": second,
                "AttributesToGet": [ "NAME" ],
                "Expression": "orders"
            }),
            wants_table_type: false,
            expected_names: vec!["orders"],
        },
    ];

    for ProjectedPage {
        case,
        database,
        database_id,
        request,
        wants_table_type,
        expected_names,
    } in &cases
    {
        let page = ok(&router, "GetTables", request).await;
        assert_eq!(
            table_names(&page),
            expected_names
                .iter()
                .map(|name| name.to_string())
                .collect::<Vec<String>>(),
            "case {case}: expected the page to hold {expected_names:?}, got {page}"
        );

        for entry in page["TableList"]
            .as_array()
            .expect("TableList is a list")
            .iter()
        {
            let name = entry["Name"].as_str().expect("Table.Name is a string");
            let expected_identity = json!({
                "CatalogId": ACCOUNT_ID,
                "DatabaseName": database,
                "DatabaseId": database_id,
                "IsRowFilteringEnabled": false,
                "IsMaterializedView": false,
            });
            for member in TABLE_IDENTITY_MEMBERS {
                assert_eq!(
                    entry[member], expected_identity[member],
                    "case {case}: {name} must carry {member} {:?}, got {:?}",
                    expected_identity[member], entry[member]
                );
            }
            assert_eq!(
                entry["TableType"],
                if *wants_table_type {
                    json!("EXTERNAL_TABLE")
                } else {
                    Value::Null
                },
                "case {case}: {name} reports TableType only when the projection names \
                 TABLE_TYPE, got {:?}",
                entry["TableType"]
            );
            for member in TABLE_DEFINITION_MEMBERS {
                assert!(
                    entry.get(member).is_none_or(Value::is_null),
                    "case {case}: {name} must not project {member}, got {:?}",
                    entry[member]
                );
            }

            // The identity a projected entry reports is the identity the unprojected read
            // of that same table reports: one stored attribution, read two ways.
            let whole = ok(
                &router,
                "GetTable",
                &json!({ "DatabaseName": database, "Name": name }),
            )
            .await;
            for member in TABLE_IDENTITY_MEMBERS {
                assert_eq!(
                    entry[member], whole["Table"][member],
                    "case {case}: {name}'s projected {member} must equal the whole read's, \
                     got {:?} against {:?}",
                    entry[member], whole["Table"][member]
                );
            }
        }
    }
}

/// A PROJECTED `GetDatabases` page reports, on every entry, the catalog that owns the
/// database and the id the catalog assigned it — the same rule one level up — while the
/// definition members the projection did not name stay absent.
///
/// The rows differ along the projection asked for (`NAME`, and `NAME` with
/// `TARGET_DATABASE` in both orders) and whether the page was narrowed by `MaxResults`.
#[tokio::test]
async fn a_projected_database_listing_reports_every_entrys_catalog_identity() {
    let router = emulator();
    let mut expected_ids: Vec<(String, Value)> = Vec::new();
    for database in ["projected_db_one", "projected_db_two"] {
        ok(
            &router,
            "CreateDatabase",
            &json!({
                "DatabaseInput": {
                    "Name": database,
                    "Description": format!("{database} description"),
                    "LocationUri": format!("s3://bucket/{database}/"),
                    "Parameters": { "owner": "data-eng" }
                }
            }),
        )
        .await;
        let read = ok(&router, "GetDatabase", &json!({ "Name": database })).await;
        expected_ids.push((
            database.to_string(),
            read["Database"]["DatabaseId"].clone(),
        ));
    }
    // The database the Data Catalog CARRIES is listed with them and carries the same
    // catalog-written members, so the page is read against it too.
    let carried = ok(&router, "GetDatabase", &json!({ "Name": "default" })).await;
    expected_ids.push((
        "default".to_string(),
        carried["Database"]["DatabaseId"].clone(),
    ));

    // (case, the request, how many entries the page holds: the two created databases and
    // the one the catalog carries, unless `MaxResults` narrows the page)
    let cases: Vec<(&str, Value, usize)> = vec![
        ("NAME alone", json!({ "AttributesToGet": [ "NAME" ] }), 3),
        (
            "NAME and TARGET_DATABASE",
            json!({ "AttributesToGet": [ "NAME", "TARGET_DATABASE" ] }),
            3,
        ),
        (
            "TARGET_DATABASE and NAME, the other order",
            json!({ "AttributesToGet": [ "TARGET_DATABASE", "NAME" ] }),
            3,
        ),
        (
            "NAME over a page narrowed by MaxResults",
            json!({ "AttributesToGet": [ "NAME" ], "MaxResults": 1 }),
            1,
        ),
    ];

    for (case, request, expected_len) in &cases {
        let page = ok(&router, "GetDatabases", request).await;
        let listed = page["DatabaseList"]
            .as_array()
            .expect("DatabaseList is a list")
            .clone();
        assert_eq!(
            listed.len(),
            *expected_len,
            "case {case}: expected {expected_len} entry/entries, got {page}"
        );
        for entry in &listed {
            let name = entry["Name"]
                .as_str()
                .expect("Database.Name is a string")
                .to_string();
            let expected_id = expected_ids
                .iter()
                .find(|(database, _)| *database == name)
                .map(|(_, id)| id.clone())
                .unwrap_or_else(|| panic!("case {case}: {name} is not a database this test made"));
            assert_eq!(
                entry["CatalogId"],
                json!(ACCOUNT_ID),
                "case {case}: {name} must carry CatalogId {ACCOUNT_ID}, got {:?}",
                entry["CatalogId"]
            );
            assert_eq!(
                entry["DatabaseId"], expected_id,
                "case {case}: {name} must carry DatabaseId {expected_id:?}, got {:?}",
                entry["DatabaseId"]
            );
            for member in [
                "Description",
                "LocationUri",
                "Parameters",
                "CreateTime",
                "CreateTableDefaultPermissions",
            ] {
                assert!(
                    entry.get(member).is_none_or(Value::is_null),
                    "case {case}: {name} must not project {member}, got {:?}",
                    entry[member]
                );
            }
        }
    }
}

/// An `AttributesToGet` that names no `NAME` asks for a projection neither listing serves
/// — "[t]he request must include the `NAME`" — and the empty list is one the member
/// "doesn't accept": each is refused with ONE fixed sentence that echoes nothing of the
/// request, so every reader of the catalog answers it the same way.
///
/// The rows differ along the operation refused, the list sent (one attribute, several, and
/// in both orders, over every value of each attribute enum —
/// `smithy_query shape=TableAttributes,DatabaseAttributes`: `NAME`, `TABLE_TYPE`,
/// `DEFAULT`, `LATEST_ICEBERG_METADATA` for tables and `NAME`, `TARGET_DATABASE` for
/// databases — other than `NAME` itself) and the rule broken.
#[tokio::test]
async fn an_attributes_to_get_the_catalog_refuses_carries_the_catalogs_sentence() {
    let router = emulator();
    ok(
        &router,
        "CreateDatabase",
        &json!({ "DatabaseInput": { "Name": "name_required_db" } }),
    )
    .await;
    ok(
        &router,
        "CreateTable",
        &json!({
            "DatabaseName": "name_required_db",
            "TableInput": { "Name": "events", "TableType": "EXTERNAL_TABLE" }
        }),
    )
    .await;

    // (case, operation, request, the message expected)
    let cases: Vec<(&str, &str, Value, &str)> = vec![
        (
            "GetTables with TABLE_TYPE alone",
            "GetTables",
            json!({ "DatabaseName": "name_required_db", "AttributesToGet": [ "TABLE_TYPE" ] }),
            MUST_CONTAIN_NAME,
        ),
        (
            "GetTables with DEFAULT alone",
            "GetTables",
            json!({ "DatabaseName": "name_required_db", "AttributesToGet": [ "DEFAULT" ] }),
            MUST_CONTAIN_NAME,
        ),
        (
            "GetTables with LATEST_ICEBERG_METADATA alone",
            "GetTables",
            json!({
                "DatabaseName": "name_required_db",
                "AttributesToGet": [ "LATEST_ICEBERG_METADATA" ]
            }),
            MUST_CONTAIN_NAME,
        ),
        (
            "GetTables with two attributes, neither of them NAME",
            "GetTables",
            json!({
                "DatabaseName": "name_required_db",
                "AttributesToGet": [ "DEFAULT", "TABLE_TYPE" ]
            }),
            MUST_CONTAIN_NAME,
        ),
        (
            "GetTables with the same two in the other order",
            "GetTables",
            json!({
                "DatabaseName": "name_required_db",
                "AttributesToGet": [ "TABLE_TYPE", "DEFAULT" ]
            }),
            MUST_CONTAIN_NAME,
        ),
        (
            "GetDatabases with TARGET_DATABASE alone",
            "GetDatabases",
            json!({ "AttributesToGet": [ "TARGET_DATABASE" ] }),
            MUST_CONTAIN_NAME,
        ),
        (
            "GetTables with an empty AttributesToGet",
            "GetTables",
            json!({ "DatabaseName": "name_required_db", "AttributesToGet": [] }),
            CANNOT_BE_EMPTY,
        ),
        (
            "GetDatabases with an empty AttributesToGet",
            "GetDatabases",
            json!({ "AttributesToGet": [] }),
            CANNOT_BE_EMPTY,
        ),
        (
            "GetTable with an empty AttributesToGet",
            "GetTable",
            json!({
                "DatabaseName": "name_required_db",
                "Name": "events",
                "AttributesToGet": []
            }),
            CANNOT_BE_EMPTY,
        ),
    ];

    for (case, operation, body, expected_message) in &cases {
        let (status, answer) = call(&router, operation, body).await;
        assert_eq!(
            status, 400,
            "case {case}: expected HTTP 400, got {status} with {answer}"
        );
        assert!(
            error_code(&answer).contains("InvalidInputException"),
            "case {case}: expected InvalidInputException, got {}",
            error_code(&answer)
        );
        assert_eq!(
            &error_message(&answer),
            expected_message,
            "case {case}: expected the message {expected_message:?}, got {:?}",
            error_message(&answer)
        );
    }
}
