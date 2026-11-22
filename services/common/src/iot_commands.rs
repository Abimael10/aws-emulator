// SPDX-License-Identifier: CC0-1.0

//! Which IoT COMMANDS exist, and the few facts about one that a reader outside the
//! control plane needs: the index `StartCommandExecution` resolves a `commandArn`
//! against.
//!
//! A command is a CONTROL-PLANE resource — `iot:CreateCommand` creates one,
//! `iot:UpdateCommand` deprecates one, `iot:DeleteCommand` removes it — while the
//! operation that starts an execution of one, `iot-jobs-data:StartCommandExecution`,
//! is served by the IoT Jobs DATA plane, a different crate. Starting an execution is
//! only possible for a command that exists, and only in the form that command admits,
//! so the two services have to consult ONE store rather than each keeping a copy of
//! the answer. This module is that store, and it lives in the shared crate because
//! the engine keys state BY TYPE: a type two service crates share must be declared in
//! a crate they both depend on.
//!
//! It holds an ARN — a command's identity on the wire (`CommandArn`,
//! `arn:<partition>:iot:<region>:<account>:command/<commandId>`) — and the facts a
//! reader outside the control plane DECIDES on ([`CommandFacts`]): whether the
//! command's payload is static, and whether it has been deprecated. It also holds the
//! PAYLOAD a command hands its device ([`CommandPayload`]), because the call that
//! delivers it — `StartCommandExecution`, which publishes the execution's request on the
//! device's reserved MQTT topic — is outside the control plane too. The rest of the
//! command's own record — parameters, description, tags — stays with its owner, so this
//! is an index and not a second copy of the resource.
//!
//! WHO WRITES IT: the owner. `iot:CreateCommand` registers the ARN it mints here with
//! the facts the create decided, `iot:UpdateCommand` records a deprecation it makes,
//! and `iot:DeleteCommand` removes the entry. A `commandArn` this index does not hold
//! names a command that does not exist — which is exactly what
//! `StartCommandExecution` must then refuse. The IoT control plane already carries the
//! same shape of store for the same reason: its `thing_types` / `billing_groups` sets
//! exist so `CreateThing` can refuse a name that is not in them, and stay empty until
//! `CreateThingType` / `CreateBillingGroup` are implemented.
//!
//! Only the TYPE lives here. Each service reaches it through its own
//! `state::command_registry`, beside the accessor for its own store, so this
//! crate needs no dependency on the engine.

use std::collections::BTreeMap;

/// How many commands the index holds at once.
///
/// The set grows only through the control plane's create, so it is
/// client-influenced and has to be bounded; the bound is the IoT Device
/// Management default quota's order of magnitude rather than a published number,
/// and a create past it is refused with [`RegisterRefusal::QuotaExceeded`]
/// instead of growing the store without limit.
pub const MAX_REGISTERED_COMMANDS: usize = 10_000;

/// What a reader outside the control plane has to know about a command to tell
/// whether a request naming it can be served at all.
///
/// Both flags are decided by the control plane and read by the data plane, which is
/// why they live in the shared index rather than being guessed from the ARN (which
/// says nothing about either) or duplicated as a second store.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CommandFacts {
    /// Whether the command's payload is STATIC: it was created with a literal
    /// `payload` and no `payloadTemplate`, so no per-execution parameter can be
    /// substituted into it. `CreateCommand` decides this once, and a command's
    /// payload is not something `UpdateCommand` can change.
    pub static_payload: bool,
    /// Whether the command has been deprecated (`iot:UpdateCommand$deprecated`): it
    /// still exists and is still readable, but it is no longer a command new
    /// executions may be created from.
    pub deprecated: bool,
}

/// The PAYLOAD a command hands its device: the bytes a command execution of it is
/// delivered with, and the content type that says how the device is to read them.
///
/// It lives in the shared index rather than in the owning control plane's own record
/// because the call that DELIVERS it to the device is in another crate
/// (`iot-jobs-data`: `StartCommandExecution`), and the bytes a device is handed must be
/// the bytes the create supplied — not a second copy that could drift from it.
///
/// Only a command created with a literal `payload` (`CreateCommand$payload`, "the payload
/// object for the static command") has one; a command whose payload is generated from a
/// `payloadTemplate` has none here, because nothing has generated those bytes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommandPayload {
    /// `CommandPayload$content`: "the static payload file for the command", verbatim.
    pub content: Vec<u8>,
    /// `CommandPayload$contentType`: "the content type that specifies the format type of
    /// the payload file", e.g. `application/json`. `None` for a payload created without
    /// one, which says nothing about how the bytes are to be read.
    pub content_type: Option<String>,
}

/// One command's entry in the index: the facts a reader DECIDES on, and the payload a
/// device is handed.
///
/// The two are kept apart in the public API — [`CommandFacts`] stays a small `Copy`
/// decision record, while a payload is bytes — so a reader that only has to judge a
/// request never copies a payload to do it.
#[derive(Debug, Clone, Default)]
struct CommandEntry {
    facts: CommandFacts,
    payload: Option<CommandPayload>,
}

/// How the IoT commands surface words a command execution that cannot be found.
///
/// The sentence is fixed around the `executionId` the caller addressed, and it is the
/// same one wherever the answer is given — the control plane's `GetCommandExecution`
/// (404 `ResourceNotFoundException`) and a device's own report on an execution's reserved
/// MQTT response topic ([`crate::iot_mqtt_broker::commands`]) — so one missing execution
/// reads one way.
#[must_use]
pub fn command_execution_not_found_message(execution_id: &str) -> String {
    format!("Command execution {execution_id} cannot be found.")
}

/// Why registering a command was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegisterRefusal {
    /// A command with this ARN is already registered: the caller is creating a
    /// command that exists, which its service reports as a conflict.
    AlreadyRegistered,
    /// The index already holds [`MAX_REGISTERED_COMMANDS`] commands.
    QuotaExceeded,
}

impl std::fmt::Display for RegisterRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AlreadyRegistered => formatter.write_str("the command already exists"),
            Self::QuotaExceeded => write!(
                formatter,
                "the maximum of {MAX_REGISTERED_COMMANDS} commands is already reached"
            ),
        }
    }
}

/// The commands that exist, each with the facts a reader outside the control plane
/// decides on.
///
/// `BTreeMap` (not `HashMap`) so the index has one deterministic order: an index
/// whose content is printed in a log or walked by a future lister must not
/// reorder itself between two runs holding the same commands.
#[derive(Debug, Default)]
pub struct IotCommandRegistry {
    commands: BTreeMap<String, CommandEntry>,
}

impl IotCommandRegistry {
    /// Whether a command with this ARN exists.
    ///
    /// Comparison is exact: an ARN carries the partition, region and account the
    /// command was created in, so a command created in one region is not the
    /// command another region's ARN names.
    #[must_use]
    pub fn contains(&self, command_arn: &str) -> bool {
        self.commands.contains_key(command_arn)
    }

    /// The facts of the command `command_arn` names, or `None` when no such command
    /// exists — the one lookup a reader outside the control plane needs, so
    /// "does it exist?" and "what may be asked of it?" are one answer and cannot
    /// disagree.
    #[must_use]
    pub fn facts(&self, command_arn: &str) -> Option<CommandFacts> {
        self.commands.get(command_arn).map(|entry| entry.facts)
    }

    /// The PAYLOAD the command `command_arn` names hands its device, or `None` when no
    /// such command exists or its create supplied no literal payload — see
    /// [`CommandPayload`].
    #[must_use]
    pub fn payload(&self, command_arn: &str) -> Option<CommandPayload> {
        self.commands.get(command_arn)?.payload.clone()
    }

    /// The same payload, BORROWED — for a reader that has to decide something from the
    /// content type (which topic the payload goes out on) before it copies the bytes, so
    /// a payload that is not going anywhere costs nothing.
    #[must_use]
    pub fn payload_ref(&self, command_arn: &str) -> Option<&CommandPayload> {
        self.commands.get(command_arn)?.payload.as_ref()
    }

    /// Record the payload the command `command_arn` names hands its device, reporting
    /// whether such a command exists.
    ///
    /// It is set BESIDE [`Self::register`] rather than through it so that registering a
    /// command stays the one small call every reader of the index already makes; a
    /// command that does not exist records nothing, because a payload is a fact about a
    /// command and not an entry of its own.
    pub fn set_payload(&mut self, command_arn: &str, payload: Option<CommandPayload>) -> bool {
        match self.commands.get_mut(command_arn) {
            Some(entry) => {
                entry.payload = payload;
                true
            }
            None => false,
        }
    }

    /// Record that the command `command_arn` names exists, with the facts its create
    /// decided, within the index's bound; `Err` says why a caller's create cannot be
    /// recorded.
    pub fn register(
        &mut self,
        command_arn: &str,
        facts: CommandFacts,
    ) -> Result<(), RegisterRefusal> {
        if self.commands.contains_key(command_arn) {
            return Err(RegisterRefusal::AlreadyRegistered);
        }
        if self.commands.len() >= MAX_REGISTERED_COMMANDS {
            return Err(RegisterRefusal::QuotaExceeded);
        }
        self.commands.insert(
            command_arn.to_string(),
            CommandEntry {
                facts,
                payload: None,
            },
        );
        Ok(())
    }

    /// Record that the command `command_arn` names is (or is no longer) deprecated,
    /// reporting whether such a command exists. A command that does not exist records
    /// nothing: the control plane refuses that update itself.
    pub fn set_deprecated(&mut self, command_arn: &str, deprecated: bool) -> bool {
        match self.commands.get_mut(command_arn) {
            Some(entry) => {
                entry.facts.deprecated = deprecated;
                true
            }
            None => false,
        }
    }

    /// Forget the command `command_arn` names, reporting whether it existed.
    pub fn remove(&mut self, command_arn: &str) -> bool {
        self.commands.remove(command_arn).is_some()
    }

    /// How many commands exist.
    #[must_use]
    pub fn len(&self) -> usize {
        self.commands.len()
    }

    /// Whether no command exists.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One row of the payload table: the case, the command's ARN, and the payload its
    /// create recorded (`None` for a create that supplied none).
    type PayloadRow = (&'static str, &'static str, Option<CommandPayload>);

    /// A static-payload command, as `CreateCommand` registers one created with a
    /// literal `payload`.
    const STATIC: CommandFacts = CommandFacts {
        static_payload: true,
        deprecated: false,
    };

    /// A fresh index knows no command, whatever ARN is asked about; one
    /// registered ARN is known and its neighbours are not.
    #[test]
    fn only_a_registered_arn_exists() {
        let mut registry = IotCommandRegistry::default();

        // (case, arn, exists once the FIRST row's ARN is registered)
        let rows: [(&str, &str, bool); 4] = [
            (
                "the registered command",
                "arn:aws:iot:us-east-1:111122223333:command/reboot",
                true,
            ),
            (
                "another command id",
                "arn:aws:iot:us-east-1:111122223333:command/restart",
                false,
            ),
            (
                "the same id in another region",
                "arn:aws:iot:eu-west-1:111122223333:command/reboot",
                false,
            ),
            (
                "the same id in another partition",
                "arn:aws-cn:iot:cn-north-1:111122223333:command/reboot",
                false,
            ),
        ];

        for (case, arn, _) in rows {
            assert!(
                !registry.contains(arn),
                "case {case:?}: an empty index must know no command, but it knows {arn:?}"
            );
        }

        registry
            .register(rows[0].1, STATIC)
            .unwrap_or_else(|refusal| panic!("registering the first command: {refusal}"));

        for (case, arn, expected) in rows {
            let actual = registry.contains(arn);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected contains({arn:?}) == {expected}, got {actual}"
            );
            let facts = registry.facts(arn);
            assert_eq!(
                facts.is_some(),
                expected,
                "case {case:?}: the facts of {arn:?} are known exactly when it exists, got \
                 {facts:?}"
            );
        }
    }

    /// The facts are the ones the registering create decided, and a deprecation is
    /// recorded on the command it names and on no other.
    #[test]
    fn the_facts_of_a_command_are_the_ones_its_owner_recorded() {
        let mut registry = IotCommandRegistry::default();

        // (case, arn, the facts the create decided)
        let rows: [(&str, &str, CommandFacts); 2] = [
            (
                "a command created with a literal payload",
                "arn:aws:iot:us-east-1:111122223333:command/reboot",
                STATIC,
            ),
            (
                "a command created with a payload template",
                "arn:aws:iot:us-east-1:111122223333:command/set-temperature",
                CommandFacts::default(),
            ),
        ];

        for (case, arn, facts) in rows {
            registry
                .register(arn, facts)
                .unwrap_or_else(|refusal| panic!("case {case:?}: registering it: {refusal}"));
        }
        for (case, arn, expected) in rows {
            let actual = registry.facts(arn);
            assert_eq!(
                actual,
                Some(expected),
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }

        // (case, the command deprecated, whether it exists, the expected flag per row)
        assert!(
            registry.set_deprecated(rows[0].1, true),
            "deprecating a command that exists reports it existed"
        );
        assert_eq!(
            (
                registry.facts(rows[0].1).map(|facts| facts.deprecated),
                registry.facts(rows[1].1).map(|facts| facts.deprecated),
            ),
            (Some(true), Some(false)),
            "a deprecation is recorded on the command it names and on no other, got {registry:?}"
        );
        assert!(
            registry.set_deprecated(rows[0].1, false),
            "undeprecating it reports it existed"
        );
        assert_eq!(
            registry.facts(rows[0].1).map(|facts| facts.deprecated),
            Some(false),
            "an undeprecated command is one new executions may be created from again, got \
             {registry:?}"
        );
        assert!(
            !registry.set_deprecated("arn:aws:iot:us-east-1:111122223333:command/absent", true),
            "deprecating a command that does not exist records nothing"
        );
        assert!(
            registry
                .facts("arn:aws:iot:us-east-1:111122223333:command/absent")
                .is_none(),
            "and leaves it non-existent, got {registry:?}"
        );
    }

    /// The payload a command hands its device is the one its create recorded, is kept
    /// per command, and goes away with the command.
    ///
    /// The rows span what the rule generalises over: a payload with a content type, one
    /// without, an EMPTY payload (a command whose create supplied no literal payload at
    /// all), and a payload of bytes that are no text.
    #[test]
    fn the_payload_of_a_command_is_the_one_its_owner_recorded() {
        let mut registry = IotCommandRegistry::default();

        // (case, arn, the payload the create recorded)
        let rows: [PayloadRow; 4] = [
            (
                "a JSON payload",
                "arn:aws:iot:us-east-1:111122223333:command/reboot",
                Some(CommandPayload {
                    content: br#"{"action":"reboot"}"#.to_vec(),
                    content_type: Some("application/json".to_string()),
                }),
            ),
            (
                "a payload with no content type",
                "arn:aws:iot:us-east-1:111122223333:command/blink",
                Some(CommandPayload {
                    content: b"blink".to_vec(),
                    content_type: None,
                }),
            ),
            (
                "a command created with no literal payload",
                "arn:aws:iot:us-east-1:111122223333:command/set-temperature",
                None,
            ),
            (
                "a payload of bytes that are no text",
                "arn:aws:iot:us-east-1:111122223333:command/firmware",
                Some(CommandPayload {
                    content: vec![0x00, 0x9f, 0x10, 0xff],
                    content_type: Some("application/octet-stream".to_string()),
                }),
            ),
        ];

        for (case, arn, payload) in &rows {
            assert!(
                !registry.set_payload(arn, payload.clone()),
                "case {case:?}: a command that does not exist records no payload"
            );
            registry
                .register(arn, CommandFacts::default())
                .unwrap_or_else(|refusal| panic!("case {case:?}: registering it: {refusal}"));
            assert!(
                registry.set_payload(arn, payload.clone()),
                "case {case:?}: recording the payload of a registered command reports it exists"
            );
        }
        for (case, arn, expected) in &rows {
            let actual = registry.payload(arn);
            assert_eq!(
                actual, *expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }

        let (case, arn, _) = rows[0];
        assert!(registry.remove(arn), "case {case:?}: removing it reports it existed");
        assert!(
            registry.payload(arn).is_none(),
            "case {case:?}: a removed command hands no payload to anything, got {:?}",
            registry.payload(arn)
        );
        assert_eq!(
            registry.payload("arn:aws:iot:us-east-1:111122223333:command/absent"),
            None,
            "a command that never existed has no payload"
        );
    }

    /// Registering is idempotent-refusing (a second create of one command is a
    /// conflict), removal takes the command back out, and the index is bounded.
    #[test]
    fn registering_is_bounded_and_reversible() {
        let mut registry = IotCommandRegistry::default();
        let arn = "arn:aws:iot:us-east-1:111122223333:command/reboot";

        assert_eq!(
            registry.register(arn, STATIC),
            Ok(()),
            "the first register succeeds"
        );
        assert_eq!(
            registry.register(arn, CommandFacts::default()),
            Err(RegisterRefusal::AlreadyRegistered),
            "case {arn:?}: a second register of one command is a conflict"
        );
        assert_eq!(
            registry.facts(arn),
            Some(STATIC),
            "case {arn:?}: a refused register leaves the command's facts as they were"
        );
        assert!(registry.remove(arn), "case {arn:?}: removing it reports it existed");
        assert!(
            !registry.contains(arn) && registry.is_empty(),
            "case {arn:?}: a removed command no longer exists, got len {}",
            registry.len()
        );
        assert!(
            !registry.remove(arn),
            "case {arn:?}: removing it twice reports it was already gone"
        );

        for index in 0..MAX_REGISTERED_COMMANDS {
            let arn = format!("arn:aws:iot:us-east-1:111122223333:command/bulk-{index}");
            assert_eq!(
                registry.register(&arn, CommandFacts::default()),
                Ok(()),
                "case {arn:?}: registering command {index} of the bound must succeed"
            );
        }
        let over = "arn:aws:iot:us-east-1:111122223333:command/over-the-bound";
        assert_eq!(
            registry.register(over, CommandFacts::default()),
            Err(RegisterRefusal::QuotaExceeded),
            "case {over:?}: the command past the bound is refused, got len {}",
            registry.len()
        );
        assert_eq!(
            registry.len(),
            MAX_REGISTERED_COMMANDS,
            "a refused register stores nothing"
        );
    }
}
