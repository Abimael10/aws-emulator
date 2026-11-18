// SPDX-License-Identifier: CC0-1.0

//! How this service answers a request the framework could not READ at all.
//!
//! The awsQuery front end tells two layers of failure apart. A body whose VALUES do not
//! satisfy the rules the model declares — an absent required member, a value outside a
//! declared bound or outside an enum's declared set — is a request that was read and then
//! refused. A body that could not be read at all — a member whose text is not a value of
//! its wire type — is the other layer, and the framework's query renderer names it with a
//! code of its own (`MalformedInput`) and reports the reader's diagnostic as the message.
//!
//! Amazon EC2 Auto Scaling does not answer a caller that way. None of its operations models
//! an input-validation fault, so EVERY problem with a request's arguments — including a
//! value its front door could not read — is reported through the awsQuery common client
//! error, `ValidationError` ([`crate::service::VALIDATION_ERROR_CODE`]): both layers are one
//! bad-argument refusal to a client, which branches on that one code. The reader's
//! diagnostic is not sent with it: it is the parser's own report, naming the text it could
//! not read and the literals that parser accepts — recorded behaviour for this class is an
//! envelope carrying the code and no `<Message>` at all, so no sentence is invented from the
//! reader's words.
//!
//! This module is the one place that answer is completed. It acts on the CODE the renderer
//! already wrote, which is what tells the two layers apart after the fact: every refusal of
//! a value this service READ carries its own code
//! ([`crate::service::EmulatedService::override_refusal`]), so a response still carrying
//! the framework's malformed-input code is one of the unreadable ones.
//!
//! Nothing else about the response is touched: the status, the `<Type>` and the whole
//! success path stay with the framework renderer that owns them.

use aws_smithy_runtime_api::http::Response as RtResponse;
use aws_smithy_types::body::SdkBody;

/// The error code element the framework's query renderer writes for a body it could not
/// read.
const MALFORMED_INPUT_CODE: &str = "<Code>MalformedInput</Code>";

/// The element this module removes from such a response.
const MESSAGE_OPEN: &str = "<Message>";
/// Its close.
const MESSAGE_CLOSE: &str = "</Message>";

/// The code element this service answers it with instead: its one client-error code, named
/// once in [`crate::service`] and spelled into the envelope's element here.
fn validation_error_code() -> String {
    format!("<Code>{}</Code>", crate::service::VALIDATION_ERROR_CODE)
}

/// The document as this service states the refusal: the framework's malformed-input code
/// replaced by this service's one client-error code, and the reader's `<Message>` element
/// removed.
///
/// `None` when the document needs nothing: it does not carry the malformed-input code, so
/// it is not one of the unreadable ones — which makes this idempotent, since a document
/// this rewrites no longer carries that code.
fn restated_body(body: &str) -> Option<String> {
    let code = body.find(MALFORMED_INPUT_CODE)?;
    let mut restated = format!(
        "{}{}{}",
        &body[..code],
        validation_error_code(),
        &body[code + MALFORMED_INPUT_CODE.len()..]
    );
    if let Some(open) = restated.find(MESSAGE_OPEN) {
        if let Some(close) = restated[open..].find(MESSAGE_CLOSE) {
            let end = open + close + MESSAGE_CLOSE.len();
            restated = format!("{}{}", &restated[..open], &restated[end..]);
        }
    }
    Some(restated)
}

/// The response as this service answers an unreadable request: the same envelope and the
/// same status, under this service's one client-error code and with no `<Message>`.
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

    /// A malformed-input envelope is restated under this service's code, loses its message
    /// and keeps everything else — whether or not the renderer wrote a message at all.
    #[test]
    fn an_unreadable_request_is_answered_as_an_argument_refusal() {
        for case in [
            "<?xml version=\"1.0\"?><ErrorResponse xmlns=\"ns\"><Error><Type>Sender</Type>\
             <Code>MalformedInput</Code><Message>invalid integer `abc`</Message></Error>\
             </ErrorResponse>",
            "<ErrorResponse><Error><Type>Sender</Type><Code>MalformedInput</Code></Error>\
             </ErrorResponse>",
        ] {
            let restated = restated_body(case).expect("rewritten");
            assert!(
                !restated.contains(MESSAGE_OPEN),
                "case {case:?}: expected no message element, got {restated:?}"
            );
            assert!(
                restated.contains(&validation_error_code())
                    && !restated.contains(MALFORMED_INPUT_CODE),
                "case {case:?}: expected this service's code, got {restated:?}"
            );
            assert!(
                restated.contains("<Type>Sender</Type>")
                    && restated.ends_with("</Error></ErrorResponse>"),
                "case {case:?}: expected the rest of the envelope kept, got {restated:?}"
            );
            // Idempotent: the restated document carries no malformed-input code, so there
            // is nothing left to do to it.
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
