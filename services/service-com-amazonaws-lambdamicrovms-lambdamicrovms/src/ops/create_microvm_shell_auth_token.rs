// SPDX-License-Identifier: CC0-1.0

//! @op CreateMicrovmShellAuthToken
//!
//! Mints the token a client presents to reach the interactive SHELL of one MicroVM, for
//! the MicroVM the `microvmIdentifier` names. The token's digest, what it grants and its
//! expiry are recorded, so the MicroVM endpoint this emulator serves
//! (`crate::state::microvm_endpoint`) accepts exactly the shell tokens this operation
//! minted and performs the client's program in that MicroVM's own machine
//! (`crate::state::microvm_shell`).
//!
//! ## Reaching this service's state
//!
//! The engine is held on `self` (constructor-injected), NOT passed per request — so
//! engine access is guaranteed by the time any request runs. Reach the store through
//! [`crate::state::service_state`] and read and write it through the guard it hands back:
//!
//! ```ignore
//!     let state = crate::state::service_state(&*self.engine)
//!         .ok_or(/* a modelled server error */)?;
//!     state.lock().your_field = /* ... */;
//! ```
//!
//! `ctx` ([`smithy_server_runtime::OperationContext`]) carries request metadata only —
//! headers, URI, method, and anything middleware placed in `ctx.parts().extensions`.
//! The engine is NOT reached through `ctx`.
//!
//! Shared types live in `super::types`; the error channel in `crate::scaffolded`.
//!
//! ---
//!
//! # Modeled contract for `CreateMicrovmShellAuthToken`
//!
//! What AWS documents for this operation, from this service's Smithy model. HTML is
//! stripped and long entries are clipped at ` …`. For the FULL text of any one item,
//! ask the model rather than reading the raw JSON:
//!
//! ```text
//! smithy_query(query="doc",shape="<Shape>",member="<Member>",model_path="services/service-com-amazonaws-lambdamicrovms-lambdamicrovms/smithy-model/com-amazonaws-lambdamicrovms-lambdamicrovms.json")
//! ```
//!
//! Creates a shell authentication token for interactive shell access to a running MicroVM.
//! The MicroVM must have been run with the SHELL_INGRESS network connector attached.
//!
//! `POST /2025-09-09/microvms/{microvmIdentifier}/shell-auth-token`, success code 200.
//!
//! ## Input
//!
//! * `expirationInMinutes` (PositiveInteger, range 1..) **required** — The duration in
//!   minutes before the shell authentication token expires.
//! * `microvmIdentifier` (MicrovmIdentifier, len 1..256, `@httpLabel`) **required** — The ID
//!   of the MicroVM to create a shell authentication token for.
//!
//! ## Output
//!
//! * `authToken` (TokenParts, len 1..10) **required** — The generated shell authentication
//!   token key-value pairs for accessing the MicroVM.
//!
//! ## Errors
//!
//! * `AccessDeniedException` (HTTP 403) — You do not have sufficient access to perform this
//!   action.
//! * `ConflictException` (HTTP 409) — The request could not be completed due to a conflict
//!   with the current state of the resource.
//! * `InternalServerException` (HTTP 500) — An internal server error occurred. Retry the
//!   request later.
//! * `ResourceNotFoundException` (HTTP 404) — The specified resource does not exist.
//! * `ThrottlingException` (HTTP 429) — The request was denied due to request throttling.
//!   Retry the request later.
//! * `ValidationException` (HTTP 400) — The input does not satisfy the constraints specified
//!   by the service.

#[allow(unused_imports)]
use super::types::*;

/// The `authToken` key a client presents the token under.
///
/// A shell token is a token of the MicroVM's published ENDPOINT — the one address a
/// MicroVM is reached on — so it is presented in the header that endpoint reads a token
/// from (`crate::state::microvm_endpoint::AUTH_TOKEN_HEADER`), the same key
/// `CreateMicrovmAuthToken` returns its own token under.
const AUTH_TOKEN_KEY: &str = crate::state::microvm_endpoint::AUTH_TOKEN_HEADER;

// `#[operation_error]` below expands to an invocation of the LOCAL declarative
// `smithy!` macro, which this line is what emits into the module. Without it the
// expansion fails with `cannot find macro 'smithy' in this scope` and the
// `CreateMicrovmShellAuthTokenError` enum is never produced.
smithy_macros::smithy_model!("smithy-model/com-amazonaws-lambdamicrovms-lambdamicrovms.json");

// The operation's MODELED errors. `#[operation_error]` accepts only variants the
// model declares for this operation, so the scaffold's `NotImplemented` cannot live
// here — hence the `CreateMicrovmShellAuthTokenError` alias below. An operation that models no errors
// generates an UNINHABITED enum here, which is correct and expected: there is nothing
// to construct until the model says otherwise.
#[smithy_macros::operation_error("com.amazonaws.lambdamicrovms#CreateMicrovmShellAuthToken")]
#[derive(Debug)]
pub enum CreateMicrovmShellAuthTokenErrorModeled {
    AccessDeniedException(AccessDeniedException),
    ConflictException(ConflictException),
    InternalServerException(InternalServerException),
    ResourceNotFoundException(ResourceNotFoundException),
    ThrottlingException(ThrottlingException),
    ValidationException(ValidationException),
}

/// This operation's error channel, as the generated `impl Operation` names it
/// (`crate::ops::create_microvm_shell_auth_token::CreateMicrovmShellAuthTokenError`). Wrapping the modeled enum is what
/// lets a scaffolded operation answer 501 for one whose model declares no errors at all.
pub type CreateMicrovmShellAuthTokenError = crate::scaffolded::Scaffolded<CreateMicrovmShellAuthTokenErrorModeled>;

/// Lets `?` and `.into()` lift a modeled failure into the error channel, so an
/// implemented handler writes `Err(SomeModeledError.into())` rather than naming the
/// wrapper variant. See `crate::scaffolded` for how to drop the wrapper entirely once
/// this operation is implemented.
impl From<CreateMicrovmShellAuthTokenErrorModeled> for CreateMicrovmShellAuthTokenError {
    fn from(modeled: CreateMicrovmShellAuthTokenErrorModeled) -> Self {
        crate::scaffolded::Scaffolded::Modeled(modeled)
    }
}

/// The `CreateMicrovmShellAuthToken` operation.
///
/// Owns the shared engine (the constructor seam) and implements
/// [`smithy_server_runtime::Operation`]. Generic over the engine type `Eng`, so this
/// crate names no concrete engine type: a migrated service that needs state picks a
/// concrete engine where it mounts the service, not here.
pub struct CreateMicrovmShellAuthToken<Eng> {
    engine: ::std::sync::Arc<Eng>,
}

impl<Eng> CreateMicrovmShellAuthToken<Eng> {
    /// Construct the operation from the shared engine. INFALLIBLE — the engine is a
    /// construction-time dependency, so engine access is guaranteed by the time any
    /// request runs. Called by the generated `EmulatedService::new(engine)` constructor.
    pub fn new(engine: ::std::sync::Arc<Eng>) -> Self {
        Self { engine }
    }
}

impl<Eng: ::std::marker::Send + ::std::marker::Sync + 'static> smithy_server_runtime::Operation
    for CreateMicrovmShellAuthToken<Eng>
{
    type Input = CreateMicrovmShellAuthTokenInput;
    type Output = CreateMicrovmShellAuthTokenOutput;
    type Error = CreateMicrovmShellAuthTokenError;

    const NAME: &'static str = "CreateMicrovmShellAuthToken";
    const INPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::CREATE_MICROVM_SHELL_AUTH_TOKEN_INPUT_SCHEMA;
    const OUTPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::CREATE_MICROVM_SHELL_AUTH_TOKEN_OUTPUT_SCHEMA;
    const SUCCESS_STATUS: u16 = 200u16;

    async fn handle(
        &self,
        input: Self::Input,
        ctx: &smithy_server_runtime::OperationContext<'_>,
    ) -> Result<Self::Output, Self::Error> {
        let Some(state) = crate::state::service_state(&*self.engine) else {
            return Err(unavailable("the MicroVM store is unavailable"));
        };

        let region = crate::state::request_region(ctx);
        let now = ::std::time::Instant::now();

        let mut store = state.lock();
        store.materialize_due(now);

        let Some(vm) = store.microvm(&region, &input.microvm_identifier) else {
            // The not-found envelope this service puts on the wire: a request-derived
            // `message`, with the optional `resourceType`/`resourceId` unset.
            return Err(
                CreateMicrovmShellAuthTokenErrorModeled::ResourceNotFoundException(
                    ResourceNotFoundException {
                        message: crate::state::microvm_not_found_message(
                            &input.microvm_identifier,
                        ),
                        resource_type: None,
                        resource_id: None,
                    },
                )
                .into(),
            );
        };

        // `expirationInMinutes` is modelled with no maximum, so the service's own
        // ceiling is what refuses an over-long lifetime, exactly as it does for
        // `CreateMicrovmAuthToken` — the two mints of this API share one cap — and AFTER
        // the lookup above, because whether the MicroVM exists at all is decided before
        // anything about the token it was asked for.
        if input.expiration_in_minutes > crate::state::MAX_AUTH_TOKEN_EXPIRATION_MINUTES {
            return Err(
                CreateMicrovmShellAuthTokenErrorModeled::ValidationException(
                    ValidationException {
                        message: Some(crate::state::auth_token_expiration_exceeded_message(
                            input.expiration_in_minutes,
                        )),
                    },
                )
                .into(),
            );
        }

        // "a shell authentication token for interactive shell access to a running
        // MicroVM": a MicroVM on its way out, or already gone, accepts no further
        // connection, so there is no shell for a token to reach — the modelled conflict
        // with the resource's current state, as `CreateMicrovmAuthToken` answers for the
        // same condition.
        if !crate::state::is_connectable_microvm_state(&vm.state) {
            let message = format!(
                "MicroVM {} is {} and accepts no connection",
                vm.microvm_id, vm.state
            );
            return Err(CreateMicrovmShellAuthTokenErrorModeled::ConflictException(
                ConflictException {
                    message: Some(message),
                    resource_id: Some(vm.microvm_id.clone()),
                    resource_type: Some("Microvm".to_string()),
                },
            )
            .into());
        }

        // "The MicroVM must have been run with the SHELL_INGRESS network connector
        // attached." The connectors the MicroVM is attached to are the ones RunMicrovm
        // persisted for it, so the requirement is read off the record rather than off this
        // request: a MicroVM reached through any other ingress connector has no shell to
        // authenticate to, which the input does not satisfy. What the refusal SAYS is the
        // service's own sentence for the condition
        // (`crate::state::no_shell_ingress_message`), not the documentation above.
        if !crate::state::attaches_shell_ingress(&vm.ingress_network_connectors) {
            return Err(
                CreateMicrovmShellAuthTokenErrorModeled::ValidationException(
                    ValidationException {
                        message: Some(crate::state::no_shell_ingress_message()),
                    },
                )
                .into(),
            );
        }
        // Nothing of the stored record is needed past this point, and the token is
        // minted outside the lock.
        let microvm_id = vm.microvm_id.clone();
        drop(store);

        // `expirationInMinutes` scopes how long the token is accepted at the MicroVM
        // endpoint; the response models the token alone, so it is recorded with the token
        // rather than reported, and the endpoint enforces it
        // (`crate::state::microvm_endpoint`).
        //
        // Nothing in a service constructs a secret: the emulator's credential store
        // mints it (see `crate::state::mint_microvm_auth_token`).
        let Some(token) = crate::state::mint_microvm_auth_token(ctx.credential_store()) else {
            return Err(unavailable(
                "no shell authentication token could be minted for the MicroVM",
            ));
        };

        // Only the DIGEST of the token is kept, so the caller holds the one copy of the
        // secret and the endpoint can still recognise it. The record carries the REGION
        // the mint was authorised in beside the `microvmId`, the same compound identity
        // the lookup above resolved through, so a token can never authenticate a MicroVM
        // of another region's partition — and what it GRANTS is that MicroVM's shell,
        // never a port of its endpoint.
        state.lock().record_auth_token(
            &region,
            &microvm_id,
            &token,
            input.expiration_in_minutes,
            crate::state::microvm_endpoint::TokenGrant::Shell,
            now,
        );

        let mut auth_token = ::std::collections::HashMap::new();
        auth_token.insert(AUTH_TOKEN_KEY.to_string(), token);
        Ok(CreateMicrovmShellAuthTokenOutput { auth_token })
    }
}

/// The modelled server error for work this handler could not complete.
fn unavailable(message: &str) -> CreateMicrovmShellAuthTokenError {
    CreateMicrovmShellAuthTokenErrorModeled::InternalServerException(InternalServerException {
        message: Some(message.to_string()),
        retry_after_seconds: None,
    })
    .into()
}
