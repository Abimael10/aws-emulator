// SPDX-License-Identifier: CC0-1.0

//! WHICH `targetArn` a command-execution request may address, and how the two ways a value
//! fails are told apart.
//!
//! `GetCommandExecution$targetArn` and `DeleteCommandExecution$targetArn` are "the ARN of
//! the device on which the command execution is being performed" / "of the target device",
//! i.e. `arn:<partition>:iot:<region>:<account>:thing/<name>`. WHICH device such a request
//! names is decided by the ARN ([`command_execution_target`] — its form, its service, its
//! partition, its account and its resource type) and then by the account's THING REGISTRY
//! ([`super::ComAmazonawsIotAwsiotserviceState::command_execution_device`]), and what a
//! value that names none is refused as depends on what the value is:
//!
//! * A value that is not an ARN at all carries no account to read it in, so it is a
//!   MALFORMED member: the modelled `ValidationException` (HTTP 400) with
//!   [`INVALID_COMMAND_EXECUTION_TARGET_MESSAGE`].
//! * A value that IS an ARN but names no device of the CALLER's own account — another
//!   account's or another partition's ARN, another service's, another IoT resource type's, a
//!   `thing/` ARN whose name no `/things/{thingName}` request could address, or one naming a
//!   thing this account's registry does not hold — is refused as an AUTHORIZATION failure:
//!   HTTP 403 with [`TARGET_NOT_IN_ACCOUNT_MESSAGE`], which states that one condition. It is
//!   not a constraint violation, so a caller can tell a permissions problem from a malformed
//!   request instead of retrying a "fixed" ARN forever.
//!
//! What the target does NOT decide is whether that device HOLDS anything: whether the named
//! device holds the named execution is the EXECUTION store's question, answered by the reader
//! as the modelled not-found error rather than as a permissions problem the caller can never
//! satisfy.

/// How this service refuses a command-execution `targetArn` that is no ARN at all.
///
/// FIXED text naming the MEMBER that was not accepted, interpolating nothing: the sentence is
/// about the member rather than about the value one request happened to send, so the offending
/// value is not echoed back.
pub const INVALID_COMMAND_EXECUTION_TARGET_MESSAGE: &str = "Invalid targetArn";

/// How this service refuses a command-execution `targetArn` that names no device of the
/// caller's own account.
///
/// A CONSTANT with no variable part: the refusal states that the ARN was looked for among the
/// account's devices and found to name none of them, and names neither the ARN nor the account
/// — an authorization answer tells the caller nothing about what the account holds.
pub const TARGET_NOT_IN_ACCOUNT_MESSAGE: &str = "targetArn not found in account";

/// Why a command-execution `targetArn` was not accepted.
///
/// The two variants are DIFFERENT refusals — a malformed member and an authorization failure
/// — at different statuses, so a handler maps one value onto the answer for that case and the
/// reader and the deleter of one execution cannot classify one value two ways.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandExecutionTargetRefusal {
    /// The value is not an ARN, so there is nothing to resolve: the modelled 400.
    Malformed,
    /// The value is an ARN that names no device of the caller's account: the 403.
    NotInAccount,
}

/// The ACCOUNT field of `value` when it is a well-formed resource ARN at all, and `None`
/// when it is not one.
///
/// The parse is [`::service_common::arn::parse`], the ONE reading of a caller-supplied ARN
/// this tree has, so what counts as an ARN here and in every other service is one answer.
fn arn_account(value: &str) -> Option<&str> {
    ::service_common::arn::parse(value).map(|arn| arn.account)
}

/// The device NAME a command-execution request's ARN names, or why the supplied value names
/// none — the half of the question the value alone decides.
///
/// The ARN is read with [`super::parse_job_target`], the one rule this service reads a
/// caller-supplied IoT target ARN with, and the ACCOUNT field is then compared with this
/// emulator's own account: only `arn:<partition>:iot:<region>:<this account>:thing/<name>`
/// can name a device of the caller. Everything else is refused, as one of the two classes
/// [`CommandExecutionTargetRefusal`] names.
///
/// Whether the account's registry HOLDS a device of that name is the other half, taken by
/// [`super::ComAmazonawsIotAwsiotserviceState::command_execution_device`] — which is what
/// every command-execution request resolves its target through.
pub fn command_execution_target(
    target_arn: &str,
) -> Result<String, CommandExecutionTargetRefusal> {
    let Some(account) = arn_account(target_arn) else {
        return Err(CommandExecutionTargetRefusal::Malformed);
    };
    match super::parse_job_target(target_arn) {
        Ok(super::JobTarget::Thing(thing_name))
            if account == ::engine::emulator_account_id() =>
        {
            Ok(thing_name)
        }
        _ => Err(CommandExecutionTargetRefusal::NotInAccount),
    }
}

impl super::ComAmazonawsIotAwsiotserviceState {
    /// The DEVICE a command-execution request names, resolved against the account's THING
    /// REGISTRY, or why the supplied value names none.
    ///
    /// The whole question [`command_execution_target`] answers, and then the one thing an
    /// ARN cannot answer by itself: whether this account's registry holds a device of that
    /// name. "targetArn not found in account" ([`TARGET_NOT_IN_ACCOUNT_MESSAGE`]) is the
    /// answer to exactly that question, so a `thing/` ARN of this account naming a device
    /// the registry does not hold is refused by it like an ARN of another account's — the
    /// value names no device of the caller either way, and an authorization answer tells
    /// the caller nothing further about what the account holds.
    ///
    /// What is still NOT decided here is whether that device HOLDS the named execution:
    /// that is the execution store's question, which the reader answers as the modelled
    /// not-found error and the deleter as a no-op.
    pub fn command_execution_device(
        &self,
        target_arn: &str,
    ) -> Result<String, CommandExecutionTargetRefusal> {
        let thing_name = command_execution_target(target_arn)?;
        if !self.things.contains_key(&thing_name) {
            return Err(CommandExecutionTargetRefusal::NotInAccount);
        }
        Ok(thing_name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: only an IoT `thing/` ARN of this emulator's own account can name a device; a
    /// value that is no ARN is malformed, and every other ARN names no device of the account.
    /// This is the half the VALUE decides, so the registry is no part of it.
    ///
    /// The rows span what the rule generalises over: the resource type the ARN names, its
    /// service, its partition, its account, its region, whether the thing name is one a
    /// request could address, and whether the value is an ARN at all.
    #[test]
    fn only_a_thing_arn_of_this_account_names_a_device() {
        let account = ::engine::emulator_account_id();
        // (case, targetArn, the device it names or why it names none)
        let rows: Vec<(&str, String, Result<String, CommandExecutionTargetRefusal>)> = vec![
            (
                "a thing ARN of this account",
                format!("arn:aws:iot:us-east-1:{account}:thing/laser-1"),
                Ok("laser-1".to_string()),
            ),
            (
                "a thing ARN of this account in another region",
                format!("arn:aws:iot:eu-west-1:{account}:thing/laser-1"),
                Ok("laser-1".to_string()),
            ),
            (
                // The value alone cannot say whether the registry holds the device: the
                // ARN's own reading answers the name, and the registry answers the rest.
                "a thing ARN of this account naming a device the registry does not hold",
                format!("arn:aws:iot:us-east-1:{account}:thing/laser-never-created"),
                Ok("laser-never-created".to_string()),
            ),
            (
                "a thing ARN of another account",
                "arn:aws:iot:us-east-1:111122223333:thing/laser-1".to_string(),
                Err(CommandExecutionTargetRefusal::NotInAccount),
            ),
            (
                "a thing group of this account",
                format!("arn:aws:iot:us-east-1:{account}:thinggroup/fleet"),
                Err(CommandExecutionTargetRefusal::NotInAccount),
            ),
            (
                "another service's thing ARN",
                format!("arn:aws:s3:us-east-1:{account}:thing/laser-1"),
                Err(CommandExecutionTargetRefusal::NotInAccount),
            ),
            (
                "a thing ARN with no name",
                format!("arn:aws:iot:us-east-1:{account}:thing/"),
                Err(CommandExecutionTargetRefusal::NotInAccount),
            ),
            (
                "a value that is no ARN",
                "laser-1".to_string(),
                Err(CommandExecutionTargetRefusal::Malformed),
            ),
            (
                "an ARN with no partition, vendor or resource",
                "arn:::::".to_string(),
                Err(CommandExecutionTargetRefusal::Malformed),
            ),
            (
                "the empty value",
                String::new(),
                Err(CommandExecutionTargetRefusal::Malformed),
            ),
        ];

        for (case, target_arn, expected) in rows {
            let actual = command_execution_target(&target_arn);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected:?} for {target_arn:?}, got {actual:?}"
            );
        }
    }

    /// RULE: a command-execution target is a device the account's REGISTRY holds, so a
    /// `thing/` ARN of this account naming a thing it does not hold names no device of the
    /// account either — the same authorization refusal every other unusable ARN earns.
    ///
    /// The rows span whether the registry holds the named thing, and the ways a value names
    /// no device whatever the registry holds.
    #[test]
    fn a_target_is_a_device_the_registry_holds() {
        let account = ::engine::emulator_account_id();
        let mut store = super::super::ComAmazonawsIotAwsiotserviceState::default();
        let registered = super::super::RegisteredThing::new(
            "laser-registered".to_string(),
            "thing-id".to_string(),
            format!("arn:aws:iot:us-east-1:{account}:thing/laser-registered"),
            super::super::ThingConfiguration::default(),
        );
        store
            .things
            .insert(registered.thing_name.clone(), registered);

        // (case, targetArn, the device it names or why it names none)
        let rows: Vec<(&str, String, Result<String, CommandExecutionTargetRefusal>)> = vec![
            (
                "a thing ARN naming a registered device",
                format!("arn:aws:iot:us-east-1:{account}:thing/laser-registered"),
                Ok("laser-registered".to_string()),
            ),
            (
                "the same device named in another region",
                format!("arn:aws:iot:eu-west-1:{account}:thing/laser-registered"),
                Ok("laser-registered".to_string()),
            ),
            (
                "a thing ARN naming a device the registry does not hold",
                format!("arn:aws:iot:us-east-1:{account}:thing/laser-never-created"),
                Err(CommandExecutionTargetRefusal::NotInAccount),
            ),
            (
                "a registered device's name under another account",
                "arn:aws:iot:us-east-1:111122223333:thing/laser-registered".to_string(),
                Err(CommandExecutionTargetRefusal::NotInAccount),
            ),
            (
                "a registered device's name under another resource type",
                format!("arn:aws:iot:us-east-1:{account}:thinggroup/laser-registered"),
                Err(CommandExecutionTargetRefusal::NotInAccount),
            ),
            (
                "a value that is no ARN",
                "laser-registered".to_string(),
                Err(CommandExecutionTargetRefusal::Malformed),
            ),
        ];

        for (case, target_arn, expected) in rows {
            let actual = store.command_execution_device(&target_arn);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected:?} for {target_arn:?}, got {actual:?}"
            );
        }
    }
}
