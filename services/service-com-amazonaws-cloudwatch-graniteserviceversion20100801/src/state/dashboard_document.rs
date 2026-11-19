// SPDX-License-Identifier: CC0-1.0

//! The dashboard DOCUMENT this store holds for a dashboard, and how many bytes it holds.
//!
//! A dashboard document is opaque text (`PutDashboardInput$DashboardBody` is a plain string),
//! so the text a write submits is what a later read answers, character for character. The
//! document the service HOLDS is not only that text, though: the dashboard body structure gives
//! some of its top-level members a default, and a document written without one of them holds
//! that default all the same — the dashboard behaves as if the member were written, because the
//! service completed the document when it stored it.
//!
//! "The size of the dashboard, in bytes." (`DashboardEntry$Size`, a long —
//! `smithy_query(query="shape",shape="DashboardEntry")`) is the size of THAT document: the
//! dashboard the service holds, not the request string it was written with. One function builds
//! it here, so the write that stores a dashboard and the listing that measures it count the same
//! bytes.

/// Every top-level member of a dashboard document whose value the service FILLS IN when the
/// write omits it, as the member the completed document carries: the name, and the default
/// value rendered as JSON.
///
/// `periodOverride` chooses whether each graph on the dashboard uses the period its own widget
/// names or the one the dashboard's time range implies, and a document that does not choose
/// holds the documented default `auto` — AWS's "Dashboard Body Structure and Syntax" reference,
/// which `PutDashboardInput$DashboardBody` points at ("For more information about the syntax,
/// see Dashboard Body Structure and Syntax."). The other top-level members that reference
/// defines — the widget array, and the time range a dashboard may fix — default to no value at
/// all rather than to one the document would hold, so they are not listed here.
const DEFAULTED_MEMBERS: &[(&str, &str)] = &[("periodOverride", "\"auto\"")];

/// The document this store holds for a dashboard whose write submitted `body`.
///
/// The submitted text is kept byte for byte, so a caller reading the dashboard back is handed
/// what it wrote; the only addition is the members the service defaults
/// ([`DEFAULTED_MEMBERS`]), each written into the document's top-level object when the body does
/// not already name it. The result is the submitted document completed — still one JSON object,
/// holding everything the body held plus every default it left to the service.
///
/// A body that is no JSON object is no dashboard document and is reported as it arrived: the
/// write that stores a dashboard refuses such a body before this store holds it
/// (`DashboardInvalidInputError`), so nothing a dashboard is listed from goes through this
/// branch.
pub fn stored_dashboard_document(body: &str) -> String {
    let parsed = ::serde_json::from_str::<::serde_json::Value>(body);
    let Ok(::serde_json::Value::Object(members)) = parsed else {
        return body.to_string();
    };
    let completed: Vec<String> = DEFAULTED_MEMBERS
        .iter()
        .filter(|(member, _)| !members.contains_key(*member))
        .map(|(member, value)| format!("\"{member}\":{value}"))
        .collect();
    if completed.is_empty() {
        return body.to_string();
    }
    // The members are written at the END of the top-level object, before the brace that closes
    // it, which is the only place they can go without touching a byte of what the caller wrote.
    let Some(closes) = body.rfind('}') else {
        return body.to_string();
    };
    let opens_a_member = if members.is_empty() { "" } else { "," };
    format!(
        "{}{opens_a_member}{}{}",
        &body[..closes],
        completed.join(","),
        &body[closes..]
    )
}

/// How many bytes the document this store holds for a dashboard written with `body` holds:
/// "The size of the dashboard, in bytes." (`DashboardEntry$Size`).
pub fn stored_dashboard_document_bytes(body: &str) -> usize {
    stored_dashboard_document(body).len()
}

#[cfg(test)]
mod tests {
    use super::{stored_dashboard_document, stored_dashboard_document_bytes};

    /// The completed document is still one JSON object, holding what the body held and the
    /// default the body omitted.
    #[test]
    fn a_body_omitting_a_defaulted_member_is_completed_with_it() {
        let body = "{\"widgets\":[]}";
        let document = stored_dashboard_document(body);
        let parsed = ::serde_json::from_str::<::serde_json::Value>(&document)
            .expect("the completed document is JSON");
        assert_eq!(
            parsed.get("periodOverride").and_then(|value| value.as_str()),
            Some("auto"),
            "expected the completed document to hold the default, got {document}"
        );
        assert!(
            parsed
                .get("widgets")
                .and_then(::serde_json::Value::as_array)
                .is_some_and(Vec::is_empty),
            "expected the submitted members to survive completion, got {document}"
        );
        assert_eq!(
            stored_dashboard_document_bytes(body),
            document.len(),
            "the size reported is the size of that document"
        );
    }

    /// A body that already names the member chose a value for it, so the document the store
    /// holds is the body itself.
    #[test]
    fn a_body_naming_a_defaulted_member_is_held_as_written() {
        for body in [
            "{\"periodOverride\":\"inherit\",\"widgets\":[]}",
            "{\"widgets\": [], \"periodOverride\": \"auto\"}",
        ] {
            let document = stored_dashboard_document(body);
            assert_eq!(
                document, body,
                "case {body:?}: a document naming every defaulted member is held as written"
            );
        }
    }
}
