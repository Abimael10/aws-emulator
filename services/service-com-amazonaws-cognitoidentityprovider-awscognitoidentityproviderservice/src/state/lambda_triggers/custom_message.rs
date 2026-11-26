// SPDX-License-Identifier: CC0-1.0

//! The custom MESSAGE trigger: the one place a pool's `LambdaConfig.CustomMessage` becomes an
//! invocation of the function that member names.
//!
//! "A custom message Lambda trigger. This trigger is an opportunity to customize all SMS and
//! email messages from your user pool. When a custom message trigger is active, your user pool
//! routes all messages to a Lambda function that returns a runtime-customized message subject
//! and body for your user pool to deliver to a user" (`LambdaConfigType$CustomMessage`, this
//! service's model). So on a pool that configures one, the function is asked for the wording of
//! every message the pool itself delivers, and it is asked BEFORE the message goes out: the
//! operation that issues the message is GATED on that invocation ([`super::invoke_gating`]), the
//! way every trigger that sits before the outcome it can modify is, so a function that cannot be
//! invoked, that failed, or that answered something other than the event it was handed is the
//! issuing operation's own Lambda failure rather than a message delivered as if no trigger were
//! configured.
//!
//! The function is asked for WORDING, not for delivery: the code itself is not handed over —
//! the event carries the PLACEHOLDER the customized message writes it in
//! ([`CODE_PARAMETER`]) and the pool substitutes the code it issued — which is what
//! distinguishes this trigger from a custom sender, the trigger a pool routes a medium's
//! messages to instead of sending them itself ([`super::custom_sender`]).
//!
//! The work splits the same way every other trigger of this service does ([`super`]):
//! [`pending`] resolves the member and builds the event from the pool and the user read under
//! the state lock, and [`invoke`] performs the invocation once that lock is RELEASED.
//!
//! Which stage issued the message is what the event's `triggerSource` reports beside the
//! trigger's own name, over the same stages a custom sender serves
//! ([`super::custom_sender::Stage`]), since both triggers sit at the one point a pool issues a
//! message.

use std::collections::HashMap;

use ::serde_json::{Map, Value};

use super::custom_sender::{Sender, Stage};
use super::{Caller, Dispatch, Failure};
use crate::ops::types::LambdaConfigType;
use crate::state::{User, UserPool};

/// The trigger whose configured function every message of a pool that configures one is asked
/// the wording of: the name a failure of this trigger states, and the name its events'
/// `triggerSource` opens with.
pub const TRIGGER: &str = "CustomMessage";

/// `request.codeParameter`: "a string for you to use as the placeholder for the verification
/// code in the custom message" (Amazon Cognito's custom message event). The code itself is the
/// pool's to substitute, so the function is handed the placeholder and never the code.
pub const CODE_PARAMETER: &str = "{####}";

/// `request.usernameParameter`: the placeholder for the user's name, which the event carries
/// for the one stage whose message names its recipient — the invitation an administrative
/// create sends, whose template "must have the {username} placeholder"
/// (`AdminCreateUser`'s invitation message).
pub const USERNAME_PARAMETER: &str = "{username}";

/// The response members this trigger reads back: the wording the function returns for each
/// medium, and the subject of an email message.
const WORDING_MEMBERS: [&str; 3] = [SMS_MESSAGE, EMAIL_MESSAGE, "emailSubject"];

/// `response.smsMessage`: the wording a text message is delivered with.
const SMS_MESSAGE: &str = "smsMessage";

/// `response.emailMessage`: the wording an email message is delivered with.
const EMAIL_MESSAGE: &str = "emailMessage";

/// The function `lambda_config` routes its messages' wording to, or `None` for a configuration
/// that names none — including one whose member carries the empty ARN its shape defaults to.
#[must_use]
pub fn configured(lambda_config: &LambdaConfigType) -> Option<&str> {
    lambda_config
        .custom_message
        .as_deref()
        .filter(|arn| !arn.is_empty())
}

/// One custom message invocation an operation owes: the invocation itself and the medium whose
/// wording it asks for.
#[derive(Debug, Clone)]
pub struct Pending {
    dispatch: Dispatch,
    sender: Sender,
}

/// The custom message invocation a message issued to `user` of `pool` over `sender`'s medium
/// owes, or `None` when the pool configures no custom message function and so its own wording
/// is what goes out.
///
/// Built while the caller still holds the state lock — the event reports the pool and the user
/// as the issuing operation left them — and invoked by [`invoke`] once that lock is released.
#[must_use]
pub fn pending(
    pool: &UserPool,
    user: &User,
    sender: Sender,
    stage: Stage,
    caller: Caller<'_>,
    client_metadata: Option<&HashMap<String, String>>,
) -> Option<Pending> {
    let function_arn = configured(&pool.lambda_config)?.to_string();
    let mut members = Map::new();
    members.insert(
        "codeParameter".to_string(),
        Value::String(CODE_PARAMETER.to_string()),
    );
    // Only the invitation an administrative create sends names its recipient in its body, so
    // only that stage's event carries the placeholder for the name.
    if stage == Stage::AdminCreateUser {
        members.insert(
            "usernameParameter".to_string(),
            Value::String(USERNAME_PARAMETER.to_string()),
        );
    }
    let mut event = super::event(
        pool,
        user,
        &trigger_source(stage),
        super::EVENT_VERSION,
        caller,
        client_metadata,
        members,
    );
    // The response half carries the members this trigger reads back, each asking for nothing:
    // the function words the ones it decides and leaves the rest as delivered, and a message
    // the function worded nothing of is delivered with the pool's own wording.
    if let Some(response) = event
        .get_mut(super::RESPONSE_HALF)
        .and_then(Value::as_object_mut)
    {
        for member in WORDING_MEMBERS {
            response.insert(member.to_string(), Value::Null);
        }
    }
    Some(Pending {
        dispatch: super::dispatch(pool, function_arn, TRIGGER, event),
        sender,
    })
}

/// This trigger's `triggerSource` for `stage`: the trigger's name and the stage that issued the
/// message.
#[must_use]
pub fn trigger_source(stage: Stage) -> String {
    format!("{TRIGGER}_{}", stage.name())
}

/// Ask the function for the wording of the pending message, and report what the trigger leaves
/// the issuing operation to answer.
///
/// Call it with the state lock RELEASED: holding it across the invocation would make every
/// other request of this service wait on the function. Whether the issuing operation has
/// already recorded what the message announces is that operation's own decision — a
/// password reset composes its message before recording either the status it moves or the
/// code it sends — and this call writes nothing either way.
pub async fn invoke(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    owed: Pending,
) -> Result<(), Failure> {
    let answered = super::invoke_gating(ctx, &owed.dispatch).await?;
    match wording_defect(&answered, owed.sender) {
        Some(defect) => Err(Failure::Unrecognized(defect.to_string())),
        None => Ok(()),
    }
}

/// What is wrong with the wording `answered` returned for a message travelling over `sender`'s
/// medium, or `None` when there is nothing wrong with it.
///
/// The function is handed the PLACEHOLDER the code is delivered in and never the code itself,
/// and the message it words "must include the codeParameter value that you received in the
/// request" (Amazon Cognito's custom message event, of both `response.emailMessage` and
/// `response.smsMessage`): wording that drops the placeholder is wording the code cannot be
/// delivered in, so Amazon Cognito "encounters an invalid Lambda response" (this service's
/// model for `InvalidLambdaResponseException`) rather than delivering a message with no code in
/// it. A function that worded nothing for this medium asked for the pool's own message, which
/// carries the code itself, so it has nothing to leave out.
fn wording_defect(answered: &Value, sender: Sender) -> Option<&'static str> {
    let member = match sender {
        Sender::Email => EMAIL_MESSAGE,
        Sender::Sms => SMS_MESSAGE,
    };
    let worded = answered
        .get(super::RESPONSE_HALF)?
        .get(member)?
        .as_str()
        .filter(|worded| !worded.is_empty())?;
    (!worded.contains(CODE_PARAMETER)).then_some(NO_CODE_PARAMETER_ERROR)
}

/// How this service states wording the code cannot be delivered in: the placeholder the event
/// handed the function is not in the message the function returned. Fixed text, quoting none of
/// the wording back.
const NO_CODE_PARAMETER_ERROR: &str =
    "The custom message does not contain the code parameter {####}";

#[cfg(test)]
mod tests {
    use super::*;

    /// Every `triggerSource` this trigger reports names the trigger, so a function can tell a
    /// custom message event from any other event it is handed, and the stage that issued the
    /// message.
    ///
    /// The stages are the ones a pool issues a message at, which is the set a custom sender
    /// serves too: the two triggers sit at the same point, one wording the message the pool
    /// delivers and one delivering it instead.
    #[test]
    fn every_trigger_source_names_the_custom_message_trigger_and_its_stage() {
        let stages = [
            (Stage::SignUp, "SignUp"),
            (Stage::AdminCreateUser, "AdminCreateUser"),
            (Stage::ResendCode, "ResendCode"),
            (Stage::ForgotPassword, "ForgotPassword"),
            (Stage::UpdateUserAttribute, "UpdateUserAttribute"),
            (Stage::VerifyUserAttribute, "VerifyUserAttribute"),
            (Stage::Authentication, "Authentication"),
        ];
        for (stage, name) in stages {
            let reported = trigger_source(stage);
            assert_eq!(
                reported,
                format!("{TRIGGER}_{name}"),
                "case {stage:?}: expected the trigger and the stage, got {reported:?}"
            );
        }
    }

    /// The wording the function returns for the medium the message travels over is the one
    /// read: wording that keeps the placeholder is wording the code is delivered in, wording
    /// that drops it is not, and wording returned for the OTHER medium decides nothing.
    #[test]
    fn the_mediums_own_wording_has_to_keep_the_code_parameter() {
        // (case, the answered event, the medium, whether the wording is refused)
        let rows = [
            (
                "email wording keeping the placeholder",
                ::serde_json::json!({"response": {EMAIL_MESSAGE: "Your code is {####}"}}),
                Sender::Email,
                false,
            ),
            (
                "email wording dropping the placeholder",
                ::serde_json::json!({"response": {EMAIL_MESSAGE: "Welcome aboard"}}),
                Sender::Email,
                true,
            ),
            (
                "text wording dropping the placeholder",
                ::serde_json::json!({"response": {SMS_MESSAGE: "Welcome aboard"}}),
                Sender::Sms,
                true,
            ),
            (
                "text wording beside email wording that drops it",
                ::serde_json::json!({
                    "response": {SMS_MESSAGE: "Your code is {####}", EMAIL_MESSAGE: "Welcome"}
                }),
                Sender::Sms,
                false,
            ),
            (
                "an answer wording nothing at all",
                ::serde_json::json!({"response": {}}),
                Sender::Email,
                false,
            ),
            (
                "an answer leaving the medium's wording null",
                ::serde_json::json!({"response": {EMAIL_MESSAGE: Value::Null}}),
                Sender::Email,
                false,
            ),
            (
                "an answer wording the medium with the empty message",
                ::serde_json::json!({"response": {SMS_MESSAGE: ""}}),
                Sender::Sms,
                false,
            ),
        ];
        for (case, answered, sender, refused) in rows {
            let defect = wording_defect(&answered, sender);
            assert_eq!(
                defect.is_some(),
                refused,
                "case {case:?}: expected refused={refused}, got {defect:?}"
            );
        }
    }
}
