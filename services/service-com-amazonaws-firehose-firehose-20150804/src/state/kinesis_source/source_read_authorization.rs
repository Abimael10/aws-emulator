// SPDX-License-Identifier: CC0-1.0

//! Whether a Firehose stream's source MAY be read at all, and what a read it may not make
//! reports.
//!
//! A stream fed by a Kinesis data stream does not receive its data: it RETRIEVES it, under
//! the role its source configuration names ("the ARN of the role that provides access to the
//! source stream" — `KinesisStreamSourceConfiguration$RoleARN`). The retrieval is therefore
//! that role's to make, exactly as a delivery is the destination role's
//! ([`crate::state::delivery`]): it happens only where the permissions in force on the role
//! allow the Kinesis Data Streams calls the retrieval is made of ([`SOURCE_READ_ACTIONS`]),
//! and the decision is IAM's, asked of IAM
//! (`service_common::iam_role::role_withholds_action_through`).
//!
//! A read the role may not make takes nothing out of the source, so the stream's shard
//! positions stay where they are and its destination gets nothing — and since no client call
//! is made for a source-fed stream at all, the only thing an operator can observe of it is
//! the stream's own logging: one event in the `LogGroupName` / `LogStreamName` the
//! destination's `CloudWatchLoggingOptions` name, under the `errorCode`
//! [`KINESIS_ACCESS_DENIED`], naming the refused call, the source stream and the role
//! ([`access_denied_message`]).
//!
//! Nothing is withheld where the question cannot be answered — a source naming no role, a
//! role this emulator's IAM does not hold, an emulator with no IAM mounted — which is how
//! every other authorization question in this crate reads a non-answer.
//!
//! ## Lock order
//!
//! IAM is asked with NO store of this service locked (the cursors are read first and the
//! lock released), and the report then locks this service's own store and the CloudWatch Logs
//! store in turn, never both at once — the order the delivery's own reporting takes.

use std::time::{Duration, SystemTime};

use crate::state::delivery::destination_logging::{report_events, source_error_event, ReportedEvent};
use crate::state::delivery::{delivery_region, DeliveryHop};
use crate::state::service_state;

use super::SourceCursor;

/// The `errorCode` a source read the stream's role is not allowed to make is published
/// under, in the `<source service>.<condition>` form Firehose publishes these under — the
/// same form the destination conditions carry
/// (`crate::state::delivery::destination_logging::S3_ACCESS_DENIED`).
pub const KINESIS_ACCESS_DENIED: &str = "Kinesis.AccessDenied";

/// The Kinesis Data Streams calls a Firehose stream's retrieval from its source is made of,
/// in the order the retrieval makes them.
///
/// The read discovers the source's shards (`DescribeStream`), takes a position in each of
/// them (`GetShardIterator`) and reads the records from that position (`GetRecords`) — the
/// three calls read-only access to a Kinesis data stream is stated with (the
/// `KinesisStreamReadPolicy` of
/// `service_com_amazonaws_cloudformation_cloudformation::state::sam::policy_templates`, the
/// same set a client's own consumer of a stream needs). A role that allows all three may make
/// the read; the FIRST one its permissions withhold is the refused call a report names.
/// `kinesis:ListShards` is the OTHER way to discover a stream's shards, so a role that allows
/// `DescribeStream` is not refused for leaving it out.
const SOURCE_READ_ACTIONS: &[&str] = &[
    "kinesis:DescribeStream",
    "kinesis:GetShardIterator",
    "kinesis:GetRecords",
];

/// How long the answer IAM gave about a stream's source read stands before the question is
/// put again, which is also the cadence a refusal that stands is reported at.
///
/// A CHOSEN span: AWS documents no cadence for these events — the real service reports a
/// refused retrieval for as long as it keeps attempting one — so this is the emulator's,
/// short enough that an operator watching the log stream sees the refusal promptly, and that a
/// permission granted in place restores the retrieval promptly, and long enough that a poll of
/// the source ([`super::SOURCE_POLL_SPAN`]) costs neither an authorization question nor a
/// reported event of its own. A refusal nobody repairs therefore reports at this cadence for
/// as long as it stands, which is what the real service does too and which the destination log
/// stream's own retention ceiling bounds (`report_events`).
pub const SOURCE_AUTHORIZATION_SPAN: Duration = Duration::from_secs(1);

/// What IAM answered about one stream's source read, and when it was asked.
///
/// Kept on the stream ([`crate::state::DeliveryStream::source_read_decision`]) so the polls
/// inside one [`SOURCE_AUTHORIZATION_SPAN`] stand on the one answer rather than asking again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceReadDecision {
    /// When the question was put to IAM.
    pub asked_at: SystemTime,
    /// The first call of [`SOURCE_READ_ACTIONS`] the role may not make, or `None` when the
    /// read may be made.
    pub refused: Option<&'static str>,
}

impl SourceReadDecision {
    /// Whether this answer still stands at `now`.
    fn stands_at(&self, now: SystemTime) -> bool {
        matches!(now.duration_since(self.asked_at), Ok(since) if since < SOURCE_AUTHORIZATION_SPAN)
    }
}

/// What stands for `cursor`'s source read at `now`: the first call the role may not make, or
/// `None` when the read may be made.
///
/// The answer IAM last gave about this stream stands for [`SOURCE_AUTHORIZATION_SPAN`], so a
/// stream polled five times a second is not asked about five times a second; once it is older
/// than that the question is put again and a refusal it answers is REPORTED as it is taken,
/// which is what makes a refusal that stands reported for as long as it stands at that same
/// cadence rather than once per poll.
pub(super) async fn source_read_refusal<Eng: Send + Sync + 'static>(
    engine: &Eng,
    hop: &DeliveryHop,
    cursor: &SourceCursor,
    now: SystemTime,
) -> Option<&'static str> {
    if let Some(decision) = cursor.read_decision {
        if decision.stands_at(now) {
            return decision.refused;
        }
    }
    let refused = refused_source_read(hop, cursor).await;
    record_decision(engine, &cursor.stream, refused, now);
    if let Some(action) = refused {
        report_refused_source_read(engine, cursor, action, now);
    }
    refused
}

/// Keep what IAM answered about this stream's source read, so the next polls inside the span
/// stand on it rather than asking again.
fn record_decision<Eng: Send + Sync + 'static>(
    engine: &Eng,
    name: &str,
    refused: Option<&'static str>,
    now: SystemTime,
) {
    let Some(state) = service_state(engine) else {
        return;
    };
    let mut store = state.lock();
    if let Some(stream) = store.streams.get_mut(name) {
        stream.source_read_decision = Some(SourceReadDecision {
            asked_at: now,
            refused,
        });
    }
}

/// The first of [`SOURCE_READ_ACTIONS`] the role of `cursor`'s source may not make, or
/// `None` when nothing stands in the way of the read.
///
/// `None` as well for every question that could not be ANSWERED: a source configuration
/// naming no role, a hop with no emulator to ask, a stream ARN carrying no Region to ask in,
/// and anything short of IAM answering that the role does not allow the call — a role this
/// account's IAM holds nothing under included, exactly as
/// `service_common::iam_role::role_withholds_action_through` reads it.
async fn refused_source_read(hop: &DeliveryHop, cursor: &SourceCursor) -> Option<&'static str> {
    let emulator = hop.emulator()?;
    let role_arn = cursor.role_arn.as_deref().filter(|arn| !arn.is_empty())?;
    let region = delivery_region(&cursor.stream_arn)?;
    for action in SOURCE_READ_ACTIONS {
        if ::service_common::iam_role::role_withholds_action_through(
            emulator,
            region,
            role_arn,
            action,
            &cursor.source_arn,
        )
        .await
        {
            return Some(action);
        }
    }
    None
}

/// Report that `action` on `cursor`'s source stream was refused, through the logging each of
/// the stream's destinations enables.
///
/// A stream whose destinations enable no logging reports nothing — there is nowhere a reader
/// could find it — and the refusal is then observable only as the absence of any delivery,
/// which is what disabling those options asks for.
fn report_refused_source_read<Eng: Send + Sync + 'static>(
    engine: &Eng,
    cursor: &SourceCursor,
    action: &str,
    now: SystemTime,
) {
    let Some(region) = delivery_region(&cursor.stream_arn) else {
        return;
    };
    let role_arn = cursor.role_arn.clone().unwrap_or_default();
    let message = access_denied_message(action, &cursor.source_arn, &role_arn);
    let document = source_error_event(
        &cursor.stream_arn,
        cursor.version_id,
        KINESIS_ACCESS_DENIED,
        &message,
    );
    let events: Vec<ReportedEvent> = cursor
        .loggings
        .iter()
        .map(|logging| ReportedEvent {
            logging: logging.clone(),
            message: document.clone(),
        })
        .collect();
    if events.is_empty() {
        return;
    }
    report_events(engine, region, events, now);
}

/// What [`KINESIS_ACCESS_DENIED`] states: the call the source read was refused, the source
/// stream it was refused on and the role it was attempted under.
///
/// The fixed part is the sentence the service states for an access-denied condition — a role
/// Firehose has to be able to assume, with an access policy that allows the call — and the
/// variable parts are read from the stream's own source configuration, so none of them is
/// fixed text.
#[must_use]
fn access_denied_message(action: &str, source_arn: &str, role_arn: &str) -> String {
    format!(
        "Access was denied when calling Kinesis. Ensure that the trust policy for the \
         provided IAM role {role_arn} allows Firehose to assume the role, and the access \
         policy allows the {action} action on the Kinesis data stream {source_arn}."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the message states the refused call, the source stream and the role, so an
    /// operator reads which permission is missing from whose policy — none of it fixed text.
    #[test]
    fn the_message_names_the_refused_call_the_source_and_the_role() {
        let account = ::engine::emulator_account_id();
        // (case, the refused call, the source stream, the role)
        let rows = [
            (
                "the records read",
                "kinesis:GetRecords",
                format!("arn:aws:kinesis:us-east-1:{account}:stream/ingest"),
                format!("arn:aws:iam::{account}:role/firehose-source"),
            ),
            (
                "the shard iterator",
                "kinesis:GetShardIterator",
                format!("arn:aws:kinesis:eu-west-2:{account}:stream/another"),
                format!("arn:aws:iam::{account}:role/service-role/reader"),
            ),
        ];
        for (case, action, source_arn, role_arn) in rows {
            let message = access_denied_message(action, &source_arn, &role_arn);
            for named in [action, source_arn.as_str(), role_arn.as_str()] {
                assert!(
                    message.contains(named),
                    "case {case:?}: expected the message to name {named:?}, got {message:?}"
                );
            }
        }
    }
}
