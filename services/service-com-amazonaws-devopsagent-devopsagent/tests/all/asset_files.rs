// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for the asset FILE plane: `CreateAssetFile`, the
//! `GetAssetFile` read-back it is proved through, and what a read of one file
//! reports at each revision of its asset.
//!
//! Every case drives the service through the same router the emulator serves, so
//! the routes, status codes, protocol envelope and state are all exercised.

use std::io::Write;
use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_devopsagent_devopsagent::state::{
    asset_file_not_found_message, asset_not_found_message, asset_version_not_found_message,
    not_a_file_of_the_asset_message, ACCESS_DENIED_MESSAGE, MAX_ASSET_CONTENT_BYTES,
    MAX_ASSET_FILE_PATH_LEN,
};

/// A SigV4 credential scope naming this service's signing name (`aidevops`), which
/// is how the router resolves which service claims a request. Signature
/// verification is switched off for these tests, so the signature is a placeholder.
const AUTHORIZATION: &str = concat!(
    "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/us-west-2/aidevops/aws4_request, ",
    "SignedHeaders=host, Signature=0123456789abcdef"
);

/// One emulator: a single shared engine behind the production router.
fn new_router() -> MultiServiceRouter {
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_devopsagent_devopsagent::EmulatedService::new(Arc::new(
            engine::InMemoryEngine::new(),
        )),
    );
    router
}

/// Send one request and return its status and parsed body (`Value::Null` for an
/// empty body).
async fn call(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let payload = body
        .as_ref()
        .map(|b| Bytes::from(serde_json::to_vec(b).expect("serialize request body")))
        .unwrap_or_default();
    let mut request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "aidevops.us-west-2.amazonaws.com")
        .header("authorization", AUTHORIZATION);
    if body.is_some() {
        request = request.header("content-type", "application/json");
    }
    let request = request.body(payload).expect("build request");

    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let parsed = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or_else(|e| {
            panic!(
                "response body is not JSON ({e}): {}",
                String::from_utf8_lossy(&bytes)
            )
        })
    };
    (status, parsed)
}

/// The wire error code (`__type`) of an error response.
fn error_code(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or("<no __type>")
        .rsplit(['#', ':'])
        .next()
        .unwrap_or_default()
        .to_string()
}

/// The `message` of an error response.
fn error_message(body: &Value) -> String {
    body.get("message")
        .or_else(|| body.get("Message"))
        .and_then(Value::as_str)
        .unwrap_or("<no message>")
        .to_string()
}

/// Create an agent space and return its id.
async fn create_space(router: &MultiServiceRouter, name: &str) -> String {
    let (status, body) = call(router, "POST", "/v1/agentspaces", Some(json!({"name": name}))).await;
    assert_eq!(
        status, 201,
        "case {name:?}: expected 201 from CreateAgentSpace, got {status}: {body}"
    );
    body["agentSpace"]["agentSpaceId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {name:?}: no agentSpaceId in {body}"))
        .to_string()
}

/// Create an asset whose content is `content` and return its id.
async fn create_asset(
    router: &MultiServiceRouter,
    space: &str,
    case: &str,
    content: Value,
) -> String {
    let (status, body) = call(
        router,
        "POST",
        &format!("/asset/agent-space/{space}/assets"),
        Some(json!({
            "assetType": "artifact",
            "metadata": {"title": case},
            "content": content,
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: expected 201 from CreateAsset, got {status}: {body}"
    );
    body["asset"]["assetId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no assetId in {body}"))
        .to_string()
}

/// An asset that holds a SET of files: content that arrived as a bundle, so the
/// file plane may add one more to it.
async fn create_bundle_asset(router: &MultiServiceRouter, space: &str, case: &str) -> String {
    let zip = aws_smithy_types::base64::encode(zip_bundle(&[("bundled.md", b"bundled")]));
    create_asset(router, space, case, json!({"zip": {"zipFile": zip}})).await
}

/// A zip archive carrying `entries`, built with the same crate the handler reads
/// one with.
fn zip_bundle(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut buffer = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut buffer);
        for (path, content) in entries {
            writer
                .start_file(*path, zip::write::SimpleFileOptions::default())
                .expect("start zip entry");
            writer.write_all(content).expect("write zip entry");
        }
        writer.finish().expect("finish zip");
    }
    buffer.into_inner()
}

/// The `CreateAssetFile` / `GetAssetFile` URI for one file of one asset. `path` is
/// already in its wire (URI) form, which is not always the stored path.
fn asset_file_uri(space: &str, asset: &str, path: &str) -> String {
    format!("/asset/agent-space/{space}/assets/{asset}/files/{path}")
}

/// RULE: a created file is persisted as the request described it and a read of it
/// answers exactly that — for every shape of body, path and metadata the model
/// allows.
#[tokio::test]
async fn created_file_round_trips_through_get() {
    // The rows differ along what the rule generalises over: the body variant
    // (text, bytes), the size of the content (empty, small, several KiB), the
    // shape of the path (flat, nested, percent-encoded space and parens) and
    // whether per-file metadata was described.
    let big_text: String = "line\n".repeat(2_000);
    let cases: Vec<(&str, &str, &str, Value)> = vec![
        (
            "text body",
            "docs/one.md",
            "docs/one.md",
            json!({"content": {"text": "# one"}}),
        ),
        (
            "bytes body",
            "bin/two.bin",
            "bin/two.bin",
            json!({"content": {"bytes": "AAECAw=="}}),
        ),
        (
            "empty text body",
            "empty.txt",
            "empty.txt",
            json!({"content": {"text": ""}}),
        ),
        (
            "several KiB of text",
            "big.txt",
            "big.txt",
            json!({"content": {"text": big_text}}),
        ),
        (
            "per-file metadata",
            "three.md",
            "three.md",
            json!({
                "content": {"text": "three"},
                "metadata": {"contentType": "text/markdown", "reviewed": true},
            }),
        ),
        (
            "deeply nested path",
            "a/b/c/d/e.md",
            "a/b/c/d/e.md",
            json!({"content": {"text": "deep"}}),
        ),
        (
            // `AssetFilePath` admits spaces and parentheses, which reach the
            // greedy path label percent-encoded.
            "percent-encoded space and parens",
            "notes/release%20(1).bin",
            "notes/release (1).bin",
            json!({"content": {"bytes": "AAECAw=="}}),
        ),
    ];

    let router = new_router();
    let space = create_space(&router, "asset-file-space").await;
    let asset = create_bundle_asset(&router, &space, "round trip").await;

    for (case, wire_path, stored_path, input) in &cases {
        let (status, created) = call(
            &router,
            "POST",
            &asset_file_uri(&space, &asset, wire_path),
            Some(input.clone()),
        )
        .await;
        assert_eq!(
            status, 201,
            "case {case:?}: expected 201 from CreateAssetFile, got {status}: {created}"
        );
        let file = &created["file"];
        assert_eq!(
            file["path"],
            json!(stored_path),
            "case {case:?}: path came back as {:?}",
            file["path"]
        );
        assert_eq!(
            file["content"], input["content"],
            "case {case:?}: content came back as {:?}",
            file["content"]
        );
        // `AssetFile$metadata` carries the members the request described for the
        // file, completed with the media type of the content the service stored
        // for it — see the `mimeType` rule in `probe_mime`.
        let mut expected_metadata = input.get("metadata").cloned().unwrap_or_else(|| json!({}));
        expected_metadata["mimeType"] = json!(if input["content"].get("text").is_some() {
            service_com_amazonaws_devopsagent_devopsagent::state::MEDIA_TYPE_MARKDOWN
        } else {
            service_com_amazonaws_devopsagent_devopsagent::state::MEDIA_TYPE_OCTET_STREAM
        });
        assert_eq!(
            file["metadata"], expected_metadata,
            "case {case:?}: metadata came back as {:?}",
            file["metadata"]
        );
        // `AssetFile$version` is the asset revision the file belongs to, which is
        // the revision the asset it was created in is at.
        assert_eq!(
            file["version"],
            json!(1),
            "case {case:?}: file belongs to revision {:?}, not the asset's",
            file["version"]
        );
        assert!(
            !file["createdAt"].is_null() && !file["updatedAt"].is_null(),
            "case {case:?}: createdAt/updatedAt missing from {created}"
        );

        // READ BACK: GetAssetFile answers exactly what CreateAssetFile persisted,
        // and a second read is identical, so reading changes nothing.
        let uri = asset_file_uri(&space, &asset, wire_path);
        let (status, fetched) = call(&router, "GET", &uri, None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 from GetAssetFile, got {status}: {fetched}"
        );
        assert_eq!(
            fetched["file"], created["file"],
            "case {case:?}: GetAssetFile returned {:?}, not what CreateAssetFile persisted ({:?})",
            fetched["file"], created["file"]
        );
        let (_, again) = call(&router, "GET", &uri, None).await;
        assert_eq!(
            again, fetched,
            "case {case:?}: two reads of one file disagree: {again:?}"
        );

        // The revision the file belongs to is readable by name; any other version
        // names no revision of the asset.
        let (status, by_version) = call(&router, "GET", &format!("{uri}?assetVersion=1"), None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 reading revision 1, got {status}: {by_version}"
        );
        assert_eq!(
            by_version["file"], created["file"],
            "case {case:?}: revision 1 read back as {:?}",
            by_version["file"]
        );
        let (status, missing) = call(&router, "GET", &format!("{uri}?assetVersion=2"), None).await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected 404 for a revision the asset never had, got {status}: {missing}"
        );
        assert_eq!(
            error_code(&missing),
            "ResourceNotFoundException",
            "case {case:?}: revision refusal answered {:?}",
            error_code(&missing)
        );
    }
}

/// RULE: an asset whose content IS one file holds exactly that file, so a request
/// to create another file in it is refused with HTTP 400 `ValidationException`
/// reporting how many files an asset of its TYPE supports; an asset whose content
/// is a SET of files (a bundle, or an import that materialises none) takes a
/// created file.
#[tokio::test]
async fn an_asset_whose_content_is_one_file_takes_no_other_file() {
    let router = new_router();
    let space = create_space(&router, "one-file-space").await;

    // The rows differ along the content the asset was created from — the member of
    // `AssetContent` (`smithy_query(query="shape", shape="AssetContent")`: a single
    // `file`, a `zip` bundle, a `sourceUrl` import) and, for a bundle, the number
    // of entries it carried.
    let cases: Vec<(&str, Value, Option<&str>)> = vec![
        (
            "single text file",
            json!({"file": {"path": "only.md", "body": {"text": "only"}}}),
            Some("only.md"),
        ),
        (
            "single binary file at a nested path",
            json!({"file": {"path": "dir/only.bin", "body": {"bytes": "AAECAw=="}}}),
            Some("dir/only.bin"),
        ),
        (
            "zip bundle of one entry",
            json!({
                "zip": {
                    "zipFile": aws_smithy_types::base64::encode(
                        zip_bundle(&[("only.md", b"only")]),
                    )
                }
            }),
            None,
        ),
        (
            "zip bundle of several entries",
            json!({
                "zip": {
                    "zipFile": aws_smithy_types::base64::encode(zip_bundle(&[
                        ("one.md", b"one"),
                        ("two.md", b"two"),
                    ]))
                }
            }),
            None,
        ),
        (
            "source url import",
            json!({"sourceUrl": {"url": "https://example.com/bundle.zip"}}),
            None,
        ),
    ];

    for (case, content, holds_one_file_at) in &cases {
        let asset = create_asset(&router, &space, case, content.clone()).await;

        // The same create, against each kind of asset: either the asset's content
        // is one file and no path at all names a file that can be created in it,
        // or the asset holds a set of files and takes one more.
        for added in ["added.md", "dir/added.md"] {
            let (status, body) = call(
                &router,
                "POST",
                &asset_file_uri(&space, &asset, added),
                Some(json!({"content": {"text": "added"}})),
            )
            .await;
            match holds_one_file_at {
                Some(_) => {
                    assert_eq!(
                        status, 400,
                        "case {case:?} adding {added:?}: expected 400, got {status}: {body}"
                    );
                    assert_eq!(
                        error_code(&body),
                        "ValidationException",
                        "case {case:?} adding {added:?}: refusal answered {:?}",
                        error_code(&body)
                    );
                    assert_eq!(
                        error_message(&body),
                        "Assets of type 'artifact' support only one file. \
                         Use UpdateAssetFile to replace the existing file.",
                        "case {case:?} adding {added:?}: message was {:?}",
                        error_message(&body)
                    );
                }
                None => assert_eq!(
                    status, 201,
                    "case {case:?} adding {added:?}: expected 201, got {status}: {body}"
                ),
            }
        }

        // The file an asset of one file already holds is still readable, and the
        // refused create persisted nothing under the path it named.
        if let Some(existing) = holds_one_file_at {
            // A create that names the path the asset already holds a file at is the
            // more specific condition, so it conflicts rather than reporting the
            // asset's shape.
            let (status, conflict) = call(
                &router,
                "POST",
                &asset_file_uri(&space, &asset, existing),
                Some(json!({"content": {"text": "again"}})),
            )
            .await;
            assert_eq!(
                status, 409,
                "case {case:?} re-creating {existing:?}: expected 409, got {status}: {conflict}"
            );
            assert_eq!(
                error_code(&conflict),
                "ConflictException",
                "case {case:?} re-creating {existing:?}: refusal answered {:?}",
                error_code(&conflict)
            );

            let (status, fetched) =
                call(&router, "GET", &asset_file_uri(&space, &asset, existing), None).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected 200 reading {existing:?}, got {status}: {fetched}"
            );
            let (status, missing) =
                call(&router, "GET", &asset_file_uri(&space, &asset, "added.md"), None).await;
            assert_eq!(
                status, 404,
                "case {case:?}: a refused create persisted a file: {status}: {missing}"
            );
            assert_eq!(
                error_message(&missing),
                "Asset file not found: added.md",
                "case {case:?}: read refusal said {:?}",
                error_message(&missing)
            );
        }
    }
}

/// RULE: a create refuses what it cannot persist — a path the asset already holds
/// a file at conflicts with the asset's current state (HTTP 409), a value that
/// names no file of the asset is invalid (HTTP 400), an asset that does not exist
/// is missing (HTTP 404) and an agent space the caller cannot reach is refused
/// before anything inside it is looked up (HTTP 403).
#[tokio::test]
async fn create_asset_file_refuses_what_it_cannot_persist() {
    let router = new_router();
    let space = create_space(&router, "refusal-space").await;
    let asset = create_bundle_asset(&router, &space, "refusals").await;

    let (status, created) = call(
        &router,
        "POST",
        &asset_file_uri(&space, &asset, "taken.md"),
        Some(json!({"content": {"text": "taken"}})),
    )
    .await;
    assert_eq!(
        status, 201,
        "the file the conflict case needs was not created: {status}: {created}"
    );

    // Each row: the space, the asset, the wire path, and the status and error code
    // the request must answer with.
    let cases: Vec<(&str, String, String, String, u16, &str)> = vec![
        (
            "path already taken by a created file",
            space.clone(),
            asset.clone(),
            "taken.md".to_string(),
            409,
            "ConflictException",
        ),
        (
            "path already taken by a bundled file",
            space.clone(),
            asset.clone(),
            "bundled.md".to_string(),
            409,
            "ConflictException",
        ),
        (
            "path that walks out of the asset",
            space.clone(),
            asset.clone(),
            "dir/../../escape.md".to_string(),
            400,
            "ValidationException",
        ),
        (
            "path past the modelled length bound",
            space.clone(),
            asset.clone(),
            "x".repeat(MAX_ASSET_FILE_PATH_LEN + 1),
            400,
            "ValidationException",
        ),
        (
            "asset that does not exist",
            space.clone(),
            "asset-0000000000000000".to_string(),
            "any.md".to_string(),
            404,
            "ResourceNotFoundException",
        ),
        (
            "agent space that does not exist",
            "as-0000000000000000".to_string(),
            asset.clone(),
            "any.md".to_string(),
            403,
            "AccessDeniedException",
        ),
    ];

    for (case, space, asset, path, expected_status, expected_code) in &cases {
        let (status, body) = call(
            &router,
            "POST",
            &asset_file_uri(space, asset, path),
            Some(json!({"content": {"text": "x"}})),
        )
        .await;
        assert_eq!(
            status, *expected_status,
            "case {case:?}: expected {expected_status}, got {status}: {body}"
        );
        assert_eq!(
            error_code(&body),
            *expected_code,
            "case {case:?}: refusal answered {:?}",
            error_code(&body)
        );
    }

    // The conflict names the asset and the path, so the caller can tell it from a
    // path that is merely invalid.
    let (_, conflict) = call(
        &router,
        "POST",
        &asset_file_uri(&space, &asset, "taken.md"),
        Some(json!({"content": {"text": "x"}})),
    )
    .await;
    assert_eq!(
        error_message(&conflict),
        format!("A file already exists at taken.md in asset {asset}"),
        "the conflict said {:?}",
        error_message(&conflict)
    );

    // A read of a path the asset holds no file at is missing, not invalid.
    let (status, missing) = call(
        &router,
        "GET",
        &asset_file_uri(&space, &asset, "nothing/here.md"),
        None,
    )
    .await;
    assert_eq!(status, 404, "expected 404 reading a file that is not there, got {status}: {missing}");
    assert_eq!(
        error_code(&missing),
        "ResourceNotFoundException",
        "the read refusal answered {:?}",
        error_code(&missing)
    );
}

/// RULE: `clientToken` is an idempotency token — a repeat of one create with the
/// same token reports the file the first call created instead of creating a
/// second, and two different tokens create two files.
#[tokio::test]
async fn repeating_a_client_token_reports_the_first_file() {
    let router = new_router();
    let space = create_space(&router, "token-space").await;
    let asset = create_bundle_asset(&router, &space, "tokens").await;

    // The rows differ along the token: the same token replayed, the same token
    // with different content, and a fresh token at another path.
    let first = json!({"content": {"text": "first"}, "clientToken": "token-a"});
    let (status, created) = call(
        &router,
        "POST",
        &asset_file_uri(&space, &asset, "token.md"),
        Some(first.clone()),
    )
    .await;
    assert_eq!(
        status, 201,
        "case \"first call\": expected 201, got {status}: {created}"
    );

    for (case, input) in [
        ("same token, same content", first.clone()),
        (
            "same token, different content",
            json!({"content": {"text": "second"}, "clientToken": "token-a"}),
        ),
    ] {
        let (status, replayed) = call(
            &router,
            "POST",
            &asset_file_uri(&space, &asset, "token.md"),
            Some(input),
        )
        .await;
        assert_eq!(
            status, 201,
            "case {case:?}: expected 201 from the replay, got {status}: {replayed}"
        );
        assert_eq!(
            replayed["file"], created["file"],
            "case {case:?}: the replay reported {:?}, not the file the first call created ({:?})",
            replayed["file"], created["file"]
        );
    }

    // A different token at the same path is a second create of a path that is
    // already taken, so it conflicts rather than replaying.
    let (status, conflict) = call(
        &router,
        "POST",
        &asset_file_uri(&space, &asset, "token.md"),
        Some(json!({"content": {"text": "third"}, "clientToken": "token-b"})),
    )
    .await;
    assert_eq!(
        status, 409,
        "case \"fresh token, taken path\": expected 409, got {status}: {conflict}"
    );

    // And at a path of its own it creates a file.
    let (status, other) = call(
        &router,
        "POST",
        &asset_file_uri(&space, &asset, "other.md"),
        Some(json!({"content": {"text": "other"}, "clientToken": "token-b"})),
    )
    .await;
    assert_eq!(
        status, 201,
        "case \"fresh token, fresh path\": expected 201, got {status}: {other}"
    );
    assert_ne!(
        other["file"], created["file"],
        "case \"fresh token, fresh path\": a second token reported the first file"
    );
}

/// RULE: the content one asset holds is bounded by the size this service stores for
/// an asset, however it arrived — a created file that fits inside what is left is
/// stored, and one byte past the bound is refused with HTTP 413
/// `ContentSizeExceededException`.
#[tokio::test]
async fn asset_content_size_bounds_a_created_file() {
    // The asset is created with all but `REMAINING` bytes of the bound, so the
    // rows below land exactly ON the bound and one byte past it. The bound itself
    // is the model's own `@length(max: 6291456)` on every asset body.
    const REMAINING: usize = 16;
    let router = new_router();
    let space = create_space(&router, "size-space").await;
    let filler = vec![b'x'; MAX_ASSET_CONTENT_BYTES - REMAINING];
    let zip = aws_smithy_types::base64::encode(zip_bundle(&[("filler.bin", &filler)]));
    let asset = create_asset(
        &router,
        &space,
        "content size",
        json!({"zip": {"zipFile": zip}}),
    )
    .await;

    // One byte past what is left of the bound, before anything is added.
    let (status, refused) = call(
        &router,
        "POST",
        &asset_file_uri(&space, &asset, "past.txt"),
        Some(json!({"content": {"text": "y".repeat(REMAINING + 1)}})),
    )
    .await;
    assert_eq!(
        status, 413,
        "case \"one byte past the bound\": expected 413, got {status}: {refused}"
    );
    assert_eq!(
        error_code(&refused),
        "ContentSizeExceededException",
        "case \"one byte past the bound\": refusal answered {:?}",
        error_code(&refused)
    );
    assert_eq!(
        error_message(&refused),
        format!("The content exceeds the maximum size of {MAX_ASSET_CONTENT_BYTES} bytes for one asset"),
        "case \"one byte past the bound\": message was {:?}",
        error_message(&refused)
    );

    // Exactly what is left of the bound is stored.
    let (status, created) = call(
        &router,
        "POST",
        &asset_file_uri(&space, &asset, "fits.txt"),
        Some(json!({"content": {"text": "y".repeat(REMAINING)}})),
    )
    .await;
    assert_eq!(
        status, 201,
        "case \"exactly the bound\": expected 201, got {status}: {created}"
    );

    // And the asset is now full, so one more byte is refused.
    let (status, full) = call(
        &router,
        "POST",
        &asset_file_uri(&space, &asset, "one-more.txt"),
        Some(json!({"content": {"text": "y"}})),
    )
    .await;
    assert_eq!(
        status, 413,
        "case \"one byte past a full asset\": expected 413, got {status}: {full}"
    );
}

/// RULE: a read of one file reports the file as of the revision of the asset the
/// request names — omitted, the revision the asset is at — so a revision that has
/// been superseded still reports the content, metadata and revision number it was
/// read with, and a path the named revision held no file at is missing in it
/// however the revisions around it stand.
///
/// How a revision comes into being and how far back they are retained are pinned
/// by the suites that own those operations (`asset_file_updates`, `asset_versions`);
/// what this covers is what a READ of one file answers at each of them.
#[tokio::test]
async fn a_file_read_reports_the_revision_it_names() {
    let router = new_router();
    let space = create_space(&router, "revision-read-space").await;
    let asset = create_bundle_asset(&router, &space, "revision reads").await;

    // Three files, created while the asset is at its first revision.
    for (path, content) in [("a.md", "a"), ("b.md", "b"), ("dir/c.md", "c")] {
        let (status, created) = call(
            &router,
            "POST",
            &asset_file_uri(&space, &asset, path),
            Some(json!({"content": {"text": content}, "metadata": {"origin": path}})),
        )
        .await;
        assert_eq!(
            status, 201,
            "setup: expected 201 creating {path:?}, got {status}: {created}"
        );
    }

    // Changing a stored file moves the asset to its next revision, and so does
    // removing one — the two kinds of change whose earlier content a revision has
    // to keep readable.
    let (status, updated) = call(
        &router,
        "PATCH",
        &asset_file_uri(&space, &asset, "a.md"),
        Some(json!({"content": {"text": "a-again"}, "metadata": {"origin": "updated"}})),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: expected 200 from UpdateAssetFile, got {status}: {updated}"
    );
    let (status, deleted) = call(
        &router,
        "DELETE",
        &asset_file_uri(&space, &asset, "b.md"),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: expected 200 from DeleteAssetFile, got {status}: {deleted}"
    );

    // The revision the asset is at after those two changes, read from the asset
    // itself rather than assumed, which is the revision a read naming none reports.
    let asset_uri = format!("/asset/agent-space/{space}/assets/{asset}");
    let (status, read_asset) = call(&router, "GET", &asset_uri, None).await;
    assert_eq!(
        status, 200,
        "setup: expected 200 from GetAsset, got {status}: {read_asset}"
    );
    let current_version = read_asset["asset"]["version"].as_i64().unwrap_or_else(|| {
        panic!("setup: no asset version to read the current revision from: {read_asset}")
    });
    // Changing a stored file and removing one each mint a revision, so the asset
    // left its first one twice.
    assert_eq!(
        current_version, 3,
        "setup: expected the two changes to mint two revisions, got revision {current_version}"
    );

    /// One read: the revision it names (`None` names no revision at all, so the
    /// read reports the one the asset is at), the path it names, and what that
    /// revision holds at that path — the content and the `origin` member of the
    /// file's metadata, or `None` because the revision holds no file there.
    struct RevisionRead {
        case: &'static str,
        version: Option<i64>,
        path: &'static str,
        holds: Option<(&'static str, &'static str)>,
    }

    // The rows differ along the revision the read names and along how the file at
    // the path stands in it: created in it, changed after it, deleted after it,
    // or never there.
    let rows = vec![
        RevisionRead {
            case: "first revision, file later changed",
            version: Some(1),
            path: "a.md",
            holds: Some(("a", "a.md")),
        },
        RevisionRead {
            case: "first revision, file later deleted",
            version: Some(1),
            path: "b.md",
            holds: Some(("b", "b.md")),
        },
        RevisionRead {
            case: "first revision, nested file",
            version: Some(1),
            path: "dir/c.md",
            holds: Some(("c", "dir/c.md")),
        },
        RevisionRead {
            case: "second revision, changed file",
            version: Some(2),
            path: "a.md",
            holds: Some(("a-again", "updated")),
        },
        RevisionRead {
            case: "second revision, file not yet deleted",
            version: Some(2),
            path: "b.md",
            holds: Some(("b", "b.md")),
        },
        RevisionRead {
            case: "second revision, untouched file",
            version: Some(2),
            path: "dir/c.md",
            holds: Some(("c", "dir/c.md")),
        },
        RevisionRead {
            case: "current revision, changed file",
            version: Some(3),
            path: "a.md",
            holds: Some(("a-again", "updated")),
        },
        RevisionRead {
            case: "current revision, deleted file",
            version: Some(3),
            path: "b.md",
            holds: None,
        },
        RevisionRead {
            case: "current revision, untouched file",
            version: Some(3),
            path: "dir/c.md",
            holds: Some(("c", "dir/c.md")),
        },
        RevisionRead {
            case: "no revision named, changed file",
            version: None,
            path: "a.md",
            holds: Some(("a-again", "updated")),
        },
        RevisionRead {
            case: "no revision named, deleted file",
            version: None,
            path: "b.md",
            holds: None,
        },
        RevisionRead {
            case: "no revision named, untouched file",
            version: None,
            path: "dir/c.md",
            holds: Some(("c", "dir/c.md")),
        },
    ];

    for row in &rows {
        let (case, version, path, expected) = (row.case, &row.version, &row.path, &row.holds);
        let uri = match version {
            Some(version) => format!(
                "{}?assetVersion={version}",
                asset_file_uri(&space, &asset, path)
            ),
            None => asset_file_uri(&space, &asset, path),
        };
        let (status, body) = call(&router, "GET", &uri, None).await;
        match expected {
            Some((content, origin)) => {
                assert_eq!(
                    status, 200,
                    "case {case:?}: expected 200 from {uri}, got {status}: {body}"
                );
                assert_eq!(
                    body["file"]["path"],
                    json!(path),
                    "case {case:?}: the read reported the path {:?}",
                    body["file"]["path"]
                );
                assert_eq!(
                    body["file"]["content"],
                    json!({"text": content}),
                    "case {case:?}: the read reported the content {:?}",
                    body["file"]["content"]
                );
                // `AssetFile$version` is the asset revision the file belongs to, so
                // a file read at a revision reports that revision.
                assert_eq!(
                    body["file"]["version"],
                    json!(version.unwrap_or(current_version)),
                    "case {case:?}: the read reported revision {:?}",
                    body["file"]["version"]
                );
                // The metadata the file carried in that revision, completed with
                // the media type of the content stored for it.
                assert_eq!(
                    body["file"]["metadata"],
                    json!({"origin": origin, "mimeType": "text/markdown"}),
                    "case {case:?}: the read reported the metadata {:?}",
                    body["file"]["metadata"]
                );
            }
            None => {
                assert_eq!(
                    status, 404,
                    "case {case:?}: expected 404 from {uri}, got {status}: {body}"
                );
                assert_eq!(
                    error_message(&body),
                    asset_file_not_found_message(path),
                    "case {case:?}: the refusal said {:?}",
                    error_message(&body)
                );
            }
        }
    }

    // Reading reports the asset; it moves it to no further revision, so the asset
    // is still at the revision the two changes above took it to.
    let (status, after_the_reads) = call(&router, "GET", &asset_uri, None).await;
    assert_eq!(
        status, 200,
        "after the reads: expected 200 from GetAsset, got {status}: {after_the_reads}"
    );
    assert_eq!(
        after_the_reads["asset"]["version"],
        json!(current_version),
        "after the reads: the asset is at revision {:?}",
        after_the_reads["asset"]["version"]
    );
}

/// RULE: a read of one file refuses what it cannot serve — an agent space the
/// caller cannot reach is refused before anything inside it is looked up (HTTP
/// 403), an asset the space does not hold and a revision the asset does not have
/// are missing (HTTP 404), and a value that names no file of the asset is invalid
/// (HTTP 400) — and a refused read reports no file and changes nothing.
///
/// That an unreachable space and an absent resource are refused this way by EVERY
/// in-space operation is pinned by `in_space_resource_refusals`; the rows for them
/// are here too because what this read must get right is the ORDER — the space
/// before anything inside it.
#[tokio::test]
async fn a_file_read_refuses_what_it_cannot_serve() {
    let router = new_router();
    let space = create_space(&router, "read-refusal-space").await;
    let asset = create_bundle_asset(&router, &space, "read refusals").await;

    // Each row: the space, the asset, the wire path and the query the read names,
    // and the status, error code and message it must answer with. The rows differ
    // along WHICH part of the request cannot be served, and — for a revision —
    // along how far past the one the asset is at the named one is. `message` is
    // `None` only where the wording belongs to the modelled constraint rather than
    // to this service, so there is nothing of this service's to assert.
    struct Refusal {
        case: &'static str,
        space: Option<&'static str>,
        asset: Option<&'static str>,
        path: String,
        query: String,
        status: u16,
        code: &'static str,
        message: Option<String>,
    }

    let rows = vec![
        Refusal {
            case: "an agent space that does not exist",
            space: Some("as-0000000000000000"),
            asset: None,
            path: "bundled.md".to_string(),
            query: String::new(),
            status: 403,
            code: "AccessDeniedException",
            message: Some(ACCESS_DENIED_MESSAGE.to_string()),
        },
        Refusal {
            case: "an asset the space does not hold",
            space: None,
            asset: Some("asset-0000000000000000"),
            path: "bundled.md".to_string(),
            query: String::new(),
            status: 404,
            code: "ResourceNotFoundException",
            message: Some(asset_not_found_message("asset-0000000000000000")),
        },
        Refusal {
            case: "a path the asset holds no file at",
            space: None,
            asset: None,
            path: "absent.md".to_string(),
            query: String::new(),
            status: 404,
            code: "ResourceNotFoundException",
            message: Some(asset_file_not_found_message("absent.md")),
        },
        Refusal {
            case: "a nested path the asset holds no file at",
            space: None,
            asset: None,
            path: "nothing/here.md".to_string(),
            query: String::new(),
            status: 404,
            code: "ResourceNotFoundException",
            message: Some(asset_file_not_found_message("nothing/here.md")),
        },
        Refusal {
            case: "the revision just past the one the asset is at",
            space: None,
            asset: None,
            path: "bundled.md".to_string(),
            query: "?assetVersion=2".to_string(),
            status: 404,
            code: "ResourceNotFoundException",
            message: Some(asset_version_not_found_message(&asset, 2)),
        },
        Refusal {
            case: "a revision far beyond the one the asset is at",
            space: None,
            asset: None,
            path: "bundled.md".to_string(),
            query: "?assetVersion=9999".to_string(),
            status: 404,
            code: "ResourceNotFoundException",
            message: Some(asset_version_not_found_message(&asset, 9999)),
        },
        Refusal {
            case: "a path that starts at a root",
            space: None,
            asset: None,
            path: "/escape.md".to_string(),
            query: String::new(),
            status: 400,
            code: "ValidationException",
            message: Some(not_a_file_of_the_asset_message("/escape.md")),
        },
        Refusal {
            case: "a path that walks out of the asset",
            space: None,
            asset: None,
            path: "../escape.md".to_string(),
            query: String::new(),
            status: 400,
            code: "ValidationException",
            message: Some(not_a_file_of_the_asset_message("../escape.md")),
        },
        Refusal {
            case: "a nested path that walks out of the asset",
            space: None,
            asset: None,
            path: "dir/../../escape.md".to_string(),
            query: String::new(),
            status: 400,
            code: "ValidationException",
            message: Some(not_a_file_of_the_asset_message("dir/../../escape.md")),
        },
        Refusal {
            // The two rows below are refused by the modelled constraint itself
            // (`AssetFilePath`'s own length bound, and `assetVersion`'s
            // `@range(min: 1)`), whose wording the framework owns rather than
            // this service — so they name no expected message.
            case: "a path past the modelled length bound",
            space: None,
            asset: None,
            path: "x".repeat(MAX_ASSET_FILE_PATH_LEN + 1),
            query: String::new(),
            status: 400,
            code: "ValidationException",
            message: None,
        },
        Refusal {
            case: "a revision below the first one, which the model admits no value of",
            space: None,
            asset: None,
            path: "bundled.md".to_string(),
            query: "?assetVersion=0".to_string(),
            status: 400,
            code: "ValidationException",
            message: None,
        },
    ];

    for row in &rows {
        let case = row.case;
        let uri = format!(
            "{}{}",
            asset_file_uri(
                row.space.unwrap_or(space.as_str()),
                row.asset.unwrap_or(asset.as_str()),
                &row.path,
            ),
            row.query
        );
        let (status, body) = call(&router, "GET", &uri, None).await;
        assert_eq!(
            status, row.status,
            "case {case:?}: expected {} from {uri}, got {status}: {body}",
            row.status
        );
        assert_eq!(
            error_code(&body),
            row.code,
            "case {case:?}: refusal answered {:?}: {body}",
            error_code(&body)
        );
        if let Some(expected) = &row.message {
            assert_eq!(
                &error_message(&body),
                expected,
                "case {case:?}: the refusal said {:?}",
                error_message(&body)
            );
        }
        assert!(
            body.get("file").is_none(),
            "case {case:?}: a refused read must report no file: {body}"
        );
    }

    // The WORDING of each class of refusal, spelled out here rather than taken from
    // the builders above, so a change in the text of one is a failure here and not
    // a change on both sides of an assertion. The values inside each message are
    // the request's own and the asset's own; only the fixed text is spelled out.
    for (case, path, query, expected) in [
        (
            "a path the asset holds no file at",
            "absent.md".to_string(),
            String::new(),
            "Asset file not found: absent.md".to_string(),
        ),
        (
            "a path that walks out of the asset",
            "../escape.md".to_string(),
            String::new(),
            "../escape.md does not name a file of the asset".to_string(),
        ),
        (
            "a revision the asset does not have",
            "bundled.md".to_string(),
            "?assetVersion=2".to_string(),
            format!("Asset not found: {asset} version 2"),
        ),
    ] {
        let (_, body) = call(
            &router,
            "GET",
            &format!("{}{query}", asset_file_uri(&space, &asset, &path)),
            None,
        )
        .await;
        assert_eq!(
            error_message(&body),
            expected,
            "case {case:?}: expected the message {expected:?}, got {:?}",
            error_message(&body)
        );
    }

    // None of the refused reads changed what a later caller sees: the file the
    // asset does hold still reads back at the revision the asset is still at.
    let (status, held) = call(
        &router,
        "GET",
        &asset_file_uri(&space, &asset, "bundled.md"),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "after the refusals: expected 200 reading the file the asset holds, got {status}: {held}"
    );
    assert_eq!(
        held["file"]["version"],
        json!(1),
        "after the refusals: the file belongs to revision {:?}",
        held["file"]["version"]
    );
}
