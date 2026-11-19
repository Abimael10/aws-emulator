// SPDX-License-Identifier: CC0-1.0

//! WHICH service refused a stage's call, and what it answered: the facts a pipe reports a
//! failed invocation by.
//!
//! A pipe's enrichment and target stages are CALLS to other services, so a refusal one of
//! them comes back with belongs to the called service rather than to the pipe: it is that
//! service's own sentence, answered with that service's status, under that answer's own
//! request id. What this module reads off such an answer is exactly those facts, so the
//! record the pipe writes about the failed invocation ([`super::execution_log`]) attributes
//! it to the service that refused rather than describing it in the pipe's own words.
//!
//! The call is made through the in-process callback into THIS emulator
//! ([`super::Clients`]), so a refusal read here was answered by the emulated service the
//! pipe named. An error that reached NO service — the transport's own answer when this
//! emulator was assembled without that service, or when more than one of its services
//! claims the request — says nothing about the resource, so no refusal is attributed to it;
//! the stage still fails, and the failure travels in the attempt's own detail as before.

use ::aws_smithy_runtime_api::client::orchestrator::HttpResponse;
use ::aws_smithy_runtime_api::client::result::SdkError;
use ::aws_smithy_types::error::metadata::ProvideErrorMetadata;

use super::CallFailure;

/// A service a stage's call is made TO, by the two names a refusal it answered is reported
/// under.
///
/// Both come from the called service's own Smithy model: a pipe does not choose what the
/// service it called is called.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Called {
    /// The namespace the service signs and names its resources under
    /// (`awsService.arnNamespace`) — the one value a client can read the attribution of a
    /// refusal off, and so the name the attribution itself carries.
    namespace: &'static str,
    /// The service as the SDK LAYER that made the call names it: its `awsService.sdkId` in
    /// the casing that SDK's own client type carries (`SFN` -> `Sfn`), which is the name a
    /// relayed clause states and is not always the namespace's own word.
    sdk: &'static str,
}

/// Lambda: `awsService.sdkId` `Lambda`, `arnNamespace` `lambda`.
pub(super) const LAMBDA: Called = Called {
    namespace: super::LAMBDA_NAMESPACE,
    sdk: "Lambda",
};

/// Step Functions: `awsService.sdkId` `SFN`, `arnNamespace` `states`.
pub(super) const STATES: Called = Called {
    namespace: super::STATES_NAMESPACE,
    sdk: "Sfn",
};

/// Amazon SQS: `awsService.sdkId` `SQS`, `arnNamespace` `sqs`.
pub(super) const SQS: Called = Called {
    namespace: super::SQS_NAMESPACE,
    sdk: "Sqs",
};

/// EventBridge: `awsService.sdkId` `EventBridge`, `arnNamespace` `events`.
pub(super) const EVENTS: Called = Called {
    namespace: super::EVENTS_NAMESPACE,
    sdk: "EventBridge",
};

impl Called {
    /// The service as a SENTENCE names it: its namespace's own word, capitalized
    /// (`states` -> `States`).
    fn named(self) -> String {
        let mut letters = self.namespace.chars();
        match letters.next() {
            None => String::new(),
            Some(first) => first.to_uppercase().chain(letters).collect(),
        }
    }
}

/// The codes the TRANSPORT answers when this emulator holds no such service, or when more
/// than one of its services claims the call: the same reading every service that calls
/// another in process makes of these two codes. Neither is an answer ABOUT the resource, so
/// no refusal is attributed to the service that never saw the call.
const TRANSPORT_REFUSALS: [&str; 2] = ["UnknownService", "AmbiguousService"];

/// What a called service ANSWERED about a call it refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Refusal {
    /// The service that refused.
    called: Called,
    /// The resource the refused call named, as the caller named it: the ARN a pipe carries
    /// for the stage that made the call.
    resource: String,
    /// The HTTP status that service answered with.
    status: u16,
    /// The identifier of that ANSWER, read off the answer itself.
    request_id: String,
    /// The sentence the called service itself worded for the refusal, verbatim.
    reason: String,
}

impl Refusal {
    /// The service that refused, as the attribution names it (`awsService`).
    pub(super) fn namespace(&self) -> &'static str {
        self.called.namespace
    }

    /// The service that refused, as a sentence names it.
    pub(super) fn service_named(&self) -> String {
        self.called.named()
    }

    /// The service that refused, as the SDK layer that relayed the call names it.
    pub(super) fn service_sdk(&self) -> &'static str {
        self.called.sdk
    }

    /// The resource the refused call named.
    pub(super) fn resource(&self) -> &str {
        &self.resource
    }

    /// The HTTP status the refusal was answered with.
    pub(super) fn status(&self) -> u16 {
        self.status
    }

    /// The identifier of the answer that refused.
    pub(super) fn request_id(&self) -> &str {
        &self.request_id
    }

    /// The called service's own sentence for the refusal.
    pub(super) fn reason(&self) -> &str {
        &self.reason
    }

    /// The CLASS of the refusal: the HTTP reason phrase of the status it was answered with,
    /// as one word (`400` -> `BadRequest`).
    ///
    /// The status is what a CALLER can classify another service's refusal by without knowing
    /// that service's own error shapes, so the class is read off it. A status with no
    /// registered phrase is reported by the phrase of its own class (`4xx` through `400`,
    /// `5xx` through `500`).
    pub(super) fn exception_type(&self) -> String {
        reason_phrase(self.status)
            .or_else(|| reason_phrase(self.status / 100 * 100))
            .unwrap_or_default()
            .chars()
            .filter(|letter| !letter.is_whitespace())
            .collect()
    }
}

/// The HTTP reason phrase of `status`, or `None` for a status with no registered phrase.
fn reason_phrase(status: u16) -> Option<&'static str> {
    ::http::StatusCode::from_u16(status)
        .ok()
        .and_then(|status| status.canonical_reason())
}

/// A stage's call that did not come back with what the pipe needed: `detail`, this
/// emulator's account of the failure, together with what the called service ANSWERED when
/// the failure was a refusal it worded ([`Refusal`]).
pub(super) fn call_failure<E>(
    detail: String,
    called: Called,
    resource: &str,
    error: &SdkError<E, HttpResponse>,
) -> CallFailure
where
    E: ProvideErrorMetadata,
{
    CallFailure {
        detail,
        function_error: None,
        refusal: refused(called, resource, error).map(Box::new),
    }
}

/// What `error` says the called service answered, or `None` for a failure no service
/// answered.
fn refused<E>(
    called: Called,
    resource: &str,
    error: &SdkError<E, HttpResponse>,
) -> Option<Refusal>
where
    E: ProvideErrorMetadata,
{
    let SdkError::ServiceError(context) = error else {
        return None;
    };
    let answered = context.err();
    let code = answered.code();
    if code.is_some_and(|code| TRANSPORT_REFUSALS.contains(&code)) {
        return None;
    }
    let raw = context.raw();
    Some(Refusal {
        called,
        resource: resource.to_string(),
        status: raw.status().as_u16(),
        // The identifier belongs to the ANSWER, so it is the one that answer carried — read
        // through the one reading of a relayed answer's own request id in this tree.
        request_id: ::service_common::relayed_refusal::answered_request_id(raw.headers()),
        // The called service's own sentence. An answer that worded none is still a refusal
        // of a class, so the class it named stands in for the sentence it did not word.
        reason: answered
            .message()
            .or(code)
            .unwrap_or_default()
            .to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::{Called, Refusal, EVENTS, LAMBDA, SQS, STATES};

    /// A refusal of `status` answered by `called`.
    fn refusal(called: Called, status: u16) -> Refusal {
        Refusal {
            called,
            resource: "arn:aws:states:us-east-1:000000000000:stateMachine:orders".to_string(),
            status,
            request_id: "a1b2c3d4-0000-4000-8000-000000000000".to_string(),
            reason: "This operation is not supported by this type of state machine".to_string(),
        }
    }

    /// RULE: a refusal is attributed to the service that answered it, under that service's
    /// own namespace and the SDK's own name for it — the namespace's word capitalized is how
    /// a sentence names it, and neither is the other.
    ///
    /// The rows are the services a pipe's stages call, which is what the rule generalises
    /// over.
    #[test]
    fn a_refusal_is_attributed_to_the_service_that_answered_it() {
        // (case, the called service, its namespace, its name in a sentence, the SDK's name)
        let rows: [(&str, Called, &str, &str, &str); 4] = [
            ("step functions", STATES, "states", "States", "Sfn"),
            ("lambda", LAMBDA, "lambda", "Lambda", "Lambda"),
            ("amazon sqs", SQS, "sqs", "Sqs", "Sqs"),
            ("eventbridge", EVENTS, "events", "Events", "EventBridge"),
        ];
        for (case, called, namespace, named, sdk) in rows {
            let refused = refusal(called, 400);
            assert_eq!(
                refused.namespace(),
                namespace,
                "case {case:?}: expected the namespace {namespace:?}, got {:?}",
                refused.namespace()
            );
            assert_eq!(
                refused.service_named(),
                named,
                "case {case:?}: expected the sentence to name it {named:?}, got {:?}",
                refused.service_named()
            );
            assert_eq!(
                refused.service_sdk(),
                sdk,
                "case {case:?}: expected the SDK's name {sdk:?}, got {:?}",
                refused.service_sdk()
            );
        }
    }

    /// RULE: the class of a refusal is the HTTP reason phrase of the status it was answered
    /// with, as one word — and a status with no registered phrase is reported by its class's.
    ///
    /// The rows differ along the status, including one past every registered phrase of its
    /// class, which is what the rule generalises over. Source: the reason phrases of the
    /// `http` crate's `StatusCode`.
    #[test]
    fn the_class_of_a_refusal_is_the_phrase_of_its_status() {
        // (case, the status answered, the class the record reports)
        let rows: [(&str, u16, &str); 7] = [
            ("a refused request", 400, "BadRequest"),
            ("a call the role may not make", 403, "Forbidden"),
            ("a resource that is not there", 404, "NotFound"),
            ("a throttled call", 429, "TooManyRequests"),
            ("a fault of the called service", 500, "InternalServerError"),
            ("a service that is not taking calls", 503, "ServiceUnavailable"),
            ("a client status with no phrase of its own", 499, "BadRequest"),
        ];
        for (case, status, expected) in rows {
            let got = refusal(STATES, status).exception_type();
            assert_eq!(
                got, expected,
                "case {case:?}: expected the class {expected:?} for {status}, got {got:?}"
            );
        }
    }
}
