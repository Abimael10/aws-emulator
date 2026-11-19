// SPDX-License-Identifier: CC0-1.0

//! What an OpsItem query reports once the OpsItems it selects have been WRITTEN AGAIN.
//!
//! The RULE asserted here: a query over a collection resolves every filter against the record
//! each resource holds AT THE INSTANT THE QUERY RUNS — "query a set of OpsItems" over "one or
//! more filters to limit the response" (`DescribeOpsItems`, `$OpsItemFilters`) — so an edit of
//! a filtered member moves the resource out of the queries its old value satisfied and into
//! the ones its new value satisfies, a removal that has settled leaves it out of every query,
//! and a query the caller bounded with no `MaxResults` is served up to the bound the model
//! allows ("the maximum number of items to return for this call").
//!
//! Every case goes through the front door a client uses — the shared harness in
//! `all/parameter_store.rs` ([`call`], [`router`], [`router_with_engine`]) dispatches `POST /`
//! with `X-Amz-Target: AmazonSSM.<Operation>` through the multi-service router — and every
//! OpsItem a query reports here was opened by `CreateOpsItem` and rewritten by
//! `UpdateOpsItem` or `DeleteOpsItem` of the same case, so a response that is not computed
//! from persisted state cannot pass.
//!
//! The tables are rows along what the rule generalises over: WHICH member a later write
//! changes (the status, the priority as a number, the severity, the category, the title, an
//! operational-data value, an operational-data pair), the operator the filter compares it
//! under, and how many OpsItems a write leaves a query reporting.

use std::sync::Arc;
use std::time::SystemTime;

use serde_json::{json, Value};
use service_com_amazonaws_ssm_amazonssm::state::ops_items::OPS_ITEM_DELETE_SETTLES_AFTER;
use service_com_amazonaws_ssm_amazonssm::state::ComAmazonawsSsmAmazonssmState;
use smithy_server_runtime::MultiServiceRouter;

use crate::ops_items::{create_ok, minimal};
use crate::parameter_store::{call, router, router_with_engine};

/// The largest page `DescribeOpsItems` serves, which is the upper bound of the modelled
/// `MaxResults` (`smithy_query(query="operation", operation="DescribeOpsItems")`:
/// `OpsItemMaxResults`, range 1..50) — and so the page a request naming no `MaxResults` is
/// served, because such a request asked for no bound of its own.
const MODELLED_MAX_PAGE: usize = 50;

/// One filter of a query.
fn filter(key: &str, operator: &str, values: &[&str]) -> Value {
    json!({ "Key": key, "Operator": operator, "Values": values })
}

/// A filter that narrows a query to ONE OpsItem, so a case reads whether its own OpsItem is
/// selected rather than whichever others the store holds.
fn only(ops_item_id: &str) -> Value {
    filter("OpsItemId", "Equal", &[ops_item_id])
}

/// `DescribeOpsItems` with `body`, asserting it succeeded and answering its response.
async fn query_ok(router: &MultiServiceRouter, case: &str, body: &Value) -> Value {
    let (status, response) = call(router, "DescribeOpsItems", body).await;
    assert_eq!(
        status, 200,
        "case {case}: DescribeOpsItems {body} must succeed, got {status} {response}"
    );
    response
}

/// The OpsItem ids one response reports, in the order it reported them.
fn summary_ids(response: &Value) -> Vec<String> {
    response["OpsItemSummaries"]
        .as_array()
        .map(|summaries| {
            summaries
                .iter()
                .map(|summary| {
                    summary["OpsItemId"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string()
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The OpsItem ids `filters` select, in the order the query reported them.
async fn selected_ids(router: &MultiServiceRouter, case: &str, filters: &[Value]) -> Vec<String> {
    let response = query_ok(router, case, &json!({ "OpsItemFilters": filters })).await;
    summary_ids(&response)
}

/// Whether `filters` select the OpsItem `ops_item_id` names.
async fn selects(
    router: &MultiServiceRouter,
    case: &str,
    ops_item_id: &str,
    under_test: &Value,
) -> bool {
    let filters = vec![only(ops_item_id), under_test.clone()];
    selected_ids(router, case, &filters)
        .await
        .iter()
        .any(|id| id == ops_item_id)
}

/// `CreateOpsItem`'s body: the required members for `title`, plus whatever `extra` names.
fn created_with(title: &str, extra: &Value) -> Value {
    let mut body = minimal(title);
    if let (Some(body), Some(extra)) = (body.as_object_mut(), extra.as_object()) {
        for (member, value) in extra {
            body.insert(member.clone(), value.clone());
        }
    }
    body
}

/// Open one OpsItem and answer its id.
async fn open(router: &MultiServiceRouter, case: &str, body: &Value) -> String {
    let created = create_ok(router, case, body).await;
    created["OpsItemId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case}: CreateOpsItem must report an OpsItemId, got {created}"))
        .to_string()
}

/// Settle every delete the store owes, as the clock does once the window has passed
/// (`crate::state::ops_items::OPS_ITEM_DELETE_SETTLES_AFTER`): the removal is materialised
/// from a stored due instant, so a case moves the instant the store is asked about rather
/// than waiting for it — nothing sleeps.
fn settle_deletes(engine: &Arc<engine::InMemoryEngine>) {
    engine
        .state::<ComAmazonawsSsmAmazonssmState>()
        .lock()
        .settle_ops_item_deletions_due(SystemTime::now() + OPS_ITEM_DELETE_SETTLES_AFTER);
}

/// One row of the edit table: a member a later write changes, with a filter the OpsItem
/// satisfies before that write and one it satisfies only after it.
struct EditCase<'a> {
    case: &'a str,
    /// The title the create opens the OpsItem under.
    title: &'a str,
    /// The members the create names beyond the required three.
    created: Value,
    /// The edit `UpdateOpsItem` makes.
    edit: Value,
    /// A filter the OpsItem satisfies BEFORE the edit and not after it.
    before: Value,
    /// A filter the OpsItem satisfies AFTER the edit and not before it.
    after: Value,
}

/// An edit moves the OpsItem between the queries it satisfies: every filter is resolved
/// against the record as it now stands, so the value the edit replaced selects it no longer
/// and the value the edit wrote selects it from then on.
#[tokio::test]
async fn an_edit_moves_the_ops_item_between_the_queries_it_satisfies() {
    let router = router();
    let cases = vec![
        EditCase {
            case: "the status a write moves on",
            title: "a disk is filling up",
            created: json!({}),
            edit: json!({ "Status": "InProgress" }),
            before: filter("Status", "Equal", &["Open"]),
            after: filter("Status", "Equal", &["InProgress"]),
        },
        EditCase {
            case: "the priority a write raises",
            title: "a node stopped reporting",
            created: json!({ "Priority": 1 }),
            edit: json!({ "Priority": 4 }),
            before: filter("Priority", "Equal", &["1"]),
            after: filter("Priority", "Equal", &["4"]),
        },
        EditCase {
            case: "the priority compared as a number, under the ordering operators",
            title: "latency regression in checkout",
            created: json!({ "Priority": 2 }),
            edit: json!({ "Priority": 5 }),
            before: filter("Priority", "LessThan", &["3"]),
            after: filter("Priority", "GreaterThan", &["4"]),
        },
        EditCase {
            case: "the severity a write lowers",
            title: "an agent is out of date",
            created: json!({ "Severity": "3" }),
            edit: json!({ "Severity": "1" }),
            before: filter("Severity", "Equal", &["3"]),
            after: filter("Severity", "Equal", &["1"]),
        },
        EditCase {
            case: "the category a write re-files it under",
            title: "a backup did not run",
            created: json!({ "Category": "Availability" }),
            edit: json!({ "Category": "Security" }),
            before: filter("Category", "Equal", &["Availability"]),
            after: filter("Category", "Equal", &["Security"]),
        },
        EditCase {
            case: "the title a write rewords, under Contains",
            title: "the data volume is filling up",
            created: json!({}),
            edit: json!({ "Title": "the data volume was drained" }),
            before: filter("Title", "Contains", &["filling"]),
            after: filter("Title", "Contains", &["drained"]),
        },
        EditCase {
            case: "an operational-data value a write replaces",
            title: "duplicate work items about one node",
            created: json!({
                "OperationalData": {
                    "team": { "Value": "platform", "Type": "SearchableString" }
                }
            }),
            edit: json!({
                "OperationalData": {
                    "team": { "Value": "storage", "Type": "SearchableString" }
                }
            }),
            before: filter("OperationalDataValue", "Equal", &["platform"]),
            after: filter("OperationalDataValue", "Equal", &["storage"]),
        },
        EditCase {
            case: "the operational-data pair a write replaces",
            title: "a queue is not draining",
            created: json!({
                "OperationalData": {
                    "owner": { "Value": "payments", "Type": "SearchableString" }
                }
            }),
            edit: json!({
                "OperationalData": {
                    "owner": { "Value": "ledger", "Type": "SearchableString" }
                }
            }),
            before: filter(
                "OperationalData",
                "Equal",
                &[r#"{"key":"owner","value":"payments"}"#],
            ),
            after: filter(
                "OperationalData",
                "Equal",
                &[r#"{"key":"owner","value":"ledger"}"#],
            ),
        },
    ];

    for EditCase {
        case,
        title,
        created,
        edit,
        before,
        after,
    } in cases
    {
        let id = open(&router, case, &created_with(title, &created)).await;

        assert!(
            selects(&router, case, &id, &before).await,
            "case {case}: {before} must select {id} before the edit, got {:?}",
            selected_ids(&router, case, &[only(&id), before.clone()]).await
        );
        assert!(
            !selects(&router, case, &id, &after).await,
            "case {case}: {after} must select nothing before the edit, got {:?}",
            selected_ids(&router, case, &[only(&id), after.clone()]).await
        );

        let mut request = edit.clone();
        request["OpsItemId"] = json!(id);
        let (status, response) = call(&router, "UpdateOpsItem", &request).await;
        assert_eq!(
            status, 200,
            "case {case}: UpdateOpsItem {request} must succeed, got {status} {response}"
        );

        assert!(
            selects(&router, case, &id, &after).await,
            "case {case}: {after} must select {id} after the edit, got {:?}",
            selected_ids(&router, case, &[only(&id), after.clone()]).await
        );
        assert!(
            !selects(&router, case, &id, &before).await,
            "case {case}: {before} must select nothing after the edit, got {:?}",
            selected_ids(&router, case, &[only(&id), before.clone()]).await
        );
    }
}

/// A delete that has settled leaves the OpsItem out of every query — the unfiltered one, the
/// one naming its id and the one its own members satisfied — while the OpsItems beside it are
/// reported exactly as before.
#[tokio::test]
async fn a_settled_delete_leaves_the_ops_item_out_of_every_query() {
    let (router, engine) = router_with_engine();
    let shared = filter("Source", "Equal", &["emulator-tests"]);

    // Three OpsItems the shared filter selects, one of which is deleted.
    let doomed = open(
        &router,
        "the OpsItem that is deleted",
        &created_with(
            "a stale work item",
            &json!({ "Severity": "4", "Category": "Cost", "Priority": 3 }),
        ),
    )
    .await;
    let kept: Vec<String> = {
        let mut kept = Vec::new();
        for title in ["a work item that stays", "another work item that stays"] {
            kept.push(open(&router, title, &created_with(title, &json!({}))).await);
        }
        kept
    };

    let before = selected_ids(&router, "before the delete", std::slice::from_ref(&shared)).await;
    for id in std::iter::once(&doomed).chain(kept.iter()) {
        assert!(
            before.contains(id),
            "before the delete: {id} must be reported, got {before:?}"
        );
    }

    let (status, response) = call(&router, "DeleteOpsItem", &json!({ "OpsItemId": doomed })).await;
    assert_eq!(
        status, 200,
        "DeleteOpsItem {doomed} must succeed, got {status} {response}"
    );
    settle_deletes(&engine);

    for (case, filters) in [
        ("every OpsItem of the Region", vec![]),
        ("the filter the deleted OpsItem satisfied", vec![shared.clone()]),
        ("the filter naming its own id", vec![only(&doomed)]),
        (
            "the filters its own members satisfied",
            vec![
                filter("Severity", "Equal", &["4"]),
                filter("Category", "Equal", &["Cost"]),
                filter("Priority", "Equal", &["3"]),
            ],
        ),
    ] {
        let reported = selected_ids(&router, case, &filters).await;
        assert!(
            !reported.contains(&doomed),
            "case {case}: a settled delete leaves {doomed} out of the query, got {reported:?}"
        );
    }

    let after = selected_ids(&router, "after the delete", std::slice::from_ref(&shared)).await;
    let mut expected = kept.clone();
    expected.sort();
    let mut reported = after.clone();
    reported.sort();
    assert_eq!(
        reported, expected,
        "after the delete: the OpsItems beside the deleted one are reported, got {after:?}"
    );
}

/// A query naming no `MaxResults` is served up to the bound the model allows, and the token it
/// hands back resumes the rest: so paging one unbounded query reports each selected OpsItem
/// exactly once.
#[tokio::test]
async fn a_query_naming_no_max_results_serves_the_modelled_maximum() {
    let router = router();
    // One OpsItem more than a page holds, so the first page is full and a second page is owed.
    let opened = MODELLED_MAX_PAGE + 1;
    let mut ids = Vec::with_capacity(opened);
    for index in 0..opened {
        let title = format!("bulk work item {index:03}");
        ids.push(open(&router, &title, &created_with(&title, &json!({}))).await);
    }

    let first = query_ok(&router, "the first page", &json!({})).await;
    let first_page = summary_ids(&first);
    assert_eq!(
        first_page.len(),
        MODELLED_MAX_PAGE,
        "an unbounded query serves {MODELLED_MAX_PAGE} OpsItems, got {} ({first})",
        first_page.len()
    );
    let token = first["NextToken"].as_str().unwrap_or_else(|| {
        panic!("an unbounded query over {opened} OpsItems must hand back a token, got {first}")
    });

    let second = query_ok(&router, "the second page", &json!({ "NextToken": token })).await;
    let second_page = summary_ids(&second);
    assert_eq!(
        second_page.len(),
        opened - MODELLED_MAX_PAGE,
        "the resumed page serves what the first one left, got {} ({second})",
        second_page.len()
    );
    assert!(
        second["NextToken"].is_null(),
        "the last page hands back no token, got {second}"
    );

    let mut paged: Vec<String> = first_page.into_iter().chain(second_page).collect();
    paged.sort();
    paged.dedup();
    let mut expected = ids.clone();
    expected.sort();
    assert_eq!(
        paged, expected,
        "paging an unbounded query reports each of the {opened} OpsItems exactly once"
    );
}
