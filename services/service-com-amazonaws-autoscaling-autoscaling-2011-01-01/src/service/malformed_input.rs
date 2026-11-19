// SPDX-License-Identifier: CC0-1.0

//! How this service answers a request value its front door could not READ.
//!
//! The awsQuery front end tells two layers of failure apart. A body whose VALUES do not
//! satisfy the rules the model declares — an absent required member, a value outside a
//! declared bound or outside an enum's declared set — is a request that was read and then
//! refused, and this service states every one of those under its one argument-refusal code
//! ([`crate::service::EmulatedService::override_refusal`]). A member whose TEXT is no value
//! of its wire type at all is the other layer, and the framework's query renderer names it
//! with a code of its own (`MalformedInput`), reporting the reader's diagnostic as the
//! message.
//!
//! Amazon EC2 Auto Scaling answers that second layer in two ways, and WHICH value could not
//! be read is what decides:
//!
//! * a BOOLEAN member's text outside the lexical space the Query wire carries a boolean in is
//!   refused by the boolean reading every awsQuery front door shares
//!   ([`crate::service::xsd_boolean`]), and this service states that refusal under its one
//!   argument-refusal code ([`crate::service::VALIDATION_ERROR_CODE`]) — recorded behaviour
//!   for a request supplying a non-boolean for a modelled boolean member is that code, which
//!   is also the only client-error code this API's callers branch on, since none of its
//!   operations models an input-validation fault;
//! * every OTHER member's text that is no value of its type — an integer, a double, a
//!   timestamp — keeps the front door's own malformed-input code, which is what a recorded
//!   refusal of such a request carries. A request the front door could not read is a
//!   different class to a client (a serialization bug on its side) from one that was read and
//!   refused, and the two are not merged into one code.
//!
//! Neither class carries the reader's diagnostic: that sentence is the parser's own report,
//! naming the text it could not read and the literals that parser accepts, and recorded
//! behaviour for both is an envelope carrying the code and no `<Message>` at all — so no
//! sentence is invented from the reader's words.
//!
//! This module is the one place those answers are completed. It acts on what the renderer
//! already wrote, which is what tells the layers apart after the fact: every refusal of a
//! value this service READ carries its own code, so a response still carrying the framework's
//! malformed-input code is one of the unreadable ones, and the message the renderer put on it
//! is the reading that refused it.
//!
//! Nothing else about the response is touched: the status, the `<Type>` and the whole success
//! path stay with the framework renderer that owns them.

use aws_smithy_runtime_api::http::Response as RtResponse;
use aws_smithy_types::body::SdkBody;
use service_common::query_boolean_values::NON_BOOLEAN_VALUE_MESSAGE;

/// The error code element the framework's query renderer writes for a body it could not
/// read.
const MALFORMED_INPUT_CODE: &str = "<Code>MalformedInput</Code>";

/// The element this module removes from such a response.
const MESSAGE_OPEN: &str = "<Message>";
/// Its close.
const MESSAGE_CLOSE: &str = "</Message>";

/// The code element this service answers an unreadable BOOLEAN with: its one argument-refusal
/// code, named once in [`crate::service`] and spelled into the envelope's element here.
fn validation_error_code() -> String {
    format!("<Code>{}</Code>", crate::service::VALIDATION_ERROR_CODE)
}

/// The message element the shared boolean reading states its refusal with
/// ([`NON_BOOLEAN_VALUE_MESSAGE`]) — the one marker that says the unreadable value was a
/// boolean's text rather than any other type's.
fn boolean_refusal_message() -> String {
    format!("{MESSAGE_OPEN}{NON_BOOLEAN_VALUE_MESSAGE}{MESSAGE_CLOSE}")
}

/// `document` with its first `<Message>…</Message>` element removed, or the document
/// unchanged when it carries none.
fn without_message(document: &str) -> String {
    let Some(open) = document.find(MESSAGE_OPEN) else {
        return document.to_string();
    };
    let Some(close) = document[open..].find(MESSAGE_CLOSE) else {
        return document.to_string();
    };
    let end = open + close + MESSAGE_CLOSE.len();
    format!("{}{}", &document[..open], &document[end..])
}

/// The document as this service states the refusal of a value its front door could not read:
/// an unreadable boolean's under this service's one argument-refusal code, every other one
/// under the code the front door already wrote, and neither carrying the reader's diagnostic.
///
/// `None` when the document needs nothing: it does not carry the malformed-input code, so it
/// is not one of the unreadable ones, or it already states the refusal that way — which makes
/// this idempotent.
fn restated_body(body: &str) -> Option<String> {
    let code = body.find(MALFORMED_INPUT_CODE)?;
    let (before, after) = (&body[..code], &body[code + MALFORMED_INPUT_CODE.len()..]);
    // Which reading refused the value decides the code: the boolean one states its refusal
    // with the protocol's own sentence, and that class is the one this service answers under
    // its argument-refusal code.
    let code = if after.contains(&boolean_refusal_message()) {
        validation_error_code()
    } else {
        MALFORMED_INPUT_CODE.to_string()
    };
    let restated = format!("{before}{code}{}", without_message(after));
    (restated != body).then_some(restated)
}

/// The response as this service answers a request value it could not read: the same envelope
/// and the same status, under the code that class carries and with no `<Message>`.
///
/// A response that is not one of those — every success, and every refusal of a value the
/// service read — is returned untouched, so no other path can be affected.
pub fn restated(mut resp: RtResponse) -> RtResponse {
    let rewritten = {
        let Some(bytes) = resp.body().bytes() else {
            return resp;
        };
        let Ok(text) = std::str::from_utf8(bytes) else {
            return resp;
        };
        match restated_body(text) {
            Some(rewritten) => rewritten,
            None => return resp,
        }
    };
    // Only a renderer that advertised a length needs it corrected.
    if resp.headers().get("content-length").is_some() {
        let length = rewritten.len().to_string();
        let _ = resp.headers_mut().try_insert("content-length", length);
    }
    *resp.body_mut() = SdkBody::from(rewritten);
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An unreadable BOOLEAN is restated under this service's argument-refusal code, loses the
    /// reader's sentence and keeps everything else.
    #[test]
    fn an_unreadable_boolean_is_answered_as_an_argument_refusal() {
        let case = format!(
            "<?xml version=\"1.0\"?><ErrorResponse xmlns=\"ns\"><Error><Type>Sender</Type>\
             <Code>MalformedInput</Code><Message>{NON_BOOLEAN_VALUE_MESSAGE}</Message></Error>\
             </ErrorResponse>"
        );
        let restated = restated_body(&case).expect("rewritten");
        assert!(
            !restated.contains(MESSAGE_OPEN),
            "case {case:?}: expected no message element, got {restated:?}"
        );
        assert!(
            restated.contains(&validation_error_code()) && !restated.contains(MALFORMED_INPUT_CODE),
            "case {case:?}: expected this service's argument-refusal code, got {restated:?}"
        );
        assert!(
            restated.contains("<Type>Sender</Type>")
                && restated.ends_with("</Error></ErrorResponse>"),
            "case {case:?}: expected the rest of the envelope kept, got {restated:?}"
        );
        // Idempotent: the restated document carries no malformed-input code, so there is
        // nothing left to do to it.
        assert!(
            restated_body(&restated).is_none(),
            "case {case:?}: expected no second rewrite of {restated:?}"
        );
    }

    /// Every OTHER unreadable value keeps the front door's own code, and loses only the
    /// reader's diagnostic — whatever that diagnostic said.
    #[test]
    fn every_other_unreadable_value_keeps_the_malformed_input_code() {
        for case in [
            "<?xml version=\"1.0\"?><ErrorResponse xmlns=\"ns\"><Error><Type>Sender</Type>\
             <Code>MalformedInput</Code><Message>invalid integer `abc`</Message></Error>\
             </ErrorResponse>",
            "<ErrorResponse><Error><Type>Sender</Type><Code>MalformedInput</Code>\
             <Message>expected a timestamp</Message></Error></ErrorResponse>",
        ] {
            let restated = restated_body(case).expect("rewritten");
            assert!(
                !restated.contains(MESSAGE_OPEN),
                "case {case:?}: expected no message element, got {restated:?}"
            );
            assert!(
                restated.contains(MALFORMED_INPUT_CODE)
                    && !restated.contains(&validation_error_code()),
                "case {case:?}: expected the front door's own code kept, got {restated:?}"
            );
            assert!(
                restated.contains("<Type>Sender</Type>")
                    && restated.ends_with("</Error></ErrorResponse>"),
                "case {case:?}: expected the rest of the envelope kept, got {restated:?}"
            );
            // Idempotent: the restated document carries no message left to remove.
            assert!(
                restated_body(&restated).is_none(),
                "case {case:?}: expected no second rewrite of {restated:?}"
            );
        }
    }

    /// Nothing else is touched: a refusal of a value the service read, and a success.
    #[test]
    fn every_other_response_is_left_alone() {
        for case in [
            "<ErrorResponse><Error><Type>Sender</Type><Code>ValidationError</Code>\
             <Message>Grace period must be a positive integer.</Message></Error></ErrorResponse>",
            "<ErrorResponse><Error><Type>Sender</Type><Code>InvalidNextToken</Code>\
             <Message>NextToken: not-a-token</Message></Error></ErrorResponse>",
            "<DescribeAutoScalingGroupsResponse><DescribeAutoScalingGroupsResult/>\
             </DescribeAutoScalingGroupsResponse>",
        ] {
            assert!(
                restated_body(case).is_none(),
                "case {case:?}: expected no rewrite"
            );
        }
    }
}
