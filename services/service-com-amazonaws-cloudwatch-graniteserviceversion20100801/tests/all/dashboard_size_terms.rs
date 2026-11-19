// SPDX-License-Identifier: CC0-1.0

//! WHAT the size a dashboard listing reports is counted over, and what it is NOT counted over.
//!
//! The rule under test: **"The size of the dashboard, in bytes." (`DashboardEntry$Size`, a long
//! — `smithy_query(query="shape",shape="DashboardEntry")`) is the byte length of the dashboard
//! DOCUMENT as the service holds it, and carries no other term**: not the dashboard's name, not
//! its ARN, not its tags, and not a term for how many dashboards the account holds. A client
//! reads that document back through `GetDashboard` ("The detailed information about the
//! dashboard, including what widgets are included and their location on the dashboard."), so the
//! number it is told moves only when the document does — which is what makes `Size` usable for
//! change detection and for budgeting against the dashboard size quota.
//!
//! `tests/all/dashboard_document_size.rs` walks the DOCUMENT dimension of that rule (which
//! document the size is the size of: the submitted body completed with the top-level members it
//! left to the service), and `tests/all/dashboard_body_byte_fidelity.rs` the renderings of one
//! value. What is walked here is the complement: everything else about a dashboard is varied —
//! how long its name is, how many tags it carries, whether those tags arrived with the write or
//! later, and whether the document was replaced — while the document's bytes are held fixed, and
//! the reported size must not move. A size carrying a term for the name or a term for the tags
//! fails these rows while matching the one dashboard a single recorded request happened to
//! create.

use crate::dashboard_document_size::expected_size;
use crate::harness::{build_router, ok};
use serde_json::{json, Value};

/// One dashboard document: a text widget whose `markdown` is `label`, written with a space
/// after every separator — one rendering, held FIXED across the rows below, because what these
/// rows vary is everything about a dashboard except its document.
fn document(label: &str) -> String {
    format!(
        "{{\"widgets\": [{{\"type\": \"text\", \"x\": 0, \"y\": 0, \"width\": 6, \"height\": 3, \
         \"properties\": {{\"markdown\": \"{label}\"}}}}]}}"
    )
}

/// `count` tags, each named for its position, as `PutDashboard` and `TagResource` take them.
fn tags(count: usize) -> Value {
    Value::Array(
        (0..count)
            .map(|index| json!({"Key": format!("key-{index}"), "Value": format!("value-{index}")}))
            .collect(),
    )
}

/// The one `DashboardEntry` the listing reports for `name`.
async fn listed_entry(
    router: &smithy_server_runtime::MultiServiceRouter,
    name: &str,
) -> Option<Value> {
    let listed = ok(
        router,
        "ListDashboards",
        &json!({"DashboardNamePrefix": name}),
    )
    .await;
    listed
        .get("DashboardEntries")?
        .as_array()?
        .iter()
        .find(|entry| entry.get("DashboardName").and_then(Value::as_str) == Some(name))
        .cloned()
}

/// The `Size` the listing reports for `name`, and the document `GetDashboard` answers for it.
async fn reported_size_and_body(
    router: &smithy_server_runtime::MultiServiceRouter,
    name: &str,
) -> (Option<i64>, Option<String>) {
    let size = listed_entry(router, name)
        .await
        .and_then(|entry| entry.get("Size").and_then(Value::as_i64));
    let got = ok(router, "GetDashboard", &json!({"DashboardName": name})).await;
    let body = got
        .get("DashboardBody")
        .and_then(Value::as_str)
        .map(ToString::to_string);
    (size, body)
}

/// Assert that the listing measures `name` by the document the service holds for the body a
/// client reads back.
async fn assert_size_is_the_readable_document(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    name: &str,
    expected: &str,
) {
    let (size, body) = reported_size_and_body(router, name).await;
    assert_eq!(
        body.as_deref(),
        Some(expected),
        "case {case:?}: GetDashboard must answer the document that was stored, got {body:?}"
    );
    let wanted = expected_size(expected);
    assert_eq!(
        size,
        Some(wanted),
        "case {case:?}: expected Size to be the {wanted} bytes of the document the service \
         holds, got {size:?}"
    );
}

#[tokio::test]
async fn the_size_counts_the_document_and_not_the_name_or_the_tags() {
    let router = build_router();
    // One document for every row, so a row that reported a different size would be reporting
    // something other than the document. The rows differ along the dashboard's NAME length and
    // along how many TAGS the write associated with it — the two terms beside the document that
    // a listing could be tempted to add.
    let body = document("the same document for every row");
    let rows: Vec<(&str, String, Value)> = vec![
        ("a short name and no tag", "b".to_string(), Value::Null),
        (
            "a longer name and no tag",
            format!("board-{}", "n".repeat(60)),
            Value::Null,
        ),
        (
            "a name at the published maximum and no tag",
            "n".repeat(255),
            Value::Null,
        ),
        ("a name and one tag", "tagged-once".to_string(), tags(1)),
        ("a name and three tags", "tagged-thrice".to_string(), tags(3)),
        (
            "a long name and three tags",
            format!("tagged-{}", "n".repeat(60)),
            tags(3),
        ),
    ];

    let mut reported: Vec<(&str, i64)> = Vec::new();
    for (case, name, tag_list) in &rows {
        let mut request = json!({"DashboardName": name, "DashboardBody": &body});
        if !tag_list.is_null() {
            request["Tags"] = tag_list.clone();
        }
        ok(&router, "PutDashboard", &request).await;
        assert_size_is_the_readable_document(&router, case, name, &body).await;
        let (size, _) = reported_size_and_body(&router, name).await;
        reported.push((case, size.unwrap_or_default()));
    }

    // The rows against EACH OTHER: one document, one size, whatever the dashboard around it —
    // which is what rules out a term for the name or for the tags even if such a term happened
    // to be right for one row.
    let (first_case, first_size) = reported[0];
    for (case, size) in &reported[1..] {
        assert_eq!(
            *size, first_size,
            "case {case:?}: the same document must be measured the same as in case \
             {first_case:?}, got {size} against {first_size}"
        );
    }
}

#[tokio::test]
async fn tagging_a_dashboard_later_does_not_change_the_size_it_reports() {
    let router = build_router();
    let name = "tagged-after-the-write";
    let body = document("tagged after the write");
    ok(
        &router,
        "PutDashboard",
        &json!({"DashboardName": name, "DashboardBody": &body}),
    )
    .await;
    assert_size_is_the_readable_document(&router, "before any tag", name, &body).await;

    // "The ARN of the CloudWatch resource that you're adding tags to."
    // (`TagResourceInput$ResourceARN`) — the dashboard's own ARN, as its reader reports it.
    let arn = ok(&router, "GetDashboard", &json!({"DashboardName": name}))
        .await
        .get("DashboardArn")
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .unwrap_or_else(|| panic!("case \"before any tag\": GetDashboard answers a DashboardArn"));
    ok(
        &router,
        "TagResource",
        &json!({"ResourceARN": arn, "Tags": tags(2)}),
    )
    .await;

    // Tags are not part of the dashboard document, so writing two of them leaves the number of
    // bytes the dashboard holds exactly where it was.
    assert_size_is_the_readable_document(&router, "after two tags were written", name, &body)
        .await;
}

#[tokio::test]
async fn replacing_the_document_moves_the_size_by_the_bytes_the_replacement_differs_by() {
    let router = build_router();
    let name = "replaced-document";
    // "If you update a dashboard, the entire contents are replaced with what you specify here."
    // (`PutDashboard`) — so each write's document is what the next listing measures. The rows
    // walk a longer document, a shorter one, one holding multi-byte characters (where bytes
    // outnumber characters) and one holding no widget at all.
    let rows: Vec<(&str, String)> = vec![
        ("the first document", document("one")),
        ("a longer document", document(&"long ".repeat(12))),
        ("a shorter document", document("x")),
        ("a document holding multi-byte characters", document("héllo ✅ 日本")),
        ("a document holding no widget", "{\"widgets\":[]}".to_string()),
    ];

    for (case, body) in &rows {
        ok(
            &router,
            "PutDashboard",
            &json!({"DashboardName": name, "DashboardBody": body}),
        )
        .await;
        assert_size_is_the_readable_document(&router, case, name, body).await;
    }

    // The multi-byte row is load-bearing only if its bytes really outnumber its characters: a
    // size counted in characters would be a different number for it.
    let (case, wide) = &rows[3];
    assert!(
        wide.len() > wide.chars().count(),
        "case {case:?}: the row must hold a character wider than one byte, got {} bytes for {} \
         characters",
        wide.len(),
        wide.chars().count()
    );
}
