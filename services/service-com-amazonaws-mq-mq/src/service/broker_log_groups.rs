// SPDX-License-Identifier: CC0-1.0

//! The log groups a BROKER READ answers that this checkout's Smithy model does not
//! declare.
//!
//! # The rule
//!
//! A broker read reports, beside the modelled logs summary, one entry per kind of log the
//! broker's ENGINE keeps — general first, then audit — each entry naming the kind, whether
//! that kind of logging is enabled on the broker, and the CloudWatch Logs log group the
//! kind's records are sent to.
//!
//! # The gap this closes
//!
//! The model a service crate is generated from declares the kinds only as the members of
//! one structure — `LogsSummary$general` with `$generalLogGroup`, and `$audit` with
//! `$auditLogGroup`
//! (`smithy_query(query="shape",shape="LogsSummary",
//! model_path="services/service-com-amazonaws-mq-mq/smithy-model/com-amazonaws-mq-mq.json")`)
//! — and declares no LIST of them anywhere, while the served wire carries both forms: the
//! summary, and the array a client enumerates a broker's log groups from (to subscribe to
//! them, to deep-link to them, or to assert that logging is off). A client reading the
//! array found no member at all here, which reads as a broker with no log group — which a
//! broker always has, since the groups are named after the broker itself.
//!
//! A member the model does not declare cannot be a field of a generated output shape, so
//! it cannot be written from a handler. It is written HERE instead, at the one seam where
//! this service renders a SUCCESS body ([`super::iso8601_timestamps`]).
//!
//! Nothing here invents a value: every entry is built from the summary the handler already
//! answered — the log group it derived from the broker's own id, and the flag it read off
//! the broker's stored logging settings — so the two forms always report one answer,
//! whatever a broker's logging is set to. A kind the broker's engine does not keep
//! contributes no entry: `Logs$audit` "does not apply to RabbitMQ brokers", so such a
//! broker reports neither the audit flag nor its group, and the array it answers holds the
//! general entry alone.
//!
//! The body is re-read and re-written as JSON (a member the model does not declare cannot
//! be written through the schema serializer), which also puts each object's members in key
//! order — the order of a JSON object's members carries no meaning on this protocol.

use aws_smithy_runtime_api::http::Response;
use aws_smithy_types::body::SdkBody;
use serde_json::{Map, Value};

/// The operation whose success body carries the log groups: the broker read.
const DESCRIBE_BROKER: &str = "DescribeBroker";

/// The member the log groups are carried under.
const LOG_GROUPS: &str = "logGroups";

/// The modelled summary every entry is read from (`DescribeBrokerResponse$logs`).
const LOGS: &str = "logs";

/// The member of an entry naming the kind of log it is about.
const ENTRY_NAME: &str = "name";

/// The member of an entry reporting whether that kind of logging is enabled.
const ENTRY_ENABLED: &str = "enabled";

/// The member of an entry naming the CloudWatch Logs log group.
const ENTRY_LOG_GROUP: &str = "logGroup";

/// One kind of log a broker keeps: the name an entry carries for it, and the members of
/// the modelled summary that report whether it is enabled and which group it is sent to.
struct LogKind {
    name: &'static str,
    enabled: &'static str,
    log_group: &'static str,
}

/// The kinds of log a broker keeps, in the order a read lists them: general, then audit.
///
/// Source: `LogsSummary`'s own members — `general`/`generalLogGroup` and
/// `audit`/`auditLogGroup` — which are the two kinds this service's model knows of and the
/// names the last segment of each group's path carries ([`crate::state::LOG_KIND_GENERAL`],
/// [`crate::state::LOG_KIND_AUDIT`]).
const LOG_KINDS: &[LogKind] = &[
    LogKind {
        name: crate::state::LOG_KIND_GENERAL,
        enabled: "general",
        log_group: "generalLogGroup",
    },
    LogKind {
        name: crate::state::LOG_KIND_AUDIT,
        enabled: "audit",
        log_group: "auditLogGroup",
    },
];

/// `body` as this service sends it for `operation` — the broker's log groups listed beside
/// the modelled summary they are read from — or `None` when this rule owns nothing about
/// it: another operation's body, a body that is not a JSON object, one carrying no summary,
/// or one that already carries the member.
#[must_use]
pub(crate) fn served_success_body(operation: &str, body: &[u8]) -> Option<Vec<u8>> {
    if operation != DESCRIBE_BROKER {
        return None;
    }
    let mut document: Value = serde_json::from_slice(body).ok()?;
    let object = document.as_object_mut()?;
    if object.contains_key(LOG_GROUPS) {
        return None;
    }
    let listed = Value::Array(log_groups(object.get(LOGS)?.as_object()?));
    object.insert(LOG_GROUPS.to_string(), listed);
    serde_json::to_vec(&document).ok()
}

/// The entries `summary` names a log group in: one per kind the broker keeps, in the order
/// [`LOG_KINDS`] gives. A kind the summary names no group for is one the broker's engine
/// does not keep, so it contributes nothing.
fn log_groups(summary: &Map<String, Value>) -> Vec<Value> {
    LOG_KINDS
        .iter()
        .filter_map(|kind| {
            let log_group = summary.get(kind.log_group)?.as_str()?;
            // The flag that pairs with the group. A kind whose group is reported carries
            // its flag as a value, `false` when that logging is off; a summary that
            // somehow carried none reports the logging as off rather than as absent.
            let enabled = summary
                .get(kind.enabled)
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let mut entry = Map::new();
            entry.insert(ENTRY_NAME.to_string(), Value::from(kind.name));
            entry.insert(ENTRY_ENABLED.to_string(), Value::from(enabled));
            entry.insert(ENTRY_LOG_GROUP.to_string(), Value::from(log_group));
            Some(Value::Object(entry))
        })
        .collect()
}

/// `resp` as this service answers `operation`: a success whose body carries the log groups
/// the served wire lists ([`served_success_body`]).
///
/// A response this rule owns nothing about is returned untouched — status, headers and
/// every byte of the body exactly as the renderer wrote them. Only a SUCCESS carries the
/// member: a broker read that failed answers the modelled error document, which names no
/// log group.
#[must_use]
pub(crate) fn with_served_log_groups(operation: &str, resp: Response) -> Response {
    if !resp.status().is_success() {
        return resp;
    }
    let rewritten = {
        // A success body of this protocol is built in memory by its renderer, so the bytes
        // are readable here; a streamed body carries no member list and is left alone.
        let Some(bytes) = resp.body().bytes() else {
            return resp;
        };
        match served_success_body(operation, bytes) {
            Some(rewritten) => rewritten,
            None => return resp,
        }
    };
    with_body(resp, rewritten)
}

/// `resp` carrying `body` instead of the bytes its renderer wrote, with a `content-length`
/// the renderer advertised corrected to describe it.
fn with_body(mut resp: Response, body: Vec<u8>) -> Response {
    if resp.headers().get("content-length").is_some() {
        let length = body.len().to_string();
        // A decimal length is always a valid header value, so this cannot fail today; if it
        // ever did, the header is REMOVED rather than left describing the body before the
        // rewrite, so the two can never disagree.
        if resp
            .headers_mut()
            .try_insert("content-length", length)
            .is_err()
        {
            resp.headers_mut().remove("content-length");
        }
    }
    *resp.body_mut() = SdkBody::from(body);
    resp
}

#[cfg(test)]
mod tests {
    use super::{served_success_body, with_served_log_groups, DESCRIBE_BROKER, LOG_GROUPS};
    use aws_smithy_types::body::SdkBody;
    use serde_json::{json, Value};

    /// A response at `status` carrying `body`.
    fn answer(status: u16, body: &str) -> aws_smithy_runtime_api::http::Response {
        aws_smithy_runtime_api::http::Response::new(
            aws_smithy_runtime_api::http::StatusCode::try_from(status).expect("a valid status"),
            SdkBody::from(body),
        )
    }

    /// A 200 response carrying `body`.
    fn response(body: &str) -> aws_smithy_runtime_api::http::Response {
        answer(200, body)
    }

    /// RULE over the bodies the framework renderer writes for a broker read: the log groups
    /// are listed beside the summary, one entry per kind the broker keeps, general first,
    /// each carrying the summary's own flag and group. The rows differ along which kinds the
    /// broker keeps and which of them are enabled.
    #[test]
    fn a_broker_read_lists_one_entry_per_log_kind_it_keeps() {
        let rows: &[(&str, Value, Value)] = &[
            (
                "both kinds, logging off",
                json!({"brokerId": "b-1", "logs": {
                    "general": false, "generalLogGroup": "/aws/amazonmq/broker/b-1/general",
                    "audit": false, "auditLogGroup": "/aws/amazonmq/broker/b-1/audit",
                }}),
                json!([
                    {"name": "general", "enabled": false,
                     "logGroup": "/aws/amazonmq/broker/b-1/general"},
                    {"name": "audit", "enabled": false,
                     "logGroup": "/aws/amazonmq/broker/b-1/audit"},
                ]),
            ),
            (
                "both kinds, both enabled",
                json!({"brokerId": "b-2", "logs": {
                    "general": true, "generalLogGroup": "/aws/amazonmq/broker/b-2/general",
                    "audit": true, "auditLogGroup": "/aws/amazonmq/broker/b-2/audit",
                }}),
                json!([
                    {"name": "general", "enabled": true,
                     "logGroup": "/aws/amazonmq/broker/b-2/general"},
                    {"name": "audit", "enabled": true,
                     "logGroup": "/aws/amazonmq/broker/b-2/audit"},
                ]),
            ),
            (
                "general enabled, audit off",
                json!({"brokerId": "b-3", "logs": {
                    "general": true, "generalLogGroup": "/aws/amazonmq/broker/b-3/general",
                    "audit": false, "auditLogGroup": "/aws/amazonmq/broker/b-3/audit",
                }}),
                json!([
                    {"name": "general", "enabled": true,
                     "logGroup": "/aws/amazonmq/broker/b-3/general"},
                    {"name": "audit", "enabled": false,
                     "logGroup": "/aws/amazonmq/broker/b-3/audit"},
                ]),
            ),
            (
                "a broker whose engine keeps no audit log",
                json!({"brokerId": "b-4", "logs": {
                    "general": false, "generalLogGroup": "/aws/amazonmq/broker/b-4/general",
                }}),
                json!([
                    {"name": "general", "enabled": false,
                     "logGroup": "/aws/amazonmq/broker/b-4/general"},
                ]),
            ),
        ];

        for (case, rendered, expected) in rows {
            let bytes = serde_json::to_vec(rendered).expect("the row serializes");
            let rewritten = served_success_body(DESCRIBE_BROKER, &bytes)
                .unwrap_or_else(|| panic!("case {case:?}: expected a rewritten body"));
            let document: Value =
                serde_json::from_slice(&rewritten).expect("the rewritten body is JSON");
            let listed = document.get(LOG_GROUPS);
            assert_eq!(
                listed,
                Some(expected),
                "case {case:?}: expected the log groups {expected:?}, got {document}"
            );
            assert_eq!(
                document.get("logs"),
                rendered.get("logs"),
                "case {case:?}: the modelled summary must be unchanged, got {document}"
            );
            assert_eq!(
                document.get("brokerId"),
                rendered.get("brokerId"),
                "case {case:?}: every other member must be unchanged, got {document}"
            );
        }
    }

    /// Nothing to rewrite: another operation's body, a body carrying no summary, one that
    /// already carries the member, one that is not an object, and one that is not JSON.
    #[test]
    fn a_body_this_rule_does_not_own_is_left_alone() {
        for (case, operation, body) in [
            (
                "another operation",
                "ListBrokers",
                r#"{"logs":{"general":false,"generalLogGroup":"/g"}}"#,
            ),
            ("no summary member", DESCRIBE_BROKER, r#"{"brokerId":"b-1"}"#),
            (
                "already served",
                DESCRIBE_BROKER,
                r#"{"logs":{"general":false,"generalLogGroup":"/g"},"logGroups":[]}"#,
            ),
            ("not an object", DESCRIBE_BROKER, "[]"),
            ("not JSON", DESCRIBE_BROKER, "<Error/>"),
        ] {
            assert!(
                served_success_body(operation, body.as_bytes()).is_none(),
                "case {case:?}: expected no rewrite"
            );
            let answered = with_served_log_groups(operation, response(body));
            assert_eq!(
                answered.body().bytes().expect("bytes"),
                body.as_bytes(),
                "case {case:?}: the body must be byte-identical"
            );
        }
    }

    /// Only a SUCCESS carries the member: a broker read that FAILED answers the modelled
    /// error document, whatever that document happens to hold. The rows span the statuses
    /// this operation's modelled errors answer at.
    #[test]
    fn an_answer_that_is_not_a_success_is_left_alone() {
        let body = r#"{"logs":{"general":false,"generalLogGroup":"/aws/amazonmq/broker/b-1/general"}}"#;
        for status in [400u16, 403, 404, 500] {
            let answered = with_served_log_groups(DESCRIBE_BROKER, answer(status, body));
            assert_eq!(
                answered.body().bytes().expect("bytes"),
                body.as_bytes(),
                "case {status}: the body must be byte-identical"
            );
        }
    }

    /// A rewritten body is described by the `content-length` the renderer advertised.
    #[test]
    fn a_rewritten_body_is_described_by_its_advertised_length() {
        let body = r#"{"logs":{"general":false,"generalLogGroup":"/aws/amazonmq/broker/b-1/general"}}"#;
        let mut resp = response(body);
        resp.headers_mut()
            .insert("content-length", body.len().to_string());
        let answered = with_served_log_groups(DESCRIBE_BROKER, resp);
        let bytes = answered.body().bytes().expect("bytes").to_vec();
        let advertised = answered.headers().get("content-length");
        assert_eq!(
            advertised,
            Some(bytes.len().to_string().as_str()),
            "the advertised length must describe the rewritten body, got {advertised:?}"
        );
    }
}
