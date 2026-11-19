// SPDX-License-Identifier: CC0-1.0

//! WHICH document the size a dashboard listing reports is the size OF.
//!
//! The rule under test: **"The size of the dashboard, in bytes." (`DashboardEntry$Size`, a long —
//! `smithy_query(query="shape",shape="DashboardEntry")`) is the byte length of the document the
//! service HOLDS for that dashboard — the body the write submitted, byte for byte, completed with
//! every top-level member of the dashboard body structure that the body left to the service.**
//! A dashboard whose document does not choose a value for such a member still behaves as though
//! it did, because the service completed the document when it stored it, and the size it reports
//! is the size of that completed document rather than of the request string.
//!
//! Only one top-level member of a dashboard body has a value the service fills in,
//! `periodOverride` (see [`DEFAULTED_MEMBER`]), so the rule has two halves a row can land on: a
//! document that names it is measured by its own bytes, and a document that omits it is measured
//! by those bytes plus the member it is held with. The rows below differ along what the rule
//! generalises over — the document's shape, its size, the rendering its text uses, how many
//! widgets it holds, whether its text is ASCII, whether it names the defaulted member and which
//! value it names, and whether the dashboard was created or replaced — so the one dashboard a
//! recorded request happens to create is one row among many.
//!
//! `tests/all/dashboard_size_terms.rs` holds the complement: the terms the size must NOT carry
//! (the name, the ARN, the tags), and `tests/all/dashboard_body_byte_fidelity.rs` the relation
//! between two renderings of one value.

use crate::harness::{build_router, ok};
use serde_json::{json, Value};

/// The member a stored dashboard document holds for the one choice a body may leave to the
/// service, rendered as that document carries it.
///
/// `periodOverride` chooses whether each graph on the dashboard uses the period its own widget
/// names or the one the dashboard's time range implies, and `auto` is the value a document that
/// does not choose holds — AWS's "Dashboard Body Structure and Syntax" reference, which
/// `PutDashboardInput$DashboardBody` points at ("For more information about the syntax, see
/// Dashboard Body Structure and Syntax."). The other top-level members that reference defines
/// (the widget array and the time range a dashboard may fix) have no value the service fills in.
const DEFAULTED_MEMBER: &str = "\"periodOverride\":\"auto\"";

/// The name of that member, as a document names it.
const DEFAULTED_MEMBER_NAME: &str = "periodOverride";

/// The size a listing must report for a dashboard written with `body`, stated as the rule reads:
/// the bytes of the document the service holds — the submitted text, plus the member above when
/// the text does not name it, and the comma that member needs unless the document holds nothing
/// else for it to follow.
pub fn expected_size(body: &str) -> i64 {
    let document = serde_json::from_str::<Value>(body).expect("a row's document is JSON");
    let members = document.as_object().expect("a row's document is an object");
    let completion = if members.contains_key(DEFAULTED_MEMBER_NAME) {
        0
    } else {
        DEFAULTED_MEMBER.len() + usize::from(!members.is_empty())
    };
    i64::try_from(body.len() + completion).expect("a row's document fits in an i64")
}

/// A dashboard document holding `count` metric widgets, rendered compactly.
fn body_with_widgets(count: usize) -> String {
    let widgets: Vec<Value> = (0..count)
        .map(|index| {
            json!({
                "type": "metric",
                "x": 0,
                "y": index * 6,
                "width": 12,
                "height": 6,
                "properties": {
                    "metrics": [["MyApp", format!("Latency{index}")]],
                    "period": 300,
                    "stat": "Average",
                    "region": "us-east-1",
                },
            })
        })
        .collect();
    serde_json::to_string(&json!({"widgets": widgets})).expect("the body serializes")
}

/// One text widget whose `markdown` is `label`, with a space after every separator — the
/// rendering a caller's own serializer writes by default, which holds more bytes than the compact
/// one for the same value.
fn spaced_text_widget(label: &str) -> String {
    format!(
        "{{\"widgets\": [{{\"type\": \"text\", \"x\": 0, \"y\": 0, \"width\": 6, \"height\": 3, \
         \"properties\": {{\"markdown\": \"{label}\"}}}}]}}"
    )
}

/// The `DashboardEntry` a listing reports for `name`.
async fn listed_entry(
    router: &smithy_server_runtime::MultiServiceRouter,
    name: &str,
) -> Option<Value> {
    ok(
        router,
        "ListDashboards",
        &json!({"DashboardNamePrefix": name}),
    )
    .await
    .get("DashboardEntries")?
    .as_array()?
    .iter()
    .find(|entry| entry.get("DashboardName").and_then(Value::as_str) == Some(name))
    .cloned()
}

/// Write `body` as the dashboard `name`, then answer the size the listing reports for it and the
/// document `GetDashboard` reads back.
async fn written_size_and_body(
    router: &smithy_server_runtime::MultiServiceRouter,
    name: &str,
    body: &str,
) -> (Option<i64>, Option<String>) {
    ok(
        router,
        "PutDashboard",
        &json!({"DashboardName": name, "DashboardBody": body}),
    )
    .await;
    let size = listed_entry(router, name)
        .await
        .and_then(|entry| entry.get("Size").and_then(Value::as_i64));
    let read_back = ok(router, "GetDashboard", &json!({"DashboardName": name}))
        .await
        .get("DashboardBody")
        .and_then(Value::as_str)
        .map(ToString::to_string);
    (size, read_back)
}

#[tokio::test]
async fn every_dashboard_is_measured_by_the_document_the_service_holds() {
    let router = build_router();
    // Rows differ along the document's shape and size, the rendering of its text, the number of
    // widgets it holds, whether that text is ASCII, and whether it names the defaulted member —
    // and for the two that name it, which of the member's values they name.
    let rows: Vec<(&str, String)> = vec![
        ("a document holding nothing", "{}".to_string()),
        (
            "a document holding no widget",
            "{\"widgets\":[]}".to_string(),
        ),
        ("a document holding one widget", body_with_widgets(1)),
        ("a document holding many widgets", body_with_widgets(40)),
        (
            "a document rendered with a space after each separator",
            spaced_text_widget("## Dashboard"),
        ),
        (
            "a document holding multi-byte text",
            spaced_text_widget("héllo ✅ 日本"),
        ),
        (
            "a document naming the default for the member the service fills in",
            "{\"widgets\":[],\"periodOverride\":\"auto\"}".to_string(),
        ),
        (
            "a document naming the member's other value",
            "{\"widgets\":[],\"periodOverride\":\"inherit\"}".to_string(),
        ),
        (
            "a document naming that member first, spaced",
            "{\"periodOverride\": \"inherit\", \"widgets\": []}".to_string(),
        ),
    ];

    for (index, (case, body)) in rows.iter().enumerate() {
        let name = format!("document-{index}");
        let (size, read_back) = written_size_and_body(&router, &name, body).await;
        // The document a client reads back is the one it wrote, whatever the service completed
        // for itself: a dashboard body is opaque text.
        assert_eq!(
            read_back.as_deref(),
            Some(body.as_str()),
            "case {case:?}: GetDashboard must answer the document that was written, got \
             {read_back:?}"
        );
        let expected = expected_size(body);
        assert_eq!(
            size,
            Some(expected),
            "case {case:?}: expected the {expected} bytes of the document the service holds for \
             a {}-byte body, got {size:?}",
            body.len()
        );
    }
}

#[tokio::test]
async fn naming_the_default_the_service_fills_in_leaves_the_dashboard_the_same_size() {
    let router = build_router();
    // A document that names the default and one that leaves it to the service are the SAME
    // dashboard, so they are the same number of bytes — the sharpest consequence of measuring the
    // completed document rather than the request string, and one no size taken from the submitted
    // text alone can satisfy. The rows differ in the document the pair is built from.
    let bases: Vec<(&str, String)> = vec![
        ("no widget", "{\"widgets\":[]}".to_string()),
        ("one metric widget", body_with_widgets(1)),
        ("three metric widgets", body_with_widgets(3)),
        (
            "a text widget rendered with spaces",
            spaced_text_widget("## Dashboard"),
        ),
    ];

    for (index, (case, base)) in bases.iter().enumerate() {
        // The same value, once leaving the member to the service and once naming the value the
        // service would fill in, each written compactly into the base document's own text.
        let closes = base.rfind('}').expect("a row's document closes");
        let naming_it = format!("{},{DEFAULTED_MEMBER}{}", &base[..closes], &base[closes..]);

        let omitted = format!("omitted-{index}");
        let named = format!("named-{index}");
        let (omitted_size, _) = written_size_and_body(&router, &omitted, base).await;
        let (named_size, _) = written_size_and_body(&router, &named, &naming_it).await;

        assert_eq!(
            omitted_size, named_size,
            "case {case:?}: a document that leaves the service's default implicit and one that \
             names it are one dashboard, so they are one size, got {omitted_size:?} against \
             {named_size:?}"
        );
        let expected = expected_size(&naming_it);
        assert_eq!(
            named_size,
            Some(expected),
            "case {case:?}: expected both to be the {expected} bytes the completed document \
             holds, got {named_size:?}"
        );
    }
}

#[tokio::test]
async fn replacing_a_document_measures_the_replacement() {
    let router = build_router();
    // "If you update a dashboard, the entire contents are replaced with what you specify here."
    // (`PutDashboard`) — so each write's document is what the next listing measures, including a
    // write that moves the dashboard between the rule's two halves.
    let name = "replaced-document";
    let rows: Vec<(&str, String)> = vec![
        ("the first document", body_with_widgets(1)),
        ("a longer document", body_with_widgets(6)),
        (
            "a document that now names the defaulted member",
            "{\"widgets\":[],\"periodOverride\":\"inherit\"}".to_string(),
        ),
        ("a document that leaves it implicit again", body_with_widgets(2)),
        ("a document holding nothing", "{}".to_string()),
    ];

    for (case, body) in &rows {
        let (size, read_back) = written_size_and_body(&router, name, body).await;
        assert_eq!(
            read_back.as_deref(),
            Some(body.as_str()),
            "case {case:?}: the replacement is the document the dashboard holds, got {read_back:?}"
        );
        let expected = expected_size(body);
        assert_eq!(
            size,
            Some(expected),
            "case {case:?}: expected the replacement to be measured at {expected} bytes, got \
             {size:?}"
        );
    }
}
