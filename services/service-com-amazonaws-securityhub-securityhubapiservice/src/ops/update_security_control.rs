// SPDX-License-Identifier: CC0-1.0

//! @op UpdateSecurityControl
//! Sets the custom parameter values of ONE published security control for the calling
//! account and Region: a parameter set with `ValueType` `CUSTOM` keeps the value the
//! request carried, and one set with `DEFAULT` goes back to the value the control's
//! definition publishes for it.
//!
//! The control is addressed by its `SecurityControlId` or its `SecurityControlArn` and
//! resolved against the published catalog
//! ([`crate::state::security_control_by_id_or_arn`]); every supplied parameter name and
//! value is checked against that control's published `ParameterDefinitions` before
//! anything is written ([`crate::state::custom_parameter_violation`]), so a call either
//! applies in full or changes nothing. What it persists is what
//! `BatchGetSecurityControls` reports.
//!
//! ---
//!
//! # Modeled contract for `UpdateSecurityControl`
//!
//! What AWS documents for this operation, from this service's Smithy model. HTML is
//! stripped and long entries are clipped at ` …`. For the FULL text of any one item,
//! ask the model rather than reading the raw JSON:
//!
//! ```text
//! smithy_query(query="doc",shape="<Shape>",member="<Member>",model_path="services/service-com-amazonaws-securityhub-securityhubapiservice/smithy-model/com-amazonaws-securityhub-securityhubapiservice.json")
//! ```
//!
//! Updates the properties of a security control.
//!
//! ## Input
//!
//! * `LastUpdateReason` (AlphaNumericNonEmptyString, pattern ^([^\u0000-\u007F]|[-_
//!   a-zA-Z0-9])+$) — The most recent reason for updating the properties of the security
//!   control. This field accepts alphanumeric characters in addition to white spaces, dashes,
//!   and underscores.
//! * `Parameters` (Parameters) **required** — An object that specifies which security control
//!   parameters to update.
//! * `SecurityControlId` (NonEmptyString, pattern \S) **required** — The Amazon Resource Name
//!   (ARN) or ID of the control to update.
//!
//! ## Errors
//!
//! * `AccessDeniedException` (HTTP 403) — You don't have permission to perform the action
//!   specified in the request.
//! * `InternalException` (HTTP 500) — Internal server error.
//! * `InvalidAccessException` (HTTP 401) — The account doesn't have permission to perform
//!   this action.
//! * `InvalidInputException` (HTTP 400) — The request was rejected because you supplied an
//!   invalid or out-of-range value for an input parameter.
//! * `LimitExceededException` (HTTP 429) — The request was rejected because it attempted to
//!   create resources beyond the current Amazon Web Services account or throttling limits.
//!   The error code describes the limit exceeded.
//! * `ResourceInUseException` (HTTP 400) — The request was rejected because it conflicts with
//!   the resource's availability. For example, you tried to update a security control that's
//!   currently in the UPDATING state.
//! * `ResourceNotFoundException` (HTTP 404) — The request was rejected because we can't find
//!   the specified resource.

#[allow(unused_imports)]
use super::types::*;

// `#[operation_error]` below expands to an invocation of the LOCAL declarative
// `smithy!` macro, which this line is what emits into the module. Without it the
// expansion fails with `cannot find macro 'smithy' in this scope` and the
// `UpdateSecurityControlError` enum is never produced.
smithy_macros::smithy_model!("smithy-model/com-amazonaws-securityhub-securityhubapiservice.json");

// The operation's MODELED errors. `#[operation_error]` accepts only variants the
// model declares for this operation, so the scaffold's `NotImplemented` cannot live
// here — hence the `UpdateSecurityControlError` alias below. An operation that models no errors
// generates an UNINHABITED enum here, which is correct and expected: there is nothing
// to construct until the model says otherwise.
#[smithy_macros::operation_error("com.amazonaws.securityhub#UpdateSecurityControl")]
#[derive(Debug)]
pub enum UpdateSecurityControlErrorModeled {
    AccessDeniedException(AccessDeniedException),
    InternalException(InternalException),
    InvalidAccessException(InvalidAccessException),
    InvalidInputException(InvalidInputException),
    LimitExceededException(LimitExceededException),
    ResourceInUseException(ResourceInUseException),
    ResourceNotFoundException(ResourceNotFoundException),
}

/// This operation's error channel, as the generated `impl Operation` names it
/// (`crate::ops::update_security_control::UpdateSecurityControlError`). Wrapping the modeled enum is what
/// lets a scaffolded operation answer 501 for one whose model declares no errors at all.
pub type UpdateSecurityControlError = crate::scaffolded::Scaffolded<UpdateSecurityControlErrorModeled>;

/// Lets `?` and `.into()` lift a modeled failure into the error channel, so an
/// implemented handler writes `Err(SomeModeledError.into())` rather than naming the
/// wrapper variant. See `crate::scaffolded` for how to drop the wrapper entirely once
/// this operation is implemented.
impl From<UpdateSecurityControlErrorModeled> for UpdateSecurityControlError {
    fn from(modeled: UpdateSecurityControlErrorModeled) -> Self {
        crate::scaffolded::Scaffolded::Modeled(modeled)
    }
}

/// The `UpdateSecurityControl` operation.
///
/// Owns the shared engine (the constructor seam) and implements
/// [`smithy_server_runtime::Operation`]. Generic over the engine type `Eng`, so this
/// crate names no concrete engine type: a migrated service that needs state picks a
/// concrete engine where it mounts the service, not here.
pub struct UpdateSecurityControl<Eng> {
    engine: ::std::sync::Arc<Eng>,
}

impl<Eng> UpdateSecurityControl<Eng> {
    /// Construct the operation from the shared engine. INFALLIBLE — the engine is a
    /// construction-time dependency, so engine access is guaranteed by the time any
    /// request runs. Called by the generated `EmulatedService::new(engine)` constructor.
    pub fn new(engine: ::std::sync::Arc<Eng>) -> Self {
        Self { engine }
    }
}

impl<Eng: ::std::marker::Send + ::std::marker::Sync + 'static> smithy_server_runtime::Operation
    for UpdateSecurityControl<Eng>
{
    type Input = UpdateSecurityControlInput;
    type Output = UpdateSecurityControlOutput;
    type Error = UpdateSecurityControlError;

    const NAME: &'static str = "UpdateSecurityControl";
    const INPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::UPDATE_SECURITY_CONTROL_INPUT_SCHEMA;
    const OUTPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::UPDATE_SECURITY_CONTROL_OUTPUT_SCHEMA;
    const SUCCESS_STATUS: u16 = 200u16;

    async fn handle(
        &self,
        input: Self::Input,
        ctx: &smithy_server_runtime::OperationContext<'_>,
    ) -> Result<Self::Output, Self::Error> {
        let state = crate::state::service_state(&*self.engine).ok_or_else(|| {
            UpdateSecurityControlError::Modeled(
                UpdateSecurityControlErrorModeled::InternalException(InternalException {
                    message: Some(crate::state::STATE_UNAVAILABLE_MESSAGE.to_string()),
                    code: Some("InternalException".to_string()),
                }),
            )
        })?;
        let invalid_input = |message: String| {
            UpdateSecurityControlError::Modeled(
                UpdateSecurityControlErrorModeled::InvalidInputException(InvalidInputException {
                    message: Some(message),
                    code: Some("InvalidInputException".to_string()),
                }),
            )
        };

        let key = crate::state::hub_key(ctx);
        let (account_id, region) = (key.0.clone(), key.1.clone());

        // Whether the request is well formed is decided before any state is read, so
        // the same request is refused identically on every hub.
        if let Some(reason) = input.last_update_reason.as_deref() {
            let characters = reason.chars().count();
            if characters > crate::state::MAX_LAST_UPDATE_REASON_LEN {
                return Err(invalid_input(format!(
                    "LastUpdateReason: a reason may not exceed {} characters, got {characters}",
                    crate::state::MAX_LAST_UPDATE_REASON_LEN
                )));
            }
        }

        let mut store = state.lock();
        if !store.hubs.contains_key(&key) {
            return Err(UpdateSecurityControlErrorModeled::InvalidAccessException(
                InvalidAccessException {
                    message: Some(crate::state::not_subscribed_message(&account_id)),
                    code: Some("InvalidAccessException".to_string()),
                },
            )
            .into());
        }

        // "The Amazon Resource Name (ARN) or ID of the control to update": an id no
        // control is published under names no resource to customize.
        let requested = input.security_control_id.as_str();
        let entry = crate::state::security_control_by_id_or_arn(&region, requested).ok_or_else(
            || {
                UpdateSecurityControlError::Modeled(
                    UpdateSecurityControlErrorModeled::ResourceNotFoundException(
                        ResourceNotFoundException {
                            message: Some(crate::state::security_control_does_not_exist_message(
                                requested,
                            )),
                            code: Some("ResourceNotFoundException".to_string()),
                        },
                    ),
                )
            },
        )?;

        // Every supplied parameter is resolved and checked against the control's
        // published definitions BEFORE anything is written, so a batch that names one
        // bad parameter changes none of them. The parameters are walked in name order,
        // so which violation a bad request is refused with does not depend on how a map
        // happened to be laid out.
        let mut writes: Vec<(String, Option<ParameterValue>)> = Vec::new();
        let mut supplied_parameters: Vec<(&String, &ParameterConfiguration)> =
            input.parameters.iter().collect();
        supplied_parameters.sort_by(|left, right| left.0.cmp(right.0));
        for (name, supplied) in supplied_parameters {
            // `ParameterValueType` is a modelled enum, so the boundary validator
            // already refuses a value outside it; checking here keeps a parameter from
            // being read as `DEFAULT` even if this handler is reached another way.
            if !PARAMETER_VALUE_TYPE_VALUES.contains(&supplied.value_type.as_str()) {
                return Err(invalid_input(format!(
                    "ValueType: {} is not a supported parameter value type; expected one of {}",
                    supplied.value_type,
                    PARAMETER_VALUE_TYPE_VALUES.join(", ")
                )));
            }
            let parameter = crate::state::published_control_parameter(entry, name).ok_or_else(
                || {
                    invalid_input(
                        crate::state::security_control_parameter_does_not_exist_message(
                            entry.control.control_id,
                            name,
                        ),
                    )
                },
            )?;
            if supplied.value_type == crate::state::PARAMETER_VALUE_TYPE_CUSTOM {
                // "When ValueType is set equal to CUSTOM, the Value field can't be
                // empty", and the value it carries must satisfy every option the
                // catalog publishes for the parameter.
                let Some(value) = supplied.value.clone() else {
                    return Err(invalid_input(format!(
                        "A Value is required for parameter {name} when ValueType is {}",
                        crate::state::PARAMETER_VALUE_TYPE_CUSTOM
                    )));
                };
                if let Some(violation) =
                    crate::state::custom_parameter_violation(parameter, &value)
                {
                    return Err(invalid_input(violation));
                }
                writes.push((parameter.name.to_string(), Some(value)));
            } else {
                // "When ValueType is set equal to DEFAULT, Security Hub CSPM ignores
                // user-provided input for the Value field": the parameter goes back to
                // the value the control's definition publishes, so the stored
                // customization is dropped rather than replaced by a copy of it.
                writes.push((parameter.name.to_string(), None));
            }
        }

        let customization = store
            .security_control_customizations
            .entry(crate::state::security_control_customization_key(
                &key,
                entry.control.control_id,
            ))
            .or_default();
        for (name, value) in writes {
            match value {
                Some(value) => {
                    customization.parameters.insert(name, value);
                }
                None => {
                    customization.parameters.remove(&name);
                }
            }
        }
        // "The MOST RECENT reason for updating the properties of the security control":
        // this call is now the most recent one, so a call that names no reason leaves
        // the control with none rather than with the previous call's.
        customization.last_update_reason = input.last_update_reason.clone();

        Ok(UpdateSecurityControlOutput::default())
    }
}
