// SPDX-License-Identifier: CC0-1.0

//! @op ResyncMFADevice
//!
//! ## What this operation answers on the wire
//!
//! "Synchronizes the specified MFA device with its IAM resource object on the Amazon Web
//! Services servers" (this operation's documentation): the device is the one the request's
//! `SerialNumber` names — for a virtual device that serial IS its ARN
//! (`VirtualMFADevice$SerialNumber`) — and what the synchronisation records is where that
//! device's clock stands, which the two authentication codes the request presents are the
//! evidence of.
//!
//! The codes are AUTHENTICATED rather than taken on trust: they have to be the pass codes a
//! device provisioned with this device's own seed emits in two consecutive time steps, looked
//! for over the resynchronisation span ([`crate::state::virtual_mfa_device_resync_step`]),
//! which is wider than the allowance a normal authentication is decided within — a device is
//! resynchronised exactly because its clock has drifted out of that one. A pair no step in
//! that span emits synchronises nothing and is refused `InvalidAuthenticationCode` (HTTP
//! 403), leaving the device associated with the user it was associated with.
//!
//! A pass code is a ONE-TIME password, so the pair has to be emitted AFTER the step the last
//! synchronisation recorded ([`crate::state::VirtualMfaDeviceRecord::synchronized_step`]): a
//! second call presenting the pair that already synchronised the device re-presents consumed
//! codes and earns the same refusal, which is what resolves two simultaneous resyncs of one
//! device into one success and one refusal.
//!
//! The output shape is `Unit`, so what the call did is observable through what it changes:
//! the device stays the named user's, and the codes it accepted cannot be presented again.
//!
//! ---
//!
//! # Modeled contract for `ResyncMFADevice`
//!
//! What AWS documents for this operation, from this service's Smithy model. HTML is
//! stripped and long entries are clipped at ` …`. For the FULL text of any one item,
//! ask the model rather than reading the raw JSON:
//!
//! ```text
//! smithy_query(query="doc",shape="<Shape>",member="<Member>",model_path="services/service-com-amazonaws-iam-awsidentitymanagementv20100508/smithy-model/com-amazonaws-iam-awsidentitymanagementv20100508.json")
//! ```
//!
//! Synchronizes the specified MFA device with its IAM resource object on the Amazon Web
//! Services servers. For more information about creating and working with virtual MFA
//! devices, see Using a virtual MFA device in the IAM User Guide.
//!
//! ## Input
//!
//! * `AuthenticationCode1` (authenticationCodeType, len 6..6, pattern ^[\d]+$) **required** —
//!   An authentication code emitted by the device. The format for this parameter is a
//!   sequence of six digits.
//! * `AuthenticationCode2` (authenticationCodeType, len 6..6, pattern ^[\d]+$) **required** —
//!   A subsequent authentication code emitted by the device. The format for this parameter is
//!   a sequence of six digits.
//! * `SerialNumber` (serialNumberType, len 9..256, pattern ^[\w+=/:,.@-]+$) **required** —
//!   Serial number that uniquely identifies the MFA device. This parameter allows (through
//!   its regex pattern) a string of characters consisting of upper and lowercase alphanumeric
//!   characters with no spaces. You can also include any of the following characters: _+=,.@-
//! * `UserName` (existingUserNameType, len 1..128, pattern ^[\w+=,.@-]+$) **required** — The
//!   name of the user whose MFA device you want to resynchronize. This parameter allows
//!   (through its regex pattern) a string of characters consisting of upper and lowercase
//!   alphanumeric characters with no spaces. You can also include any of the following
//!   characters: _+=,.@-
//!
//! ## Errors
//!
//! * `ConcurrentModificationException` (HTTP 409) — The request was rejected because multiple
//!   requests to change this object were submitted simultaneously. Wait a few minutes and
//!   submit your request again.
//! * `InvalidAuthenticationCodeException` (HTTP 403) — The request was rejected because the
//!   authentication code was not recognized. The error message describes the specific error.
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
// `ResyncMFADeviceError` enum is never produced.
smithy_macros::smithy_model!("smithy-model/com-amazonaws-iam-awsidentitymanagementv20100508.json");

#[derive(Debug, Clone, Default)]
pub struct ResyncMFADeviceOutput;

impl ResyncMFADeviceOutput {
    // `with_http` carries this operation's modelled response status. Remove it and
    // the status falls back to the protocol default, so a modelled 204 answers 200.
    pub const SCHEMA: ::aws_smithy_schema::Schema = ::aws_smithy_schema::Schema::new_struct(
        ::aws_smithy_schema::ShapeId::from_static("smithy.api#Unit", "smithy.api", "Unit"),
        ::aws_smithy_schema::ShapeType::Structure,
        &[],
    );
}

pub static RESYNC_MFA_DEVICE_OUTPUT_SCHEMA: &::aws_smithy_schema::Schema = &ResyncMFADeviceOutput::SCHEMA;

impl ::aws_smithy_schema::serde::SerializableStruct for ResyncMFADeviceOutput {
    fn serialize_members(&self, _s: &mut dyn ::aws_smithy_schema::serde::ShapeSerializer) -> Result<(), ::aws_smithy_schema::serde::SerdeError> { Ok(()) }
}

// The operation's MODELED errors. `#[operation_error]` accepts only variants the
// model declares for this operation, so the scaffold's `NotImplemented` cannot live
// here — hence the `ResyncMFADeviceError` alias below. An operation that models no errors
// generates an UNINHABITED enum here, which is correct and expected: there is nothing
// to construct until the model says otherwise.
#[smithy_macros::operation_error("com.amazonaws.iam#ResyncMFADevice")]
#[derive(Debug)]
pub enum ResyncMFADeviceErrorModeled {
    ConcurrentModificationException(ConcurrentModificationException),
    InvalidAuthenticationCodeException(InvalidAuthenticationCodeException),
    LimitExceededException(LimitExceededException),
    NoSuchEntityException(NoSuchEntityException),
    ServiceFailureException(ServiceFailureException),
}

/// This operation's error channel, as the generated `impl Operation` names it
/// (`crate::ops::resync_mfa_device::ResyncMFADeviceError`). Wrapping the modeled enum is what
/// lets a scaffolded operation answer 501 for one whose model declares no errors at all.
pub type ResyncMFADeviceError = crate::scaffolded::Scaffolded<ResyncMFADeviceErrorModeled>;

/// Lets `?` and `.into()` lift a modeled failure into the error channel, so an
/// implemented handler writes `Err(SomeModeledError.into())` rather than naming the
/// wrapper variant. See `crate::scaffolded` for how to drop the wrapper entirely once
/// this operation is implemented.
impl From<ResyncMFADeviceErrorModeled> for ResyncMFADeviceError {
    fn from(modeled: ResyncMFADeviceErrorModeled) -> Self {
        crate::scaffolded::Scaffolded::Modeled(modeled)
    }
}

/// The `ResyncMFADevice` operation.
///
/// Owns the shared engine (the constructor seam) and implements
/// [`smithy_server_runtime::Operation`]. Generic over the engine type `Eng`, so this
/// crate names no concrete engine type: a migrated service that needs state picks a
/// concrete engine where it mounts the service, not here.
pub struct ResyncMFADevice<Eng> {
    engine: ::std::sync::Arc<Eng>,
}

impl<Eng> ResyncMFADevice<Eng> {
    /// Construct the operation from the shared engine. INFALLIBLE — the engine is a
    /// construction-time dependency, so engine access is guaranteed by the time any
    /// request runs. Called by the generated `EmulatedService::new(engine)` constructor.
    pub fn new(engine: ::std::sync::Arc<Eng>) -> Self {
        Self { engine }
    }
}

impl<Eng: ::std::marker::Send + ::std::marker::Sync + 'static> smithy_server_runtime::Operation
    for ResyncMFADevice<Eng>
{
    type Input = ResyncMFADeviceInput;
    type Output = ResyncMFADeviceOutput;
    type Error = ResyncMFADeviceError;

    const NAME: &'static str = "ResyncMFADevice";
    const INPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::RESYNC_MFA_DEVICE_INPUT_SCHEMA;
    const OUTPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = RESYNC_MFA_DEVICE_OUTPUT_SCHEMA;
    const SUCCESS_STATUS: u16 = 200u16;

    async fn handle(
        &self,
        input: Self::Input,
        ctx: &smithy_server_runtime::OperationContext<'_>,
    ) -> Result<Self::Output, Self::Error> {
        let Some(state) = crate::state::service_state(&*self.engine) else {
            return Err(ResyncMFADeviceErrorModeled::ServiceFailureException(
                ServiceFailureException {
                    message: Some("the IAM store is unavailable".to_string()),
                },
            )
            .into());
        };
        // A mutation: the synchronisation it records is written, so the lock is held across
        // the lookup, the authentication and the write — which is also what makes two
        // simultaneous resyncs of one device decide one after the other.
        let mut store = state.lock();

        // IAM names "are not distinguished by case", so the user is found under the
        // case-folded name whatever case the request spelled it in.
        let user_key = crate::state::name_key(&input.user_name);
        let held = store.users.get(&user_key);

        // IAM authorizes its own API: the signing principal's identity-based policies are
        // evaluated before anything is read or written. The decision is made on the ARN of
        // the USER whose device is resynchronised — the held user's own, or, for a name the
        // account holds no user under, the ARN that name would carry at the default path —
        // and a refusal names that user as the request named it, synchronising nothing.
        let resource = held.map_or_else(
            || crate::state::user_arn(crate::state::DEFAULT_PATH, &input.user_name),
            |user| user.arn.clone(),
        );
        crate::state::authorization::authorize_named(
            &store,
            ctx,
            crate::state::authorization::RESYNC_MFA_DEVICE,
            &resource,
            &format!("user {}", input.user_name),
        )?;

        if held.is_none() {
            return Err(ResyncMFADeviceErrorModeled::NoSuchEntityException(
                NoSuchEntityException {
                    message: Some(format!(
                        "The user with name {} cannot be found.",
                        input.user_name
                    )),
                },
            )
            .into());
        }

        // The device the serial number names, among the account's virtual devices: a serial
        // naming none of them references an entity that does not exist, which is what the
        // refusal reports, naming the serial the request supplied.
        let Some(device) = store.virtual_mfa_device(&input.serial_number).cloned() else {
            return Err(ResyncMFADeviceErrorModeled::NoSuchEntityException(
                NoSuchEntityException {
                    message: Some(crate::state::mfa_device_not_found_message(
                        &input.serial_number,
                    )),
                },
            )
            .into());
        };

        // A device is ONE user's, and this call resynchronises "the user whose MFA device you
        // want to resynchronize" (`$UserName`): a device that user does not hold is, for this
        // request, the entity that does not exist — so a device nobody holds and a device
        // another user holds are both refused, and neither is touched.
        if device.user_key.as_deref() != Some(user_key.as_str()) {
            return Err(ResyncMFADeviceErrorModeled::NoSuchEntityException(
                NoSuchEntityException {
                    message: Some(crate::state::mfa_device_not_found_for_user_message(
                        &input.serial_number,
                        &input.user_name,
                    )),
                },
            )
            .into());
        }

        // Where the device's clock stands: the step within the resynchronisation span that
        // emits the presented pair (see the module docs). A pair no step in that span emits
        // is not this device's, and a pair at or before the step the last synchronisation
        // recorded re-presents codes that device has already been synchronised by — a pass
        // code is a one-time password. Either way nothing is written and the device stays the
        // user's.
        let synchronized_step = crate::state::virtual_mfa_device_resync_step(
            &device.base32_string_seed,
            &input.authentication_code1,
            &input.authentication_code2,
            ::std::time::SystemTime::now(),
        )
        // Both codes of the pair are consumed, so what the device is synchronised through is
        // the step the SECOND one was emitted in.
        .map(|step| step.saturating_add(1))
        .filter(|through| device.synchronized_step.is_none_or(|last| *through > last));
        let Some(synchronized_step) = synchronized_step else {
            ::tracing::info!(
                audit = true,
                operation = "ResyncMFADevice",
                outcome = "refused_invalid_authentication_code",
                caller_akid = %::engine::extract_akid(&ctx.parts().headers),
                user_name = %input.user_name,
                serial_number = %input.serial_number,
                "no MFA device was resynchronised"
            );
            return Err(
                ResyncMFADeviceErrorModeled::InvalidAuthenticationCodeException(
                    InvalidAuthenticationCodeException {
                        message: Some(crate::state::INVALID_AUTHENTICATION_CODE.to_string()),
                    },
                )
                .into(),
            );
        };

        // What the account keeps: the device synchronised through that step. The association
        // and the enable date are NOT touched — "synchronizes the specified MFA device with
        // its IAM resource object" changes where the device's clock is known to stand, not
        // whose device it is.
        if let Some(record) = store.virtual_mfa_devices.get_mut(&input.serial_number) {
            record.synchronized_step = Some(synchronized_step);
        }

        ::tracing::info!(
            audit = true,
            operation = "ResyncMFADevice",
            outcome = "resynchronised",
            caller_akid = %::engine::extract_akid(&ctx.parts().headers),
            user_name = %input.user_name,
            serial_number = %input.serial_number,
            "an MFA device was resynchronised"
        );

        Ok(ResyncMFADeviceOutput)
    }
}
