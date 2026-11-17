// SPDX-License-Identifier: CC0-1.0

//! The one class of published event a rule's STATE decides a match on: the management
//! (control-plane) API calls CloudTrail delivers onto an account's default event bus.
//!
//! Every management call a logging trail of the account records is published onto that bus
//! under the `detail-type` `AWS API Call via CloudTrail`, the calls that only REPORTED the
//! account included, each record carrying its own read/write kind as `readOnly`
//! (`service_common::api_call_events`). An account reads far more than it writes, so which
//! of them a rule is matched against is the rule's own: `ENABLED` "matches events against
//! the rule, except for Amazon Web Services management events delivered through CloudTrail",
//! and `ENABLED_WITH_ALL_CLOUDTRAIL_MANAGEMENT_EVENTS` is "enabled for all events, including
//! Amazon Web Services management events delivered through CloudTrail"
//! (`smithy_query(query="doc", shape="PutRuleRequest", member="State")`). What that parts is
//! the READS: a management call that changed the account is matched by either state, and a
//! read-only one only by the second.
//!
//! The test is made on the ENVELOPE, not on who published it: the two facts it reads are the
//! `detail-type` EventBridge itself stamps on this channel and the `readOnly` of the
//! CloudTrail record carried as the event's `detail`. So a rule's state decides the same
//! match whichever publication the event arrived by, and an event of any other kind is
//! matched by its pattern alone.

use super::RuleRecord;

/// The `detail-type` CloudTrail's management-event fan-out carries — the shared constant the
/// publisher stamps it with, so the channel is named once for the whole emulator.
pub use ::service_common::api_call_events::API_CALL_DETAIL_TYPE;

/// The envelope member the `detail-type` sits at.
const DETAIL_TYPE_FIELD: &str = "detail-type";

/// The path, inside the envelope, of the CloudTrail record's own read/write kind.
const DETAIL_READ_ONLY_PATH: &str = "/detail/readOnly";

/// Whether `envelope` is a READ-ONLY management event delivered through CloudTrail: an event
/// of this channel whose record reports the call as a read.
///
/// A record that reports no `readOnly` at all is not one: nothing says the call only reported
/// the account, so the event is matched like any other.
#[must_use]
pub fn is_read_only_management_event(envelope: &::serde_json::Value) -> bool {
    envelope.get(DETAIL_TYPE_FIELD).and_then(::serde_json::Value::as_str)
        == Some(API_CALL_DETAIL_TYPE)
        && envelope.pointer(DETAIL_READ_ONLY_PATH) == Some(&::serde_json::Value::Bool(true))
}

/// Whether a rule in `state` is matched against `envelope` at all.
///
/// The pattern decides every other event; this decides the one class a rule's state speaks
/// to, which is the module's subject. A rule reaching routing has already been admitted by
/// [`RuleRecord::matches_published_events`], so `DISABLED` is not re-tested here.
#[must_use]
pub fn matched_by_state(envelope: &::serde_json::Value, admits_read_only: bool) -> bool {
    admits_read_only || !is_read_only_management_event(envelope)
}

/// Whether `rule` is matched against the read-only management events of this channel, as
/// [`matched_by_state`] takes it.
#[must_use]
pub fn admits_read_only(rule: &RuleRecord) -> bool {
    rule.matches_cloudtrail_read_only_events()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One published envelope with the given `detail-type` and `detail`.
    fn envelope(detail_type: &str, detail: ::serde_json::Value) -> ::serde_json::Value {
        ::serde_json::json!({
            "source": "aws.cloudfront",
            "detail-type": detail_type,
            "detail": detail,
        })
    }

    /// The reads of this channel are parted from everything else: only a record of this
    /// `detail-type` reporting `readOnly` true is one.
    #[test]
    fn only_a_read_only_record_of_this_channel_is_a_read_only_management_event() {
        for (case, envelope, expected) in [
            (
                "a read-only API call via CloudTrail",
                envelope(API_CALL_DETAIL_TYPE, ::serde_json::json!({"readOnly": true})),
                true,
            ),
            (
                "a write API call via CloudTrail",
                envelope(API_CALL_DETAIL_TYPE, ::serde_json::json!({"readOnly": false})),
                false,
            ),
            (
                "a record of this channel reporting no kind at all",
                envelope(API_CALL_DETAIL_TYPE, ::serde_json::json!({})),
                false,
            ),
            (
                "a service event via CloudTrail",
                envelope(
                    "AWS Service Event via CloudTrail",
                    ::serde_json::json!({"readOnly": true}),
                ),
                false,
            ),
            (
                "an event of a service's own channel",
                envelope("Object Created", ::serde_json::json!({"readOnly": true})),
                false,
            ),
            (
                "an event carrying no detail at all",
                ::serde_json::json!({"detail-type": API_CALL_DETAIL_TYPE}),
                false,
            ),
        ] {
            let actual = is_read_only_management_event(&envelope);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected is_read_only_management_event {expected}, got {actual}"
            );
        }
    }

    /// What a rule's state parts: the reads of this channel, and nothing else.
    #[test]
    fn a_rules_state_parts_the_reads_of_this_channel_alone() {
        let read = envelope(API_CALL_DETAIL_TYPE, ::serde_json::json!({"readOnly": true}));
        let write = envelope(API_CALL_DETAIL_TYPE, ::serde_json::json!({"readOnly": false}));
        let other = envelope("Object Created", ::serde_json::json!({"size": 1}));
        for (case, envelope, admits_read_only, expected) in [
            ("a read against an ENABLED rule", &read, false, false),
            ("a read against an all-management rule", &read, true, true),
            ("a write against an ENABLED rule", &write, false, true),
            ("a write against an all-management rule", &write, true, true),
            ("another event against an ENABLED rule", &other, false, true),
            (
                "another event against an all-management rule",
                &other,
                true,
                true,
            ),
        ] {
            let actual = matched_by_state(envelope, admits_read_only);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected matched_by_state {expected}, got {actual}"
            );
        }
    }
}
