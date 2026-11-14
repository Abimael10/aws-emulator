// SPDX-License-Identifier: CC0-1.0

//! @op DeleteVirtualMFADevice
//!
//! ## What this operation answers on the wire
//!
//! "Deletes a virtual MFA device. You must deactivate a user's virtual MFA device before you
//! can delete it" (this operation's documentation): the device is the one the request's
//! `SerialNumber` names — for a virtual device that serial IS its ARN
//! (`VirtualMFADevice$SerialNumber`) — and a delete removes it from the account, so no later
//! read of the account's devices reports it and the serial names nothing again.
//!
//! A device still ENABLED for a user "has attached subordinate entities"
//! (`DeleteConflictException`, HTTP 409): the association `EnableMFADevice` wrote is the
//! dependent that blocks the delete, and the device is NOT deleted until
//! `DeactivateMFADevice` clears it. A serial the account holds no device under is the entity
//! that does not exist (`NoSuchEntityException`, HTTP 404), and nothing is removed for it.
//!
//! ---
//!
//! # Modeled contract for `DeleteVirtualMFADevice`
//!
//! What AWS documents for this operation, from this service's Smithy model. HTML is
//! stripped and long entries are clipped at ` …`. For the FULL text of any one item,
//! ask the model rather than reading the raw JSON:
//!
//! ```text
//! smithy_query(query="doc",shape="<Shape>",member="<Member>",model_path="services/service-com-amazonaws-iam-awsidentitymanagementv20100508/smithy-model/com-amazonaws-iam-awsidentitymanagementv20100508.json")
//! ```
//!
//! Deletes a virtual MFA device. You must deactivate a user's virtual MFA device before you
//! can delete it. For information about deactivating MFA devices, see DeactivateMFADevice.
//!
//! ## Input
//!
//! * `SerialNumber` (serialNumberType, len 9..256, pattern ^[\w+=/:,.@-]+$) **required** —
//!   The serial number that uniquely identifies the MFA device. For virtual MFA devices, the
//!   serial number is the same as the ARN. This parameter allows (through its regex pattern)
//!   a string of characters consisting of upper and lowercase alphanumeric characters with no
//!   spaces. You can also include any of the following characters: =,.@:/-
//!
//! ## Errors
//!
//! * `ConcurrentModificationException` (HTTP 409) — The request was rejected because multiple
//!   requests to change this object were submitted simultaneously. Wait a few minutes and
//!   submit your request again.
//! * `DeleteConflictException` (HTTP 409) — The request was rejected because it attempted to
//!   delete a resource that has attached subordinate entities. The error message describes
//!   these entities.
//! * `LimitExceededException` (HTTP 409) — The request was rejected because it attempted to
//!   create resources beyond the current Amazon Web Services account limits. The error
//!   message describes the limit exceeded.
//! * `NoSuchEntityException` (HTTP 404) — The request was rejected because it referenced a
//!   resource entity that does not exist. The error message describes the resource.
//! * `ServiceFailureException` (HTTP 500) — The request processing has failed because of an
//!   unknown error, exception or failure.

#[allow(unused_imports)]
use super::types::*;

// `#[operation_error]` below expands to an invocation of the LOCAL declarative
// `smithy!` macro, which this line is what emits into the module. Without it the
// expansion fails with `cannot find macro 'smithy' in this scope` and the
// `DeleteVirtualMFADeviceError` enum is never produced.
smithy_macros::smithy_model!("smithy-model/com-amazonaws-iam-awsidentitymanagementv20100508.json");

#[derive(Debug, Clone, Default)]
pub struct DeleteVirtualMFADeviceOutput;

impl DeleteVirtualMFADeviceOutput {
    // `with_http` carries this operation's modelled response status. Remove it and
    // the status falls back to the protocol default, so a modelled 204 answers 200.
    pub const SCHEMA: ::aws_smithy_schema::Schema = ::aws_smithy_schema::Schema::new_struct(
        ::aws_smithy_schema::ShapeId::from_static("smithy.api#Unit", "smithy.api", "Unit"),
        ::aws_smithy_schema::ShapeType::Structure,
        &[],
    );
}

pub static DELETE_VIRTUAL_MFA_DEVICE_OUTPUT_SCHEMA: &::aws_smithy_schema::Schema = &DeleteVirtualMFADeviceOutput::SCHEMA;

impl ::aws_smithy_schema::serde::SerializableStruct for DeleteVirtualMFADeviceOutput {
    fn serialize_members(&self, _s: &mut dyn ::aws_smithy_schema::serde::ShapeSerializer) -> Result<(), ::aws_smithy_schema::serde::SerdeError> { Ok(()) }
}

// The operation's MODELED errors. `#[operation_error]` accepts only variants the
// model declares for this operation, so the scaffold's `NotImplemented` cannot live
// here — hence the `DeleteVirtualMFADeviceError` alias below. An operation that models no errors
// generates an UNINHABITED enum here, which is correct and expected: there is nothing
// to construct until the model says otherwise.
#[smithy_macros::operation_error("com.amazonaws.iam#DeleteVirtualMFADevice")]
#[derive(Debug)]
pub enum DeleteVirtualMFADeviceErrorModeled {
    ConcurrentModificationException(ConcurrentModificationException),
    DeleteConflictException(DeleteConflictException),
    LimitExceededException(LimitExceededException),
    NoSuchEntityException(NoSuchEntityException),
    ServiceFailureException(ServiceFailureException),
}

/// This operation's error channel, as the generated `impl Operation` names it
/// (`crate::ops::delete_virtual_mfa_device::DeleteVirtualMFADeviceError`). Wrapping the modeled enum is what
/// lets a scaffolded operation answer 501 for one whose model declares no errors at all.
pub type DeleteVirtualMFADeviceError = crate::scaffolded::Scaffolded<DeleteVirtualMFADeviceErrorModeled>;

/// Lets `?` and `.into()` lift a modeled failure into the error channel, so an
/// implemented handler writes `Err(SomeModeledError.into())` rather than naming the
/// wrapper variant. See `crate::scaffolded` for how to drop the wrapper entirely once
/// this operation is implemented.
impl From<DeleteVirtualMFADeviceErrorModeled> for DeleteVirtualMFADeviceError {
    fn from(modeled: DeleteVirtualMFADeviceErrorModeled) -> Self {
        crate::scaffolded::Scaffolded::Modeled(modeled)
    }
}

/// The `DeleteVirtualMFADevice` operation.
///
/// Owns the shared engine (the constructor seam) and implements
/// [`smithy_server_runtime::Operation`]. Generic over the engine type `Eng`, so this
/// crate names no concrete engine type: a migrated service that needs state picks a
/// concrete engine where it mounts the service, not here.
pub struct DeleteVirtualMFADevice<Eng> {
    engine: ::std::sync::Arc<Eng>,
}

impl<Eng> DeleteVirtualMFADevice<Eng> {
    /// Construct the operation from the shared engine. INFALLIBLE — the engine is a
    /// construction-time dependency, so engine access is guaranteed by the time any
    /// request runs. Called by the generated `EmulatedService::new(engine)` constructor.
    pub fn new(engine: ::std::sync::Arc<Eng>) -> Self {
        Self { engine }
    }
}

impl<Eng: ::std::marker::Send + ::std::marker::Sync + 'static> smithy_server_runtime::Operation
    for DeleteVirtualMFADevice<Eng>
{
    type Input = DeleteVirtualMFADeviceInput;
    type Output = DeleteVirtualMFADeviceOutput;
    type Error = DeleteVirtualMFADeviceError;

    const NAME: &'static str = "DeleteVirtualMFADevice";
    const INPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::DELETE_VIRTUAL_MFA_DEVICE_INPUT_SCHEMA;
    const OUTPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = DELETE_VIRTUAL_MFA_DEVICE_OUTPUT_SCHEMA;
    const SUCCESS_STATUS: u16 = 200u16;

    async fn handle(
        &self,
        input: Self::Input,
        ctx: &smithy_server_runtime::OperationContext<'_>,
    ) -> Result<Self::Output, Self::Error> {
        let Some(state) = crate::state::service_state(&*self.engine) else {
            return Err(DeleteVirtualMFADeviceErrorModeled::ServiceFailureException(
                ServiceFailureException {
                    message: Some("the IAM store is unavailable".to_string()),
                },
            )
            .into());
        };
        // A mutation: the lock is held across the lookup, the dependent check and the
        // removal, so a device cannot be enabled between being found free and being
        // deleted.
        let mut store = state.lock();

        // IAM authorizes its own API: the caller's identity-based policies are evaluated
        // before anything is read or deleted, on the ARN of the device the request
        // addresses — which for a virtual MFA device is the serial number itself. A
        // refusal names the device as the request named it and deletes nothing.
        crate::state::authorization::authorize_named(
            &store,
            ctx,
            crate::state::authorization::DELETE_VIRTUAL_MFA_DEVICE,
            &input.serial_number,
            &format!("mfa device {}", input.serial_number),
        )?;

        let Some(device) = store.virtual_mfa_device(&input.serial_number) else {
            return Err(DeleteVirtualMFADeviceErrorModeled::NoSuchEntityException(
                NoSuchEntityException {
                    message: Some(crate::state::mfa_device_not_found_message(
                        &input.serial_number,
                    )),
                },
            )
            .into());
        };

        // "You must deactivate a user's virtual MFA device before you can delete it": a
        // device a user still holds has a dependent, so it is refused in the class IAM
        // refuses every delete with a blocking dependent in — the fixed sentence naming
        // what has to change (`crate::state::DELETE_CONFLICT_DEACTIVATE_MFA_DEVICE`),
        // naming neither the device nor its holder — and the device is NOT deleted.
        if device.user_key.is_some() {
            return Err(DeleteVirtualMFADeviceErrorModeled::DeleteConflictException(
                DeleteConflictException {
                    message: Some(
                        crate::state::DELETE_CONFLICT_DEACTIVATE_MFA_DEVICE.to_string(),
                    ),
                },
            )
            .into());
        }

        // The record is the whole device — its seed and its tags live in it — so removing
        // it is what makes the serial name nothing again.
        store.virtual_mfa_devices.remove(&input.serial_number);

        ::tracing::info!(
            audit = true,
            operation = "DeleteVirtualMFADevice",
            outcome = "deleted",
            caller_akid = %::engine::extract_akid(&ctx.parts().headers),
            serial_number = %input.serial_number,
            "a virtual MFA device was deleted"
        );

        Ok(DeleteVirtualMFADeviceOutput)
    }
}
