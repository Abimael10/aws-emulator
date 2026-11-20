// SPDX-License-Identifier: CC0-1.0

//! The custom SMS sender trigger's own identity: what `LambdaConfig.CustomSMSSender` names
//! and how its events are labelled.
//!
//! "The configuration of a custom SMS sender Lambda trigger. This trigger routes all SMS
//! notifications from a user pool to a Lambda function that delivers the message using
//! custom logic" (`LambdaConfigType$CustomSMSSender`, this service's model). So on a pool
//! that configures one, the function is the pool's whole SMS delivery path: a text message
//! is not sent by this service at all, it is handed to that function.
//!
//! The machinery that builds the event, encrypts the code and performs the invocation is the
//! one every custom sender shares ([`super::custom_sender`]); this module holds only what is
//! this trigger's own, so the SMS sender and the email sender
//! ([`super::custom_email`]) cannot disagree about anything else.

use crate::ops::types::LambdaConfigType;

/// The trigger whose configured function every text message of a pool that configures one is
/// handed to: the name a failure of this trigger states, and the name its events'
/// `triggerSource` opens with.
pub const TRIGGER: &str = "CustomSMSSender";

/// What this sender's event reports as `request.type`: the request shape the function is
/// handed, as the trigger's one version defines it.
pub const REQUEST_TYPE: &str = "customSMSSenderRequestV1";

/// The function `lambda_config` routes its text messages to, and the trigger version that
/// member asks for, or `None` for a configuration that names no custom SMS sender at all.
#[must_use]
pub fn configured(lambda_config: &LambdaConfigType) -> Option<(&str, &str)> {
    let configured = lambda_config.custom_sms_sender.as_ref()?;
    Some((
        configured.lambda_arn.as_str(),
        configured.lambda_version.as_str(),
    ))
}
