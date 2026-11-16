// SPDX-License-Identifier: CC0-1.0

//! The address `GetWorkload` publishes for a catalogue entry's ICON
//! (`WorkloadData$iconUrl`), and the image this emulator serves there.
//!
//! The rule under test is not about one entry or one host: an absolute URL this emulator
//! hands a client for an artifact of its OWN addresses the endpoint that client is already
//! talking to, at a path built from the record being reported, and the emulator answers a
//! fetch of that address with the artifact. A URL naming any other host leaves the
//! emulator — with egress blocked the fetch fails, with egress open it reaches content the
//! emulator never served — so no test, and no console-like client, can rely on it.
//!
//! Every assertion therefore runs over a TABLE whose rows differ along what the rule
//! generalises over: the catalogue ENTRY whose icon is addressed (every entry the
//! catalogue reports, not the one the report cited) and the ENDPOINT this emulator is
//! published on (the default one and a published one), which is what the host of the
//! address has to follow.

use std::collections::BTreeSet;

use serde_json::json;

use crate::harness::{catalogue, Fetched, Harness};
use service_com_amazonaws_launchwizard_launchwizard::state::workload_icons::{
    icon_path, ICON_TOKEN_FIELD,
};

/// The endpoint an emulator that is not served on a port is reached at, which is the one
/// the harness's own router publishes (`smithy_server_runtime::UNSERVED_URL_BASE`).
const DEFAULT_ENDPOINT: &str = smithy_server_runtime::UNSERVED_URL_BASE;

/// The endpoints this suite publishes the emulator on, beside the default one: a host and
/// port no part of this service knows about, so an address that merely looked right by
/// coincidence cannot pass.
const PUBLISHED_ENDPOINTS: [&str; 2] = [
    "https://launch-wizard.emulator.test:9443",
    "https://127.0.0.1:18443",
];

/// The eight bytes every PNG file starts with (the PNG signature, RFC 2083 section 3.1).
const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

/// The media type an icon is served under.
const PNG_MEDIA_TYPE: &str = "image/png";

/// `WorkloadData$iconUrl` as `GetWorkload` reports it for `name`, required to be a
/// non-empty string.
async fn icon_url(harness: &Harness, case: &str, name: &str) -> String {
    let body = harness
        .ok(case, "/getWorkload", json!({"workloadName": name}))
        .await;
    let reported = body["workload"]["iconUrl"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        !reported.is_empty(),
        "case {case:?}: GetWorkload must report an iconUrl, got {body}"
    );
    reported
}

/// The origin of an absolute URL (`scheme://host[:port]`), and the rest of it.
fn split_origin(url: &str) -> (String, String) {
    let after_scheme = url.find("://").map_or(0, |at| at + 3);
    let rest_at = url[after_scheme..]
        .find('/')
        .map_or(url.len(), |at| after_scheme + at);
    (url[..rest_at].to_string(), url[rest_at..].to_string())
}

/// The path of an absolute URL, without the query a signature travels in.
fn path_of(url: &str) -> String {
    let (_, rest) = split_origin(url);
    rest.split('?').next().unwrap_or_default().to_string()
}

/// Fetch `url` and require an icon: HTTP 200, the PNG media type, and a body that decodes
/// as a square 8-bit RGB PNG with pixels. Hands back the exact bytes served.
async fn fetched_icon(harness: &Harness, case: &str, url: &str) -> Vec<u8> {
    let served = harness.fetch(url).await;
    assert_eq!(
        served.status, 200,
        "case {case:?}: following iconUrl {url:?} must serve the icon, got HTTP {} with {:?}",
        served.status,
        body_text(&served)
    );
    assert_eq!(
        served.content_type.as_deref(),
        Some(PNG_MEDIA_TYPE),
        "case {case:?}: the icon must be served as {PNG_MEDIA_TYPE}, got {:?}",
        served.content_type
    );
    assert_eq!(
        served.body.get(..PNG_SIGNATURE.len()),
        Some(&PNG_SIGNATURE[..]),
        "case {case:?}: the served icon must be a PNG image, got {:?}",
        served.body.get(..PNG_SIGNATURE.len())
    );
    // A real image, not bytes that merely start like one: the decoder reads the header the
    // service's encoder wrote.
    let decoded = png::Decoder::new(std::io::Cursor::new(&served.body))
        .read_info()
        .unwrap_or_else(|error| panic!("case {case:?}: expected a decodable PNG: {error}"));
    let info = decoded.info();
    assert!(
        info.width > 0 && info.width == info.height,
        "case {case:?}: expected a square icon with pixels, got {}x{}",
        info.width,
        info.height
    );
    served.body
}

/// What a non-image answer said, for an assertion message.
fn body_text(served: &Fetched) -> String {
    String::from_utf8_lossy(&served.body).to_string()
}

#[tokio::test]
async fn every_entrys_icon_is_addressed_on_the_endpoint_the_client_called() {
    // One row per published endpoint, including the default one the harness carries, so
    // what the host of the address follows is the endpoint rather than a fixed name.
    let mut harnesses: Vec<(String, Harness)> =
        vec![(DEFAULT_ENDPOINT.to_string(), Harness::new())];
    for endpoint in PUBLISHED_ENDPOINTS {
        harnesses.push((endpoint.to_string(), Harness::published_at(endpoint)));
    }

    for (endpoint, harness) in &harnesses {
        let names = catalogue(harness).await;
        assert!(
            names.len() >= 2,
            "the catalogue must hold more than one workload for this table to span it, \
             got {names:?}"
        );
        let mut paths: BTreeSet<String> = BTreeSet::new();
        for name in &names {
            let case = format!("{endpoint}/{name}");
            let url = icon_url(harness, &case, name).await;
            let (origin, rest) = split_origin(&url);
            assert_eq!(
                &origin, endpoint,
                "case {case:?}: iconUrl must address the endpoint the client called, got {url:?}"
            );
            // The entry's own part of the address, and the signature that makes it an
            // address this emulator issued rather than a path a client may compose.
            assert!(
                rest.contains(name),
                "case {case:?}: iconUrl must name the entry whose icon it is, got {url:?}"
            );
            assert!(
                rest.contains("X-Amz-Signature=") && rest.contains("X-Amz-Credential="),
                "case {case:?}: iconUrl must be an address this emulator signed, got {url:?}"
            );
            paths.insert(path_of(&url));
        }
        // Each entry's icon has its OWN address, so the catalogue tells its entries apart
        // rather than pointing every one at a single image.
        assert_eq!(
            paths.len(),
            names.len(),
            "case {endpoint:?}: every entry must address an icon of its own, got {paths:?}"
        );
    }
}

#[tokio::test]
async fn following_an_entrys_icon_address_serves_that_entrys_icon() {
    let harness = Harness::published_at(PUBLISHED_ENDPOINTS[0]);
    let names = catalogue(&harness).await;
    // One row per catalogue entry: the rule is that the address leads to the artifact, so
    // it holds for every entry and not only for the one the report cited.
    let mut images: BTreeSet<Vec<u8>> = BTreeSet::new();
    for name in &names {
        let url = icon_url(&harness, name, name).await;
        let image = fetched_icon(&harness, name, &url).await;
        // Fetching the artifact changes nothing: the same address serves the same image,
        // and reading it did not disturb the catalogue entry it belongs to.
        let again = fetched_icon(&harness, &format!("{name} again"), &url).await;
        assert_eq!(
            again, image,
            "case {name:?}: the same icon address must serve the same image"
        );
        images.insert(image);
    }
    assert_eq!(
        images.len(),
        names.len(),
        "every entry's icon must be its own image, got {} distinct images for {} entries",
        images.len(),
        names.len()
    );
}

#[tokio::test]
async fn an_address_naming_no_catalogue_entry_serves_no_icon() {
    let harness = Harness::new();
    let names = catalogue(&harness).await;
    let template = icon_url(&harness, "template", &names[0]).await;
    // Rows differ along HOW the address fails to name an entry: a name no entry carries, a
    // name that differs from a real one by case, and the name of a deployment pattern of a
    // real entry (a name the service knows, but not as a workload).
    for unknown in ["NoSuchWorkload", "sap", "SapHanaSingle"] {
        let url = template.replace(&format!("/{}/", names[0]), &format!("/{unknown}/"));
        assert_ne!(
            url, template,
            "case {unknown:?}: the test must address a different entry than {:?}",
            names[0]
        );
        let served = harness.fetch(&url).await;
        assert_eq!(
            served.status, 404,
            "case {unknown:?}: an address naming no catalogue entry must serve nothing, got \
             HTTP {} with {:?}",
            served.status,
            body_text(&served)
        );
        assert_ne!(
            served.content_type.as_deref(),
            Some(PNG_MEDIA_TYPE),
            "case {unknown:?}: a refusal must not be served as an image, got {:?}",
            body_text(&served)
        );
    }
}

#[tokio::test]
async fn an_address_whose_authenticator_this_store_did_not_mint_serves_no_icon() {
    let harness = Harness::new();
    let names = catalogue(&harness).await;
    assert!(
        names.len() >= 2,
        "this table needs two catalogue entries to cross their addresses, got {names:?}"
    );
    let first = icon_url(&harness, "first", &names[0]).await;
    let second = icon_url(&harness, "second", &names[1]).await;
    // The icon the first entry's address leads to, so the rows below are measured against a
    // fetch that DOES work.
    let served = fetched_icon(&harness, "first", &first).await;
    assert!(!served.is_empty(), "the icon served must carry bytes");

    // Rows differ along what the address carries in place of the authenticator this store
    // minted for THIS entry: nothing, a value of a minted one's shape, a value that is not
    // hex at all, and the authenticator this store minted for ANOTHER entry.
    let token = query_field(&first, ICON_TOKEN_FIELD)
        .unwrap_or_else(|| panic!("an icon address must carry its {ICON_TOKEN_FIELD}: {first:?}"));
    let other = query_field(&second, ICON_TOKEN_FIELD)
        .unwrap_or_else(|| panic!("an icon address must carry its {ICON_TOKEN_FIELD}: {second:?}"));
    let same_shape = "ab".repeat(token.len() / 2);
    for (case, forged) in [
        ("no authenticator", String::new()),
        ("a value of the same shape", same_shape),
        ("a value that is not hex", "not-a-token".to_string()),
        ("the authenticator of another entry", other),
    ] {
        let url = first.replace(
            &format!("{ICON_TOKEN_FIELD}={token}"),
            &format!("{ICON_TOKEN_FIELD}={forged}"),
        );
        assert_ne!(
            url, first,
            "case {case:?}: the row must change the address's {ICON_TOKEN_FIELD}"
        );
        let answered = harness.fetch(&url).await;
        assert_eq!(
            answered.status, 404,
            "case {case:?}: an address this store did not mint must serve nothing, got HTTP {} \
             with {:?}",
            answered.status,
            body_text(&answered)
        );
        assert_ne!(
            answered.content_type.as_deref(),
            Some(PNG_MEDIA_TYPE),
            "case {case:?}: a refusal must not be served as an image, got {:?}",
            body_text(&answered)
        );
    }
}

#[test]
fn an_icon_address_path_round_trips_a_catalogue_key_and_reads_back_nothing_else() {
    // The address layout is one statement, written by `icon_path` and read back by
    // `workload_in_icon_path`: what it round-trips is a `WorkloadName` (len 1..100,
    // `^[A-Za-z][a-zA-Z0-9-_]*$`, `smithy_query(query="shape", shape="WorkloadName")`), which
    // is one URL path segment carrying no character a URL encodes. Rows differ along the FORM
    // of the label: keys of that shape, and labels a caller could compose that are not one —
    // path and query characters, a traversal step, an empty label, a leading digit, and one
    // past the length ceiling.
    let over_the_ceiling = format!("A{}", "b".repeat(100));
    let round_trips = [
        "SAP",
        "SO0023",
        "a",
        "Mixed-Name_1",
        &"A".repeat(100),
        &"Z-_9".repeat(2),
    ];
    for name in round_trips {
        let read_back = workload_in_icon_path(&path_segments(&icon_path(name)));
        assert_eq!(
            read_back.as_deref(),
            Some(name),
            "case {name:?}: the address layout must read back the key it was built from, got \
             {read_back:?}"
        );
    }
    for label in [
        "SAP/extra",
        "..",
        "../SAP",
        "",
        "9SAP",
        "SAP?x=1",
        "SAP#frag",
        "SAP icon",
        &over_the_ceiling,
    ] {
        let read_back = workload_in_icon_path(&path_segments(&icon_path(label)));
        assert_eq!(
            read_back, None,
            "case {label:?}: a label that is no catalogue key must read back as no address, \
             got {read_back:?}"
        );
    }
}

/// The decoded path segments of `path`, as the serving route reads them off a request.
fn path_segments(path: &str) -> Vec<String> {
    path.strip_prefix('/')
        .unwrap_or(path)
        .split('/')
        .map(ToString::to_string)
        .collect()
}

/// `workload_in_icon_path` over owned segments, as an owned answer.
fn workload_in_icon_path(segments: &[String]) -> Option<String> {
    service_com_amazonaws_launchwizard_launchwizard::state::workload_icons::workload_in_icon_path(
        segments,
    )
    .map(ToString::to_string)
}

/// The value of one query field of an absolute URL, or `None` when it carries no such field.
fn query_field(url: &str, field: &str) -> Option<String> {
    let query = url.split_once('?').map(|(_, query)| query)?;
    query.split('&').find_map(|pair| {
        let (name, value) = pair.split_once('=')?;
        (name == field).then(|| value.to_string())
    })
}

#[tokio::test]
async fn an_icon_path_without_the_address_the_emulator_issued_is_not_served() {
    let harness = Harness::new();
    let names = catalogue(&harness).await;
    let url = icon_url(&harness, "issued", &names[0]).await;
    let (origin, rest) = split_origin(&url);
    let path = rest.split('?').next().unwrap_or_default().to_string();
    // Rows differ along what is left of the address the emulator issued: the bare path,
    // and the path carrying only the credential half of the signature. An icon has exactly
    // one address — the one a catalogue read handed out — so neither is served an image.
    for (case, query) in [("bare path", ""), ("credential only", "?X-Amz-Credential=x")] {
        let composed = format!("{origin}{path}{query}");
        let served = harness.fetch(&composed).await;
        assert_ne!(
            served.content_type.as_deref(),
            Some(PNG_MEDIA_TYPE),
            "case {case:?}: {composed:?} is not an address this emulator issued, so it must \
             not be served an image, got HTTP {}",
            served.status
        );
        assert_ne!(
            served.status, 200,
            "case {case:?}: {composed:?} is not an address this emulator issued, so it must \
             not be answered 200, got {:?}",
            body_text(&served)
        );
    }
}
