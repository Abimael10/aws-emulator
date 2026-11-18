// SPDX-License-Identifier: CC0-1.0

//! A PROBE of what a real SOCKET delivers for a published repository logo, as opposed to
//! what this service states for it in process (`repository_logo_asset.rs`, the other half
//! of the same rule).
//!
//! `RepositoryCatalogData$logoUrl` is an address this emulator hands a client to fetch
//! LATER, with a plain HTTP client: a browser `<img>`, a gallery preview, a publish
//! pipeline validating the asset it published. What such a client is served is decided by
//! the whole HTTP stack below this crate — the response this service states, then the
//! HTTP/1.1 writer that puts it on the wire — so the status line, the headers and the
//! bytes a socket really delivers are pinned here, in the response text itself, rather
//! than asserted against the in-process response the sibling suite reads.
//!
//! Both addresses the asset is reachable at are probed, because they are written by two
//! different encoders: the ADDRESS THE URL NAMES, which is the socket this service opens
//! for its asset plane (`state::logo_socket`, whose encoder writes a field name in the
//! canonical spelling a client reading the media type looks for), and the emulator's own
//! front door, which claims the same path and writes every field name normalised to lower
//! case.

use serde_json::json;

use crate::repository_image_lifecycle::{ok, service, text};
use crate::repository_logo_asset::{blob_member, gif, png};

/// One `GET` of `path` written straight onto a socket, with everything the server answered
/// read back as the bytes it was delivered as. No credential and no `X-Amz-Target`: exactly
/// what a browser sends.
///
/// `connection: close` so the server closes the socket after answering and the read below
/// terminates on EOF rather than on a parse of the framing this probe is about.
fn raw_get(port: u16, path: &str) -> Vec<u8> {
    use std::io::{Read, Write};

    let mut stream =
        std::net::TcpStream::connect(("127.0.0.1", port)).expect("connect to the served port");
    let request = format!("GET {path} HTTP/1.1\r\nhost: 127.0.0.1\r\nconnection: close\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .expect("write the request");
    let mut answer = Vec::new();
    stream
        .read_to_end(&mut answer)
        .expect("read the whole answer");
    answer
}

/// The response's head as it was SENT, and its body as the bytes they were delivered as. An
/// image payload is not text, so the split is over the BYTES: decoding the whole answer as
/// UTF-8 would rewrite the very bytes this probe is about. The head's own bytes are kept as
/// received, because the SPELLING of a field name is what half of this probe is about.
fn head_and_body(answer: &[u8], case: &str) -> (String, Vec<u8>) {
    let separator = b"\r\n\r\n";
    let at = answer
        .windows(separator.len())
        .position(|window| window == separator)
        .unwrap_or_else(|| panic!("case {case:?}: expected a response head in the answer"));
    let head = String::from_utf8_lossy(&answer[..at]).to_string();
    (head, answer[at + separator.len()..].to_vec())
}

/// The port and path of a URL this registry published, so a client can dial exactly the
/// address it was handed.
fn address_of(url: &str, case: &str) -> (u16, String) {
    let authority_and_path = url
        .split_once("://")
        .map(|(_scheme, rest)| rest)
        .unwrap_or_else(|| panic!("case {case:?}: expected a URL with a scheme, got {url:?}"));
    let at = authority_and_path
        .find('/')
        .unwrap_or_else(|| panic!("case {case:?}: expected a URL with a path, got {url:?}"));
    let port = authority_and_path[..at]
        .rsplit(':')
        .next()
        .and_then(|port| port.parse().ok())
        .unwrap_or_else(|| panic!("case {case:?}: expected an address with a port, got {url:?}"));
    (port, authority_and_path[at..].to_string())
}

/// RULE: the socket answers a published logo with `200`, the media type the payload's own
/// bytes announce, the asset's length, and the bytes themselves — the same facts the
/// in-process response states, now as they are written on the wire.
///
/// HTTP/1.1 field names are case-insensitive (RFC 9110 §5.1) and the writer below this
/// crate emits every one of them lower-cased, so the head is matched lower-cased too: what
/// is asserted is the FIELD and its value, which is what a client dispatching on the media
/// type reads.
#[tokio::test(flavor = "multi_thread")]
async fn the_socket_delivers_a_published_logo_with_its_media_type_and_length() {
    let router = service();

    // The rows differ in the payload's own format, so the media type on the wire is the one
    // THESE bytes announce, and in the repository-name form the asset's path carries.
    let cases: [(&str, &str, Vec<u8>, &str); 3] = [
        ("a PNG", "wire/png-logo", png(64, 64), "image/png"),
        ("a GIF", "wire-gif-logo", gif(120, 90), "image/gif"),
        (
            "bytes of no image format",
            "wire/opaque-logo",
            vec![0u8, 1, 2, 3, 4, 5],
            "application/octet-stream",
        ),
    ];
    let mut assets: Vec<(&str, Vec<u8>, &str, String)> = Vec::new();
    for (case, repository, blob, media_type) in cases {
        ok(
            &router,
            "CreateRepository",
            json!({"repositoryName": repository}),
            case,
        )
        .await;
        let written = ok(
            &router,
            "PutRepositoryCatalogData",
            json!({
                "repositoryName": repository,
                "catalogData": {"description": "a hosted logo", "logoImageBlob": blob_member(&blob)},
            }),
            case,
        )
        .await;
        let url = text(&written, &["catalogData", "logoUrl"], case);
        let path = url
            .split_once("://")
            .and_then(|(_scheme, rest)| rest.find('/').map(|at| rest[at..].to_string()))
            .unwrap_or_else(|| panic!("case {case:?}: expected a URL with a path, got {url:?}"));
        assets.push((case, blob, media_type, path));
    }

    // One listener for all the rows: what is under test is the answer, not the binding.
    let (endpoint, _served) = emulator_server::serve_on_ephemeral_port(router).await;
    let port: u16 = endpoint
        .rsplit(':')
        .next()
        .and_then(|port| port.parse().ok())
        .unwrap_or_else(|| panic!("an endpoint with a port, got {endpoint:?}"));

    for (case, blob, media_type, path) in assets {
        let fetched = path.clone();
        let answer = tokio::task::spawn_blocking(move || raw_get(port, &fetched))
            .await
            .expect("the socket exchange runs to completion");
        let (head, body) = head_and_body(&answer, case);
        let head = head.to_ascii_lowercase();
        assert!(
            head.starts_with("http/1.1 200"),
            "case {case:?}: expected a 200 on the wire for {path:?}, got {head:?}"
        );
        let stated = format!("content-type: {media_type}");
        assert!(
            head.contains(&stated),
            "case {case:?}: expected the asset served as {media_type:?}, got {head:?}"
        );
        let length = format!("content-length: {}", blob.len());
        assert!(
            head.contains(&length),
            "case {case:?}: expected the asset's own length stated, got {head:?}"
        );
        assert_eq!(
            body, blob,
            "case {case:?}: expected the {} uploaded bytes on the wire, got {} of them",
            blob.len(),
            body.len()
        );
    }
}

/// RULE: the ADDRESS a published `logoUrl` names answers the asset with the media type the
/// payload's own bytes announce, stated under the canonical, hyphen-separated spelling of
/// the field name (`Content-Type`) — so a client that reads a response's field names AS
/// RECEIVED, which is what a browser `<img>`, an image pipeline and a publish-time
/// validator do, finds the type of what it fetched.
///
/// The rows differ in the payload's own format, so the type stated is the one THESE bytes
/// announce; the field NAME is the fixed half of the rule and the same in every row. Each
/// row is fetched at the address its own URL names, with no credential and no
/// `X-Amz-Target`: exactly what a browser sends.
#[tokio::test(flavor = "multi_thread")]
async fn the_published_address_states_the_media_type_under_its_canonical_field_name() {
    let router = service();

    let cases: [(&str, &str, Vec<u8>, &str); 3] = [
        ("a PNG", "published/png-logo", png(64, 64), "image/png"),
        ("a GIF", "published-gif-logo", gif(120, 90), "image/gif"),
        (
            "bytes of no image format",
            "published/opaque-logo",
            vec![9u8, 8, 7, 6, 5],
            "application/octet-stream",
        ),
    ];
    for (case, repository, blob, media_type) in cases {
        ok(
            &router,
            "CreateRepository",
            json!({"repositoryName": repository}),
            case,
        )
        .await;
        let written = ok(
            &router,
            "PutRepositoryCatalogData",
            json!({
                "repositoryName": repository,
                "catalogData": {"description": "a hosted logo", "logoImageBlob": blob_member(&blob)},
            }),
            case,
        )
        .await;
        let url = text(&written, &["catalogData", "logoUrl"], case);

        // The published URL is dialled as it stands: its own port, its own path.
        let (port, path) = address_of(&url, case);
        let fetched = path.clone();
        let answer = tokio::task::spawn_blocking(move || raw_get(port, &fetched))
            .await
            .expect("the socket exchange runs to completion");
        let (head, body) = head_and_body(&answer, case);
        assert!(
            head.starts_with("HTTP/1.1 200"),
            "case {case:?}: expected a 200 at the published address {url:?}, got {head:?}"
        );
        let stated = format!("Content-Type: {media_type}");
        assert!(
            head.contains(&stated),
            "case {case:?}: expected the asset served as {stated:?} under that exact field \
             name, got {head:?}"
        );
        let length = format!("Content-Length: {}", blob.len());
        assert!(
            head.contains(&length),
            "case {case:?}: expected {length:?} under that exact field name, got {head:?}"
        );
        assert_eq!(
            body, blob,
            "case {case:?}: expected the {} uploaded bytes at {url:?}, got {} of them",
            blob.len(),
            body.len()
        );
    }
}

/// RULE: the address the asset plane is published at serves THAT plane and nothing else: a
/// path naming no asset of this registry is answered 404 there, and no other surface of the
/// emulator is reachable through it.
#[tokio::test(flavor = "multi_thread")]
async fn the_published_address_serves_the_asset_plane_alone() {
    let router = service();
    let repository = "published/scoped-logo";
    let blob = png(64, 64);
    ok(
        &router,
        "CreateRepository",
        json!({"repositoryName": repository}),
        "the published asset",
    )
    .await;
    let written = ok(
        &router,
        "PutRepositoryCatalogData",
        json!({
            "repositoryName": repository,
            "catalogData": {"logoImageBlob": blob_member(&blob)},
        }),
        "the published asset",
    )
    .await;
    let url = text(
        &written,
        &["catalogData", "logoUrl"],
        "the published asset",
    );
    let (port, path) = address_of(&url, "the published asset");

    // The rows span the paths this address does not serve: the emulator's own control-plane
    // route, a path under another plane's namespace, a well-formed asset path this registry
    // holds nothing for, and the real gallery's own logo path.
    let absent = path.replace(
        path.rsplit('/').next().unwrap_or_default(),
        &"0".repeat(64),
    );
    let cases: [(&str, String); 4] = [
        ("the control-plane route", "/".to_string()),
        (
            "another plane's namespace",
            "/_aws/es/000000000000/index".to_string(),
        ),
        ("an asset id nothing was stored under", absent),
        (
            "the real gallery's logo path",
            format!("/logos/{repository}/logo.png"),
        ),
    ];
    for (case, path) in cases {
        let fetched = path.clone();
        let answer = tokio::task::spawn_blocking(move || raw_get(port, &fetched))
            .await
            .expect("the socket exchange runs to completion");
        let (head, body) = head_and_body(&answer, case);
        assert!(
            head.starts_with("HTTP/1.1 404"),
            "case {case:?}: expected nothing served for {path:?} at the asset address, \
             got {head:?}"
        );
        assert_ne!(
            body, blob,
            "case {case:?}: expected no asset bytes for {path:?}"
        );
    }
}
