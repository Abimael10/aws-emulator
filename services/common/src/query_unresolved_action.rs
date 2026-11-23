// SPDX-License-Identifier: CC0-1.0

//! The refusal an AWS Query front door answers an `Action`/`Version` pair it cannot
//! resolve with.
//!
//! An AWS Query request states the call it is making in two fields: `Action`, the
//! operation, and `Version`, the API version that operation is asked for. The service
//! resolves the PAIR, and a pair it cannot resolve — an `Action` no operation of the
//! serving model answers, or an `Action` asked for at a version that model does not
//! declare — is refused HTTP `400` with the code `InvalidAction` and the sentence
//!
//! ```text
//! Could not find operation <Action> for version <Version>
//! ```
//!
//! whose two variable parts are the request's OWN `Action` and `Version` values.
//!
//! That is one sentence for every Query front door, so it lives here rather than beside
//! each one: an action no model declares, an action asked for at a version no model
//! declares, and an action named in a query string rather than in a form body are the one
//! refusal, and cannot be worded differently from each other.
//!
//! What stays with the service is everything the service alone knows:
//!
//! * WHERE the pair is read from — the form-encoded body is where the protocol puts it,
//!   while a call signed under SigV4's query-parameter scheme carries the whole call,
//!   `Action` and `Version` included, in the query string instead;
//! * which version a request that names none is served as, which is the serving model's
//!   own declared API version;
//! * the `<RequestId>` the `awsQuery` error envelope carries, added by the writer every
//!   error of that service passes through ([`crate::query_error_envelope`]).
//!
//! The status, the code and the sentence are the only things stated here, and the envelope
//! itself is the framework's own `awsQuery` error document
//! (`aws_smithy_schema_server::query_error_body`), so a refusal raised on this path and one
//! a generated definition raises cannot answer in different shapes.

use ::aws_smithy_runtime_api::http::{Response as RtResponse, StatusCode};
use ::aws_smithy_types::body::SdkBody;

/// The error code AWS answers an action/version pair it cannot resolve with. Reserved for
/// exactly that: never a well-formed request to an action the service does have.
pub const INVALID_ACTION_CODE: &str = "InvalidAction";

/// The HTTP status that refusal carries.
const UNRESOLVED_PAIR_STATUS: u16 = 400;

/// The media type an `awsQuery` error document is served as.
const XML_MEDIA_TYPE: &str = "text/xml";

/// The sentence AWS states for a pair it could not resolve: fixed text around the
/// request's own `Action` and `Version`.
#[must_use]
pub fn unresolved_pair_message(action: &str, version: &str) -> String {
    format!("Could not find operation {action} for version {version}")
}

/// The whole `400` `InvalidAction` `<ErrorResponse>` for that pair, in the framework's own
/// `awsQuery` error document.
///
/// The `<RequestId>` the envelope carries is NOT added here: a service adds it through the
/// one writer every error of that service passes through, so the id a client correlates on
/// is minted in exactly one place (see the [module docs](self)).
#[must_use]
pub fn unresolved_pair_response(action: &str, version: &str) -> RtResponse {
    let body = ::aws_smithy_schema_server::query_error_body(
        false,
        INVALID_ACTION_CODE,
        &unresolved_pair_message(action, version),
    );
    let mut resp = RtResponse::new(
        StatusCode::try_from(UNRESOLVED_PAIR_STATUS).expect("400 is a valid status"),
        SdkBody::from(body),
    );
    resp.headers_mut().insert("content-type", XML_MEDIA_TYPE);
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The text of the one `<name>…</name>` element of an `awsQuery` error document.
    fn element(body: &str, name: &str) -> Option<String> {
        let open = format!("<{name}>");
        let close = format!("</{name}>");
        let start = body.find(&open)? + open.len();
        let end = body[start..].find(&close)? + start;
        Some(body[start..end].to_string())
    }

    /// RULE: the sentence is fixed text around the request's own `Action` and `Version`,
    /// whatever either of them is.
    #[test]
    fn the_sentence_interpolates_the_requests_own_action_and_version() {
        // The rows differ along what the two values can be: an action of another API, one
        // of no API at all, an empty one, a version of the service and a version of none.
        let rows: &[(&str, &str, &str, &str)] = &[
            (
                "an action no model declares",
                "BogusAction",
                "2011-06-15",
                "Could not find operation BogusAction for version 2011-06-15",
            ),
            (
                "an action of another API, at this one's version",
                "DescribeLoadBalancers",
                "2014-10-31",
                "Could not find operation DescribeLoadBalancers for version 2014-10-31",
            ),
            (
                "an action at a version no model declares",
                "GetCallerIdentity",
                "2099-01-01",
                "Could not find operation GetCallerIdentity for version 2099-01-01",
            ),
            (
                "an empty action",
                "",
                "2011-06-15",
                "Could not find operation  for version 2011-06-15",
            ),
            (
                "an empty version",
                "GetCallerIdentity",
                "",
                "Could not find operation GetCallerIdentity for version ",
            ),
        ];
        for (case, action, version, expected) in rows {
            let actual = unresolved_pair_message(action, version);
            assert_eq!(
                &actual, expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// RULE: the refusal is HTTP 400, code `InvalidAction`, in the `awsQuery` error
    /// envelope — a `Sender` fault, since the caller named the action — and the action and
    /// version it interpolates are written as the document's text rather than as markup, so
    /// a value a caller chose cannot break the envelope.
    #[test]
    fn the_refusal_is_a_four_hundred_invalid_action_envelope() {
        let resp = unresolved_pair_response("BogusAction", "2011-06-15");
        let body = String::from_utf8(
            resp.body()
                .bytes()
                .expect("an in-memory body")
                .to_vec(),
        )
        .expect("a UTF-8 body");
        let status = resp.status().as_u16();
        assert_eq!(
            status, 400,
            "case the refusal's status: expected 400, got {status} for {body}"
        );
        let media_type = resp
            .headers()
            .get("content-type")
            .map(str::to_string);
        assert_eq!(
            media_type.as_deref(),
            Some(XML_MEDIA_TYPE),
            "case the refusal's media type: expected {XML_MEDIA_TYPE:?}, got {media_type:?}"
        );
        let stated = (
            element(&body, "Type"),
            element(&body, "Code"),
            element(&body, "Message"),
        );
        let expected = (
            Some("Sender".to_string()),
            Some(INVALID_ACTION_CODE.to_string()),
            Some("Could not find operation BogusAction for version 2011-06-15".to_string()),
        );
        assert_eq!(
            stated, expected,
            "case the refusal's error triple: expected {expected:?}, got {stated:?} for {body}"
        );

        // A value carrying the characters XML makes markup of reaches the document escaped,
        // so the envelope still carries exactly one `<Code>` and one `<Message>`.
        let hostile = unresolved_pair_response("</Message><Code>Oops</Code>", "2011-06-15&<");
        let body = String::from_utf8(
            hostile
                .body()
                .bytes()
                .expect("an in-memory body")
                .to_vec(),
        )
        .expect("a UTF-8 body");
        let code = element(&body, "Code");
        assert_eq!(
            code.as_deref(),
            Some(INVALID_ACTION_CODE),
            "case a hostile action: expected the one code the envelope states, got {code:?} \
             for {body}"
        );
        let message = element(&body, "Message").unwrap_or_default();
        assert!(
            !message.contains('<')
                && message.split('&').skip(1).all(|after| {
                    ["amp;", "lt;", "gt;", "quot;", "apos;", "#"]
                        .iter()
                        .any(|reference| after.starts_with(reference))
                }),
            "case a hostile action: expected the message to be written as text, got \
             {message:?} for {body}"
        );
    }
}
