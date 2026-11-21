// SPDX-License-Identifier: CC0-1.0

//! The VIRTUAL-HOSTED half of the object-scoped refusal table: a request that names its
//! bucket in the host names its object in the whole path, and the refusal's envelope names
//! that same key.
//!
//! "Both the virtual-hosted-style requests and the path-style requests are supported"
//! (`GetObject`'s own documentation): a path-style request carries the bucket as the first
//! segment of its path, and a virtual-hosted-style one carries it as the first label of its
//! host — `https://bucket-name.s3.region-code.amazonaws.com/key-name` — leaving the whole
//! path to be the key. Every SDK's URL presigner writes the second form, so a client reading
//! an object through a presigned URL addresses it that way; the key an object-scoped 404
//! names must therefore be the same key in either form, which is what this suite drives.
//!
//! The vehicle is the tagging read, and the key an object-scoped 404 names is the key the
//! request addressed — decoded and whole — in either address form.
//!
//! The rows, the setup and the envelope assertion are
//! [`super::object_scoped_refusal_envelope`]'s own, so the two address forms are judged by
//! one table and one rule rather than by two copies of either.
//!
//! A request names its bucket ONCE, which is what the second half of this suite drives: a
//! virtual-hosted request's path is the KEY whatever text its first segment carries — a
//! bucket's own name, another bucket's name or a name that is no bucket's — so a key written
//! under a prefix named after a bucket is addressed whole rather than having that segment
//! consumed a second time as the bucket. The rows span the hosts that form is sent to: the
//! loopback name and address a client configured with one local endpoint URL dials, AWS's own
//! published host, and an endpoint reached under a name of its own, where the path is what
//! settles which bucket a request addresses.

use super::bucket_lifecycle::{router, send_bytes, send_bytes_to_host};
use super::object_and_tagging_roundtrip::{bucket, get_object, text};
use super::object_scoped_refusal_envelope::{
    assert_names_object, bucket_holding, key_spelling_rows,
};
use service_com_amazonaws_s3_amazons3::state::NO_SUCH_KEY_CODE;

#[tokio::test]
async fn a_virtual_hosted_request_is_refused_about_the_key_its_whole_path_names() {
    let router = router();
    let name = "object-envelope-host-style";
    bucket_holding(&router, name).await;

    for (case, spelled, key) in key_spelling_rows() {
        let answer = send_bytes_to_host(
            &router,
            "GET",
            &format!("{name}.s3.us-east-1.amazonaws.com"),
            &format!("/{spelled}?tagging"),
            &[],
            b"",
        )
        .await;
        assert_names_object(
            &format!("{case} addressed virtual-hosted"),
            &answer,
            NO_SUCH_KEY_CODE,
            key,
            None,
        );
    }
}

/// The bucket the rows below address, and the name their keys are prefixed with: the two are
/// the same text, which is one of the coincidences the rule is about.
const PREFIX_BUCKET: &str = "host-style-prefix-bucket";

/// A SECOND bucket this emulator holds, for the other coincidence: a key prefixed with the
/// name of a bucket that is not the one the request addresses.
const OTHER_BUCKET: &str = "host-style-other-bucket";

/// The bytes the round trip below stores. Fixed content: what is under test is the address
/// the write landed on, not the payload.
const PREFIXED_BODY: &[u8] = b"host-style-prefixed-key-subject";

/// The virtual-hosted address of `name` at the loopback NAME — the plainest of the local
/// forms below.
fn local_host_of(name: &str) -> String {
    format!("{name}.localhost")
}

/// The virtual-hosted addresses of `name` at an endpoint this emulator is reached at — the
/// form a client configured with one endpoint URL sends: the loopback name and the loopback
/// address, with and without the port such an endpoint is reached on (the emulator's own
/// default data port, which is part of the host a client writes), a name UNDER the loopback
/// zone RFC 6761 reserves for that interface, a loopback name with the root dot written, and
/// one of AWS's own published S3 hosts.
fn served_endpoint_hosts_of(name: &str) -> Vec<String> {
    vec![
        local_host_of(name),
        format!("{name}.localhost:9297"),
        format!("{name}.127.0.0.1:9297"),
        format!("{name}.s3.localhost:9297"),
        format!("{name}.localhost."),
        format!("{name}.s3.us-east-1.amazonaws.com"),
    ]
}

/// A host this emulator is NOT known to be reached at: an operator's own DNS name for it.
/// Such a host could be an endpoint of its own whose first label happens to be a bucket's
/// name, so there the PATH is what settles the address form.
fn named_endpoint_host_of(name: &str) -> String {
    format!("{name}.s3.example.test")
}

/// The keys a virtual-hosted request has to address WHOLE: one row per shape a key can take
/// whose leading segment is a bucket name, plus a control whose leading segment is
/// bucket-SHAPED but names no bucket at all.
///
/// Each row is (case, the key as the request URI spells it, the key the emulator must
/// address).
fn bucket_named_prefix_rows() -> Vec<(&'static str, String, String)> {
    vec![
        (
            "a key under a prefix named after its own bucket",
            format!("{PREFIX_BUCKET}/reports/summary.json"),
            format!("{PREFIX_BUCKET}/reports/summary.json"),
        ),
        (
            "the same key with that prefix repeated",
            format!("{PREFIX_BUCKET}/{PREFIX_BUCKET}/reports/summary.json"),
            format!("{PREFIX_BUCKET}/{PREFIX_BUCKET}/reports/summary.json"),
        ),
        (
            "a prefix named after the bucket, ending in a separator",
            format!("{PREFIX_BUCKET}/reports/"),
            format!("{PREFIX_BUCKET}/reports/"),
        ),
        (
            "a key that IS the bucket's own name",
            PREFIX_BUCKET.to_string(),
            PREFIX_BUCKET.to_string(),
        ),
        (
            "the bucket's name joined to the rest by an encoded separator",
            format!("{PREFIX_BUCKET}%2Freports/summary.json"),
            format!("{PREFIX_BUCKET}/reports/summary.json"),
        ),
        (
            "a key under a prefix named after ANOTHER bucket this emulator holds",
            format!("{OTHER_BUCKET}/reports/summary.json"),
            format!("{OTHER_BUCKET}/reports/summary.json"),
        ),
        (
            "a key that IS another bucket's name",
            OTHER_BUCKET.to_string(),
            OTHER_BUCKET.to_string(),
        ),
        (
            "a leading segment that is bucket-shaped but names no bucket",
            "no-such-prefix-bucket/reports/summary.json".to_string(),
            "no-such-prefix-bucket/reports/summary.json".to_string(),
        ),
    ]
}

#[tokio::test]
async fn a_virtual_hosted_key_whose_leading_segment_names_a_bucket_is_addressed_whole() {
    let router = router();
    bucket(&router, PREFIX_BUCKET).await;
    bucket(&router, OTHER_BUCKET).await;

    for host in served_endpoint_hosts_of(PREFIX_BUCKET) {
        for (case, spelled, key) in bucket_named_prefix_rows() {
            let refused = send_bytes_to_host(
                &router,
                "GET",
                &host,
                &format!("/{spelled}?tagging"),
                &[],
                b"",
            )
            .await;
            assert_names_object(
                &format!("{case}, read at {host:?}"),
                &refused,
                NO_SUCH_KEY_CODE,
                // The WHOLE path the host-addressed request named, which is the key.
                &key,
                None,
            );
        }
    }
}

#[tokio::test]
async fn an_object_written_virtual_hosted_under_its_bucket_name_reads_back_at_that_key() {
    let router = router();
    bucket(&router, PREFIX_BUCKET).await;
    let host = local_host_of(PREFIX_BUCKET);
    let key = format!("{PREFIX_BUCKET}/reports/summary.json");

    let written = send_bytes_to_host(
        &router,
        "PUT",
        &host,
        &format!("/{key}"),
        &[("content-type", "application/octet-stream")],
        PREFIXED_BODY,
    )
    .await;
    assert_eq!(
        written.status, 200,
        "case {key:?}: the virtual-hosted write must succeed, got {} with body {:?}",
        written.status,
        text(&written)
    );

    // The key the BUCKET holds, read from the bucket's own listing: the whole key the request
    // named, its prefix segment included.
    let listed = send_bytes(
        &router,
        "GET",
        &format!("/{PREFIX_BUCKET}?list-type=2"),
        &[],
        b"",
    )
    .await;
    assert_eq!(
        listed.elements("Key"),
        vec![key.clone()],
        "case {key:?}: the listing must report the key the write landed on, got {:?}",
        text(&listed)
    );

    // And it reads back at that key in BOTH address forms, which is what makes the two
    // addresses one object rather than two.
    let path_style = get_object(&router, PREFIX_BUCKET, &key, "", &[]).await;
    assert_eq!(
        path_style.body.as_ref(),
        PREFIXED_BODY,
        "case {key:?}: the path-style read must serve the stored bytes, got status {} and \
         body {:?}",
        path_style.status,
        text(&path_style)
    );
    let host_style = send_bytes_to_host(&router, "GET", &host, &format!("/{key}"), &[], b"").await;
    assert_eq!(
        host_style.body.as_ref(),
        PREFIXED_BODY,
        "case {key:?}: the virtual-hosted read must serve the stored bytes, got status {} and \
         body {:?}",
        host_style.status,
        text(&host_style)
    );
}

#[tokio::test]
async fn a_host_whose_label_names_no_bucket_still_names_the_bucket_in_its_path() {
    let router = router();
    bucket(&router, PREFIX_BUCKET).await;

    // The boundary of the rule: a host whose first label is bucket-SHAPED but names no bucket
    // this emulator holds carries no bucket at all, so the first path segment is the bucket
    // and the key is what follows it — which is what leaves a client that reaches a local
    // endpoint under its own name addressing buckets path-style.
    let refused = send_bytes_to_host(
        &router,
        "GET",
        "no-such-host-bucket.localhost",
        &format!("/{PREFIX_BUCKET}/reports/absent.json?tagging"),
        &[],
        b"",
    )
    .await;
    assert_names_object(
        "a host label naming no bucket",
        &refused,
        NO_SUCH_KEY_CODE,
        "reports/absent.json",
        None,
    );
}

#[tokio::test]
async fn at_a_host_of_its_own_name_the_path_settles_which_bucket_is_addressed() {
    let router = router();
    bucket(&router, PREFIX_BUCKET).await;
    bucket(&router, OTHER_BUCKET).await;
    let host = named_endpoint_host_of(PREFIX_BUCKET);

    // A host this emulator is not known to be reached at, whose first label addresses a
    // bucket: a path segment that addresses no OTHER bucket contradicts nothing, so the host
    // still names the bucket and the whole path is the key.
    let whole = send_bytes_to_host(
        &router,
        "GET",
        &host,
        &format!("/{PREFIX_BUCKET}/reports/absent.json?tagging"),
        &[],
        b"",
    )
    .await;
    assert_names_object(
        "a key under a prefix named after its own bucket, at a host of the endpoint's own name",
        &whole,
        NO_SUCH_KEY_CODE,
        &format!("{PREFIX_BUCKET}/reports/absent.json"),
        None,
    );

    // And a first segment that addresses ANOTHER bucket is where a path-style request names
    // its bucket, so it is read as the bucket and the key is what follows it.
    let path_style = send_bytes_to_host(
        &router,
        "GET",
        &host,
        &format!("/{OTHER_BUCKET}/reports/absent.json?tagging"),
        &[],
        b"",
    )
    .await;
    assert_names_object(
        "a path naming another bucket than the host's label, at a host of the endpoint's own \
         name",
        &path_style,
        NO_SUCH_KEY_CODE,
        "reports/absent.json",
        None,
    );
}
