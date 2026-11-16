// SPDX-License-Identifier: CC0-1.0

//! The form the `streamIdentifier` label has to be in, on the two operations that
//! take one.
//!
//! RULE under test: `GetStream` and `DeleteStream` address a stream by a
//! `streamIdentifier` `@httpLabel` targeting `StreamId`
//! (`smithy_query(query="operation", operation="GetStream,DeleteStream")`), and a
//! stream id is a base32-encoded 26-character string — 26 symbols of the alphabet
//! this service renders an identifier in, which leaves out `i` and `l` (they read
//! as `1`), `o` (it reads as `0`) and `u` altogether. `StreamId`'s `@pattern`
//! (`^[a-z0-9]{26}$`) cannot express that alphabet, so a value can satisfy the
//! pattern and still be no stream id: such a value names no stream that could
//! exist — `StreamArn` runs over the same encoded id
//! (`smithy_query(query="shape", shape="StreamArn")`), so it could not appear in
//! one — and both operations refuse it as the modelled `ValidationException` (HTTP
//! 400, the `ValidationException` wire code, its `@required` `reason` filled, its
//! `fieldList` naming the member and Aurora DSQL's sentence for a member whose
//! value it cannot read), BEFORE any lookup and before `DeleteStream` consults its
//! `clientToken` ledger. A value the encoding DOES spell is a stream id, resolved
//! against the store like any other, so one the cluster does not hold is the
//! modelled 404.
//!
//! The rows span how a value inside the pattern can fall outside the encoding
//! (the table `tests/all/identifier_pattern_is_the_whole_form.rs` owns, which pins
//! the same class of value over every other identifier-carrying route), each run
//! against a cluster that is live, one the account does not hold and one that is
//! itself outside the encoding — so the refusal is shown to be decided on the
//! value rather than on what the store holds.
//!
//! Every case drives the real request path through the `Dsql` harness in
//! `tests/all/harness.rs`, this crate's one test client.

use std::time::{Duration, Instant};

use serde_json::Value;

use crate::harness::{absent_identifier, expected_stream_arn, Dsql};
use crate::identifier_pattern_is_the_whole_form::values_outside_the_encoding;
use crate::resource_identifier_absence::{
    expected_unreadable_sentence, stream_request, the_store_is_untouched, IdCase, IDENTIFIER_LEN,
    IDENTIFIER_SYMBOLS, LEADING_SYMBOL_MAX,
};

/// The member a stream identifier arrives in, as both operations bind it
/// (`smithy_query(query="operation", operation="GetStream,DeleteStream")`:
/// `streamIdentifier`).
const STREAM_MEMBER: &str = "streamIdentifier";

/// Look again until the stream `cluster` holds reports `ACTIVE`, the status a
/// delete of it accepts, or a generous deadline passes: the clock, not a caller,
/// moves a stream out of `CREATING`, so this waits for the CONDITION rather than
/// for a span of time.
async fn stream_reports_active(dsql: &Dsql, cluster: &str, stream: &str) {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let read = dsql.get_stream(cluster, stream).await;
        let status = read.str("status");
        if status == "ACTIVE" {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {stream:?}: the stream never reported ACTIVE, last status {status:?} \
             in {:?}",
            read.body
        );
        ::tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// RULE, over both operations and over every value of the class: a
/// `streamIdentifier` the encoding does not spell is refused with the modelled
/// `ValidationException` — 400, the wire code, the `@required` `reason`, the
/// `fieldList` naming the member, and the sentence that names the member and the
/// format the value had to be in while echoing nothing the caller sent.
#[tokio::test]
async fn a_stream_identifier_outside_the_encoding_is_refused_as_input() {
    let dsql = Dsql::new();
    let live = dsql.create_settled().await;
    let expected = expected_unreadable_sentence(STREAM_MEMBER);

    for case in values_outside_the_encoding() {
        for (operation, answer) in [
            ("GetStream", dsql.get_stream(&live, &case.value).await),
            (
                "DeleteStream",
                dsql.delete_stream(&live, &case.value, "").await,
            ),
        ] {
            let name = format!("{} on {operation}", case.name);
            assert_eq!(
                answer.status, 400,
                "case {name}: a streamIdentifier outside the encoding must answer 400, \
                 got {} {:?}",
                answer.status, answer.body
            );
            assert_eq!(
                answer.error.as_deref(),
                Some("ValidationException"),
                "case {name}: expected ValidationException, got {:?}",
                answer.error
            );
            assert_eq!(
                answer.message(),
                expected,
                "case {name}: expected {expected:?}, got {:?}",
                answer.message()
            );
            assert!(
                !answer.str("reason").is_empty(),
                "case {name}: ValidationException models reason as @required, got {:?}",
                answer.body
            );
            let field_list = answer.get("fieldList");
            let fields = field_list.as_array().map(Vec::as_slice).unwrap_or_default();
            assert!(
                fields
                    .iter()
                    .any(|field| field.get("name").and_then(Value::as_str) == Some(STREAM_MEMBER)),
                "case {name}: the refusal must name {STREAM_MEMBER:?} in fieldList, \
                 got {field_list:?}"
            );
            // A refusal of the request names no resource, and reports no ARN: the
            // value could not appear in a StreamArn at all, so neither the ARN a
            // not-found answer would name nor the value itself belongs in it.
            let fabricated = expected_stream_arn(dsql.region(), &live, &case.value);
            assert_eq!(
                answer.get("resourceId"),
                Value::Null,
                "case {name}: a refusal of the request names no resource, got {:?}",
                answer.body
            );
            assert!(
                !answer.message().contains(&fabricated) && !answer.message().contains(&case.value),
                "case {name}: the refusal must not report an ARN that could never be \
                 minted, got {:?}",
                answer.message()
            );
        }
    }

    the_store_is_untouched(&dsql, &live).await;
}

/// RULE, on the ORDER the label's form is decided in: the value's form is decided
/// before anything is looked up, so the refusal does not depend on what the store
/// holds — the same malformed `streamIdentifier` answers the same 400 under a
/// cluster that is live, under a well-formed cluster the account does not hold and
/// under a cluster identifier that is itself outside the encoding — and
/// `DeleteStream` answers it the same way with a `clientToken`, which it would
/// otherwise consult its ledger for.
#[tokio::test]
async fn the_streams_label_is_decided_before_any_lookup() {
    let dsql = Dsql::new();
    let live = dsql.create_settled().await;

    let clusters = [
        ("a cluster that is live", live.clone()),
        ("a cluster the account does not hold", absent_identifier()),
        (
            "a cluster identifier outside the encoding too",
            "iiiiiiiiiiiiiiiiiiiiiiiiii".to_string(),
        ),
    ];
    let expected = expected_unreadable_sentence(STREAM_MEMBER);

    for case in values_outside_the_encoding() {
        for (cluster_name, cluster) in &clusters {
            for (operation, answer) in [
                ("GetStream", dsql.get_stream(cluster, &case.value).await),
                (
                    "DeleteStream",
                    dsql.delete_stream(cluster, &case.value, "").await,
                ),
                (
                    "DeleteStream carrying a client token",
                    dsql.delete_stream(cluster, &case.value, "?client-token=form-check")
                        .await,
                ),
            ] {
                let name = format!("{} under {cluster_name} on {operation}", case.name);
                assert_eq!(
                    answer.status, 400,
                    "case {name}: the refusal must not depend on what the store holds, \
                     got {} {:?}",
                    answer.status, answer.body
                );
                assert_eq!(
                    answer.error.as_deref(),
                    Some("ValidationException"),
                    "case {name}: expected ValidationException, got {:?}",
                    answer.error
                );
                assert_eq!(
                    answer.message(),
                    expected,
                    "case {name}: expected {expected:?}, got {:?}",
                    answer.message()
                );
            }
        }
    }

    the_store_is_untouched(&dsql, &live).await;
}

/// RULE, the other way round: the refusal is about the VALUE, so the stream that
/// exists is left exactly as it was — it still reads back through `GetStream`
/// while every value outside the encoding keeps answering 400, it is still the only
/// stream its cluster lists, and its own identifier, which the encoding does spell,
/// still resolves through `GetStream` and then through `DeleteStream`.
#[tokio::test]
async fn a_refused_stream_identifier_leaves_the_stream_that_exists_alone() {
    let dsql = Dsql::new();
    let live = dsql.create_settled().await;

    let created = dsql.create_stream(&live, stream_request()).await;
    assert_eq!(
        created.status, 200,
        "CreateStream must be served, got {} {:?}",
        created.status, created.body
    );
    let stream = created.str("streamIdentifier");

    for case in values_outside_the_encoding() {
        for (operation, answer) in [
            ("GetStream", dsql.get_stream(&live, &case.value).await),
            (
                "DeleteStream",
                dsql.delete_stream(&live, &case.value, "").await,
            ),
        ] {
            assert_eq!(
                answer.status,
                400,
                "case {:?} on {operation}: expected the 400 refusal, got {} {:?}",
                case.name,
                answer.status,
                answer.body
            );
        }

        let read = dsql.get_stream(&live, &stream).await;
        assert_eq!(
            read.status,
            200,
            "case {:?}: the minted stream must still be read back, got {} {:?}",
            case.name,
            read.status,
            read.body
        );
    }

    let listed = dsql.list_streams(&live, "").await.get("streams");
    let streams = listed.as_array().map(Vec::as_slice).unwrap_or_default();
    assert_eq!(
        streams.len(),
        1,
        "the refused calls must neither create nor remove a stream, got {listed:?}"
    );
    assert_eq!(
        streams
            .first()
            .and_then(|entry| entry.get("streamIdentifier"))
            .and_then(Value::as_str),
        Some(stream.as_str()),
        "the cluster must still list the stream it holds, got {listed:?}"
    );

    // The complement, through the mutating operation too: the stream's own
    // identifier is of the encoded form, so it is resolved rather than refused —
    // once the clock has moved the stream into the status a delete accepts.
    stream_reports_active(&dsql, &live, &stream).await;
    let deleted = dsql.delete_stream(&live, &stream, "").await;
    assert_eq!(
        deleted.status, 200,
        "the stream's own identifier must be served, got {} {:?}",
        deleted.status, deleted.body
    );
}

/// RULE, the complement over the whole alphabet: every value the encoding DOES
/// spell is a stream id, so both operations resolve it against the store — a
/// cluster that holds no such stream answers the modelled 404 with the stream ARN
/// it addressed, including for values above the range this service mints from,
/// which are base32-encoded 26-character strings all the same.
#[tokio::test]
async fn a_stream_identifier_the_encoding_spells_is_resolved_against_the_store() {
    let dsql = Dsql::new();
    let live = dsql.create_settled().await;

    let mut cases = vec![
        IdCase {
            name: "the smallest value the encoding renders".to_string(),
            value: "0".repeat(IDENTIFIER_LEN),
        },
        IdCase {
            name: "the largest value inside the minted range".to_string(),
            value: format!("{LEADING_SYMBOL_MAX}{}", "z".repeat(IDENTIFIER_LEN - 1)),
        },
        IdCase {
            name: "the smallest value above the minted range".to_string(),
            value: format!("8{}", "0".repeat(IDENTIFIER_LEN - 1)),
        },
        IdCase {
            name: "the largest value the encoding spells".to_string(),
            value: "z".repeat(IDENTIFIER_LEN),
        },
    ];
    // Each symbol of the encoding, in the body of the value: the complement is
    // about the alphabet rather than about which of its symbols a value uses.
    for symbol in IDENTIFIER_SYMBOLS.chars() {
        let mut value: Vec<char> = "0".repeat(IDENTIFIER_LEN).chars().collect();
        value[IDENTIFIER_LEN / 2] = symbol;
        cases.push(IdCase {
            name: format!("{symbol:?} in the body of the value"),
            value: value.into_iter().collect(),
        });
    }

    for case in &cases {
        let arn = expected_stream_arn(dsql.region(), &live, &case.value);
        for (operation, answer) in [
            ("GetStream", dsql.get_stream(&live, &case.value).await),
            (
                "DeleteStream",
                dsql.delete_stream(&live, &case.value, "").await,
            ),
        ] {
            let name = format!("{} on {operation}", case.name);
            assert_eq!(
                answer.status, 404,
                "case {name}: a stream id the cluster does not hold must answer 404, \
                 got {} {:?}",
                answer.status, answer.body
            );
            assert_eq!(
                answer.error.as_deref(),
                Some("ResourceNotFoundException"),
                "case {name}: expected ResourceNotFoundException, got {:?}",
                answer.error
            );
            assert_eq!(
                answer.message(),
                format!("The resource with ARN {arn} doesn't exist. Verify the ARN and try again."),
                "case {name}: expected the sentence naming {arn:?}, got {:?}",
                answer.message()
            );
            assert_eq!(
                answer.str("resourceId"),
                case.value,
                "case {name}: resourceId is @required and is the identifier the request \
                 carried, got {:?}",
                answer.body
            );
            assert_eq!(
                answer.str("resourceType"),
                "stream",
                "case {name}: resourceType is @required and is the addressed resource's \
                 own type, got {:?}",
                answer.body
            );
        }
    }

    the_store_is_untouched(&dsql, &live).await;
}
