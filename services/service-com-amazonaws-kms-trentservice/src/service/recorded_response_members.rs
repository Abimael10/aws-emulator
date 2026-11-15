// SPDX-License-Identifier: CC0-1.0

//! The response members this service renders that this checkout's Smithy model does not
//! declare.
//!
//! # The rule this module implements
//!
//! An alias listing reports HOW MANY aliases the answer carries, beside the aliases
//! themselves: a `ListAliases` success names the page's size in `AliasCount`, so a caller
//! reads the size of the page it was given without walking the list, and the member is
//! present on every such answer — a full page, a short last page, a list narrowed to one
//! key, and an endpoint holding no aliases at all (`0`).
//!
//! The count is of the ALIASES THIS ANSWER CARRIES, which is what makes it consistent with
//! the two members it sits beside: `Truncated` says whether more remain and `NextMarker`
//! says where to resume, so the count that describes this response is the length of the
//! page in it, not a total across pages the caller cannot see.
//!
//! # Why it is written here
//!
//! The model a service crate is generated from is a SNAPSHOT of the API, and this one
//! declares `ListAliasesResponse` with `Aliases`, `NextMarker` and `Truncated` alone
//! (`smithy_query(query="operation",operation="ListAliases")`), while the wire this service
//! emulates answers the count beside them. A member the model does not declare cannot be a
//! field of a generated shape, so it cannot be written from the operation's handler: it is
//! written HERE instead, at the one seam this service renders a success body through
//! ([`RecordedResponseJson::serialize_output`]), the same way the EC2 crate writes the
//! elements its own model lost.
//!
//! Nothing here invents a value: the count is read back out of the page the handler
//! computed, so an answer reporting no aliases reports a count of zero and a page the
//! handler shortened reports the shortened length — and the count is never written beside a
//! page that is absent, since a body naming no list is answered the empty list the count
//! then describes. A response of any other operation, and one whose body is not a JSON
//! object at all, is returned byte for byte.

use aws_smithy_runtime_api::http::{Request, Response};
use aws_smithy_schema::serde::{SerializableStruct, ShapeDeserializer};
use aws_smithy_schema::{Schema, ShapeId};
use aws_smithy_schema_server::json::JsonCodec;
use aws_smithy_schema_server::protocol::{ProtocolError, ServerProtocol};
use aws_smithy_schema_server::rpc::HttpRpcServerProtocol;
use aws_smithy_types::body::SdkBody;
use serde_json::Value;

/// The operation whose answer carries a count of the page it reports.
const LIST_ALIASES: &str = "ListAliases";

/// The modelled member an alias listing reports its page in.
const ALIASES_MEMBER: &str = "Aliases";

/// The member that page's size is reported in.
const ALIAS_COUNT_MEMBER: &str = "AliasCount";

/// The header a rendered body's length is advertised in, when the renderer advertised one.
const CONTENT_LENGTH: &str = "content-length";

/// `resp`, the rendered success of `operation_name`, as this service answers it: an alias
/// listing reports the size of the page it carries (see the module docs).
///
/// Every other operation's answer, and a listing whose body is not a JSON object, is
/// returned untouched — status, headers and every byte of the body exactly as the renderer
/// wrote them.
#[must_use]
pub fn with_recorded_members(operation_name: &str, resp: Response) -> Response {
    if operation_name != LIST_ALIASES {
        return resp;
    }
    with_rewritten_body(resp, body_reporting_alias_count)
}

/// `body` with the size of the page it carries reported in [`ALIAS_COUNT_MEMBER`], or
/// `None` when the body is not a JSON object and so reports no page at all.
///
/// A body carrying no alias list reports an EMPTY list and a count of zero rather than
/// neither: the two members describe one page, so a count is never written beside a page
/// that is absent. Every other member is written back as it was, so only those two are
/// added.
#[must_use]
fn body_reporting_alias_count(body: &[u8]) -> Option<Vec<u8>> {
    let mut document: Value = serde_json::from_slice(body).ok()?;
    let object = document.as_object_mut()?;
    let page = object
        .entry(ALIASES_MEMBER)
        .or_insert_with(|| Value::Array(Vec::new()));
    let counted = page.as_array().map_or(0, Vec::len);
    object.insert(ALIAS_COUNT_MEMBER.to_string(), Value::from(counted));
    serde_json::to_vec(&document).ok()
}

/// `resp` with its body replaced by what `rewrite` makes of it and the advertised length
/// corrected to match.
///
/// A response whose body is not in memory, and one `rewrite` answers `None` for, is
/// returned untouched. A length header value the HTTP layer will not carry is REMOVED
/// rather than left describing the body before the rewrite, so the two can never disagree.
fn with_rewritten_body(
    mut resp: Response,
    rewrite: impl FnOnce(&[u8]) -> Option<Vec<u8>>,
) -> Response {
    let rewritten = {
        // A JSON success body is built in memory by its renderer, so the bytes are
        // readable here; a streamed body carries no JSON document and is left alone.
        let Some(bytes) = resp.body().bytes() else {
            return resp;
        };
        match rewrite(bytes) {
            Some(rewritten) => rewritten,
            None => return resp,
        }
    };
    if resp.headers().get(CONTENT_LENGTH).is_some() {
        let length = rewritten.len().to_string();
        if resp.headers_mut().try_insert(CONTENT_LENGTH, length).is_err() {
            resp.headers_mut().remove(CONTENT_LENGTH);
        }
    }
    *resp.body_mut() = SdkBody::from(rewritten);
    resp
}

/// This service's wire protocol: `awsJson1_1` exactly as the model declares it, with the
/// response members this service is recorded answering beside the modelled ones (see the
/// module docs).
///
/// Routing, request deserialization and every error stay with the framework's own
/// `awsJson1_1`, built here exactly as the generated entry builds it.
#[derive(Debug)]
pub struct RecordedResponseJson {
    inner: HttpRpcServerProtocol<JsonCodec>,
}

impl RecordedResponseJson {
    /// Build the protocol once, at service construction:
    /// [`smithy_server_runtime::Service::override_protocol`] hands back a borrow of it and
    /// must never build one per request.
    pub fn new() -> Self {
        Self {
            inner: aws_smithy_schema_server::rpc::AwsJson11(),
        }
    }
}

impl Default for RecordedResponseJson {
    fn default() -> Self {
        Self::new()
    }
}

impl ServerProtocol for RecordedResponseJson {
    type Request = Request;
    type Response = Response;

    fn protocol_id(&self) -> &ShapeId {
        self.inner.protocol_id()
    }

    fn routing_key<'a>(&self, request: &'a Self::Request) -> Result<&'a str, ProtocolError> {
        self.inner.routing_key(request)
    }

    fn deserialize_input<'a>(
        &self,
        request: &'a Self::Request,
        input_schema: &Schema,
    ) -> Result<Box<dyn ShapeDeserializer + 'a>, ProtocolError> {
        // Nothing about a REQUEST changes: it is read by the model's own codec.
        self.inner.deserialize_input(request, input_schema)
    }

    fn serialize_output(
        &self,
        output: &dyn SerializableStruct,
        output_schema: &Schema,
        operation_name: &str,
        success_status: u16,
    ) -> Result<Self::Response, ProtocolError> {
        let resp = self
            .inner
            .serialize_output(output, output_schema, operation_name, success_status)?;
        Ok(with_recorded_members(operation_name, resp))
    }

    fn serialize_error(
        &self,
        error: &dyn SerializableStruct,
        error_schema: &Schema,
        error_code: &str,
        http_status: u16,
    ) -> Result<Self::Response, ProtocolError> {
        // A failure is the framework's own, byte for byte: no member of this module's
        // concern belongs in one.
        self.inner
            .serialize_error(error, error_schema, error_code, http_status)
    }

    fn serialize_event_stream_message(
        &self,
        event_type: &str,
        is_exception: bool,
        event: &dyn SerializableStruct,
        event_schema: &Schema,
    ) -> Result<Self::Response, ProtocolError> {
        self.inner
            .serialize_event_stream_message(event_type, is_exception, event, event_schema)
    }
}

#[cfg(test)]
mod tests {
    use super::{with_recorded_members, ALIAS_COUNT_MEMBER, CONTENT_LENGTH, LIST_ALIASES};
    use aws_smithy_runtime_api::http::{Response, StatusCode};
    use aws_smithy_types::body::SdkBody;
    use serde_json::Value;

    /// A rendered response carrying `body`.
    fn response(body: &str) -> Response {
        Response::new(
            StatusCode::try_from(200).expect("a valid status"),
            SdkBody::from(body),
        )
    }

    /// The body of `resp` as JSON.
    fn decoded(resp: &Response) -> Value {
        serde_json::from_slice(resp.body().bytes().expect("an in-memory body"))
            .expect("the body is JSON")
    }

    /// One alias entry, as the renderer writes one.
    fn entry(name: &str) -> String {
        format!(
            r#"{{"AliasName":"{name}","AliasArn":"arn:aws:kms:us-east-1:000000000000:{name}",
             "TargetKeyId":"11111111-2222-3333-4444-555555555555"}}"#
        )
        .replace('\n', "")
    }

    /// RULE: an alias listing reports the size of the page it carries. The rows differ
    /// along what the rule generalises over — how many aliases the page holds (none, one,
    /// several), whether the answer is truncated or complete, and whether the rendered body
    /// names the page at all.
    #[test]
    fn an_alias_listing_reports_the_size_of_the_page_it_carries() {
        let rows: Vec<(&str, String, u64)> = vec![
            (
                "an endpoint holding no aliases",
                r#"{"Aliases":[],"Truncated":false}"#.to_string(),
                0,
            ),
            (
                "a single alias",
                format!(r#"{{"Aliases":[{}],"Truncated":false}}"#, entry("alias/aws/backup")),
                1,
            ),
            (
                "several aliases",
                format!(
                    r#"{{"Aliases":[{},{},{}],"Truncated":false}}"#,
                    entry("alias/one"),
                    entry("alias/two"),
                    entry("alias/three")
                ),
                3,
            ),
            (
                "a truncated page",
                format!(
                    r#"{{"Aliases":[{},{}],"NextMarker":"abcd","Truncated":true}}"#,
                    entry("alias/one"),
                    entry("alias/two")
                ),
                2,
            ),
            (
                "a body naming no page at all",
                r#"{"Truncated":false}"#.to_string(),
                0,
            ),
        ];

        for (case, body, expected) in rows {
            let answered = with_recorded_members(LIST_ALIASES, response(&body));
            let document = decoded(&answered);
            let actual = document.get(ALIAS_COUNT_MEMBER).and_then(Value::as_u64);
            assert_eq!(
                actual,
                Some(expected),
                "case {case:?}: expected {ALIAS_COUNT_MEMBER} {expected}, got {document}"
            );
            // The count describes the page the answer carries, so the two always agree.
            let page = document
                .get("Aliases")
                .and_then(Value::as_array)
                .map(Vec::len);
            assert_eq!(
                page,
                Some(usize::try_from(expected).expect("a page fits in usize")),
                "case {case:?}: expected a page of {expected}, got {document}"
            );
            // Every modelled member survives with its value.
            let rendered: Value = serde_json::from_str(&body).expect("the row is JSON");
            for (member, value) in rendered.as_object().expect("an object") {
                assert_eq!(
                    document.get(member),
                    Some(value),
                    "case {case:?}: expected {member} to survive as {value}, got {document}"
                );
            }
        }
    }

    /// Nothing to count: another operation's answer, and a body that is not a JSON object,
    /// are answered byte for byte.
    #[test]
    fn an_answer_that_is_not_an_alias_listing_is_returned_unchanged() {
        for (case, operation, body) in [
            ("another listing", "ListKeys", r#"{"Keys":[],"Truncated":false}"#),
            ("a key read", "DescribeKey", r#"{"KeyMetadata":{"KeyId":"x"}}"#),
            ("a body that is not an object", LIST_ALIASES, r#"["alias/one"]"#),
            ("a body that is not JSON", LIST_ALIASES, "not json at all"),
        ] {
            let answered = with_recorded_members(operation, response(body));
            assert_eq!(
                answered.body().bytes().expect("an in-memory body"),
                body.as_bytes(),
                "case {case:?}: expected the body unchanged"
            );
        }
    }

    /// A rewritten body's advertised length describes the bytes that were written.
    #[test]
    fn a_rewritten_body_is_described_by_its_advertised_length() {
        let body = r#"{"Aliases":[],"Truncated":false}"#;
        let mut resp = response(body);
        resp.headers_mut()
            .insert(CONTENT_LENGTH, body.len().to_string());
        let answered = with_recorded_members(LIST_ALIASES, resp);
        let length = answered
            .body()
            .bytes()
            .expect("an in-memory body")
            .len()
            .to_string();
        assert_eq!(
            answered.headers().get(CONTENT_LENGTH),
            Some(length.as_str()),
            "the advertised length must describe the rewritten body, got {:?}",
            answered.headers().get(CONTENT_LENGTH)
        );
    }
}
