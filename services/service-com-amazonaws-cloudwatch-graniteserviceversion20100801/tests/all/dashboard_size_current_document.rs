// SPDX-License-Identifier: CC0-1.0

//! WHICH text the size a dashboard listing reports is counted over when a widget leaves members
//! out, and WHEN that number moves.
//!
//! The rule under test: **"The size of the dashboard, in bytes." (`DashboardEntry$Size`, a long —
//! `smithy_query(query="shape",shape="DashboardEntry")`) is the byte length of the document the
//! service holds for that dashboard AS IT STANDS** — the text the write submitted, which is the
//! document its reader answers ("The detailed information about the dashboard, including what
//! widgets are included and their location on the dashboard.",
//! `smithy_query(query="doc",shape="GetDashboardOutput",member="DashboardBody")`), completed with
//! the top-level members of the dashboard body structure that the body left to the service.
//!
//! Two consequences a row here can land on, neither of them walked by the suites beside this one:
//!
//! * A WIDGET that leaves members out is measured by the text it was written with. The document
//!   the service holds is the one `GetDashboard` answers, so a member no document holds — a
//!   widget's position on the grid, the rendering a graph uses — is in no document's bytes
//!   either: two documents that differ in such a member are two sizes, and the one naming none of
//!   them is the smaller. `tests/all/dashboard_document_size.rs` walks documents whose widgets are
//!   written out in full; the rows below walk each widget kind the body structure defines with
//!   nothing but the member that kind requires.
//! * The number follows the document the dashboard holds NOW: a dashboard whose document was
//!   replaced, or whose name was freed and taken again, is measured by the document it holds at
//!   that point and never by one the name held before, and asking twice answers the same number
//!   both times, because a listing reports a dashboard rather than changing it.

use crate::dashboard_document_size::{expected_size, listed_entry, written_size_and_body};
use crate::harness::{build_router, ok};
use serde_json::{json, Value};

/// A metric widget naming its data source and nothing else: no position on the grid, no size on
/// it, and none of the rendering members a graph may choose.
const MINIMAL_METRIC_WIDGET: &str =
    r#"{"type":"metric","properties":{"metrics":[["MyApp","Latency"]],"region":"us-east-1"}}"#;

/// The same metric widget with its position, its size and its rendering members written out — a
/// longer text, so a larger dashboard.
const LAID_OUT_METRIC_WIDGET: &str = r#"{"type":"metric","x":0,"y":0,"width":12,"height":6,"properties":{"metrics":[["MyApp","Latency"]],"region":"us-east-1","view":"timeSeries","stacked":false,"period":300,"stat":"Average"}}"#;

/// A text widget naming only the markdown it renders.
const MINIMAL_TEXT_WIDGET: &str = r#"{"type":"text","properties":{"markdown":"Minimal note"}}"#;

/// The same text widget with its position and size written out.
const LAID_OUT_TEXT_WIDGET: &str =
    r#"{"type":"text","x":0,"y":0,"width":6,"height":3,"properties":{"markdown":"Minimal note"}}"#;

/// A dashboard document holding `widgets`, rendered compactly.
fn body_of(widgets: &[&str]) -> String {
    format!("{{\"widgets\":[{}]}}", widgets.join(","))
}

/// The `Size` the listing reports for `name` right now, read through the entry that listing
/// carries for it.
async fn listed_size(
    router: &smithy_server_runtime::MultiServiceRouter,
    name: &str,
) -> Option<i64> {
    listed_entry(router, name)
        .await
        .and_then(|entry| entry.get("Size").and_then(Value::as_i64))
}

/// Write `body` as the dashboard `name` and answer the size the listing then reports for it,
/// having first asserted that the document a client reads back is the text that was written.
///
/// The write, the listing and the read-back are the sequence
/// `dashboard_document_size::written_size_and_body` performs; what is added here is the pair of
/// assertions every row below makes of it.
async fn written_size(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    name: &str,
    body: &str,
) -> i64 {
    let (size, read_back) = written_size_and_body(router, name, body).await;
    assert_eq!(
        read_back.as_deref(),
        Some(body),
        "case {case:?}: the document the service holds is the one its reader answers, got \
         {read_back:?}"
    );
    let expected = expected_size(body);
    assert_eq!(
        size,
        Some(expected),
        "case {case:?}: expected the {expected} bytes of the document the service holds for this \
         {}-byte body, got {size:?}",
        body.len()
    );
    size.unwrap_or_default()
}

#[tokio::test]
async fn a_widget_that_leaves_members_out_is_measured_by_the_text_it_was_written_with() {
    let router = build_router();
    // One row per kind of widget the dashboard body structure defines a required property for,
    // each written with nothing BUT that property — the kinds and their defining members as
    // `src/ops/put_dashboard.rs` reads them off AWS's "Dashboard Body Structure and Syntax"
    // reference (`DEFINING_PROPERTY` and `metric_property_problems`, from the reference
    // `PutDashboardInput$DashboardBody` points at) — plus the same metric widget written out in
    // full, a document mixing the two renderings, and a minimal widget in a document that already
    // names the member the service fills in.
    let rows: Vec<(&str, String)> = vec![
        (
            "a metric widget naming only its data source",
            body_of(&[MINIMAL_METRIC_WIDGET]),
        ),
        (
            "the same metric widget with its layout and rendering written out",
            body_of(&[LAID_OUT_METRIC_WIDGET]),
        ),
        (
            "a text widget naming only its markdown",
            body_of(&[MINIMAL_TEXT_WIDGET]),
        ),
        (
            "an alarm widget naming only the alarm it watches",
            body_of(&[
                r#"{"type":"alarm","properties":{"alarms":["arn:aws:cloudwatch:us-east-1:000000000000:alarm:probe"]}}"#,
            ]),
        ),
        (
            "a log widget naming only its query",
            body_of(&[r#"{"type":"log","properties":{"query":"fields @timestamp"}}"#]),
        ),
        (
            "a custom widget naming only its endpoint",
            body_of(&[
                r#"{"type":"custom","properties":{"endpoint":"arn:aws:lambda:us-east-1:000000000000:function:widget"}}"#,
            ]),
        ),
        (
            "a document mixing a minimal widget and one written out in full",
            body_of(&[MINIMAL_TEXT_WIDGET, LAID_OUT_TEXT_WIDGET]),
        ),
        (
            "a minimal widget in a document that names the member the service fills in",
            format!(
                "{{\"widgets\":[{MINIMAL_METRIC_WIDGET}],\"periodOverride\":\"inherit\"}}"
            ),
        ),
    ];

    let mut sizes: Vec<(&str, i64)> = Vec::new();
    for (index, (case, body)) in rows.iter().enumerate() {
        let name = format!("widget-{index}");
        sizes.push((case, written_size(&router, case, &name, body).await));
    }

    // The two metric rows against each other: the written-out document holds the position, the
    // size and the rendering members the minimal one leaves out, so it is the larger dashboard by
    // exactly the bytes its text holds more of. A size counted over a filled-in rendering of
    // either document would answer one number for both.
    let (minimal_case, minimal) = sizes[0];
    let (written_out_case, written_out) = sizes[1];
    let carried = i64::try_from(LAID_OUT_METRIC_WIDGET.len() - MINIMAL_METRIC_WIDGET.len())
        .expect("the two widgets differ by a number of bytes that fits in an i64");
    assert!(
        carried > 0,
        "case {written_out_case:?}: the written-out widget must be the longer text, got \
         {carried} bytes more than case {minimal_case:?}'s"
    );
    assert_eq!(
        written_out - minimal,
        carried,
        "case {written_out_case:?} against {minimal_case:?}: the written-out document holds \
         {carried} bytes more than the minimal one, got {written_out} against {minimal}"
    );
}

#[tokio::test]
async fn the_size_reported_is_the_size_of_the_document_the_dashboard_holds_now() {
    let router = build_router();
    let name = "current-document";
    // That the document each write stores is what the next listing measures is walked by
    // `tests/all/dashboard_document_size.rs`; what is walked here is how long such a number
    // STANDS. Asking a listing again answers the same number, because a listing reports a
    // dashboard rather than changing one — so nothing about the size depends on how many times it
    // was read, whatever the document holds.
    let documents: Vec<(&str, String)> = vec![
        ("the document first written", body_of(&[LAID_OUT_METRIC_WIDGET])),
        (
            "a replacement that leaves a widget's layout out",
            body_of(&[MINIMAL_METRIC_WIDGET]),
        ),
    ];
    for (case, body) in &documents {
        let size = written_size(&router, case, name, body).await;
        let again = listed_size(&router, name).await;
        assert_eq!(
            again,
            Some(size),
            "case {case:?}: reading the listing again must report the same {size} bytes, got \
             {again:?}"
        );
    }

    // A name freed and taken again holds the document the NEW write submitted: what the name
    // reported before is not what the dashboard now under it is measured by.
    let held_before = body_of(&[LAID_OUT_METRIC_WIDGET]);
    let held_now = body_of(&[MINIMAL_METRIC_WIDGET]);
    let before = written_size(&router, "before the name was freed", name, &held_before).await;
    ok(
        &router,
        "DeleteDashboards",
        &json!({"DashboardNames": [name]}),
    )
    .await;
    let after = written_size(&router, "the name taken again", name, &held_now).await;
    assert_ne!(
        after,
        before,
        "case \"the name taken again\": the dashboard now under this name holds a {}-byte \
         document rather than the {}-byte one the name held before, got {after} for both",
        held_now.len(),
        held_before.len()
    );
}
