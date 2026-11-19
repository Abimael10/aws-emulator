// SPDX-License-Identifier: CC0-1.0

//! The ONE request id a response names its request by, reported in the header this protocol
//! carries it in AND written into the service-detail clause a backend's refusal states.
//!
//! Every response AWS sends names the request it answered, in `x-amzn-requestid` for this
//! service's protocol (`service_common::query_error_envelope`, the shared rule). A refusal
//! raised by one of the backends BEHIND Amazon EMR also states that id in text: the Studio,
//! notebook and application-user-interface surfaces word a refusal as the sentence for its
//! class followed by ` (Service: <backend>; Status Code: <status>; Error Code: <code>;
//! Request ID: <id>; Proxy: null)`, and an SDK surfaces that whole sentence to its caller.
//! The two are one fact about one call — a client that correlates the id it read out of the
//! message with the id the response header reports has to read ONE value — so the message
//! cannot mint an id of its own.
//!
//! So such a message is worded with [`crate::state::REQUEST_ID_PLACEHOLDER`] where the id
//! goes, and this module — the layer every response an operation of this service produced
//! passes through — reports the id and writes it into that clause. The id is the one the
//! response is ALREADY named by where a layer named it, and a freshly minted one only where
//! none did, so however many layers report it a caller never sees two ids for one call.
//!
//! Only the CLAUSE's id field is substituted — the marker between `Request ID: ` and
//! `; Proxy: null)` — and only in a FAILURE's body, so text a caller stored or a value a
//! diagnostic quoted out of the request is left exactly as the handler wrote it.

use ::aws_smithy_runtime_api::http::Response as RtResponse;
use ::aws_smithy_types::body::SdkBody;
use ::service_common::query_error_envelope::{named_request_as, new_request_id};

/// The field of the service-detail clause that states the id, and the text that closes the
/// clause after it — the two fixed parts [`crate::state::REQUEST_ID_PLACEHOLDER`] is written
/// between, so a substitution can only ever rewrite that one field.
const REQUEST_ID_FIELD: &str = "Request ID: ";
/// See [`REQUEST_ID_FIELD`].
const CLAUSE_TAIL: &str = "; Proxy: null)";

/// The lowest HTTP status that is a failure, and so the lowest one answered by a body that
/// words a refusal at all.
const FIRST_FAILURE_STATUS: u16 = 400;

/// The clause's id field as a refusal WORDS it, and as it is SENT once the response knows
/// which request it answers.
fn clause(stated: &str) -> String {
    format!("{REQUEST_ID_FIELD}{stated}{CLAUSE_TAIL}")
}

/// `body` with every service-detail clause stating the marker stating `request_id` instead,
/// or `None` when it states none — which makes this idempotent and leaves a body that words
/// no such clause untouched.
///
/// The marker is only recognised INSIDE the clause, between the field that opens it and the
/// text that closes it, so nothing else in the body can be rewritten: a value a caller sent
/// is left as the handler wrote it even when it carries the marker's own text.
fn substitute_request_id(body: &str, request_id: &str) -> Option<String> {
    let marker = clause(crate::state::REQUEST_ID_PLACEHOLDER);
    body.contains(&marker)
        .then(|| body.replace(&marker, &clause(request_id)))
}

/// The response as it is sent: ONE request id, reported in the header this protocol names a
/// request in and written into every service-detail clause the body states.
///
/// Status, headers and every other byte of the body are left exactly as the renderer wrote
/// them; a SUCCESS is not rewritten at all (no output of this model words a clause), and
/// neither is a failure that states none.
pub fn report(resp: RtResponse) -> RtResponse {
    let (mut resp, request_id) = named_request_as(resp, new_request_id);
    if resp.status().as_u16() < FIRST_FAILURE_STATUS {
        return resp;
    }
    let rewritten = {
        // An error body is built in memory by its renderer, so the bytes are readable here;
        // a streamed body words no refusal and is left alone.
        let Some(bytes) = resp.body().bytes() else {
            return resp;
        };
        let Ok(text) = ::std::str::from_utf8(bytes) else {
            return resp;
        };
        match substitute_request_id(text, &request_id) {
            Some(rewritten) => rewritten,
            None => return resp,
        }
    };
    // Only a renderer that advertised a length needs it corrected; the framework's error
    // renderers set `content-type` alone and leave framing to the transport.
    if resp.headers().get("content-length").is_some() {
        let length = rewritten.len().to_string();
        let _ = resp.headers_mut().try_insert("content-length", length);
    }
    *resp.body_mut() = SdkBody::from(rewritten);
    resp
}

#[cfg(test)]
mod tests {
    use super::{clause, substitute_request_id};

    /// The id a response reports is written into the clause that states the marker, and the
    /// rest of the document — including text that carries the marker outside a clause — is
    /// left exactly as it was.
    #[test]
    fn the_stated_id_is_written_only_into_the_service_detail_clause() {
        let reported = "11112222-3333-4444-5555-666677778888";
        let marker = crate::state::REQUEST_ID_PLACEHOLDER;
        let body = format!(
            "{{\"__type\":\"InvalidRequestException\",\"Message\":\"vpc-{marker} is not \
             valid. (Service: AWSEditors; Status Code: 400; Error Code: \
             InvalidRequestException; Request ID: {marker}; Proxy: null)\"}}"
        );
        let substituted =
            substitute_request_id(&body, reported).expect("the clause states the marker");
        assert_eq!(
            substituted,
            body.replace(&clause(marker), &clause(reported)),
            "case \"a refusal stating the clause\": expected the reported id in the clause, \
             got {substituted:?}"
        );
        assert!(
            substituted.contains(&format!("vpc-{marker} is not valid.")),
            "case \"a submitted value carrying the marker\": expected the sentence left \
             alone, got {substituted:?}"
        );
        // Idempotent: a document with no marker left needs nothing.
        assert_eq!(
            substitute_request_id(&substituted, reported),
            None,
            "case \"a substituted document\": expected no second rewrite"
        );
    }

    /// Nothing to substitute: a body that words no clause, and one whose clause already
    /// states an id, are both left alone.
    #[test]
    fn a_body_stating_no_marker_is_left_alone() {
        for case in [
            "{\"__type\":\"InvalidRequestException\",\"Message\":\"Cluster id 'j-1' is not \
             valid.\"}",
            "{\"__type\":\"InvalidRequestException\",\"Message\":\"Studio does not exist. \
             (Service: AWSEditors; Status Code: 400; Error Code: InvalidRequestException; \
             Request ID: 99998888-7777-6666-5555-444433332222; Proxy: null)\"}",
            "{\"JobFlowId\":\"j-0000000000001\"}",
        ] {
            assert_eq!(
                substitute_request_id(case, "11112222-3333-4444-5555-666677778888"),
                None,
                "case {case:?}: expected no substitution"
            );
        }
    }
}
