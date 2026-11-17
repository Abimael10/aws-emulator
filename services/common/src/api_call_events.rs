// SPDX-License-Identifier: CC0-1.0

//! The management-event FAN-OUT: a served control-plane call published onto the account's
//! default EventBridge bus, as the event CloudTrail delivers for it.
//!
//! An account has three readers of its management events. One is CloudTrail's own
//! `LookupEvents`, which reads the log [`crate::management_events`] holds. The second is
//! EventBridge: while a trail of the Region is logging management events, EVERY call that
//! trail logs is also published onto the account's default event bus, under the
//! `detail-type` [`API_CALL_DETAIL_TYPE`] with `source` `aws.<service>` and the CloudTrail
//! record of the call as the event's `detail`. That is the only path a control-plane call of
//! a service with no event source of its own has to an event-driven consumer, and it is what
//! a rule like
//! `{"source":["aws.cloudfront"],"detail-type":["AWS API Call via CloudTrail"]}` matches.
//! The third reads the trail's own bucket, which is [`crate::trail_log_files`]' leg of the
//! same delivery and travels with the event here.
//!
//! A call that only REPORTED state is published on this channel like any other, carrying
//! `readOnly` true in its record: WHICH rules such an event reaches is EventBridge's to
//! decide from the rule's own state, not the publisher's to withhold — so a consumer asking
//! for all of an account's management events sees the reads too, while one that did not is
//! driven by the calls that changed the account. The one read nothing is published for is a
//! read this emulator's own plumbing made ([`is_plumbing_read`]), which in AWS is no API call
//! of the account at all.
//!
//! The publication therefore hangs off the SAME seam the recording does
//! ([`crate::audited_operation`]): the service that served the call records it, the log
//! answers whether the account's CloudTrail is delivering that Region's management events
//! ([`crate::management_events::ManagementEventLog::record_delivered`]), and what comes
//! back is published here. No service states the channel for itself.
//!
//! EventBridge is reached the way any emulated service reaches another: through the
//! in-process callback ([`smithy_server_runtime::OperationContext::callback`]), with the
//! client this crate's one construction of it builds
//! ([`crate::eventbridge_client`]), so this module names no service crate. Publication is
//! BEST EFFORT — an emulator with no EventBridge mounted has nowhere to publish, and a
//! caller is never failed because the fan-out could not be made.

use smithy_server_runtime::EmbeddedEmulator;

/// The `detail-type` CloudTrail's management-event fan-out carries, which is what a rule
/// written for this channel matches on.
pub const API_CALL_DETAIL_TYPE: &str = "AWS API Call via CloudTrail";

/// The bus the fan-out publishes onto: "AWS events are published to the default event bus"
/// — a rule on any other bus sees none of them.
pub const DEFAULT_EVENT_BUS: &str = "default";

/// The `source` prefix every AWS-published event carries, ahead of the service's own
/// endpoint prefix (`aws.cloudfront`).
pub const AWS_SOURCE_PREFIX: &str = "aws.";

/// The suffix a CloudTrail `eventSource` carries after the service's endpoint prefix
/// (`cloudfront.amazonaws.com`), which is what is stripped to build the event `source`.
const EVENT_SOURCE_SUFFIX: &str = ".amazonaws.com";

/// The `source` of the event published for a call recorded under `event_source`.
///
/// CloudTrail names the serving service as `<endpoint-prefix>.amazonaws.com` and
/// EventBridge names the same service as `aws.<endpoint-prefix>`, so the one is derived
/// from the other rather than stated twice per service. A value that does not carry the
/// suffix is taken whole, which is the honest reading of a source this rule does not
/// recognise.
#[must_use]
pub fn event_source_of(event_source: &str) -> String {
    let prefix = event_source
        .strip_suffix(EVENT_SOURCE_SUFFIX)
        .unwrap_or(event_source);
    format!("{AWS_SOURCE_PREFIX}{prefix}")
}

/// One recorded call, as what the account's CloudTrail delivers for it.
///
/// Built by the log that recorded the call — so both the `detail` and each log file's record
/// are the very document `LookupEvents` reports and none of them can disagree — and
/// delivered by [`DeliveredApiCall::deliver`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveredApiCall {
    /// The published event's `source`: `aws.<endpoint-prefix>` of the serving service.
    pub source: String,
    /// The Region the event is published in, which is the Region the call was recorded in.
    pub region: String,
    /// The instant the call was recorded, as whole milliseconds since the epoch: the
    /// published event's `time`, which is WHEN the call was served and not when the
    /// publication was made.
    pub time_millis: i64,
    /// The published event's `detail`: the CloudTrail record of the call
    /// ([`crate::management_events::ManagementEvent::to_cloudtrail_event_json`]).
    pub detail: serde_json::Value,
    /// The log files the account's logging trails deliver this record as — one per trail
    /// that logs the call, into the bucket that trail was created with
    /// ([`crate::trail_log_files`]). Empty when no logging trail of the account has a
    /// destination bucket.
    pub log_files: Vec<crate::trail_log_files::DeliveredLogFile>,
    /// Whether the recorded call only REPORTED the account, as its record's `readOnly` says.
    ///
    /// Carried because it decides one thing about the delivery: a read an emulated service
    /// made in-process while serving a call of its own is this emulator's own plumbing and no
    /// API call of the account, so no event is published for it ([`is_plumbing_read`]).
    pub read_only: bool,
}

/// Whether a served management call is one this emulator's own PLUMBING made rather than one
/// a principal of the account made: a READ one emulated service makes to another through the
/// in-process callback while serving a call of its own.
///
/// In AWS such a read is the serving service's own machinery and no API call of the account
/// at all — EventBridge reading the policy in force over a target queue before it hands an
/// event over is the shape — so the account's CloudTrail publishes nothing for it. In this
/// emulator one service reaches another ONLY through the in-process callback, whose chain
/// depth the framework stamps on every such request as an EXTENSION
/// ([`smithy_server_runtime::CallbackRecursionDepth`]), so that stamp is what tells the two
/// apart: an extension never travels over the wire, so nothing a client sends — on any
/// socket, signed with any credential — carries one.
///
/// It is the READS that are withheld, and only they. A call that CHANGED the account is the
/// account's change however it was reached (a stack provisioning a resource, a function
/// writing one), and publishing it is what drives the rules written over an account's
/// writes. Withholding the reads is also what keeps this channel from feeding itself:
/// delivering an event reads the documents in force over the targets it is delivered to, and
/// an event published for each of those reads would be delivered again, read again, without
/// end.
#[must_use]
pub fn is_plumbing_read(parts: &::http::request::Parts, read_only: bool) -> bool {
    read_only
        && parts
            .extensions
            .get::<::smithy_server_runtime::CallbackRecursionDepth>()
            .is_some()
}

impl DeliveredApiCall {
    /// Deliver this record everywhere the account's CloudTrail delivers it: into the bucket
    /// of each trail that logs the call, and onto the account's default event bus.
    ///
    /// `parts` is the request of the call being recorded, because one fact about that request
    /// decides the BUS leg: a read this emulator's own plumbing made is published nowhere
    /// ([`is_plumbing_read`]). The trail's bucket receives it either way — that leg is
    /// CloudTrail's record of what the log holds.
    ///
    /// Nothing about the call that produced it changes: a failure is traced and the
    /// operation's own answer stands, for the reason the module docs give.
    pub async fn deliver(self, parts: &::http::request::Parts, callback: &EmbeddedEmulator) {
        for log_file in self.log_files {
            log_file.deliver(callback).await;
        }
        if is_plumbing_read(parts, self.read_only) {
            return;
        }
        let client = crate::eventbridge_client::eventbridge_client(callback, &self.region);
        let entry = ::aws_sdk_eventbridge::types::PutEventsRequestEntry::builder()
            .event_bus_name(DEFAULT_EVENT_BUS)
            .source(&self.source)
            .detail_type(API_CALL_DETAIL_TYPE)
            // "If no time stamp is provided, the time stamp of the PutEvents call is
            // used" — the call's own instant is provided, so the event is stamped when
            // the call was served.
            .time(::aws_sdk_eventbridge::primitives::DateTime::from_millis(
                self.time_millis,
            ))
            .detail(self.detail.to_string())
            .build();
        let answered = client.put_events().entries(entry).send().await;
        match answered {
            Ok(answer) if answer.failed_entry_count() > 0 => {
                ::tracing::warn!(
                    source = %self.source,
                    region = %self.region,
                    "the management-event fan-out was not accepted by the default event bus"
                );
            }
            Ok(_) => {}
            Err(error) => {
                ::tracing::warn!(
                    source = %self.source,
                    region = %self.region,
                    error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                    "the management-event fan-out could not be published"
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A request carrying the callback stamp `depth`, or none at all.
    fn request(depth: Option<u8>) -> ::http::request::Parts {
        let mut request = ::http::Request::builder().method("POST").uri("/");
        if let Some(depth) = depth {
            request = request.extension(::smithy_server_runtime::CallbackRecursionDepth(depth));
        }
        request
            .body(())
            .expect("a well-formed request")
            .into_parts()
            .0
    }

    /// What the publisher withholds is one class of call: a READ that arrived through the
    /// in-process callback. The rows differ along both facts that decide it.
    #[test]
    fn only_a_read_made_through_the_in_process_callback_is_plumbing() {
        for (case, depth, read_only, expected) in [
            ("a client's read", None, true, false),
            ("a client's write", None, false, false),
            ("a read one service made for another", Some(1), true, true),
            ("a read made deeper in a chain", Some(7), true, true),
            ("a write one service made for another", Some(1), false, false),
        ] {
            let parts = request(depth);
            let actual = is_plumbing_read(&parts, read_only);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected is_plumbing_read {expected}, got {actual}"
            );
        }
    }

    #[test]
    fn an_event_source_becomes_the_bus_source_of_the_same_service() {
        for (event_source, expected) in [
            ("cloudfront.amazonaws.com", "aws.cloudfront"),
            ("iam.amazonaws.com", "aws.iam"),
            ("elasticloadbalancing.amazonaws.com", "aws.elasticloadbalancing"),
            // A value that does not carry the suffix is taken whole rather than
            // truncated at a guess.
            ("cloudfront", "aws.cloudfront"),
        ] {
            let actual = event_source_of(event_source);
            assert_eq!(
                actual, expected,
                "case {event_source:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }
}
