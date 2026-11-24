// SPDX-License-Identifier: CC0-1.0

//! WHOSE media an announced ingest address takes, asked of every (address, stream) pair
//! two streams of one account make.
//!
//! The rule under test, one level above the single session that showed it:
//!
//! > An announced ingest address belongs to the ONE stream its discovery call named —
//! > `GetDataEndpoint` takes `StreamName`/`StreamARN` so that "an endpoint for a specified
//! > stream" is what it hands back — so that stream's identifiers are the only ones that
//! > name a stream there. A `POST /putMedia` whose `x-amzn-stream-name` names another
//! > stream of the account resolves to no stream that address serves and is refused as the
//! > unresolvable stream it is: HTTP 404, `ResourceNotFoundException` in the error type
//! > field, and an error acknowledgement carrying the one fixed sentence this family states
//! > for a stream that is not there to serve — with nothing persisted to either stream.
//!
//! The rows are every pairing of two announced addresses with two streams, so the answer
//! is shown to belong to the PAIR rather than to one of the two streams: each address
//! takes its own stream's media and refuses the other's, in both directions, and each
//! stream's archive is counted after every row. Which identifier forms, and which ways an
//! identifier can fail to name a stream here, the refusal covers is the sibling suite
//! [`crate::ingest_stream_scoping`]'s first table.

use crate::ingest_plane_addressing::{error_type, member, status, text, Harness};
use crate::put_media_ingest::{cluster, ingest_headers, Container};

/// The address the discovery calls in this suite are made at: a client on the machine this
/// emulator runs on reaches the front door at a loopback address.
const CONTROL: &str = "127.0.0.1:9297";

/// The one sentence this family states for a stream that is not there to serve, written
/// out as the wire carries it rather than read off the code under test — "not found or not
/// active" is the fixed clause of this refusal.
const STREAM_NOT_FOUND: &str = "The requested stream is not found or not active.";

/// The producer clock every session in this suite is stamped from: a whole second, well
/// behind this machine's clock, so no row is refused for a timestamp that has not happened.
fn producer_start() -> std::time::SystemTime {
    std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_millis(1_761_000_000_000)
}

/// One two-Cluster container, as a producer submits one session's media.
fn container(marker: u8) -> Vec<u8> {
    Container::of(vec![cluster(0, 2, 20, marker), cluster(1000, 2, 20, marker ^ 0x33)]).bytes()
}

/// RULE: the answer belongs to the (address, stream) PAIR — each announced ingest address
/// takes the media of the stream it was discovered for and refuses every other stream's
/// with the unresolvable-stream class, in both directions.
#[tokio::test]
async fn each_ingest_address_takes_its_own_stream_and_refuses_the_others_both_ways() {
    let harness = Harness::new();
    let first = "paired-first";
    let second = "paired-second";
    harness.create_stream(CONTROL, first).await;
    harness.create_stream(CONTROL, second).await;
    let first_host = harness
        .announced_authority(CONTROL, first, "PUT_MEDIA")
        .await;
    let second_host = harness
        .announced_authority(CONTROL, second, "PUT_MEDIA")
        .await;
    assert_ne!(
        first_host, second_host,
        "setup: each stream must be announced its own ingest address, got {first_host:?} \
         for both"
    );

    // (case, the address the session was sent to, the stream it named, whether that
    // address serves it) — every pairing of the two addresses with the two streams.
    let rows: &[(&str, &str, &str, bool)] = &[
        (
            "the first stream at its own address",
            &first_host,
            first,
            true,
        ),
        (
            "the second stream at the first's address",
            &first_host,
            second,
            false,
        ),
        (
            "the second stream at its own address",
            &second_host,
            second,
            true,
        ),
        (
            "the first stream at the second's address",
            &second_host,
            first,
            false,
        ),
    ];

    let mut archived = std::collections::HashMap::from([(first, 0usize), (second, 0usize)]);
    for (index, (case, host, named, served)) in rows.iter().enumerate() {
        // The identical session, differing only in the address it reached and the stream
        // it named.
        let response = harness
            .put_media_at(
                host,
                &ingest_headers(
                    ("x-amzn-stream-name", (*named).to_string()),
                    "RELATIVE",
                    producer_start(),
                ),
                container(0xA0 + index as u8),
            )
            .await;
        if *served {
            assert_eq!(
                status(&response),
                200,
                "case {case:?}: expected the ingest to be served, got {} {}",
                status(&response),
                text(&response)
            );
            *archived.get_mut(named).expect("both streams are tracked") += 2;
        } else {
            assert_eq!(
                status(&response),
                404,
                "case {case:?}: expected the unresolvable-stream refusal (HTTP 404), got {} {}",
                status(&response),
                text(&response)
            );
            assert_eq!(
                error_type(&response),
                "ResourceNotFoundException",
                "case {case:?}: expected ResourceNotFoundException, got {:?} {}",
                error_type(&response),
                text(&response)
            );
            assert_eq!(
                member(&response, "Message"),
                STREAM_NOT_FOUND,
                "case {case:?}: expected the one fixed sentence {STREAM_NOT_FOUND:?}, got {:?}",
                member(&response, "Message")
            );
            assert!(
                !text(&response).contains(*named),
                "case {case:?}: the refusal states no stream from the request, got {}",
                text(&response)
            );
        }

        for stream in [first, second] {
            let held = harness.archived_fragments(CONTROL, stream).await;
            let expected = archived[stream];
            assert_eq!(
                held, expected,
                "case {case:?}: {stream:?} must hold only what its own address accepted, \
                 expected {expected}, got {held}"
            );
        }
    }
}
