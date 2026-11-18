// SPDX-License-Identifier: CC0-1.0

//! WHOSE media an announced ingest endpoint takes, and what a producer's signature has to
//! declare before any of it is taken.
//!
//! Two rules, both about admitting a `POST /putMedia` at the address
//! `GetDataEndpoint(APIName=PUT_MEDIA)` handed the producer:
//!
//! 1. the address belongs to the STREAM it was discovered for — "Gets an endpoint for a
//!    specified stream for either reading or writing", with the stream named in the
//!    discovery call's own input — so a session naming any other stream, whether that
//!    stream is another active stream of the account or a name that exists nowhere, asked
//!    an endpoint for a stream it does not serve: refused HTTP 404
//!    `ResourceNotFoundException` with the sentence this family states for a stream that
//!    is not there to serve, and nothing persisted to either stream;
//! 2. the payload hash the producer's signature covers has to be DECLARED in
//!    `x-amz-content-sha256`, because the ingest front end is handed a stream and never
//!    buffers a body to hash: a session that declares none carries a signature over
//!    nothing that can be checked and is refused HTTP 403 `InvalidSignatureException`,
//!    again with nothing persisted.
//!
//! Each table spans what its rule generalises over: rule 1 over which stream the session
//! names and in which identifier form (the host's own by name and by ARN, another active
//! stream by name and by ARN, a name and an ARN that name nothing); rule 2 over what the
//! request declares as its payload hash (the unsigned-payload marker, a hex digest of the
//! body, no header at all, an empty one) and over whether the refusal comes before the
//! stream is resolved at all.

use crate::ingest_plane_addressing::{event_types, status, text, Harness};
use crate::put_media_ingest::{cluster, ingest_headers, Container, IngestHeaders};

/// The address the discovery calls in this suite are made at: a client on the machine this
/// emulator runs on reaches it at a loopback address.
const CONTROL: &str = "127.0.0.1:9297";

/// The header a producer declares the payload hash its signature covers in.
const CONTENT_SHA256: &str = "x-amz-content-sha256";

/// The one sentence this family states for a stream that is not there to serve, written
/// out as the wire carries it rather than read off the code under test — the clause "not
/// found or not active" is the fixed text of the refusal.
const STREAM_NOT_FOUND: &str = "The requested stream is not found or not active.";

/// The producer clock every session in this suite is stamped from: a whole second, well
/// behind this machine's clock, so no row is refused for a timestamp that has not happened.
fn producer_start() -> std::time::SystemTime {
    std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_millis(1_761_000_000_000)
}

/// One two-Cluster container, as a producer submits one session's media.
fn container(marker: u8) -> Vec<u8> {
    Container::of(vec![cluster(0, 2, 20, marker), cluster(1000, 2, 20, marker ^ 0x11)]).bytes()
}

/// The error code a refusal states, from the field a Kinesis Video media API states one in.
fn error_code(response: &http::Response<bytes::Bytes>) -> String {
    response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string()
}

/// The `Message` member a refusal carries, or the empty string when it carries none.
fn message(response: &http::Response<bytes::Bytes>) -> String {
    serde_json::from_slice::<serde_json::Value>(response.body())
        .ok()
        .and_then(|body| {
            body.get("Message")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_default()
}

/// `headers` with the declared payload hash replaced by `declared`, or removed when it is
/// `None`.
fn declaring(headers: &IngestHeaders, declared: Option<&str>) -> IngestHeaders {
    let mut headers: IngestHeaders = headers
        .iter()
        .filter(|(name, _)| *name != CONTENT_SHA256)
        .cloned()
        .collect();
    if let Some(declared) = declared {
        headers.push((CONTENT_SHA256, declared.to_string()));
    }
    headers
}

/// How a row addresses the stream it submits media for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Named {
    /// The stream the endpoint was discovered for, by `StreamName`.
    OwnStreamByName,
    /// The same stream, by `StreamARN` — the other identifier form of one resource.
    OwnStreamByArn,
    /// Another stream of this account, active and readable, by `StreamName`.
    OtherStreamByName,
    /// That same other stream, by `StreamARN`.
    OtherStreamByArn,
    /// A `StreamName` no stream of this account answers to.
    AbsentName,
    /// A well-formed `StreamARN` no stream of this account answers to.
    AbsentArn,
}

/// One row of rule 1: which stream the session names, and whether this endpoint serves it.
#[derive(Debug, Clone, Copy)]
struct Row {
    case: &'static str,
    named: Named,
    served: bool,
}

/// The rows, differing along which stream the identical session named and in which form.
const ROWS: &[Row] = &[
    Row {
        case: "the endpoint's own stream, by name",
        named: Named::OwnStreamByName,
        served: true,
    },
    Row {
        case: "the endpoint's own stream, by ARN",
        named: Named::OwnStreamByArn,
        served: true,
    },
    Row {
        case: "another active stream of the account, by name",
        named: Named::OtherStreamByName,
        served: false,
    },
    Row {
        case: "another active stream of the account, by ARN",
        named: Named::OtherStreamByArn,
        served: false,
    },
    Row {
        case: "a stream name that exists nowhere",
        named: Named::AbsentName,
        served: false,
    },
    Row {
        case: "a well-formed stream ARN that exists nowhere",
        named: Named::AbsentArn,
        served: false,
    },
];

/// RULE: an announced ingest endpoint serves the ONE stream it was discovered for — a
/// session naming any other stream is refused 404 `ResourceNotFoundException` with the
/// sentence this family states for a stream that is not there to serve, and persists
/// nothing to either stream, while the endpoint keeps serving its own stream in either
/// identifier form.
#[tokio::test]
async fn an_ingest_endpoint_serves_only_the_stream_it_was_discovered_for() {
    let harness = Harness::new();
    let own = "scoped-own";
    let other = "scoped-other";
    let own_arn = harness.create_stream(CONTROL, own).await;
    let other_arn = harness.create_stream(CONTROL, other).await;
    let own_host = harness.announced_authority(CONTROL, own, "PUT_MEDIA").await;
    let other_host = harness.announced_authority(CONTROL, other, "PUT_MEDIA").await;
    assert_ne!(
        own_host, other_host,
        "two streams must each be announced their own ingest endpoint, got {own_host:?} \
         for both"
    );
    // An ARN of the same shape as a real one, for a stream this account never created.
    let absent_arn = own_arn.replace(own, "scoped-never-created");

    let mut own_archived = 0usize;
    for (index, row) in ROWS.iter().enumerate() {
        let Row { case, named, .. } = *row;
        let addressing = match named {
            Named::OwnStreamByName => ("x-amzn-stream-name", own.to_string()),
            Named::OwnStreamByArn => ("x-amzn-stream-arn", own_arn.clone()),
            Named::OtherStreamByName => ("x-amzn-stream-name", other.to_string()),
            Named::OtherStreamByArn => ("x-amzn-stream-arn", other_arn.clone()),
            Named::AbsentName => ("x-amzn-stream-name", "scoped-never-created".to_string()),
            Named::AbsentArn => ("x-amzn-stream-arn", absent_arn.clone()),
        };
        // The identical session, differing only in the stream it names.
        let headers = ingest_headers(addressing, "RELATIVE", producer_start());
        let response = harness
            .put_media_at(&own_host, &headers, container(0x40 + index as u8))
            .await;

        if row.served {
            assert_eq!(
                status(&response),
                200,
                "case {case:?}: the endpoint must serve its own stream, got {} {}",
                status(&response),
                text(&response)
            );
            assert_eq!(
                event_types(&response),
                vec![
                    "BUFFERING",
                    "RECEIVED",
                    "BUFFERING",
                    "RECEIVED",
                    "PERSISTED",
                    "PERSISTED"
                ],
                "case {case:?}: expected the acknowledgement stream of two accepted \
                 Clusters, got {}",
                text(&response)
            );
            own_archived += 2;
        } else {
            assert_eq!(
                status(&response),
                404,
                "case {case:?}: a stream this endpoint does not serve must be refused, \
                 got {} {}",
                status(&response),
                text(&response)
            );
            assert_eq!(
                error_code(&response),
                "ResourceNotFoundException",
                "case {case:?}: expected the missing-stream class, got {:?} {}",
                error_code(&response),
                text(&response)
            );
            assert_eq!(
                message(&response),
                STREAM_NOT_FOUND,
                "case {case:?}: expected the family's own sentence, got {:?}",
                message(&response)
            );
            assert!(
                !text(&response).contains(other) && !text(&response).contains("scoped-never"),
                "case {case:?}: the refusal states no stream from the request, got {}",
                text(&response)
            );
        }

        // Nothing the refused rows submitted reaches either stream's archive: the host's
        // own stream holds exactly what the served rows ingested, and the stream the
        // refused rows named holds nothing at all.
        let archived = harness.archived_fragments(CONTROL, own).await;
        assert_eq!(
            archived, own_archived,
            "case {case:?}: the endpoint's own stream must hold only what it accepted, \
             got {archived} fragments"
        );
        let other_archived = harness.archived_fragments(CONTROL, other).await;
        assert_eq!(
            other_archived, 0,
            "case {case:?}: a session refused on another stream's endpoint must persist \
             nothing to the stream it named, got {other_archived} fragments"
        );
    }

    // And the other stream's OWN endpoint takes its media: the refusals above are about
    // which endpoint was addressed, not about the stream.
    let accepted = harness
        .put_media_at(
            &other_host,
            &ingest_headers(
                ("x-amzn-stream-name", other.to_string()),
                "RELATIVE",
                producer_start(),
            ),
            container(0x80),
        )
        .await;
    assert_eq!(
        status(&accepted),
        200,
        "case \"the other stream on its own endpoint\": expected the ingest to be served, \
         got {} {}",
        status(&accepted),
        text(&accepted)
    );
    let other_archived = harness.archived_fragments(CONTROL, other).await;
    assert_eq!(
        other_archived, 2,
        "case \"the other stream on its own endpoint\": expected its own session to be \
         archived, got {other_archived} fragments"
    );
}

/// One row of rule 2: what the session declared as the payload hash its signature covers.
#[derive(Debug, Clone, Copy)]
struct SignatureRow {
    case: &'static str,
    /// The `x-amz-content-sha256` the request carries, or `None` for a request that
    /// carries none.
    declared: Option<&'static str>,
    /// Whether the session is admitted and its media taken.
    admitted: bool,
}

/// The rows, differing along what the request declares as its payload hash.
const SIGNATURE_ROWS: &[SignatureRow] = &[
    SignatureRow {
        case: "the unsigned-payload marker a producer sends for a stream",
        declared: Some("UNSIGNED-PAYLOAD"),
        admitted: true,
    },
    SignatureRow {
        case: "a declared hex digest",
        // A producer that hashed what it is sending declares the digest instead; the
        // signature still covers a declared payload hash, which is what the front end
        // verifies against, so the session is taken.
        declared: Some("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"),
        admitted: true,
    },
    SignatureRow {
        case: "no declaration at all",
        declared: None,
        admitted: false,
    },
    SignatureRow {
        case: "an empty declaration",
        declared: Some(""),
        admitted: false,
    },
];

/// RULE: a session whose signature covers no declared payload hash is refused 403
/// `InvalidSignatureException` with nothing persisted, while the same bytes declaring one
/// are taken.
#[tokio::test]
async fn an_ingest_session_declaring_no_payload_hash_is_refused_and_persists_nothing() {
    let harness = Harness::new();
    let name = "signed-probe";
    harness.create_stream(CONTROL, name).await;
    let host = harness.announced_authority(CONTROL, name, "PUT_MEDIA").await;
    let signed = ingest_headers(
        ("x-amzn-stream-name", name.to_string()),
        "RELATIVE",
        producer_start(),
    );

    let mut archived = 0usize;
    for (index, row) in SIGNATURE_ROWS.iter().enumerate() {
        let SignatureRow {
            case,
            declared,
            admitted,
        } = *row;
        // The identical bytes, differing only in what the request declares its signature
        // covers.
        let headers = declaring(&signed, declared);
        let response = harness
            .put_media_at(&host, &headers, container(0x20 + index as u8))
            .await;

        if admitted {
            assert_eq!(
                status(&response),
                200,
                "case {case:?}: a declared payload hash is verifiable, so the session is \
                 taken; got {} {}",
                status(&response),
                text(&response)
            );
            archived += 2;
        } else {
            assert_eq!(
                status(&response),
                403,
                "case {case:?}: a signature covering no declared payload hash must be \
                 refused, got {} {}",
                status(&response),
                text(&response)
            );
            assert_eq!(
                error_code(&response),
                "InvalidSignatureException",
                "case {case:?}: expected the signature-failure class, got {:?} {}",
                error_code(&response),
                text(&response)
            );
            assert!(
                !text(&response).contains(name) && !text(&response).contains(CONTENT_SHA256),
                "case {case:?}: the refusal carries no value from the request, got {}",
                text(&response)
            );
        }

        let held = harness.archived_fragments(CONTROL, name).await;
        assert_eq!(
            held, archived,
            "case {case:?}: only an admitted session may persist fragments, got {held}"
        );
    }
}

/// RULE: the signature is settled BEFORE anything the request says is read, so a session
/// that declares no payload hash is refused as the signature failure it is even when the
/// stream it names is one this endpoint would refuse — and the store is untouched either
/// way.
#[tokio::test]
async fn the_signature_is_settled_before_the_stream_the_session_names_is_resolved() {
    let harness = Harness::new();
    let own = "ordered-own";
    let other = "ordered-other";
    harness.create_stream(CONTROL, own).await;
    harness.create_stream(CONTROL, other).await;
    let host = harness.announced_authority(CONTROL, own, "PUT_MEDIA").await;

    // (case, the stream the session names) — each one this endpoint does not serve, so
    // the stream-level refusal is the one that would fire if the signature were not
    // settled first.
    let cases: &[(&str, &str)] = &[
        ("another active stream of the account", other),
        ("a stream name that exists nowhere", "ordered-never-created"),
    ];
    for (case, named) in cases {
        let headers = declaring(
            &ingest_headers(
                ("x-amzn-stream-name", (*named).to_string()),
                "RELATIVE",
                producer_start(),
            ),
            None,
        );
        let response = harness.put_media_at(&host, &headers, container(0x60)).await;
        assert_eq!(
            status(&response),
            403,
            "case {case:?}: the unverifiable signature is the refusal, got {} {}",
            status(&response),
            text(&response)
        );
        assert_eq!(
            error_code(&response),
            "InvalidSignatureException",
            "case {case:?}: expected the signature-failure class, got {:?} {}",
            error_code(&response),
            text(&response)
        );
    }

    for stream in [own, other] {
        let archived = harness.archived_fragments(CONTROL, stream).await;
        assert_eq!(
            archived, 0,
            "case {stream:?}: a refused session persists nothing, got {archived} fragments"
        );
    }
}
