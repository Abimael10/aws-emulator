// SPDX-License-Identifier: CC0-1.0

//! The ROUND TRIP of an uploaded list's `Location`, end to end over the wire.
//!
//! Every request goes through [`smithy_server_runtime::MultiServiceRouter::dispatch`]
//! (the harness lives in the `detector_lifecycle` suite), so routing, restJson1
//! (de)serialization, modelled-constraint validation and the handlers are exercised
//! together.
//!
//! The rule under test, asserted over a TABLE of inputs rather than over one recorded
//! call: an uploaded list's `Location` is a required output member with a `len 1..300`,
//! no pattern, no default and no trait that licenses re-addressing it
//! (`smithy_query(operation="GetIPSet,GetThreatIntelSet,GetThreatEntitySet,GetTrustedEntitySet")`),
//! so a read-back reports the URI the create or the update STORED, character for character,
//! whichever endpoint the read arrives at. Which means the value a read returns is a value
//! the matching update accepts again: a describe-then-apply reconciler that writes back
//! what it read changes nothing.
//!
//! The rows span what the rule generalises over:
//!
//! * the uploaded-list FAMILY, because the rule is the family's and not one operation's:
//!   the IPSet, the threat list and the two entity sets each store a `Location`;
//! * the SHAPE of the URI — the depth of the key, the characters a bucket and a key
//!   carry, percent-encoding, letter case in the key and in the scheme — since none of
//!   that is any part of what the member reports;
//! * the ENDPOINT the read arrives at, which decides nothing;
//! * the published `@length` bound of the member, at the bound and one past it.
//!
//! The SPELLINGS of the stored URI — the `s3://` form beside the path-style and
//! virtual-hosted-style object URLs — are the `uploaded_list_announced_host` suite's table;
//! the rows here hold the `s3://` form and vary the object it names, and own the write-back
//! and the length bound.

use serde_json::{json, Value};

use crate::detector_lifecycle::{call_at, enabled_detector, new_router, Router};

/// One uploaded-list family: the route segment its sub-resource hangs off, the `jsonName`
/// of the id its create reports and the `Format` value a create needs. The `Format` values
/// are members of `IpSetFormat`, `ThreatIntelSetFormat`, `ThreatEntitySetFormat` and
/// `TrustedEntitySetFormat`
/// (`smithy_query(shape="IpSetFormat,ThreatIntelSetFormat,ThreatEntitySetFormat,TrustedEntitySetFormat")`).
struct Family {
    segment: &'static str,
    id_member: &'static str,
    format: &'static str,
}

/// The four uploaded-list families, each storing a `Location` of its own.
const FAMILIES: [Family; 4] = [
    Family {
        segment: "ipset",
        id_member: "ipSetId",
        format: "TXT",
    },
    Family {
        segment: "threatintelset",
        id_member: "threatIntelSetId",
        format: "STIX",
    },
    Family {
        segment: "threatentityset",
        id_member: "threatEntitySetId",
        format: "TXT",
    },
    Family {
        segment: "trustedentityset",
        id_member: "trustedEntitySetId",
        format: "TXT",
    },
];

/// The endpoint the creates of this suite are sent to: where a request landed is no part
/// of what a read reports.
const CREATE_ENDPOINT: &str = "127.0.0.1:39461";

/// The endpoint authorities every row is read at: this emulator's own endpoint as a
/// loopback literal with a port and by a loopback name with another port, each the only
/// address such a client resolves, and this service's own published host, which is no
/// address of this emulator's.
fn read_endpoints() -> [&'static str; 3] {
    [
        "127.0.0.1:39461",
        "localhost:4566",
        "guardduty.us-east-1.amazonaws.com",
    ]
}

/// The stored `s3://` URIs, each differing from its neighbours in what the rule
/// generalises over: the depth of the object key, the characters a bucket name and a key
/// carry (dashes, dots, digits), percent-encoding in the key, letter case in the key, and
/// the case of the scheme itself — a URI scheme is case-insensitive, so an upper-case one
/// addresses the same object and is still the caller's own string, reported as written.
fn stored_locations() -> Vec<(&'static str, String)> {
    vec![
        ("a single-segment key", "s3://gd-lists/list.txt".to_string()),
        (
            "a deep key",
            "s3://gd-lists/lists/2026/03/01/trusted.txt".to_string(),
        ),
        (
            "a bucket carrying dots, dashes and digits",
            "s3://gd.probe-lists.2026/malicious-ips.txt".to_string(),
        ),
        (
            "a percent-encoded space in the key",
            "s3://gd-lists/lists/my%20list.txt".to_string(),
        ),
        (
            "mixed case and dots in the key",
            "s3://gd-lists/Lists/v1.2/IPs.TXT".to_string(),
        ),
        (
            "an upper-case scheme",
            "S3://gd-lists/list.txt".to_string(),
        ),
    ]
}

/// A router holding one enabled detector. One per row: a detector holds at most one
/// trusted entity set and the account at most one ACTIVATED IPSet, and neither allowance
/// is this rule's business.
async fn detector() -> (Router, String) {
    let router = new_router();
    let detector_id = enabled_detector(&router).await;
    (router, detector_id)
}

/// Create a list of `segment` holding `location`, and return the id the create reported.
/// `activate` is false throughout: the activation allowances are other suites' rules.
async fn create_list(
    router: &Router,
    detector_id: &str,
    family: &Family,
    name: &str,
    location: &str,
    case: &str,
) -> String {
    let (status, created) = call_at(
        router,
        CREATE_ENDPOINT,
        "POST",
        &format!("/detector/{detector_id}/{}", family.segment),
        Some(json!({
            "name": name,
            "format": family.format,
            "location": location,
            "activate": false
        })),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the create to be served, got {status} {created:?}"
    );
    created[family.id_member]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected an id, got {created:?}"))
        .to_string()
}

/// Read the list back at `endpoint`, asserting the read is served.
async fn read_list(router: &Router, endpoint: &str, path: &str, case: &str) -> Value {
    let (status, body) = call_at(router, endpoint, "GET", path, None).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the stored list must read back, got {status} {body:?}"
    );
    body
}

#[tokio::test]
async fn a_stored_s3_uri_reads_back_as_stored_at_every_endpoint() {
    for family in &FAMILIES {
        for (why, location) in stored_locations() {
            let (router, detector_id) = detector().await;
            let case = format!("{}: {why}", family.segment);
            let id = create_list(
                &router,
                &detector_id,
                family,
                "round-trip-list",
                &location,
                &case,
            )
            .await;
            let path = format!("/detector/{detector_id}/{}/{id}", family.segment);

            for endpoint in read_endpoints() {
                let case = format!("{case} read at {endpoint}");
                // Twice at each endpoint, with nothing in between: a read reports and
                // neither re-addresses nor changes what a later caller sees.
                for attempt in 1..=2 {
                    let read = read_list(&router, endpoint, &path, &case).await;
                    let served = read["location"].as_str().unwrap_or_default();
                    assert_eq!(
                        served, location,
                        "case {case:?} read {attempt}: expected the stored URI \
                         {location:?}, got {served:?}"
                    );
                }
            }
        }
    }
}

#[tokio::test]
async fn the_location_a_read_returns_is_accepted_back_by_the_update() {
    for family in &FAMILIES {
        for (why, location) in stored_locations() {
            let (router, detector_id) = detector().await;
            let case = format!("{}: {why}", family.segment);
            let id = create_list(
                &router,
                &detector_id,
                family,
                "reconciled-list",
                &location,
                &case,
            )
            .await;
            let path = format!("/detector/{detector_id}/{}/{id}", family.segment);

            let read = read_list(&router, read_endpoints()[0], &path, &case).await;
            let served = read["location"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: expected a location, got {read:?}"))
                .to_string();

            // The value the read handed back, written straight back: the update the
            // service's own output feeds has nothing to refuse.
            let (status, updated) = call_at(
                &router,
                read_endpoints()[1],
                "POST",
                &path,
                Some(json!({"location": served})),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: writing back {served:?} expected 200, got {status} {updated:?}"
            );

            let after = read_list(&router, read_endpoints()[2], &path, &case).await;
            let again = after["location"].as_str().unwrap_or_default();
            assert_eq!(
                again, location,
                "case {case:?}: writing back the read value must change nothing, expected \
                 {location:?}, got {again:?}"
            );
        }
    }
}

/// The `Location` member's published bound, `@length` 1..300
/// (`smithy_query(shape="Location")`): a URI of exactly 300 characters round-trips whole,
/// and one character past the bound is refused with HTTP 400 and stores no list.
#[tokio::test]
async fn a_location_at_the_published_length_bound_round_trips_and_one_past_it_is_refused() {
    /// The upper bound the model declares for the `Location` shape.
    const MAX_LOCATION: usize = 300;

    for family in &FAMILIES {
        let prefix = "s3://gd-lists/";
        for (why, length, accepted) in [
            ("one inside the bound", MAX_LOCATION - 1, true),
            ("at the bound", MAX_LOCATION, true),
            ("one past the bound", MAX_LOCATION + 1, false),
        ] {
            let (router, detector_id) = detector().await;
            let case = format!(
                "{}: a location {why} ({length} characters)",
                family.segment
            );
            let location = format!("{prefix}{}", "k".repeat(length - prefix.len()));
            assert_eq!(
                location.len(),
                length,
                "case {case:?}: the row must send {length} characters, got {}",
                location.len()
            );

            if accepted {
                let id = create_list(
                    &router,
                    &detector_id,
                    family,
                    "bounded-list",
                    &location,
                    &case,
                )
                .await;
                let path = format!("/detector/{detector_id}/{}/{id}", family.segment);
                let read = read_list(&router, read_endpoints()[0], &path, &case).await;
                let served = read["location"].as_str().unwrap_or_default();
                assert_eq!(
                    served, location,
                    "case {case:?}: expected the whole stored URI back, got {served:?}"
                );
                continue;
            }

            let (status, refused) = call_at(
                &router,
                CREATE_ENDPOINT,
                "POST",
                &format!("/detector/{detector_id}/{}", family.segment),
                Some(json!({
                    "name": "over-bound-list",
                    "format": family.format,
                    "location": location,
                    "activate": false
                })),
            )
            .await;
            assert_eq!(
                status, 400,
                "case {case:?}: expected HTTP 400, got {status} {refused:?}"
            );
            let (status, listed) = call_at(
                &router,
                CREATE_ENDPOINT,
                "GET",
                &format!("/detector/{detector_id}/{}", family.segment),
                None,
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: the listing must be served, got {status} {listed:?}"
            );
            let ids = listed[format!("{}s", family.id_member)]
                .as_array()
                .cloned()
                .unwrap_or_default();
            assert!(
                ids.is_empty(),
                "case {case:?}: a refused create must store no list, got {ids:?}"
            );
        }
    }
}
