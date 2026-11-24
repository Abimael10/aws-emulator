// SPDX-License-Identifier: CC0-1.0

//! The parameter COMBINATIONS this service refuses, decided before any member's value is
//! read.
//!
//! Some filters an operation of this API publishes are only served TOGETHER with another
//! parameter: the request names a narrowing the service cannot answer on its own, so it is
//! refused as the incompatible set of parameters it is —
//! `InvalidParameterCombinationException`, "two or more incompatible parameters were
//! specified", which renders as HTTP 400 `InvalidParameterCombination`
//! (`smithy_query(query="operation", operation="DescribeUpdateActions")`).
//!
//! That refusal comes FIRST: the combination is a fact about which parameters the request
//! carries, not about what they hold, so the service answers it whatever the values are —
//! including a value outside a modelled enum, which the framework's validating walk would
//! otherwise refuse before any handler ran (see [`super::enum_value`]). A handler cannot
//! decide it for that reason: by the time one runs, a request carrying a bad value has
//! already been refused. So it is decided here, at the same wrapper the authorization gate
//! is asked at ([`super::query_error_envelope::RequestIdEnvelopeOperation`]), from the form
//! parameters the request carries — and because it is asked ahead of the operation, it is
//! asked once rather than repeated in each handler whose operation publishes such a filter.
//!
//! Only the code and the sentence are this module's business: the HTTP status and the
//! envelope are the framework's, exactly as for every other pre-dispatch refusal.

use ::std::borrow::Cow;

use ::aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse};
use ::aws_smithy_schema_server::ProtocolError;
use ::smithy_server_runtime::{Refused, Service};

// The envelope this refusal renders in, declared once beside the wrapper that owns this
// service's error envelope (`super::query_error_envelope`), so the two gates asked ahead of
// every operation cannot drift about it.
use super::query_error_envelope::QUERY_ENVELOPE;

/// The awsQuery error code `InvalidParameterCombinationException` renders as.
///
/// Spelled here because this refusal is answered BEFORE an operation is dispatched, where no
/// modelled error shape is in hand: a handler returning that exception never names the code at
/// all — the framework renders it from the model's own `awsQueryError` trait on the shape
/// (`smithy_query(query="operation", operation="DescribeUpdateActions")`: `code
/// InvalidParameterCombination, httpResponseCode 400`), which is the string below.
const INVALID_PARAMETER_COMBINATION: &str = "InvalidParameterCombination";

/// One filter an operation serves only together with another parameter.
///
/// A named record rather than a tuple, so each part says what it is where it is read.
struct ServedTogether {
    /// The operation this rule belongs to, as its own shape name — which is the routing key
    /// an awsQuery `Action` resolves to.
    operation: &'static str,
    /// The member whose presence needs the companion below.
    member: &'static str,
    /// The member it is only served together with.
    together_with: &'static str,
    /// The sentence the service refuses the pair with: wholly fixed text, naming the two
    /// parameters in its own prose rather than by their wire names.
    message: &'static str,
}

/// Every such rule this service states.
///
/// `DescribeUpdateActions$UpdateActionStatus` is "the status of the update action": an update
/// action belongs to one service update, so the status of one is only a narrowing of the
/// actions of a NAMED update, and the service refuses the filter sent without
/// `ServiceUpdateName`.
const SERVED_TOGETHER: &[ServedTogether] = &[ServedTogether {
    operation: "DescribeUpdateActions",
    member: "UpdateActionStatus",
    together_with: "ServiceUpdateName",
    message: "Update action status needs to be provided together with service update name.",
}];

/// This service's refusal for a request that carries one of [`SERVED_TOGETHER`]'s members
/// without the parameter it is only served with, and `None` for every other request.
///
/// The operation asked about is taken from the request's routing key and only when the
/// routing table SERVES that key, so it is always a modelled operation's name: a request
/// naming no operation of this service still reaches the routing failure rather than a
/// decision about a name nothing serves.
#[must_use]
pub fn refusal_response(svc: &dyn Service, req: &RtRequest) -> Option<RtResponse> {
    let operation = svc.definition().protocol().routing_key(req).ok()?;
    svc.definition().routing_table().get(operation)?;
    let mut rules = SERVED_TOGETHER
        .iter()
        .filter(|rule| rule.operation == operation)
        .peekable();
    rules.peek()?;

    let parameters: Vec<(Cow<'_, str>, Cow<'_, str>)> =
        super::authorization::request_parameters(req).collect();
    let broken = rules.find(|rule| {
        supplies(&parameters, rule.member) && !supplies(&parameters, rule.together_with)
    })?;
    Some(::smithy_server_runtime::protocol_error_response_with(
        QUERY_ENVELOPE,
        // The class of failure: a request the declared rules refuse. The status (400) and the
        // envelope come from it; the code and the sentence are this service's own, below.
        ProtocolError::Invalid(Vec::new()),
        Some(Refused {
            code: INVALID_PARAMETER_COMBINATION.to_string(),
            message: broken.message.to_string(),
        }),
    ))
}

/// Whether the request SUPPLIES `member` at all.
///
/// A scalar arrives under the member's own name and a list under
/// `<member>.member.<position>`, so a parameter whose name is the member or steps INTO it
/// counts; an empty value supplies nothing, which is how an awsQuery client sends a member it
/// is not filtering on.
fn supplies(parameters: &[(Cow<'_, str>, Cow<'_, str>)], member: &str) -> bool {
    parameters.iter().any(|(name, value)| {
        !value.is_empty()
            && (name == member
                || name
                    .strip_prefix(member)
                    .is_some_and(|rest| rest.starts_with('.')))
    })
}
