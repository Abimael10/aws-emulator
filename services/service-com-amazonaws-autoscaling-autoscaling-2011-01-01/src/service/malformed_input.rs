// SPDX-License-Identifier: CC0-1.0

//! How this service answers a request the framework could not READ at all.
//!
//! The awsQuery front end tells two layers of failure apart. A body whose VALUES do not
//! satisfy the rules the model declares — an absent required member, a value outside a
//! declared bound or outside an enum's declared set — is a request that was read and then
//! refused, and Amazon EC2 Auto Scaling answers it `ValidationError` with the sentence
//! naming what was wrong ([`crate::service::EmulatedService::override_refusal`]). A body
//! that could not be read at all — a member whose text is not a value of its wire type —
//! is a different answer: the error code says the input was malformed and NO message is
//! sent, because there is no read value to say anything about.
//!
//! This module is the one place that second answer is completed. It acts on the CODE the
//! renderer already wrote, which is what tells the two layers apart after the fact: every
//! refusal of a value this service READ carries its own code, so a response still carrying
//! the framework's malformed-input code is one of the unreadable ones.
//!
//! Nothing else about the response is touched: the status, the `<Type>`, the `<Code>` and
//! the whole success path stay with the framework renderer that owns them.

use aws_smithy_runtime_api::http::Response as RtResponse;
use aws_smithy_types::body::SdkBody;

/// The error code the framework's query renderer writes for a body it could not read.
const MALFORMED_INPUT_CODE: &str = "<Code>MalformedInput</Code>";

/// The element this module removes from such a response.
const MESSAGE_OPEN: &str = "<Message>";
/// Its close.
const MESSAGE_CLOSE: &str = "</Message>";

/// The document with its `<Message>` element removed, or `None` when it needs nothing: it
/// does not carry the malformed-input code, or it carries no message element.
fn without_message_element(body: &str) -> Option<String> {
    if !body.contains(MALFORMED_INPUT_CODE) {
        return None;
    }
    let open = body.find(MESSAGE_OPEN)?;
    let close = body[open..].find(MESSAGE_CLOSE)? + open + MESSAGE_CLOSE.len();
    Some(format!("{}{}", &body[..open], &body[close..]))
}

/// The response as this service answers an unreadable request: the same envelope, the same
/// status and the same code, with no `<Message>`.
///
/// A response that is not one of those — every success, and every refusal of a value the
/// service read — is returned untouched, so no other path can be affected.
pub fn without_message(mut resp: RtResponse) -> RtResponse {
    let rewritten = {
        let Some(bytes) = resp.body().bytes() else {
            return resp;
        };
        let Ok(text) = std::str::from_utf8(bytes) else {
            return resp;
        };
        match without_message_element(text) {
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

    /// A malformed-input envelope loses its message and keeps everything else.
    #[test]
    fn an_unreadable_request_is_answered_without_a_message() {
        let body = "<?xml version=\"1.0\"?><ErrorResponse xmlns=\"ns\"><Error><Type>Sender</Type>\
                    <Code>MalformedInput</Code><Message>invalid integer `abc`</Message></Error>\
                    </ErrorResponse>";
        let rewritten = without_message_element(body).expect("rewritten");
        assert!(
            !rewritten.contains(MESSAGE_OPEN),
            "expected no message element, got {rewritten:?}"
        );
        assert!(
            rewritten.contains(MALFORMED_INPUT_CODE) && rewritten.contains("<Type>Sender</Type>"),
            "expected the rest of the envelope kept, got {rewritten:?}"
        );
    }

    /// Nothing else is touched: a refusal of a value the service read, and a success.
    #[test]
    fn every_other_response_is_left_alone() {
        for case in [
            "<ErrorResponse><Error><Type>Sender</Type><Code>ValidationError</Code>\
             <Message>Grace period must be a positive integer.</Message></Error></ErrorResponse>",
            "<DescribeAutoScalingGroupsResponse><DescribeAutoScalingGroupsResult/>\
             </DescribeAutoScalingGroupsResponse>",
            "<ErrorResponse><Error><Type>Sender</Type><Code>MalformedInput</Code></Error>\
             </ErrorResponse>",
        ] {
            assert!(
                without_message_element(case).is_none(),
                "case {case:?}: expected no rewrite"
            );
        }
    }
}
