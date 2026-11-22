// SPDX-License-Identifier: CC0-1.0

//! The reserved MQTT topics the IoT COMMANDS data plane runs on: how a started command
//! execution reaches its device, and how the device's own report on it is applied and
//! acknowledged.
//!
//! A command execution is not polled over HTTP by the device. The control plane creates
//! the command and `StartCommandExecution` creates one execution of it, and from that
//! point the whole exchange is MQTT on the device's own reserved topics:
//!
//! * `$aws/commands/things/{thingName}/executions/{executionId}/request/{format}` — the
//!   command's payload, delivered to the device. It is published when the execution is
//!   STARTED, and again to a device that SUBSCRIBES to a filter covering it while the
//!   execution is still one the device has not taken up, which is how a device that was
//!   not yet subscribed when the execution was created still receives it.
//! * `$aws/commands/things/{thingName}/executions/{executionId}/response/{format}` — the
//!   device's own report: the status it has reached, and optionally the reason and the
//!   result it reached it with.
//! * `…/response/accepted/{format}` and `…/response/rejected/{format}` — the service's
//!   answer to that report, so a device learns whether its update was applied.
//!
//! `{format}` is the PAYLOAD FORMAT of the message's body, the level AWS publishes these
//! topics with ([`payload_format`]). A request is delivered on the format the command's
//! own payload content type names, so a device that subscribed for one format is not
//! handed bytes of another. A device's REPORT is read as a JSON document and answered on
//! the format level it was published on; a body that is no such document is refused on the
//! execution's own `rejected` topic rather than silently dropped.
//!
//! What the report WRITES is the shared job-execution store
//! ([`crate::iot_job_executions`]) — the very record the control plane's
//! `GetCommandExecution` reads and `DeleteCommandExecution` decides on — so a device
//! driving its execution to a terminal status is what makes those two operations answer
//! differently. Nothing else about the execution is invented here: the status, the reason
//! and the result a reader reports are the ones the device sent.
//!
//! LOCK ORDER: one store at a time. The executions are read or written under their own
//! guard, the command index under its own, and the broker's own store last, by
//! [`super::publish_to_subscribers`] — no guard is held while another is taken.
//!
//! WHAT IS NOT PINNED BY A RECORDING: the topics themselves and the status a report drives
//! an execution to are, but the BODIES of the two answers are not. The member names the
//! `accepted` and `rejected` messages carry (`executionId`, `status`; `executionId`,
//! `code`, `message`), the refusal codes ([`ReportRefusal::code`]) and the sentences the
//! refusals read with are this emulator's own: they state the execution the report
//! addressed and what became of it, so a device learns the outcome, but a device library
//! expecting other member names will not find them.

use ::serde_json::{Map, Value};

use crate::iot_commands::IotCommandRegistry;
use crate::iot_job_executions::{
    now_epoch_seconds, CommandResultValue, CommandStatusReason, JobExecutionStore, STATUS_QUEUED,
};

use super::{now_millis, publish_to_subscribers, DirectMessage, MqttBrokerState};

/// The first two levels of the reserved namespace every commands topic lives under.
pub const COMMANDS_PREFIX: &str = "$aws/commands";

/// The level that names WHICH kind of device the next level identifies.
///
/// `things` is the only one this emulator serves: a command execution is created for the
/// IoT THING a `targetArn` names (`iot-jobs-data`: `StartCommandExecution$targetArn`) and
/// is keyed by that thing's name, so a thing is the device every execution belongs to.
pub const THINGS_DEVICE_TYPE: &str = "things";

/// The levels that name the execution within one device's namespace, and the two halves
/// of the exchange.
pub const EXECUTIONS_LEVEL: &str = "executions";
/// See [`EXECUTIONS_LEVEL`].
pub const REQUEST_LEVEL: &str = "request";
/// See [`EXECUTIONS_LEVEL`].
pub const RESPONSE_LEVEL: &str = "response";
/// See [`EXECUTIONS_LEVEL`].
pub const ACCEPTED_LEVEL: &str = "accepted";
/// See [`EXECUTIONS_LEVEL`].
pub const REJECTED_LEVEL: &str = "rejected";

/// The payload format of a JSON body, as the reserved topics' last level spells it, and
/// the content type a message in it carries.
pub const JSON_FORMAT: &str = "json";
/// See [`JSON_FORMAT`].
pub const JSON_CONTENT_TYPE: &str = "application/json";
/// The payload format of a CBOR body, the other format the reserved topics are published
/// with.
pub const CBOR_FORMAT: &str = "cbor";

/// The QoS the service's own publications on these topics are made at: the highest this
/// broker delivers at ([`super::wire::MAX_GRANTED_QOS`]), so a device that asked for an
/// acknowledged delivery of its command gets one.
const RESERVED_QOS: i32 = super::wire::MAX_GRANTED_QOS;

/// The statuses a DEVICE may report for a command execution.
///
/// `CommandExecutionStatus` declares six values (`smithy_query(query="shape",
/// shape="CommandExecutionStatus")`: `CREATED`, `IN_PROGRESS`, `SUCCEEDED`, `FAILED`,
/// `REJECTED`, `TIMED_OUT`), and two of them are not a device's to send: `CREATED` is the
/// status the service's own start put the execution in, and `TIMED_OUT` is the one the
/// cloud's own timer reaches when a device reports nothing at all. So these four are what
/// a report may carry; the status values themselves are spelled the same in the stored
/// `JobExecutionStatus` vocabulary, except `CREATED`/`QUEUED`, which is not reportable.
pub const DEVICE_REPORTABLE_STATUSES: [&str; 4] =
    ["IN_PROGRESS", "SUCCEEDED", "FAILED", "REJECTED"];

/// The most bytes one device REPORT may carry.
///
/// CHOSEN as a process bound, not a published AWS quota: the reason and the result a
/// report carries are PERSISTED on the execution the control plane then reports, so
/// without a ceiling a device could size this process's store with one message per
/// execution. A larger report is refused on the execution's own `rejected` topic rather
/// than stored in part.
pub const MAX_DEVICE_REPORT_BYTES: usize = 8 * 1024;

/// The maximum length of a reason CODE, and the pattern one satisfies: `StatusReason`'s
/// own `reasonCode` constraints (`smithy_query(query="shape",shape="StatusReason")`:
/// length 0..64, pattern `^[A-Z0-9_-]+$`).
pub const MAX_REASON_CODE_LEN: usize = 64;
/// The maximum length of a reason DESCRIPTION: `StatusReason$reasonDescription`'s own
/// `length 0..1024`.
pub const MAX_REASON_DESCRIPTION_LEN: usize = 1024;

/// How many owed command requests ONE subscribe may be handed.
///
/// CHOSEN as a process bound, not a published AWS quota: a device's subscription makes the
/// broker replay the requests it has not taken up, and one packet may carry many filters,
/// so without a ceiling the work and the memory of one SUBSCRIBE would be the product of
/// its filter count and the executions each device holds. The ceiling is the published
/// per-device command-execution concurrency limit
/// ([`crate::iot_job_executions::MAX_CONCURRENT_COMMAND_EXECUTIONS_PER_THING`]) — the most
/// outstanding executions one device can legitimately be waiting on — so a device is
/// handed everything it is really owed and a packet asking for more is cut off.
pub const MAX_REPLAYED_REQUESTS_PER_SUBSCRIBE: usize =
    crate::iot_job_executions::MAX_CONCURRENT_COMMAND_EXECUTIONS_PER_THING;

/// How many payload BYTES one subscribe may be handed.
///
/// CHOSEN as a process bound as well: a command's payload is caller-chosen, so a message
/// count alone leaves the product of the count and the payload size unbounded. The ceiling
/// is what one session's own queue holds ([`super::MAX_QUEUED_SESSION_BYTES`]), because a
/// replay larger than that is more than the subscriber could be holding anyway.
pub const MAX_REPLAYED_REQUEST_BYTES_PER_SUBSCRIBE: usize = super::MAX_QUEUED_SESSION_BYTES;

/// What one SUBSCRIBE may still be handed: the messages and the payload bytes left of its
/// own allowance.
///
/// It is ONE budget for the whole packet, spent across every filter the packet carries, so
/// the bound is on the PRODUCT (filters x executions x payload bytes) and not on any one
/// factor.
#[derive(Debug, Clone, Copy)]
pub struct ReplayBudget {
    messages: usize,
    bytes: usize,
}

impl Default for ReplayBudget {
    fn default() -> Self {
        Self::of_one_subscribe()
    }
}

impl ReplayBudget {
    /// The allowance of one SUBSCRIBE packet.
    #[must_use]
    pub fn of_one_subscribe() -> Self {
        Self {
            messages: MAX_REPLAYED_REQUESTS_PER_SUBSCRIBE,
            bytes: MAX_REPLAYED_REQUEST_BYTES_PER_SUBSCRIBE,
        }
    }

    /// Whether this budget is spent, so nothing more is built at all.
    #[must_use]
    pub fn is_spent(&self) -> bool {
        self.messages == 0 || self.bytes == 0
    }

    /// Take one message of `payload_bytes` out of the budget, answering whether it fits.
    /// A message that does not fit takes nothing and leaves the budget spent.
    fn take(&mut self, payload_bytes: usize) -> bool {
        if self.messages == 0 || payload_bytes > self.bytes {
            self.messages = 0;
            return false;
        }
        self.messages -= 1;
        self.bytes -= payload_bytes;
        true
    }
}

/// The payload FORMAT level a command's messages are published on, for a payload of
/// `content_type`.
///
/// The reserved topics carry the format of the body as their last level, and the two
/// formats they are published with are JSON and CBOR. A content type whose subtype names
/// CBOR is delivered on the CBOR level and everything else on the JSON one, which is the
/// format a command created with no content type at all is read in.
#[must_use]
pub fn payload_format(content_type: Option<&str>) -> &'static str {
    let subtype = content_type
        .and_then(|content_type| content_type.split(';').next())
        .and_then(|media_type| media_type.rsplit('/').next())
        .map(str::trim)
        .unwrap_or_default()
        .to_ascii_lowercase();
    if subtype == CBOR_FORMAT {
        CBOR_FORMAT
    } else {
        JSON_FORMAT
    }
}

/// The reserved topic prefix of ONE command execution of one thing — every topic of that
/// execution hangs off it.
#[must_use]
pub fn execution_prefix(thing_name: &str, execution_id: &str) -> String {
    format!(
        "{COMMANDS_PREFIX}/{THINGS_DEVICE_TYPE}/{thing_name}/{EXECUTIONS_LEVEL}/{execution_id}"
    )
}

/// The topic one execution's REQUEST — the command's payload — is delivered on.
#[must_use]
pub fn request_topic(thing_name: &str, execution_id: &str, format: &str) -> String {
    format!(
        "{}/{REQUEST_LEVEL}/{format}",
        execution_prefix(thing_name, execution_id)
    )
}

/// The topic a device REPORTS on one execution's status on.
#[must_use]
pub fn response_topic(thing_name: &str, execution_id: &str, format: &str) -> String {
    format!(
        "{}/{RESPONSE_LEVEL}/{format}",
        execution_prefix(thing_name, execution_id)
    )
}

/// The topic an APPLIED report is acknowledged on.
#[must_use]
pub fn accepted_topic(thing_name: &str, execution_id: &str, format: &str) -> String {
    format!(
        "{}/{RESPONSE_LEVEL}/{ACCEPTED_LEVEL}/{format}",
        execution_prefix(thing_name, execution_id)
    )
}

/// The topic a REFUSED report is answered on.
#[must_use]
pub fn rejected_topic(thing_name: &str, execution_id: &str, format: &str) -> String {
    format!(
        "{}/{RESPONSE_LEVEL}/{REJECTED_LEVEL}/{format}",
        execution_prefix(thing_name, execution_id)
    )
}

/// Which execution of which device a topic a client published on addresses, or `None`
/// for a topic that is not a device's report at all.
///
/// A report's topic is `$aws/commands/things/{thingName}/executions/{executionId}/
/// response/{format}`: eight levels, the last naming the format the body is in. The
/// service's own `accepted` / `rejected` topics hang one level deeper and are therefore
/// NOT reports — a client that publishes on one addresses no report, and this answers
/// `None` so nothing is applied for it.
#[must_use]
pub fn addressed_report(topic: &str) -> Option<(String, String, String)> {
    let levels: Vec<&str> = topic.split('/').collect();
    let [aws, commands, device_type, thing_name, executions, execution_id, response, format] =
        levels.as_slice()
    else {
        return None;
    };
    let expected = [
        ("$aws", *aws),
        ("commands", *commands),
        (THINGS_DEVICE_TYPE, *device_type),
        (EXECUTIONS_LEVEL, *executions),
        (RESPONSE_LEVEL, *response),
    ];
    if expected.iter().any(|(level, read)| level != read) {
        return None;
    }
    if thing_name.is_empty() || execution_id.is_empty() || format.is_empty() {
        return None;
    }
    Some((
        (*thing_name).to_string(),
        (*execution_id).to_string(),
        (*format).to_string(),
    ))
}

/// Why a device's report was NOT applied, as the code the `rejected` answer carries.
///
/// The codes are the vocabulary the reserved topics of this family answer a refused
/// device update with: the addressed execution does not exist, the report is not one the
/// service can read, or the status it asks for is not one the execution can move to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportRefusal {
    /// No command execution of that device under that id.
    ResourceNotFound,
    /// The report itself cannot be read, or carries a value no member admits.
    InvalidRequest,
    /// The status is not one the execution's current status may move to.
    InvalidStateTransition,
}

impl ReportRefusal {
    /// The code a `rejected` answer carries for this refusal.
    #[must_use]
    pub fn code(self) -> &'static str {
        match self {
            Self::ResourceNotFound => "ResourceNotFound",
            Self::InvalidRequest => "InvalidRequest",
            Self::InvalidStateTransition => "InvalidStateTransition",
        }
    }
}

/// What one device report asks for, read out of its body.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct DeviceReport {
    /// The `CommandExecutionStatus` the device reports.
    status: String,
    /// The reason it gave for that status, when it gave one.
    status_reason: Option<CommandStatusReason>,
    /// The result it shared, empty when it shared none.
    result: std::collections::BTreeMap<String, CommandResultValue>,
}

/// Read one device report out of the JSON body it was published with.
///
/// `Err` names the refusal the device is answered with: a body that is no JSON object, a
/// `status` that is absent or is not one a device may report, and a reason or result
/// outside what the members admit are each the report's own fault and change nothing.
fn read_report(payload: &[u8]) -> Result<DeviceReport, (ReportRefusal, String)> {
    let invalid = |message: String| (ReportRefusal::InvalidRequest, message);
    if payload.len() > MAX_DEVICE_REPORT_BYTES {
        return Err(invalid(format!(
            "the report is {} bytes, over the {MAX_DEVICE_REPORT_BYTES} byte maximum",
            payload.len()
        )));
    }
    let body: Value = ::serde_json::from_slice(payload)
        .map_err(|error| invalid(format!("the report is no JSON document: {error}")))?;
    let Some(body) = body.as_object() else {
        return Err(invalid("the report is no JSON object".to_string()));
    };
    let Some(status) = body.get("status").and_then(Value::as_str) else {
        return Err(invalid("the report carries no status".to_string()));
    };
    if !DEVICE_REPORTABLE_STATUSES.contains(&status) {
        return Err(invalid(format!(
            "status must be one of {}, got {status}",
            DEVICE_REPORTABLE_STATUSES.join(", ")
        )));
    }
    Ok(DeviceReport {
        status: status.to_string(),
        status_reason: read_status_reason(body.get("statusReason")).map_err(invalid)?,
        result: read_result(body.get("result")).map_err(invalid)?,
    })
}

/// Whether one character is one a `reasonCode` may carry: `StatusReason$reasonCode`'s
/// own pattern `^[A-Z0-9_-]+$`.
fn is_reason_code_character(character: char) -> bool {
    character.is_ascii_uppercase()
        || character.is_ascii_digit()
        || character == '_'
        || character == '-'
}

/// Read a report's `statusReason`, against what `StatusReason` admits: a reason has a
/// `reasonCode` (required, within [`MAX_REASON_CODE_LEN`] and matching the member's
/// pattern) and optionally a `reasonDescription` within [`MAX_REASON_DESCRIPTION_LEN`].
fn read_status_reason(value: Option<&Value>) -> Result<Option<CommandStatusReason>, String> {
    let Some(value) = value.filter(|value| !value.is_null()) else {
        return Ok(None);
    };
    let Some(object) = value.as_object() else {
        return Err("statusReason is no JSON object".to_string());
    };
    let Some(reason_code) = object.get("reasonCode").and_then(Value::as_str) else {
        return Err("statusReason carries no reasonCode".to_string());
    };
    let code_len = reason_code.chars().count();
    if code_len > MAX_REASON_CODE_LEN {
        return Err(format!(
            "a reasonCode of {code_len} characters is over the {MAX_REASON_CODE_LEN} character \
             maximum"
        ));
    }
    if reason_code.is_empty() || !reason_code.chars().all(is_reason_code_character) {
        return Err(format!(
            "the reasonCode {reason_code} is not 1..{MAX_REASON_CODE_LEN} characters matching \
             ^[A-Z0-9_-]+$"
        ));
    }
    let reason_description = match object.get("reasonDescription") {
        None | Some(Value::Null) => None,
        Some(Value::String(description)) => {
            let description_len = description.chars().count();
            if description_len > MAX_REASON_DESCRIPTION_LEN {
                return Err(format!(
                    "a reasonDescription of {description_len} characters is over the \
                     {MAX_REASON_DESCRIPTION_LEN} character maximum"
                ));
            }
            Some(description.clone())
        }
        Some(_) => return Err("reasonDescription is no string".to_string()),
    };
    Ok(Some(CommandStatusReason {
        reason_code: reason_code.to_string(),
        reason_description,
    }))
}

/// Read a report's `result`: a map of names to `CommandExecutionResult` values, each
/// carrying the attributes the device sent (`S`, `B`, `BIN`, the members the shape
/// declares) and nothing it did not.
fn read_result(
    value: Option<&Value>,
) -> Result<std::collections::BTreeMap<String, CommandResultValue>, String> {
    let Some(value) = value.filter(|value| !value.is_null()) else {
        return Ok(std::collections::BTreeMap::new());
    };
    let Some(object) = value.as_object() else {
        return Err("result is no JSON object".to_string());
    };
    let mut result = std::collections::BTreeMap::new();
    for (name, entry) in object {
        let Some(entry) = entry.as_object() else {
            return Err(format!("the result value of {name} is no JSON object"));
        };
        let string = match entry.get("S") {
            None | Some(Value::Null) => None,
            Some(Value::String(text)) if !text.is_empty() => Some(text.clone()),
            Some(_) => {
                return Err(format!(
                    "the S attribute of the result value of {name} is no non-empty string"
                ))
            }
        };
        let boolean = match entry.get("B") {
            None | Some(Value::Null) => None,
            Some(Value::Bool(flag)) => Some(*flag),
            Some(_) => {
                return Err(format!(
                    "the B attribute of the result value of {name} is no boolean"
                ))
            }
        };
        // A binary attribute travels as base64 in a JSON body, the one encoding a JSON
        // document can carry bytes in.
        let binary = match entry.get("BIN") {
            None | Some(Value::Null) => None,
            Some(Value::String(encoded)) => Some(
                ::aws_smithy_types::base64::decode(encoded).map_err(|error| {
                    format!(
                        "the BIN attribute of the result value of {name} is no base64 blob: \
                         {error}"
                    )
                })?,
            ),
            Some(_) => {
                return Err(format!(
                    "the BIN attribute of the result value of {name} is no base64 blob"
                ))
            }
        };
        result.insert(
            name.clone(),
            CommandResultValue {
                s: string,
                b: boolean,
                bin: binary,
            },
        );
    }
    Ok(result)
}

/// The stores the reserved commands topics are served from: the executions a report
/// writes, and the index the payload a request carries is read from.
///
/// Both are the SHARED stores of the engine this broker belongs to, so an execution a
/// device drives here is the execution the control plane reports and a payload a device is
/// handed is the one its command was created with.
#[derive(Clone)]
pub struct CommandsPlane {
    executions: ::engine::ServiceState<JobExecutionStore>,
    commands: ::engine::ServiceState<IotCommandRegistry>,
}

/// Written by hand rather than derived: a store handle carries no `Debug`. What is
/// diagnosable about this plane is that it is attached at all.
impl std::fmt::Debug for CommandsPlane {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("CommandsPlane")
    }
}

impl CommandsPlane {
    /// The plane of the shared engine `engine`, or `None` for an engine this emulator did
    /// not build, which vends no stores — in which case the reserved topics carry nothing
    /// rather than a session being refused.
    #[must_use]
    pub fn of<Eng: Send + Sync + 'static>(engine: &Eng) -> Option<Self> {
        let shared =
            (engine as &dyn ::std::any::Any).downcast_ref::<::engine::InMemoryEngine>()?;
        Some(Self {
            executions: shared.state::<JobExecutionStore>(),
            commands: shared.state::<IotCommandRegistry>(),
        })
    }

    /// The REQUEST message of one command execution of `thing_name`: the payload its
    /// command hands the device, on the topic of the format that payload's content type
    /// names.
    ///
    /// `None` when the command the execution runs has no payload of its own in the shared
    /// index — a command whose payload is generated from a `payloadTemplate` rather than
    /// supplied as a literal one — because there are then no bytes to hand the device, and
    /// a message the service never produced is not published.
    #[must_use]
    fn request_message(
        &self,
        thing_name: &str,
        execution_id: &str,
        command_arn: &str,
    ) -> Option<DirectMessage> {
        let commands = self.commands.lock();
        let payload = commands.payload_ref(command_arn)?;
        let format = payload_format(payload.content_type.as_deref());
        Some(DirectMessage {
            topic: request_topic(thing_name, execution_id, format),
            payload: payload.content.clone(),
            qos: RESERVED_QOS,
            content_type: payload.content_type.clone(),
            delivered_at_millis: now_millis(),
            ..DirectMessage::default()
        })
    }

    /// Deliver the REQUEST of one started command execution to its device.
    ///
    /// Called by the start itself, so a device that is already subscribed is handed the
    /// command as soon as the execution exists; a device that subscribes later is handed
    /// it then ([`Self::pending_requests`]). Answers how many sessions it reached, which
    /// is zero for a device that is neither connected nor holding a persistent session.
    pub fn deliver_request(
        &self,
        broker: &::engine::ServiceState<MqttBrokerState>,
        thing_name: &str,
        execution_id: &str,
        command_arn: &str,
    ) -> usize {
        match self.request_message(thing_name, execution_id, command_arn) {
            Some(message) => publish_to_subscribers(broker, &message),
            None => 0,
        }
    }

    /// The REQUESTS a device subscribing to `filter` is owed, within what `budget` still
    /// allows: one per command execution of the device the filter names that the device has
    /// not taken up yet and whose request topic that filter covers.
    ///
    /// This is what makes the order of a device's own calls not matter: an execution
    /// created before the device subscribed is delivered when it does, exactly as one
    /// created after it is delivered by the start. It is a READ of the executions — the
    /// status, the reason and the instants of every record are left exactly as they stand,
    /// because a command a device is handed is one it has not reported on yet.
    ///
    /// BOUNDED by `budget`, which is the ONE allowance of the whole SUBSCRIBE packet and is
    /// spent here: the topic of each candidate is decided before any payload byte is
    /// copied, so a request the filter does not cover costs nothing, and the messages this
    /// answers with are within the packet's remaining message and byte allowance however
    /// many filters it carries and however many executions each device holds.
    ///
    /// A filter that names no single device in the reserved commands space (its device
    /// level is a wildcard, or it is no commands filter at all) is owed nothing: a
    /// device's reserved topics are its own.
    #[must_use]
    pub fn pending_requests(&self, filter: &str, budget: &mut ReplayBudget) -> Vec<DirectMessage> {
        let Some(thing_name) = addressed_device(filter) else {
            return Vec::new();
        };
        if budget.is_spent() {
            return Vec::new();
        }
        let now = now_epoch_seconds();
        let pending: Vec<(String, String)> = {
            let mut executions = self.executions.lock();
            // A timer the start armed moves an execution to `TIMED_OUT` by itself, so the
            // transitions the clock has already made due are recorded before anything is
            // read: an execution whose deadline has passed is not one a device is handed.
            executions.time_out_due_executions(&thing_name, now);
            executions
                .executions_for(&thing_name)
                .iter()
                .filter(|record| record.is_command_execution() && record.status == STATUS_QUEUED)
                .filter_map(|record| {
                    record
                        .command_arn
                        .clone()
                        .map(|command_arn| (record.job_id.clone(), command_arn))
                })
                .collect()
        };
        let mut owed = Vec::new();
        let commands = self.commands.lock();
        for (execution_id, command_arn) in pending {
            if budget.is_spent() {
                break;
            }
            // The payload is BORROWED to decide the topic and whether the filter covers
            // it; its bytes are copied only for a message that is really going out.
            let Some(payload) = commands.payload_ref(&command_arn) else {
                continue;
            };
            let format = payload_format(payload.content_type.as_deref());
            let topic = request_topic(&thing_name, &execution_id, format);
            if !super::topic_filter_covers(filter, &topic) {
                continue;
            }
            if !budget.take(payload.content.len()) {
                break;
            }
            owed.push(DirectMessage {
                topic,
                payload: payload.content.clone(),
                qos: RESERVED_QOS,
                content_type: payload.content_type.clone(),
                delivered_at_millis: now_millis(),
                ..DirectMessage::default()
            });
        }
        owed
    }

    /// Apply the report a client published on `topic` to the execution it addresses, and
    /// answer the message the device is acknowledged with.
    ///
    /// `None` for a topic that is no report at all, which is every other publication this
    /// broker carries. Otherwise the answer is the `accepted` message of an applied report
    /// or the `rejected` message naming why it was not applied — a device learns the
    /// outcome either way.
    #[must_use]
    pub fn apply_report(&self, topic: &str, payload: &[u8]) -> Option<DirectMessage> {
        let (thing_name, execution_id, format) = addressed_report(topic)?;
        let answer = self.record_report(&thing_name, &execution_id, payload);
        Some(match answer {
            Ok(status) => accepted_message(&thing_name, &execution_id, &format, &status),
            Err((refusal, message)) => {
                rejected_message(&thing_name, &execution_id, &format, refusal, &message)
            }
        })
    }

    /// Write one device report onto the execution it addresses, answering the status the
    /// execution now holds or why the report changed nothing.
    fn record_report(
        &self,
        thing_name: &str,
        execution_id: &str,
        payload: &[u8],
    ) -> Result<String, (ReportRefusal, String)> {
        let report = read_report(payload)?;
        let now = now_epoch_seconds();
        let not_found = || {
            (
                ReportRefusal::ResourceNotFound,
                not_found_message(execution_id),
            )
        };
        let mut executions = self.executions.lock();
        // The transitions the clock already owes are materialised first, so a report that
        // arrives after the execution's own timeout is judged against `TIMED_OUT` rather
        // than against the status the record still spelled.
        executions.time_out_due_executions(thing_name, now);
        // A command execution is reached by the `executionId` the start minted and keyed
        // the record by; an execution a JOB created is not one of this device's command
        // executions, however its own id happens to be spelled.
        let index = executions
            .execution_index(thing_name, execution_id, None)
            .filter(|index| {
                executions
                    .record(thing_name, *index)
                    .is_some_and(|record| record.is_command_execution())
            })
            .ok_or_else(not_found)?;
        let record = executions
            .record_mut(thing_name, index)
            .ok_or_else(not_found)?;
        if let Some(reason) = record.transition_refusal(&report.status) {
            return Err((ReportRefusal::InvalidStateTransition, reason));
        }
        record.status = report.status.clone();
        if record.status == crate::iot_job_executions::STATUS_IN_PROGRESS {
            // The device reporting that it has taken the command up is a start: it
            // records `startedAt`, which the command surface then reports.
            record.start(now);
        }
        record.command_status_reason = report.status_reason;
        record.command_result = report.result;
        record.last_updated_at = now;
        // "Job execution versions are incremented each time they are updated by a device"
        // — `JobExecution$versionNumber`, and a report on the reserved topic is a device
        // update of the very same record.
        record.version_number += 1;
        Ok(record.status.clone())
    }
}

/// How a report naming an execution the device does not hold is worded: the one sentence
/// the commands surface words a missing command execution with
/// ([`crate::iot_commands::command_execution_not_found_message`]).
fn not_found_message(execution_id: &str) -> String {
    crate::iot_commands::command_execution_not_found_message(execution_id)
}

/// WHICH device a filter in the reserved commands space names, or `None` when it names
/// no single one.
///
/// A device's reserved topics begin `$aws/commands/things/{thingName}/`, so the filter's
/// fourth level is the device — and a filter that carries a wildcard there, or that is no
/// commands filter at all, names no device.
#[must_use]
pub fn addressed_device(filter: &str) -> Option<String> {
    let mut levels = filter.split('/');
    if levels.next() != Some("$aws") || levels.next() != Some("commands") {
        return None;
    }
    if levels.next() != Some(THINGS_DEVICE_TYPE) {
        return None;
    }
    let thing_name = levels.next()?;
    if thing_name.is_empty() || thing_name == "+" || thing_name == "#" {
        return None;
    }
    Some(thing_name.to_string())
}

/// The `accepted` answer of an applied report: which execution it was applied to, and
/// the status that execution now holds.
fn accepted_message(
    thing_name: &str,
    execution_id: &str,
    format: &str,
    status: &str,
) -> DirectMessage {
    let mut body = Map::new();
    body.insert("executionId".to_string(), Value::from(execution_id));
    body.insert("status".to_string(), Value::from(status));
    json_message(
        accepted_topic(thing_name, execution_id, format),
        &Value::Object(body),
    )
}

/// The `rejected` answer of a report that changed nothing: which execution it addressed,
/// the code of the refusal and its sentence.
fn rejected_message(
    thing_name: &str,
    execution_id: &str,
    format: &str,
    refusal: ReportRefusal,
    message: &str,
) -> DirectMessage {
    let mut body = Map::new();
    body.insert("executionId".to_string(), Value::from(execution_id));
    body.insert("code".to_string(), Value::from(refusal.code()));
    body.insert("message".to_string(), Value::from(message));
    json_message(
        rejected_topic(thing_name, execution_id, format),
        &Value::Object(body),
    )
}

/// One JSON message of the service's own, as it goes onto a reserved topic.
fn json_message(topic: String, body: &Value) -> DirectMessage {
    DirectMessage {
        topic,
        payload: ::serde_json::to_vec(body).unwrap_or_default(),
        qos: RESERVED_QOS,
        content_type: Some(JSON_CONTENT_TYPE.to_string()),
        delivered_at_millis: now_millis(),
        ..DirectMessage::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One row of the report-topic table: the case, the topic a client published on, and
    /// the device, execution and format it addresses (`None` for a topic that is no
    /// report). Named rather than written inline, so the table's type stays readable.
    type ReportRow = (
        &'static str,
        &'static str,
        Option<(&'static str, &'static str, &'static str)>,
    );

    /// One row of the readable-report table: the case, the body published, and the status
    /// it reports or the refusal it is.
    type ReadRow<'body> = (
        &'static str,
        &'body str,
        Result<&'static str, ReportRefusal>,
    );

    /// RULE: a command's messages carry the payload FORMAT of their body as the last
    /// level of the topic, and WHICH format that is comes from the payload's own content
    /// type — the CBOR level for a CBOR body and the JSON level for everything else,
    /// including a payload created with no content type at all.
    ///
    /// The rows span what the rule generalises over: the two formats, a content type with
    /// parameters after it, one spelled in another case, a type whose subtype is neither,
    /// and no content type at all.
    #[test]
    fn the_topic_format_level_is_the_one_the_content_type_names() {
        // (case, the payload's content type, the format level its messages carry)
        let rows: &[(&str, Option<&str>, &str)] = &[
            ("a JSON payload", Some("application/json"), JSON_FORMAT),
            ("a CBOR payload", Some("application/cbor"), CBOR_FORMAT),
            (
                "a content type carrying a parameter",
                Some("application/cbor; charset=utf-8"),
                CBOR_FORMAT,
            ),
            (
                "a content type in another case",
                Some("Application/CBOR"),
                CBOR_FORMAT,
            ),
            (
                "a content type that is neither",
                Some("application/octet-stream"),
                JSON_FORMAT,
            ),
            ("a payload with no content type", None, JSON_FORMAT),
        ];
        for (case, content_type, expected) in rows {
            let actual = payload_format(*content_type);
            assert_eq!(
                actual, *expected,
                "case {case:?}: expected the {expected} level, got {actual}"
            );
        }
    }

    /// RULE: one execution's reserved topics all hang off
    /// `$aws/commands/things/{thingName}/executions/{executionId}`, so two executions —
    /// and two devices — never share a topic.
    #[test]
    fn every_topic_of_one_execution_hangs_off_its_own_prefix() {
        let prefix = execution_prefix("laser-1", "abc123");
        assert_eq!(
            prefix, "$aws/commands/things/laser-1/executions/abc123",
            "expected the execution's own reserved prefix, got {prefix}"
        );
        // (case, the topic built, what it must be)
        let rows: &[(&str, String, String)] = &[
            (
                "the request",
                request_topic("laser-1", "abc123", JSON_FORMAT),
                format!("{prefix}/request/json"),
            ),
            (
                "the report",
                response_topic("laser-1", "abc123", JSON_FORMAT),
                format!("{prefix}/response/json"),
            ),
            (
                "the accepted answer",
                accepted_topic("laser-1", "abc123", JSON_FORMAT),
                format!("{prefix}/response/accepted/json"),
            ),
            (
                "the rejected answer",
                rejected_topic("laser-1", "abc123", CBOR_FORMAT),
                format!("{prefix}/response/rejected/cbor"),
            ),
            (
                "another device's request",
                request_topic("laser-2", "abc123", JSON_FORMAT),
                "$aws/commands/things/laser-2/executions/abc123/request/json".to_string(),
            ),
        ];
        for (case, built, expected) in rows {
            assert_eq!(
                built, expected,
                "case {case:?}: expected {expected}, got {built}"
            );
        }
    }

    /// RULE: a publication addresses a device's REPORT exactly when its topic is that
    /// device's `…/executions/{executionId}/response/{format}` — and the service's own
    /// `accepted` / `rejected` topics, which hang one level deeper, are not reports.
    ///
    /// The rows span what the rule generalises over: a report on each format level, the
    /// two answers the service itself publishes, the request topic, a topic with a level
    /// missing or one too many, an empty device or execution level, and a topic outside
    /// the reserved commands space altogether.
    #[test]
    fn only_a_response_topic_addresses_a_device_report() {
        // (case, the topic published on, the device/execution/format it addresses)
        let rows: &[ReportRow] = &[
            (
                "a JSON report",
                "$aws/commands/things/laser-1/executions/abc123/response/json",
                Some(("laser-1", "abc123", "json")),
            ),
            (
                "a CBOR report",
                "$aws/commands/things/laser-1/executions/abc123/response/cbor",
                Some(("laser-1", "abc123", "cbor")),
            ),
            (
                "the service's own accepted answer",
                "$aws/commands/things/laser-1/executions/abc123/response/accepted/json",
                None,
            ),
            (
                "the service's own rejected answer",
                "$aws/commands/things/laser-1/executions/abc123/response/rejected/json",
                None,
            ),
            (
                "the request topic",
                "$aws/commands/things/laser-1/executions/abc123/request/json",
                None,
            ),
            (
                "a report with no format level",
                "$aws/commands/things/laser-1/executions/abc123/response",
                None,
            ),
            (
                "a report naming no device",
                "$aws/commands/things//executions/abc123/response/json",
                None,
            ),
            (
                "a report naming no execution",
                "$aws/commands/things/laser-1/executions//response/json",
                None,
            ),
            (
                "a report of a device type this plane does not serve",
                "$aws/commands/clients/client-1/executions/abc123/response/json",
                None,
            ),
            (
                "a topic outside the reserved commands space",
                "devices/laser-1/response/json",
                None,
            ),
        ];
        for (case, topic, expected) in rows {
            let addressed = addressed_report(topic);
            let expected = expected.map(|(thing_name, execution_id, format)| {
                (
                    thing_name.to_string(),
                    execution_id.to_string(),
                    format.to_string(),
                )
            });
            assert_eq!(
                addressed, expected,
                "case {case:?}: expected {expected:?}, got {addressed:?}"
            );
        }
    }

    /// RULE: a filter in the reserved commands space names the ONE device whose level 4
    /// it spells, and names none when that level is a wildcard or the filter is no
    /// commands filter at all — a device's reserved topics are its own.
    #[test]
    fn a_commands_filter_names_the_device_its_fourth_level_spells() {
        // (case, the filter subscribed to, the device it names)
        let rows: &[(&str, &str, Option<&str>)] = &[
            (
                "every execution of one device",
                "$aws/commands/things/laser-1/executions/+/request/json",
                Some("laser-1"),
            ),
            (
                "one execution of one device",
                "$aws/commands/things/laser-1/executions/abc123/request/json",
                Some("laser-1"),
            ),
            (
                "a device's whole reserved subtree",
                "$aws/commands/things/laser-1/#",
                Some("laser-1"),
            ),
            (
                "every device",
                "$aws/commands/things/+/executions/+/request/json",
                None,
            ),
            ("the whole commands space", "$aws/commands/things/#", None),
            (
                "a device type this plane does not serve",
                "$aws/commands/clients/client-1/executions/+/request/json",
                None,
            ),
            ("a filter outside the commands space", "devices/+/request", None),
        ];
        for (case, filter, expected) in rows {
            let named = addressed_device(filter);
            assert_eq!(
                named.as_deref(),
                *expected,
                "case {case:?}: expected {expected:?}, got {named:?}"
            );
        }
    }

    /// RULE: a device's report carries a status it may report, and optionally a reason and
    /// a result within what their members admit; anything else is refused and applied to
    /// nothing.
    ///
    /// The rows span what the rule generalises over: each reportable status, the statuses
    /// only the service and the clock reach, a value the enum does not declare, a body
    /// that is no object, a missing status, a reason without its code and with a code
    /// outside its pattern, a description past its length, a result value that is no
    /// attribute object and one carrying each attribute type, and a report past the
    /// byte ceiling.
    #[test]
    fn a_report_is_read_only_when_it_is_one_the_service_can_apply() {
        let oversized = format!(
            r#"{{"status":"SUCCEEDED","statusReason":{{"reasonCode":"OK","reasonDescription":"{}"}}}}"#,
            "x".repeat(MAX_DEVICE_REPORT_BYTES)
        );
        let long_description = format!(
            r#"{{"status":"SUCCEEDED","statusReason":{{"reasonCode":"OK","reasonDescription":"{}"}}}}"#,
            "x".repeat(MAX_REASON_DESCRIPTION_LEN + 1)
        );
        let at_the_description_limit = format!(
            r#"{{"status":"SUCCEEDED","statusReason":{{"reasonCode":"OK","reasonDescription":"{}"}}}}"#,
            "x".repeat(MAX_REASON_DESCRIPTION_LEN)
        );
        let long_code = format!(
            r#"{{"status":"SUCCEEDED","statusReason":{{"reasonCode":"{}"}}}}"#,
            "A".repeat(MAX_REASON_CODE_LEN + 1)
        );

        // (case, the body published, the status it reports or the refusal it is)
        let rows: &[ReadRow<'_>] = &[
            ("a device taking the work up", r#"{"status":"IN_PROGRESS"}"#, Ok("IN_PROGRESS")),
            ("a device succeeding", r#"{"status":"SUCCEEDED"}"#, Ok("SUCCEEDED")),
            ("a device failing", r#"{"status":"FAILED"}"#, Ok("FAILED")),
            ("a device rejecting the work", r#"{"status":"REJECTED"}"#, Ok("REJECTED")),
            (
                "the status the service's own start set",
                r#"{"status":"CREATED"}"#,
                Err(ReportRefusal::InvalidRequest),
            ),
            (
                "the status the clock's own timer reaches",
                r#"{"status":"TIMED_OUT"}"#,
                Err(ReportRefusal::InvalidRequest),
            ),
            (
                "a status the enum does not declare",
                r#"{"status":"DONE"}"#,
                Err(ReportRefusal::InvalidRequest),
            ),
            ("a body that is no object", r#"["SUCCEEDED"]"#, Err(ReportRefusal::InvalidRequest)),
            ("a body that is no JSON", "SUCCEEDED", Err(ReportRefusal::InvalidRequest)),
            ("a report with no status", "{}", Err(ReportRefusal::InvalidRequest)),
            (
                "a reason with every member",
                r#"{"status":"SUCCEEDED","statusReason":{"reasonCode":"DEVICE_OK-1","reasonDescription":"done"}}"#,
                Ok("SUCCEEDED"),
            ),
            (
                "a reason with no code",
                r#"{"status":"SUCCEEDED","statusReason":{"reasonDescription":"done"}}"#,
                Err(ReportRefusal::InvalidRequest),
            ),
            (
                "a code outside the member's pattern",
                r#"{"status":"SUCCEEDED","statusReason":{"reasonCode":"not upper"}}"#,
                Err(ReportRefusal::InvalidRequest),
            ),
            ("a code at the member's length", &long_code[..], Err(ReportRefusal::InvalidRequest)),
            (
                "a description at the member's length",
                &at_the_description_limit[..],
                Ok("SUCCEEDED"),
            ),
            (
                "a description past the member's length",
                &long_description[..],
                Err(ReportRefusal::InvalidRequest),
            ),
            (
                "a result carrying each attribute type",
                r#"{"status":"SUCCEEDED","result":{"a":{"S":"ok"},"b":{"B":true},"c":{"BIN":"AAEC"}}}"#,
                Ok("SUCCEEDED"),
            ),
            (
                "a result value that is no attribute object",
                r#"{"status":"SUCCEEDED","result":{"a":"ok"}}"#,
                Err(ReportRefusal::InvalidRequest),
            ),
            (
                "a BIN attribute that is no base64 blob",
                r#"{"status":"SUCCEEDED","result":{"a":{"BIN":"not base64!"}}}"#,
                Err(ReportRefusal::InvalidRequest),
            ),
            (
                "a report past the byte ceiling",
                &oversized[..],
                Err(ReportRefusal::InvalidRequest),
            ),
        ];
        for (case, body, expected) in rows {
            let read = read_report(body.as_bytes());
            match (expected, &read) {
                (Ok(status), Ok(report)) => assert_eq!(
                    report.status, *status,
                    "case {case:?}: expected the status {status}, got {report:?}"
                ),
                (Err(refusal), Err((actual, message))) => assert_eq!(
                    actual, refusal,
                    "case {case:?}: expected {refusal:?}, got {actual:?} ({message})"
                ),
                (expected, actual) => panic!(
                    "case {case:?}: expected {expected:?}, got {actual:?}"
                ),
            }
        }

        // The attributes a result carries are the ones the device sent, read as the
        // values the store holds.
        let report = read_report(
            br#"{"status":"SUCCEEDED","result":{"a":{"S":"ok"},"b":{"B":false},"c":{"BIN":"AAEC"}}}"#,
        )
        .expect("the report is one the service can apply");
        assert_eq!(
            report.result.get("a").and_then(|value| value.s.clone()),
            Some("ok".to_string()),
            "a string attribute is the device's own string, got {report:?}"
        );
        assert_eq!(
            report.result.get("b").and_then(|value| value.b),
            Some(false),
            "a boolean attribute is the device's own boolean, got {report:?}"
        );
        assert_eq!(
            report.result.get("c").and_then(|value| value.bin.clone()),
            Some(vec![0x00, 0x01, 0x02]),
            "a binary attribute is the bytes the device base64-encoded, got {report:?}"
        );
        assert_eq!(
            report.status_reason, None,
            "a report that sent no reason carries none, got {report:?}"
        );
    }
}
