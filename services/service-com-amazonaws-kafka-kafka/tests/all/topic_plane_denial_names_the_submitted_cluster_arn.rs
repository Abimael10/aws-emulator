// SPDX-License-Identifier: CC0-1.0

//! What a client reads when a Topic Management call names a cluster reference that
//! identifies no cluster, driven end to end over the same `MultiServiceRouter` the
//! emulator serves from (the shared `kafka_harness`).
//!
//! RULE under test: a topic call reaches a cluster's Apache Kafka plane, and it
//! names that cluster in one non-greedy `@httpLabel` path segment
//! (`smithy_query(query="operation",operation="DescribeTopicPartitions")`:
//! `ClusterArn` is an `@httpLabel`), so the value the data-plane authorization
//! decision is taken from is the segment AS THE URI CARRIED IT — percent-encoded
//! over RFC 3986's unreserved set. A reference that identifies no cluster reaches
//! no grant, and the refusal quotes exactly that: `User: <caller> is not authorized
//! to perform: kafka-cluster:Connect on resource: <the percent-encoded ClusterArn>`
//! and NOTHING after it, no document having been reached to be named — so a client
//! recovers the offending reference by percent-decoding the resource clause.
//!
//! WHICH references the gate refuses, over all six operations, is
//! `topic_cluster_arn_authorization`, whose table this suite reuses; the rows here
//! differ along the other half of the rule — which bytes of the submitted value the
//! encoding has to act on: an ARN's own `:` and `/`, the unreserved characters that
//! must stay literal, a space, a literal `%`, a `+`, and non-ASCII text whose UTF-8
//! bytes are escaped one by one.

use crate::kafka_harness::{http_label, Emulator, ACCOUNT, REGION};
use crate::topic_cluster_arn_authorization::{
    make, not_authorized, references_naming_no_cluster, topic_calls, ACCESS_DENIED_CODE,
    ACCESS_DENIED_STATUS, CLUSTER_CONNECT_ACTION,
};

/// Cluster REFERENCES of this service that identify no cluster, each carrying
/// different bytes for the encoding to act on: the shared table of what makes a
/// reference identify none, plus the rows this suite adds for the encoding itself.
fn references_to_encode() -> Vec<(&'static str, String)> {
    let mut rows = references_naming_no_cluster();
    rows.extend([
        (
            // Only the unreserved characters survive encoding, so a name made of
            // them is quoted literally while the ARN's own punctuation is not.
            "a name of unreserved characters only",
            format!("arn:aws:kafka:{REGION}:{ACCOUNT}:cluster/a-b.c_d~e"),
        ),
        (
            "a name carrying a space",
            format!("arn:aws:kafka:{REGION}:{ACCOUNT}:cluster/a cluster"),
        ),
        (
            // A value a client that decodes the clause would corrupt if the resource
            // were quoted raw.
            "a name carrying a literal percent and plus",
            format!("arn:aws:kafka:{REGION}:{ACCOUNT}:cluster/100%+more"),
        ),
        (
            // Katakana and an accented Latin letter: each UTF-8 byte is encoded.
            "a non-ASCII name",
            format!("arn:aws:kafka:{REGION}:{ACCOUNT}:cluster/\u{30af}\u{30e9}-caf\u{e9}"),
        ),
    ]);
    rows
}

/// RULE: every topic call carrying a cluster reference that identifies no cluster
/// is refused by authorization with the one sentence, which names the submitted
/// `ClusterArn` as the URI carried it and ends there — whichever operation answers
/// and whatever bytes the value holds.
#[tokio::test]
async fn the_denial_quotes_the_submitted_cluster_arn_as_the_uri_carried_it() {
    let emulator = Emulator::new();

    for (case, value) in references_to_encode() {
        let quoted = http_label(&value);
        let expected = not_authorized(&quoted);
        for call in topic_calls() {
            let reply = make(&emulator, &call, &value).await;
            assert_eq!(
                (reply.status, reply.error_code(), reply.error_message()),
                (ACCESS_DENIED_STATUS, ACCESS_DENIED_CODE, expected.as_str()),
                "case {case:?}/{}: expected the authorization refusal {expected:?}, got {} {}",
                call.name,
                reply.status,
                reply.body
            );
            // The sentence ends at the resource: no clause names a document,
            // because no document was reached to withhold the grant.
            assert!(
                reply.error_message().ends_with(&quoted),
                "case {case:?}/{}: nothing may follow the resource, got {:?}",
                call.name,
                reply.error_message()
            );
            // And the resource clause really is encoded: a value needing encoding
            // never appears raw.
            if quoted != value {
                assert!(
                    !reply.error_message().contains(value.as_str()),
                    "case {case:?}/{}: expected the resource percent-encoded, got {:?}",
                    call.name,
                    reply.error_message()
                );
            }
        }
    }
}

/// RULE: percent-decoding the resource clause returns the submitted `ClusterArn`
/// byte for byte — which is the property a client that recovers the offending
/// reference from the denial depends on.
#[tokio::test]
async fn the_resource_clause_decodes_back_to_the_submitted_value() {
    let emulator = Emulator::new();
    let prefix = not_authorized("");
    let partitions = topic_calls()
        .into_iter()
        .find(|call| call.name == "DescribeTopicPartitions")
        .expect("the surface has a DescribeTopicPartitions");

    for (case, value) in references_to_encode() {
        let reply = make(&emulator, &partitions, &value).await;
        let message = reply.error_message().to_string();
        let quoted = message.strip_prefix(&prefix).unwrap_or_else(|| {
            panic!("case {case:?}: expected the refusal to open with {prefix:?}, got {message:?}")
        });
        let decoded = percent_decode(quoted);
        assert_eq!(
            decoded,
            Some(value.clone()),
            "case {case:?}: expected the resource clause {quoted:?} to decode to the submitted \
             value, got {decoded:?}"
        );
    }
    assert!(
        prefix.contains(CLUSTER_CONNECT_ACTION),
        "the sentence read back must be the data plane's, got {prefix:?}"
    );
}

/// `text` with its `%XX` escapes undone, or `None` when it is not a
/// percent-encoded byte string — the inverse a client applies to the resource
/// clause.
fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut decoded: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = text.get(index + 1..index + 3)?;
            decoded.push(u8::from_str_radix(hex, 16).ok()?);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).ok()
}
