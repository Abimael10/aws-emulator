// SPDX-License-Identifier: CC0-1.0

//! The response member the live DataZone wire renders that this checkout's Smithy model does
//! not declare.
//!
//! # The gap this closes
//!
//! The model a service crate is generated from is a SNAPSHOT of the API, and the live wire
//! has moved past it here: the domain listing's 200 body carries its domain summaries under
//! `domains`, while the model snapshot declares the member as `items`
//! (`smithy_query(query="operation",operation="ListDomains")` reports `items`, target
//! `DomainSummaries`, `@required`, with no `jsonName`). A recorded listing of an account that
//! holds no domain answers `domains` as an empty array — a `@required` list is on the wire
//! whether or not it has anything in it — so a client that reads the list by the name the
//! service renders reads nothing from this emulator at all.
//!
//! A member the model does not declare cannot be a field of a generated shape, so it cannot
//! be written from a handler. It is written HERE instead, at the one seam where this service
//! renders a SUCCESS body ([`super::rest_json_error_body::DataZoneRestJson::serialize_output`]),
//! beside the seam that owns what a FAILURE body carries
//! ([`super::rest_json_error_body`]).
//!
//! Nothing here invents a value: the member carries the SAME list the modelled member does,
//! read from the body the framework renderer just wrote from the handler's own output, so the
//! two names always report one answer and the modelled name keeps rendering exactly as it
//! did. An SDK generated from this model reads `items` and ignores the other name; a client
//! that reads the live wire's name now finds it.
//!
//! The body is re-read and re-written as JSON (a member the model does not declare cannot be
//! written through the schema serializer), which also puts the object's members in key order
//! — the order of a JSON object's members carries no meaning on this protocol, and both names
//! carry the same value whichever order they are written in.

use aws_smithy_runtime_api::http::Response;
use serde_json::Value;

use super::response_body::with_body;

/// The operation whose success body carries the member below: the domain listing.
const LIST_DOMAINS: &str = "ListDomains";

/// The member name the MODEL declares the domain summaries under.
const MODELLED_DOMAIN_SUMMARIES: &str = "items";

/// The member name the live wire renders them under as well.
const SERVED_DOMAIN_SUMMARIES: &str = "domains";

/// `body` as this service sends it for `operation` — the domain summaries also under the
/// name the live wire renders them — or `None` when this rule owns nothing about it: another
/// operation's body, a body that is not a JSON object, or one that already carries the
/// member.
#[must_use]
pub fn served_success_body(operation: &str, body: &[u8]) -> Option<Vec<u8>> {
    if operation != LIST_DOMAINS {
        return None;
    }
    let mut document: Value = serde_json::from_slice(body).ok()?;
    let object = document.as_object_mut()?;
    if object.contains_key(SERVED_DOMAIN_SUMMARIES) {
        return None;
    }
    let listed = object.get(MODELLED_DOMAIN_SUMMARIES)?.clone();
    object.insert(SERVED_DOMAIN_SUMMARIES.to_string(), listed);
    serde_json::to_vec(&document).ok()
}

/// `resp` as this service answers `operation`: a success whose body carries the members the
/// live wire renders ([`served_success_body`]).
///
/// A response this rule owns nothing about is returned untouched — status, headers and every
/// byte of the body exactly as the renderer wrote them.
#[must_use]
pub fn with_served_success_body(operation: &str, resp: Response) -> Response {
    let rewritten = {
        // A success body of this protocol is built in memory by its renderer, so the bytes
        // are readable here; a streamed body carries no member list and is left alone.
        let Some(bytes) = resp.body().bytes() else {
            return resp;
        };
        match served_success_body(operation, bytes) {
            Some(rewritten) => rewritten,
            None => return resp,
        }
    };
    with_body(resp, rewritten)
}

#[cfg(test)]
mod tests {
    use super::{served_success_body, with_served_success_body, SERVED_DOMAIN_SUMMARIES};
    use aws_smithy_types::body::SdkBody;
    use serde_json::{json, Value};

    /// A 200 response carrying `body`.
    fn response(body: &str) -> aws_smithy_runtime_api::http::Response {
        aws_smithy_runtime_api::http::Response::new(
            aws_smithy_runtime_api::http::StatusCode::try_from(200).expect("a valid status"),
            SdkBody::from(body),
        )
    }

    /// RULE over the bodies the framework renderer writes for the domain listing: the
    /// summaries are carried under the served name as well, with the same value — whatever
    /// the list holds, and whatever else the body carries. The rows differ along how many
    /// domains are listed and whether the body carries a pagination token.
    #[test]
    fn the_domain_listing_carries_its_summaries_under_the_served_name() {
        let rows: &[(&str, Value)] = &[
            ("no domains at all", json!({ "items": [] })),
            (
                "one domain",
                json!({ "items": [{ "id": "dzd_1", "name": "a" }] }),
            ),
            (
                "a page with more to come",
                json!({
                    "items": [{ "id": "dzd_1" }, { "id": "dzd_2" }],
                    "nextToken": "647a645f32",
                }),
            ),
        ];

        for (case, rendered) in rows {
            let bytes = serde_json::to_vec(rendered).expect("the row serializes");
            let rewritten = served_success_body("ListDomains", &bytes)
                .unwrap_or_else(|| panic!("case {case:?}: expected a rewritten body"));
            let document: Value =
                serde_json::from_slice(&rewritten).expect("the rewritten body is JSON");
            assert_eq!(
                document.get(SERVED_DOMAIN_SUMMARIES),
                rendered.get("items"),
                "case {case:?}: expected the served member to carry the summaries, got \
                 {document}"
            );
            assert_eq!(
                document.get("items"),
                rendered.get("items"),
                "case {case:?}: the modelled member must be unchanged, got {document}"
            );
            assert_eq!(
                document.get("nextToken"),
                rendered.get("nextToken"),
                "case {case:?}: every other member must be unchanged, got {document}"
            );
        }
    }

    /// Nothing to rewrite: another operation's body, a body carrying no summaries, one that
    /// already carries the served member, one that is not an object, and one that is not
    /// JSON.
    #[test]
    fn a_body_this_rule_does_not_own_is_left_alone() {
        for (case, operation, body) in [
            ("another operation", "GetDomain", r#"{"items":[]}"#),
            ("no summaries member", "ListDomains", r#"{"nextToken":"a"}"#),
            (
                "already served",
                "ListDomains",
                r#"{"items":[],"domains":[]}"#,
            ),
            ("not an object", "ListDomains", "[]"),
            ("not JSON", "ListDomains", "<Error/>"),
        ] {
            assert!(
                served_success_body(operation, body.as_bytes()).is_none(),
                "case {case:?}: expected no rewrite"
            );
            let answered = with_served_success_body(operation, response(body));
            assert_eq!(
                answered.body().bytes().expect("bytes"),
                body.as_bytes(),
                "case {case:?}: the body must be byte-identical"
            );
        }
    }

    /// A rewritten body is described by the `content-length` the renderer advertised.
    #[test]
    fn a_rewritten_body_is_described_by_its_advertised_length() {
        let body = r#"{"items":[]}"#;
        let mut resp = response(body);
        resp.headers_mut()
            .insert("content-length", body.len().to_string());
        let answered = with_served_success_body("ListDomains", resp);
        let bytes = answered.body().bytes().expect("bytes").to_vec();
        let advertised = answered.headers().get("content-length");
        assert_eq!(
            advertised,
            Some(bytes.len().to_string().as_str()),
            "the advertised length must describe the rewritten body, got {advertised:?}"
        );
    }
}
