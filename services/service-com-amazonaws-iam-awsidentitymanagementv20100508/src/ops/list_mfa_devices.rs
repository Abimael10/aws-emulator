// SPDX-License-Identifier: CC0-1.0

//! @op ListMFADevices
//!
//! ## What this operation answers on the wire
//!
//! A READ: "lists the MFA devices for an IAM user" — every device the named user holds,
//! whatever KIND it is, which the account keeps as virtual MFA devices and as FIDO security
//! keys ([`crate::state::ComAmazonawsIamAwsidentitymanagementv20100508State::mfa_devices_of_user`]
//! is the one projection over both) — reported in ascending order of the serial each device
//! is addressed by, paged by `MaxItems` and `Marker`.
//!
//! "If you do not specify a user name, IAM determines the user name implicitly based on the
//! Amazon Web Services access key ID signing the request", resolved in the one place this
//! service resolves an absent `UserName`. A name the account holds no user under names no
//! user whose devices could be reported, which is the one modelled client error this
//! operation declares (`NoSuchEntityException`, HTTP 404).
//!
//! ---
//!
//! # Modeled contract for `ListMFADevices`
//!
//! What AWS documents for this operation, from this service's Smithy model. HTML is
//! stripped and long entries are clipped at ` …`. For the FULL text of any one item,
//! ask the model rather than reading the raw JSON:
//!
//! ```text
//! smithy_query(query="doc",shape="<Shape>",member="<Member>",model_path="services/service-com-amazonaws-iam-awsidentitymanagementv20100508/smithy-model/com-amazonaws-iam-awsidentitymanagementv20100508.json")
//! ```
//!
//! Lists the MFA devices for an IAM user. If the request includes a IAM user name, then
//! this operation lists all the MFA devices associated with the specified user. If you do
//! not specify a user name, IAM determines the user name implicitly based on the Amazon Web
//! Services access key ID signing the request for this operation. You can paginate the
//! results using the MaxItems and Marker parameters.
//!
//! ## Input
//!
//! * `Marker` (markerType, len 1..320, pattern ^[\u0020-\u00FF]+$) — Use this parameter only
//!   when paginating results and only after you receive a response indicating that the
//!   results are truncated. Set it to the value of the Marker element in the response that
//!   you received to indicate where the next call should start.
//! * `MaxItems` (maxItemsType, range 1..1000) — Use this only when paginating results to
//!   indicate the maximum number of items you want in the response. If additional items exist
//!   beyond the maximum you specify, the IsTruncated response element is true. If you do not
//!   include this parameter, the number of items defaults to 100. Note that IAM might return
//!   fewer results, even when there are more results available. In that case, the IsTruncated
//!   response element returns true, and Marker contains a value to include in the subsequent
//!   call that tells the service where to continue from.
//! * `UserName` (existingUserNameType, len 1..128, pattern ^[\w+=,.@-]+$) — The name of the
//!   user whose MFA devices you want to list. This parameter allows (through its regex
//!   pattern) a string of characters consisting of upper and lowercase alphanumeric
//!   characters with no spaces. You can also include any of the following characters: _+=,.@-
//!
//! ## Output
//!
//! * `IsTruncated` (booleanType, default false) **required** — A flag that indicates whether
//!   there are more items to return. If your results were truncated, you can make a
//!   subsequent pagination request using the Marker request parameter to retrieve more items.
//!   Note that IAM might return fewer than the MaxItems number of results even when there are
//!   more results available. We recommend that you check IsTruncated after every call to
//!   ensure that you receive all your results.
//! * `MFADevices` (mfaDeviceListType) **required** — A list of MFA devices.
//! * `Marker` (responseMarkerType) — When IsTruncated is true, this element is present and
//!   contains the value to use for the Marker parameter in a subsequent pagination request.
//!
//! ## Errors
//!
//! * `NoSuchEntityException` (HTTP 404) — The request was rejected because it referenced a
//!   resource entity that does not exist. The error message describes the resource.
//! * `ServiceFailureException` (HTTP 500) — The request processing has failed because of an
//!   unknown error, exception or failure.

#[allow(unused_imports)]
use super::types::*;

// `#[operation_error]` below expands to an invocation of the LOCAL declarative
// `smithy!` macro, which this line is what emits into the module. Without it the
// expansion fails with `cannot find macro 'smithy' in this scope` and the
// `ListMFADevicesError` enum is never produced.
smithy_macros::smithy_model!("smithy-model/com-amazonaws-iam-awsidentitymanagementv20100508.json");

// The operation's MODELED errors. `#[operation_error]` accepts only variants the
// model declares for this operation, so the scaffold's `NotImplemented` cannot live
// here — hence the `ListMFADevicesError` alias below. An operation that models no errors
// generates an UNINHABITED enum here, which is correct and expected: there is nothing
// to construct until the model says otherwise.
#[smithy_macros::operation_error("com.amazonaws.iam#ListMFADevices")]
#[derive(Debug)]
pub enum ListMFADevicesErrorModeled {
    NoSuchEntityException(NoSuchEntityException),
    ServiceFailureException(ServiceFailureException),
}

/// This operation's error channel, as the generated `impl Operation` names it
/// (`crate::ops::list_mfa_devices::ListMFADevicesError`). Wrapping the modeled enum is what
/// lets a scaffolded operation answer 501 for one whose model declares no errors at all.
pub type ListMFADevicesError = crate::scaffolded::Scaffolded<ListMFADevicesErrorModeled>;

/// Lets `?` and `.into()` lift a modeled failure into the error channel, so an
/// implemented handler writes `Err(SomeModeledError.into())` rather than naming the
/// wrapper variant. See `crate::scaffolded` for how to drop the wrapper entirely once
/// this operation is implemented.
impl From<ListMFADevicesErrorModeled> for ListMFADevicesError {
    fn from(modeled: ListMFADevicesErrorModeled) -> Self {
        crate::scaffolded::Scaffolded::Modeled(modeled)
    }
}

/// The `ListMFADevices` operation.
///
/// Owns the shared engine (the constructor seam) and implements
/// [`smithy_server_runtime::Operation`]. Generic over the engine type `Eng`, so this
/// crate names no concrete engine type: a migrated service that needs state picks a
/// concrete engine where it mounts the service, not here.
pub struct ListMFADevices<Eng> {
    engine: ::std::sync::Arc<Eng>,
}

impl<Eng> ListMFADevices<Eng> {
    /// Construct the operation from the shared engine. INFALLIBLE — the engine is a
    /// construction-time dependency, so engine access is guaranteed by the time any
    /// request runs. Called by the generated `EmulatedService::new(engine)` constructor.
    pub fn new(engine: ::std::sync::Arc<Eng>) -> Self {
        Self { engine }
    }
}

impl<Eng: ::std::marker::Send + ::std::marker::Sync + 'static> smithy_server_runtime::Operation
    for ListMFADevices<Eng>
{
    type Input = ListMFADevicesInput;
    type Output = ListMFADevicesOutput;
    type Error = ListMFADevicesError;

    const NAME: &'static str = "ListMFADevices";
    const INPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::LIST_MFA_DEVICES_INPUT_SCHEMA;
    const OUTPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::LIST_MFA_DEVICES_OUTPUT_SCHEMA;
    const SUCCESS_STATUS: u16 = 200u16;

    async fn handle(
        &self,
        input: Self::Input,
        ctx: &smithy_server_runtime::OperationContext<'_>,
    ) -> Result<Self::Output, Self::Error> {
        let Some(state) = crate::state::service_state(&*self.engine) else {
            return Err(ListMFADevicesErrorModeled::ServiceFailureException(
                ServiceFailureException {
                    message: Some("the IAM store is unavailable".to_string()),
                },
            )
            .into());
        };

        // The page resumes strictly after the POSITION in the device order the marker
        // names, so a token this service's encoding cannot have produced is refused rather
        // than silently read as "start from the beginning" — the rule every listing of this
        // service follows. A token that decodes but names a device deactivated since the
        // previous page is NOT an error: the position still orders the remainder.
        let resume_after = match &input.marker {
            Some(marker) => {
                match crate::state::decode_marker(crate::state::MARKER_SCOPE_MFA_DEVICES, marker)
                {
                    Some(key) => Some(key),
                    None => {
                        return Err(crate::scaffolded::Scaffolded::validation_error(
                            crate::state::INVALID_MARKER,
                        ));
                    }
                }
            }
            None => None,
        };
        // `maxItemsType` is range 1..1000 and the boundary validator has already refused
        // anything outside it, so the only choice left here is the default for an absent
        // `MaxItems`: "if you do not include this parameter, the number of items defaults
        // to 100". `.max(1)` is what makes the cast lossless.
        let page_size = input
            .max_items
            .unwrap_or(crate::state::DEFAULT_MAX_ITEMS)
            .max(1) as usize;

        // A read: it "lists the MFA devices for an IAM user" and writes nothing.
        let store = state.lock();

        // Whose devices: the named user, or — with no `UserName` — "the user name is
        // determined implicitly based on the Amazon Web Services access key ID signing the
        // request", resolved in the one place this service resolves an absent `UserName`
        // (`crate::state::authorization::caller_identity`), exactly as
        // `ListSigningCertificates` resolves it, and refused the same way when the signing
        // credential names no IAM user of this account.
        let user_key = match &input.user_name {
            Some(user_name) => crate::state::name_key(user_name),
            None => {
                match crate::state::authorization::caller_identity::implicit_user_key(&store, ctx) {
                    Some(user_key) => user_key,
                    None => return Err(crate::state::must_specify_user_name()),
                }
            }
        };

        // IAM authorizes its own API: the caller's identity-based policies are evaluated
        // before any device is reported. The decision is made on the ARN of the user the
        // devices belong to — the held user's own ARN, or, for a name the account holds no
        // user under, the ARN that name would carry at the default path — and a refusal
        // names that user as the REQUEST named it. A request that named none is reported
        // with the spelling the account holds the user under, since IAM names "are not
        // distinguished by case" and the resolved key is case-folded; a credential whose
        // user the account no longer holds leaves that key as the only name there is.
        let named_user = input.user_name.clone().unwrap_or_else(|| {
            store
                .users
                .get(&user_key)
                .map_or_else(|| user_key.clone(), |user| user.user_name.clone())
        });
        let resource = store.users.get(&user_key).map_or_else(
            || crate::state::user_arn(crate::state::DEFAULT_PATH, &named_user),
            |user| user.arn.clone(),
        );
        crate::state::authorization::authorize_named(
            &store,
            ctx,
            crate::state::authorization::LIST_MFA_DEVICES,
            &resource,
            &format!("user {named_user}"),
        )?;

        // The devices reported are those of an IAM user the account holds: a name the
        // account does not hold names no user whose devices could be reported.
        let Some(user) = store.users.get(&user_key) else {
            return Err(ListMFADevicesErrorModeled::NoSuchEntityException(
                NoSuchEntityException {
                    message: Some(format!("The user with name {named_user} cannot be found.")),
                },
            )
            .into());
        };
        // Reported in the case the user was created with, not the case the request happened
        // to spell, since IAM names "are not distinguished by case".
        let user_name = user.user_name.clone();

        let held = store.mfa_devices_of_user(&user_key, &user_name);
        let tail = held.into_iter().filter(|device| {
            resume_after
                .as_deref()
                .is_none_or(|key| device.serial_number.as_str() > key)
        });

        let mut mfa_devices: Vec<MFADevice> = Vec::new();
        let mut is_truncated = false;
        let mut last_key = None;
        for device in tail {
            if mfa_devices.len() == page_size {
                // There is at least one more device of this user, so the caller needs
                // another page to see it.
                is_truncated = true;
                break;
            }
            last_key = Some(device.serial_number.clone());
            mfa_devices.push(device);
        }

        Ok(ListMFADevicesOutput {
            // The member is required, and empty is the answer for a user holding no MFA
            // device at all.
            mfa_devices,
            marker: is_truncated
                .then(|| {
                    last_key.as_deref().map(|key| {
                        crate::state::encode_marker(crate::state::MARKER_SCOPE_MFA_DEVICES, key)
                    })
                })
                .flatten(),
            is_truncated,
        })
    }
}
