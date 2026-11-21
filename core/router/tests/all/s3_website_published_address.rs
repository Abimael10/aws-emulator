// SPDX-License-Identifier: CC0-1.0

//! The ADDRESS a bucket's static-website surface is reachable at on the ASSEMBLED emulator.
//!
//! The rule under test: **a bucket configured for static website hosting is served on a
//! website surface distinct from the REST API, at an address the client in front of this
//! emulator can dial.** A client that resolves Amazon's own names reaches that surface at the
//! bucket's website host (`<bucket>.s3-website-<Region>.amazonaws.com`); a client configured
//! with this emulator's ONE endpoint resolves no such name, so the same surface is served on
//! that endpoint under the reserved data-plane path this workspace addresses a local plane by
//! (`/_aws/s3-website/<bucket>`, `service_common::data_plane_addressing`) — what the host
//! cannot carry, the path carries. The REST address of the same keys keeps serving the
//! object's own bytes.
//!
//! What only this suite reaches is that the WHOLE emulator serves that address: which service
//! claims an anonymous request arriving at the emulator's own endpoint under that path is
//! decided by the production mount, so a crate-local harness — which has one service mounted
//! — cannot state it. The S3 crate's own `website_endpoint` suite carries the tables of what
//! the surface answers; this one pins the address on the assembled router.
//!
//! Every website request here is ANONYMOUS — no `Authorization` header at all — which is what
//! a browser addressing a static site sends. The rows differ along what the rule generalises
//! over: which path within the site is addressed, which method is used, and which of the
//! addressable forms a stored redirect location carries.

use bytes::Bytes;
use smithy_server_runtime::MultiServiceRouter;

/// The Region every call here is signed for, and so the Region the bucket's website host
/// names.
const REGION: &str = "us-west-2";

/// The endpoint this emulator publishes to a client on the machine it runs on: a loopback
/// address and a port. A client configured with it resolves no other name, which is why the
/// website surface is addressed by PATH on it.
const EMULATOR_ENDPOINT_HOST: &str = "127.0.0.1:9297";

/// The bucket serving the site, the media type its documents are stored under, and the index
/// document the site answers a directory with.
const BUCKET: &str = "router-website-origin";
const HTML: &str = "text/html";
const INDEX_KEY: &str = "index.html";
const INDEX_BODY: &str = "<html>index</html>";

/// The bytes stored BEHIND a redirecting object, which is what the REST address serves while
/// the website surface answers the redirect.
const BEHIND_THE_REDIRECT: &[u8] = b"the bytes behind the redirect";

/// The stored redirect locations this suite spans: the three forms
/// `WebsiteRedirectLocation` admits — "the website redirect location must have a prefix of
/// 'http://' or 'https://' or '/'", which is what that member's own refusal states.
const REDIRECTS: &[(&str, &str, &str)] = &[
    ("a path on the site itself", "old.html", "/new.html"),
    (
        "an absolute URL over TLS",
        "ext.html",
        "https://example.com/landing",
    ),
    (
        "an absolute URL in the clear",
        "plain.html",
        "http://example.com/landing",
    ),
];

/// The whole emulator over one engine, exactly as the binary assembles it — so which service
/// claims each request, the anonymous website one included, is decided by the production
/// mount rather than by a hand-picked set.
fn router() -> MultiServiceRouter {
    let engine = aws_emulator_router::new_engine();
    let mut router = aws_emulator_router::build_router(engine);
    // Signature verification is the front door's concern, not this suite's: what is under
    // test is the address a site is served at, and a browser fetching a static site signs
    // nothing.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` header whose credential scope names `s3`, which is what routes the
/// REST calls below. The signature is never checked: request authentication is off.
fn authorization() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/s3/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// One answer: its status, its `Location` field and its body.
struct Answer {
    status: u16,
    location: Option<String>,
    body: Bytes,
}

impl Answer {
    /// The body as text, which is what a document is compared as.
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

/// Dispatch one request and read the answer.
async fn dispatch(router: &MultiServiceRouter, request: http::Request<Bytes>) -> Answer {
    let response = router.dispatch(request).await;
    let location = response
        .headers()
        .get("location")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    Answer {
        status: response.status().as_u16(),
        location,
        body: Bytes::copy_from_slice(response.body()),
    }
}

/// One request against S3's REST surface, path-style and signed, which is how the site is
/// PUBLISHED and how an object's own bytes are read back.
async fn s3(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> Answer {
    let host = format!("s3.{REGION}.amazonaws.com");
    let mut builder = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", &host)
        .header("authorization", authorization());
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let request = builder
        .body(Bytes::copy_from_slice(body))
        .expect("a well-formed request");
    dispatch(router, request).await
}

/// The path this emulator serves the bucket's website surface at on its own endpoint: the
/// reserved data-plane namespace (`_aws`), the website plane's own label (`s3-website`, the
/// label the bucket's website host carries too) and the bucket.
///
/// Spelled out here rather than taken from the crate under test: this address IS what a
/// client is pointed at, so the suite pins the spelling itself.
fn site_path() -> String {
    format!("/_aws/s3-website/{BUCKET}")
}

/// One ANONYMOUS request to the bucket's website surface at the address this emulator
/// publishes: its own endpoint, the site's path, and the path within the site.
async fn published(router: &MultiServiceRouter, method: &str, within: &str) -> Answer {
    let path = format!("{}{within}", site_path());
    let request = http::Request::builder()
        .method(method)
        .uri(format!("http://{EMULATOR_ENDPOINT_HOST}{path}"))
        .header("host", EMULATOR_ENDPOINT_HOST)
        .body(Bytes::new())
        .expect("a well-formed request");
    dispatch(router, request).await
}

/// Stand up the site: the bucket, the public-access settings and bucket policy that publish
/// it, its website configuration, its index document, and one redirecting object per row of
/// [`REDIRECTS`].
async fn publish_site(router: &MultiServiceRouter) {
    let created = s3(
        router,
        "PUT",
        &format!("/{BUCKET}"),
        &[("content-type", "application/xml")],
        format!(
            "<CreateBucketConfiguration>\
             <LocationConstraint>{REGION}</LocationConstraint></CreateBucketConfiguration>"
        )
        .as_bytes(),
    )
    .await;
    assert_eq!(
        created.status,
        200,
        "CreateBucket must succeed, got {} with {:?}",
        created.status,
        created.text()
    );
    let unblocked = s3(
        router,
        "PUT",
        &format!("/{BUCKET}?publicAccessBlock"),
        &[("content-type", "application/xml")],
        b"<PublicAccessBlockConfiguration>\
          <BlockPublicAcls>false</BlockPublicAcls>\
          <IgnorePublicAcls>false</IgnorePublicAcls>\
          <BlockPublicPolicy>false</BlockPublicPolicy>\
          <RestrictPublicBuckets>false</RestrictPublicBuckets>\
          </PublicAccessBlockConfiguration>",
    )
    .await;
    assert_eq!(
        unblocked.status,
        200,
        "PutPublicAccessBlock must succeed, got {} with {:?}",
        unblocked.status,
        unblocked.text()
    );
    let policy = s3(
        router,
        "PUT",
        &format!("/{BUCKET}?policy"),
        &[("content-type", "application/json")],
        format!(
            "{{\"Version\":\"2012-10-17\",\"Statement\":[\
             {{\"Sid\":\"PublicRead\",\"Effect\":\"Allow\",\"Principal\":\"*\",\
             \"Action\":\"s3:GetObject\",\"Resource\":\"arn:aws:s3:::{BUCKET}/*\"}}]}}"
        )
        .as_bytes(),
    )
    .await;
    assert_eq!(
        policy.status,
        204,
        "PutBucketPolicy must succeed, got {} with {:?}",
        policy.status,
        policy.text()
    );
    let configured = s3(
        router,
        "PUT",
        &format!("/{BUCKET}?website"),
        &[("content-type", "application/xml")],
        format!(
            "<WebsiteConfiguration>\
             <IndexDocument><Suffix>{INDEX_KEY}</Suffix></IndexDocument>\
             </WebsiteConfiguration>"
        )
        .as_bytes(),
    )
    .await;
    assert_eq!(
        configured.status,
        200,
        "PutBucketWebsite must succeed, got {} with {:?}",
        configured.status,
        configured.text()
    );
    let index = s3(
        router,
        "PUT",
        &format!("/{BUCKET}/{INDEX_KEY}"),
        &[("content-type", HTML)],
        INDEX_BODY.as_bytes(),
    )
    .await;
    assert_eq!(
        index.status,
        200,
        "PutObject of the index document must succeed, got {} with {:?}",
        index.status,
        index.text()
    );
    for (case, key, location) in REDIRECTS {
        let written = s3(
            router,
            "PUT",
            &format!("/{BUCKET}/{key}"),
            &[
                ("content-type", HTML),
                ("x-amz-website-redirect-location", location),
            ],
            BEHIND_THE_REDIRECT,
        )
        .await;
        assert_eq!(
            written.status,
            200,
            "case {case:?}: the redirecting object must be written, got {} with {:?}",
            written.status,
            written.text()
        );
    }
}

#[tokio::test]
async fn the_assembled_emulator_serves_a_buckets_website_surface_at_its_published_address() {
    let router = router();
    publish_site(&router).await;

    // The site root: the index document of the directory it addresses, served by the surface
    // the published address names and not by the REST API (which answers a listing there).
    let root = published(&router, "GET", "/").await;
    assert_eq!(
        (root.status, root.text().as_str()),
        (200, INDEX_BODY),
        "case \"the site root\": expected the index document at {}, got {} with {:?}",
        site_path(),
        root.status,
        root.text()
    );

    // Each stored redirect: HTTP 301 to exactly the location the object carries, serving no
    // body, while the REST address of the SAME key serves the object's bytes.
    for (case, key, location) in REDIRECTS {
        let answered = published(&router, "GET", &format!("/{key}")).await;
        assert_eq!(
            answered.status,
            301,
            "case {case:?}: expected a 301 at {}, got {} with {:?}",
            site_path(),
            answered.status,
            answered.text()
        );
        assert_eq!(
            answered.location.as_deref(),
            Some(*location),
            "case {case:?}: expected the Location the object carries, got {:?}",
            answered.location
        );
        assert_eq!(
            answered.text(),
            "",
            "case {case:?}: a redirect serves no body, got {:?}",
            answered.text()
        );
        let read = s3(&router, "GET", &format!("/{BUCKET}/{key}"), &[], b"").await;
        assert_eq!(
            read.status,
            200,
            "case {case:?}: expected the REST address to serve the object, got {} with {:?}",
            read.status,
            read.text()
        );
        assert_eq!(
            read.body.as_ref(),
            BEHIND_THE_REDIRECT,
            "case {case:?}: expected the REST address to serve the stored bytes, got {:?}",
            read.text()
        );
        assert_eq!(
            read.location, None,
            "case {case:?}: the REST address redirects nothing, got {:?}",
            read.location
        );
    }

    // And the surface serves reads alone: every other method is the 405 a website endpoint
    // answers, at this address as at the bucket's website host.
    for method in ["PUT", "POST", "DELETE"] {
        let refused = published(&router, method, &format!("/{INDEX_KEY}")).await;
        assert_eq!(
            refused.status,
            405,
            "case {method:?}: expected 405 at {}, got {} with {:?}",
            site_path(),
            refused.status,
            refused.text()
        );
    }
}
