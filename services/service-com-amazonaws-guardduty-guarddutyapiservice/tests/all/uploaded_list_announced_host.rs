// SPDX-License-Identifier: CC0-1.0

//! WHAT an uploaded list's `Location` reads back as, end to end over the wire.
//!
//! Every request goes through [`smithy_server_runtime::MultiServiceRouter::dispatch`]
//! (the harness lives in the `detector_lifecycle` suite), so routing, restJson1
//! (de)serialization, modelled-constraint validation and the handlers are exercised
//! together.
//!
//! The rule under test, asserted over a TABLE of inputs rather than over one recorded
//! call: a `Location` is the URI of the file an uploaded list was uploaded as, submitted by
//! the account, so a read reports that URI character for character — in the spelling the
//! writer chose, scheme and host included. It is a record of what was configured, not an
//! address this service mints, so nothing about the request reading it takes part in the
//! answer.
//!
//! The rows therefore span what the rule generalises over:
//!
//! * the SPELLING of the submitted URI — every S3 URL form is kept as written, so no two
//!   rows read back as one value;
//! * the uploaded-list FAMILY, since the rule is the family's and not the IPSet's: the
//!   IPSet, the threat list and the two entity sets each store a location;
//! * the ENDPOINT the read arrives at, which takes no part in the answer: a client
//!   configured with the emulator's own endpoint and a caller that resolves this service's
//!   own published host are told the same URI.

use serde_json::json;

use crate::detector_lifecycle::{
    call_at, enabled_detector, new_router, Router, ABSENT_DETECTOR_ID,
};

/// The bucket every row's location names.
const BUCKET: &str = "gd-uploaded-lists";

/// The object key every row's location names, deep enough that the whole path and not
/// just a last segment has to survive the read-back.
const KEY: &str = "lists/2026/trusted.txt";

/// One uploaded-list family: the route segment its sub-resource hangs off, the `jsonName`
/// of the id the create reports and the `Format` value a create needs. The `Format`
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
/// object URL on a host that is no S3 endpoint at all. Each addresses the same file, and
/// each reads back as the URI it was written as.
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

/// The endpoint authorities every row is read at: the emulator's own endpoint as a loopback
/// literal with a port and by its loopback name on another, each the only address such a
/// client resolves, and this service's own published host. None of them takes part in the
/// answer.
const READ_ENDPOINTS: [&str; 3] = [
    "127.0.0.1:39461",
    "localhost:4566",
    "guardduty.us-east-1.amazonaws.com",
];

/// The endpoint the creates of this suite are sent to: a location is read by its own
/// syntax, so where the create landed is no part of what a read reports either.
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
async fn an_uploaded_lists_location_reads_back_as_the_uri_that_was_stored() {
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

            for endpoint in READ_ENDPOINTS {
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
                let reported = read["location"].as_str().unwrap_or_default();
                assert_eq!(
                    reported, location,
                    "case {case:?}: expected the stored URI {location:?}, got {reported:?}"
                );
            }
        }
    }
}

/// The read is a report, so it changes nothing: a second read answers the same URI, a
/// relocation reads back as the URI the UPDATE named, and a read under a detector nothing
/// minted is still refused rather than answered a location.
#[tokio::test]
async fn a_relocation_reads_back_as_the_uri_the_update_named() {
    for (segment, id_member, format) in FAMILIES {
        let (router, detector_id) = detector().await;
        let case = segment;
        let created_location = format!("s3://{BUCKET}/{KEY}");
        let (status, created) = call_at(
            &router,
            CREATE_ENDPOINT,
            "POST",
            &format!("/detector/{detector_id}/{segment}"),
            Some(json!({
                "name": "relocated-list",
                "format": format,
                "location": created_location,
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

        // Twice, with nothing in between: a read reports and does not re-address.
        for attempt in 1..=2 {
            let (status, read) = call_at(&router, PUBLIC_READ_ENDPOINT, "GET", &path, None).await;
            assert_eq!(
                status, 200,
                "case {case:?} read {attempt}: expected 200, got {status} {read:?}"
            );
            assert_eq!(
                read["location"].as_str(),
                Some(created_location.as_str()),
                "case {case:?} read {attempt}: expected {created_location:?}, got {read:?}"
            );
        }

        // A relocation: the read follows the URI the update named, in another bucket and
        // under another key, submitted in another spelling again.
        let moved = "https://gd-moved-lists.s3.amazonaws.com/moved/list.txt";
        let (status, updated) = call_at(
            &router,
            CREATE_ENDPOINT,
            "POST",
            &path,
            Some(json!({"location": moved})),
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
        assert_eq!(
            read["location"].as_str(),
            Some(moved),
            "case {case:?}: expected the relocation read back as {moved:?}, got {read:?}"
        );

        // And nothing is reported for a list no create minted: the same read under a
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
            "case {case:?}: a refusal must report no location, got {refused:?}"
        );
    }
}

/// The published host of this service — the endpoint the reads of the relocation test
/// arrive at, which takes no part in what they report.
const PUBLIC_READ_ENDPOINT: &str = "guardduty.us-east-1.amazonaws.com";
