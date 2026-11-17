// SPDX-License-Identifier: CC0-1.0

//! What a general purpose bucket answers a request that addresses a RENAME, and what a
//! directory bucket answers the same request.
//!
//! Every assertion drives a real HTTP request through the router (the harness the
//! bucket-lifecycle and object suites in this crate already own), so what is pinned is the
//! wire behaviour — status, error code, `<Message>`, the element that names what was refused
//! — and what the two keys hold afterwards.
//!
//! The rule under test: a request addressed to a general purpose bucket that asks, through a
//! member S3 serves for directory buckets alone, for functionality that bucket's endpoint does
//! not implement is refused with HTTP 501 `NotImplemented` naming the request element that
//! asked for it — the `<Header>` and the sentence that code publishes for a header when the
//! member arrives in a header, and the `<QueryParameter>` and the sentence for a query
//! parameter when the parameter is all that asks. A rename is asked for by
//! `x-amz-rename-source`: the model binds the rename source to that header and makes it
//! `@required` (`smithy_query(query="operation",operation="RenameObject")` reports
//! `RenameSource: {httpHeader: "x-amz-rename-source", required: true}`, on the URI
//! `http {method: "PUT", uri: "/{Bucket}/{Key+}?renameObject"}`), and `RenameObject` "renames
//! an existing object in a directory bucket that uses the S3 Express One Zone storage class"
//! (its own documentation) — so a general purpose bucket's endpoint implements nothing for
//! either. Nothing is written by a refused request, so a rename a general purpose bucket
//! cannot serve never lands as an ordinary write of the bytes the request carried.
//!
//! The rows differ along what the rule generalises over: whether the request carries the
//! `x-amz-rename-source` header the rename's own route requires (which is what decides both
//! whether the request is shaped like a rename or like an ordinary write and which element the
//! refusal names), how the parameter is spelled in the query string, whether it travels at
//! all, whether another query parameter travels beside it, which conditional or idempotency
//! members travel with it, the form the source is written in, the shape of the destination
//! key, and whether the destination key already holds an object.

use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_s3_amazons3::scaffolded::NOT_IMPLEMENTED_QUERY_PARAMETER_MESSAGE;
use service_com_amazonaws_s3_amazons3::service::object_write::{
    RENAME_OBJECT_PARAMETER, RENAME_SOURCE_HEADER,
};
use service_com_amazonaws_s3_amazons3::state::{
    NOT_IMPLEMENTED_CODE, NOT_IMPLEMENTED_HEADER_MESSAGE, NOT_IMPLEMENTED_STATUS,
};

use super::bucket_lifecycle::{router, send_bytes, Answer};
use super::directory_bucket_sessions_and_appends::directory_bucket;
use super::object_and_tagging_roundtrip::{bucket, get_object, put_object, text};

/// The bytes every source object in this suite holds, so a refused request that had instead
/// been served as a write would be visible as a key holding the request's own payload.
const SOURCE: &[u8] = b"rename-source-payload";

/// The bytes a refused request carries as its body — what an ordinary write would have
/// stored at the key it addressed.
const WRITTEN_IF_SERVED: &[u8] = b"renamed-by-a-write";

/// The key every row renames FROM, written before each row runs.
const SOURCE_KEY: &str = "source.bin";

/// The name the rows spell the source bucket with, replaced by the bucket a test really
/// addresses — so one table is driven against a general purpose bucket and against a
/// directory bucket.
const BUCKET_PLACEHOLDER: &str = "<bucket>";

/// One row: the case, the query string the request carries after the key, the destination key
/// it addresses, the value `x-amz-rename-source` carries (when the row sends that header at
/// all), the other headers travelling with it, and whether the destination key already holds
/// an object.
struct Row {
    case: &'static str,
    query: &'static str,
    key: &'static str,
    source: Option<&'static str>,
    headers: &'static [(&'static str, &'static str)],
    destination_held: bool,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the bare parameter, with the rename source header the route requires",
            query: "?renameObject",
            key: "renamed/flat.bin",
            source: Some("source.bin"),
            headers: &[],
            destination_held: false,
        },
        Row {
            case: "the parameter spelled with an empty value, the bucket in front of the key",
            query: "?renameObject=",
            key: "renamed/logs/2026/nested.bin",
            source: Some("<bucket>/source.bin"),
            headers: &[],
            destination_held: false,
        },
        Row {
            case: "another parameter travelling in front of it, a leading slash on the source",
            query: "?x-id=RenameObject&renameObject",
            key: "renamed/with-another-parameter.bin",
            source: Some("/<bucket>/source.bin"),
            headers: &[],
            destination_held: false,
        },
        Row {
            case: "no parameter at all, so the header alone asks for the rename",
            query: "",
            key: "renamed/header-only.bin",
            source: Some("source.bin"),
            headers: &[],
            destination_held: false,
        },
        Row {
            case: "a conditional rename, carrying the destination condition",
            query: "?renameObject",
            key: "renamed/conditional.bin",
            source: Some("source.bin"),
            headers: &[("If-None-Match", "*")],
            destination_held: false,
        },
        Row {
            case: "an idempotent rename, carrying a client token",
            query: "?renameObject",
            key: "renamed/tokened.bin",
            source: Some("source.bin"),
            headers: &[("x-amz-client-token", "a-caller-chosen-token")],
            destination_held: false,
        },
        Row {
            case: "a destination key that already holds an object",
            query: "?renameObject",
            key: "renamed/held.bin",
            source: Some("source.bin"),
            headers: &[],
            destination_held: true,
        },
        Row {
            case: "the bare parameter and no rename source header at all",
            query: "?renameObject",
            key: "renamed/parameter-only.bin",
            source: None,
            headers: &[],
            destination_held: false,
        },
        Row {
            case: "the parameter beside another one, and no rename source header",
            query: "?x-id=PutObject&renameObject",
            key: "renamed/parameter-only-beside-another.bin",
            source: None,
            headers: &[],
            destination_held: false,
        },
    ]
}

/// The request a row describes, driven against `bucket_name`.
async fn ask_for_the_rename(router: &MultiServiceRouter, bucket_name: &str, row: &Row) -> Answer {
    let source = row
        .source
        .map(|source| source.replace(BUCKET_PLACEHOLDER, bucket_name));
    let mut headers: Vec<(&str, &str)> = Vec::new();
    if let Some(source) = source.as_deref() {
        headers.push((RENAME_SOURCE_HEADER, source));
    }
    headers.extend_from_slice(row.headers);
    send_bytes(
        router,
        "PUT",
        &format!("/{bucket_name}/{}{}", row.key, row.query),
        &headers,
        WRITTEN_IF_SERVED,
    )
    .await
}

/// Whether the request a row describes is ROUTED to the rename: the `?renameObject` parameter
/// the model pins on the operation's URI is what addresses it, and the source header is the
/// member it requires, so a request missing either is shaped like an ordinary write.
fn routed_as_rename(row: &Row) -> bool {
    row.query.contains(RENAME_OBJECT_PARAMETER) && row.source.is_some()
}

/// RULE: a request that asks a GENERAL PURPOSE bucket for a rename is refused HTTP 501
/// `NotImplemented` naming the element that asked — the `x-amz-rename-source` header whenever
/// the request carries it, and otherwise the `?renameObject` parameter — and nothing is
/// written.
#[tokio::test]
async fn a_rename_addressed_to_a_general_purpose_bucket_is_not_implemented() {
    let router = router();
    let name = "general-purpose-renames";
    bucket(&router, name).await;
    let stored = put_object(&router, name, SOURCE_KEY, SOURCE, &[]).await;
    assert_eq!(
        stored.status,
        200,
        "the source object must be written first, got {} with body {:?}",
        stored.status,
        text(&stored)
    );

    for row in rows() {
        let case = row.case;
        if row.destination_held {
            let held = put_object(&router, name, row.key, SOURCE, &[]).await;
            assert_eq!(
                held.status,
                200,
                "case {case:?}: the destination fixture write must succeed, got {} with body \
                 {:?}",
                held.status,
                text(&held)
            );
        }

        let refused = ask_for_the_rename(&router, name, &row).await;
        assert_eq!(
            refused.status,
            NOT_IMPLEMENTED_STATUS,
            "case {case:?}: expected HTTP {NOT_IMPLEMENTED_STATUS}, got {} with body {:?}",
            refused.status,
            text(&refused)
        );
        assert_eq!(
            refused.error_code(),
            NOT_IMPLEMENTED_CODE,
            "case {case:?}: expected the published code, got {:?}",
            refused.error_code()
        );
        // The refusal names the element that ASKED: the header when the request carries one S3
        // serves for directory buckets alone, and otherwise the parameter. Each sentence names
        // only the CLASS of element, so the element itself is the only part of the refusal that
        // says which one the caller must drop.
        let (expected_message, element, expected_element) = if row.source.is_some() {
            (
                NOT_IMPLEMENTED_HEADER_MESSAGE,
                "Header",
                RENAME_SOURCE_HEADER,
            )
        } else {
            (
                NOT_IMPLEMENTED_QUERY_PARAMETER_MESSAGE,
                "QueryParameter",
                RENAME_OBJECT_PARAMETER,
            )
        };
        assert_eq!(
            refused.error_message(),
            expected_message,
            "case {case:?}: expected the published sentence {expected_message:?}, got {:?}",
            refused.error_message()
        );
        assert_eq!(
            refused.element(element).as_deref(),
            Some(expected_element),
            "case {case:?}: the refusal must name the {element} that asked for it, expected \
             {expected_element:?}, got {:?} in {:?}",
            refused.element(element),
            text(&refused)
        );

        // Nothing was written: a destination that held nothing still holds nothing, and one
        // that held the source bytes still holds exactly those.
        let after = get_object(&router, name, row.key, "", &[]).await;
        let expected = if row.destination_held { 200 } else { 404 };
        assert_eq!(
            after.status, expected,
            "case {case:?}: expected the destination to answer {expected} after the refusal, \
             got {} with body {:?}",
            after.status,
            text(&after)
        );
        if row.destination_held {
            assert_eq!(
                after.body.as_ref(),
                SOURCE,
                "case {case:?}: a refused rename must leave the destination's bytes alone, got \
                 {:?}",
                text(&after)
            );
        }

        // And the source is still there, with its own bytes: nothing was moved.
        let source = get_object(&router, name, SOURCE_KEY, "", &[]).await;
        assert_eq!(
            source.body.as_ref(),
            SOURCE,
            "case {case:?}: a refused rename must leave the source exactly as it was, got \
             {:?}",
            text(&source)
        );
    }

    // The SAME write without either element is served, so the refusals above are about the
    // element that asked and not about the request's path or payload.
    let served = put_object(&router, name, "renamed/plain.bin", WRITTEN_IF_SERVED, &[]).await;
    assert_eq!(
        served.status,
        200,
        "an ordinary write of the same bytes must be served, got {} with body {:?}",
        served.status,
        text(&served)
    );

    // A bucket this emulator does not hold answers that, the way the write path answers the
    // same address: the kind of a bucket that is not there says nothing about what it serves.
    let absent = send_bytes(
        &router,
        "PUT",
        "/general-purpose-renames-absent/renamed.bin?renameObject",
        &[(RENAME_SOURCE_HEADER, "general-purpose-renames/source.bin")],
        WRITTEN_IF_SERVED,
    )
    .await;
    assert_eq!(
        absent.status,
        404,
        "a bucket this emulator does not hold answers 404, got {} with body {:?}",
        absent.status,
        text(&absent)
    );
    assert_eq!(
        absent.error_code(),
        "NoSuchBucket",
        "a bucket this emulator does not hold answers NoSuchBucket, got {:?}",
        absent.error_code()
    );
}

/// RULE, the other side of it: a DIRECTORY bucket is the kind S3 serves renames for, so none of
/// the same requests is refused for the header or the parameter there — the ones addressed to
/// the rename MOVE the object (the destination holds what the source held and the source holds
/// nothing), and the ones that are shaped like an ordinary write are that write.
#[tokio::test]
async fn a_directory_bucket_refuses_neither_the_header_nor_the_parameter() {
    let router = router();
    let name = "renames--use1-az4--x-s3";
    directory_bucket(&router, name, "use1-az4").await;

    for row in rows() {
        let case = row.case;
        let stored = put_object(&router, name, SOURCE_KEY, SOURCE, &[]).await;
        assert_eq!(
            stored.status,
            200,
            "case {case:?}: the source object must be written first, got {} with body {:?}",
            stored.status,
            text(&stored)
        );
        if row.destination_held {
            let held = put_object(&router, name, row.key, WRITTEN_IF_SERVED, &[]).await;
            assert_eq!(
                held.status,
                200,
                "case {case:?}: the destination fixture write must succeed, got {}",
                held.status
            );
        }

        let answered = ask_for_the_rename(&router, name, &row).await;
        assert_eq!(
            answered.status,
            200,
            "case {case:?}: a directory bucket must serve the request, got {} with body {:?}",
            answered.status,
            text(&answered)
        );
        assert_eq!(
            answered.element("Header"),
            None,
            "case {case:?}: a bucket that serves renames must refuse no header, got {:?}",
            text(&answered)
        );
        assert_eq!(
            answered.element("QueryParameter"),
            None,
            "case {case:?}: a bucket that serves renames must refuse no parameter, got {:?}",
            text(&answered)
        );

        // What the two keys hold afterwards: a request addressed to the rename MOVED the
        // object, and one shaped like an ordinary write stored its own bytes and left the
        // source where it was.
        let (expected_destination, expected_source_status) = if routed_as_rename(&row) {
            (SOURCE, 404)
        } else {
            (WRITTEN_IF_SERVED, 200)
        };
        let destination = get_object(&router, name, row.key, "", &[]).await;
        assert_eq!(
            (destination.status, destination.body.as_ref()),
            (200, expected_destination),
            "case {case:?}: expected the destination to hold {:?}, got {} with body {:?}",
            String::from_utf8_lossy(expected_destination),
            destination.status,
            text(&destination)
        );
        let source = get_object(&router, name, SOURCE_KEY, "", &[]).await;
        assert_eq!(
            source.status,
            expected_source_status,
            "case {case:?}: expected the source to answer {expected_source_status} after the \
             request, got {} with body {:?}",
            source.status,
            text(&source)
        );
    }
}
