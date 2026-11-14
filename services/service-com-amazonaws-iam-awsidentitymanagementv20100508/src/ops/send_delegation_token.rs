// SPDX-License-Identifier: CC0-1.0

//! @op SendDelegationToken
//!
//! ## What this operation answers on the wire
//!
//! A mutation: it resolves the `DelegationRequestId` the request carries against the
//! delegation requests the account holds and, for one it holds in the accepted state, records
//! that the exchange token has been sent — "after the SendDelegationToken API call is
//! successful, the request transitions to a FINALIZED state and cannot be rolled back" — so
//! the read that follows reports that state and the last-updated instant this call wrote. The
//! output shape is `Unit`, so what the call did is observable only through
//! `GetDelegationRequest` and `ListDelegationRequests`.
//!
//! "The delegation request must be in the ACCEPTED state when calling this API": a request in
//! any other state has no accepted delegation to send a token for, so the call is refused
//! with the modelled `InvalidInputException` (HTTP 400, awsQuery code `InvalidInput`) and the
//! request stays in the state the account holds it in — which is what keeps a request that was
//! rejected before the token was sent from being finalized afterwards.
//!
//! An identifier the account holds NO delegation request under references "a resource entity
//! that does not exist", so the call is refused with the modelled `NoSuchEntityException`
//! (HTTP 404, awsQuery code `NoSuchEntity`) whose message describes the resource asked about
//! ([`crate::state::delegation_request_write_not_found_message`]) and nothing is written for
//! it.
//!
//! The token itself is not carried anywhere by this call: it "is sent to the partner via an
//! asynchronous notification channel, established by the partner", which is outside this
//! account's state, so what the account records is the transition.
//!
//! ---
//!
//! # Modeled contract for `SendDelegationToken`
//!
//! What AWS documents for this operation, from this service's Smithy model. HTML is
//! stripped and long entries are clipped at ` …`. For the FULL text of any one item,
//! ask the model rather than reading the raw JSON:
//!
//! ```text
//! smithy_query(query="doc",shape="<Shape>",member="<Member>",model_path="services/service-com-amazonaws-iam-awsidentitymanagementv20100508/smithy-model/com-amazonaws-iam-awsidentitymanagementv20100508.json")
//! ```
//!
//! Sends the exchange token for an accepted delegation request. The exchange token is sent
//! to the partner via an asynchronous notification channel, established by the partner. The
//! delegation request must be in the ACCEPTED state when calling this API. After the
//! SendDelegationToken API call is successful, the request transitions to a FINALIZED state
//! and cannot be rolled back. However, a user may reject an accepted request before the
//! SendDelegationToken API is called. For more details, see Managing Permissions for
//! Delegation Requests.
//!
//! ## Input
//!
//! * `DelegationRequestId` (delegationRequestIdType, len 16..128, pattern ^[\w-]+$)
//!   **required** — The unique identifier of the delegation request for which to send the
//!   token.
//!
//! ## Errors
//!
//! * `ConcurrentModificationException` (HTTP 409) — The request was rejected because multiple
//!   requests to change this object were submitted simultaneously. Wait a few minutes and
//!   submit your request again.
//! * `InvalidInputException` (HTTP 400) — The request was rejected because an invalid or
//!   out-of-range value was supplied for an input parameter.
//! * `NoSuchEntityException` (HTTP 404) — The request was rejected because it referenced a
//!   resource entity that does not exist. The error message describes the resource.
//! * `ServiceFailureException` (HTTP 500) — The request processing has failed because of an
//!   unknown error, exception or failure.

#[allow(unused_imports)]
use super::types::*;

/// The `stateType` value a delegation request has to be in for its exchange token to be
/// sent: `ACCEPTED`.
///
/// "The delegation request must be in the ACCEPTED state when calling this API"
/// (`SendDelegationToken`), one of the seven values `stateType` declares
/// ([`super::types::STATE_TYPE_VALUES`]) and the one `AcceptDelegationRequest` writes.
const STATE_ACCEPTED: &str = "ACCEPTED";

/// The `stateType` value a request whose token has been sent reaches: "after the
/// SendDelegationToken API call is successful, the request transitions to a FINALIZED state
/// and cannot be rolled back" (`SendDelegationToken`). See [`STATE_ACCEPTED`].
const STATE_FINALIZED: &str = "FINALIZED";

/// How this operation refuses a delegation request that is not in the accepted state: the
/// condition the operation's own documentation states, as fixed text — what it reports is
/// which state the API requires, not which state the request happens to be in.
const NOT_ACCEPTED: &str =
    "The delegation request must be in the ACCEPTED state when calling this API.";

// `#[operation_error]` below expands to an invocation of the LOCAL declarative
// `smithy!` macro, which this line is what emits into the module. Without it the
// expansion fails with `cannot find macro 'smithy' in this scope` and the
// `SendDelegationTokenError` enum is never produced.
smithy_macros::smithy_model!("smithy-model/com-amazonaws-iam-awsidentitymanagementv20100508.json");

#[derive(Debug, Clone, Default)]
pub struct SendDelegationTokenOutput;

impl SendDelegationTokenOutput {
    // `with_http` carries this operation's modelled response status. Remove it and
    // the status falls back to the protocol default, so a modelled 204 answers 200.
    pub const SCHEMA: ::aws_smithy_schema::Schema = ::aws_smithy_schema::Schema::new_struct(
        ::aws_smithy_schema::ShapeId::from_static("smithy.api#Unit", "smithy.api", "Unit"),
        ::aws_smithy_schema::ShapeType::Structure,
        &[],
    );
}

pub static SEND_DELEGATION_TOKEN_OUTPUT_SCHEMA: &::aws_smithy_schema::Schema = &SendDelegationTokenOutput::SCHEMA;

impl ::aws_smithy_schema::serde::SerializableStruct for SendDelegationTokenOutput {
    fn serialize_members(&self, _s: &mut dyn ::aws_smithy_schema::serde::ShapeSerializer) -> Result<(), ::aws_smithy_schema::serde::SerdeError> { Ok(()) }
}

// The operation's MODELED errors. `#[operation_error]` accepts only variants the
// model declares for this operation, so the scaffold's `NotImplemented` cannot live
// here — hence the `SendDelegationTokenError` alias below. An operation that models no errors
// generates an UNINHABITED enum here, which is correct and expected: there is nothing
// to construct until the model says otherwise.
#[smithy_macros::operation_error("com.amazonaws.iam#SendDelegationToken")]
#[derive(Debug)]
pub enum SendDelegationTokenErrorModeled {
    ConcurrentModificationException(ConcurrentModificationException),
    InvalidInputException(InvalidInputException),
    NoSuchEntityException(NoSuchEntityException),
    ServiceFailureException(ServiceFailureException),
}

/// This operation's error channel, as the generated `impl Operation` names it
/// (`crate::ops::send_delegation_token::SendDelegationTokenError`). Wrapping the modeled enum is what
/// lets a scaffolded operation answer 501 for one whose model declares no errors at all.
pub type SendDelegationTokenError = crate::scaffolded::Scaffolded<SendDelegationTokenErrorModeled>;

/// Lets `?` and `.into()` lift a modeled failure into the error channel, so an
/// implemented handler writes `Err(SomeModeledError.into())` rather than naming the
/// wrapper variant. See `crate::scaffolded` for how to drop the wrapper entirely once
/// this operation is implemented.
impl From<SendDelegationTokenErrorModeled> for SendDelegationTokenError {
    fn from(modeled: SendDelegationTokenErrorModeled) -> Self {
        crate::scaffolded::Scaffolded::Modeled(modeled)
    }
}

/// The `SendDelegationToken` operation.
///
/// Owns the shared engine (the constructor seam) and implements
/// [`smithy_server_runtime::Operation`]. Generic over the engine type `Eng`, so this
/// crate names no concrete engine type: a migrated service that needs state picks a
/// concrete engine where it mounts the service, not here.
pub struct SendDelegationToken<Eng> {
    engine: ::std::sync::Arc<Eng>,
}

impl<Eng> SendDelegationToken<Eng> {
    /// Construct the operation from the shared engine. INFALLIBLE — the engine is a
    /// construction-time dependency, so engine access is guaranteed by the time any
    /// request runs. Called by the generated `EmulatedService::new(engine)` constructor.
    pub fn new(engine: ::std::sync::Arc<Eng>) -> Self {
        Self { engine }
    }
}

impl<Eng: ::std::marker::Send + ::std::marker::Sync + 'static> smithy_server_runtime::Operation
    for SendDelegationToken<Eng>
{
    type Input = SendDelegationTokenInput;
    type Output = SendDelegationTokenOutput;
    type Error = SendDelegationTokenError;

    const NAME: &'static str = "SendDelegationToken";
    const INPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::SEND_DELEGATION_TOKEN_INPUT_SCHEMA;
    const OUTPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = SEND_DELEGATION_TOKEN_OUTPUT_SCHEMA;
    const SUCCESS_STATUS: u16 = 200u16;

    async fn handle(
        &self,
        input: Self::Input,
        ctx: &smithy_server_runtime::OperationContext<'_>,
    ) -> Result<Self::Output, Self::Error> {
        let Some(state) = crate::state::service_state(&*self.engine) else {
            return Err(SendDelegationTokenErrorModeled::ServiceFailureException(
                ServiceFailureException {
                    message: Some("the IAM store is unavailable".to_string()),
                },
            )
            .into());
        };

        // A mutation: the request "transitions to a FINALIZED state", so the lock is held
        // for write across the lookup, the state check and the write — a request cannot be
        // rejected between being found accepted and being finalized.
        let mut store = state.lock();

        // The acting principal the audit records below are attributed to.
        let caller_akid = ::engine::extract_akid(&ctx.parts().headers);

        // IAM decides a call against the caller's policies before it touches the resource,
        // so the decision is made first and on the ARN the identifier names — the same ARN
        // whether or not the account holds a request under it, which is what keeps a
        // refused caller from learning which identifiers the account holds. A refusal names
        // the request as the request named it.
        crate::state::authorization::authorize_named(
            &store,
            ctx,
            crate::state::authorization::SEND_DELEGATION_TOKEN,
            &crate::state::delegation_request_arn(&input.delegation_request_id),
            &format!("delegation request {}", input.delegation_request_id),
        )?;

        // The identifier is the handle `CreateDelegationRequest` filed the request under,
        // held verbatim, so it is looked up verbatim — exactly as `GetDelegationRequest`
        // resolves it. An identifier the account holds nothing under is refused, and the
        // refusal writes nothing.
        let Some(held) = store
            .delegation_requests
            .get_mut(&input.delegation_request_id)
        else {
            ::tracing::info!(
                audit = true,
                operation = "SendDelegationToken",
                outcome = "refused_no_such_delegation_request",
                caller_akid = %caller_akid,
                delegation_request_id = %input.delegation_request_id,
                "no exchange token was sent"
            );
            return Err(
                SendDelegationTokenErrorModeled::NoSuchEntityException(NoSuchEntityException {
                    message: Some(crate::state::delegation_request_write_not_found_message(
                        &input.delegation_request_id,
                    )),
                })
                .into(),
            );
        };

        // "The delegation request must be in the ACCEPTED state when calling this API":
        // every other state — a request nobody has accepted, one already finalized, one
        // rejected or expired — has no accepted delegation to send a token for, so the
        // value supplied for the identifier is one this call cannot act on and the request
        // stays as the account holds it.
        if held.state.as_deref() != Some(STATE_ACCEPTED) {
            ::tracing::info!(
                audit = true,
                operation = "SendDelegationToken",
                outcome = "refused_state",
                caller_akid = %caller_akid,
                delegation_request_id = %input.delegation_request_id,
                state = %held.state.as_deref().unwrap_or_default(),
                "no exchange token was sent"
            );
            return Err(
                SendDelegationTokenErrorModeled::InvalidInputException(InvalidInputException {
                    message: Some(NOT_ACCEPTED.to_string()),
                })
                .into(),
            );
        }

        // "After the SendDelegationToken API call is successful, the request transitions to
        // a FINALIZED state and cannot be rolled back" — which is exactly what the next read
        // of the request reports, and what refuses a second send of the same request.
        held.state = Some(STATE_FINALIZED.to_string());
        // "Last updated timestamp of the request" (`DelegationRequest$UpdatedTime`): the
        // instant this call wrote the record.
        held.updated_time = Some(crate::state::now());

        ::tracing::info!(
            audit = true,
            operation = "SendDelegationToken",
            outcome = "sent",
            caller_akid = %caller_akid,
            delegation_request_id = %input.delegation_request_id,
            state = %STATE_FINALIZED,
            "the exchange token of the delegation request was sent"
        );

        // The operation's output shape is `Unit`: what was written is reported by
        // `GetDelegationRequest`, which reads the record this call just wrote.
        Ok(SendDelegationTokenOutput)
    }
}
