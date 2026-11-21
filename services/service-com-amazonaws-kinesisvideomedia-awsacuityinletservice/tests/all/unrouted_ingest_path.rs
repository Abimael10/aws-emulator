// SPDX-License-Identifier: CC0-1.0

//! WHAT an address that binds no route for the ingest path answers a producer, and what
//! decides that answer.
//!
//! The rule under test: an endpoint of this family answers a request for a PATH it binds
//! no route for with HTTP 404 whose whole body is the bare element naming the
//! unknown-operation class, with no `x-amzn-errortype` header and nothing of the request
//! in it — and the answer is settled from the ADDRESS the request reached, ahead of
//! reading anything the request carries, so every session a producer could send earns the
//! identical refusal there and nothing is ingested. `POST /putMedia` is bound at the
//! origin `GetDataEndpoint(APIName=PUT_MEDIA)` announces for a stream and nowhere else, so
//! the control-plane endpoint the discovery call itself arrived on, and an origin announced
//! for the ARCHIVE plane — this stream's or another stream's — each host no API at that
//! path at all.
//!
//! That refusal is NOT the front door's undeterminable-operation envelope (HTTP 400 with
//! `Output`/`Version`), which is what a request on a path this family DOES bind, and can
//! name no operation in, earns. The second test holds that boundary, so the two refusals
//! cannot be collapsed into one wording.
//!
//! The first table's rows differ in the SESSION the producer sent — a well-formed one and
//! a well-formed two-Cluster one, each addressing the stream in a different identifier
//! form, and one per refusal class this plane has of its own (no addressing header, a
//! stream that exists nowhere, no declared payload hash, a timecode type outside the
//! published value set, no media at all) — crossed with the ADDRESS it was sent to: this
//! emulator's control-plane front door by its loopback literal and by name, the archive
//! origin announced for this stream, and the archive origin announced for another stream.
//! Every row is then sent to the origin the ingest route IS bound at, where each earns its
//! OWN answer, which is what shows the refusal above belongs to the address.

use crate::control_plane_stream_reads::{error_code, json};
use crate::ingest_plane_addressing::{event_types, status, text, Harness};
use crate::put_media_ingest::{cluster, ingest_headers, Container, IngestHeaders};
use crate::read_plane_addressing::unknown_operation_envelope;

/// The address the discovery calls in this suite are made at: a client on the machine this
/// emulator runs on reaches its front door at a loopback address.
const CONTROL: &str = "127.0.0.1:9297";

/// The same front door, named rather than numbered — the other form a client on this
/// machine addresses it in.
const CONTROL_BY_NAME: &str = "localhost:9297";

/// The whole body an endpoint of this family answers a path it binds NO route for with,
/// written out as the wire carries it rather than read off the code under test, so a drift
/// away from it must fail here.
const UNROUTED_PATH_BODY: &str = "<UnknownOperationException/>";

/// A stream name no stream of this account answers to.
const ABSENT_STREAM: &str = "unrouted-never-created";

/// The header a producer names the stream to ingest into by name in.
const STREAM_NAME: &str = "x-amzn-stream-name";

/// The header a producer declares the payload hash its signature covers in.
const CONTENT_SHA256: &str = "x-amz-content-sha256";

/// The producer clock every session in this suite is stamped from: a whole second well
/// behind this machine's clock, so no row is refused for a fragment captured at an instant
/// that has not happened.
fn producer_start() -> std::time::SystemTime {
    std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_millis(1_761_000_000_000)
}

/// One container carrying one Cluster per `marker`, a second apart, each of two frames
/// whose bytes carry that marker.
fn container(markers: &[u8]) -> Vec<u8> {
    Container::of(
        markers
            .iter()
            .enumerate()
            .map(|(index, marker)| cluster(index as u64 * 1000, 2, 20, *marker))
            .collect(),
    )
    .bytes()
}

/// `headers` without the header `name`, for the rows whose point is a header a producer
/// failed to send.
fn without(headers: &IngestHeaders, name: &str) -> IngestHeaders {
    headers
        .iter()
        .filter(|(header, _)| *header != name)
        .cloned()
        .collect()
}

/// The acknowledgements a session's answer carries, in order: the body is a STREAM of
/// them, one JSON object per line, which is what a producer reads off this wire.
fn acknowledgements(response: &http::Response<bytes::Bytes>) -> Vec<serde_json::Value> {
    text(response)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .collect()
}

/// Which session a row sends. Each one past the two well-formed sessions is a request this
/// plane has a refusal of its OWN for, so a row that earned that refusal instead of the
/// unrouted-path answer would mean the request had been read at an address that routes it
/// nowhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Session {
    /// A well-formed one-Cluster session addressing the stream by name.
    WellFormedByName,
    /// A well-formed two-Cluster session addressing the same stream by its ARN.
    WellFormedByArn,
    /// A session carrying no addressing header at all.
    NoAddressingHeader,
    /// A session naming a stream no account of this emulator holds.
    AbsentStream,
    /// A session whose signature covers no declared payload hash.
    NoDeclaredPayloadHash,
    /// A session whose timecode type is outside the published value set.
    UnknownTimecodeType,
    /// A session carrying no media at all.
    NoMedia,
}

/// One row: the session sent, and what the SAME session earns at the origin the ingest
/// route is bound at.
#[derive(Debug, Clone, Copy)]
struct Row {
    case: &'static str,
    session: Session,
    /// The status it earns at the announced write origin.
    served_status: u16,
    /// The error class it is classified by there, where it earns one.
    served_code: Option<&'static str>,
    /// How many fragments it persists there.
    served_fragments: usize,
    /// The `(ErrorCode, ErrorId)` of the in-band fault its answer carries there, for a row
    /// whose fault is in the MEDIA rather than in the request.
    served_fault: Option<(&'static str, u64)>,
}

/// The rows, differing along the session the producer sent.
const ROWS: &[Row] = &[
    Row {
        case: "a well-formed one-Cluster session addressed by name",
        session: Session::WellFormedByName,
        served_status: 200,
        served_code: None,
        served_fragments: 1,
        served_fault: None,
    },
    Row {
        case: "a well-formed two-Cluster session addressed by ARN",
        session: Session::WellFormedByArn,
        served_status: 200,
        served_code: None,
        served_fragments: 2,
        served_fault: None,
    },
    Row {
        case: "a session carrying no addressing header",
        session: Session::NoAddressingHeader,
        served_status: 400,
        served_code: Some("InvalidArgumentException"),
        served_fragments: 0,
        served_fault: None,
    },
    Row {
        case: "a session naming a stream that exists nowhere",
        session: Session::AbsentStream,
        served_status: 404,
        served_code: Some("ResourceNotFoundException"),
        served_fragments: 0,
        served_fault: None,
    },
    Row {
        case: "a session declaring no payload hash",
        session: Session::NoDeclaredPayloadHash,
        served_status: 403,
        served_code: Some("InvalidSignatureException"),
        served_fragments: 0,
        served_fault: None,
    },
    Row {
        case: "a session whose timecode type is outside the value set",
        session: Session::UnknownTimecodeType,
        served_status: 400,
        served_code: Some("InvalidArgumentException"),
        served_fragments: 0,
        served_fault: None,
    },
    Row {
        case: "a session carrying no media at all",
        session: Session::NoMedia,
        // A container this plane cannot read is a fault in the MEDIA, which this family
        // reports in band inside the session's own answer rather than as an HTTP error:
        // the one acknowledgement the session carries is that fault, stating the class and
        // id of a container that does not read as Matroska and naming no fragment, since
        // none was taken in.
        served_status: 200,
        served_code: None,
        served_fragments: 0,
        served_fault: Some(("INVALID_MKV_DATA", 4006)),
    },
];

/// The headers and body of one row's session, naming the stream `name` / `arn`.
fn session_request(session: Session, name: &str, arn: &str) -> (IngestHeaders, Vec<u8>) {
    let by_name = (STREAM_NAME, name.to_string());
    match session {
        Session::WellFormedByName => (
            ingest_headers(by_name, "RELATIVE", producer_start()),
            container(&[0x31]),
        ),
        Session::WellFormedByArn => (
            ingest_headers(
                ("x-amzn-stream-arn", arn.to_string()),
                "RELATIVE",
                producer_start(),
            ),
            container(&[0x32, 0x33]),
        ),
        Session::NoAddressingHeader => (
            without(
                &ingest_headers(by_name, "RELATIVE", producer_start()),
                STREAM_NAME,
            ),
            container(&[0x34]),
        ),
        Session::AbsentStream => (
            ingest_headers(
                (STREAM_NAME, ABSENT_STREAM.to_string()),
                "RELATIVE",
                producer_start(),
            ),
            container(&[0x35]),
        ),
        Session::NoDeclaredPayloadHash => (
            without(
                &ingest_headers(by_name, "RELATIVE", producer_start()),
                CONTENT_SHA256,
            ),
            container(&[0x36]),
        ),
        Session::UnknownTimecodeType => (
            // A value outside the two the ingest plane publishes.
            ingest_headers(by_name, "SOMETIMES", producer_start()),
            container(&[0x37]),
        ),
        Session::NoMedia => (
            ingest_headers(by_name, "RELATIVE", producer_start()),
            Vec::new(),
        ),
    }
}

/// RULE: a request for a path an endpoint of this family binds NO route for is answered
/// HTTP 404 whose whole body is the bare unknown-operation element, with no error-type
/// header and nothing of the request in it, decided by the address alone — so every
/// session, including the ones this plane has refusals of its own for, earns that identical
/// answer at the control-plane endpoint and at an origin announced for the archive plane,
/// and ingests nothing, while each of them earns its OWN answer at the origin the ingest
/// route is bound at.
#[tokio::test]
async fn the_unrouted_ingest_path_is_refused_by_the_address_alone_and_ingests_nothing() {
    let harness = Harness::new();
    let probe = "unrouted-probe";
    let other = "unrouted-other";
    let arn = harness.create_stream(CONTROL, probe).await;
    harness.create_stream(CONTROL, other).await;
    let write_origin = harness.announced_authority(CONTROL, probe, "PUT_MEDIA").await;
    let archive_origin = harness
        .announced_authority(CONTROL, probe, "LIST_FRAGMENTS")
        .await;
    let other_archive_origin = harness
        .announced_authority(CONTROL, other, "LIST_FRAGMENTS")
        .await;
    assert_ne!(
        write_origin, archive_origin,
        "the ingest and the archive plane must be announced at their own addresses, \
         got {write_origin:?} for both"
    );
    assert_ne!(
        write_origin, CONTROL,
        "the ingest plane must not be announced at the control-plane address, \
         got {write_origin:?}"
    );

    // (case, the address the identical session was sent to) — every one of them an address
    // of this emulator that binds no route for the ingest path.
    let addresses: Vec<(&str, &str)> = vec![
        (
            "the control-plane front door, by its loopback address",
            CONTROL,
        ),
        ("the control-plane front door, by name", CONTROL_BY_NAME),
        (
            "the archive origin announced for this stream",
            archive_origin.as_str(),
        ),
        (
            "the archive origin announced for another stream",
            other_archive_origin.as_str(),
        ),
    ];

    for row in ROWS {
        let Row { case, session, .. } = *row;
        for (address, authority) in &addresses {
            let (headers, body) = session_request(session, probe, &arn);
            let response = harness.put_media_at(authority, &headers, body).await;

            assert_eq!(
                status(&response),
                404,
                "case {case:?} at {address:?} ({authority}): an address that binds no route \
                 for the ingest path answers 404, got {} {}",
                status(&response),
                text(&response)
            );
            assert_eq!(
                text(&response).trim(),
                UNROUTED_PATH_BODY,
                "case {case:?} at {address:?} ({authority}): expected the bare \
                 unknown-operation element as the whole body, got {:?}",
                text(&response)
            );
            assert_eq!(
                error_code(&response),
                "",
                "case {case:?} at {address:?} ({authority}): no operation was identified, so \
                 none is named in the error-type header, got {:?}",
                error_code(&response)
            );
            assert!(
                !text(&response).contains(probe) && !text(&response).contains(ABSENT_STREAM),
                "case {case:?} at {address:?} ({authority}): the refusal states nothing from \
                 the request, got {:?}",
                text(&response)
            );

            for stream in [probe, other] {
                let archived = harness.archived_fragments(CONTROL, stream).await;
                assert_eq!(
                    archived, 0,
                    "case {case:?} at {address:?} ({authority}): a session sent to an address \
                     that routes it nowhere ingests nothing, got {archived} fragment(s) on \
                     {stream:?}"
                );
            }
        }
    }

    // The identical sessions at the origin the ingest route IS bound at: each one earns the
    // answer its own content calls for, which is what the refusals above were decided
    // ahead of.
    let mut expected = 0usize;
    for row in ROWS {
        let Row {
            case,
            session,
            served_status,
            served_code,
            served_fragments,
            served_fault,
        } = *row;
        let (headers, body) = session_request(session, probe, &arn);
        let response = harness.put_media_at(&write_origin, &headers, body).await;

        assert_eq!(
            status(&response),
            served_status,
            "case {case:?} at the announced write origin: expected HTTP {served_status}, \
             got {} {}",
            status(&response),
            text(&response)
        );
        assert_ne!(
            text(&response).trim(),
            UNROUTED_PATH_BODY,
            "case {case:?} at the announced write origin: the ingest path is bound here, so \
             the unrouted-path refusal is not the answer, got {:?}",
            text(&response)
        );
        if let Some(served_code) = served_code {
            assert_eq!(
                error_code(&response),
                served_code,
                "case {case:?} at the announced write origin: expected the {served_code} \
                 class, got {:?} {}",
                error_code(&response),
                text(&response)
            );
        }
        let acknowledged: Vec<String> = event_types(&response);
        assert_eq!(
            acknowledged.iter().filter(|event| *event == "PERSISTED").count(),
            served_fragments,
            "case {case:?} at the announced write origin: expected {served_fragments} \
             PERSISTED acknowledgement(s), got {acknowledged:?}"
        );
        match served_fault {
            Some((error_class, error_id)) => {
                let faults: Vec<serde_json::Value> = acknowledgements(&response)
                    .into_iter()
                    .filter(|acknowledgement| {
                        acknowledgement.get("EventType").and_then(serde_json::Value::as_str)
                            == Some("ERROR")
                    })
                    .collect();
                assert_eq!(
                    faults.len(),
                    1,
                    "case {case:?} at the announced write origin: expected one in-band fault \
                     acknowledgement, got {:?}",
                    text(&response)
                );
                assert_eq!(
                    faults[0].get("ErrorCode").and_then(serde_json::Value::as_str),
                    Some(error_class),
                    "case {case:?} at the announced write origin: expected the {error_class} \
                     class in band, got {:?}",
                    faults[0]
                );
                assert_eq!(
                    faults[0].get("ErrorId").and_then(serde_json::Value::as_u64),
                    Some(error_id),
                    "case {case:?} at the announced write origin: expected ErrorId \
                     {error_id}, got {:?}",
                    faults[0]
                );
                assert_eq!(
                    faults[0].get("FragmentNumber"),
                    None,
                    "case {case:?} at the announced write origin: a container that does not \
                     read holds no fragment to name, got {:?}",
                    faults[0]
                );
            }
            None => assert!(
                !acknowledged.iter().any(|event| event == "ERROR"),
                "case {case:?} at the announced write origin: expected no in-band fault, \
                 got {acknowledged:?}"
            ),
        }

        expected += served_fragments;
        let archived = harness.archived_fragments(CONTROL, probe).await;
        assert_eq!(
            archived, expected,
            "case {case:?} at the announced write origin: expected {expected} archived \
             fragment(s), got {archived}"
        );
    }
}

/// RULE: a path this family's models DO bind, asked for at an address that binds no
/// operation for it, keeps the front door's undeterminable-operation envelope — HTTP 400
/// carrying `Output` and `Version`, with no error-type header — so the unrouted-path
/// refusal above is a classification of the path and not a blanket status swap.
///
/// The read route's own refusal is what the recording behind the resolved report
/// `20261029T173603Z-kinesisvideo-front-door-answers-an-unroutable-operation-with.md`
/// grades: the modelled read route asked for at the control-plane endpoint is answered
/// with that envelope, which is why the status this suite's first test pins for the
/// unmodelled ingest path is not served here.
///
/// The rows differ in the address the modelled read route was asked for at; the address
/// announced for the plane that binds it still serves it.
#[tokio::test]
async fn a_path_the_models_bind_keeps_the_front_doors_undeterminable_operation_envelope() {
    let harness = Harness::new();
    let probe = "envelope-probe";
    harness.create_stream(CONTROL, probe).await;
    let read_origin = harness.announced_authority(CONTROL, probe, "GET_MEDIA").await;
    let archive_origin = harness
        .announced_authority(CONTROL, probe, "LIST_FRAGMENTS")
        .await;
    let read = serde_json::json!({
        "StreamName": probe,
        "StartSelector": { "StartSelectorType": "EARLIEST" },
    });

    // (case, the address the modelled read route was asked for at).
    let addresses: Vec<(&str, &str)> = vec![
        ("the control-plane front door", CONTROL),
        ("the announced archive origin", archive_origin.as_str()),
    ];
    for (case, authority) in &addresses {
        let response = harness.call_at(authority, "/getMedia", &read).await;
        assert_eq!(
            status(&response),
            400,
            "case {case:?} ({authority}): expected the front door's undeterminable-operation \
             answer, got {} {}",
            status(&response),
            text(&response)
        );
        assert_eq!(
            json(&response),
            unknown_operation_envelope(),
            "case {case:?} ({authority}): expected the unknown-operation envelope as the \
             whole body, got {:?}",
            text(&response)
        );
        assert_eq!(
            error_code(&response),
            "",
            "case {case:?} ({authority}): no operation was identified, so none is named in \
             the error-type header, got {:?}",
            error_code(&response)
        );
    }

    let served = harness.call_at(&read_origin, "/getMedia", &read).await;
    assert_eq!(
        status(&served),
        200,
        "case \"the announced read origin\" ({read_origin}): the address announced for the \
         plane that binds the route serves it, got {} {}",
        status(&served),
        text(&served)
    );
}
