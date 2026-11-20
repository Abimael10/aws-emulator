// SPDX-License-Identifier: CC0-1.0

//! @op UpdateTieringConfiguration
//!
//! Rewrites the stored tiering configuration the `TieringConfigurationName`
//! addresses ([`crate::state::TieringConfigurationRecord`] — the SAME record
//! `CreateTieringConfiguration` stored and both reads report) with the body the
//! request carries, and records the instant it did.
//!
//! The rules this handler holds:
//!
//! * a tiering configuration is addressed by NAME, and the name "cannot be
//!   updated after it is created" (this operation's model doc), so an update is
//!   resolved against the configuration that name already holds and never
//!   stores a new one: a name the account holds no configuration under is the
//!   one not-found refusal
//!   ([`crate::state::tiering_configuration_not_found`]), shared with the read
//!   and the delete;
//! * the body an update carries — the vault it applies to and its selections —
//!   REPLACES the stored one and is stored as submitted, exactly as the create
//!   stores its body, so the reads report an updated configuration the way they
//!   report a created one;
//! * the identity the create minted (the name, the ARN derived from it and the
//!   creation instant) survives every update, and the update instant is what
//!   this call rewrites — reported here and by both reads
//!   (`LastUpdatedTime`), which the create set to its own instant;
//! * the selections the body carries may only name the one type of resource
//!   tiering supports — "currently limited to `S3`" (model doc for
//!   `ResourceSelection$ResourceType`) — so a body naming any other type is
//!   refused `InvalidParameterValueException`
//!   ([`crate::state::unsupported_tiering_resource_type`]), the SAME reading the
//!   create applies, and the stored configuration is left as it was;
//! * however many updates of one name arrive together, each resolves the store
//!   under the same lock, so each applies its whole body and the configuration
//!   the reads report is one submitted body rather than a mixture of several.
//!
//! Shared types live in `super::types`; the error channel in `crate::scaffolded`.
//!
//! ---
//!
//! # Modeled contract for `UpdateTieringConfiguration`
//!
//! What AWS documents for this operation, from this service's Smithy model. HTML is
//! stripped and long entries are clipped at ` …`. For the FULL text of any one item,
//! ask the model rather than reading the raw JSON:
//!
//! ```text
//! smithy_query(query="doc",shape="<Shape>",member="<Member>",model_path="services/service-com-amazonaws-backup-cryocontrollerusermanager/smithy-model/com-amazonaws-backup-cryocontrollerusermanager.json")
//! ```
//!
//! This request will send changes to your specified tiering configuration.
//! TieringConfigurationName cannot be updated after it is created. ResourceSelection can
//! contain: Resources TieringDownSettingsInDays ResourceType
//!
//! ## Input
//!
//! * `TieringConfiguration` (TieringConfigurationInputForUpdate) **required** — Specifies the
//!   body of a tiering configuration.
//! * `TieringConfigurationName` (TieringConfigurationName, pattern ^[a-zA-Z0-9_]{1,200}$)
//!   **required** — The name of a tiering configuration to update.
//!
//! ## Output
//!
//! * `CreationTime` (timestamp) — The date and time a tiering configuration was created, in
//!   Unix format and Coordinated Universal Time (UTC). The value of CreationTime is accurate
//!   to milliseconds. For example, the value 1516925490.087 represents Friday, January 26,
//!   2018 12:11:30.087AM.
//! * `LastUpdatedTime` (timestamp) — The date and time a tiering configuration was updated,
//!   in Unix format and Coordinated Universal Time (UTC). The value of LastUpdatedTime is
//!   accurate to milliseconds. For example, the value 1516925490.087 represents Friday,
//!   January 26, 2018 12:11:30.087AM.
//! * `TieringConfigurationArn` (ARN) — An Amazon Resource Name (ARN) that uniquely identifies
//!   the updated tiering configuration.
//! * `TieringConfigurationName` (TieringConfigurationName, pattern ^[a-zA-Z0-9_]{1,200}$) —
//!   This unique string is the name of the tiering configuration.
//!
//! ## Errors
//!
//! * `AlreadyExistsException` (HTTP 400) — The required resource already exists.
//! * `ConflictException` (HTTP 409) — Backup can't perform the action that you requested
//!   until it finishes performing a previous action. Try again later.
//! * `InvalidParameterValueException` (HTTP 400) — Indicates that something is wrong with a
//!   parameter's value. For example, the value is out of range.
//! * `LimitExceededException` (HTTP 400) — A limit in the request has been exceeded; for
//!   example, a maximum number of items allowed in a request.
//! * `MissingParameterValueException` (HTTP 400) — Indicates that a required parameter is
//!   missing.
//! * `ResourceNotFoundException` (HTTP 400) — A resource that is required for the action
//!   doesn't exist.
//! * `ServiceUnavailableException` (HTTP 500) — The request failed due to a temporary failure
//!   of the server.

#[allow(unused_imports)]
use super::types::*;

// `#[operation_error]` below expands to an invocation of the LOCAL declarative
// `smithy!` macro, which this line is what emits into the module. Without it the
// expansion fails with `cannot find macro 'smithy' in this scope` and the
// `UpdateTieringConfigurationError` enum is never produced.
smithy_macros::smithy_model!("smithy-model/com-amazonaws-backup-cryocontrollerusermanager.json");

// The operation's MODELED errors. `#[operation_error]` accepts only variants the
// model declares for this operation, so the scaffold's `NotImplemented` cannot live
// here — hence the `UpdateTieringConfigurationError` alias below. An operation that models no errors
// generates an UNINHABITED enum here, which is correct and expected: there is nothing
// to construct until the model says otherwise.
#[smithy_macros::operation_error("com.amazonaws.backup#UpdateTieringConfiguration")]
#[derive(Debug)]
pub enum UpdateTieringConfigurationErrorModeled {
    AlreadyExistsException(AlreadyExistsException),
    ConflictException(ConflictException),
    InvalidParameterValueException(InvalidParameterValueException),
    LimitExceededException(LimitExceededException),
    MissingParameterValueException(MissingParameterValueException),
    ResourceNotFoundException(ResourceNotFoundException),
    ServiceUnavailableException(ServiceUnavailableException),
}

/// This operation's error channel, as the generated `impl Operation` names it
/// (`crate::ops::update_tiering_configuration::UpdateTieringConfigurationError`). Wrapping the modeled enum is what
/// lets a scaffolded operation answer 501 for one whose model declares no errors at all.
pub type UpdateTieringConfigurationError = crate::scaffolded::Scaffolded<UpdateTieringConfigurationErrorModeled>;

/// Lets `?` and `.into()` lift a modeled failure into the error channel, so an
/// implemented handler writes `Err(SomeModeledError.into())` rather than naming the
/// wrapper variant. See `crate::scaffolded` for how to drop the wrapper entirely once
/// this operation is implemented.
impl From<UpdateTieringConfigurationErrorModeled> for UpdateTieringConfigurationError {
    fn from(modeled: UpdateTieringConfigurationErrorModeled) -> Self {
        crate::scaffolded::Scaffolded::Modeled(modeled)
    }
}

/// The `UpdateTieringConfiguration` operation.
///
/// Owns the shared engine (the constructor seam) and implements
/// [`smithy_server_runtime::Operation`]. Generic over the engine type `Eng`, so this
/// crate names no concrete engine type: a migrated service that needs state picks a
/// concrete engine where it mounts the service, not here.
pub struct UpdateTieringConfiguration<Eng> {
    engine: ::std::sync::Arc<Eng>,
}

impl<Eng> UpdateTieringConfiguration<Eng> {
    /// Construct the operation from the shared engine. INFALLIBLE — the engine is a
    /// construction-time dependency, so engine access is guaranteed by the time any
    /// request runs. Called by the generated `EmulatedService::new(engine)` constructor.
    pub fn new(engine: ::std::sync::Arc<Eng>) -> Self {
        Self { engine }
    }
}

impl<Eng: ::std::marker::Send + ::std::marker::Sync + 'static> smithy_server_runtime::Operation
    for UpdateTieringConfiguration<Eng>
{
    type Input = UpdateTieringConfigurationInput;
    type Output = UpdateTieringConfigurationOutput;
    type Error = UpdateTieringConfigurationError;

    const NAME: &'static str = "UpdateTieringConfiguration";
    const INPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::UPDATE_TIERING_CONFIGURATION_INPUT_SCHEMA;
    const OUTPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::UPDATE_TIERING_CONFIGURATION_OUTPUT_SCHEMA;
    const SUCCESS_STATUS: u16 = 200u16;

    async fn handle(
        &self,
        input: Self::Input,
        ctx: &smithy_server_runtime::OperationContext<'_>,
    ) -> Result<Self::Output, Self::Error> {
        let _ = ctx;
        // `TieringConfiguration` is `@required`, as are its `BackupVaultName`
        // and `ResourceSelection` and each selection's members, and the
        // `TieringDownSettingsInDays` range is modelled, so an absent or
        // out-of-range body is refused before this handler runs. The type of
        // resource a selection may name is stated in prose rather than in a
        // trait, so it is read here — before the store is reached, so a refused
        // update rewrites nothing.
        if let Some(refusal) = crate::state::unsupported_tiering_resource_type(
            &input.tiering_configuration.resource_selection,
        ) {
            return Err(
                UpdateTieringConfigurationErrorModeled::InvalidParameterValueException(refusal)
                    .into(),
            );
        }
        let state = crate::state::service_state(&*self.engine).ok_or_else(|| {
            UpdateTieringConfigurationErrorModeled::ServiceUnavailableException(
                ServiceUnavailableException {
                    message: Some("the backup vault store is unavailable".to_string()),
                    ..Default::default()
                },
            )
        })?;
        let now = ::std::time::SystemTime::now();
        let mut store = state.lock();

        // The name addresses the configuration to rewrite; one the account
        // holds no configuration under is the not-found condition the read and
        // the delete answer. Resolved under the lock the rewrite below takes,
        // so a refused update changes nothing.
        let record = store
            .stored_tiering_configuration_mut(&input.tiering_configuration_name)
            .map_err(UpdateTieringConfigurationErrorModeled::ResourceNotFoundException)?;
        record.apply_update(input.tiering_configuration, now);
        Ok(UpdateTieringConfigurationOutput {
            tiering_configuration_arn: Some(record.arn.clone()),
            tiering_configuration_name: Some(record.name.clone()),
            // The create's instant, which an update leaves alone, beside the
            // instant this update stored the new body.
            creation_time: Some(crate::state::modelled_timestamp(record.creation_time)),
            last_updated_time: Some(crate::state::modelled_timestamp(record.last_updated_time)),
        })
    }
}
