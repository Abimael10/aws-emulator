// SPDX-License-Identifier: CC0-1.0

//! The AWS CLI's `aws xray put-trace-segments`, as a program a caller-supplied command line can
//! ask for: what one command line asks of the CLI, and the call that submits the segment
//! documents it names.
//!
//! # The rule this module implements
//!
//! Several emulated services run a command line a client supplied — a container's `command`, a
//! cluster step's arguments, a build's commands — and such a line reaches a service of the
//! account through the AWS CLI, which "provides direct access to the public APIs of Amazon Web
//! Services" one command at a time. `aws xray put-trace-segments` "uploads segment documents to
//! Amazon Web Services X-Ray" (`PutTraceSegments`), so a command line that submits a segment has
//! that segment ingested: the trace it belongs to becomes retrievable through the reads X-Ray
//! publishes for it — `GetTraceSummaries` and `BatchGetTraces` — and the status the command exits
//! with is the status of that call. WHO made the call is whichever principal the session running
//! the line signs with, so X-Ray judges it against the policies in force on that principal: an
//! instrumented workload whose role does not carry the action is refused, and the command reports
//! that refusal and exits with the status the CLI answers a service error with — which is what a
//! program written `if aws xray put-trace-segments …; then … else … fi` branches on.
//!
//! That is ONE rule, so it lives here once rather than per service, beside the sibling command
//! groups ([`crate::aws_s3_cli`], [`crate::aws_sns_cli`], [`crate::aws_cloudwatch_cli`]): two
//! services running the same command line must not submit a different document or answer a
//! different status. What each service keeps is its own — the SESSION the line runs in and WHICH
//! credential the call is signed with.
//!
//! The documents themselves are the CALLER's: "the segment document" is a JSON document the
//! instrumented workload writes, so what travels is the text the line carried, unread and
//! unchanged, and X-Ray's own ingest decides what it is worth
//! ([`crate::xray_segments`] is the sibling path for a service reporting its OWN work).
//!
//! Nothing here builds a client, resolves a name, opens a socket or starts a process. The client
//! these functions are HANDED is the one this workspace constructs for that hop
//! ([`crate::xray_segments::xray_client_through`], [`crate::xray_segments::xray_client_signed_as`]),
//! whose HTTP client IS the emulator's own in-process callback
//! (`smithy_server_runtime::EmbeddedEmulator::http_client`): a `send()` on it is dispatched inside
//! this process to the emulated X-Ray service and reaches nothing off it — the SDK's own HTTPS
//! stack is not even compiled in (`aws-sdk-xray` is declared `default-features = false`), and the
//! endpoint URL that client carries is addressing for that transport rather than a host, built from
//! the Region alone. So the Region a command line names selects an endpoint of THIS emulator and
//! can never be an address, and the documents a line carries travel to the emulated service and
//! nowhere else.

use crate::aws_cli_options::{CommandLine, OutputFormat, GLOBAL_PARAMETERS};

/// The CLI's command GROUP this program answers for: `aws xray`, the commands of AWS X-Ray.
pub const XRAY_COMMAND_GROUP: &str = "xray";

/// The command of that group this program performs, as the CLI spells it: `put-trace-segments`.
/// SOURCE: the `aws xray` command reference. Every other command of the group — and every other
/// group — is one this program performs nothing of.
pub const PERFORMED_XRAY_COMMANDS: [&str; 1] = [PUT_TRACE_SEGMENTS_COMMAND];

/// `put-trace-segments`, the one command of [`PERFORMED_XRAY_COMMANDS`].
const PUT_TRACE_SEGMENTS_COMMAND: &str = "put-trace-segments";

/// The API operation that command calls, as the CLI names it in its account of a refusal.
pub const PUT_TRACE_SEGMENTS_OPERATION: &str = "PutTraceSegments";

/// The one parameter of the command: "`--trace-segment-documents` (list) A string containing a
/// JSON document defining one or more segments or subsegments"
/// (`PutTraceSegments$TraceSegmentDocuments`, required).
const TRACE_SEGMENT_DOCUMENTS_PARAMETER: &str = "trace-segment-documents";

/// What ONE `aws xray put-trace-segments` command line asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PutTraceSegmentsCall {
    /// The segment documents the line named, in the order it wrote them.
    pub documents: Vec<String>,
    /// `--region`, and `None` for a line naming none.
    pub region: Option<String>,
    /// `--output`, the format the answer is printed in.
    pub output: OutputFormat,
}

/// What the command line `arguments` — the words AFTER the program name — asks of the CLI's X-Ray
/// commands, or `None` for every invocation this program performs nothing of.
///
/// It performs [`PUT_TRACE_SEGMENTS_COMMAND`] over the one parameter that command takes and the
/// options every command takes. Every other command of the group, every other parameter, and a
/// line naming no document at all is answered nothing for.
#[must_use]
pub fn requested_put_trace_segments(arguments: &[String]) -> Option<PutTraceSegmentsCall> {
    let mut words = arguments.iter().map(String::as_str);
    if words.next()? != XRAY_COMMAND_GROUP {
        return None;
    }
    if !PERFORMED_XRAY_COMMANDS.contains(&words.next()?) {
        return None;
    }
    let line = CommandLine::read(words, &[], &[TRACE_SEGMENT_DOCUMENTS_PARAMETER])?;
    let mut taken: Vec<&str> = vec![TRACE_SEGMENT_DOCUMENTS_PARAMETER];
    taken.extend(GLOBAL_PARAMETERS);
    if !line.names_only(&taken) {
        return None;
    }
    Some(PutTraceSegmentsCall {
        // The documents are the one member the call requires, so a line naming none is not an
        // invocation.
        documents: line.many(TRACE_SEGMENT_DOCUMENTS_PARAMETER)?.to_vec(),
        region: line.region(),
        output: match line.one(crate::aws_cli_options::OUTPUT_PARAMETER) {
            Some(written) => OutputFormat::of(written)?,
            None => OutputFormat::default(),
        },
    })
}

/// Submit the documents `call` names through `client`, and answer what the CLI prints for the
/// call: the response document, in the format the line asked for.
///
/// `client` is the in-process hop described in [the module docs](self): its transport is the
/// emulator's own callback, so the `send()` below dispatches to the emulated X-Ray service inside
/// this process and the documents a command line carried reach nothing off it.
///
/// The Region the line named ([`PutTraceSegmentsCall::region`]) is the CALLER's to apply: it
/// addresses the client it hands over — which is to say which emulated endpoint of this process
/// that client is built for — exactly as the sibling groups' calls do.
///
/// `Err` carries what the CLI reports about a call the service refused — its own error code and
/// message, in the CLI's words for one ([`crate::aws_cli_options::service_answered`]).
pub async fn submitted_trace_segments(
    client: &::aws_sdk_xray::Client,
    call: &PutTraceSegmentsCall,
) -> Result<String, String> {
    let answered = client
        .put_trace_segments()
        .set_trace_segment_documents(Some(call.documents.clone()))
        .send()
        .await
        .map_err(|error| {
            crate::aws_cli_options::service_answered(PUT_TRACE_SEGMENTS_OPERATION, &error)
        })?;
    // "Segments that failed processing" is the one member the operation answers
    // (`PutTraceSegments$UnprocessedTraceSegments`), each with the segment's id and what X-Ray
    // reported about it.
    let unprocessed: Vec<::serde_json::Value> = answered
        .unprocessed_trace_segments()
        .iter()
        .map(|segment| {
            let mut members = ::serde_json::Map::new();
            for (member, held) in [
                ("Id", segment.id()),
                ("ErrorCode", segment.error_code()),
                ("Message", segment.message()),
            ] {
                if let Some(held) = held {
                    members.insert(member.to_string(), ::serde_json::Value::String(held.to_string()));
                }
            }
            ::serde_json::Value::Object(members)
        })
        .collect();
    let document = ::serde_json::json!({ "UnprocessedTraceSegments": unprocessed });
    Ok(crate::aws_cli_options::printed_answer(&document, call.output))
}

#[cfg(test)]
mod tests {
    use super::{requested_put_trace_segments, PutTraceSegmentsCall};
    use crate::aws_cli_options::OutputFormat;

    /// The words of a command line after the program name, as a session hands them to a program.
    fn words(line: &str) -> Vec<String> {
        line.split_whitespace().map(ToString::to_string).collect()
    }

    /// One call submitting `documents` with no other option named.
    fn call(documents: &[&str]) -> PutTraceSegmentsCall {
        PutTraceSegmentsCall {
            documents: documents
                .iter()
                .map(|document| (*document).to_string())
                .collect(),
            region: None,
            output: OutputFormat::default(),
        }
    }

    /// RULE: `aws xray put-trace-segments` over the one parameter that command takes is what this
    /// program performs of the X-Ray group, and every other invocation is one it answers nothing
    /// for — so the session reports it as a command it could not run rather than as a call.
    ///
    /// The rows differ along what the rule generalises over: how many documents the line carries,
    /// the two spellings a value is written in, which options it names beside them, and the forms
    /// that are not an invocation of this command at all.
    #[test]
    fn the_invocations_this_program_performs() {
        // (case, the arguments after `aws`, what it asks for)
        let rows: [(&str, &str, Option<PutTraceSegmentsCall>); 7] = [
            (
                "one document",
                "xray put-trace-segments --trace-segment-documents {\"name\":\"frame\"}",
                Some(call(&["{\"name\":\"frame\"}"])),
            ),
            (
                "two documents, the words after the parameter",
                "xray put-trace-segments --trace-segment-documents {\"id\":\"one\"} {\"id\":\"two\"}",
                Some(call(&["{\"id\":\"one\"}", "{\"id\":\"two\"}"])),
            ),
            (
                "a document written with an equals sign",
                "xray put-trace-segments --trace-segment-documents={\"id\":\"one\"}",
                Some(call(&["{\"id\":\"one\"}"])),
            ),
            (
                "another Region and the tab-separated format",
                "xray put-trace-segments --trace-segment-documents {\"id\":\"one\"} \
                 --region eu-west-1 --output text",
                Some(PutTraceSegmentsCall {
                    region: Some("eu-west-1".to_string()),
                    output: OutputFormat::Text,
                    ..call(&["{\"id\":\"one\"}"])
                }),
            ),
            (
                "another command of the group",
                "xray get-trace-summaries --start-time 0",
                None,
            ),
            ("a line naming no document", "xray put-trace-segments", None),
            (
                "a parameter this command does not take",
                "xray put-trace-segments --trace-segment-documents {\"id\":\"one\"} \
                 --cli-input-json file:///tmp/in.json",
                None,
            ),
        ];
        for (case, line, expected) in rows {
            let asked = requested_put_trace_segments(&words(line));
            assert_eq!(
                asked, expected,
                "case {case:?}: expected {expected:?}, got {asked:?}"
            );
        }
    }
}
