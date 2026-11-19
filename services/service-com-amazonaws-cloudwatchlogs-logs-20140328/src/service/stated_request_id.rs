// SPDX-License-Identifier: CC0-1.0

//! The ONE request id a response names its request by, reported in the header this protocol
//! carries it in AND written into the SDK envelope clause a refusal's message states.
//!
//! Every response of this service names the request it answered, in `x-amzn-RequestId`
//! ([`::service_common::query_error_envelope`], the shared rule). A refusal of this service
//! also STATES that id in text: CloudWatch Logs words a refusal as the sentence for its class
//! followed by ` (Service: AWSLogs; Status Code: <status>; Error Code: <code>; Request ID:
//! <id>; Proxy: null)`, and an SDK surfaces that whole sentence to its caller. The two are one
//! fact about one call — a client that correlates the id it read out of the message with the
//! id the response header reports has to read ONE value — so the message cannot mint an id of
//! its own.
//!
//! So the message is worded with [`crate::state::REQUEST_ID_PLACEHOLDER`] where the id goes
//! ([`crate::state::with_sdk_error_envelope`]), and this module — the layer every response of
//! this service passes through — reports the id and writes it into that clause:
//!
//! * [`report`], applied to every response, both to the one an operation produced
//!   ([`StatedRequestIdOperation`]) and to the one no operation produced (a routing failure, a
//!   transport failure, a pre-dispatch authentication refusal), which
//!   [`crate::service::EmulatedService`] wires through the shared definition wrapper;
//! * the id itself is the one the response is ALREADY named by where a layer named it, and a
//!   freshly minted one only where none did, so however many layers report it a caller never
//!   sees two ids for one call.
//!
//! Only the envelope CLAUSE is substituted — the marker between `Request ID: ` and
//! `; Proxy: null)` — and only in a FAILURE's body, so text a caller stored or a diagnostic
//! quoted out of a submitted query string is left exactly as the handler wrote it.

use ::aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse};
use ::aws_smithy_types::body::SdkBody;
use ::service_common::query_error_envelope::{named_request_as, new_request_id};
use ::smithy_server_runtime::{BoxFuture, ErasedOperation, OperationContext, Service};

/// The field of the SDK envelope clause that states the id, and the text that closes the
/// clause after it — the two fixed parts [`crate::state::with_sdk_error_envelope`] writes the
/// marker between, so a substitution can only ever rewrite that one field.
const REQUEST_ID_FIELD: &str = "Request ID: ";
/// See [`REQUEST_ID_FIELD`].
const CLAUSE_TAIL: &str = "; Proxy: null)";

/// The lowest HTTP status that is a failure, and so the lowest one answered by a body that
/// words a refusal at all.
const FIRST_FAILURE_STATUS: u16 = 400;

/// The envelope clause's id field as a refusal WORDS it, and as it is SENT once the response
/// knows which request it answers.
fn clause(stated: &str) -> String {
    format!("{REQUEST_ID_FIELD}{stated}{CLAUSE_TAIL}")
}

/// `body` with every SDK envelope clause stating the marker stating `request_id` instead, or
/// `None` when it states none — which makes this idempotent and leaves a body that words no
/// envelope untouched.
///
/// The marker is only recognised INSIDE the clause, between the field that opens it and the
/// text that closes it, so nothing else in the body can be rewritten: a value a caller stored,
/// and the symbol a parser diagnostic quotes out of a submitted query string, are left as the
/// handler wrote them even when they carry the marker's own text.
///
/// `pub(crate)` because the AUDIT leg states the same id in the same clause: the message a
/// record of a failed call carries is the one the CALLER was answered with, so the record
/// substitutes it through this same reading rather than a copy of it
/// ([`crate::service::management_events`]).
pub(crate) fn substitute_request_id(body: &str, request_id: &str) -> Option<String> {
    let marker = clause(crate::state::REQUEST_ID_PLACEHOLDER);
    body.contains(&marker)
        .then(|| body.replace(&marker, &clause(request_id)))
}

/// The response as it is sent: ONE request id, reported in the header this protocol names a
/// request in and written into every SDK envelope clause the body states.
///
/// The id is the one the response is already named by, and a freshly minted one only where no
/// layer named it ([`named_request_as`]) — so this is idempotent and two layers of this
/// service can never report two ids for one call. Status, headers and every other byte of the
/// body are left exactly as the renderer wrote them; a SUCCESS is not rewritten at all (no
/// output of this model words an envelope), and neither is a failure that states no clause.
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

/// Serves the dispatch this service wraps and names the request its response answers
/// ([`report`]).
///
/// The one place every response an operation of this service produced passes through — a
/// success, a modelled error, the authorization refusal and a constraint refusal raised while
/// an operation was being served — so no answer can leave without naming its request, and none
/// can state an id in its message that the response does not report.
pub struct StatedRequestIdOperation<O> {
    operation: O,
}

impl<O: ErasedOperation> StatedRequestIdOperation<O> {
    /// Wrap `operation`, reporting the request id on whatever it answers.
    pub fn new(operation: O) -> Self {
        Self { operation }
    }
}

impl<O: ErasedOperation> ErasedOperation for StatedRequestIdOperation<O> {
    fn run<'r>(
        &'r self,
        svc: &'r dyn Service,
        req: &'r RtRequest,
        ctx: &'r OperationContext<'r>,
    ) -> BoxFuture<'r, RtResponse> {
        Box::pin(async move { report(self.operation.run(svc, req, ctx).await) })
    }

    fn auth_schemes(&self) -> Option<&'static [&'static str]> {
        // The wrapped dispatch's own answer: wrapping must not change which schemes sign an
        // operation.
        self.operation.auth_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The id a response reports is written into the clause that states the marker, and the
    /// rest of the document — including text that carries the marker outside a clause — is
    /// left exactly as it was.
    #[test]
    fn the_stated_id_is_written_only_into_the_envelope_clause() {
        let reported = "11112222-3333-4444-5555-666677778888";
        let marker = crate::state::REQUEST_ID_PLACEHOLDER;
        let body = format!(
            "{{\"__type\":\"MalformedQueryException\",\"message\":\"unexpected symbol found \
             {marker} at line 1 and position 18 (Service: AWSLogs; Status Code: 400; Error \
             Code: MalformedQueryException; Request ID: {marker}; Proxy: null)\"}}"
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
            substituted.contains(&format!("unexpected symbol found {marker} at line 1")),
            "case \"a quoted symbol carrying the marker\": expected the diagnostic left \
             alone, got {substituted:?}"
        );
        // Idempotent: a document with no marker left needs nothing.
        assert_eq!(
            substitute_request_id(&substituted, reported),
            None,
            "case \"a substituted document\": expected no second rewrite"
        );
    }

    /// Nothing to substitute: a body that words no envelope clause, and one whose clause
    /// already states an id, are both left alone.
    #[test]
    fn a_body_stating_no_marker_is_left_alone() {
        for case in [
            "{\"__type\":\"ResourceNotFoundException\",\"message\":\"Specified resource does \
             not exist!\"}",
            "{\"__type\":\"InvalidParameterException\",\"message\":\"no (Service: AWSLogs; \
             Status Code: 400; Error Code: InvalidParameterException; Request ID: \
             99998888-7777-6666-5555-444433332222; Proxy: null)\"}",
            "{\"queryId\":\"d2bf0b5d-abcd-4e0a-9f1f-0123456789ab\"}",
        ] {
            assert_eq!(
                substitute_request_id(case, "11112222-3333-4444-5555-666677778888"),
                None,
                "case {case:?}: expected no substitution"
            );
        }
    }
}
