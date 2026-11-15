// SPDX-License-Identifier: CC0-1.0

//! How MediaLive words a request value that falls outside a modelled enum.
//!
//! The framework validates every request against the constraints the model
//! declares and, for a value outside an enum's value set, renders the shared
//! `ValidationException` envelope ("1 validation error detected: Value '…' at
//! '…' failed to satisfy constraint: Member must satisfy enum value set: […]").
//! MediaLive does not model `ValidationException` on any operation
//! (`crate::service::error_envelope::MODELLED_EXCEPTIONS` is the whole set its
//! model declares), so a client matching on the error types MediaLive publishes
//! never matches that refusal and classifies it as an unknown error. MediaLive
//! itself answers such a request with its own `BadRequestException` (400), in
//! wording that belongs to the KIND OF REQUEST rather than to the member:
//!
//! * a request that writes a CHANNEL quotes the request it could not read and
//!   the value it could not read, and
//! * a request that writes an INPUT (or an input security group, which shares
//!   the same request family) states the fixed sentence.
//!
//! The question cannot be answered from the structured refusal the
//! `Service::override_refusal` seam is handed, because that seam sees the
//! violations alone: not which operation was called, and not the request body
//! the channel wording quotes. So the check is made HERE, in the wrapper every
//! routable request of this service is already served through
//! ([`super::error_envelope::ErrorEnvelopeOperation`]), where both are in hand —
//! and it is made with the framework's OWN validator over the framework's OWN
//! deserializer, so what counts as an enum violation is exactly what the model
//! says and every enum member of the request is covered, at any depth, rather
//! than a hand-written list of members.
//!
//! Only an ENUM violation is reworded, and only when it is the FIRST violation
//! the walk reports, so the walk's own priority is unchanged. Every other
//! refusal the framework can raise for these requests — an absent required
//! member, a value outside a declared length or range — keeps the framework's
//! own wording and status, because nothing records how MediaLive states those
//! and this module does not guess.

use aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse};
use aws_smithy_schema_server::constraints::ViolationKind;
use aws_smithy_schema_server::ProtocolError;
use smithy_server_runtime::{ErasedServiceDefinition, Operation, Refused};

use crate::ops::{create_channel, create_input, update_channel, update_channel_class, update_input};

/// The error code every refusal this module words carries: MediaLive's own
/// modelled 400, which every operation below declares
/// (`smithy_query(query="operation", operation="CreateChannel,CreateInput")`).
const BAD_REQUEST: &str = "BadRequestException";

/// The family of request an operation belongs to, which is what decides how
/// MediaLive words an enum refusal for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Family {
    /// Every operation whose request body IS a channel configuration.
    Channel,
    /// Every operation whose request body is an input's configuration.
    Input,
}

impl Family {
    /// How this family words the refusal of `value`, which the request `body`
    /// carried outside a modelled enum.
    ///
    /// The channel family quotes the request itself — what a caller has to look
    /// at to find the value — and then the value. The input family states its
    /// sentence and nothing else.
    fn message(self, body: &[u8], value: Option<&str>) -> String {
        match self {
            Family::Channel => format!(
                "Invalid channel request '{}' Invalid enum value'{}'",
                echoed_request(body),
                value.unwrap_or_default()
            ),
            Family::Input => "Invalid input or ISG request Invalid enum value".to_string(),
        }
    }
}

/// The request `body` as this refusal quotes it: the submitted bytes as text,
/// held to the clamp this tree states for a refusal echo.
///
/// Neither the message nor the allocation behind it may be SIZED BY THE REQUEST,
/// so the echo goes through [`::service_common::validation::quoted_echo`] — the
/// one refusal-echo clamp of this tree, which keeps a prefix on a character
/// boundary and names how many bytes were submitted
/// (`<prefix>... [<n> bytes]`). The whole message is that echo plus its fixed
/// text plus the offending value, which the framework's own walk has already
/// elided, so it stays inside the budget that clamp belongs to.
///
/// A body the codec READ is valid UTF-8, so reading it as text copies nothing
/// and only the clamp allocates. A body that is not is decoded only as far as
/// the echo could carry it, so the replacement character cannot cost three bytes
/// per byte sent either.
fn echoed_request(body: &[u8]) -> String {
    let text: ::std::borrow::Cow<'_, str> = match ::std::str::from_utf8(body) {
        Ok(text) => ::std::borrow::Cow::Borrowed(text),
        Err(_) => {
            let head = &body[..body.len().min(MAX_ECHOED_REQUEST)];
            ::std::borrow::Cow::Owned(String::from_utf8_lossy(head).into_owned())
        }
    };
    ::service_common::validation::quoted_echo(&text).into_owned()
}

/// How far a body that is not valid UTF-8 is decoded before the clamp holds it:
/// the clamp's own ceiling ([`::service_common::validation::quoted_echo`] quotes
/// at most 16 KiB), since nothing past it can reach the message.
const MAX_ECHOED_REQUEST: usize = 16 * 1024;

/// How this service words the refusal of `req`, when the request carries a value
/// outside a modelled enum — and `None` for every request that does not, which
/// is every request the framework then walks as it always did.
///
/// The operations listed are the ones whose request bodies carry enum-valued
/// members at all: the three that write a channel and the two that write an
/// input. The remaining creates of those families (`CreatePartnerInput`,
/// `CreateInputSecurityGroup`, `UpdateInputSecurityGroup`) declare no enum
/// member anywhere in their inputs, so there is nothing of this rule to apply to
/// them.
pub fn enum_refusal(definition: &dyn ErasedServiceDefinition, req: &RtRequest) -> Option<Refused> {
    let operation = definition.protocol().routing_key(req).ok()?;
    let (family, violations) = match operation {
        "CreateChannel" => (
            Family::Channel,
            violations::<create_channel::CreateChannel<NoEngine>>(definition, req),
        ),
        "UpdateChannel" => (
            Family::Channel,
            violations::<update_channel::UpdateChannel<NoEngine>>(definition, req),
        ),
        "UpdateChannelClass" => (
            Family::Channel,
            violations::<update_channel_class::UpdateChannelClass<NoEngine>>(definition, req),
        ),
        "CreateInput" => (
            Family::Input,
            violations::<create_input::CreateInput<NoEngine>>(definition, req),
        ),
        "UpdateInput" => (
            Family::Input,
            violations::<update_input::UpdateInput<NoEngine>>(definition, req),
        ),
        _ => return None,
    };
    // The FIRST violation the walk reports is the one the refusal is about, so
    // this reword keeps the walk's OWN priority: a request whose first problem
    // is something else — an absent required member, a value outside a declared
    // length or range — is left to the framework exactly as before, and only a
    // request the walk refuses FOR its enum value is reworded. MediaLive's
    // wording names one value, so a request with several carries the first.
    let offending = violations.first()?;
    if !matches!(offending.kind, ViolationKind::EnumValue(_)) {
        return None;
    }
    let body = req.body().bytes().unwrap_or_default();
    Some(Refused {
        code: BAD_REQUEST.to_string(),
        message: family.message(body, offending.value.as_deref()),
    })
}

/// The refusal `chosen` as this service's own, rendered in the envelope the
/// protocol `req` is served in: the status and the document shape stay with the
/// framework, and only the code and the message are this service's.
pub fn refusal_response(
    definition: &dyn ErasedServiceDefinition,
    req: &RtRequest,
    chosen: Refused,
) -> RtResponse {
    // The refusal IS a constraint failure, so it travels as one: that is what
    // fixes the 400 the protocol renders it at, exactly as the framework's own
    // walk would have rendered it.
    definition.refusal_response(ProtocolError::Invalid(Vec::new()), Some(chosen), req)
}

/// Every constraint violation the model declares for `O` finds in `req`, as the
/// framework's own walk finds them.
///
/// The walk itself is the tree's shared one
/// ([`::service_common::validation::refusing_violations`]): the protocol's own
/// deserializer and `constraints::validate` over the built input, so this cannot
/// find a different set of violations from the walk that follows it. What is
/// DONE with them is this service's, which is why only that part lives here.
fn violations<O: Operation>(
    definition: &dyn ErasedServiceDefinition,
    req: &RtRequest,
) -> Vec<::aws_smithy_schema_server::constraints::Violation> {
    ::service_common::validation::refusing_violations::<O>(definition.protocol(), req)
}

/// The engine type the walks above name.
///
/// Each operation of this service is generic over its engine, and
/// [`Operation`] is what carries the input type and the input schema the walk
/// reads — both of which are per-OPERATION and not per-engine. The walk
/// constructs no operation and touches no store, so the engine it names is this
/// placeholder rather than the concrete engine the service was mounted on.
type NoEngine = ();
