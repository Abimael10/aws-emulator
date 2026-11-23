// SPDX-License-Identifier: CC0-1.0

//! The cross-member preconditions a user-pool write has to satisfy before the
//! pool is stored.
//!
//! Each rule below holds over the configuration a request would LEAVE the pool
//! with, not over one member in isolation, so the operations that write a pool —
//! `CreateUserPool` and `UpdateUserPool` — ask the same question of the same
//! values and answer it with the same sentence.

use super::*;

/// The message a user-pool write is refused with when it selects `phone_number`
/// for auto-verification while the pool would have no SMS delivery path
/// ([`sms_configuration_missing_for_auto_verification`]).
///
/// Fixed text: it names neither the pool nor the attribute list the request
/// carried, since the member and the setting it depends on are the same two for
/// every such request.
pub const SMS_CONFIGURATION_REQUIRED_MESSAGE: &str =
    "SMS configuration is required when phone_number is selected for auto verification";

/// The `VerifiedAttributeType` value whose verification message is delivered over
/// SMS (the other member of that enumeration, `email`, is delivered by email —
/// `smithy_query(query="shape",shape="VerifiedAttributeType")`).
pub const PHONE_NUMBER_VERIFIED_ATTRIBUTE: &str = "phone_number";

/// Whether the configuration a write would leave the pool with asks Amazon
/// Cognito to verify a phone number with no way to send the message.
///
/// A phone number is verified by a code delivered over SMS, and a text message
/// leaves a pool only over one of the two paths a pool can configure for it
/// ([`names_an_sms_delivery_path`]), so auto-verifying `phone_number` without
/// either asks for a verification nothing could deliver. Every argument is the
/// EFFECTIVE value of the request — what the pool would hold once it is written —
/// so the configuration is judged as the caller left it, whichever operation
/// wrote it.
#[must_use]
pub fn sms_configuration_missing_for_auto_verification(
    auto_verified_attributes: &[String],
    sms_configuration: Option<&SmsConfigurationType>,
    lambda_config: Option<&LambdaConfigType>,
) -> bool {
    auto_verified_attributes
        .iter()
        .any(|attribute| attribute == PHONE_NUMBER_VERIFIED_ATTRIBUTE)
        && !names_an_sms_delivery_path(sms_configuration, lambda_config)
}

/// The message a user-pool write is refused with when its `LambdaConfig`
/// configures a custom sender trigger without the key the codes it hands over
/// are encrypted under ([`custom_sender_without_kms_key`]).
///
/// Fixed text: it names the member that is missing and the trigger family that
/// needs it, which is the same pair for every such request, and neither the pool
/// nor the function the trigger names.
pub const CUSTOM_SENDER_KMS_KEY_REQUIRED_MESSAGE: &str =
    "KMSKeyId parameter is required when Custom SMS/Email Lambda trigger is used.";

/// Whether the `LambdaConfig` a write would leave the pool with routes messages
/// to a custom sender trigger while naming no key to encrypt them under.
///
/// A custom sender trigger is handed the message instead of the pool sending it
/// — "this trigger routes all SMS notifications from a user pool to a Lambda
/// function that delivers the message using custom logic"
/// (`LambdaConfigType$CustomSMSSender`, and `CustomEmailSender` for email) — and
/// the code it is handed travels encrypted under the key the same configuration
/// names: `LambdaConfigType$KMSKeyID` is "the ARN of an KMS key. Amazon Cognito
/// uses the key to encrypt codes and temporary passwords sent to custom sender
/// Lambda triggers" (this service's model). So EITHER custom sender without that
/// key is a configuration whose trigger could never be handed a code at all
/// ([`crate::state::lambda_triggers`] refuses the invocation for exactly that
/// reason), and the write is refused rather than stored. A key member carrying
/// the empty string names no key, exactly as an absent one does.
///
/// The argument is the EFFECTIVE value of the request — what the pool would hold
/// once it is written — so the configuration is judged as the caller left it,
/// whichever operation wrote it.
#[must_use]
pub fn custom_sender_without_kms_key(lambda_config: &LambdaConfigType) -> bool {
    let configures_custom_sender = lambda_config.custom_sms_sender.is_some()
        || lambda_config.custom_email_sender.is_some();
    configures_custom_sender
        && lambda_config
            .kms_key_id
            .as_deref()
            .is_none_or(str::is_empty)
}

/// The message a user-pool write is refused with when it requires or offers MFA
/// without the SMS delivery path and phone-number verification that factor rests
/// on ([`mfa_without_sms_delivery`]).
///
/// Fixed text: it names the configuration the request is missing, which is the
/// same pair for every such request, and neither the pool nor the mode asked for.
pub const MFA_SMS_CONFIGURATION_REQUIRED_MESSAGE: &str =
    "SMS configuration and Auto verification for phone_number are required when MFA is \
     required/optional";

/// Whether the configuration a write would leave the pool with turns MFA on
/// without the configuration the factor it would challenge over needs.
///
/// "The `CreateUserPool` operation supports only SMS MFA configuration. If you set
/// `MfaConfiguration` to either of these values, include an `SmsConfiguration` in
/// the same request … If you omit `SmsConfiguration`, the operation returns an
/// `InvalidParameterException`" (`CreateUserPoolRequest.MfaConfiguration` in this
/// service's model) — a pool that writes its MFA mode through this surface can
/// challenge over SMS alone, so it takes both halves of that delivery path: a
/// caller to send the message through ([`names_an_sms_caller`]) and the phone
/// number auto-verified, since a code is sent only to a number the pool verifies
/// ([`PHONE_NUMBER_VERIFIED_ATTRIBUTE`]). A pool with MFA off challenges over
/// nothing and is judged by neither.
///
/// Every argument is the EFFECTIVE value of the request — what the pool would
/// hold once it is written — so the configuration is judged as the caller left
/// it, whichever operation wrote it. The MFA factors `SetUserPoolMfaConfig`
/// configures are that operation's own ([`crate::state::UserPool`] holds them
/// beside this mode), so they are not part of this question.
#[must_use]
pub fn mfa_without_sms_delivery(
    mfa_configuration: &str,
    sms_configuration: Option<&SmsConfigurationType>,
    auto_verified_attributes: &[String],
    lambda_config: Option<&LambdaConfigType>,
) -> bool {
    if mfa_configuration == MFA_OFF {
        return false;
    }
    !names_an_sms_delivery_path(sms_configuration, lambda_config)
        || !auto_verified_attributes
            .iter()
            .any(|attribute| attribute == PHONE_NUMBER_VERIFIED_ATTRIBUTE)
}

/// The message a user-pool write is refused with when its
/// `AccountRecoverySetting` repeats a priority or a mechanism
/// ([`duplicate_recovery_mechanisms`]). Fixed text, naming neither the
/// mechanisms nor the priorities the request carried.
pub const DUPLICATE_RECOVERY_MECHANISMS_MESSAGE: &str =
    "Invalid account recovery setting parameter. Account Recovery Setting cannot have \
     duplicate priorities or recovery mechanisms.";

/// Whether `setting`'s `RecoveryMechanisms` repeat a `Priority` value or a
/// mechanism `Name`.
///
/// A recovery option's priority is its RANK among the pool's mechanisms — "if
/// `verified_email` has a priority of 1 and `verified_phone_number` has a
/// priority of 2, your user pool sends account-recovery messages to a verified
/// email address but falls back to an SMS message" (`RecoveryOptionType` in this
/// service's model) — so two mechanisms at one priority leave the fallback order
/// undefined, and one mechanism listed twice gives that mechanism two ranks.
/// Neither is a configuration a pool can apply, so a request carrying one is
/// refused rather than stored.
#[must_use]
pub fn duplicate_recovery_mechanisms(setting: &AccountRecoverySettingType) -> bool {
    let Some(mechanisms) = setting.recovery_mechanisms.as_deref() else {
        return false;
    };
    let mut priorities: BTreeSet<i32> = BTreeSet::new();
    let mut names: BTreeSet<&str> = BTreeSet::new();
    mechanisms.iter().any(|mechanism| {
        !priorities.insert(mechanism.priority) || !names.insert(mechanism.name.as_str())
    })
}

/// The message a user-pool write is refused with when its
/// `AccountRecoverySetting` combines `admin_only` with another recovery
/// mechanism ([`admin_only_recovery_combined`]). Fixed text, naming neither the
/// mechanisms nor the pool.
pub const ADMIN_ONLY_RECOVERY_COMBINED_MESSAGE: &str =
    "Invalid account recovery setting parameter. Account Recovery Setting cannot use \
     admin_only setting with any other recovery mechanisms.";

/// Whether `setting`'s `RecoveryMechanisms` name `admin_only` BESIDE another
/// mechanism.
///
/// "The `admin_only` option prevents self-service account recovery"
/// (`RecoveryOptionType` in this service's model), so it is not one mechanism
/// among others that a reset message could fall back to: it says the pool sends
/// no such message at all. A configuration that lists it together with a
/// mechanism that DOES deliver one claims both at once, which is no configuration
/// a pool can apply, so a request carrying it is refused rather than stored —
/// exactly as a configuration that ranks one mechanism twice is
/// ([`duplicate_recovery_mechanisms`]).
#[must_use]
pub fn admin_only_recovery_combined(setting: &AccountRecoverySettingType) -> bool {
    let Some(mechanisms) = setting.recovery_mechanisms.as_deref() else {
        return false;
    };
    mechanisms.len() > 1
        && mechanisms
            .iter()
            .any(|mechanism| mechanism.name == RECOVERY_ADMIN_ONLY)
}

/// The service principal THIS service assumes a caller-supplied role as, so that
/// role's trust policy has to admit it: a pool's SMS caller role
/// ([`sms_caller_role_verdict`]) and a user import job's CloudWatch Logs
/// role ([`crate::state::IMPORT_LOGS_SERVICE_PRINCIPAL`]) are taken on by one
/// service, so the gates that read them cannot disagree about who is assuming.
pub const COGNITO_IDP_SERVICE_PRINCIPAL: &str = "cognito-idp.amazonaws.com";

/// The message a user-pool write is refused with when the role its
/// `SmsConfiguration` names does not let Amazon Cognito assume it
/// ([`SmsCallerRole::Unassumable`]). Fixed text, naming neither the role nor
/// the pool.
pub const SMS_ROLE_TRUST_RELATIONSHIP_MESSAGE: &str =
    "Role does not have a trust relationship allowing Cognito to assume the role";

/// The message a user-pool write is refused with when the role its
/// `SmsConfiguration` names cannot send the message once assumed
/// ([`SmsCallerRole::WithholdsPublish`]). Fixed text, naming neither the role,
/// the pool nor the action: the defect is the same one for every such role.
pub const SMS_ROLE_ACCESS_POLICY_MESSAGE: &str =
    "Role does not have permission to publish with SNS";

/// The condition key the assume Amazon Cognito makes presents the external id
/// under: "if you provide an `ExternalId`, your Amazon Cognito user pool includes
/// it in the request to assume your IAM role. You can configure the role trust
/// policy to require that Amazon Cognito, and any principal, provide the
/// `ExternalID`" (`SmsConfigurationType.ExternalId` in this service's model).
const EXTERNAL_ID_CONDITION_KEY: &str = "sts:ExternalId";

/// The condition key that assume names the resource it is made FOR, which for an
/// SMS caller role is the user pool the message would leave: the key a trust
/// policy narrowed to one pool is written over, exactly as the pool's Lambda
/// triggers present it on the invocation they make
/// ([`crate::state::lambda_triggers`]).
const SOURCE_ARN_CONDITION_KEY: &str = "aws:SourceArn";

/// The condition key that assume names the account owning that resource by.
const SOURCE_ACCOUNT_CONDITION_KEY: &str = "aws:SourceAccount";

/// The IAM action a user pool performs under its SMS caller role: Amazon SNS's
/// `Publish`, "the ARN of the IAM role in your Amazon Web Services account that
/// Amazon Cognito will use to send SMS messages" (`SmsConfigurationType` in this
/// service's model) naming the role it is performed as.
const SNS_PUBLISH_ACTION: &str = "sns:Publish";

/// The resource that publish names. A text message is published to a PHONE
/// NUMBER rather than to a topic — Amazon SNS's `Publish` takes "the phone number
/// to which you want to deliver an SMS message" instead of a target ARN — so the
/// call names no resource of its own and only a grant covering every resource
/// lets the role make it.
const EVERY_RESOURCE: &str = "*";

/// The ARN of the user pool `user_pool_id` names in `region`:
/// `arn:<partition>:cognito-idp:<region>:<account>:userpool/<UserPoolId>`, the
/// form [`crate::state::new_user_pool`] mints a pool's own ARN in.
///
/// What a write that has not read the pool yet names it by, so the assume it
/// validates presents the same source ARN the pool's stored one would
/// ([`UserPool::arn_in_region`]).
#[must_use]
pub fn user_pool_arn(region: &str, user_pool_id: &str) -> String {
    ::engine::emulator_arn(
        crate::state::SIGNING_NAME,
        region,
        &format!("userpool/{user_pool_id}"),
    )
}

/// The Amazon SNS caller role `sms_configuration` names, when it names one: the
/// `SnsCallerArn` it carries, and `None` where that is absent or the member's own
/// empty default (`OptionalArnType` with `@default ""`).
#[must_use]
pub fn sns_caller_role_arn(sms_configuration: Option<&SmsConfigurationType>) -> Option<&str> {
    sms_configuration
        .and_then(|sms| sms.sns_caller_arn.as_deref())
        .filter(|role_arn| !role_arn.is_empty())
}

/// Whether `sms_configuration` names a caller an SMS message can be sent through
/// at all.
///
/// A text message leaves a user pool through the caller its SMS configuration
/// names, and there are two of them: the Amazon SNS caller role
/// ([`sns_caller_role_arn`]) and the Amazon Web Services End User Messaging
/// configuration — "in a user pool, provide either the Amazon SNS configuration
/// (`SnsCallerArn`) or this configuration, but not both"
/// (`SmsConfigurationType.EumsSms` in this service's model). A configuration that
/// carries neither names nobody to send through, exactly as a request or a pool
/// that carries no `SmsConfiguration` at all.
///
/// The ONE reading of that question, so the MFA factor's precondition
/// (`SetUserPoolMfaConfig`) and the pool's own SMS delivery path
/// ([`names_an_sms_delivery_path`]) cannot answer it differently.
#[must_use]
pub fn names_an_sms_caller(sms_configuration: Option<&SmsConfigurationType>) -> bool {
    sns_caller_role_arn(sms_configuration).is_some()
        || sms_configuration.is_some_and(|sms| sms.eums_sms.is_some())
}

/// Whether a configuration can deliver a text message AT ALL, over either of the
/// two paths a pool can configure for one.
///
/// A text message leaves a pool through the caller its `SmsConfiguration` names
/// ([`names_an_sms_caller`]), or it is handed to the function its
/// `CustomSMSSender` names — that trigger "rout[ing] all SMS notifications from a
/// user pool to a Lambda function that delivers the message using custom logic"
/// (`LambdaConfigType$CustomSMSSender` in this service's model), which makes the
/// function the whole delivery path and asks for no caller to send through. A
/// configuration that names NEITHER can deliver nothing by SMS.
///
/// The ONE reading of that question, taken on the EFFECTIVE values of a request —
/// what the pool would hold once it is written — so the preconditions a pool
/// write is held to ([`sms_configuration_missing_for_auto_verification`],
/// [`mfa_without_sms_delivery`]) and the stored pool's own answer
/// ([`UserPool::delivers_sms`]) cannot disagree about it.
#[must_use]
pub fn names_an_sms_delivery_path(
    sms_configuration: Option<&SmsConfigurationType>,
    lambda_config: Option<&LambdaConfigType>,
) -> bool {
    names_an_sms_caller(sms_configuration)
        || lambda_config.is_some_and(|lambda_config| {
            crate::state::lambda_triggers::custom_sender::Sender::Sms
                .configured_in(lambda_config)
                .is_some()
        })
}

/// What IAM answers about the Amazon SNS caller role a pool write names: whether
/// the pool could send a text message through it at all, and if not, WHICH half of
/// that is missing.
///
/// A pool sends SMS by taking the role on and publishing as it, so the two halves
/// fail differently and are reported differently — which is the whole point of
/// this being a verdict rather than a predicate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmsCallerRole {
    /// Nothing IAM answered stops the pool sending through this configuration —
    /// including a configuration that names no Amazon SNS caller to probe.
    Usable,
    /// Amazon Cognito cannot take the role on, so nothing could be sent as it:
    /// [`SMS_ROLE_TRUST_RELATIONSHIP_MESSAGE`].
    Unassumable,
    /// The role is one Amazon Cognito can take on, and the permissions in force
    /// on it do not let it publish the message:
    /// [`SMS_ROLE_ACCESS_POLICY_MESSAGE`].
    WithholdsPublish,
}

/// What this account's IAM answers about the Amazon SNS caller
/// `sms_configuration` names, for the pool `user_pool_arn` names.
///
/// "To send SMS messages with Amazon SNS in the Amazon Web Services Region that
/// you want, the Amazon Cognito user pool uses an Identity and Access Management
/// (IAM) role in your Amazon Web Services account" — `SnsCallerArn` being "the
/// ARN of the IAM role in your Amazon Web Services account that Amazon Cognito
/// will use to send SMS messages" (`SmsConfigurationType` in this service's
/// model). A pool sends a text message by ASSUMING that role and PUBLISHING as
/// it, so the configuration is probed by asking IAM both of those questions
/// rather than by storing a delivery path that could never send.
///
/// The assume is [`SmsCallerRole::Unassumable`] on three answers:
///
/// * the value does not NAME an IAM role of this account — it is not an IAM role
///   ARN at all, or it names another account, whose roles this account cannot
///   hand to Amazon Cognito ([`service_common::iam_role::role_name`]);
/// * this account's IAM answers that it holds no role under that name, so there
///   is no role to assume;
/// * it holds the role and the trust policy it carries does not grant the assume
///   Amazon Cognito would MAKE: the principal is not admitted, or the grant's
///   conditions do not cover a request presenting the configuration's
///   `ExternalId` ([`EXTERNAL_ID_CONDITION_KEY`]) for this pool
///   ([`SOURCE_ARN_CONDITION_KEY`]) in this account
///   ([`SOURCE_ACCOUNT_CONDITION_KEY`]). Those three are the keys that assume
///   really carries, so a trust policy narrowed by any of them is read as the
///   grant it is rather than as a clause nothing satisfies.
///
/// A role that CAN be assumed still sends nothing unless the permissions in force
/// on it allow the publish the pool makes as it
/// ([`service_common::iam_role::role_withholds_action`], IAM's own simulation of
/// that role's policies), which is [`SmsCallerRole::WithholdsPublish`] — a
/// separate defect of a separate document, reported separately.
///
/// Both readings are the tree's one reading of a caller-supplied role, so a
/// document is evaluated where every other service's is. Nothing is refused on an
/// answer this emulator could not read, or where there is no IAM to ask at all: a
/// refusal has to be grounded in what IAM said. Await it with no state guard
/// held.
pub async fn sms_caller_role_verdict(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
    user_pool_arn: &str,
    sms_configuration: Option<&SmsConfigurationType>,
) -> SmsCallerRole {
    // An Amazon SNS caller is what this reading is about: a configuration that
    // names none asks for no assume, and neither does one that delivers through
    // End User Messaging instead ([`names_an_sms_caller`]).
    let Some(role_arn) = sns_caller_role_arn(sms_configuration) else {
        return SmsCallerRole::Usable;
    };
    if ::service_common::iam_role::role_name(role_arn).is_none() {
        return SmsCallerRole::Unassumable;
    }
    if ::service_common::iam_role::role_assume_is_unauthorized_through(
        ctx.callback(),
        region,
        role_arn,
        COGNITO_IDP_SERVICE_PRINCIPAL,
        &assume_context(user_pool_arn, sms_configuration),
    )
    .await
    {
        return SmsCallerRole::Unassumable;
    }
    if ::service_common::iam_role::role_withholds_action(
        ctx,
        region,
        role_arn,
        SNS_PUBLISH_ACTION,
        EVERY_RESOURCE,
    )
    .await
    {
        return SmsCallerRole::WithholdsPublish;
    }
    SmsCallerRole::Usable
}

/// The condition keys the assume Amazon Cognito makes for the pool
/// `user_pool_arn` names carries: the resource the role is taken on FOR and the
/// account that owns it, and the external id the configuration presents when it
/// presents one.
///
/// Nothing is stated for a key the request does not really carry, so a trust
/// policy that narrows the grant to an external id this configuration does not
/// present is a grant this assume cannot be shown to satisfy.
fn assume_context(
    user_pool_arn: &str,
    sms_configuration: Option<&SmsConfigurationType>,
) -> Vec<(String, String)> {
    let mut context = vec![(
        SOURCE_ARN_CONDITION_KEY.to_string(),
        user_pool_arn.to_string(),
    )];
    if let Some(account) = ::service_common::arn::parse(user_pool_arn)
        .map(|parsed| parsed.account.to_string())
        .filter(|account| !account.is_empty())
    {
        context.push((SOURCE_ACCOUNT_CONDITION_KEY.to_string(), account));
    }
    if let Some(external_id) = sms_configuration
        .and_then(|sms| sms.external_id.as_deref())
        .filter(|external_id| !external_id.is_empty())
    {
        context.push((
            EXTERNAL_ID_CONDITION_KEY.to_string(),
            external_id.to_string(),
        ));
    }
    context
}
