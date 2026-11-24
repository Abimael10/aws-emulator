// SPDX-License-Identifier: CC0-1.0

//! End-to-end suite for what a `ListPermissions` ANSWER carries about each record it
//! reports, and in what ORDER.
//!
//! Two rules, each over a table of inputs rather than one request.
//!
//! * Every entry reports the write that made the record it describes:
//!   `PrincipalResourcePermissions$LastUpdated` ("The date and time when the resource was
//!   last updated") and `$LastUpdatedBy` ("The user who updated the record") — the call that
//!   granted, added to or trimmed the record for an explicitly granted one, and the create
//!   of the resource for a grant that resource was created carrying, which is the same
//!   instant the Data Catalog reports as its `Table$CreateTime`.
//! * A listing reports its entries ordered by what the ANSWER carries — the principal the
//!   permissions are held by, then the `Resource` member they are held on and the names that
//!   address it — and the page `MaxResults` bounds is cut from that order, so a walk over
//!   every page size sees one order and the entries that come first are on the first page.
//!
//! The rows span what the rules generalise over: the resource form granted on (a database, a
//! table, a table's columns), how many scopes a grant lands on, whether a later grant adds
//! to a record or a revoke trims it, which principals hold the entries (a group, roles whose
//! identifiers sort on either side of the account's own identity, identifiers of different
//! lengths) and the page size the walk asks for.
//!
//! The Data Catalog is seeded through the shared harness, because a grant naming an entity
//! the catalog does not hold is refused; every call goes over the wire the way an SDK client
//! does, so a response can only be what the handler computed from stored state.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::glue_catalog::{
    account_catalog, create_database_with_input, create_table, emulator, lake_formation as call,
    list_permissions as list, table_created_at, table_created_by,
};

/// The database every row grants on, created handing its tables NO default permissions, so
/// what a listing reports is this suite's own grants and what the table's creation made.
const DATABASE: &str = "sales";

/// Its table.
const TABLE: &str = "orders";

/// An emulator whose catalog holds [`DATABASE`] and its [`TABLE`].
async fn seeded() -> MultiServiceRouter {
    let router = emulator();
    create_database_with_input(
        &router,
        &json!({ "Name": DATABASE, "CreateTableDefaultPermissions": [] }),
    )
    .await;
    create_table(&router, DATABASE, TABLE).await;
    router
}

/// The `Table` resource a grant and a listing name.
fn on_table() -> Value {
    json!({ "Table": { "DatabaseName": DATABASE, "Name": TABLE } })
}

/// Every column of that table, named by the wildcard.
fn on_columns() -> Value {
    json!({
        "TableWithColumns": {
            "DatabaseName": DATABASE,
            "Name": TABLE,
            "ColumnWildcard": {},
        }
    })
}

/// The database itself.
fn on_database() -> Value {
    json!({ "Database": { "Name": DATABASE } })
}

/// Grant `permissions` on `resource` to `principal`, asserting the grant was accepted.
async fn grant(
    router: &MultiServiceRouter,
    case: &str,
    principal: &str,
    resource: &Value,
    permissions: &[&str],
) {
    let (status, payload) = call(
        router,
        "GrantPermissions",
        &json!({
            "Principal": { "DataLakePrincipalIdentifier": principal },
            "Resource": resource,
            "Permissions": permissions,
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the grant of {permissions:?} to {principal} to be accepted, \
         got {status} {payload:?}"
    );
}

/// The `LastUpdated` of one entry, as the epoch-seconds number the wire form carries.
fn write_instant(case: &str, entry: &Value) -> f64 {
    entry
        .get("LastUpdated")
        .and_then(Value::as_f64)
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected the entry to carry a LastUpdated, got {entry:?}")
        })
}

/// The `LastUpdatedBy` of one entry.
fn written_by(case: &str, entry: &Value) -> String {
    entry
        .get("LastUpdatedBy")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected the entry to carry a LastUpdatedBy, got {entry:?}")
        })
        .to_string()
}

/// The principal one entry is held by.
fn held_by(entry: &Value) -> String {
    entry
        .get("Principal")
        .and_then(|principal| principal.get("DataLakePrincipalIdentifier"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The `Resource` member one entry is held on: the member name alone, which is what the
/// reported order groups entries by.
fn resource_member(entry: &Value) -> String {
    entry
        .get("Resource")
        .and_then(Value::as_object)
        .and_then(|resource| resource.keys().next().cloned())
        .unwrap_or_default()
}

/// One entry as the order assertions compare it: the principal it is held by and the
/// `Resource` member it is held on.
type Placed = (String, String);

/// Those two fields of each listed entry, in the order the listing reported them.
fn placed(listed: &[Value]) -> Vec<Placed> {
    listed
        .iter()
        .map(|entry| (held_by(entry), resource_member(entry)))
        .collect()
}

#[tokio::test]
async fn every_listed_entry_reports_the_write_that_made_the_record() {
    // Rows differ along the resource form granted on and how many scopes the grant lands
    // on: a database (one record), a table whose permissions split across the table and its
    // columns (two records), the table alone, and the table's columns alone.
    let catalog = account_catalog();
    let analyst = format!("arn:aws:iam::{catalog}:role/analyst");
    let cases: Vec<(&str, Value, Vec<&str>, usize)> = vec![
        ("a database grant", on_database(), vec!["DESCRIBE"], 1),
        (
            "a table grant held at two scopes",
            on_table(),
            vec!["DESCRIBE", "SELECT"],
            2,
        ),
        ("a table grant", on_table(), vec!["DESCRIBE"], 1),
        ("a column grant", on_columns(), vec!["SELECT"], 1),
    ];

    for (case, resource, permissions, records) in cases {
        let router = seeded().await;
        // The identity this emulator serves every call as, asked of the Data Catalog
        // (`Table$CreatedBy`) rather than spelled here, so the assertion follows the
        // emulator's own identity contract.
        let caller = table_created_by(&router, DATABASE, TABLE).await;
        grant(&router, case, &analyst, &resource, &permissions).await;

        let listed = list(
            &router,
            case,
            &json!({ "Principal": { "DataLakePrincipalIdentifier": analyst } }),
        )
        .await;
        assert_eq!(
            listed.len(),
            records,
            "case {case:?}: expected {records} entries for the grant, got {listed:?}"
        );
        for entry in &listed {
            let instant = write_instant(case, entry);
            assert!(
                instant > 0.0,
                "case {case:?}: expected the write instant of a granted record, got {entry:?}"
            );
            let by = written_by(case, entry);
            assert_eq!(
                by, caller,
                "case {case:?}: expected the record written by {caller:?}, got {by:?} in \
                 {entry:?}"
            );
        }

        // A second grant on the same record is another write to it, so what the record
        // reports as its last update is no older than what the first write left.
        let first = write_instant(case, &listed[0]);
        grant(&router, case, &analyst, &resource, &permissions).await;
        let relisted = list(
            &router,
            case,
            &json!({ "Principal": { "DataLakePrincipalIdentifier": analyst } }),
        )
        .await;
        assert_eq!(
            relisted.len(),
            records,
            "case {case:?}: a repeated grant is the same records, got {relisted:?}"
        );
        let second = write_instant(case, &relisted[0]);
        assert!(
            second >= first,
            "case {case:?}: expected the repeated grant's write to be no older than {first}, \
             got {second}"
        );
    }
}

#[tokio::test]
async fn a_grant_a_table_was_created_carrying_reports_that_create() {
    // The complement of the rule for a record no `GrantPermissions` call wrote: creating the
    // table is the write that made it, so the entry reports the instant the Data Catalog
    // records as the table's own creation and the principal it records as its creator. Rows
    // differ along which of the table's two creation-time records is read.
    let router = seeded().await;
    let creator = table_created_by(&router, DATABASE, TABLE).await;
    let created_at = table_created_at(&router, DATABASE, TABLE).await;

    let cases: Vec<(&str, Value, &str)> = vec![
        ("the table's own record", on_table(), "Table"),
        ("its column-level record", on_columns(), "TableWithColumns"),
    ];
    for (case, filter, member) in cases {
        let listed = list(&router, case, &json!({ "Resource": filter })).await;
        let entry = listed
            .iter()
            .find(|entry| held_by(entry) == creator && resource_member(entry) == member)
            .unwrap_or_else(|| {
                panic!("case {case:?}: expected a {member} entry for {creator}, got {listed:?}")
            });
        assert_eq!(
            write_instant(case, entry),
            created_at,
            "case {case:?}: expected the create instant {created_at} the catalog records, got \
             {entry:?}"
        );
        assert_eq!(
            written_by(case, entry),
            creator,
            "case {case:?}: expected the record written by the table's creator {creator:?}, \
             got {entry:?}"
        );
    }
}

#[tokio::test]
async fn a_revoke_that_trims_a_record_is_a_write_to_it() {
    // A revoke that leaves a record holding something is a write to that record, so what it
    // reports as its last update is no older than the grant it trimmed. Rows differ along
    // which permission of the record the revoke takes away.
    let catalog = account_catalog();
    let analyst = format!("arn:aws:iam::{catalog}:role/analyst");
    for (case, revoked) in [
        ("the first of two permissions", "DESCRIBE"),
        ("the second of two permissions", "ALTER"),
    ] {
        let router = seeded().await;
        grant(&router, case, &analyst, &on_table(), &["DESCRIBE", "ALTER"]).await;
        let filter = json!({ "Principal": { "DataLakePrincipalIdentifier": analyst } });
        let granted = list(&router, case, &filter).await;
        let before = write_instant(case, &granted[0]);

        let (status, payload) = call(
            &router,
            "RevokePermissions",
            &json!({
                "Principal": { "DataLakePrincipalIdentifier": analyst },
                "Resource": on_table(),
                "Permissions": [revoked],
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the revoke of {revoked} to be accepted, got {status} \
             {payload:?}"
        );

        let left = list(&router, case, &filter).await;
        assert_eq!(
            left.len(),
            1,
            "case {case:?}: expected the trimmed record to be left, got {left:?}"
        );
        let after = write_instant(case, &left[0]);
        assert!(
            after >= before,
            "case {case:?}: expected the revoke's write to be no older than {before}, got \
             {after}"
        );
        assert_eq!(
            written_by(case, &left[0]),
            table_created_by(&router, DATABASE, TABLE).await,
            "case {case:?}: expected the record written by the revoking caller, got {left:?}"
        );
    }
}

#[tokio::test]
async fn a_page_is_cut_from_the_entries_in_the_order_the_answer_carries_them() {
    // The grants below span the dimensions the order is a function of: a group and three IAM
    // principals whose identifiers sort on either side of the account's own identity
    // (`IAM_ALLOWED_PRINCIPALS` before every ARN, `role/` before `user/`) and are of
    // different lengths, each holding entries on one, two or three of the resource forms.
    let router = seeded().await;
    let catalog = account_catalog();
    let creator = table_created_by(&router, DATABASE, TABLE).await;
    let analyst = format!("arn:aws:iam::{catalog}:role/analyst");
    let auditor = format!("arn:aws:iam::{catalog}:role/zeta-a-much-longer-principal-name");
    let reader = format!("arn:aws:iam::{catalog}:user/zz-last-of-all");
    let case = "the whole order";

    grant(&router, case, &analyst, &on_table(), &["DESCRIBE"]).await;
    grant(&router, case, &analyst, &on_columns(), &["SELECT"]).await;
    grant(&router, case, &analyst, &on_database(), &["DESCRIBE"]).await;
    grant(&router, case, &auditor, &on_table(), &["DESCRIBE"]).await;
    grant(&router, case, &reader, &on_database(), &["DESCRIBE"]).await;

    // What the listing must report, in order: the rule applied to this suite's own data —
    // by the principal the entry is held by, then the `Resource` member it is held on. The
    // creation-time records are in it too (the database's own default grant to the group,
    // and the two the table's creation made for its creator), because the order is over
    // every entry the listing reports.
    let mut expected: Vec<Placed> = vec![
        ("IAM_ALLOWED_PRINCIPALS".to_string(), "Database".to_string()),
        (analyst.clone(), "Database".to_string()),
        (analyst.clone(), "Table".to_string()),
        (analyst.clone(), "TableWithColumns".to_string()),
        (auditor.clone(), "Table".to_string()),
        (creator.clone(), "Table".to_string()),
        (creator.clone(), "TableWithColumns".to_string()),
        (reader.clone(), "Database".to_string()),
    ];
    expected.sort();

    let whole = placed(&list(&router, case, &json!({})).await);
    assert_eq!(
        whole, expected,
        "case {case:?}: expected the entries in the order {expected:?}, got {whole:?}"
    );

    // Every page size walks exactly that order: the published bounds of `MaxResults`
    // (`range 1..1000`), the sizes on either side of the number of entries, and no size at
    // all, which is one page of everything.
    for page_size in [Some(1_i64), Some(2), Some(3), Some(7), Some(8), Some(9), Some(1000), None] {
        let case = format!("MaxResults={page_size:?}");
        let mut seen: Vec<Placed> = Vec::new();
        let mut token: Option<String> = None;
        let mut pages = 0_usize;
        loop {
            let mut body = json!({});
            if let Some(page_size) = page_size {
                body["MaxResults"] = json!(page_size);
            }
            if let Some(token) = &token {
                body["NextToken"] = json!(token);
            }
            let (status, payload) = call(&router, "ListPermissions", &body).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected page {pages} to answer 200, got {status} {payload:?}"
            );
            let entries = payload["PrincipalResourcePermissions"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            if let Some(page_size) = page_size {
                assert!(
                    entries.len() as i64 <= page_size,
                    "case {case:?}: a page carries at most MaxResults entries, got {entries:?}"
                );
            }
            seen.extend(placed(&entries));
            pages += 1;
            assert!(
                pages <= expected.len() + 1,
                "case {case:?}: paging did not terminate after {pages} pages, saw {seen:?}"
            );
            match payload.get("NextToken").and_then(Value::as_str) {
                Some(next) => token = Some(next.to_string()),
                None => break,
            }
        }
        assert_eq!(
            seen, expected,
            "case {case:?}: expected the pages to walk {expected:?} in order, saw {seen:?}"
        );
    }
}

#[tokio::test]
async fn the_first_page_carries_the_entries_that_come_first_in_that_order() {
    // The membership consequence of the order, which is what a client reads a bounded page
    // for: a principal whose identifier sorts before the account's own identity reads its
    // own grants back on the first page, however long its identifier is and however much
    // the table's creation granted beside it. Rows differ along the page size asked for.
    let router = seeded().await;
    let catalog = account_catalog();
    // Longer than the account's own identity, which is what the ARN sorts against: the
    // order is the identifier's, not its length's.
    let probe = format!("arn:aws:iam::{catalog}:role/probe-principal-with-a-long-name");
    let case = "a principal that sorts first";
    grant(&router, case, &probe, &on_table(), &["DESCRIBE"]).await;
    grant(&router, case, &probe, &on_columns(), &["SELECT"]).await;

    for page_size in [2_i64, 3, 4] {
        let case = format!("MaxResults={page_size}");
        let page = list(
            &router,
            &case,
            &json!({ "Resource": on_table(), "MaxResults": page_size }),
        )
        .await;
        let first_two = placed(&page).into_iter().take(2).collect::<Vec<Placed>>();
        let expected: Vec<Placed> = vec![
            (probe.clone(), "Table".to_string()),
            (probe.clone(), "TableWithColumns".to_string()),
        ];
        assert_eq!(
            first_two, expected,
            "case {case:?}: expected the granted entries {expected:?} first, got {page:?}"
        );
        // Each of them reports its own grant: the table's record holds what was granted on
        // the table, the column-level one what was granted on the columns.
        let permissions = |member: &str| -> Vec<String> {
            page.iter()
                .find(|entry| resource_member(entry) == member)
                .and_then(|entry| entry.get("Permissions").and_then(Value::as_array).cloned())
                .unwrap_or_default()
                .iter()
                .map(|value| value.as_str().unwrap_or_default().to_string())
                .collect()
        };
        assert_eq!(
            permissions("Table"),
            vec!["DESCRIBE".to_string()],
            "case {case:?}: expected the table entry to hold DESCRIBE, got {page:?}"
        );
        assert_eq!(
            permissions("TableWithColumns"),
            vec!["SELECT".to_string()],
            "case {case:?}: expected the column entry to hold SELECT, got {page:?}"
        );
    }
}
