// SPDX-License-Identifier: CC0-1.0

//! The IAM Identity Center dependency an application registration has: the instance
//! the managed application is created in, and the refusal the creation is denied with.
//!
//! `CreateRedshiftIdcApplication` and `CreateQev2IdcApplication` each "create a new
//! managed application" in the IAM Identity Center instance their `IdcInstanceArn`
//! names, and each reports that application back
//! (`RedshiftIdcApplication$IdcManagedApplicationArn`,
//! `Qev2IdcApplication$IdcManagedApplicationArn`) — so the create performs
//! `sso:CreateApplication` on the caller's behalf, in that instance. An IAM Identity
//! Center application is a resource OF its instance, so an instance the account does
//! not hold has no application namespace to create one in: the dependent call is
//! denied, and Amazon Redshift surfaces that denial on the
//! `DependentServiceAccessDeniedFault` channel both operations model ("A dependent
//! service denied access for the integration", HTTP 403), registering nothing.
//!
//! The denial was answered by that OTHER service, so it is reported as a relayed refusal:
//! the downstream sentence verbatim, then the envelope the SDK layer that relayed the call
//! states around a failed call (which service answered, with what status, under which
//! request id, after how many attempts) — the one form the whole workspace renders
//! ([`::service_common::relayed_refusal`]).
//!
//! WHICH instances the account holds is the owning service's own rule, asked here
//! through its read-only reading of it
//! ([`::service_com_amazonaws_ssoadmin_swbexternalservice::state::ComAmazonawsSsoadminSwbexternalserviceState::holds_instance`]),
//! so nothing about that rule is restated in this crate and a caller that creates (or
//! deletes) an instance changes the next answer. Asking is a READ: it enables no
//! instance and decides nothing for the IAM Identity Center service.
//!
//! Nothing is refused on an assumption: an emulator with no IAM Identity Center to ask
//! answers nothing about the account's instances, so it refuses nothing — the same
//! reading [`super::serverless`] takes of a peer that never answered.

/// The IAM Identity Center action an application registration performs on the caller's
/// behalf, as IAM names it.
pub const SSO_CREATE_APPLICATION: &str = "sso:CreateApplication";

/// The dependent service the nested create is made TO, named the way the SDK layer that
/// relayed the call reports it — which is neither that endpoint's signing name (`sso`)
/// nor the casing of its `sdkId`.
pub const IDENTITY_CENTER_SERVICE: &str = "SsoAdmin";

/// The `@httpError` the dependent service's own `CreateApplication` binds to the channel
/// it reports an `InstanceArn` that resolves to no instance of the account on
/// (`smithy_query(query="operation",operation="CreateApplication",model_path="services/service-com-amazonaws-ssoadmin-swbexternalservice/smithy-model/com-amazonaws-ssoadmin-swbexternalservice.json")`
/// — `ResourceNotFoundException`, `@httpError` 404), which is the refusal the nested call
/// behind this denial came back with.
const INSTANCE_UNRESOLVED_MODELED_STATUS: u16 = 404;

/// The HTTP status the dependent call was answered with, as the envelope reports it.
///
/// The status is a fact about the ANSWER the called endpoint gave, so it is read from
/// that service's own statement of what it sends for a modelled refusal
/// ([`::service_com_amazonaws_ssoadmin_swbexternalservice::scaffolded::modeled_error_status`])
/// rather than restated here: that endpoint is awsJson1_1 and does not send every shape's
/// own `@httpError`, so a status guessed from the model would name a status it never
/// answers with.
fn identity_center_refusal_status() -> u16 {
    ::service_com_amazonaws_ssoadmin_swbexternalservice::scaffolded::modeled_error_status(
        INSTANCE_UNRESOLVED_MODELED_STATUS,
    )
}

/// The partition an IAM Identity Center resource is named under when the value the
/// request named is no ARN to read one off. Every ARN this emulator emits carries it.
const DEFAULT_PARTITION: &str = "aws";

/// The segment an IAM Identity Center INSTANCE ARN carries before its instance id
/// (`arn:aws:sso:::instance/ssoins-…`).
const INSTANCE_SEGMENT: &str = ":instance/";

/// The IAM principal every request to this emulator speaks for — the one the account
/// holds and `sts:GetCallerIdentity` reports — which is the principal an authorization
/// refusal names.
///
/// Read from the shared definition, so the principal this service is refused in the
/// name of is the same one every other service of this emulator names, and the same one
/// a client that reads its own identity is given.
pub fn caller_principal() -> String {
    ::service_common::caller_principal::ambient_user_arn()
}

/// The IAM resource an `sso:CreateApplication` is evaluated against: the APPLICATION
/// namespace of one IAM Identity Center instance,
/// `arn:<partition>:sso::*:application/<instance id>/*`.
///
/// An IAM Identity Center application is a resource of its instance, so the instance's
/// id is part of the resource's name; the account and the application id are the IAM
/// wildcards the authorization is evaluated over, since the application has no id until
/// the call that would create it succeeds. The instance id is read off the ARN the
/// request named — an instance ARN carries it after its `:instance/` segment — so
/// nothing here is derived from a recording.
pub fn identity_center_application_resource(instance_arn: &str) -> String {
    let partition = ::service_common::arn::parse(instance_arn)
        .map(|arn| arn.partition)
        .unwrap_or(DEFAULT_PARTITION);
    let instance = instance_arn
        .split_once(INSTANCE_SEGMENT)
        .map(|(_, instance)| instance)
        .unwrap_or(instance_arn);
    format!("arn:{partition}:sso::*:application/{instance}/*")
}

/// Whether the account's IAM Identity Center ANSWERED that `instance_arn` names no
/// instance of this account.
///
/// `false` — refuse nothing — when nothing is KNOWN about the account's instances: an
/// emulator the IAM Identity Center service is not mounted on cannot show the instance
/// to be absent.
pub fn identity_center_instance_is_absent<Eng: Send + Sync + 'static>(
    engine: &Eng,
    instance_arn: &str,
) -> bool {
    use ::service_com_amazonaws_ssoadmin_swbexternalservice::state as identity_center;

    let Some(state) = identity_center::service_state(engine) else {
        return false;
    };
    let held = state.lock().holds_instance(instance_arn);
    !held
}

/// The message an application registration is denied with when the
/// `sso:CreateApplication` it performs on the caller's behalf is denied, or `None` when
/// nothing denies it.
///
/// The one condition this service can SHOW: the instance the request names is not one
/// the account's IAM Identity Center holds, so there is no application namespace to
/// create the managed application in. The sentence is the standard IAM
/// authorization-failure one, from the single shared statement of it
/// ([`::service_common::iam_authorization::not_authorized_by_resource_policy`]) rather
/// than a copy, so two services that reach the same decision word it the same way: the
/// principal that called, the action performed on its behalf, and the application
/// namespace of the instance the request named.
///
/// The refusal was answered by the DEPENDENT service, not by this one, so the sentence is
/// carried through inside the envelope the SDK layer that relayed the call states around a
/// failed call — the service that answered, the status it answered with, that call's own
/// request id and how many attempts it took — rendered by the workspace's one writer of
/// that envelope ([`::service_common::relayed_refusal`]). Without it a client cannot tell
/// which service refused, read the downstream status or quote a request id, and tooling
/// that parses the dependent-exception tail reads the refusal as this service's own.
/// The request id belongs to the relayed CALL, so it is minted per call and nothing in the
/// request decides it.
pub fn sso_create_application_denial<Eng: Send + Sync + 'static>(
    engine: &Eng,
    instance_arn: &str,
) -> Option<String> {
    if !identity_center_instance_is_absent(engine, instance_arn) {
        return None;
    }
    let reason = ::service_common::iam_authorization::not_authorized_by_resource_policy(
        &caller_principal(),
        SSO_CREATE_APPLICATION,
        &identity_center_application_resource(instance_arn),
    );
    Some(::service_common::relayed_refusal::relayed_refusal_message(
        &reason,
        ::service_common::relayed_refusal::RelayedCall::once(
            IDENTITY_CENTER_SERVICE,
            identity_center_refusal_status(),
        ),
    ))
}
