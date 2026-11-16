// SPDX-License-Identifier: CC0-1.0

//! The repository logo a catalog write published is served by the WHOLE assembled emulator,
//! at the URL it reported, to a client that presents no credential.
//!
//! The rule under test: a URL this emulator hands a client to fetch later leads back to
//! this emulator, and the request that fetches it carries no routing key at all — no SigV4
//! credential scope, no `X-Amz-Target`, and a loopback `Host` that names no service — only
//! the path the service itself minted. So the assembled router, which is every service of
//! this workspace at once, must resolve such a path to the ONE service that published it
//! and answer the stored asset with the media type its bytes announce; no other service may
//! claim it, and nothing may be answered for a path of the same shape that names no stored
//! asset.
//!
//! This needs the WHOLE router rather than one service's own: what it pins is the
//! front-door resolution of a keyless request among every service mounted here, which a
//! single-service router cannot exercise (`ecr-public`'s own suite covers the asset's
//! content, over its own tables). Assembling the router costs stack (see `boot_serve.rs`),
//! so it is built — and the requests driven — on a sized thread.

use std::sync::Arc;

use aws_emulator_router::{build_router, InMemoryEngine};
use smithy_server_runtime::MultiServiceRouter;

/// Keep this at or above `boot_serve.rs`'s stack: the same assembly runs here.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// ECR Public is a partition-wide service whose endpoint lives in `us-east-1`, which is
/// also the Region the catalog writes below are signed for.
const REGION: &str = "us-east-1";

/// A header-valid PNG whose IHDR states `width` x `height` (ISO 15948 §5.2 signature, then
/// the 13-byte IHDR chunk the dimensions are read out of). The pixel data is not what any
/// rule here reads, so the payload stays small whatever size it declares.
fn png(width: u32, height: u32) -> Vec<u8> {
    let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    bytes.extend_from_slice(&13u32.to_be_bytes());
    bytes.extend_from_slice(b"IHDR");
    bytes.extend_from_slice(&width.to_be_bytes());
    bytes.extend_from_slice(&height.to_be_bytes());
    // Bit depth, colour type, compression, filter and interlace, then the chunk's CRC.
    bytes.extend_from_slice(&[8, 0, 0, 0, 0]);
    bytes.extend_from_slice(&[0, 0, 0, 0]);
    bytes
}

/// A header-valid GIF whose screen descriptor states `width` x `height` (GIF89a, the two
/// little-endian 16-bit fields that follow the signature).
fn gif(width: u16, height: u16) -> Vec<u8> {
    let mut bytes = b"GIF89a".to_vec();
    bytes.extend_from_slice(&width.to_le_bytes());
    bytes.extend_from_slice(&height.to_le_bytes());
    bytes.extend_from_slice(&[0x80, 0, 0]);
    bytes
}

/// Drive one ECR Public operation through the assembled router: `POST /` with an
/// `awsJson1_1` body and the `X-Amz-Target` header naming the operation.
fn ecr_public(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    operation: &str,
    body: serde_json::Value,
) -> (u16, serde_json::Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://api.ecr-public.{REGION}.amazonaws.com/"))
        .header("host", format!("api.ecr-public.{REGION}.amazonaws.com"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/ecr-public\
                 /aws4_request, SignedHeaders=host;x-amz-target, Signature=deadbeef"
            ),
        )
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("SpencerFrontendService.{operation}"))
        .body(bytes::Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = runtime.block_on(router.dispatch(request));
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).to_string();
    let parsed = serde_json::from_str(&text).unwrap_or(serde_json::Value::Null);
    (status, parsed)
}

/// One anonymous fetch of `url` through the assembled router: a bare `GET` of the URL's own
/// path, with a loopback `Host` and nothing else — no credential, no target. Answers the
/// status, the `content-type` the response states (empty when it states none) and the body.
fn fetch(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    url: &str,
    case: &str,
) -> (u16, String, Vec<u8>) {
    let path = url
        .split_once("://")
        .and_then(|(_scheme, rest)| rest.find('/').map(|at| rest[at..].to_string()))
        .unwrap_or_else(|| panic!("case {case:?}: expected a URL with a path, got {url:?}"));
    let request = http::Request::builder()
        .method("GET")
        .uri(path)
        .header("host", "127.0.0.1:9297")
        .body(bytes::Bytes::new())
        .expect("a well-formed request");
    let response = runtime.block_on(router.dispatch(request));
    let status = response.status().as_u16();
    let media_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    (status, media_type, response.into_body().to_vec())
}

/// The string member of a (possibly nested) response object.
fn text(body: &serde_json::Value, path: &[&str], case: &str) -> String {
    let mut cursor = body;
    for step in path {
        cursor = &cursor[*step];
    }
    cursor
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a string at {path:?}, got {body}"))
        .to_string()
}

#[test]
fn the_assembled_router_serves_a_published_repository_logo_to_an_anonymous_client() {
    std::thread::Builder::new()
        .name("ecr-public-logo-asset".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
            let mut router = build_router(engine);
            router.set_request_authentication(false);
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build()
                .expect("a current-thread runtime");

            // The rows differ in the payload's own format — so the media type the asset is
            // served as is the one ITS bytes announce — and in the repository-name form the
            // asset's path carries, flat and namespaced, because the path is all the
            // assembled router has to resolve the fetch by.
            let payloads: [(&str, &str, Vec<u8>, &str); 3] = [
                ("a PNG", "gallery-logo", png(64, 64), "image/png"),
                ("a GIF", "gallery/logo", gif(120, 90), "image/gif"),
                ("a larger PNG", "org/team/logo", png(512, 256), "image/png"),
            ];
            for (case, repository, blob, media_type) in payloads {
                let (status, created) = ecr_public(
                    &router,
                    &runtime,
                    "CreateRepository",
                    serde_json::json!({"repositoryName": repository}),
                );
                assert_eq!(
                    status, 200,
                    "case {case:?}: CreateRepository must be accepted, got {status} {created}"
                );
                let (status, written) = ecr_public(
                    &router,
                    &runtime,
                    "PutRepositoryCatalogData",
                    serde_json::json!({
                        "repositoryName": repository,
                        "catalogData": {
                            "description": "a hosted logo",
                            "logoImageBlob": aws_smithy_types::base64::encode(&blob),
                        },
                    }),
                );
                assert_eq!(
                    status, 200,
                    "case {case:?}: the catalog write must be accepted, got {status} {written}"
                );
                let url = text(&written, &["catalogData", "logoUrl"], case);

                // The anonymous fetch of that URL is answered by the service that published
                // it — with the stored bytes and the media type they announce.
                let (status, served_type, body) = fetch(&router, &runtime, &url, case);
                assert_eq!(
                    status, 200,
                    "case {case:?}: expected {url:?} to serve the asset, got {status}"
                );
                assert_eq!(
                    body,
                    blob,
                    "case {case:?}: expected the uploaded bytes ({} of them), got {}",
                    blob.len(),
                    body.len()
                );
                assert_eq!(
                    served_type, media_type,
                    "case {case:?}: expected the asset served as {media_type:?}, \
                     got {served_type:?}"
                );

                // A path of the same shape naming an asset this registry never stored is
                // answered by nothing: no service invents an asset for it.
                let (base, _asset_id) = url
                    .rsplit_once('/')
                    .unwrap_or_else(|| panic!("case {case:?}: expected an asset id in {url:?}"));
                let unknown = format!("{base}/{}", "0".repeat(64));
                let (status, _, body) = fetch(&router, &runtime, &unknown, case);
                assert_eq!(
                    (status, body.len()),
                    (404, 0),
                    "case {case:?}: expected nothing served for {unknown:?}"
                );
            }
        })
        .expect("spawn the logo-asset thread")
        .join()
        .expect("the assembled router must serve the published logo");
}
