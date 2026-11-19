// SPDX-License-Identifier: CC0-1.0

//! WHICH HOST an uploaded list's `Location` is announced at, end to end over the wire.
//!
//! Every request goes through [`smithy_server_runtime::MultiServiceRouter::dispatch`]
//! (the harness lives in the `detector_lifecycle` suite), so routing, restJson1
//! (de)serialization, modelled-constraint validation and the handlers are exercised
//! together.
//!
//! The rule under test, asserted over a TABLE of inputs rather than over one recorded
//! call: a `Location` is the address of the FILE an uploaded list was uploaded as, which a
//! caller follows to read it, so the address a read reports leads back to THIS emulator.
//! The caller's own value fixes WHICH object — the bucket and the key it names — and the
//! read answers that object addressed path-style at the endpoint serving the read; it never
//! answers a host AWS publishes its own endpoints at, which resolves nowhere near this
//! process or, worse, to genuine AWS infrastructure.
//!
//! The rows therefore span what the rule generalises over:
//!
//! * the SPELLING of the submitted address — every S3 URL form names one and the same
//!   object, so every row of the table must be announced at the SAME address;
//! * the uploaded-list FAMILY, since the rule is the family's and not the IPSet's: the
//!   IPSet, the threat list and the two entity sets each store a location;
//! * the ENDPOINT the read arrives at, which is what decides the announced host: a client
//!   configured with the emulator's own endpoint resolves nothing else and is handed that
//!   very address, port included, while a caller that resolves this service's own published
//!   host is answered the base the framework reports for an emulator served on no port of
//!   its own.

use serde_json::json;

use crate::detector_lifecycle::{
    call_at, enabled_detector, new_router, Router, ABSENT_DETECTOR_ID, EMULATOR_BASE,
};

/// The bucket every row's location names.
const BUCKET: &str = "gd-uploaded-lists";

/// The object key every row's location names, deep enough that the whole path and not
/// just a last segment has to survive the announcement.
const KEY: &str = "lists/2026/trusted.txt";

/// One uploaded-list family: the route segment its sub-resource hangs off, the `Format`
/// value a create needs and the `jsonName` of the id the create reports. The `Format`
/// values are members of `IpSetFormat`, `ThreatIntelSetFormat`, `ThreatEntitySetFormat`
/// and `TrustedEntitySetFormat`
/// (`smithy_query(shape="IpSetFormat,ThreatIntelSetFormat,ThreatEntitySetFormat,TrustedEntitySetFormat")`).
const FAMILIES: [(&str, &str, &str); 4] = [
    ("ipset", "ipSetId", "TXT"),
    ("threatintelset", "threatIntelSetId", "STIX"),
    ("threatentityset", "threatEntitySetId", "TXT"),
    ("trustedentityset", "trustedEntitySetId", "TXT"),
];

/// The spellings of the ONE object [`BUCKET`] and [`KEY`] name: the `s3://` scheme, the
/// Region-less and the regional path-style public endpoints, the virtual-hosted-style one,
/// an object URL on a private S3 endpoint reached over `http` on a port of its own, and an
/// object URL on a host that is no S3 endpoint at all. Each addresses the same file, so
/// each must be announced at the same address.
fn spellings() -> [(&'static str, String); 6] {
    [
        ("the s3:// scheme", format!("s3://{BUCKET}/{KEY}")),
        (
            "a path-style public S3 endpoint",
            format!("https://s3.amazonaws.com/{BUCKET}/{KEY}"),
        ),
        (
            "a regional path-style public S3 endpoint",
            format!("https://s3.eu-west-1.amazonaws.com/{BUCKET}/{KEY}"),
        ),
        (
            "a virtual-hosted-style public S3 endpoint",
            format!("https://{BUCKET}.s3.us-west-2.amazonaws.com/{KEY}"),
        ),
        (
            "an object URL on a private S3 endpoint",
            format!("http://127.0.0.1:9000/{BUCKET}/{KEY}"),
        ),
        (
            "an object URL on a host that is no S3 endpoint",
            format!("https://files.example-corp.com/{BUCKET}/{KEY}"),
        ),
    ]
}

/// The endpoint authorities every row is read at, and the base a location announced to
/// such a caller is rooted at: the emulator's own endpoint as a loopback literal with a
/// port and by its loopback name on another, each the only address such a client resolves,
/// and this service's own published host, which is no address of this emulator's.
fn read_endpoints() -> [(&'static str, String); 3] {
    [
        ("127.0.0.1:39461", "https://127.0.0.1:39461".to_string()),
        ("localhost:4566", "https://localhost:4566".to_string()),
        ("guardduty.us-east-1.amazonaws.com", EMULATOR_BASE.to_string()),
    ]
}

/// The endpoint the creates of this suite are sent to: a location is read by its own
/// syntax, so where the create landed is no part of what a read announces.
const CREATE_ENDPOINT: &str = "127.0.0.1:39461";

/// A router holding one enabled detector. One per row: a detector holds at most one
/// trusted entity set and the account at most one ACTIVATED IPSet, and neither allowance
/// is this rule's business.
async fn detector() -> (Router, String) {
    let router = new_router();
    let detector_id = enabled_detector(&router).await;
    (router, detector_id)
}

#[tokio::test]
async fn an_uploaded_lists_location_is_announced_at_the_endpoint_serving_the_read() {
    for (segment, id_member, format) in FAMILIES {
        for (why, location) in spellings() {
            let (router, detector_id) = detector().await;
            let create_case = format!("{segment}: {why}");
            let (status, created) = call_at(
                &router,
                CREATE_ENDPOINT,
                "POST",
                &format!("/detector/{detector_id}/{segment}"),
                Some(json!({
                    "name": "announced-list",
                    "format": format,
                    "location": location,
                    "activate": true
                })),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {create_case:?}: expected the create to be served, got {status} {created:?}"
            );
            let id = created[id_member]
                .as_str()
                .unwrap_or_else(|| {
                    panic!("case {create_case:?}: expected an id, got {created:?}")
                })
                .to_string();

            for (endpoint, base) in read_endpoints() {
                let case = format!("{create_case} read at {endpoint}");
                let (status, read) = call_at(
                    &router,
                    endpoint,
                    "GET",
                    &format!("/detector/{detector_id}/{segment}/{id}"),
                    None,
                )
                .await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: the stored list must read back, got {status} {read:?}"
                );
                let announced = read["location"].as_str().unwrap_or_default();
                // The SAME object the create named — bucket and key — at the endpoint
                // this read arrived at, path-style, whichever spelling was submitted.
                let expected = format!("{base}/{BUCKET}/{KEY}");
                assert_eq!(
                    announced, expected,
                    "case {case:?}: expected the object announced as {expected:?}, got \
                     {announced:?}"
                );
                // And never a host AWS publishes its own endpoints at: the file is in
                // this emulator's object storage, and a client following an AWS host
                // leaves the emulated environment carrying its credentials. The list of
                // those zones is the shared one
                // (`service_common::data_plane_addressing::AWS_ENDPOINT_DNS_SUFFIXES`).
                let host = ::service_common::data_plane_addressing::host_only(
                    ::service_common::data_plane_addressing::url_authority(announced),
                );
                assert!(
                    !::service_common::data_plane_addressing::addresses_an_aws_endpoint(host),
                    "case {case:?}: an announced location must not name a host AWS \
                     publishes, got {announced:?}"
                );
            }
        }
    }
}

/// The announcement is a READ-side projection, so it changes nothing: a second read
/// answers the same address, a relocation is announced at the object the UPDATE named, and
/// a read of a detector nothing minted is still refused rather than answered an address.
#[tokio::test]
async fn a_relocation_is_announced_at_the_object_the_update_named() {
    for (segment, id_member, format) in FAMILIES {
        let (router, detector_id) = detector().await;
        let case = segment;
        let (status, created) = call_at(
            &router,
            CREATE_ENDPOINT,
            "POST",
            &format!("/detector/{detector_id}/{segment}"),
            Some(json!({
                "name": "relocated-list",
                "format": format,
                "location": format!("s3://{BUCKET}/{KEY}"),
                "activate": false
            })),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the baseline create expected 200, got {status} {created:?}"
        );
        let id = created[id_member]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: expected an id, got {created:?}"))
            .to_string();
        let path = format!("/detector/{detector_id}/{segment}/{id}");
        let expected = format!("{EMULATOR_BASE}/{BUCKET}/{KEY}");

        // Twice, with nothing in between: a read reports and does not re-address.
        for attempt in 1..=2 {
            let (status, read) = call_at(&router, PUBLIC_READ_ENDPOINT, "GET", &path, None).await;
            assert_eq!(
                status, 200,
                "case {case:?} read {attempt}: expected 200, got {status} {read:?}"
            );
            assert_eq!(
                read["location"].as_str(),
                Some(expected.as_str()),
                "case {case:?} read {attempt}: expected {expected:?}, got {read:?}"
            );
        }

        // A relocation: the announced address follows the object the update named, in
        // another bucket and under another key, submitted in another spelling again.
        let moved_bucket = "gd-moved-lists";
        let moved_key = "moved/list.txt";
        let (status, updated) = call_at(
            &router,
            CREATE_ENDPOINT,
            "POST",
            &path,
            Some(json!({
                "location":
                    format!("https://{moved_bucket}.s3.amazonaws.com/{moved_key}")
            })),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the relocation expected 200, got {status} {updated:?}"
        );
        let (status, read) = call_at(&router, PUBLIC_READ_ENDPOINT, "GET", &path, None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the relocated list must read back, got {status} {read:?}"
        );
        let expected_moved = format!("{EMULATOR_BASE}/{moved_bucket}/{moved_key}");
        assert_eq!(
            read["location"].as_str(),
            Some(expected_moved.as_str()),
            "case {case:?}: expected the relocation announced as {expected_moved:?}, got \
             {read:?}"
        );

        // And nothing is announced for a list no create minted: the same read under a
        // detector id nothing minted is refused.
        let (status, refused) = call_at(
            &router,
            PUBLIC_READ_ENDPOINT,
            "GET",
            &format!("/detector/{ABSENT_DETECTOR_ID}/{segment}/{id}"),
            None,
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: a read under a detector nothing minted expected 400, got {status} \
             {refused:?}"
        );
        assert!(
            refused.get("location").is_none(),
            "case {case:?}: a refusal must announce no location, got {refused:?}"
        );
    }
}

/// The published host of this service, which is no address of this emulator's — the
/// endpoint the reads of the relocation test arrive at.
const PUBLIC_READ_ENDPOINT: &str = "guardduty.us-east-1.amazonaws.com";
