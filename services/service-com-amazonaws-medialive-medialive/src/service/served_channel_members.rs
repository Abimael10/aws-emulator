// SPDX-License-Identifier: CC0-1.0

//! The `maintenanceStatus` member a recorded MediaLive channel body carries, on the
//! channel-returning shapes whose model snapshot does not declare it.
//!
//! # The gap this closes
//!
//! A channel's maintenance surface reaches the wire as the modelled sub-structure
//! `Maintenance` (jsonName `maintenance`, target `MaintenanceStatus` = {maintenanceDay,
//! maintenanceStartTime, maintenanceDeadline, maintenanceScheduledDate}), which every
//! channel-returning shape binds and this service already serves out of the channel record,
//! and — beside it — the flat `maintenanceStatus` member.
//!
//! That flat member is declared by exactly one shape of this model snapshot:
//! `smithy_query(query="operation",operation="RestartChannelPipelines")` reports
//! `MaintenanceStatus`, jsonName `maintenanceStatus`, target `__string`, documented as the
//! time in milliseconds by when a pipeline-VM restart must occur. A recorded `DescribeChannel`
//! and `DeleteChannel` of a channel with no such event scheduled, and a recorded
//! `CreateChannel`'s `$.channel`, carry that same member as the EMPTY STRING — the member is
//! on the wire with no deadline in it, which is a value a client reads rather than a missing
//! key.
//!
//! What this module writes is therefore exactly that: the member, holding no deadline. This
//! emulator runs no maintenance scheduler and schedules no pipeline-VM restart for any
//! channel, so no channel has a deadline to report — the same reason the one handler whose
//! shape DOES declare the member gives for the value it answers
//! ([`crate::ops::restart_channel_pipelines`]). Nothing is invented to fill the key: no form
//! beyond the empty string is grounded in the model or in a recording, and a channel's
//! scheduled date and deadline are reported where the model declares them, in the
//! `maintenance` sub-structure.
//!
//! A member the model does not declare on a shape cannot be a field of that generated shape
//! (`smithy_macros::smithy` refuses a field corresponding to no Smithy member), so no handler
//! and no schema serializer in this crate can write it there. It is written HERE instead, over
//! the body the framework's own serializer just rendered, at the seam this service already
//! runs every answer through ([`super::error_envelope::ErrorEnvelopeOperation`]).
//!
//! `RestartChannelPipelines` is deliberately NOT one of the operations below: its own shape
//! declares the member, so its handler owns what that response reports and this rewrite must
//! not reverse it. The crate holds one rule for the member — no event scheduled, no deadline —
//! and each response renders it where the shape that carries it decides. Every OTHER
//! channel-returning shape is covered, because the member belongs to the channel body rather
//! than to one operation: a recording covers the describe, the delete and the create's
//! `$.channel`, and the remaining four answer that same channel object, so a channel read
//! through one of them cannot report differently from a describe of it.
//!
//! The rewrite ITSELF is not this module's: the body is read and written back value by value
//! through the shared plumbing every service whose wire carries an unmodelled member rewrites
//! a rendered success body with ([`service_common::success_body_members`]), so every value the
//! renderer wrote — a pipelines count, an egress address, a channel id — is written back byte
//! for byte, and a response this rule owns nothing about (a failure, a streamed body, another
//! operation's answer) is left exactly as the renderer wrote it.
//!
//! REMOVE this rewrite when the model is refreshed to a snapshot that declares the member on
//! every channel-returning shape: it is then a field of each output shape like any other, and
//! `tests/all/channel_maintenance_and_egress.rs` pins the wire form either way.

use ::aws_smithy_runtime_api::http::Response as RtResponse;
use ::serde_json::value::RawValue;
use ::service_common::success_body_members::{raw_value, with_rewritten_success_body, RawObject};

/// The operations whose success body IS the channel object: the channel-description
/// responses, which carry one member set between them
/// (`smithy_query(query="operation",operation="DescribeChannel,DeleteChannel,StartChannel,StopChannel")`).
///
/// A recording covers the first two (`$.maintenanceStatus` on a describe and on a delete);
/// the start and the stop answer that same object, so a channel read through one of them
/// reports what a describe of it reports.
///
/// `RestartChannelPipelines` carries that same member set and is deliberately NOT here: its
/// own shape DECLARES the member, so its handler owns what that response reports.
const CHANNEL_BODY_OPERATIONS: [&str; 4] = [
    "DeleteChannel",
    "DescribeChannel",
    "StartChannel",
    "StopChannel",
];

/// The operations whose success body carries that same channel object under one member:
/// their output is `{Channel -> "channel"}`
/// (`smithy_query(query="operation",operation="CreateChannel,UpdateChannel,UpdateChannelClass")`).
///
/// A recording covers the create (`$.channel.maintenanceStatus`); the two updates answer the
/// same object, and this service's own suites compare an update's answer against a describe
/// of the same channel member by member, so the two cannot report a channel differently.
const CHANNEL_MEMBER_OPERATIONS: [&str; 3] =
    ["CreateChannel", "UpdateChannel", "UpdateChannelClass"];

/// The member those three carry the channel under.
const CHANNEL_MEMBER: &str = "channel";

/// The modelled maintenance sub-structure, which is what makes a body a channel's.
const MAINTENANCE: &str = "maintenance";

/// The flat member the wire carries beside that sub-structure.
const MAINTENANCE_STATUS: &str = "maintenanceStatus";

/// What a channel with no pipeline-VM restart scheduled reports as its maintenance status:
/// the member is on the wire, and holds no deadline.
const NO_DEADLINE: &str = "";

/// Whether this value is a channel — one carrying the modelled maintenance sub-structure
/// every channel-returning shape binds. A body that is not one is left alone: there is no
/// channel in it to report a maintenance status for.
fn is_a_channel(channel: &RawValue) -> bool {
    ::serde_json::from_str::<RawObject>(channel.get())
        .is_ok_and(|object| object.contains_key(MAINTENANCE))
}

/// The channel object `channel` as this service sends it — its maintenance status on the
/// wire — or `None` when this rule owns nothing about it: a value that is not a channel, or
/// one already carrying the member.
fn served_channel(channel: &RawValue) -> Option<Vec<u8>> {
    if !is_a_channel(channel) {
        return None;
    }
    // The member's own bytes, rendered before the object that borrows them is built.
    let reported = ::serde_json::to_string(NO_DEADLINE).ok()?;
    let mut object: RawObject = ::serde_json::from_str(channel.get()).ok()?;
    if object.contains_key(MAINTENANCE_STATUS) {
        return None;
    }
    object.insert(MAINTENANCE_STATUS.to_string(), raw_value(reported.as_bytes())?);
    ::serde_json::to_vec(&object).ok()
}

/// `body` as this service sends it for `operation` — the maintenance status on the channel
/// the body carries — or `None` when this rule owns nothing about it: an operation whose
/// recording does not cover the member, a body that carries no channel, or one already
/// carrying the member.
#[must_use]
pub fn served_success_body(operation: &str, body: &[u8]) -> Option<Vec<u8>> {
    if CHANNEL_BODY_OPERATIONS.contains(&operation) {
        return served_channel(raw_value(body)?);
    }
    if !CHANNEL_MEMBER_OPERATIONS.contains(&operation) {
        return None;
    }
    // The channel's own rewritten bytes, taken first so they outlive the body object that
    // carries them.
    let served = {
        let object: RawObject = ::serde_json::from_slice(body).ok()?;
        served_channel(object.get(CHANNEL_MEMBER).copied()?)?
    };
    let mut object: RawObject = ::serde_json::from_slice(body).ok()?;
    object.insert(CHANNEL_MEMBER.to_string(), raw_value(&served)?);
    ::serde_json::to_vec(&object).ok()
}

/// `resp` as this service answers `operation`: a success whose channel carries the member
/// the recorded wire carries ([`served_success_body`]).
#[must_use]
pub fn with_served_success_body(operation: &str, resp: RtResponse) -> RtResponse {
    with_rewritten_success_body(resp, |body| served_success_body(operation, body))
}

#[cfg(test)]
mod tests {
    //! The rewriting rule, driven on bodies built by hand. The wire behaviour it adds up to
    //! — which members a channel body carries, over this service's own wire — is
    //! `tests/all/channel_maintenance_and_egress.rs`.

    use super::*;
    use ::serde_json::Value;

    /// A channel body carrying the modelled maintenance sub-structure `maintenance`.
    fn channel(maintenance: &str) -> String {
        format!(r#"{{"id":"1000000","maintenance":{maintenance},"pipelinesRunningCount":0}}"#)
    }

    /// RULE: the member is on the wire holding no deadline, whatever the channel's modelled
    /// maintenance settings say — this emulator schedules no pipeline-VM restart for any
    /// channel, so none has a deadline to report — and the modelled sub-structure is left
    /// exactly as the renderer wrote it. The rows differ along what those settings carry,
    /// including a scheduled date and a deadline, which the sub-structure reports and this
    /// member does not.
    #[test]
    fn the_member_is_on_the_wire_holding_no_deadline() {
        for (case, maintenance) in [
            (
                "a window and nothing scheduled",
                r#"{"maintenanceDay":"THURSDAY","maintenanceStartTime":"02:00"}"#,
            ),
            (
                "maintenance scheduled",
                r#"{"maintenanceDay":"FRIDAY","maintenanceStartTime":"15:00","maintenanceScheduledDate":"2031-02-03"}"#,
            ),
            (
                "a deadline the sub-structure reports",
                r#"{"maintenanceDay":"SUNDAY","maintenanceStartTime":"00:00","maintenanceDeadline":"2031-02-10T00:00:00Z"}"#,
            ),
            ("settings naming no window at all", "{}"),
        ] {
            let body = channel(maintenance);
            let rewritten = served_success_body("DescribeChannel", body.as_bytes())
                .unwrap_or_else(|| panic!("case {case:?}: expected a rewritten body"));
            let document: Value =
                ::serde_json::from_slice(&rewritten).expect("the rewritten body is JSON");
            assert_eq!(
                document[MAINTENANCE_STATUS].as_str(),
                Some(NO_DEADLINE),
                "case {case:?}: expected the member to hold no deadline, got {document}"
            );
            let settings: Value =
                ::serde_json::from_str(maintenance).expect("the row's settings are JSON");
            assert_eq!(
                document[MAINTENANCE], settings,
                "case {case:?}: the modelled sub-structure must be unchanged, got {document}"
            );
            assert_eq!(
                document["id"].as_str(),
                Some("1000000"),
                "case {case:?}: every other member must be unchanged, got {document}"
            );
        }
    }

    /// RULE: the member goes ON the channel, wherever the body carries it — at the root for
    /// the channel-description responses, and under the `channel` member for the create.
    #[test]
    fn the_member_is_reported_on_the_channel_the_body_carries() {
        let maintenance = r#"{"maintenanceDay":"TUESDAY","maintenanceStartTime":"21:00"}"#;

        for operation in CHANNEL_BODY_OPERATIONS {
            let body = channel(maintenance);
            let rewritten = served_success_body(operation, body.as_bytes())
                .unwrap_or_else(|| panic!("case {operation:?}: expected a rewritten body"));
            let document: Value =
                ::serde_json::from_slice(&rewritten).expect("the rewritten body is JSON");
            assert_eq!(
                document[MAINTENANCE_STATUS].as_str(),
                Some(NO_DEADLINE),
                "case {operation:?}: expected the member at the root, got {document}"
            );
        }

        for operation in CHANNEL_MEMBER_OPERATIONS {
            let body = format!(r#"{{"channel":{}}}"#, channel(maintenance));
            let rewritten = served_success_body(operation, body.as_bytes())
                .unwrap_or_else(|| panic!("case {operation:?}: expected a rewritten body"));
            let document: Value =
                ::serde_json::from_slice(&rewritten).expect("the rewritten body is JSON");
            assert_eq!(
                document[CHANNEL_MEMBER][MAINTENANCE_STATUS].as_str(),
                Some(NO_DEADLINE),
                "case {operation:?}: expected the member on the channel, got {document}"
            );
            assert!(
                document[MAINTENANCE_STATUS].is_null(),
                "case {operation:?}: the member belongs to the channel, not beside it, got \
                 {document}"
            );
        }
    }

    /// RULE: every value the body already carried is written back byte for byte — a
    /// renderer's own digits above all, which no re-rendering of a number may touch. The
    /// rows differ along the kind of value sitting beside the maintenance settings.
    #[test]
    fn every_value_the_body_already_carried_is_written_back_unchanged() {
        for (case, body, kept) in [
            (
                "a count",
                r#"{"maintenance":{"maintenanceDay":"MONDAY"},"pipelinesRunningCount":2}"#,
                "2",
            ),
            (
                "a trailing zero a shorter rendering would drop",
                r#"{"maintenance":{"maintenanceDay":"MONDAY"},"score":1.2500}"#,
                "1.2500",
            ),
            (
                "escaped text",
                r#"{"maintenance":{"maintenanceDay":"MONDAY"},"name":"a \"quoted\" name"}"#,
                r#"a \"quoted\" name"#,
            ),
        ] {
            let rewritten = served_success_body("DescribeChannel", body.as_bytes())
                .unwrap_or_else(|| panic!("case {case:?}: expected a rewritten body"));
            let rendered = String::from_utf8(rewritten).expect("the rewritten body is text");
            assert!(
                rendered.contains(kept),
                "case {case:?}: expected {kept:?} written back unchanged, got {rendered}"
            );
        }
    }

    /// Nothing to rewrite: an operation this rule does not cover — the listing, whose
    /// entries are a summary shape of their own, and the pipeline restart, whose own shape
    /// declares the member so that its handler owns it — a body carrying no channel, one
    /// that already carries the member, one that is not an object, and one that is not JSON.
    #[test]
    fn a_body_this_rule_does_not_own_is_left_alone() {
        let channel = channel(r#"{"maintenanceDay":"MONDAY","maintenanceStartTime":"02:00"}"#);
        for operation in ["ListChannels", "RestartChannelPipelines"] {
            assert!(
                served_success_body(operation, channel.as_bytes()).is_none(),
                "case {operation:?}: the body must be left alone"
            );
        }

        for (case, operation, body) in [
            ("no maintenance settings", "DescribeChannel", r#"{"id":"1000000"}"#),
            (
                "already served",
                "DescribeChannel",
                r#"{"maintenance":{"maintenanceDay":"MONDAY"},"maintenanceStatus":""}"#,
            ),
            ("not an object", "DescribeChannel", "[]"),
            ("not JSON", "DescribeChannel", "<Error/>"),
            (
                "the channel member is not an object",
                "CreateChannel",
                r#"{"channel":null}"#,
            ),
        ] {
            assert!(
                served_success_body(operation, body.as_bytes()).is_none(),
                "case {case:?}: the body must be left alone"
            );
        }
    }

    /// A failure is never rewritten: its body is this service's own refusal, whose wording
    /// belongs to the error envelope's path.
    #[test]
    fn a_failure_body_is_left_alone() {
        let refusal = r#"{"message":"no","maintenance":{"maintenanceDay":"MONDAY"}}"#;
        let resp = RtResponse::new(
            ::aws_smithy_runtime_api::http::StatusCode::try_from(404u16).expect("a status"),
            ::aws_smithy_types::body::SdkBody::from(refusal),
        );
        let answered = with_served_success_body("DescribeChannel", resp);
        assert_eq!(
            answered.body().bytes().expect("bytes"),
            refusal.as_bytes(),
            "a failure body must be byte-identical"
        );
    }
}
