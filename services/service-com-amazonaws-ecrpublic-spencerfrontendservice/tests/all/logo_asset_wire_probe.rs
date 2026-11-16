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

/// The response's head, lower-cased, and its body as the bytes they were delivered as. An
/// image payload is not text, so the split is over the BYTES: decoding the whole answer as
/// UTF-8 would rewrite the very bytes this probe is about.
fn head_and_body(answer: &[u8], case: &str) -> (String, Vec<u8>) {
    let separator = b"\r\n\r\n";
    let at = answer
        .windows(separator.len())
        .position(|window| window == separator)
        .unwrap_or_else(|| panic!("case {case:?}: expected a response head in the answer"));
    let head = String::from_utf8_lossy(&answer[..at]).to_ascii_lowercase();
    (head, answer[at + separator.len()..].to_vec())
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
