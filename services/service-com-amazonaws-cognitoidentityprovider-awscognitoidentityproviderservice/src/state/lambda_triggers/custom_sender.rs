// SPDX-License-Identifier: CC0-1.0

//! The custom sender triggers: the one place a pool's `LambdaConfig.CustomEmailSender` or
//! `LambdaConfig.CustomSMSSender` becomes an invocation of the function that member names.
//!
//! "The configuration of a custom email sender Lambda trigger. This trigger routes all email
//! notifications from a user pool to a Lambda function" (`LambdaConfigType$CustomEmailSender`),
//! and "the configuration of a custom SMS sender Lambda trigger. This trigger routes all SMS
//! notifications from a user pool to a Lambda function that delivers the message using custom
//! logic" (`LambdaConfigType$CustomSMSSender`, both from this service's model). So the pool
//! configures one custom sender PER MEDIUM, and on a pool that configures the sender of the
//! medium a message travels over, the operation that issues the message does not send it
//! itself: it hands the code to that function, which is the only place the message is
//! delivered at all. The code travels encrypted "with the key that you provided in the
//! KMSKeyID parameter" — `LambdaConfigType$KMSKeyID` is "the ARN of an KMS key. Amazon
//! Cognito uses the key to encrypt codes and temporary passwords sent to
//! `CustomEmailSender` and `CustomSMSSender`" — so the key the pool configured is asked to
//! wrap the data key of the message [`super::esdk`] writes.
//!
//! The work splits the same way every other trigger of this service does
//! ([`super`]): [`pending`] resolves the member for the medium and builds the event from the
//! pool and the user read under the state lock, and [`invoke`] encrypts the code and
//! performs the invocation once that lock is RELEASED.
//!
//! Which stage issued the code is what the event's `triggerSource` reports beside the
//! trigger's own name ([`Stage`]); the request half is the `customEmailSenderRequestV1` or
//! `customSMSSenderRequestV1` document, whose `code` is the encrypted code and whose
//! `userAttributes` are the user's own.

use std::collections::HashMap;

use ::base64::Engine as _;
use ::serde_json::{Map, Value};

use super::esdk;
use super::{custom_email, custom_sms, Caller, Dispatch, Failure, Verdict};
use crate::ops::types::LambdaConfigType;
use crate::state::{User, UserPool};

/// The ARN namespace of the key service, whose keys a custom sender's `KMSKeyID` names.
const KMS_ARN_NAMESPACE: &str = "kms";

/// The error a pool that routes its messages to a custom sender without configuring a key
/// for them carries: there is no key to encrypt the code under, so the message cannot be
/// handed over at all.
const NO_KEY_CONFIGURED_ERROR: &str =
    "No KMS key is configured for the custom sender of this user pool";

/// The error a code that could not be prepared for the function carries.
const NO_RANDOMNESS_ERROR: &str = "The code could not be prepared for the custom sender";

/// The error a key service answer carrying no wrapped key carries.
const NO_WRAPPED_KEY_ERROR: &str = "The configured KMS key wrapped no key for the code";

/// Which custom sender a message is routed to: the one of the MEDIUM it travels over.
///
/// A pool configures the two independently — `CustomEmailSender` "routes all email
/// notifications" and `CustomSMSSender` "routes all SMS notifications" (this service's
/// model) — so the medium of the message decides which member is read, which trigger name
/// the event reports and which request document it carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Sender {
    /// The custom email sender, the channel of every message addressed to an email address.
    Email,
    /// The custom SMS sender, the channel of every message addressed to a phone number.
    Sms,
}

impl Sender {
    /// The trigger whose configured function this sender's events are delivered to, as a
    /// failure of that trigger names it and as the event's `triggerSource` opens with.
    #[must_use]
    pub fn trigger(self) -> &'static str {
        match self {
            Self::Email => custom_email::TRIGGER,
            Self::Sms => custom_sms::TRIGGER,
        }
    }

    /// What this sender's event reports as `request.type`: the request shape the function is
    /// handed, as the trigger's one version defines it.
    #[must_use]
    fn request_type(self) -> &'static str {
        match self {
            Self::Email => custom_email::REQUEST_TYPE,
            Self::Sms => custom_sms::REQUEST_TYPE,
        }
    }

    /// This sender's `triggerSource` for `stage`: the trigger's name and the stage that
    /// issued the code.
    #[must_use]
    pub fn trigger_source(self, stage: Stage) -> String {
        format!("{}_{}", self.trigger(), stage.name())
    }

    /// The custom sender a message delivered over `medium` is routed to — an email address
    /// is mailed and a phone number is texted
    /// ([`crate::state::DELIVERY_MEDIUM_ATTRIBUTES`] maps the two value sets to each other)
    /// — or `None` for a value no `DeliveryMediumType` names.
    #[must_use]
    pub fn of_medium(medium: &str) -> Option<Self> {
        match crate::state::medium_destination_attribute(medium) {
            Some("email") => Some(Self::Email),
            Some("phone_number") => Some(Self::Sms),
            _ => None,
        }
    }

    /// The custom sender a message addressed to `attribute`'s stored value is routed to: the
    /// sender of the medium that attribute is reached over
    /// ([`crate::state::delivery_medium`]).
    #[must_use]
    pub fn of_attribute(attribute: &str) -> Option<Self> {
        crate::state::delivery_medium(attribute).and_then(Self::of_medium)
    }

    /// The function `pool` routes this sender's messages to, as its `LambdaConfig` names it,
    /// or `None` for a pool that configures none — including one whose member carries the
    /// empty ARN its shape defaults to.
    #[must_use]
    pub fn configured_function(self, pool: &UserPool) -> Option<&str> {
        let (lambda_arn, _) = self.configured(pool)?;
        Some(lambda_arn)
    }

    /// The function and the trigger version `pool` configures for this sender.
    #[must_use]
    fn configured(self, pool: &UserPool) -> Option<(&str, &str)> {
        self.configured_in(&pool.lambda_config)
    }

    /// The function and the trigger version a `LambdaConfig` configures for this sender,
    /// which is what the operations that WRITE a pool read the delivery path off: they hold
    /// the configuration the write would leave behind rather than a stored pool.
    #[must_use]
    pub fn configured_in(self, lambda_config: &LambdaConfigType) -> Option<(&str, &str)> {
        let (lambda_arn, lambda_version) = match self {
            Self::Email => custom_email::configured(lambda_config)?,
            Self::Sms => custom_sms::configured(lambda_config)?,
        };
        (!lambda_arn.is_empty()).then_some((lambda_arn, lambda_version))
    }
}

/// Which stage issued the code a custom sender is handed, which is the second half of the
/// event's `triggerSource`: the trigger's own name is the first ([`Sender::trigger_source`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// A sign-up's confirmation code, sent by `SignUp`.
    SignUp,
    /// The temporary password of the invitation an administrative create sends, by
    /// `AdminCreateUser` — the credential "the user must enter ... in the sign-in page,
    /// along with a new password to be used in all future sign-ins"
    /// (`AdminCreateUser.TemporaryPassword`), which is one of the "temporary passwords sent
    /// to `CustomEmailSender`" the pool's key wraps (`LambdaConfigType$KMSKeyID`).
    AdminCreateUser,
    /// A sign-up confirmation code sent again, by `ResendConfirmationCode`.
    ResendCode,
    /// A password-reset code, sent by `ForgotPassword` and by `AdminResetUserPassword`.
    ForgotPassword,
    /// The code for an attribute an update of the user's attributes asked to verify.
    UpdateUserAttribute,
    /// The code for one attribute, asked for by `GetUserAttributeVerificationCode`.
    VerifyUserAttribute,
    /// The one-time password of a message sign-in challenge.
    Authentication,
}

impl Stage {
    /// This stage's name, as a `triggerSource` reports it after the trigger's own.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::SignUp => "SignUp",
            Self::AdminCreateUser => "AdminCreateUser",
            Self::ResendCode => "ResendCode",
            Self::ForgotPassword => "ForgotPassword",
            Self::UpdateUserAttribute => "UpdateUserAttribute",
            Self::VerifyUserAttribute => "VerifyUserAttribute",
            Self::Authentication => "Authentication",
        }
    }
}

/// One custom sender invocation an operation owes: the invocation itself, the trigger it
/// serves, the key the code is encrypted under and the code that was issued.
///
/// The code is held in the CLEAR here and encrypted only when the invocation is performed,
/// because encrypting it is a call to another service and this is built under the state
/// lock.
#[derive(Debug, Clone)]
pub struct Pending {
    dispatch: Dispatch,
    sender: Sender,
    kms_key_id: Option<String>,
    code: String,
}

impl Pending {
    /// The event this invocation delivers, before the code is encrypted into it.
    #[must_use]
    pub fn event(&self) -> &Value {
        self.dispatch.event()
    }

    /// The function this invocation is of, as the pool's `LambdaConfig` names it.
    #[must_use]
    pub fn function_arn(&self) -> &str {
        self.dispatch.function_arn()
    }

    /// Which custom sender this invocation is of, which is the sender of the medium the
    /// message travels over.
    #[must_use]
    pub fn sender(&self) -> Sender {
        self.sender
    }

    /// The key the code is encrypted under, as the pool's `LambdaConfig` names it, or `None`
    /// for a pool that configures none.
    #[must_use]
    pub fn kms_key_id(&self) -> Option<&str> {
        self.kms_key_id.as_deref()
    }

    /// The code that was issued, which the function is the only recipient of.
    #[must_use]
    pub fn code(&self) -> &str {
        &self.code
    }
}

/// The custom sender invocation a code issued to `user` of `pool` and handed to `sender`
/// owes, or `None` when the pool configures no function for that sender and so the message
/// is not routed to one at all.
///
/// Built while the caller still holds the state lock — the event reports the pool and the
/// user as the issuing operation left them — and invoked by [`invoke`] once that lock is
/// released.
#[must_use]
pub fn pending(
    pool: &UserPool,
    user: &User,
    sender: Sender,
    stage: Stage,
    caller: Caller<'_>,
    client_metadata: Option<&HashMap<String, String>>,
    code: &str,
) -> Option<Pending> {
    let (function_arn, lambda_version) = sender.configured(pool)?;
    let function_arn = function_arn.to_string();
    let version = super::event_version(lambda_version);
    let mut members = Map::new();
    members.insert(
        "type".to_string(),
        Value::String(sender.request_type().to_string()),
    );
    // The code is encrypted when the invocation is performed, so the member it will travel
    // in is there from the start and nothing else decides where it goes.
    members.insert("code".to_string(), Value::Null);
    let mut event = super::event(
        pool,
        user,
        &sender.trigger_source(stage),
        &version,
        caller,
        client_metadata,
        members,
    );
    // A custom sender SENDS the message; it does not amend the outcome of the operation that
    // issued it, so the event it is handed carries no response half to fill in.
    if let Value::Object(members) = &mut event {
        members.remove("response");
    }
    Some(Pending {
        dispatch: super::dispatch(pool, function_arn, sender.trigger(), event),
        sender,
        kms_key_id: pool
            .lambda_config
            .kms_key_id
            .clone()
            .filter(|key| !key.is_empty()),
        code: code.to_string(),
    })
}

/// Fire `sender` for a code issued for `username` in `user_pool_id`, and report what it
/// leaves the issuing operation to answer.
///
/// For an operation that holds NO lock: the pool and the user are read under a lock of its
/// own, which is released before the code is encrypted and the function invoked, so no other
/// request of this service waits on either. A pool that configures no function for that
/// sender is answered by nothing being invoked at all.
#[allow(clippy::too_many_arguments)]
pub async fn fire(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    store: &::engine::ServiceState<super::State>,
    user_pool_id: &str,
    username: &str,
    sender: Sender,
    stage: Stage,
    caller: Caller<'_>,
    client_metadata: Option<&HashMap<String, String>>,
    code: &str,
) -> Result<(), Failure> {
    let owed = {
        let guard = store.lock();
        let pool = guard.pool(user_pool_id);
        let user = pool.and_then(|pool| pool.user(username));
        match (pool, user) {
            (Some(pool), Some(user)) => {
                pending(pool, user, sender, stage, caller, client_metadata, code)
            }
            _ => None,
        }
    };
    match owed {
        Some(owed) => invoke(ctx, owed).await,
        None => Ok(()),
    }
}

/// The same, for a code delivered over `medium`: the sender of that medium is the one the
/// message is handed to, and a medium no `DeliveryMediumType` names is handed to nobody.
#[allow(clippy::too_many_arguments)]
pub async fn fire_over_medium(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    store: &::engine::ServiceState<super::State>,
    user_pool_id: &str,
    username: &str,
    medium: &str,
    stage: Stage,
    caller: Caller<'_>,
    client_metadata: Option<&HashMap<String, String>>,
    code: &str,
) -> Result<(), Failure> {
    let Some(sender) = Sender::of_medium(medium) else {
        return Ok(());
    };
    fire(
        ctx,
        store,
        user_pool_id,
        username,
        sender,
        stage,
        caller,
        client_metadata,
        code,
    )
    .await
}

/// The same, for a code the message was addressed to `attribute`'s stored value: the sender
/// of the medium that attribute is reached over is the one the message is handed to, and an
/// attribute no message is addressed over is handed to nobody.
#[allow(clippy::too_many_arguments)]
pub async fn fire_over_attribute(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    store: &::engine::ServiceState<super::State>,
    user_pool_id: &str,
    username: &str,
    attribute: &str,
    stage: Stage,
    caller: Caller<'_>,
    client_metadata: Option<&HashMap<String, String>>,
    code: &str,
) -> Result<(), Failure> {
    let Some(sender) = Sender::of_attribute(attribute) else {
        return Ok(());
    };
    fire(
        ctx,
        store,
        user_pool_id,
        username,
        sender,
        stage,
        caller,
        client_metadata,
        code,
    )
    .await
}

/// Encrypt the pending code under the pool's key and invoke the function it is destined for.
///
/// Call it with the state lock RELEASED: the issuing operation's own writes are already
/// persisted — the code is recorded, which is what the operation that consumes it checks a
/// presented code against — and holding the lock across the key-service call and the
/// invocation would make every other request of this service wait on them.
pub async fn invoke(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    owed: Pending,
) -> Result<(), Failure> {
    let Pending {
        mut dispatch,
        sender,
        kms_key_id,
        code,
    } = owed;
    let Some(kms_key_id) = kms_key_id else {
        return Err(Failure::NotInvokable(super::failure_message(
            sender.trigger(),
            NO_KEY_CONFIGURED_ERROR,
        )));
    };
    let region = dispatch.region.clone();
    let encrypted = encrypted_code(ctx, sender, &region, &kms_key_id, &code).await?;
    if let Some(request) = dispatch.event.get_mut("request").and_then(Value::as_object_mut) {
        request.insert("code".to_string(), Value::String(encrypted));
    }
    // What the function ANSWERS decides nothing: a sender is handed the message to deliver,
    // not the outcome to amend. A function that could not be invoked at all, or that
    // reported an error, is the trigger's own failure and is the issuing operation's to
    // report ([`super::invoke`]).
    match super::invoke(ctx, &dispatch).await? {
        Verdict::Answered(_) | Verdict::NoVerdict(_) => Ok(()),
    }
}

/// `code`, as the message the function receives it in: a fresh data key wrapped by the
/// configured key, and the code encrypted under that data key ([`super::esdk`]), carried
/// base64-encoded because the event member is text.
///
/// `pool_region` is where the key is asked for when the configured value names no Region of
/// its own — a bare key id or an alias does not.
async fn encrypted_code(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    sender: Sender,
    pool_region: &str,
    kms_key_id: &str,
    code: &str,
) -> Result<String, Failure> {
    let random = ::ring::rand::SystemRandom::new();
    let mut data_key = [0_u8; esdk::DATA_KEY_LENGTH];
    let mut message_id = [0_u8; esdk::MESSAGE_ID_LENGTH];
    ::ring::rand::SecureRandom::fill(&random, &mut data_key)
        .and_then(|()| ::ring::rand::SecureRandom::fill(&random, &mut message_id))
        .map_err(|_| trigger_failed(sender, NO_RANDOMNESS_ERROR))?;

    let region = ::service_common::arn::region_of(kms_key_id, KMS_ARN_NAMESPACE)
        .unwrap_or(pool_region)
        .to_string();
    let wrapped = kms_client(ctx, &region)
        .encrypt()
        .key_id(kms_key_id)
        .plaintext(::aws_sdk_kms::primitives::Blob::new(data_key.to_vec()))
        .send()
        .await
        .map_err(|error| {
            trigger_failed(
                sender,
                ::aws_smithy_types::error::metadata::ProvideErrorMetadata::message(&error)
                    .unwrap_or(NO_WRAPPED_KEY_ERROR),
            )
        })?;
    let Some(blob) = wrapped.ciphertext_blob() else {
        return Err(trigger_failed(sender, NO_WRAPPED_KEY_ERROR));
    };
    // The key service reports the key it used, which is the key a reader has to ask to
    // unwrap the data key; the configured value is what names it when it reports none.
    let message = esdk::seal(
        &data_key,
        blob.as_ref(),
        wrapped.key_id().unwrap_or(kms_key_id),
        &message_id,
        code.as_bytes(),
    )
    .map_err(|fault| trigger_failed(sender, fault.message()))?;
    Ok(::base64::engine::general_purpose::STANDARD.encode(message))
}

/// The failure of `sender`'s trigger carrying `error`, worded the way every user-pool
/// trigger's failure is.
fn trigger_failed(sender: Sender, error: &str) -> Failure {
    Failure::NotInvokable(super::failure_message(sender.trigger(), error))
}

/// The key service's client for `region`, over the in-process callback into this same
/// emulator: the key a pool configures is one this emulator itself holds, so the call never
/// leaves the process.
///
/// The transport is in-process, so this disables what a network client needs and this one
/// cannot have, exactly as [`super::lambda_client`] does and for the same reason.
fn kms_client(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
) -> ::aws_sdk_kms::Client {
    let config = ::aws_sdk_kms::Config::builder()
        .behavior_version(::aws_sdk_kms::config::BehaviorVersion::latest())
        .http_client(ctx.callback().http_client())
        .credentials_provider(ctx.callback().aws_credentials_provider())
        .region(::aws_sdk_kms::config::Region::new(region.to_string()))
        .endpoint_url(format!("https://{KMS_ARN_NAMESPACE}.{region}.amazonaws.com"))
        .retry_config(::aws_sdk_kms::config::retry::RetryConfig::disabled())
        .timeout_config(::aws_sdk_kms::config::timeout::TimeoutConfig::disabled())
        .identity_cache(::aws_sdk_kms::config::IdentityCache::no_cache())
        .stalled_stream_protection(::aws_sdk_kms::config::StalledStreamProtectionConfig::disabled())
        .build();
    ::aws_sdk_kms::Client::from_conf(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every `triggerSource` a custom sender reports names the trigger, so a function can
    /// tell a custom sender event from any other event it is handed, and the stage that
    /// issued the code.
    ///
    /// The stages are the ones Amazon Cognito documents a custom sender source for, and each
    /// is reported by BOTH senders: the two triggers sit at the same stages, one per medium.
    #[test]
    fn every_trigger_source_names_the_custom_sender_and_its_stage() {
        let stages = [
            (Stage::SignUp, "SignUp"),
            (Stage::AdminCreateUser, "AdminCreateUser"),
            (Stage::ResendCode, "ResendCode"),
            (Stage::ForgotPassword, "ForgotPassword"),
            (Stage::UpdateUserAttribute, "UpdateUserAttribute"),
            (Stage::VerifyUserAttribute, "VerifyUserAttribute"),
            (Stage::Authentication, "Authentication"),
        ];
        for sender in [Sender::Email, Sender::Sms] {
            for (stage, name) in stages {
                let reported = sender.trigger_source(stage);
                assert_eq!(
                    reported,
                    format!("{}_{name}", sender.trigger()),
                    "case {sender:?}/{stage:?}: expected the trigger and the stage, got \
                     {reported:?}"
                );
            }
        }
    }

    /// Which custom sender a message is routed to is decided by the MEDIUM it travels over:
    /// the email sender for a message to an email address, the SMS sender for one to a phone
    /// number, and nothing for a value no `DeliveryMediumType` names.
    #[test]
    fn the_medium_decides_which_custom_sender_a_message_is_routed_to() {
        let rows = [
            ("EMAIL", Some(Sender::Email)),
            ("SMS", Some(Sender::Sms)),
            ("PIGEON", None),
            ("", None),
            ("sms", None),
        ];
        for (medium, expected) in rows {
            let chosen = Sender::of_medium(medium);
            assert_eq!(
                chosen, expected,
                "case {medium:?}: expected {expected:?}, got {chosen:?}"
            );
        }
    }

    /// The attribute a message is addressed to decides the same thing, through the medium
    /// that attribute is reached over.
    #[test]
    fn the_addressed_attribute_decides_the_same_custom_sender() {
        let rows = [
            ("email", Some(Sender::Email)),
            ("phone_number", Some(Sender::Sms)),
            ("preferred_username", None),
        ];
        for (attribute, expected) in rows {
            let chosen = Sender::of_attribute(attribute);
            assert_eq!(
                chosen, expected,
                "case {attribute:?}: expected {expected:?}, got {chosen:?}"
            );
        }
    }
}
