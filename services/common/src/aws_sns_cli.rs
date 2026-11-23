// SPDX-License-Identifier: CC0-1.0

//! The AWS CLI's `aws sns publish`, as a program a caller-supplied command line can ask for:
//! what one command line asks of the CLI, and the call that carries the notification it names.
//!
//! # The rule this module implements
//!
//! Several emulated services run a command line a client supplied — a container's `command`, a
//! cluster step's arguments, a build's commands — and such a line reaches a service of the
//! account through the AWS CLI, which "provides direct access to the public APIs of Amazon Web
//! Services" one command at a time. `aws sns publish` "sends a message to an Amazon SNS topic,
//! a text message (SMS message) directly to a phone number, or a message to a mobile platform
//! endpoint" (the `aws sns publish` command reference), so a command line that publishes a
//! message has that message published: the subscribers of the topic it names receive it, and
//! the status the command exits with is the status of that call — "0 -- Command was successful"
//! when the service answered it and "254 -- … the service returned an error" when the service
//! refused it (AWS CLI return codes). WHO made the call is whichever principal the session
//! running the line signs with, so the service judges it against the policies in force on that
//! principal and the command reports the refusal it answered, verbatim as the CLI words a
//! service's own refusal.
//!
//! That is ONE rule, so it lives here once rather than per service, beside the object-store
//! command groups of [`crate::aws_s3_cli`]: two services running the same command line must not
//! publish a different message or answer a different status. What each service keeps is its own
//! — the SESSION the line runs in and WHICH credential the call is signed with.
//!
//! This module holds the two halves nothing about a caller makes different:
//!
//! * what one command line ASKS for ([`requested_publish`]) — the command group, the command and
//!   the members of the call, each read as the CLI reads them. Every other command group, every
//!   other command of this group, and every FORM of this command this program does not perform
//!   (a parameter it does not take, a line missing the members the call requires) is answered
//!   `None`, which the caller's session reports as a command it could not run rather than as a
//!   call;
//! * the CALL that carries it ([`published`]), made with the in-process Amazon SNS client the
//!   caller hands over ([`crate::sns_client`]) — so the message a command published is the
//!   message that topic's own subscribers receive — and the document the CLI prints for it.
//!
//! Nothing here opens a socket, resolves a name or starts a process: the client is the caller's
//! in-process hop, and the topic a command line names is a topic of this emulator.

/// The CLI's command GROUP this program answers for: `aws sns`, the commands of Amazon Simple
/// Notification Service.
pub const SNS_COMMAND_GROUP: &str = "sns";

/// The command of that group this program performs, as the CLI spells it: `publish`. SOURCE:
/// the `aws sns` command reference. Every other command of the group — and every other group —
/// is one this program performs nothing of.
pub const PERFORMED_SNS_COMMANDS: [&str; 1] = [PUBLISH_COMMAND];

/// `publish`, the one command of [`PERFORMED_SNS_COMMANDS`].
const PUBLISH_COMMAND: &str = "publish";

/// The API operation that command calls, as the CLI names it in its account of a refusal
/// ([`service_answered`]).
pub const PUBLISH_OPERATION: &str = "Publish";

/// The status one of these commands answers when the call was made: "0 -- Command was
/// successful" (AWS CLI return codes), held once in this workspace.
pub use crate::aws_s3_cli::TRANSFER_SUCCEEDED as COMMAND_SUCCEEDED;

/// The status it answers when the service REFUSED the call: "254 -- The command was successfully
/// parsed and a request was made to the specified service but the service returned an error"
/// (AWS CLI return codes), held once in this workspace. It is the status a shell reads, so a
/// program written `if aws sns publish …; then … else … fi` takes its other branch.
pub use crate::aws_s3_cli::API_SERVICE_ERROR as COMMAND_SERVICE_ERROR;

/// The status it answers when the CLI itself could not carry the command out — no credential to
/// sign with among them: "255 -- General error" (AWS CLI return codes), held once in this
/// workspace.
pub use crate::aws_s3_cli::API_COMMAND_ERROR as COMMAND_ERROR;

/// What a call a SERVICE refused reports, as the CLI words it: "An error occurred (<code>) when
/// calling the <operation> operation: <message>". The sentence is the CLI's own rather than any
/// one service's, so this group words a refusal with the workspace's one spelling of it.
pub use crate::aws_s3_cli::store_answered as service_answered;

/// What the CLI prints the answer to one call as, which this group's `--output` option selects:
/// "The AWS CLI supports the following output formats" — `json`, "the output is formatted as a
/// JSON string", and `text`, "the output is formatted as multiple lines of tab-separated string
/// values" (AWS CLI, controlling command output). Every other format is one this program does
/// not print, so a line asking for it is not an invocation it performs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OutputFormat {
    /// `json`, the CLI's default output format.
    #[default]
    Json,
    /// `text`.
    Text,
}

impl OutputFormat {
    /// The format `written` names, or `None` for one this program does not print.
    fn of(written: &str) -> Option<Self> {
        match written {
            "json" => Some(Self::Json),
            "text" => Some(Self::Text),
            _ => None,
        }
    }
}

/// WHERE one publish sends its message, as the command line addressed it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PublishTarget {
    /// `--topic-arn`, "the topic you want to publish to" (`Publish$TopicArn`).
    Topic(String),
    /// `--target-arn`, "if you don't specify a value for the `TargetArn` parameter, you must
    /// specify a value for the `PhoneNumber` or `TopicArn` parameters" (`Publish$TargetArn`) —
    /// the endpoint form of the same call.
    Endpoint(String),
}

/// What ONE `aws sns publish` command line asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishCall {
    /// The destination the line addressed.
    pub target: PublishTarget,
    /// `--message`, "the message you want to send" (`Publish$Message`, required).
    pub message: String,
    /// `--subject`, "optional parameter to be used as the 'Subject' line when the message is
    /// delivered to email endpoints" (`Publish$Subject`), and `None` for a line naming none.
    pub subject: Option<String>,
    /// `--message-structure`, "set `MessageStructure` to `json` if you want to send a different
    /// message for each protocol" (`Publish$MessageStructure`).
    pub message_structure: Option<String>,
    /// `--message-group-id`, "this parameter applies only to FIFO (first-in-first-out) topics"
    /// (`Publish$MessageGroupId`).
    pub message_group_id: Option<String>,
    /// `--message-deduplication-id`, which applies to the same topics
    /// (`Publish$MessageDeduplicationId`).
    pub message_deduplication_id: Option<String>,
    /// `--region`, "the region to use", which overrides the one the environment holds, and
    /// `None` for a line naming none — addressed to the Region its environment is in.
    pub region: Option<String>,
    /// `--output`, the format the answer is printed in.
    pub output: OutputFormat,
}

/// What the command line `arguments` — the words AFTER the program name — asks of the CLI's
/// notification commands, or `None` for every invocation this program performs nothing of.
///
/// It performs [`PUBLISH_COMMAND`] over the parameters that command takes: the destination
/// (`--topic-arn` or `--target-arn`), `--message`, `--subject`, `--message-structure`,
/// `--message-group-id`, `--message-deduplication-id`, and the global `--region` and
/// `--output`. Each is read in both forms the CLI takes a parameter's value in
/// ([`crate::aws_s3_cli::parameter_of`]). Every other command of the group, every other
/// parameter (each changes what the command does, so a line carrying one is not this
/// invocation), a line missing the members the call requires, one addressing two destinations at
/// once, and one carrying an operand the command does not take, is answered nothing for.
#[must_use]
pub fn requested_publish(arguments: &[String]) -> Option<PublishCall> {
    let mut words = arguments.iter().map(String::as_str);
    if words.next()? != SNS_COMMAND_GROUP {
        return None;
    }
    if !PERFORMED_SNS_COMMANDS.contains(&words.next()?) {
        return None;
    }
    let mut held: Vec<(&str, String)> = Vec::new();
    while let Some(word) = words.next() {
        // An operand is not a form this command takes: every member of the call is named. Both
        // forms the CLI takes a parameter's value in are read the one way this workspace reads
        // them, so one emulated CLI reads a line the same way whichever group it belongs to.
        let (parameter, value) = crate::aws_s3_cli::parameter_of(word, &mut words)?;
        // A parameter given twice is not an invocation either.
        if held.iter().any(|(named, _)| *named == parameter) {
            return None;
        }
        held.push((parameter, value));
    }
    let taken = |name: &str| {
        held.iter()
            .find(|(named, _)| *named == name)
            .map(|(_, value)| value.clone())
    };
    // Every parameter of the line has to be one this command takes: one it does not changes
    // what the call is.
    const TAKEN_PARAMETERS: [&str; 9] = [
        "topic-arn",
        "target-arn",
        "message",
        "subject",
        "message-structure",
        "message-group-id",
        "message-deduplication-id",
        "region",
        "output",
    ];
    if held
        .iter()
        .any(|(named, _)| !TAKEN_PARAMETERS.contains(named))
    {
        return None;
    }
    let target = match (taken("topic-arn"), taken("target-arn")) {
        (Some(topic), None) => PublishTarget::Topic(topic),
        (None, Some(endpoint)) => PublishTarget::Endpoint(endpoint),
        // A line addressing both, or neither, is not a call this command makes.
        _ => return None,
    };
    let output = match taken("output") {
        Some(written) => OutputFormat::of(&written)?,
        None => OutputFormat::default(),
    };
    Some(PublishCall {
        target,
        // The message is the one member the call requires, so a line naming none is not one.
        message: taken("message")?,
        subject: taken("subject"),
        message_structure: taken("message-structure"),
        message_group_id: taken("message-group-id"),
        message_deduplication_id: taken("message-deduplication-id"),
        region: taken("region").filter(|region| !region.is_empty()),
        output,
    })
}

/// Publish the message `call` names through `client`, and answer what the CLI prints for the
/// call: the members the service answered, in the format the line asked for.
///
/// The Region the line named ([`PublishCall::region`]) is the CALLER's to apply: it addresses
/// the client it hands over, exactly as a caller does for the low-level object commands
/// ([`crate::aws_s3_cli::ApiCall::region`]) — a client is where a Region is configured, so one
/// is not built here.
///
/// `Err` carries what the CLI reports about a call the service refused — its own error code and
/// message, in the CLI's words for one ([`service_answered`]).
pub async fn published(
    client: &::aws_sdk_sns::Client,
    call: &PublishCall,
) -> Result<String, String> {
    let mut request = client.publish().message(&call.message);
    request = match &call.target {
        PublishTarget::Topic(arn) => request.topic_arn(arn),
        PublishTarget::Endpoint(arn) => request.target_arn(arn),
    };
    if let Some(subject) = &call.subject {
        request = request.subject(subject);
    }
    if let Some(structure) = &call.message_structure {
        request = request.message_structure(structure);
    }
    if let Some(group) = &call.message_group_id {
        request = request.message_group_id(group);
    }
    if let Some(deduplication) = &call.message_deduplication_id {
        request = request.message_deduplication_id(deduplication);
    }
    let answered = request
        .send()
        .await
        .map_err(|error| service_answered(PUBLISH_OPERATION, &error))?;
    let members = [
        ("MessageId", answered.message_id().map(ToString::to_string)),
        (
            "SequenceNumber",
            answered.sequence_number().map(ToString::to_string),
        ),
    ];
    Ok(match call.output {
        OutputFormat::Json => crate::aws_s3_cli::printed_document(&members),
        OutputFormat::Text => crate::aws_s3_cli::printed_values(&members),
    })
}

#[cfg(test)]
mod tests {
    use super::{requested_publish, OutputFormat, PublishCall, PublishTarget};

    /// The words of a command line after the program name, as a session hands them to a
    /// program.
    fn words(line: &str) -> Vec<String> {
        line.split_whitespace().map(ToString::to_string).collect()
    }

    /// One call with the destination and message below and no other member named, which is the
    /// shortest invocation this program performs.
    fn call(target: PublishTarget, message: &str) -> PublishCall {
        PublishCall {
            target,
            message: message.to_string(),
            subject: None,
            message_structure: None,
            message_group_id: None,
            message_deduplication_id: None,
            region: None,
            output: OutputFormat::Json,
        }
    }

    /// The ARN of a topic, as a command line names one.
    fn topic(name: &str) -> PublishTarget {
        PublishTarget::Topic(format!("arn:aws:sns:us-west-2:000000000000:{name}"))
    }

    /// RULE: `aws sns publish` over the parameters that command takes is what this program
    /// performs of the notification group, and every other invocation is one it answers nothing
    /// for — so the session reports it as a command it could not run rather than as a call.
    ///
    /// The rows differ along which members the line names, the two forms a parameter's value is
    /// written in, the destination (a topic, an endpoint), the format asked for, and the forms
    /// that are not an invocation of this command at all.
    #[test]
    fn the_invocations_this_program_performs() {
        // (case, the arguments after `aws`, what it asks for)
        let rows: [(&str, &str, Option<PublishCall>); 14] = [
            (
                "the destination and the message alone",
                "sns publish --topic-arn arn:aws:sns:us-west-2:000000000000:alerts --message up",
                Some(call(topic("alerts"), "up")),
            ),
            (
                "a value written with an equals sign",
                "sns publish --topic-arn=arn:aws:sns:us-west-2:000000000000:alerts \
                 --message=down",
                Some(call(topic("alerts"), "down")),
            ),
            (
                "an endpoint rather than a topic",
                "sns publish --target-arn arn:aws:sns:us-west-2:000000000000:endpoint/APNS/one \
                 --message ping",
                Some(call(
                    PublishTarget::Endpoint(
                        "arn:aws:sns:us-west-2:000000000000:endpoint/APNS/one".to_string(),
                    ),
                    "ping",
                )),
            ),
            (
                "a subject and another Region",
                "sns publish --topic-arn arn:aws:sns:eu-west-1:000000000000:alerts --message up \
                 --subject Status --region eu-west-1",
                Some(PublishCall {
                    target: PublishTarget::Topic(
                        "arn:aws:sns:eu-west-1:000000000000:alerts".to_string(),
                    ),
                    subject: Some("Status".to_string()),
                    region: Some("eu-west-1".to_string()),
                    ..call(topic("alerts"), "up")
                }),
            ),
            (
                "a per-protocol message",
                "sns publish --topic-arn arn:aws:sns:us-west-2:000000000000:alerts \
                 --message {\"default\":\"up\"} --message-structure json",
                Some(PublishCall {
                    message: "{\"default\":\"up\"}".to_string(),
                    message_structure: Some("json".to_string()),
                    ..call(topic("alerts"), "up")
                }),
            ),
            (
                "the members a FIFO topic takes",
                "sns publish --topic-arn arn:aws:sns:us-west-2:000000000000:alerts.fifo \
                 --message up --message-group-id orders --message-deduplication-id one",
                Some(PublishCall {
                    target: topic("alerts.fifo"),
                    message_group_id: Some("orders".to_string()),
                    message_deduplication_id: Some("one".to_string()),
                    ..call(topic("alerts"), "up")
                }),
            ),
            (
                "the tab-separated output format",
                "sns publish --topic-arn arn:aws:sns:us-west-2:000000000000:alerts --message up \
                 --output text",
                Some(PublishCall {
                    output: OutputFormat::Text,
                    ..call(topic("alerts"), "up")
                }),
            ),
            (
                "another command of the group",
                "sns create-topic --name alerts",
                None,
            ),
            ("another command group", "sqs send-message --queue-url q", None),
            (
                "a line naming no message",
                "sns publish --topic-arn arn:aws:sns:us-west-2:000000000000:alerts",
                None,
            ),
            (
                "a line naming no destination",
                "sns publish --message up",
                None,
            ),
            (
                "a line addressing two destinations at once",
                "sns publish --topic-arn arn:aws:sns:us-west-2:000000000000:alerts \
                 --target-arn arn:aws:sns:us-west-2:000000000000:endpoint/APNS/one --message up",
                None,
            ),
            (
                "a parameter this command does not take",
                "sns publish --topic-arn arn:aws:sns:us-west-2:000000000000:alerts --message up \
                 --message-attributes file:///tmp/attributes.json",
                None,
            ),
            (
                "an output format this program does not print",
                "sns publish --topic-arn arn:aws:sns:us-west-2:000000000000:alerts --message up \
                 --output yaml",
                None,
            ),
        ];
        for (case, line, expected) in rows {
            let asked = requested_publish(&words(line));
            assert_eq!(
                asked, expected,
                "case {case:?}: expected {expected:?}, got {asked:?}"
            );
        }
    }

    /// RULE: a parameter with no value after it, and one given twice, is not an invocation this
    /// program performs either — so neither is read as a call with a member missing or a member
    /// of the caller's choosing.
    #[test]
    fn the_forms_that_name_no_call() {
        // (case, the arguments after `aws`)
        let rows: [(&str, &str); 3] = [
            (
                "a parameter with no value after it",
                "sns publish --topic-arn arn:aws:sns:us-west-2:000000000000:alerts --message",
            ),
            (
                "a parameter given twice",
                "sns publish --topic-arn arn:aws:sns:us-west-2:000000000000:alerts \
                 --message one --message two",
            ),
            (
                "an operand, which this command takes none of",
                "sns publish arn:aws:sns:us-west-2:000000000000:alerts up",
            ),
        ];
        for (case, line) in rows {
            let asked = requested_publish(&words(line));
            assert!(
                asked.is_none(),
                "case {case:?}: expected no call, got {asked:?}"
            );
        }
    }
}
