// SPDX-License-Identifier: CC0-1.0

//! @op DeactivateMFADevice
//!
//! ## What this operation answers on the wire
//!
//! "Deactivates the specified MFA device and removes it from association with the user name
//! for which it was originally enabled" (this operation's documentation): the association
//! `EnableMFADevice` wrote is cleared, so the device is one no user holds again — free to be
//! enabled for a user or deleted — and the user is one no device is enabled for.
//!
//! "If no user name is included, it defaults to the principal making the request", resolved
//! in the one place this service resolves an absent `UserName`. A serial the account holds no
//! device under, and a device the named user does not hold, are both the entity that does not
//! exist (`NoSuchEntityException`, HTTP 404), and neither deactivates anything.
//!
//! The USER is resolved first, as every operation addressing one of a user's own sub-resources
//! resolves it (`EnableMFADevice`, `ResyncMFADevice`, `ListMFADevices`, the access-key and
//! certificate writes): a name the account holds no user under names no device-holder either,
//! so "the error message describes the resource" (`NoSuchEntityException`) describes the USER,
//! and the device-scoped sentence is kept for the one case it is true of — the user is held and
//! the serial is not one of that user's devices.
//!
//! The serial is resolved among the account's VIRTUAL MFA devices, which is every device a
//! caller can have enabled: a FIDO security key is enrolled by the key itself through the
//! browser's WebAuthn ceremony, and IAM's API binds no operation that enrolls one, so no
//! serial reaching this handler names a security key this emulator could hold. Should one be
//! enrollable, this lookup is what has to span both kinds — the projection that already does
//! is [`crate::state::ComAmazonawsIamAwsidentitymanagementv20100508State::mfa_devices_of_user`].
//!
//! ---
//!
//! # Modeled contract for `DeactivateMFADevice`
//!
//! What AWS documents for this operation, from this service's Smithy model. HTML is
//! stripped and long entries are clipped at ` …`. For the FULL text of any one item,
//! ask the model rather than reading the raw JSON:
//!
//! ```text
//! smithy_query(query="doc",shape="<Shape>",member="<Member>",model_path="services/service-com-amazonaws-iam-awsidentitymanagementv20100508/smithy-model/com-amazonaws-iam-awsidentitymanagementv20100508.json")
//! ```
//!
//! Deactivates the specified MFA device and removes it from association with the user name
//! for which it was originally enabled. For more information about creating and working
//! with virtual MFA devices, see Enabling a virtual multi-factor authentication (MFA)
//! device in the IAM User Guide.
//!
//! ## Input
//!
//! * `SerialNumber` (serialNumberType, len 9..256, pattern ^[\w+=/:,.@-]+$) **required** —
//!   The serial number that uniquely identifies the MFA device. For virtual MFA devices, the
//!   serial number is the device ARN. This parameter allows (through its regex pattern) a
//!   string of characters consisting of upper and lowercase alphanumeric characters with no
//!   spaces. You can also include any of the following characters: =,.@:/-
//! * `UserName` (existingUserNameType, len 1..128, pattern ^[\w+=,.@-]+$) — The name of the
//!   user whose MFA device you want to deactivate. This parameter is optional. If no user
//!   name is included, it defaults to the principal making the request. When you make this
//!   request with root user credentials, you must use an AssumeRoot session to omit the user
//!   name. This parameter allows (through its regex pattern) a string of characters
//!   consisting of upper and lowercase alphanumeric characters with no spaces. You can also
//!   include any of the following characters: _+=,.@-
//!
//! ## Errors
//!
//! * `ConcurrentModificationException` (HTTP 409) — The request was rejected because multiple
//!   requests to change this object were submitted simultaneously. Wait a few minutes and
//!   submit your request again.
//! * `EntityTemporarilyUnmodifiableException` (HTTP 409) — The request was rejected because
//!   it referenced an entity that is temporarily unmodifiable, such as a user name that was
//!   deleted and then recreated. The error indicates that the request is likely to succeed if
//!   you try again after waiting several minutes. The error message describes the entity.
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
// `DeactivateMFADeviceError` enum is never produced.
smithy_macros::smithy_model!("smithy-model/com-amazonaws-iam-awsidentitymanagementv20100508.json");

#[derive(Debug, Clone, Default)]
pub struct DeactivateMFADeviceOutput;

impl DeactivateMFADeviceOutput {
    // `with_http` carries this operation's modelled response status. Remove it and
    // the status falls back to the protocol default, so a modelled 204 answers 200.
    pub const SCHEMA: ::aws_smithy_schema::Schema = ::aws_smithy_schema::Schema::new_struct(
        ::aws_smithy_schema::ShapeId::from_static("smithy.api#Unit", "smithy.api", "Unit"),
        ::aws_smithy_schema::ShapeType::Structure,
        &[],
    );
}

pub static DEACTIVATE_MFA_DEVICE_OUTPUT_SCHEMA: &::aws_smithy_schema::Schema = &DeactivateMFADeviceOutput::SCHEMA;

impl ::aws_smithy_schema::serde::SerializableStruct for DeactivateMFADeviceOutput {
    fn serialize_members(&self, _s: &mut dyn ::aws_smithy_schema::serde::ShapeSerializer) -> Result<(), ::aws_smithy_schema::serde::SerdeError> { Ok(()) }
}

// The operation's MODELED errors. `#[operation_error]` accepts only variants the
// model declares for this operation, so the scaffold's `NotImplemented` cannot live
// here — hence the `DeactivateMFADeviceError` alias below. An operation that models no errors
// generates an UNINHABITED enum here, which is correct and expected: there is nothing
// to construct until the model says otherwise.
#[smithy_macros::operation_error("com.amazonaws.iam#DeactivateMFADevice")]
#[derive(Debug)]
pub enum DeactivateMFADeviceErrorModeled {
    ConcurrentModificationException(ConcurrentModificationException),
    EntityTemporarilyUnmodifiableException(EntityTemporarilyUnmodifiableException),
    LimitExceededException(LimitExceededException),
    NoSuchEntityException(NoSuchEntityException),
    ServiceFailureException(ServiceFailureException),
}

/// This operation's error channel, as the generated `impl Operation` names it
/// (`crate::ops::deactivate_mfa_device::DeactivateMFADeviceError`). Wrapping the modeled enum is what
/// lets a scaffolded operation answer 501 for one whose model declares no errors at all.
pub type DeactivateMFADeviceError = crate::scaffolded::Scaffolded<DeactivateMFADeviceErrorModeled>;

/// Lets `?` and `.into()` lift a modeled failure into the error channel, so an
/// implemented handler writes `Err(SomeModeledError.into())` rather than naming the
/// wrapper variant. See `crate::scaffolded` for how to drop the wrapper entirely once
/// this operation is implemented.
impl From<DeactivateMFADeviceErrorModeled> for DeactivateMFADeviceError {
    fn from(modeled: DeactivateMFADeviceErrorModeled) -> Self {
        crate::scaffolded::Scaffolded::Modeled(modeled)
    }
}

/// The `DeactivateMFADevice` operation.
///
/// Owns the shared engine (the constructor seam) and implements
/// [`smithy_server_runtime::Operation`]. Generic over the engine type `Eng`, so this
/// crate names no concrete engine type: a migrated service that needs state picks a
/// concrete engine where it mounts the service, not here.
pub struct DeactivateMFADevice<Eng> {
    engine: ::std::sync::Arc<Eng>,
}

impl<Eng> DeactivateMFADevice<Eng> {
    /// Construct the operation from the shared engine. INFALLIBLE — the engine is a
    /// construction-time dependency, so engine access is guaranteed by the time any
    /// request runs. Called by the generated `EmulatedService::new(engine)` constructor.
    pub fn new(engine: ::std::sync::Arc<Eng>) -> Self {
        Self { engine }
    }
}

impl<Eng: ::std::marker::Send + ::std::marker::Sync + 'static> smithy_server_runtime::Operation
    for DeactivateMFADevice<Eng>
{
    type Input = DeactivateMFADeviceInput;
    type Output = DeactivateMFADeviceOutput;
    type Error = DeactivateMFADeviceError;

    const NAME: &'static str = "DeactivateMFADevice";
    const INPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::DEACTIVATE_MFA_DEVICE_INPUT_SCHEMA;
    const OUTPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = DEACTIVATE_MFA_DEVICE_OUTPUT_SCHEMA;
    const SUCCESS_STATUS: u16 = 200u16;

    async fn handle(
        &self,
        input: Self::Input,
        ctx: &smithy_server_runtime::OperationContext<'_>,
    ) -> Result<Self::Output, Self::Error> {
        let Some(state) = crate::state::service_state(&*self.engine) else {
            return Err(DeactivateMFADeviceErrorModeled::ServiceFailureException(
                ServiceFailureException {
                    message: Some("the IAM store is unavailable".to_string()),
                },
            )
            .into());
        };
        // A mutation: the association is cleared, so the lock is held across the lookup and
        // the write.
        let mut store = state.lock();

        // Whose device: the named user, or — with no `UserName` — the principal making the
        // request, resolved in the one place this service resolves an absent `UserName`
        // (`crate::state::authorization::caller_identity`) and refused the same way when the
        // signing credential names no IAM user of this account.
        let user_key = match &input.user_name {
            Some(user_name) => crate::state::name_key(user_name),
            None => {
                match crate::state::authorization::caller_identity::implicit_user_key(&store, ctx) {
                    Some(user_key) => user_key,
                    None => return Err(crate::state::must_specify_user_name()),
                }
            }
        };
        let named_user = input.user_name.clone().unwrap_or_else(|| {
            // A request that named no user is reported with the spelling the account holds
            // the user under, since IAM names "are not distinguished by case" and the
            // resolved key is case-folded; a credential whose user the account no longer
            // holds leaves that key as the only name there is.
            store
                .users
                .get(&user_key)
                .map_or_else(|| user_key.clone(), |user| user.user_name.clone())
        });

        // IAM authorizes its own API: the caller's identity-based policies are evaluated
        // before anything is read or written, on the ARN of the user the device is
        // deactivated for — the held user's own, or, for a name the account holds no user
        // under, the ARN that name would carry at the default path. A refusal names that
        // user as the request named it and deactivates nothing.
        let resource = store.users.get(&user_key).map_or_else(
            || crate::state::user_arn(crate::state::DEFAULT_PATH, &named_user),
            |user| user.arn.clone(),
        );
        crate::state::authorization::authorize_named(
            &store,
            ctx,
            crate::state::authorization::DEACTIVATE_MFA_DEVICE,
            &resource,
            &format!("user {named_user}"),
        )?;

        // The association cleared is one an IAM user holds, so the USER is resolved before the
        // serial is: a name the account holds no user under holds no device either, and the
        // entity that does not exist is that user — reported in this service's one wording for
        // an absent user, naming it as the request named it. Only once the user IS held can a
        // refusal be about the device.
        if !store.users.contains_key(&user_key) {
            return Err(DeactivateMFADeviceErrorModeled::NoSuchEntityException(
                NoSuchEntityException {
                    message: Some(format!("The user with name {named_user} cannot be found.")),
                },
            )
            .into());
        }

        // The device the serial number names, among the account's virtual devices: a serial
        // naming none of them references an entity that does not exist, which is what the
        // refusal reports, naming the serial the request supplied.
        let Some(device) = store.virtual_mfa_device(&input.serial_number) else {
            return Err(DeactivateMFADeviceErrorModeled::NoSuchEntityException(
                NoSuchEntityException {
                    message: Some(crate::state::mfa_device_not_found_message(
                        &input.serial_number,
                    )),
                },
            )
            .into());
        };

        // The association removed is the one "for which it was originally enabled": a device
        // the named user does not hold is, for this request, the entity that does not exist
        // — so a device nobody holds and a device another user holds are both refused, and
        // neither is touched. What this call addresses is the ENROLMENT rather than the
        // device, so the absence of one is reported in this operation's own fixed sentence
        // (`crate::state::MFA_DEVICE_INVALID_FOR_USER`), which quotes neither the serial nor
        // the user back.
        if device.user_key.as_deref() != Some(user_key.as_str()) {
            return Err(DeactivateMFADeviceErrorModeled::NoSuchEntityException(
                NoSuchEntityException {
                    message: Some(crate::state::MFA_DEVICE_INVALID_FOR_USER.to_string()),
                },
            )
            .into());
        }

        // What the account keeps: the device with no holder and no enable instant — "the
        // date and time on which the virtual MFA device was enabled"
        // (`VirtualMFADevice$EnableDate`) is a fact about an association this call removed,
        // so a device nobody holds reports none, exactly as one that was never enabled does.
        if let Some(record) = store.virtual_mfa_devices.get_mut(&input.serial_number) {
            record.user_key = None;
            record.enable_date = None;
        }

        ::tracing::info!(
            audit = true,
            operation = "DeactivateMFADevice",
            outcome = "deactivated",
            caller_akid = %::engine::extract_akid(&ctx.parts().headers),
            user_name = %named_user,
            serial_number = %input.serial_number,
            "an MFA device was deactivated"
        );

        Ok(DeactivateMFADeviceOutput)
    }
}
