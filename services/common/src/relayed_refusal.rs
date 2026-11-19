// SPDX-License-Identifier: CC0-1.0

//! How a service reports a refusal ANOTHER service answered.
//!
//! An operation whose input names a resource of a plane this service does not own
//! resolves that reference by CALLING the service that owns it, so a refusal it comes
//! back with belongs to the called service rather than to the one the client addressed.
//! The calling service does not re-word it: the downstream reason travels verbatim
//! inside the envelope the SDK layer that relayed the call states around a failed call,
//! so the caller can see WHICH service refused, with what HTTP status and under which
//! request id — detail the calling service's own modelled exception carries nowhere else.
//!
//! The envelope is four facts about the relayed call and nothing more: the service it was
//! made to, the status that service answered it with, that call's own request id, and how
//! many attempts it took ([`relay_envelope`]). The service name and the status come from
//! the service that actually answered, because a message pairing one service's name with
//! another's status misattributes the failure; the request id belongs to the relayed CALL,
//! so it is minted here, per call, rather than derived from anything in the request.
//!
//! This is the workspace's ONE rendering of that envelope, so a client parsing the service,
//! the status or the request id back out of a message reads one form whichever emulated
//! service relayed the call.
//!
//! AWS states that envelope in TWO renderings, because two generations of SDK render a failed
//! call differently, and which one a message carries is the called service's own answer
//! rather than anything the relaying service chooses: [`relay_envelope`] is the
//! comma-separated one that counts attempts, and [`caught_exception_message`] the
//! semicolon-separated one that names the exception CLASS the SDK caught and the proxy it
//! came through. Both live here so a client parses one form per rendering whichever service
//! relayed the call, and so does the reading of the relayed answer's own request id
//! ([`answered_request_id`]). The copies that predate them — `state::kms_reference` in
//! `service-com-amazonaws-ebs-ebs`, `state::encryption` in
//! `service-com-amazonaws-dynamodb-dynamodb-20120810`, `state::ecs_services` in
//! `service-com-amazonaws-codedeploy-codedeploy-20141006` and the request-id readings in
//! `service-com-amazonaws-secretsmanager-secretsmanager`'s `state::envelope` and its siblings
//! — are to be MIGRATED onto them, each in the change that owns its service; nothing new
//! should build either envelope, or read that id, by hand.

use crate::query_error_envelope::new_request_id;

/// The relayed call a refusal is reported from: the service the call was made to, the
/// HTTP status that service answered it with, and how many attempts reaching it took.
///
/// The three travel TOGETHER because they are what a client parses back out of a relayed
/// message — who refused, with what status, and whether anything was retried — and a
/// message that mixes two calls' facts attributes the failure to the wrong service.
#[derive(Debug, Clone, Copy)]
pub struct RelayedCall<'a> {
    /// The called service's name, as the SDK layer that relayed the call reports it,
    /// which is neither its endpoint prefix nor the casing of its `sdkId`.
    pub service: &'a str,
    /// The HTTP status that service answered the relayed call with.
    pub status: u16,
    /// How many attempts the relayed call took.
    pub attempts: u32,
}

impl<'a> RelayedCall<'a> {
    /// A relayed call that was made exactly ONCE.
    ///
    /// A refusal of the caller's own input is not retried — there is nothing about it a
    /// second attempt could change — so the call that came back refused was the first and
    /// only one. A caller that does retry states its own count on [`RelayedCall`].
    #[must_use]
    pub fn once(service: &'a str, status: u16) -> Self {
        Self {
            service,
            status,
            attempts: 1,
        }
    }
}

/// The envelope a relayed refusal is reported inside, including the space that separates it
/// from the reason it follows — so a caller that carries a downstream reason through
/// APPENDS this to it.
///
/// The request id belongs to the relayed CALL, so one is minted here per call. A caller that
/// already HOLDS the answer's own identifier — because it read it off that answer, and
/// reports it beside this message — states it through [`relay_envelope_of`] instead, so the
/// id in the message and the id in the field are the one identifier of the one call.
#[must_use]
pub fn relay_envelope(call: RelayedCall<'_>) -> String {
    relay_envelope_of(call, &new_request_id())
}

/// [`relay_envelope`] for a caller that holds the relayed answer's own request id
/// ([`answered_request_id`]) rather than minting one for it.
#[must_use]
pub fn relay_envelope_of(call: RelayedCall<'_>, request_id: &str) -> String {
    let RelayedCall {
        service,
        status,
        attempts,
    } = call;
    format!(
        " (Service: {service}, Status Code: {status}, Request ID: {request_id}) \
         (SDK Attempt Count: {attempts})"
    )
}

/// `reason`, the message the called service itself gave, as the calling service reports it
/// to its caller: that reason verbatim, then the envelope naming the call it came from
/// ([`relay_envelope`]).
#[must_use]
pub fn relayed_refusal_message(reason: &str, call: RelayedCall<'_>) -> String {
    format!("{reason}{}", relay_envelope(call))
}

/// The relayed call a CAUGHT service exception is reported from: the service that answered,
/// the HTTP status the refusal's class carries, the error code it answered under — which is
/// also the simple name of the exception class the SDK caught — and the identifier of that
/// answer.
///
/// The four travel TOGETHER for the same reason [`RelayedCall`]'s three do: they are what a
/// client parses back out of the message, and a message mixing two calls' facts attributes
/// the failure to the wrong call. `request_id` belongs to the ANSWER, so a caller passes the
/// one that answer carried (or, where the answer carried none, one it minted for it) rather
/// than anything derived from the request.
#[derive(Debug, Clone, Copy)]
pub struct CaughtCall<'a> {
    /// The called service's name, as the SDK layer that relayed the call reports it.
    pub service: &'a str,
    /// The HTTP status the refusal's class is answered with.
    pub status: u16,
    /// The error code the called service answered under.
    pub code: &'a str,
    /// The identifier of the answer being relayed.
    pub request_id: &'a str,
}

/// How this rendering states a field the relayed answer did not carry — the proxy above all,
/// there being no proxy in front of an in-process call.
const ABSENT_FIELD: &str = "null";

/// The headers an answer's own request identifier is read from, in the order they are read:
/// the two spellings AWS reports it under.
const REQUEST_ID_HEADERS: [&str; 2] = ["x-amzn-requestid", "x-amz-request-id"];

/// The identifier of the ANSWER a refusal is relayed from: the one that answer's headers
/// carried, or a fresh one of the same form when it carried none.
///
/// A request identifier belongs to the ANSWER, so the relay reports the one the relayed answer
/// carried; an answer that carries none is still the answer to one call, so the envelope states
/// an identifier of that form for it rather than leaving out a field it states. Never derived
/// from the request.
///
/// `headers` is the relayed response's own header map, as the SDK's `raw_response` reports it.
#[must_use]
pub fn answered_request_id(headers: &::aws_smithy_runtime_api::http::Headers) -> String {
    REQUEST_ID_HEADERS
        .into_iter()
        .find_map(|header| headers.get(header))
        .map_or_else(new_request_id, str::to_string)
}

/// A caught service exception as the calling service reports it: its own fixed `opening`, the
/// caught exception's fully qualified class (`class_package` and the answered code), the
/// sentence the called service worded, and the clause naming the call it came from.
///
/// `opening` is the relaying service's own introduction, WITHOUT its colon, and `class_package`
/// the package the called service's exception classes live in; either may be empty, for a
/// service that states no opening of its own or relays an answer that names no class. Nothing
/// here is worded by the relaying service beyond its opening: `reason` is the called service's
/// own sentence and every other part comes from that answer.
#[must_use]
pub fn caught_exception_message(
    opening: &str,
    class_package: &str,
    reason: &str,
    call: CaughtCall<'_>,
) -> String {
    let CaughtCall {
        service,
        status,
        code,
        request_id,
    } = call;
    let clause = format!(
        "(Service: {service}; Status Code: {status}; Error Code: {code}; Request ID: \
         {request_id}; Proxy: {ABSENT_FIELD})"
    );
    let mut message = String::new();
    if !opening.is_empty() {
        message.push_str(opening);
        message.push_str(": ");
    }
    if !class_package.is_empty() {
        message.push_str(class_package);
        message.push('.');
        message.push_str(code);
        message.push_str(": ");
    }
    if !reason.is_empty() {
        message.push_str(reason);
        message.push(' ');
    }
    message.push_str(&clause);
    message
}

#[cfg(test)]
mod tests {
    use super::{
        caught_exception_message, relay_envelope, relay_envelope_of, relayed_refusal_message,
        CaughtCall, RelayedCall,
    };
    use crate::query_error_envelope::is_request_id;

    /// The envelope states the relayed call's own facts, and the reason it follows is
    /// carried through untouched.
    #[test]
    fn a_relayed_refusal_states_the_reason_then_the_call_it_came_from() {
        let rows = [
            ("Knowledge Base does not exist.", "CloverKnowledge", 404u16, 1u32),
            ("key does not exist", "Kms", 400, 1),
            ("certificate authority not found", "EksCertificateAuthority", 404, 3),
        ];
        for (reason, service, status, attempts) in rows {
            let call = RelayedCall {
                service,
                status,
                attempts,
            };
            let message = relayed_refusal_message(reason, call);
            let head = format!("{reason} (Service: {service}, Status Code: {status}, Request ID: ");
            let tail = format!(") (SDK Attempt Count: {attempts})");
            assert!(
                message.starts_with(&head),
                "case {reason:?}: expected the message to start {head:?}, got {message:?}"
            );
            assert!(
                message.ends_with(&tail),
                "case {reason:?}: expected the message to end {tail:?}, got {message:?}"
            );
            let request_id = message
                .strip_prefix(&head)
                .and_then(|rest| rest.strip_suffix(&tail))
                .unwrap_or_default();
            assert!(
                is_request_id(request_id),
                "case {reason:?}: expected a request id, got {request_id:?}"
            );
        }
    }

    /// The request id belongs to the relayed CALL, so two refusals never report one id.
    #[test]
    fn each_relayed_call_reports_its_own_request_id() {
        let call = RelayedCall::once("CloverKnowledge", 404);
        let first = relay_envelope(call);
        let second = relay_envelope(call);
        assert_ne!(
            first, second,
            "case \"two relayed calls\": expected each call its own request id, got {first:?} \
             twice"
        );
        assert_eq!(
            call.attempts, 1,
            "case \"a call made once\": expected one attempt, got {}",
            call.attempts
        );
    }

    /// A caller that HOLDS the answered id states that one, so the id in a message and the
    /// id in the field beside it are the one identifier of the one call.
    #[test]
    fn an_answered_request_id_is_the_one_the_envelope_states() {
        // (case, the service, the status, the identifier the answer carried)
        let rows = [
            ("a step functions refusal", "Sfn", 400u16, "a1b2c3d4-0000-4000-8000-000000000000"),
            ("a queue refusal", "Sqs", 403, "9f8e7d6c-1111-4111-8111-111111111111"),
        ];
        for (case, service, status, request_id) in rows {
            let envelope = relay_envelope_of(RelayedCall::once(service, status), request_id);
            let expected = format!(
                " (Service: {service}, Status Code: {status}, Request ID: {request_id}) \
                 (SDK Attempt Count: 1)"
            );
            assert_eq!(
                envelope, expected,
                "case {case:?}: expected {expected:?}, got {envelope:?}"
            );
        }
    }

    /// The other rendering states the opening, the caught class, the called service's own
    /// sentence and the clause — and leaves out the part a row does not carry rather than
    /// rendering it empty.
    #[test]
    fn a_caught_exception_states_its_class_and_the_call_it_came_from() {
        // (case, opening, class package, the called service's sentence, the expected message)
        let rows = [
            (
                "an opening, a class and a sentence",
                "KMS Exception",
                "com.amazonaws.services.kms.model",
                "Invalid arn eu-west-1",
                "KMS Exception: com.amazonaws.services.kms.model.NotFoundException: Invalid arn \
                 eu-west-1 (Service: AWSKMS; Status Code: 400; Error Code: NotFoundException; \
                 Request ID: r-1; Proxy: null)",
            ),
            (
                "no opening of the relaying service's own",
                "",
                "com.amazonaws.services.kms.model",
                "Invalid arn eu-west-1",
                "com.amazonaws.services.kms.model.NotFoundException: Invalid arn eu-west-1 \
                 (Service: AWSKMS; Status Code: 400; Error Code: NotFoundException; Request ID: \
                 r-1; Proxy: null)",
            ),
            (
                "an answer that names no class and no sentence",
                "KMS Exception",
                "",
                "",
                "KMS Exception: (Service: AWSKMS; Status Code: 400; Error Code: \
                 NotFoundException; Request ID: r-1; Proxy: null)",
            ),
        ];
        for (case, opening, package, reason, expected) in rows {
            let call = CaughtCall {
                service: "AWSKMS",
                status: 400,
                code: "NotFoundException",
                request_id: "r-1",
            };
            let message = caught_exception_message(opening, package, reason, call);
            assert_eq!(
                message, expected,
                "case {case:?}: expected {expected:?}, got {message:?}"
            );
        }
    }
}
