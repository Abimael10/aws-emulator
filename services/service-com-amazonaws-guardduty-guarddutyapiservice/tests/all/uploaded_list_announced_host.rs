// SPDX-License-Identifier: CC0-1.0

//! WHAT an uploaded list's `Location` reads back as, end to end over the wire.
//!
//! Every request goes through [`smithy_server_runtime::MultiServiceRouter::dispatch`]
//! (the harness lives in the `detector_lifecycle` suite), so routing, restJson1
//! (de)serialization, modelled-constraint validation and the handlers are exercised
//! together.
//!
//! The rule under test, asserted over a TABLE of inputs rather than over one recorded
//! call: a `Location` is "the URI of the file that contains the IPSet" and its
//! threat-list and entity-set siblings
//! (`smithy_query(operation="GetIPSet,GetThreatIntelSet,GetThreatEntitySet,GetTrustedEntitySet")`)
//! — the address a client dereferences to fetch the list. A read therefore reports the
//! stored URI as the writer spelled it, EXCEPT where that URI names a host this emulator
//! does not answer on, which no client of this emulator can fetch the list from: such a
//! value is announced as the same object — the bucket and key the stored URI names — at
//! the address the reading request arrived at.
//!
//! So the rows span what the rule generalises over:
//!
//! * the SPELLING of the stored URI: the `s3://` form names no host at all (its authority
//!   is the bucket), so it reads back character for character, while every object URL —
//!   path-style, regional, virtual-hosted-style, over `http` on a port of its own, and on
//!   a host that is no S3 endpoint — names a host and is re-announced;
//! * the uploaded-list FAMILY, since the rule is the family's and not the IPSet's: the
//!   IPSet, the threat list and the two entity sets each store a location;
//! * the ENDPOINT the read arrives at, which is the address announced: two addresses of
//!   this emulator (a loopback literal with a port and a loopback name with another), and
//!   this service's own published AWS host, which is no address of this emulator — a
//!   request arriving there resolves none in this harness, where nothing is bound to a
//!   socket, so there is nothing to announce and the stored URI is reported;
//! * the published `@length` bound of the member (`Location`, len 1..300), at the bound
//!   and one past it: an announced address that would not fit the member is not served.
//!
//! The stored value itself is never touched, which the suite checks by reading the same
//! list back where no address is announced and by feeding the stored URI through the
//! update that validates it.

use serde_json::{json, Value};

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

/// The endpoint authorities that are THIS emulator's own: a loopback literal with a port
/// and a loopback name with another, each the only address such a client resolves. A read
/// arriving at one of them announces an uploaded list at that very address.
const OWN_ENDPOINTS: [&str; 2] = ["127.0.0.1:39461", "localhost:4566"];

/// This service's own published AWS host. It names a service rather than this emulator,
/// and this harness binds no socket, so a read arriving there resolves no address of this
/// emulator to announce and reports the stored URI.
const PUBLIC_ENDPOINT: &str = "guardduty.us-east-1.amazonaws.com";

/// The endpoint the creates of this suite are sent to: a location is validated by its own
/// syntax, so where the create landed is no part of what is stored.
const CREATE_ENDPOINT: &str = "127.0.0.1:39461";

/// The spellings of the ONE object [`BUCKET`] and [`KEY`] name, and whether a read at an
/// address of this emulator re-announces it there: the `s3://` scheme names no host and so
/// never does, while every object URL below names a host this emulator does not answer on.
fn spellings() -> [(&'static str, String, bool); 6] {
    [
        ("the s3:// scheme", format!("s3://{BUCKET}/{KEY}"), false),
        (
            "a path-style public S3 endpoint",
            format!("https://s3.amazonaws.com/{BUCKET}/{KEY}"),
            true,
        ),
        (
            "a regional path-style public S3 endpoint",
            format!("https://s3.eu-west-1.amazonaws.com/{BUCKET}/{KEY}"),
            true,
        ),
        (
            "a virtual-hosted-style public S3 endpoint",
            format!("https://{BUCKET}.s3.us-west-2.amazonaws.com/{KEY}"),
            true,
        ),
        (
            "an object URL on a private S3 endpoint",
            format!("http://10.0.0.7:9000/{BUCKET}/{KEY}"),
            true,
        ),
        (
            "an object URL on a host that is no S3 endpoint",
            format!("https://files.example-corp.com/{BUCKET}/{KEY}"),
            true,
        ),
    ]
}

/// The address a read arriving at `endpoint` announces the object `bucket`/`key` at: the
/// path-style object URL on that endpoint, which is what a client reaches this emulator by.
fn announced_at(endpoint: &str, bucket: &str, key: &str) -> String {
    format!("https://{endpoint}/{bucket}/{key}")
}

/// A router holding one enabled detector. One per row: a detector holds at most one
/// trusted entity set and the account at most one ACTIVATED IPSet, and neither allowance
/// is this rule's business.
async fn detector() -> (Router, String) {
    let router = new_router();
    let detector_id = enabled_detector(&router).await;
    (router, detector_id)
}

/// Create a list of `segment` holding `location`, returning the id the create reported.
async fn create_list(
    router: &Router,
    detector_id: &str,
    segment: &str,
    id_member: &str,
    format: &str,
    location: &str,
    case: &str,
) -> String {
    let (status, created) = call_at(
        router,
        CREATE_ENDPOINT,
        "POST",
        &format!("/detector/{detector_id}/{segment}"),
        Some(json!({
            "name": "announced-list",
            "format": format,
            "location": location,
            "activate": false
        })),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the create to be served, got {status} {created:?}"
    );
    created[id_member]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected an id, got {created:?}"))
        .to_string()
}

/// Read a list back at `endpoint`, asserting the read is served.
async fn read_list(router: &Router, endpoint: &str, path: &str, case: &str) -> Value {
    let (status, body) = call_at(router, endpoint, "GET", path, None).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the stored list must read back, got {status} {body:?}"
    );
    body
}

#[tokio::test]
async fn a_location_is_announced_at_the_address_the_read_arrived_at() {
    for (segment, id_member, format) in FAMILIES {
        for (why, location, reannounced) in spellings() {
            let (router, detector_id) = detector().await;
            let create_case = format!("{segment}: {why}");
            let id = create_list(
                &router,
                &detector_id,
                segment,
                id_member,
                format,
                &location,
                &create_case,
            )
            .await;
            let path = format!("/detector/{detector_id}/{segment}/{id}");

            for endpoint in OWN_ENDPOINTS {
                let case = format!("{create_case} read at {endpoint}");
                let expected = match reannounced {
                    true => announced_at(endpoint, BUCKET, KEY),
                    false => location.clone(),
                };
                // Twice at each address, with nothing in between: a read reports, so a
                // later caller sees exactly what the first one saw.
                for attempt in 1..=2 {
                    let read = read_list(&router, endpoint, &path, &case).await;
                    let reported = read["location"].as_str().unwrap_or_default();
                    assert_eq!(
                        reported, expected,
                        "case {case:?} read {attempt}: expected {expected:?}, got {reported:?}"
                    );
                    assert!(
                        reported.chars().count() <= 300,
                        "case {case:?} read {attempt}: the member's published bound is 300 \
                         characters, got {} in {reported:?}",
                        reported.chars().count()
                    );
                }
            }

            // No address of this emulator resolves for a read that arrived at this
            // service's published host, so there is nothing to announce: the STORED URI
            // is reported, which is also what shows the reads above wrote nothing.
            let case = format!("{create_case} read at {PUBLIC_ENDPOINT}");
            let read = read_list(&router, PUBLIC_ENDPOINT, &path, &case).await;
            let reported = read["location"].as_str().unwrap_or_default();
            assert_eq!(
                reported, location,
                "case {case:?}: expected the stored URI {location:?}, got {reported:?}"
            );

            // And the stored value is still the caller's own, so the update that
            // validates a `Location` accepts it unchanged.
            let (status, updated) = call_at(
                &router,
                CREATE_ENDPOINT,
                "POST",
                &path,
                Some(json!({"location": reported})),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {create_case:?}: writing the stored URI back expected 200, got {status} \
                 {updated:?}"
            );
        }
    }
}

/// The member's published bound, `@length` 1..300 (`smithy_query(shape="Location")`): an
/// address this emulator would announce is served only while it fits that bound, and the
/// stored URI — accepted under the same bound — is reported when it would not.
#[tokio::test]
async fn an_announced_address_past_the_published_bound_falls_back_to_the_stored_uri() {
    /// The upper bound the model declares for the `Location` shape.
    const MAX_LOCATION: usize = 300;
    /// A short path-style S3 endpoint host, so the announced authority is the LONGER of
    /// the two and the rewrite can cross the bound.
    const SHORT_S3_HOST: &str = "s3.cn";

    let endpoint = OWN_ENDPOINTS[0];
    // `https://` + authority + `/` + bucket + `/` + key, the announced form.
    let announced_overhead = "https://".len() + endpoint.len() + 2 + BUCKET.len();
    for (why, key_length, announced) in [
        ("exactly at the bound", MAX_LOCATION - announced_overhead, true),
        (
            "one character past the bound",
            MAX_LOCATION - announced_overhead + 1,
            false,
        ),
    ] {
        for (segment, id_member, format) in FAMILIES {
            let (router, detector_id) = detector().await;
            let case = format!("{segment}: an announced address {why}");
            let key = "k".repeat(key_length);
            let location = format!("https://{SHORT_S3_HOST}/{BUCKET}/{key}");
            assert!(
                location.chars().count() <= MAX_LOCATION,
                "case {case:?}: the row's own stored URI must be within the bound, got {}",
                location.chars().count()
            );

            let id = create_list(
                &router,
                &detector_id,
                segment,
                id_member,
                format,
                &location,
                &case,
            )
            .await;
            let path = format!("/detector/{detector_id}/{segment}/{id}");
            let expected = match announced {
                true => announced_at(endpoint, BUCKET, &key),
                false => location.clone(),
            };
            assert_eq!(
                announced,
                announced_at(endpoint, BUCKET, &key).chars().count() <= MAX_LOCATION,
                "case {case:?}: the row must describe its own announced length"
            );

            let read = read_list(&router, endpoint, &path, &case).await;
            let reported = read["location"].as_str().unwrap_or_default();
            assert_eq!(
                reported, expected,
                "case {case:?}: expected {expected:?}, got {reported:?}"
            );
            assert!(
                reported.chars().count() <= MAX_LOCATION,
                "case {case:?}: expected a value within {MAX_LOCATION} characters, got {}",
                reported.chars().count()
            );
        }
    }
}

/// A read is a report: a relocation reads back as the object the UPDATE named — announced
/// at the address the read arrived at, stored as the caller wrote it — and a read under a
/// detector nothing minted is refused rather than answered a location.
#[tokio::test]
async fn a_relocation_reads_back_as_the_object_the_update_named() {
    for (segment, id_member, format) in FAMILIES {
        let (router, detector_id) = detector().await;
        let case = segment;
        let created_location = format!("s3://{BUCKET}/{KEY}");
        let id = create_list(
            &router,
            &detector_id,
            segment,
            id_member,
            format,
            &created_location,
            case,
        )
        .await;
        let path = format!("/detector/{detector_id}/{segment}/{id}");

        // The `s3://` URI names no host, so it reads back as itself wherever the read
        // arrives.
        for endpoint in [OWN_ENDPOINTS[0], PUBLIC_ENDPOINT] {
            let read = read_list(&router, endpoint, &path, case).await;
            assert_eq!(
                read["location"].as_str(),
                Some(created_location.as_str()),
                "case {case:?} read at {endpoint}: expected {created_location:?}, got {read:?}"
            );
        }

        // A relocation, in another bucket and under another key, submitted as a
        // virtual-hosted-style object URL on a public S3 host.
        let moved_bucket = "gd-moved-lists";
        let moved_key = "moved/list.txt";
        let moved = format!("https://{moved_bucket}.s3.amazonaws.com/{moved_key}");
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
        let read = read_list(&router, OWN_ENDPOINTS[1], &path, case).await;
        let expected = announced_at(OWN_ENDPOINTS[1], moved_bucket, moved_key);
        assert_eq!(
            read["location"].as_str(),
            Some(expected.as_str()),
            "case {case:?}: expected the relocated object announced as {expected:?}, got {read:?}"
        );
        let stored = read_list(&router, PUBLIC_ENDPOINT, &path, case).await;
        assert_eq!(
            stored["location"].as_str(),
            Some(moved.as_str()),
            "case {case:?}: expected the stored URI {moved:?}, got {stored:?}"
        );

        // And nothing is reported for a list no create minted: the same read under a
        // detector id nothing minted is refused.
        let (status, refused) = call_at(
            &router,
            PUBLIC_ENDPOINT,
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
