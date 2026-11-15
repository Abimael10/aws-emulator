// SPDX-License-Identifier: CC0-1.0

//! The ENVELOPE a refusal of this service leaves under, rendered in one place: the status
//! the refusal carries, the error-type header that names its shape, and the `message` body.
//!
//! # The header set
//!
//! This service is `restJson1`, and on that protocol a client resolves a failure's error
//! SHAPE from the error-type header (`x-amzn-errortype`), naming the code the refusal
//! carries. The body names the same shape in its `__type` discriminator, so a client that
//! reads the header and one that reads the body agree about what was refused.
//!
//! How many TIMES that header is carried is part of the wire too, and this service does not
//! answer every refusal with the same multiplicity. The protocol's error serializer writes
//! it once, which is what a modelled error, a constraint violation and the tagging surface's
//! own code leave with. The FAIL-CLOSED refusal — the one this service answers where the
//! caller's ownership of the resource it named cannot be established
//! ([`crate::scaffolded::UNAUTHORIZED_EXCEPTION_CODE`]) — is carried REPEATED on this
//! service's wire, so a client that joins repeated header values reads the shape name twice
//! over, and middleware that de-duplicates or splits the header is exercised. That is a
//! property of the REFUSAL and not of the surface that decided it, so it is applied to every
//! response this service answers, whether the refusal was decided ahead of dispatch by one
//! of the gates or returned by a handler's own error channel
//! ([`with_fail_closed_header_set`]).
//!
//! # Why the rendering is here rather than in each gate
//!
//! The refusals this service answers above its handlers are decided in two places — the IAM
//! gate and the instance gate ([`super::AuthorizationGate`] and `super::InstanceGate`) — and
//! both have to answer the SAME envelope a handler's own error channel produces for the same
//! value ([`crate::scaffolded::Scaffolded::unauthorized`] and its siblings). Rendering it
//! once, through the service's own protocol, is what keeps the two from drifting apart.

use smithy_server_runtime::{Refused, Service};

/// Render `refusal` in this service's own protocol envelope, with the code, the sentence and
/// the STATUS the refusal carries.
///
/// The status is why this does not go through `ErasedServiceDefinition::refusal_response`:
/// that renderer states a pre-dispatch refusal, whose status the protocol fixes (400 for an
/// invalid request), and the fail-closed refusal is reported with its own
/// ([`crate::scaffolded::UNAUTHORIZED_EXCEPTION_STATUS`]). Serializing the refusal itself
/// through the protocol is also what makes this answer identical to the one a handler's
/// error channel produces for the same value: the same `__type`, the same `x-amzn-errortype`
/// header set and the same `message` body member.
///
/// A refusal that will not serialize still has to refuse: it falls back to the pre-dispatch
/// renderer, which carries the code and the sentence in this service's envelope even though
/// it cannot carry the status.
pub(super) fn refusal_response(
    svc: &dyn Service,
    req: &aws_smithy_runtime_api::http::Request,
    refusal: &crate::scaffolded::ServiceRefusal,
) -> aws_smithy_runtime_api::http::Response {
    match svc.definition().protocol().serialize_error(
        refusal,
        &crate::scaffolded::SERVICE_REFUSAL_SCHEMA,
        refusal.code,
        refusal.status,
    ) {
        Ok(response) => with_fail_closed_header_set(response),
        Err(error) => {
            tracing::warn!(
                code = refusal.code,
                %error,
                "the fail-closed refusal would not serialize; it is answered through the \
                 pre-dispatch renderer instead"
            );
            svc.definition().refusal_response(
                aws_smithy_schema_server::ProtocolError::Invalid(Vec::new()),
                Some(Refused {
                    code: refusal.code.to_string(),
                    message: refusal.message.clone(),
                }),
                req,
            )
        }
    }
}

/// The header a `restJson1` client resolves a failure's error SHAPE from.
const ERROR_TYPE_HEADER: &str = "x-amzn-errortype";

/// How many occurrences of [`ERROR_TYPE_HEADER`] the fail-closed refusal leaves with: the
/// multiplicity this service's wire carries it at, as against the one occurrence the
/// protocol's error serializer writes for every other refusal class.
const FAIL_CLOSED_ERROR_TYPE_OCCURRENCES: usize = 2;

/// Give `response` the error-type header set the refusal it carries leaves under, and
/// answer it.
///
/// A response that does not name the fail-closed code in that header — a served request, a
/// modelled error, a constraint violation, the tagging surface's own refusal — is answered
/// unchanged, so this is the whole difference between the two multiplicities and not a
/// rewrite of the envelope.
///
/// Idempotent, and applied at the one exit every response of this service leaves through
/// (`super`'s authorization gate), so a refusal rendered here and one a handler's error
/// channel rendered end up with the same set however many layers they passed.
///
/// The append is FALLIBLE for the same reason the serializer's own insert is: a header value
/// rejects bytes a `&str` accepts, and a panic on the refusal path would become a 500 an SDK
/// retries. A rejected value leaves the set as it was — the status, the code and the
/// sentence still refuse the request.
pub(super) fn with_fail_closed_header_set(
    mut response: aws_smithy_runtime_api::http::Response,
) -> aws_smithy_runtime_api::http::Response {
    let carried = response
        .headers()
        .get_all(ERROR_TYPE_HEADER)
        .filter(|value| *value == crate::scaffolded::UNAUTHORIZED_EXCEPTION_CODE)
        .count();
    if carried == 0 {
        return response;
    }
    for _ in carried..FAIL_CLOSED_ERROR_TYPE_OCCURRENCES {
        if let Err(error) = response.headers_mut().try_append(
            ERROR_TYPE_HEADER,
            crate::scaffolded::UNAUTHORIZED_EXCEPTION_CODE,
        ) {
            tracing::warn!(
                %error,
                "the fail-closed refusal's error-type header would not be carried again; it \
                 is answered with the set it already has"
            );
            break;
        }
    }
    response
}
