// SPDX-License-Identifier: CC0-1.0

//! @op GetSecurityControlDefinition
//! Reports the definition of ONE security control: the standard-agnostic entry
//! `crate::state`'s security-control catalog publishes under the `SecurityControlId`
//! the request names, rendered by the same
//! [`crate::state::security_control_definition_view`] every operation that answers
//! with a `SecurityControlDefinition` uses, so a single read and a listing cannot
//! describe a control differently.
//!
//! The catalog is the service's own published metadata, identical for every caller and
//! unchanged by what an account customized, so this operation reads no account state
//! and gates on no hub (see [`crate::state::not_subscribed_message`]). An id the
//! catalog publishes no control under names no resource, which is the one failure this
//! lookup has.
//!
//! ---
//!
//! # Modeled contract for `GetSecurityControlDefinition`
//!
//! What AWS documents for this operation, from this service's Smithy model. HTML is
//! stripped and long entries are clipped at ` …`. For the FULL text of any one item,
//! ask the model rather than reading the raw JSON:
//!
//! ```text
//! smithy_query(query="doc",shape="<Shape>",member="<Member>",model_path="services/service-com-amazonaws-securityhub-securityhubapiservice/smithy-model/com-amazonaws-securityhub-securityhubapiservice.json")
//! ```
//!
//! Retrieves the definition of a security control. The definition includes the control
//! title, description, Region availability, parameter definitions, and other details.
//!
//! ## Input
//!
//! * `SecurityControlId` (NonEmptyString, pattern \S) **required** — The ID of the security
//!   control to retrieve the definition for. This field doesn’t accept an Amazon Resource
//!   Name (ARN).
//!
//! ## Output
//!
//! * `SecurityControlDefinition` (SecurityControlDefinition) **required** — Provides metadata
//!   for a security control, including its unique standard-agnostic identifier, title,
//!   description, severity, availability in Amazon Web Services Regions, and a link to
//!   remediation steps.
//!
//! ## Errors
//!
//! * `InternalException` (HTTP 500) — Internal server error.
//! * `InvalidAccessException` (HTTP 401) — The account doesn't have permission to perform
//!   this action.
//! * `InvalidInputException` (HTTP 400) — The request was rejected because you supplied an
//!   invalid or out-of-range value for an input parameter.
//! * `LimitExceededException` (HTTP 429) — The request was rejected because it attempted to
//!   create resources beyond the current Amazon Web Services account or throttling limits.
//!   The error code describes the limit exceeded.
//! * `ResourceNotFoundException` (HTTP 404) — The request was rejected because we can't find
//!   the specified resource.

#[allow(unused_imports)]
use super::types::*;

// `#[operation_error]` below expands to an invocation of the LOCAL declarative
// `smithy!` macro, which this line is what emits into the module. Without it the
// expansion fails with `cannot find macro 'smithy' in this scope` and the
// `GetSecurityControlDefinitionError` enum is never produced.
smithy_macros::smithy_model!("smithy-model/com-amazonaws-securityhub-securityhubapiservice.json");

// The operation's MODELED errors. `#[operation_error]` accepts only variants the
// model declares for this operation, so the scaffold's `NotImplemented` cannot live
// here — hence the `GetSecurityControlDefinitionError` alias below. An operation that models no errors
// generates an UNINHABITED enum here, which is correct and expected: there is nothing
// to construct until the model says otherwise.
#[smithy_macros::operation_error("com.amazonaws.securityhub#GetSecurityControlDefinition")]
#[derive(Debug)]
pub enum GetSecurityControlDefinitionErrorModeled {
    InternalException(InternalException),
    InvalidAccessException(InvalidAccessException),
    InvalidInputException(InvalidInputException),
    LimitExceededException(LimitExceededException),
    ResourceNotFoundException(ResourceNotFoundException),
}

/// This operation's error channel, as the generated `impl Operation` names it
/// (`crate::ops::get_security_control_definition::GetSecurityControlDefinitionError`). Wrapping the modeled enum is what
/// lets a scaffolded operation answer 501 for one whose model declares no errors at all.
pub type GetSecurityControlDefinitionError = crate::scaffolded::Scaffolded<GetSecurityControlDefinitionErrorModeled>;

/// Lets `?` and `.into()` lift a modeled failure into the error channel, so an
/// implemented handler writes `Err(SomeModeledError.into())` rather than naming the
/// wrapper variant. See `crate::scaffolded` for how to drop the wrapper entirely once
/// this operation is implemented.
impl From<GetSecurityControlDefinitionErrorModeled> for GetSecurityControlDefinitionError {
    fn from(modeled: GetSecurityControlDefinitionErrorModeled) -> Self {
        crate::scaffolded::Scaffolded::Modeled(modeled)
    }
}

/// The `GetSecurityControlDefinition` operation.
///
/// Owns the shared engine (the constructor seam) and implements
/// [`smithy_server_runtime::Operation`]. Generic over the engine type `Eng`, so this
/// crate names no concrete engine type: a migrated service that needs state picks a
/// concrete engine where it mounts the service, not here.
pub struct GetSecurityControlDefinition<Eng> {
    engine: ::std::sync::Arc<Eng>,
}

impl<Eng> GetSecurityControlDefinition<Eng> {
    /// Construct the operation from the shared engine. INFALLIBLE — the engine is a
    /// construction-time dependency, so engine access is guaranteed by the time any
    /// request runs. Called by the generated `EmulatedService::new(engine)` constructor.
    pub fn new(engine: ::std::sync::Arc<Eng>) -> Self {
        Self { engine }
    }
}

impl<Eng: ::std::marker::Send + ::std::marker::Sync + 'static> smithy_server_runtime::Operation
    for GetSecurityControlDefinition<Eng>
{
    type Input = GetSecurityControlDefinitionInput;
    type Output = GetSecurityControlDefinitionOutput;
    type Error = GetSecurityControlDefinitionError;

    const NAME: &'static str = "GetSecurityControlDefinition";
    const INPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::GET_SECURITY_CONTROL_DEFINITION_INPUT_SCHEMA;
    const OUTPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::GET_SECURITY_CONTROL_DEFINITION_OUTPUT_SCHEMA;
    const SUCCESS_STATUS: u16 = 200u16;

    async fn handle(
        &self,
        input: Self::Input,
        ctx: &smithy_server_runtime::OperationContext<'_>,
    ) -> Result<Self::Output, Self::Error> {
        // The published catalog is the service's own metadata, the same definition for
        // every caller in every Region, so this operation reads no account state and
        // nothing off the request but the id: the engine stays on `self` as the
        // constructor seam requires, unread.
        let _ = (ctx, &self.engine);

        // "The ID of the security control to retrieve the definition for": the id is
        // resolved against the published catalog, the single definition of which ids
        // name a security control. An id no control is published under — an ARN
        // included, which this field does not accept — names no resource to report.
        let requested = input.security_control_id.as_str();
        let entry = crate::state::security_control_by_id(requested).ok_or_else(|| {
            GetSecurityControlDefinitionErrorModeled::ResourceNotFoundException(
                ResourceNotFoundException {
                    message: Some(crate::state::security_control_does_not_exist_message(
                        requested,
                    )),
                    code: Some("ResourceNotFoundException".to_string()),
                },
            )
        })?;

        Ok(GetSecurityControlDefinitionOutput {
            // The one definition of how a control definition is reported, shared with
            // every other operation that answers with a `SecurityControlDefinition`.
            security_control_definition: crate::state::security_control_definition_view(entry),
        })
    }
}
