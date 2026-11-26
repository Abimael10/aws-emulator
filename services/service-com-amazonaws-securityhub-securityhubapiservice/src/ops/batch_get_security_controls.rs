// SPDX-License-Identifier: CC0-1.0

//! @op BatchGetSecurityControls
//! Reports a batch of published security controls as they stand for the calling account
//! and Region: each control's catalogued metadata, whether the standards this account
//! subscribes to leave it enabled, and its EFFECTIVE parameters — the values
//! `UpdateSecurityControl` persisted in `crate::state`, over the defaults the catalog
//! publishes for every parameter nobody customized
//! ([`crate::state::security_control_view`]).
//!
//! An id the catalog publishes no control under fails only its own entry of the batch:
//! it is reported under `UnprocessedIds` with the `RESOURCE_NOT_FOUND` error code, so
//! one unknown id does not cost a caller the controls it asked for alongside it.
//!
//! ---
//!
//! # Modeled contract for `BatchGetSecurityControls`
//!
//! What AWS documents for this operation, from this service's Smithy model. HTML is
//! stripped and long entries are clipped at ` …`. For the FULL text of any one item,
//! ask the model rather than reading the raw JSON:
//!
//! ```text
//! smithy_query(query="doc",shape="<Shape>",member="<Member>",model_path="services/service-com-amazonaws-securityhub-securityhubapiservice/smithy-model/com-amazonaws-securityhub-securityhubapiservice.json")
//! ```
//!
//! Provides details about a batch of security controls for the current Amazon Web Services
//! account and Amazon Web Services Region.
//!
//! ## Input
//!
//! * `SecurityControlIds` (StringList) **required** — A list of security controls (identified
//!   with SecurityControlId, SecurityControlArn, or a mix of both parameters). The security
//!   control ID or Amazon Resource Name (ARN) is the same across standards.
//!
//! ## Output
//!
//! * `SecurityControls` (SecurityControls) **required** — An array that returns the
//!   identifier, Amazon Resource Name (ARN), and other details about a security control. The
//!   same information is returned whether the request includes SecurityControlId or
//!   SecurityControlArn.
//! * `UnprocessedIds` (UnprocessedSecurityControls) — A security control (identified with
//!   SecurityControlId, SecurityControlArn, or a mix of both parameters) for which details
//!   cannot be returned.
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

#[allow(unused_imports)]
use super::types::*;

// `#[operation_error]` below expands to an invocation of the LOCAL declarative
// `smithy!` macro, which this line is what emits into the module. Without it the
// expansion fails with `cannot find macro 'smithy' in this scope` and the
// `BatchGetSecurityControlsError` enum is never produced.
smithy_macros::smithy_model!("smithy-model/com-amazonaws-securityhub-securityhubapiservice.json");

// The operation's MODELED errors. `#[operation_error]` accepts only variants the
// model declares for this operation, so the scaffold's `NotImplemented` cannot live
// here — hence the `BatchGetSecurityControlsError` alias below. An operation that models no errors
// generates an UNINHABITED enum here, which is correct and expected: there is nothing
// to construct until the model says otherwise.
#[smithy_macros::operation_error("com.amazonaws.securityhub#BatchGetSecurityControls")]
#[derive(Debug)]
pub enum BatchGetSecurityControlsErrorModeled {
    InternalException(InternalException),
    InvalidAccessException(InvalidAccessException),
    InvalidInputException(InvalidInputException),
    LimitExceededException(LimitExceededException),
}

/// This operation's error channel, as the generated `impl Operation` names it
/// (`crate::ops::batch_get_security_controls::BatchGetSecurityControlsError`). Wrapping the modeled enum is what
/// lets a scaffolded operation answer 501 for one whose model declares no errors at all.
pub type BatchGetSecurityControlsError = crate::scaffolded::Scaffolded<BatchGetSecurityControlsErrorModeled>;

/// Lets `?` and `.into()` lift a modeled failure into the error channel, so an
/// implemented handler writes `Err(SomeModeledError.into())` rather than naming the
/// wrapper variant. See `crate::scaffolded` for how to drop the wrapper entirely once
/// this operation is implemented.
impl From<BatchGetSecurityControlsErrorModeled> for BatchGetSecurityControlsError {
    fn from(modeled: BatchGetSecurityControlsErrorModeled) -> Self {
        crate::scaffolded::Scaffolded::Modeled(modeled)
    }
}

/// The `BatchGetSecurityControls` operation.
///
/// Owns the shared engine (the constructor seam) and implements
/// [`smithy_server_runtime::Operation`]. Generic over the engine type `Eng`, so this
/// crate names no concrete engine type: a migrated service that needs state picks a
/// concrete engine where it mounts the service, not here.
pub struct BatchGetSecurityControls<Eng> {
    engine: ::std::sync::Arc<Eng>,
}

impl<Eng> BatchGetSecurityControls<Eng> {
    /// Construct the operation from the shared engine. INFALLIBLE — the engine is a
    /// construction-time dependency, so engine access is guaranteed by the time any
    /// request runs. Called by the generated `EmulatedService::new(engine)` constructor.
    pub fn new(engine: ::std::sync::Arc<Eng>) -> Self {
        Self { engine }
    }
}

/// `UnprocessedErrorCode$RESOURCE_NOT_FOUND`: the entry's control could not be found.
/// "The `NOT_FOUND` value has been deprecated and replaced by the
/// `RESOURCE_NOT_FOUND` value" (`smithy_query(query="doc",
/// shape="UnprocessedSecurityControl", member="ErrorCode")`), so a control nothing
/// answers to is reported under the current value.
const UNPROCESSED_RESOURCE_NOT_FOUND: &str = "RESOURCE_NOT_FOUND";

impl<Eng: ::std::marker::Send + ::std::marker::Sync + 'static> smithy_server_runtime::Operation
    for BatchGetSecurityControls<Eng>
{
    type Input = BatchGetSecurityControlsInput;
    type Output = BatchGetSecurityControlsOutput;
    type Error = BatchGetSecurityControlsError;

    const NAME: &'static str = "BatchGetSecurityControls";
    const INPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::BATCH_GET_SECURITY_CONTROLS_INPUT_SCHEMA;
    const OUTPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::BATCH_GET_SECURITY_CONTROLS_OUTPUT_SCHEMA;
    const SUCCESS_STATUS: u16 = 200u16;

    async fn handle(
        &self,
        input: Self::Input,
        ctx: &smithy_server_runtime::OperationContext<'_>,
    ) -> Result<Self::Output, Self::Error> {
        let state = crate::state::service_state(&*self.engine).ok_or_else(|| {
            BatchGetSecurityControlsError::Modeled(
                BatchGetSecurityControlsErrorModeled::InternalException(InternalException {
                    message: Some(crate::state::STATE_UNAVAILABLE_MESSAGE.to_string()),
                    code: Some("InternalException".to_string()),
                }),
            )
        })?;

        // One call reports one bounded batch of controls: a batch longer than this
        // service's bound is refused as an out-of-range input rather than served, so
        // neither the work one call does under the state lock nor the response it
        // produces grows with what a caller sends.
        let requested = input.security_control_ids.len();
        if requested > crate::state::MAX_SECURITY_CONTROL_BATCH {
            return Err(
                BatchGetSecurityControlsErrorModeled::InvalidInputException(
                    InvalidInputException {
                        message: Some(format!(
                            "SecurityControlIds: a request may address at most {} security controls, got {requested}",
                            crate::state::MAX_SECURITY_CONTROL_BATCH
                        )),
                        code: Some("InvalidInputException".to_string()),
                    },
                )
                .into(),
            );
        }

        let key = crate::state::hub_key(ctx);
        let region = key.1.clone();
        let mut store = state.lock();
        if !store.hubs.contains_key(&key) {
            return Err(
                BatchGetSecurityControlsErrorModeled::InvalidAccessException(
                    InvalidAccessException {
                        message: Some(crate::state::not_subscribed_message(&key.0)),
                        code: Some("InvalidAccessException".to_string()),
                    },
                )
                .into(),
            );
        }

        // A control's enablement follows the standards this account subscribes to, so
        // the subscription transitions the clock has already made due are materialised
        // before they are read — whether or not anybody called.
        crate::state::settle_standards(&mut store, &key);

        let mut security_controls: Vec<SecurityControl> = Vec::new();
        let mut unprocessed: Vec<UnprocessedSecurityControl> = Vec::new();
        for identifier in &input.security_control_ids {
            // "A list of security controls (identified with SecurityControlId,
            // SecurityControlArn, or a mix of both parameters)": either form addresses
            // the same published control.
            let Some(entry) = crate::state::security_control_by_id_or_arn(&region, identifier)
            else {
                unprocessed.push(UnprocessedSecurityControl {
                    security_control_id: identifier.clone(),
                    error_code: UNPROCESSED_RESOURCE_NOT_FOUND.to_string(),
                    error_reason: Some(crate::state::security_control_does_not_exist_message(
                        identifier,
                    )),
                });
                continue;
            };
            let customization = store.security_control_customizations.get(
                &crate::state::security_control_customization_key(&key, entry.control.control_id),
            );
            let status = crate::state::security_control_status(&store, &key, entry);
            security_controls.push(crate::state::security_control_view(
                &region,
                entry,
                customization,
                &status,
            ));
        }

        Ok(BatchGetSecurityControlsOutput {
            security_controls,
            unprocessed_ids: Some(unprocessed),
        })
    }
}
