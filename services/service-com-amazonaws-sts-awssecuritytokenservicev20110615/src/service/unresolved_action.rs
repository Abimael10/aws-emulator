// SPDX-License-Identifier: CC0-1.0

//! The refusal this service answers a call whose `Action` names no operation it has.
//!
//! An `awsQuery` call states the operation it is for in its `Action` field and the API
//! version that operation is asked for in its `Version` field, and the service resolves
//! the PAIR: a request naming an action this model declares no operation for is refused
//! `400` `InvalidAction`, naming the action it could not find and the version it looked
//! for it in. That sentence is one rule for every Query front door and is stated once
//! ([`::service_common::query_unresolved_action`]); what this module owns is the part only
//! this service knows — WHERE the pair is read from, and which version a call that names
//! none is looked up at.
//!
//! Both halves of the pair are read from wherever the request carries its CALL, which is
//! the same choice the protocol this service serves with makes for routing and for
//! deserializing ([`super::QuerySignedCalls`]): the form-encoded body where the call is a
//! body, and the query string where SigV4's query-parameter scheme put the whole call
//! there instead. Reading the body alone would answer a `GET` whose query string plainly
//! carries `Action=…` that its request is missing the field it did send.
//!
//! A request that names NO action anywhere is left to the framework's own diagnostic:
//! there is no action to report as not found, and the absence of the field is a different
//! thing to tell the caller about than an action that does not resolve.

use aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse};

/// The one API version this service's model declares, from
/// `smithy_query(query="service_info",
/// model_path="services/service-com-amazonaws-sts-awssecuritytokenservicev20110615/smithy-model/com-amazonaws-sts-awssecuritytokenservicev20110615.json")`
/// (`version: 2011-06-15`).
///
/// It is the version a call that states none is looked up at: the version is the client's
/// statement of which API it compiled against, and with none stated there is nothing to
/// disagree with, so the front door's own API is the one the action was not found in.
pub const API_VERSION: &str = "2011-06-15";

/// The `awsQuery` field naming the API version a call is made against.
const VERSION_FIELD: &str = "Version";

/// The refusal for `req`, or `None` when the request names no action at all (see the
/// [module docs](self)), which leaves it on the framework's own rendering.
///
/// The envelope is the shared one, then passed through the two writers every error of this
/// service passes through — its `<Message>` stated as a text node
/// ([`super::message_text`]) and the `<RequestId>` that envelope carries
/// ([`::service_common::query_error_envelope`]) — because this seam is reached by the
/// router directly rather than through the wrapped definition those writers live on.
pub(super) fn unresolved_action_response(req: &RtRequest) -> Option<RtResponse> {
    let call = call(req)?;
    let action = stated_field(call, super::ACTION_FIELD)?;
    let version = stated_field(call, VERSION_FIELD).unwrap_or_else(|| API_VERSION.to_string());
    let refusal = ::service_common::query_unresolved_action::unresolved_pair_response(
        &action, &version,
    );
    Some(::service_common::query_error_envelope::with_request_id(
        super::message_text::with_literal_message_text(refusal),
    ))
}

/// The `name=value&…` text `req` carries its CALL in: the query string when SigV4 put the
/// call there, and the form-encoded body otherwise — the same choice this service's
/// protocol makes ([`super::call_in_query_string`]).
fn call(req: &RtRequest) -> Option<&str> {
    match super::call_in_query_string(req) {
        Some(query) => Some(query),
        None => ::std::str::from_utf8(req.body().bytes()?).ok(),
    }
}

/// The value of one field of a call, decoded as the form encoding both the body and a
/// signed query string are written in — the framework's own reader, so a value a client
/// percent-encoded is reported as the client spelled it.
fn stated_field(call: &str, name: &str) -> Option<String> {
    ::aws_smithy_schema_server::parse_form_urlencoded(call.as_bytes())
        .into_iter()
        .find(|(field, _)| field == name)
        .map(|(_, value)| value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aws_smithy_types::body::SdkBody;

    /// One request as the router hands it to this seam: a method, a URI and a body.
    fn request(method: &str, uri: &str, body: &str) -> RtRequest {
        let mut req = RtRequest::new(SdkBody::from(body.to_string()));
        req.set_method(method)
            .expect("the method is a valid HTTP method");
        req.set_uri(uri).expect("the URI is valid");
        req
    }

    /// The text of the one `<name>…</name>` element of the refusal.
    fn element(body: &str, name: &str) -> Option<String> {
        let open = format!("<{name}>");
        let close = format!("</{name}>");
        let start = body.find(&open)? + open.len();
        let end = body[start..].find(&close)? + start;
        Some(body[start..end].to_string())
    }

    /// The `<Message>` of the refusal for `req`, or `None` where this seam declines.
    fn message(req: &RtRequest) -> Option<String> {
        let resp = unresolved_action_response(req)?;
        let body = String::from_utf8(resp.body().bytes().expect("an in-memory body").to_vec())
            .expect("a UTF-8 body");
        element(&body, "Message")
    }

    /// RULE: the pair is read from wherever the request carries its call, and a request
    /// carrying no action anywhere is left to the framework.
    #[test]
    fn the_pair_is_read_from_the_call_wherever_it_is_carried() {
        // The rows differ along where the call is (a form body, a query string, both), and
        // whether the call states a version at all.
        let rows: &[(&str, RtRequest, Option<&str>)] = &[
            (
                "a form-encoded body",
                request("POST", "/", "Action=BogusAction&Version=2011-06-15"),
                Some("Could not find operation BogusAction for version 2011-06-15"),
            ),
            (
                "a query string, with no body at all",
                request("GET", "/?Action=BogusAction&Version=2011-06-15", ""),
                Some("Could not find operation BogusAction for version 2011-06-15"),
            ),
            (
                "a query string naming another version",
                request("GET", "/?Action=BogusAction&Version=2099-01-01", ""),
                Some("Could not find operation BogusAction for version 2099-01-01"),
            ),
            (
                "a call stating no version",
                request("GET", "/?Action=BogusAction", ""),
                Some("Could not find operation BogusAction for version 2011-06-15"),
            ),
            (
                "a body-carried call, with a query string beside it",
                request(
                    "POST",
                    "/?Action=FromTheQuery&Version=2099-01-01",
                    "Action=FromTheBody&Version=2011-06-15",
                ),
                Some("Could not find operation FromTheBody for version 2011-06-15"),
            ),
            (
                "a percent-encoded action",
                request("GET", "/?Action=Bogus%20Action&Version=2011-06-15", ""),
                Some("Could not find operation Bogus Action for version 2011-06-15"),
            ),
            (
                "a request naming no action anywhere",
                request("POST", "/", "Version=2011-06-15"),
                None,
            ),
            ("an empty request", request("POST", "/", ""), None),
        ];
        for (case, req, expected) in rows {
            let actual = message(req);
            assert_eq!(
                actual.as_deref(),
                *expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// RULE: the refusal carries the status, the code and the envelope members an
    /// `awsQuery` error of this service carries, the `<RequestId>` included.
    #[test]
    fn the_refusal_is_this_services_own_invalid_action_envelope() {
        let req = request("GET", "/?Action=BogusAction&Version=2011-06-15", "");
        let resp = unresolved_action_response(&req).expect("an action that resolves to nothing");
        let status = resp.status().as_u16();
        let body = String::from_utf8(resp.body().bytes().expect("an in-memory body").to_vec())
            .expect("a UTF-8 body");
        let stated = (
            status,
            element(&body, "Type"),
            element(&body, "Code"),
            element(&body, "RequestId").is_some(),
        );
        let expected = (
            400,
            Some("Sender".to_string()),
            Some("InvalidAction".to_string()),
            true,
        );
        assert_eq!(
            stated, expected,
            "expected {expected:?}, got {stated:?} for {body}"
        );
    }
}
