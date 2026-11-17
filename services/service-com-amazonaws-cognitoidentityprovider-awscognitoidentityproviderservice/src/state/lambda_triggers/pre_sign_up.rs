// SPDX-License-Identifier: CC0-1.0

//! The pre-sign-up trigger: the one place a pool's `LambdaConfig.PreSignUp` becomes an
//! invocation of the function that member names.
//!
//! "The configuration of a pre sign-up Lambda trigger in a user pool. This trigger evaluates
//! new users and can bypass confirmation, link a federated user profile, or block sign-up
//! requests" (`LambdaConfigType$PreSignUp`, this service's model). The stage sits BEFORE the
//! account exists: the function evaluates the account a registration proposes, so the
//! operation that fired it is GATED on what the function answered ([`super::invoke_gating`])
//! — a function that could not be invoked, one that did not answer inside the trigger budget
//! and one that RAN AND REPORTED AN ERROR each refuse the registration, which is how a
//! function blocks a sign-up — and what a RETURNING function answered is what gets stored
//! ([`Decision`]).
//!
//! Two stages reach it, each with its own `triggerSource` ([`Registration`]): the self-service
//! registration and the administrative create. Both model `ValidationData` as "temporary user
//! attributes that contribute to the outcomes of your pre sign-up Lambda trigger … Your
//! function can automatically confirm and verify select users" (this service's model), which
//! is both why they are the stages that reach this trigger and what the answer decides.
//!
//! The work splits the way every other trigger of this service does ([`super`]):
//! [`pre_sign_up`] resolves the member and builds the event from the pool read under the state
//! lock, and [`invoke_pre_sign_up`] performs the invocation once that lock is RELEASED.
//!
//! The event names no stored user — there is none yet, which is the whole point of the stage —
//! so its `userName` and `request.userAttributes` are the account the registration WOULD
//! store, rendered from what the request submitted.

use std::collections::{BTreeMap, HashMap};

use ::serde_json::{Map, Value};

use super::{Caller, Dispatch, Failure};
use crate::ops::types::AttributeType;
use crate::state::{User, UserPool, UserStatus};

/// The trigger whose configured function every pre-sign-up event is delivered to, as a
/// failure of that trigger names it ([`super::failure_message`]).
pub const PRE_SIGN_UP_TRIGGER: &str = "PreSignUp";

/// The response member a function bypasses the registrant's own confirmation with.
const AUTO_CONFIRM_USER: &str = "autoConfirmUser";

/// The response member a function marks a registrant's value verified with, beside the
/// attribute whose stored value it is about: a pre-sign-up function "can automatically
/// confirm and verify select users" (`SignUp.ValidationData` in this service's model), and
/// the values a user pool verifies are `email` and `phone_number` (`VerifiedAttributeType`),
/// whose flags are [`crate::state::verified_flag`]'s.
const AUTO_VERIFY_MEMBERS: [(&str, &str); 2] = [
    ("autoVerifyEmail", "email"),
    ("autoVerifyPhone", "phone_number"),
];

/// Which registration asked a pool to evaluate a new user, which is what the event's
/// `triggerSource` reports: the trigger's name and the stage that reached it.
///
/// The two operations that register an account are the two that model `ValidationData` as
/// attributes "that contribute to the outcomes of your pre sign-up Lambda trigger" (this
/// service's model), and a function tells them apart by this member alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Registration {
    /// A self-service registration, by `SignUp`.
    SignUp,
    /// An account an administrator created, by `AdminCreateUser`.
    AdminCreateUser,
}

impl Registration {
    /// This stage's `triggerSource`.
    #[must_use]
    pub fn trigger_source(self) -> &'static str {
        match self {
            Self::SignUp => "PreSignUp_SignUp",
            Self::AdminCreateUser => "PreSignUp_AdminCreateUser",
        }
    }
}

/// The account a registration proposes, as the event reports it.
///
/// Nothing is stored yet, so these are the values the registration WOULD store: the name the
/// account would be held under — which in a pool that mints its usernames is the id the pool
/// minted and not the sign-in value the request submitted — and the attribute values that
/// account would carry.
#[derive(Debug, Clone, Copy)]
pub struct Proposed<'request> {
    /// The username the account would be stored under.
    pub username: &'request str,
    /// The attribute values that account would carry.
    pub attributes: &'request BTreeMap<String, String>,
    /// `ValidationData`: the temporary attributes the request collected for this trigger,
    /// which are not retained.
    pub validation_data: Option<&'request [AttributeType]>,
}

/// The pre-sign-up invocation a registration of `proposed` in `pool` owes, or `None` when the
/// pool configures no pre-sign-up function and so the registration is evaluated by nobody.
///
/// Built while the caller still holds the state lock, and invoked by [`invoke_pre_sign_up`]
/// once that lock is released.
#[must_use]
pub fn pre_sign_up(
    pool: &UserPool,
    proposed: Proposed<'_>,
    registration: Registration,
    caller: Caller<'_>,
    client_metadata: Option<&HashMap<String, String>>,
) -> Option<Dispatch> {
    let function_arn = pool
        .lambda_config
        .pre_sign_up
        .clone()
        .filter(|arn| !arn.is_empty())?;

    let mut request = Map::new();
    request.insert(
        "userAttributes".to_string(),
        Value::Object(attribute_members(proposed.attributes)),
    );
    // "Temporary user attributes that contribute to the outcomes of your pre sign-up Lambda
    // trigger … for custom validation of information that you collect from your users but
    // don't need to retain" (this service's model): the data THIS request collected, and a
    // request that collected none carries none.
    if let Some(submitted) = proposed
        .validation_data
        .filter(|submitted| !submitted.is_empty())
    {
        request.insert(
            "validationData".to_string(),
            Value::Object(validation_members(submitted)),
        );
    }
    // `ClientMetadata` is "a map of custom key-value pairs that you can provide as input for
    // any custom workflows that this action triggers" (every operation that models it), so the
    // member appears only for a request that assigned one.
    if let Some(metadata) = client_metadata.filter(|metadata| !metadata.is_empty()) {
        request.insert(
            "clientMetadata".to_string(),
            Value::Object(super::metadata_members(metadata)),
        );
    }

    // Built here rather than through [`super::event`]: that event reports a STORED user's
    // attributes and its status, and this stage has neither — the account the function
    // evaluates does not exist yet.
    let event = ::serde_json::json!({
        "version": super::EVENT_VERSION,
        "triggerSource": registration.trigger_source(),
        // The pool's own Region and id, not the request's: the event names the resource the
        // trigger is configured on.
        "region": pool.region,
        "userPoolId": pool.id,
        "userName": proposed.username,
        "callerContext": {
            "awsSdkVersion": caller
                .aws_sdk_version
                .unwrap_or(super::UNKNOWN_AWS_SDK_VERSION),
            "clientId": caller.client_id.unwrap_or(super::CLIENT_ID_NOT_APPLICABLE),
        },
        "request": Value::Object(request),
        // The response half carries the members this trigger reads back, each asking for
        // nothing: the function amends the ones it decides and leaves the rest as delivered,
        // so a function that reads one before setting it finds it there.
        "response": Value::Object(delivered_response()),
    });
    Some(super::dispatch(
        pool,
        function_arn,
        PRE_SIGN_UP_TRIGGER,
        event,
    ))
}

/// The response half a pre-sign-up event is delivered with: every member this trigger reads
/// back, asking for nothing.
fn delivered_response() -> Map<String, Value> {
    let mut response = Map::new();
    response.insert(AUTO_CONFIRM_USER.to_string(), Value::Bool(false));
    for (member, _) in AUTO_VERIFY_MEMBERS {
        response.insert(member.to_string(), Value::Bool(false));
    }
    response
}

/// The proposed attributes as a trigger event's `request.userAttributes`.
fn attribute_members(attributes: &BTreeMap<String, String>) -> Map<String, Value> {
    attributes
        .iter()
        .map(|(name, value)| (name.clone(), Value::String(value.clone())))
        .collect()
}

/// `ValidationData` as a trigger event's `request.validationData`: the key-value pairs the
/// request collected, a pair submitted without a value carrying the empty one.
fn validation_members(submitted: &[AttributeType]) -> Map<String, Value> {
    submitted
        .iter()
        .map(|attribute| {
            (
                attribute.name.clone(),
                Value::String(attribute.value.clone().unwrap_or_default()),
            )
        })
        .collect()
}

/// What a returning pre-sign-up function decided about the account the registration proposed.
///
/// The trigger "can bypass confirmation" (`LambdaConfigType$PreSignUp`) and its function "can
/// automatically confirm and verify select users" (`SignUp.ValidationData`), so the answer
/// decides two things about the account that is about to be stored: whether the registrant
/// still owes its own confirmation, and which of its values the pool takes as verified. A
/// member stated as anything other than a boolean asks for nothing, the event having been
/// delivered with a boolean in its place ([`delivered_response`]).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Decision {
    /// Whether the registrant's own confirmation is bypassed.
    confirm_user: bool,
    /// The attributes whose stored value the function marked verified, in the order
    /// [`AUTO_VERIFY_MEMBERS`] names them.
    verified_attributes: Vec<&'static str>,
}

impl Decision {
    /// What a registration nothing was invoked for decided: nothing, which is the pool that
    /// configures no pre-sign-up function.
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }

    /// What the function answered, read out of the event it returned.
    ///
    /// Each member is read in the type it was DELIVERED in: the event handed the function a
    /// boolean for every member this trigger reads back ([`delivered_response`]), so a
    /// boolean is what an answer states one as. A member the answer states as anything else
    /// asks for neither the bypass nor the verification: there is no verdict in it to read,
    /// which is an answer Amazon Cognito cannot use at all
    /// ([`Failure::Unrecognized`]), and a member the answer leaves out asks for nothing, as
    /// it was delivered.
    pub fn read(answered: &Value) -> Result<Self, Failure> {
        let asked = |member: &str| -> Result<bool, Failure> {
            match answered
                .get("response")
                .and_then(|response| response.get(member))
            {
                None | Some(Value::Null) => Ok(false),
                Some(Value::Bool(asked)) => Ok(*asked),
                Some(_) => Err(Failure::Unrecognized(
                    super::UNRECOGNIZABLE_OUTPUT_MESSAGE.to_string(),
                )),
            }
        };
        let confirm_user = asked(AUTO_CONFIRM_USER)?;
        let mut verified_attributes = Vec::new();
        for (member, attribute) in AUTO_VERIFY_MEMBERS {
            if asked(member)? {
                verified_attributes.push(attribute);
            }
        }
        Ok(Self {
            confirm_user,
            verified_attributes,
        })
    }

    /// Apply this decision to `user`, the account the registration is about to store.
    ///
    /// The confirmation that is bypassed is the REGISTRANT's own — the one a user that signed
    /// itself up answers with the code the sign-up sent — so it is the account still awaiting
    /// that answer (`UNCONFIRMED`) that is confirmed instead. An account in any other status
    /// owes no such answer: an administratively created one "must change their password to
    /// complete sign-in" (`UserStatusType` in this service's model), which is not a
    /// confirmation this trigger can bypass.
    ///
    /// A value is marked verified only where the account carries one: the flag records
    /// whether THAT value was confirmed, so there is nothing to record for an attribute the
    /// registration stores no value for.
    pub fn apply(&self, user: &mut User) {
        if self.confirm_user && user.status == UserStatus::Unconfirmed {
            user.status = UserStatus::Confirmed;
        }
        for attribute in &self.verified_attributes {
            let Some(flag) = crate::state::verified_flag(attribute) else {
                continue;
            };
            let holds_a_value = user
                .attributes
                .get(*attribute)
                .is_some_and(|value| !value.is_empty());
            if holds_a_value {
                user.attributes
                    .insert(flag.to_string(), crate::state::FLAG_CONFIRMED.to_string());
            }
        }
    }
}

/// Perform the pre-sign-up invocation `dispatch` names, and report what the function decided.
///
/// Call it with the state lock RELEASED: holding it across the invocation would make every
/// other request of this service wait on the function. `None` is a pool that configures no
/// pre-sign-up function, which decides nothing and invokes nothing.
///
/// Every failure is the registering operation's to report, because this trigger GATES it: the
/// function evaluates an account that does not exist yet, so an invocation that reached no
/// function, one that was not answered inside the trigger budget, a function that RAN AND
/// REPORTED AN ERROR ([`super::invoke_gating`]) and one that answered something there is no
/// verdict to read in ([`Decision::read`]) each leave the operation nothing to store.
pub async fn invoke_pre_sign_up(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    dispatch: Option<&Dispatch>,
) -> Result<Decision, Failure> {
    match dispatch {
        Some(dispatch) => {
            let answered = super::invoke_gating(ctx, dispatch).await?;
            Decision::read(&answered)
        }
        None => Ok(Decision::none()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every `triggerSource` this trigger reports names the trigger and the stage that
    /// reached it, so a function can tell an administrative create from a self-service
    /// registration by that member alone.
    #[test]
    fn every_trigger_source_names_the_trigger_and_its_stage() {
        // The two stages this service registers an account through.
        let cases = [
            (Registration::SignUp, "SignUp"),
            (Registration::AdminCreateUser, "AdminCreateUser"),
        ];
        for (registration, stage) in cases {
            let reported = registration.trigger_source();
            assert_eq!(
                reported,
                format!("{PRE_SIGN_UP_TRIGGER}_{stage}"),
                "case {registration:?}: expected the trigger and the stage, got {reported:?}"
            );
        }
    }

    /// One row of the table below: the case, the response half the function answered with,
    /// and the decision that answer amounts to.
    type DecisionRow = (&'static str, Value, (bool, Vec<&'static str>));

    /// The decision read out of an answered event: what the function asked for, and nothing
    /// for a member it did not state at all.
    #[test]
    fn the_decision_is_what_the_function_answered() {
        // (case, the response half the function answered with, expected decision)
        let rows: [DecisionRow; 7] = [
            (
                "a function that bypassed the confirmation alone",
                ::serde_json::json!({ "autoConfirmUser": true }),
                (true, vec![]),
            ),
            (
                "a function that verified the email address alone",
                ::serde_json::json!({ "autoVerifyEmail": true }),
                (false, vec!["email"]),
            ),
            (
                "a function that verified the phone number alone",
                ::serde_json::json!({ "autoVerifyPhone": true }),
                (false, vec!["phone_number"]),
            ),
            (
                "a function that asked for all three",
                ::serde_json::json!({
                    "autoConfirmUser": true,
                    "autoVerifyEmail": true,
                    "autoVerifyPhone": true,
                }),
                (true, vec!["email", "phone_number"]),
            ),
            (
                "a function that answered the event as it was delivered",
                ::serde_json::json!({
                    "autoConfirmUser": false,
                    "autoVerifyEmail": false,
                    "autoVerifyPhone": false,
                }),
                (false, vec![]),
            ),
            (
                "a function that answered an empty response half",
                ::serde_json::json!({}),
                (false, vec![]),
            ),
            (
                "a function that answered a response half naming other members",
                ::serde_json::json!({ "autoConfirmUserPlease": true }),
                (false, vec![]),
            ),
        ];

        for (case, response, (confirm_user, verified_attributes)) in rows {
            let answered = ::serde_json::json!({ "response": response });
            let read = Decision::read(&answered);
            let expected = Ok(Decision {
                confirm_user,
                verified_attributes,
            });
            assert_eq!(
                read, expected,
                "case {case:?}: expected {expected:?}, got {read:?}"
            );
        }
    }

    /// A member answered in a type the event did not deliver it in carries no verdict, so
    /// the answer is one this service cannot read at all.
    #[test]
    fn a_member_answered_in_another_type_is_an_answer_that_cannot_be_read() {
        // The rows differ along WHICH member carries the wrong type and which JSON type it
        // is, across every member this trigger reads back.
        let rows = [
            (
                "the confirmation stated as text",
                ::serde_json::json!({ "autoConfirmUser": "true" }),
            ),
            (
                "the confirmation stated as a number",
                ::serde_json::json!({ "autoConfirmUser": 1 }),
            ),
            (
                "the email verification stated as an object",
                ::serde_json::json!({ "autoVerifyEmail": { "asked": true } }),
            ),
            (
                "the phone verification stated as a list",
                ::serde_json::json!({ "autoVerifyPhone": [true] }),
            ),
        ];
        for (case, response) in rows {
            let answered = ::serde_json::json!({ "response": response });
            let read = Decision::read(&answered);
            assert_eq!(
                read,
                Err(Failure::Unrecognized(
                    super::super::UNRECOGNIZABLE_OUTPUT_MESSAGE.to_string()
                )),
                "case {case:?}: expected an answer that cannot be read, got {read:?}"
            );
        }
    }

    /// A function that answered no event at all, and one whose answer carries no response
    /// half, decided nothing.
    #[test]
    fn an_answer_with_no_response_half_decides_nothing() {
        let rows = [
            ("no event at all", Value::Null),
            ("an event with no response half", ::serde_json::json!({})),
            (
                "an event whose response half is not an object",
                ::serde_json::json!({ "response": "autoConfirmUser" }),
            ),
        ];
        for (case, answered) in rows {
            let read = Decision::read(&answered);
            assert_eq!(
                read,
                Ok(Decision::none()),
                "case {case:?}: expected nothing decided, got {read:?}"
            );
        }
    }
}
