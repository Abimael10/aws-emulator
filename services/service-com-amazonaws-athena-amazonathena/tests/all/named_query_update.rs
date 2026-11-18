// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for `UpdateNamedQuery`: what one writes onto a saved query is what the
//! next `GetNamedQuery` reports, the members the update cannot carry are left as the create
//! stored them, and an id this account holds no query under is refused with nothing written.
//! Driven over the wire (awsJson1_1, `x-amz-target: AmazonAthena.<Operation>`) through the
//! same router the emulator boots, so every assertion is about what an SDK client observes.
//!
//! THE RULE UNDER TEST: an update of a saved query replaces that query's name, its query
//! text and its description with the ones the request carries — a request carrying no
//! description leaving it with none — while its id, its database and its workgroup stay the
//! ones the create minted and stored, and a request naming an id this account holds no query
//! under is refused with `InvalidRequestException` (HTTP 400) and writes nothing.
//!
//! Each test asserts that rule over a TABLE rather than one request: the rows differ along
//! which members the update changes, whether the description is added, replaced or dropped,
//! how long the query text is, how many statements it holds, and whether the query lives in
//! the account's default workgroup or in one the test created.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_athena_amazonathena::state::{
    self, DEFAULT_WORK_GROUP, ERROR_CODE_INVALID_INPUT,
};

use crate::harness::{assert_refusal, call, create_work_group, router};

/// A query text of `statements` statements, so a row can differ along how much text the
/// update writes without any row quoting one recorded query.
fn query_text(statements: usize) -> String {
    (0..statements)
        .map(|position| format!("SELECT {position} AS position FROM events"))
        .collect::<Vec<_>>()
        .join("; ")
}

/// Save one query and answer with the `NamedQueryId` the create minted.
async fn create_named_query(router: &MultiServiceRouter, request: &Value) -> String {
    let (status, body) = call(router, "CreateNamedQuery", request).await;
    assert_eq!(
        status, 200,
        "CreateNamedQuery({request}) must be accepted, got {status} with {body:?}"
    );
    body["NamedQueryId"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateNamedQuery must answer with an id, got {body:?}"))
        .to_string()
}

/// `GetNamedQuery`'s view of one saved query: `(status, NamedQuery)`.
async fn read_named_query(router: &MultiServiceRouter, id: &str) -> (u16, Value) {
    let (status, body) = call(router, "GetNamedQuery", &json!({ "NamedQueryId": id })).await;
    (status, body["NamedQuery"].clone())
}

/// Assert that the query `id` names reports exactly `expected`, member by member, so a row
/// states both what the update wrote and what it left alone.
fn assert_named_query(case: &str, id: &str, found: &Value, expected: &Value) {
    for member in ["Name", "QueryString", "Database", "WorkGroup"] {
        assert_eq!(
            found[member], expected[member],
            "case {case:?}: expected {member} {:?}, got {:?} in {found:?}",
            expected[member], found[member]
        );
    }
    assert_eq!(
        found["Description"], expected["Description"],
        "case {case:?}: expected Description {:?}, got {:?} in {found:?}",
        expected["Description"], found["Description"]
    );
    assert_eq!(
        found["NamedQueryId"].as_str(),
        Some(id),
        "case {case:?}: expected the query to keep the id {id}, got {found:?}"
    );
}

/// One row of the update table: the query to save, the update to send, and what the read
/// must report afterwards.
struct UpdateCase {
    case: &'static str,
    /// The `CreateNamedQuery` body the row starts from, minus the `Name`.
    create: Value,
    /// The `UpdateNamedQuery` body the row sends, minus the `NamedQueryId`.
    update: Value,
    /// The `Name` the created query carries.
    created_name: &'static str,
    /// The `Description` a read must report once the update is applied, or `None` when it
    /// must report none.
    description: Option<String>,
}

/// The rows: each changes a different combination of the three members an update carries —
/// the name alone, the query text alone, both at once — over queries that differ in the
/// workgroup they live in, in how much text they hold and in whether the create stored a
/// description at all.
fn update_cases() -> Vec<UpdateCase> {
    vec![
        UpdateCase {
            case: "the name alone, the query text unchanged",
            create: json!({
                "Database": "logs",
                "QueryString": query_text(1),
                "Description": "the created description",
            }),
            update: json!({
                "Name": "renamed-query",
                "QueryString": query_text(1),
                "Description": "the created description",
            }),
            created_name: "created-query",
            description: Some("the created description".to_string()),
        },
        UpdateCase {
            case: "the query text alone, the name unchanged",
            create: json!({
                "Database": "logs",
                "QueryString": query_text(1),
                "Description": "the created description",
            }),
            update: json!({
                "Name": "created-query",
                "QueryString": query_text(4),
                "Description": "the created description",
            }),
            created_name: "created-query",
            description: Some("the created description".to_string()),
        },
        UpdateCase {
            case: "the name, the text and the description at once",
            create: json!({
                "Database": "logs",
                "QueryString": query_text(2),
                "Description": "the created description",
            }),
            update: json!({
                "Name": "errors-by-hour",
                "QueryString": query_text(7),
                "Description": "the updated description",
            }),
            created_name: "created-query",
            description: Some("the updated description".to_string()),
        },
        UpdateCase {
            case: "a description added where the create stored none",
            create: json!({
                "Database": "logs",
                "QueryString": query_text(1),
            }),
            update: json!({
                "Name": "described-now",
                "QueryString": query_text(1),
                "Description": "a description the create did not carry",
            }),
            created_name: "created-query",
            description: Some("a description the create did not carry".to_string()),
        },
        UpdateCase {
            case: "no description, where the create stored one",
            create: json!({
                "Database": "logs",
                "QueryString": query_text(3),
                "Description": "the created description",
            }),
            update: json!({
                "Name": "undescribed-now",
                "QueryString": query_text(3),
            }),
            created_name: "created-query",
            description: None,
        },
        UpdateCase {
            case: "a query in a workgroup of its own, with a long query text",
            create: json!({
                "Database": "warehouse",
                "QueryString": query_text(2),
                "WorkGroup": "wg-named-query-update",
            }),
            update: json!({
                // A description at the modelled maximum (1024 characters), so the row
                // differs along the SIZE of what the update writes as well.
                "Description": "d".repeat(1024),
                "Name": "wide-scan",
                "QueryString": query_text(400),
            }),
            created_name: "created-query",
            description: Some("d".repeat(1024)),
        },
        UpdateCase {
            case: "a query whose create named no workgroup, so it lives in the default one",
            create: json!({
                "Database": "logs",
                "QueryString": query_text(1),
                "Description": "the created description",
            }),
            update: json!({
                "Name": "default-workgroup-query",
                "QueryString": query_text(2),
                "Description": "the updated description",
            }),
            created_name: "created-query",
            description: Some("the updated description".to_string()),
        },
    ]
}

/// An update writes the members it carries onto the query its id names and leaves the id,
/// the database and the workgroup as the create stored them.
#[tokio::test]
async fn an_update_writes_what_it_carries_and_leaves_the_rest() {
    let router = router();
    create_work_group(&router, "wg-named-query-update").await;

    for case in update_cases() {
        let mut create = case.create.clone();
        create["Name"] = json!(case.created_name);
        let id = create_named_query(&router, &create).await;

        let mut update = case.update.clone();
        update["NamedQueryId"] = json!(id);
        let (status, body) = call(&router, "UpdateNamedQuery", &update).await;
        assert_eq!(
            status, 200,
            "case {:?}: expected the update to answer 200, got {status} with {body:?}",
            case.case
        );

        let (read_status, found) = read_named_query(&router, &id).await;
        assert_eq!(
            read_status, 200,
            "case {:?}: expected GetNamedQuery to answer 200, got {read_status} with {found:?}",
            case.case
        );
        let expected = json!({
            "Name": case.update["Name"],
            "QueryString": case.update["QueryString"],
            // Neither member the update may change: the database and the workgroup the
            // create stored, the default workgroup when the create named none.
            "Database": case.create["Database"],
            "WorkGroup": json!(case
                .create
                .get("WorkGroup")
                .and_then(Value::as_str)
                .unwrap_or(DEFAULT_WORK_GROUP)),
            "Description": case
                .description
                .as_ref()
                .map_or(Value::Null, |text| json!(text)),
        });
        assert_named_query(case.case, &id, &found, &expected);
    }
}

/// An id this account holds no query under is refused, and the refusal writes nothing: the
/// rows differ along WHY the id resolves to nothing — one no create ever minted, one whose
/// query a delete removed, and one whose workgroup was deleted with its contents.
#[tokio::test]
async fn an_unknown_id_is_refused_and_writes_nothing() {
    let router = router();

    // A query that must be untouched by every refused update below.
    let standing = create_named_query(
        &router,
        &json!({
            "Name": "standing-query",
            "Database": "logs",
            "QueryString": query_text(2),
            "Description": "the standing description",
        }),
    )
    .await;

    // An id of a query a delete removed.
    let deleted = create_named_query(
        &router,
        &json!({
            "Name": "deleted-query",
            "Database": "logs",
            "QueryString": query_text(1),
        }),
    )
    .await;
    let (delete_status, delete_body) = call(
        &router,
        "DeleteNamedQuery",
        &json!({ "NamedQueryId": deleted }),
    )
    .await;
    assert_eq!(
        delete_status, 200,
        "the query {deleted} must be deleted, got {delete_status} with {delete_body:?}"
    );

    // An id of a query whose workgroup was deleted with its contents.
    create_work_group(&router, "wg-named-query-update-dropped").await;
    let orphaned = create_named_query(
        &router,
        &json!({
            "Name": "orphaned-query",
            "Database": "logs",
            "QueryString": query_text(1),
            "WorkGroup": "wg-named-query-update-dropped",
        }),
    )
    .await;
    let (drop_status, drop_body) = call(
        &router,
        "DeleteWorkGroup",
        &json!({
            "WorkGroup": "wg-named-query-update-dropped",
            "RecursiveDeleteOption": true,
        }),
    )
    .await;
    assert_eq!(
        drop_status, 200,
        "the workgroup holding {orphaned} must be deleted with its contents, got {drop_status} \
         with {drop_body:?}"
    );

    let unknown: Vec<(&str, String)> = vec![
        (
            "an id no create ever minted",
            "11111111-2222-3333-4444-555555555555".to_string(),
        ),
        ("an id of a deleted query", deleted.clone()),
        (
            "an id whose workgroup was deleted with its contents",
            orphaned.clone(),
        ),
        ("an id that is not a UUID at all", "not-an-id".to_string()),
    ];
    for (case, id) in &unknown {
        let (status, body) = call(
            &router,
            "UpdateNamedQuery",
            &json!({
                "NamedQueryId": id,
                "Name": "written-by-a-refused-update",
                "QueryString": query_text(1),
            }),
        )
        .await;
        // Asserted against the wording itself, not only against the helper the handler
        // words it with: the fixed text is Athena's and the id the CALLER supplied is its
        // one variable part.
        assert_refusal(
            case,
            status,
            &body,
            ERROR_CODE_INVALID_INPUT,
            &format!("NamedQuery: {id} does not exist"),
        );
        assert_eq!(
            crate::harness::message(&body),
            state::named_query_not_found(id),
            "case {case:?}: expected the wording every miss of a named query answers with, \
             got {body:?}"
        );
    }

    // Nothing the refused updates named was written, and the query that does exist is
    // exactly as its create left it.
    let (read_status, found) = read_named_query(&router, &standing).await;
    assert_eq!(
        read_status, 200,
        "expected the standing query to still read back, got {read_status} with {found:?}"
    );
    assert_named_query(
        "the standing query after every refused update",
        &standing,
        &found,
        &json!({
            "Name": "standing-query",
            "QueryString": query_text(2),
            "Database": "logs",
            "WorkGroup": DEFAULT_WORK_GROUP,
            "Description": "the standing description",
        }),
    );
    for id in [&deleted, &orphaned] {
        let (status, body) = read_named_query(&router, id).await;
        assert_eq!(
            status, 400,
            "expected the id {id} to still resolve to no query, got {status} with {body:?}"
        );
    }
}

/// An update touches the ONE query its id names: every other saved query of that workgroup
/// and of the account's other workgroups still reads back as its own create stored it, the
/// workgroup still lists the updated id, and sending the same update again leaves the same
/// record (the operation is idempotent).
#[tokio::test]
async fn an_update_touches_only_the_query_it_names() {
    let router = router();
    create_work_group(&router, "wg-named-query-update-siblings").await;

    // Three queries the rows are told apart by: two in one workgroup, one in another.
    let sibling_work_groups: Vec<(&str, Option<&str>)> = vec![
        ("target", Some("wg-named-query-update-siblings")),
        ("sibling-same-work-group", Some("wg-named-query-update-siblings")),
        ("sibling-default-work-group", None),
    ];
    let mut ids: Vec<(&str, Option<&str>, String)> = Vec::new();
    for (name, work_group) in &sibling_work_groups {
        let mut request = json!({
            "Name": name,
            "Database": "logs",
            "QueryString": query_text(2),
            "Description": format!("{name} as created"),
        });
        if let Some(work_group) = work_group {
            request["WorkGroup"] = json!(work_group);
        }
        let id = create_named_query(&router, &request).await;
        ids.push((name, *work_group, id));
    }
    let (_, target_work_group, target) = ids[0].clone();

    // The same update twice: an update is idempotent, so the second leaves the record the
    // first one wrote.
    let update = json!({
        "NamedQueryId": target,
        "Name": "target-renamed",
        "QueryString": query_text(5),
        "Description": "target as updated",
    });
    for attempt in ["the first update", "the same update again"] {
        let (status, body) = call(&router, "UpdateNamedQuery", &update).await;
        assert_eq!(
            status, 200,
            "case {attempt:?}: expected the update to answer 200, got {status} with {body:?}"
        );
        let (read_status, found) = read_named_query(&router, &target).await;
        assert_eq!(
            read_status, 200,
            "case {attempt:?}: expected GetNamedQuery to answer 200, got {read_status} with \
             {found:?}"
        );
        assert_named_query(
            attempt,
            &target,
            &found,
            &json!({
                "Name": "target-renamed",
                "QueryString": query_text(5),
                "Database": "logs",
                "WorkGroup": target_work_group,
                "Description": "target as updated",
            }),
        );
    }

    // Every other query is as its own create stored it.
    for (name, work_group, id) in ids.iter().skip(1) {
        let (status, found) = read_named_query(&router, id).await;
        assert_eq!(
            status, 200,
            "case {name:?}: expected GetNamedQuery to answer 200, got {status} with {found:?}"
        );
        assert_named_query(
            name,
            id,
            &found,
            &json!({
                "Name": name,
                "QueryString": query_text(2),
                "Database": "logs",
                "WorkGroup": json!(work_group.unwrap_or(DEFAULT_WORK_GROUP)),
                "Description": format!("{name} as created"),
            }),
        );
    }

    // The updated query is still one of its workgroup's saved queries: an update moves no
    // query out of the workgroup that holds it.
    let (list_status, listed) = call(
        &router,
        "ListNamedQueries",
        &json!({ "WorkGroup": "wg-named-query-update-siblings", "MaxResults": 50 }),
    )
    .await;
    assert_eq!(
        list_status, 200,
        "expected ListNamedQueries to answer 200, got {list_status} with {listed:?}"
    );
    let reported: Vec<&str> = listed["NamedQueryIds"]
        .as_array()
        .map(|ids| ids.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    assert!(
        reported.contains(&target.as_str()),
        "expected the workgroup to still list the updated query {target}, got {listed:?}"
    );
    assert_eq!(
        reported.len(),
        2,
        "expected the workgroup's two saved queries to be listed, got {listed:?}"
    );
}
