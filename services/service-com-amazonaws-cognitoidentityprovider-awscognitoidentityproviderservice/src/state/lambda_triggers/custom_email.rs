// SPDX-License-Identifier: CC0-1.0

//! The custom EMAIL sender trigger's own identity: what `LambdaConfig.CustomEmailSender`
//! names and how its events are labelled.
//!
//! "The configuration of a custom email sender Lambda trigger. This trigger routes all email
//! notifications from a user pool to a Lambda function" (`LambdaConfigType$CustomEmailSender`,
//! this service's model). The machinery that builds the event, encrypts the code and
//! performs the invocation is the one every custom sender shares
//! ([`super::custom_sender`]); this module holds only what is this trigger's own, so the
//! email sender and the SMS sender ([`super::custom_sms`]) cannot disagree about anything
//! else.

use crate::ops::types::LambdaConfigType;

/// The trigger whose configured function every email message of a pool that configures one
/// is handed to: the name a failure of this trigger states, and the name its events'
/// `triggerSource` opens with.
pub const TRIGGER: &str = "CustomEmailSender";

/// What this sender's event reports as `request.type`: the request shape the function is
/// handed, as the trigger's one version defines it.
pub const REQUEST_TYPE: &str = "customEmailSenderRequestV1";

/// The function `lambda_config` routes its email to, and the trigger version that member
/// asks for, or `None` for a configuration that names no custom email sender at all.
#[must_use]
pub fn configured(lambda_config: &LambdaConfigType) -> Option<(&str, &str)> {
    let configured = lambda_config.custom_email_sender.as_ref()?;
    Some((
        configured.lambda_arn.as_str(),
        configured.lambda_version.as_str(),
    ))
}
