// SPDX-License-Identifier: CC0-1.0

//! What `GetConnection` reports under `Identity`, driven over HTTP through the same
//! `MultiServiceRouter` the production emulator serves with.
//!
//! > **A member is reported only where the connection's opening handshake carried a value for
//! > it.** `Identity` declares `SourceIp` and `UserAgent`
//! > (`smithy_query(query="shape", shape="Identity")`), each read off the record the handshake
//! > left; a client that sent no `User-Agent` field has no user agent, so the answer carries
//! > NO `userAgent` member rather than one synthesised as the empty string. "Absent" and
//! > "empty" are different answers to a caller: an empty string says the client named itself
//! > that way.
//!
//! The rows differ along what the rule generalises over: whether the handshake carried a user
//! agent at all, the form of the value when it did (a token, one carrying spaces and
//! punctuation, the empty one), and the address family of the source address — one row of
//! which is the whole response envelope, member for member.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use engine::InMemoryEngine;
use serde_json::Value;

use crate::support::{as_wire_timestamp, call_at, minted, register_identity, rendered, HOST};

/// A registration instant in the past, so the reported timestamps are this record's rather
/// than whatever the clock reads while the suite runs.
fn connected_at() -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000)
}

/// One connection's recorded identity: the address it was established from, and what its
/// opening handshake named it as when it named anything.
struct Row {
    case: &'static str,
    source_ip: &'static str,
    /// `None` is a handshake that carried no `User-Agent` field at all.
    user_agent: Option<&'static str>,
}

const ROWS: &[Row] = &[
    Row {
        case: "a client naming itself with a token",
        source_ip: "192.0.2.10",
        user_agent: Some("wscat/5.2.0"),
    },
    Row {
        case: "a value carrying spaces and punctuation",
        source_ip: "198.51.100.7",
        user_agent: Some("Mozilla/5.0 (compatible; emulator probe)"),
    },
    Row {
        case: "an IPv6 client",
        source_ip: "2001:db8::1",
        user_agent: Some("curl/8.5.0"),
    },
    Row {
        case: "a client that named itself with the empty value",
        source_ip: "192.0.2.11",
        user_agent: Some(""),
    },
    Row {
        case: "a client whose handshake carried no User-Agent field",
        source_ip: "203.0.113.9",
        user_agent: None,
    },
];

/// The answer's `identity` member as a JSON object, naming the case when it is not one.
fn identity_of(case: &str, body: &bytes::Bytes) -> serde_json::Map<String, Value> {
    let document: Value = serde_json::from_slice(body)
        .unwrap_or_else(|e| panic!("case {case:?}: the answer must be JSON ({e}): {body:?}"));
    document
        .get("identity")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: the answer must carry an identity object, got {}",
                rendered(body)
            )
        })
}

#[tokio::test]
async fn an_identity_member_is_reported_only_where_the_handshake_carried_one() {
    for row in ROWS {
        let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
        let connection_id = minted(row.case);
        register_identity(
            &engine,
            &connection_id,
            row.source_ip,
            row.user_agent,
            connected_at(),
        );

        let (status, body) = call_at(
            &engine,
            HOST,
            "GET",
            &format!("/@connections/{connection_id}"),
            Vec::new(),
        )
        .await;
        assert_eq!(
            status,
            200,
            "case {:?}: expected the modelled 200, got {status} {}",
            row.case,
            rendered(&body)
        );

        let identity = identity_of(row.case, &body);
        let source_ip = identity.get("sourceIp").and_then(Value::as_str);
        assert_eq!(
            source_ip,
            Some(row.source_ip),
            "case {:?}: expected identity.sourceIp {:?}, got {source_ip:?}",
            row.case,
            row.source_ip
        );

        let user_agent = identity.get("userAgent").and_then(Value::as_str);
        assert_eq!(
            user_agent, row.user_agent,
            "case {:?}: expected identity.userAgent {:?}, got {user_agent:?} in {}",
            row.case,
            row.user_agent,
            rendered(&body)
        );
        // Absent is not the empty string: a handshake that carried no value leaves the member
        // out of the object altogether.
        assert_eq!(
            identity.contains_key("userAgent"),
            row.user_agent.is_some(),
            "case {:?}: expected the userAgent member present={}, got the object {:?}",
            row.case,
            row.user_agent.is_some(),
            identity
        );
    }
}

/// The ENVELOPE around that identity: `GetConnection` reports exactly the modelled members
/// `GetConnectionResponse` declares — `connectedAt`, `identity` and `lastActiveAt`
/// (`smithy_query(query="shape", shape="GetConnectionResponse")`) — with both timestamps the
/// stored instants in the `date-time` form the model's `__timestampIso8601` declares, and
/// `identity` carrying no member beyond the two the shape declares.
#[tokio::test]
async fn the_reported_envelope_carries_the_modelled_members_and_nothing_else() {
    for row in ROWS {
        let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
        let connection_id = minted(row.case);
        register_identity(
            &engine,
            &connection_id,
            row.source_ip,
            row.user_agent,
            connected_at(),
        );

        let (status, body) = call_at(
            &engine,
            HOST,
            "GET",
            &format!("/@connections/{connection_id}"),
            Vec::new(),
        )
        .await;
        assert_eq!(
            status,
            200,
            "case {:?}: expected the modelled 200, got {status} {}",
            row.case,
            rendered(&body)
        );

        let document: Value = serde_json::from_slice(&body).unwrap_or_else(|e| {
            panic!("case {:?}: the answer must be JSON ({e})", row.case)
        });
        let members: Vec<&str> = document
            .as_object()
            .map(|object| object.keys().map(String::as_str).collect())
            .unwrap_or_default();
        assert_eq!(
            members,
            vec!["connectedAt", "identity", "lastActiveAt"],
            "case {:?}: expected exactly the modelled members, got {}",
            row.case,
            rendered(&body)
        );

        let wire_instant = as_wire_timestamp(connected_at());
        for member in ["connectedAt", "lastActiveAt"] {
            let reported = document.get(member).and_then(Value::as_str);
            assert_eq!(
                reported,
                Some(wire_instant.as_str()),
                "case {:?}: expected {member} {wire_instant:?}, got {reported:?}",
                row.case
            );
        }

        let identity = identity_of(row.case, &body);
        let declared: Vec<&str> = identity
            .keys()
            .map(String::as_str)
            .filter(|member| *member != "userAgent")
            .collect();
        assert_eq!(
            declared,
            vec!["sourceIp"],
            "case {:?}: identity must carry no member the shape does not declare, got {identity:?}",
            row.case
        );
    }
}
