// SPDX-License-Identifier: CC0-1.0

//! Where the classic hosted UI publishes the style sheet a level of a user pool
//! has stored: the `CSSUrl` every answer that renders a UI customization
//! carries, and the document a browser fetches from it — driven through the
//! router exactly as an SDK and a browser drive them (`POST /` with
//! `X-Amz-Target: AWSCognitoIdentityProviderService.<Op>`, and an anonymous
//! `GET` of the reported `CSSUrl`).
//!
//! The rule under test: a hosted UI customization that holds a style sheet
//! publishes it, so every answer that renders that customization — the write's
//! own and every later read's — reports one `CSSUrl` locating the STORED
//! document of that level under the `CSSVersion` the answer reports for it, and
//! that URL serves the stored document; a level that holds no style sheet
//! publishes none.
//!
//! Each table row names the case it asserts and every assertion reports the
//! value it actually saw.

use bytes::Bytes;
use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::managed_login_branding::{call, client, ok, pool, router};

/// The endpoint every request of this suite arrives at, which is the endpoint a
/// locator this service hands out has to name: the host the shared harness
/// addresses.
const ENDPOINT: &str = "cognito-idp.us-east-1.amazonaws.com";

/// The `ClientId` that names a pool's POOL-LEVEL branding: "Set the default
/// configuration for all clients with a ClientId of ALL"
/// (`smithy_query(query="doc",shape="SetUICustomization")`).
const ALL_CLIENTS: &str = "ALL";

/// The modelled length maximum of `CSSType`, the type of
/// `SetUICustomization.CSS` (`smithy_query(query="shape",shape="CSSType")`); its
/// minimum is 0.
const MAX_CSS: usize = 131_072;

/// An ANONYMOUS `GET` of `url` on [`ENDPOINT`], as a browser rendering the
/// hosted UI fetches a stylesheet: no credential and no `X-Amz-Target`, only the
/// path this service published. Answers the status, the body as text and the
/// `Content-Type`.
async fn fetch(router: &MultiServiceRouter, url: &str) -> (u16, String, Option<String>) {
    let path = url
        .strip_prefix(&format!("https://{ENDPOINT}"))
        .unwrap_or_else(|| panic!("{url} must be a locator on {ENDPOINT}"));
    let request = http::Request::builder()
        .method("GET")
        .uri(path)
        .header("host", ENDPOINT)
        .body(Bytes::new())
        .expect("the request is well formed");
    let response = router.dispatch(request).await;
    let media_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .map(ToString::to_string);
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).to_string(),
        media_type,
    )
}

/// A `SetUICustomization` request body; a member left `None` is left out of the
/// request entirely. `ImageFile` is a blob, so it travels base64-encoded.
fn set_request(
    user_pool_id: &str,
    client_id: Option<&str>,
    css: Option<&str>,
    image_file: Option<&[u8]>,
) -> Value {
    let mut body = Map::new();
    body.insert("UserPoolId".to_string(), json!(user_pool_id));
    if let Some(client_id) = client_id {
        body.insert("ClientId".to_string(), json!(client_id));
    }
    if let Some(css) = css {
        body.insert("CSS".to_string(), json!(css));
    }
    if let Some(image_file) = image_file {
        body.insert(
            "ImageFile".to_string(),
            json!(::aws_smithy_types::base64::encode(image_file)),
        );
    }
    Value::Object(body)
}

/// The `CSSUrl` a rendered customization carries, or `None` when it carries
/// none.
fn css_url(rendered: &Value) -> Option<&str> {
    rendered.get("CSSUrl").and_then(Value::as_str)
}

/// The `CSSVersion` a rendered customization reports.
fn css_version(case: &str, rendered: &Value) -> String {
    rendered["CSSVersion"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: a stored customization reports its CSS version, \
                                  got {rendered}"))
        .to_string()
}

/// One row of the table: the level a row configures and the document it stores
/// there.
struct CssCase {
    case: &'static str,
    /// The app client the row configures, or `None` for the pool-level default.
    client_level: bool,
    css: Option<String>,
    image_file: Option<Vec<u8>>,
}

/// The table. The rows differ along what the rule generalises over: the level
/// the document is stored at (an app client, and the pool-level default), the
/// character set and the size of the document — the empty document and the
/// modelled length maximum 131072 of `CSSType` included — and whether a logo
/// image is stored beside it or the write carries no document at all.
fn css_cases() -> Vec<CssCase> {
    vec![
        CssCase {
            case: "an ASCII style sheet at an app client",
            client_level: true,
            css: Some(".label-customizable { color: #1166ff; }".to_string()),
            image_file: None,
        },
        CssCase {
            case: "an ASCII style sheet at the pool-level default",
            client_level: false,
            css: Some(".banner-customizable { padding: 25px 0; }".to_string()),
            image_file: None,
        },
        CssCase {
            case: "a style sheet of non-ASCII text: CJK, an emoji, a combining mark and RTL",
            client_level: true,
            css: Some(".banner::after { content: \"ロゴ 🎨 e\u{301} مرحبا\"; }".to_string()),
            image_file: None,
        },
        CssCase {
            case: "a style sheet at the modelled length minimum 0",
            client_level: true,
            css: Some(String::new()),
            image_file: None,
        },
        CssCase {
            case: "a style sheet at the modelled length maximum",
            client_level: true,
            css: Some("a".repeat(MAX_CSS)),
            image_file: None,
        },
        CssCase {
            case: "a style sheet stored beside a logo image",
            client_level: true,
            css: Some("#root { --brand: #00ff88; }".to_string()),
            image_file: Some(b"\x89PNG\r\n\x1a\nlogo".to_vec()),
        },
        CssCase {
            case: "a write that stores a logo image and no style sheet",
            client_level: true,
            css: None,
            image_file: Some(b"\x89PNG\r\n\x1a\nlogo".to_vec()),
        },
        CssCase {
            case: "a write that stores nothing at all",
            client_level: false,
            css: None,
            image_file: None,
        },
    ]
}

#[tokio::test]
async fn every_stored_style_sheet_is_published_at_the_url_the_answers_report() {
    let router = router();

    for (index, row) in css_cases().into_iter().enumerate() {
        let case = row.case;
        let user_pool_id = pool(&router, &format!("css-locator-{index}-pool")).await;
        let client_id = client(&router, &user_pool_id, &format!("css-locator-{index}")).await;
        let level = if row.client_level {
            client_id.clone()
        } else {
            ALL_CLIENTS.to_string()
        };
        let written = ok(
            &router,
            "SetUICustomization",
            &set_request(
                &user_pool_id,
                row.client_level.then_some(client_id.as_str()),
                row.css.as_deref(),
                row.image_file.as_deref(),
            ),
        )
        .await;
        let written = &written["UICustomization"];
        let read = ok(
            &router,
            "GetUICustomization",
            &json!({ "UserPoolId": user_pool_id, "ClientId": level }),
        )
        .await;
        let read = &read["UICustomization"];

        // A level that holds no style sheet publishes none: there is no
        // document to fetch.
        let Some(stored_css) = row.css.as_deref() else {
            assert_eq!(
                (css_url(written), css_url(read)),
                (None, None),
                "case {case:?}: a level holding no style sheet publishes no CSSUrl, got \
                 {written} and {read}"
            );
            continue;
        };

        // The locator addresses THIS level's document under the version the
        // same answer reports for it.
        let version = css_version(case, written);
        let expected =
            format!("https://{ENDPOINT}/{user_pool_id}/{level}/{version}/assets/CSS/custom-css.css");
        assert_eq!(
            css_url(written),
            Some(expected.as_str()),
            "case {case:?}: the write must publish the document it stored, got {written}"
        );
        // The write and the read render one locator: both answer the stored
        // document, so neither can drift from the other.
        assert_eq!(
            (css_url(read), css_version(case, read).as_str()),
            (Some(expected.as_str()), version.as_str()),
            "case {case:?}: the read must publish the same document as the write, got {read}"
        );

        // The URL really serves the stored document, to an anonymous browser
        // with no credential of its own.
        let (status, body, media_type) = fetch(&router, &expected).await;
        assert_eq!(
            (status, body.as_str()),
            (200, stored_css),
            "case {case:?}: the published URL must serve the stored document, got {status} with \
             {} bytes",
            body.len()
        );
        assert_eq!(
            media_type.as_deref(),
            Some("text/css; charset=utf-8"),
            "case {case:?}: the document is served as a style sheet, got {media_type:?}"
        );

        // A version this level never stored its document at is not published,
        // so nothing is served for it.
        let stale = format!(
            "https://{ENDPOINT}/{user_pool_id}/{level}/1/assets/CSS/custom-css.css"
        );
        let (status, _, _) = fetch(&router, &stale).await;
        assert_ne!(
            status, 200,
            "case {case:?}: a version this level holds no document at must serve nothing"
        );
    }
}

#[tokio::test]
async fn a_rewritten_style_sheet_is_published_as_the_document_in_force() {
    let router = router();
    let user_pool_id = pool(&router, "css-rewrite-pool").await;
    let client_id = client(&router, &user_pool_id, "css-rewrite-client").await;

    // The rows differ along the document stored by each successive write of the
    // same level, including one that leaves the stored document in place by
    // carrying no `CSS` at all.
    let rows: [(&str, Option<&str>, &str); 4] = [
        (
            "the first document",
            Some(".first { color: red; }"),
            ".first { color: red; }",
        ),
        (
            "a rewritten document",
            Some(".second { color: blue; }"),
            ".second { color: blue; }",
        ),
        (
            "a write that carries no CSS, which keeps the stored document",
            None,
            ".second { color: blue; }",
        ),
        (
            "a document rewritten to the empty style sheet",
            Some(""),
            "",
        ),
    ];

    for (case, written_css, expected_css) in rows {
        let written = ok(
            &router,
            "SetUICustomization",
            &set_request(&user_pool_id, Some(&client_id), written_css, None),
        )
        .await;
        let written = &written["UICustomization"];
        let version = css_version(case, written);
        let expected = format!(
            "https://{ENDPOINT}/{user_pool_id}/{client_id}/{version}/assets/CSS/custom-css.css"
        );
        assert_eq!(
            css_url(written),
            Some(expected.as_str()),
            "case {case:?}: the write publishes the document in force, got {written}"
        );
        let (status, body, _) = fetch(&router, &expected).await;
        assert_eq!(
            (status, body.as_str()),
            (200, expected_css),
            "case {case:?}: the published URL serves the document in force, got {status} with \
             {body:?}"
        );
    }
}

#[tokio::test]
async fn an_app_client_without_its_own_style_sheet_is_published_the_pool_level_document() {
    let router = router();
    let user_pool_id = pool(&router, "css-fallback-pool").await;
    let client_id = client(&router, &user_pool_id, "css-fallback-client").await;
    let pool_level = ".pool-level { color: #123456; }";
    let written = ok(
        &router,
        "SetUICustomization",
        &set_request(&user_pool_id, None, Some(pool_level), None),
    )
    .await;
    let version = css_version("the pool-level default", &written["UICustomization"]);

    // "Returns user-pool level branding information if no app client branding is
    // applied" (`GetUICustomization`), so the locator the app client is served
    // addresses the POOL-LEVEL document it falls back to.
    let read = ok(
        &router,
        "GetUICustomization",
        &json!({ "UserPoolId": user_pool_id, "ClientId": client_id }),
    )
    .await;
    let expected = format!(
        "https://{ENDPOINT}/{user_pool_id}/{ALL_CLIENTS}/{version}/assets/CSS/custom-css.css"
    );
    assert_eq!(
        css_url(&read["UICustomization"]),
        Some(expected.as_str()),
        "the app client must be published the document it is served, got {read}"
    );
    let (status, body, _) = fetch(&router, &expected).await;
    assert_eq!(
        (status, body.as_str()),
        (200, pool_level),
        "the published URL must serve the pool-level document, got {status} with {body:?}"
    );

    // A pool with no customization at either level publishes nothing, and no
    // read of one mints a locator.
    let other_pool = pool(&router, "css-empty-pool").await;
    let (status, payload) = call(
        &router,
        "GetUICustomization",
        &json!({ "UserPoolId": other_pool }),
    )
    .await;
    assert_eq!(
        (status, css_url(&payload["UICustomization"])),
        (200, None),
        "a pool with no customization publishes no document, got {status} with {payload}"
    );
}
