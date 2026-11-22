// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the S3 WEBSITE ENDPOINT: the surface a bucket configured for static
//! website hosting is served on.
//!
//! RULE: a request addressed to a bucket's WEBSITE surface — which is a different endpoint
//! from the REST API's, reached at the bucket's website host
//! (`<bucket>.s3-website-<Region>.…`) and, for a client that resolves nothing but this
//! emulator's own endpoint, at the reserved data-plane path this emulator serves the same
//! surface under on it (`/_aws/s3-website/<bucket>`) — is served from that bucket's website
//! configuration: a request addressing a directory gets that directory's `IndexDocument`
//! suffix, a key the public may read gets its bytes, a key it may read that is not there gets
//! the `ErrorDocument` body with HTTP 404, content the bucket does not make public gets 403, an
//! object carrying `x-amz-website-redirect-location` gets a 301 to it, a `RoutingRule` whose
//! `Condition` holds gets the redirect that rule names, and any method but a read gets 405.
//! The REST endpoint is untouched: a missing key there is still `NoSuchKey`, whatever routing
//! rules the bucket carries, and the bytes of a redirecting object are still what it serves.
//!
//! Every request here is ANONYMOUS and goes over the wire through the router, exactly as a
//! browser addressing a static site makes it, so what is pinned is the HTTP behaviour (status,
//! `Location`, body, media type) rather than any handler's internals. The suites are TABLES
//! whose rows differ along what the rule generalises over: which path is addressed (the root, a
//! directory, a key, a missing key, a private key), which method is used, which condition a
//! routing rule carries and which replacement it names, which of the two addresses the request
//! reached the surface by, and which of the addressable forms a stored redirect location
//! carries.

use std::sync::Arc;

use service_com_amazonaws_s3_amazons3::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

use super::bucket_lifecycle::{router, send_bytes, Answer};
use super::object_and_tagging_roundtrip::{bucket, put_object, text};

/// The Region the website host below names, which is the one this crate's suites address.
const REGION: &str = "us-east-1";

/// The media type the HTML objects below are stored under, and the one a served object is
/// answered with.
const HTML: &str = "text/html";

/// The website host of `bucket`: the older hyphenated form of the two Amazon S3 publishes
/// (`<bucket>.s3-website-<Region>.amazonaws.com`), so the suite also pins that both are read.
fn website_host(bucket: &str) -> String {
    format!("{bucket}.s3-website-{REGION}.amazonaws.com")
}

/// The dotted form of the same endpoint (`<bucket>.s3-website.<Region>.amazonaws.com`).
fn dotted_website_host(bucket: &str) -> String {
    format!("{bucket}.s3-website.{REGION}.amazonaws.com")
}

/// One ANONYMOUS request to a website host: no `Authorization` header at all, which is what a
/// browser sends.
async fn website(
    router: &MultiServiceRouter,
    method: &str,
    host: &str,
    path: &str,
) -> Answer {
    let request = http::Request::builder()
        .method(method)
        .uri(format!("http://{host}{path}"))
        .header("host", host)
        .body(bytes::Bytes::new())
        .expect("the test request must build");
    let response = router.dispatch(request).await;
    let (parts, body) = response.into_parts();
    Answer {
        status: parts.status.as_u16(),
        headers: parts.headers,
        body,
    }
}

/// The bucket policy that publishes the site: `s3:GetObject` on every object to every
/// principal — "a bucket policy statement that allows `s3:GetObject` on this object to every
/// principal", which is how a static site is published — with one key held back by an explicit
/// `Deny`, so the suite spans both directions of the decision.
fn site_policy(bucket: &str) -> String {
    format!(
        "{{\"Version\":\"2012-10-17\",\"Statement\":[\
         {{\"Sid\":\"PublicRead\",\"Effect\":\"Allow\",\"Principal\":\"*\",\
         \"Action\":\"s3:GetObject\",\"Resource\":\"arn:aws:s3:::{bucket}/*\"}},\
         {{\"Sid\":\"KeepOnePrivate\",\"Effect\":\"Deny\",\"Principal\":\"*\",\
         \"Action\":\"s3:GetObject\",\"Resource\":\"arn:aws:s3:::{bucket}/private.html\"}}]}}"
    )
}

/// `PUT /<bucket>?publicAccessBlock` — `PutPublicAccessBlock` with every setting OFF, which is
/// the step a static site needs before its policy can be public: "public policies are prevented
/// by the BlockPublicPolicy setting in S3 Block Public Access" until it is turned off.
async fn allow_public_policies(router: &MultiServiceRouter, bucket: &str) {
    let body = "<PublicAccessBlockConfiguration>\
         <BlockPublicAcls>false</BlockPublicAcls>\
         <IgnorePublicAcls>false</IgnorePublicAcls>\
         <BlockPublicPolicy>false</BlockPublicPolicy>\
         <RestrictPublicBuckets>false</RestrictPublicBuckets>\
         </PublicAccessBlockConfiguration>";
    let answered = send_bytes(
        router,
        "PUT",
        &format!("/{bucket}?publicAccessBlock"),
        &[("content-type", "application/xml")],
        body.as_bytes(),
    )
    .await;
    assert_eq!(
        answered.status, 200,
        "the public access block must be accepted, got {} with {:?}",
        answered.status,
        text(&answered)
    );
}

/// `PUT /<bucket>?policy` — `PutBucketPolicy`.
async fn put_policy(router: &MultiServiceRouter, bucket: &str, document: &str) {
    let answered = send_bytes(
        router,
        "PUT",
        &format!("/{bucket}?policy"),
        &[("content-type", "application/json")],
        document.as_bytes(),
    )
    .await;
    assert_eq!(
        answered.status, 204,
        "the bucket policy must be accepted, got {} with {:?}",
        answered.status,
        text(&answered)
    );
}

/// `PUT /<bucket>?website` — `PutBucketWebsite` with the index and error documents, plus
/// whatever `extra` the row adds (a `RoutingRules` container).
async fn put_website(router: &MultiServiceRouter, bucket: &str, extra: &str) {
    let body = format!(
        "<WebsiteConfiguration><IndexDocument><Suffix>index.html</Suffix></IndexDocument>\
         <ErrorDocument><Key>error.html</Key></ErrorDocument>{extra}</WebsiteConfiguration>"
    );
    let answered = send_bytes(
        router,
        "PUT",
        &format!("/{bucket}?website"),
        &[("content-type", "application/xml")],
        body.as_bytes(),
    )
    .await;
    assert_eq!(
        answered.status, 200,
        "the website configuration must be accepted, got {} with {:?}",
        answered.status,
        text(&answered)
    );
}

/// The keys every served bucket below holds, each with the body it serves.
const INDEX_BODY: &str = "<html>index</html>";
/// See [`INDEX_BODY`].
const SUB_INDEX_BODY: &str = "<html>sub index</html>";
/// See [`INDEX_BODY`].
const ERROR_BODY: &str = "<html>error</html>";
/// See [`INDEX_BODY`].
const PAGE_BODY: &str = "<html>page</html>";
/// See [`INDEX_BODY`].
const PRIVATE_BODY: &str = "<html>private</html>";

/// Stand up a bucket serving a static site: the objects, the policy that makes all but
/// `private.html` public, and the website configuration (plus `extra`).
async fn site(router: &MultiServiceRouter, name: &str, extra: &str) {
    bucket(router, name).await;
    for (key, body) in [
        ("index.html", INDEX_BODY),
        ("sub/index.html", SUB_INDEX_BODY),
        ("error.html", ERROR_BODY),
        ("page.html", PAGE_BODY),
        ("documents/guide.html", PAGE_BODY),
        ("private.html", PRIVATE_BODY),
    ] {
        let written = put_object(
            router,
            name,
            key,
            body.as_bytes(),
            &[("content-type", HTML)],
        )
        .await;
        assert_eq!(
            written.status, 200,
            "{key:?} must be written first, got {} with {:?}",
            written.status,
            text(&written)
        );
    }
    allow_public_policies(router, name).await;
    put_policy(router, name, &site_policy(name)).await;
    put_website(router, name, extra).await;
}

/// One row of the served-request table: the method and path addressed, and what the endpoint
/// must answer.
struct Row {
    case: &'static str,
    method: &'static str,
    path: &'static str,
    status: u16,
    /// The body the answer must carry, or `None` where the row does not pin one.
    body: Option<&'static str>,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the root, which addresses the index document",
            method: "GET",
            path: "/",
            status: 200,
            body: Some(INDEX_BODY),
        },
        Row {
            case: "a directory, which addresses the index document inside it",
            method: "GET",
            path: "/sub/",
            status: 200,
            body: Some(SUB_INDEX_BODY),
        },
        Row {
            case: "a public key",
            method: "GET",
            path: "/page.html",
            status: 200,
            body: Some(PAGE_BODY),
        },
        Row {
            case: "a HEAD of a public key, which carries no body",
            method: "HEAD",
            path: "/page.html",
            status: 200,
            body: Some(""),
        },
        Row {
            case: "a key the public may read that is not there",
            method: "GET",
            path: "/missing.html",
            status: 404,
            body: Some(ERROR_BODY),
        },
        Row {
            case: "a directory with no index document in it",
            method: "GET",
            path: "/empty/",
            status: 404,
            body: Some(ERROR_BODY),
        },
        Row {
            case: "content the bucket does not make public",
            method: "GET",
            path: "/private.html",
            status: 403,
            body: None,
        },
        Row {
            case: "a write, which the website endpoint does not serve",
            method: "PUT",
            path: "/page.html",
            status: 405,
            body: None,
        },
        Row {
            case: "a delete, which it does not serve either",
            method: "DELETE",
            path: "/page.html",
            status: 405,
            body: None,
        },
        Row {
            case: "a POST",
            method: "POST",
            path: "/",
            status: 405,
            body: None,
        },
    ]
}

#[tokio::test]
async fn the_website_endpoint_serves_the_bucket_its_configuration_describes() {
    let router = router();
    let name = "website-endpoint-site";
    site(&router, name, "").await;

    // Both published spellings of the endpoint reach the same surface.
    for host in [website_host(name), dotted_website_host(name)] {
        for row in rows() {
            let answered = website(&router, row.method, &host, row.path).await;
            assert_eq!(
                answered.status, row.status,
                "case {:?} on {host:?}: expected HTTP {}, got {} with body {:?}",
                row.case,
                row.status,
                answered.status,
                text(&answered)
            );
            if let Some(expected) = row.body {
                assert_eq!(
                    text(&answered),
                    expected,
                    "case {:?} on {host:?}: expected the body {expected:?}, got {:?}",
                    row.case,
                    text(&answered)
                );
            }
            // A HEAD sends no body and still reports the length its GET would carry, which is
            // what a client sizing the object reads.
            if row.method == "HEAD" {
                assert_eq!(
                    answered.header("content-length").as_deref(),
                    Some(PAGE_BODY.len().to_string().as_str()),
                    "case {:?}: expected the entity length on a HEAD, got {:?}",
                    row.case,
                    answered.header("content-length")
                );
            }
            // Nothing this surface serves carries a `Location`, which is what a redirect is.
            if row.status == 200 {
                assert_eq!(
                    answered.header("location"),
                    None,
                    "case {:?}: a served object carries no Location, got {:?}",
                    row.case,
                    answered.header("location")
                );
                assert_eq!(
                    answered.header("content-type").as_deref(),
                    Some(HTML),
                    "case {:?}: expected the media type the object is stored under, got {:?}",
                    row.case,
                    answered.header("content-type")
                );
            }
        }
    }
}

#[tokio::test]
async fn an_object_carrying_a_redirect_location_is_answered_as_a_redirect() {
    let router = router();
    let name = "website-endpoint-redirect";
    site(&router, name, "").await;
    // The object the website endpoint redirects FROM: its bytes are what the REST endpoint
    // still serves.
    let written = put_object(
        &router,
        name,
        "redirect.html",
        b"the bytes behind the redirect",
        &[
            ("content-type", HTML),
            ("x-amz-website-redirect-location", "/page.html"),
        ],
    )
    .await;
    assert_eq!(
        written.status, 200,
        "the redirecting object must be written, got {} with {:?}",
        written.status,
        text(&written)
    );

    let host = website_host(name);
    let answered = website(&router, "GET", &host, "/redirect.html").await;
    assert_eq!(
        answered.status, 301,
        "expected the website endpoint to answer a redirect, got {} with {:?}",
        answered.status,
        text(&answered)
    );
    assert_eq!(
        answered.header("location").as_deref(),
        Some("/page.html"),
        "expected the Location the object carries, got {:?}",
        answered.header("location")
    );

    // The REST endpoint serves the BODY of the same object: the redirect is the website
    // endpoint's reading of that member, not a property of the object plane.
    let read = send_bytes(&router, "GET", &format!("/{name}/redirect.html"), &[], b"").await;
    assert_eq!(
        read.status, 200,
        "expected the REST endpoint to serve the object, got {} with {:?}",
        read.status,
        text(&read)
    );
    assert_eq!(
        text(&read),
        "the bytes behind the redirect",
        "expected the REST endpoint to serve the stored bytes, got {:?}",
        text(&read)
    );
}

/// The routing rules the suite below configures: one conditioned on the ERROR a request earns,
/// one on the KEY PREFIX it addresses.
const ROUTING_RULES: &str = "<RoutingRules>\
     <RoutingRule>\
     <Condition><HttpErrorCodeReturnedEquals>404</HttpErrorCodeReturnedEquals></Condition>\
     <Redirect><HttpRedirectCode>302</HttpRedirectCode>\
     <ReplaceKeyWith>fallback.html</ReplaceKeyWith></Redirect>\
     </RoutingRule>\
     <RoutingRule>\
     <Condition><KeyPrefixEquals>docs/</KeyPrefixEquals></Condition>\
     <Redirect><ReplaceKeyPrefixWith>documents/</ReplaceKeyPrefixWith></Redirect>\
     </RoutingRule>\
     </RoutingRules>";

/// One row of the routing table: the path addressed, and the status and `Location` the rules
/// call for (`None` for a request no rule redirects).
struct RoutingRow {
    case: &'static str,
    path: &'static str,
    status: u16,
    location: Option<&'static str>,
}

fn routing_rows() -> Vec<RoutingRow> {
    vec![
        RoutingRow {
            case: "a missing key, which the error-conditioned rule redirects",
            path: "/missing.html",
            status: 302,
            location: Some("/fallback.html"),
        },
        RoutingRow {
            case: "a key under the prefix, which the prefix rule redirects",
            path: "/docs/guide.html",
            status: 301,
            location: Some("/documents/guide.html"),
        },
        RoutingRow {
            case: "a deeper key under the prefix",
            path: "/docs/sub/guide.html",
            status: 301,
            location: Some("/documents/sub/guide.html"),
        },
        RoutingRow {
            case: "an existing key no rule names, which is served",
            path: "/page.html",
            status: 200,
            location: None,
        },
        RoutingRow {
            case: "the root, which is still the index document",
            path: "/",
            status: 200,
            location: None,
        },
    ]
}

#[tokio::test]
async fn the_routing_rules_redirect_the_requests_their_conditions_name() {
    let router = router();
    let name = "website-endpoint-routing";
    site(&router, name, ROUTING_RULES).await;
    let host = website_host(name);

    for row in routing_rows() {
        let answered = website(&router, "GET", &host, row.path).await;
        assert_eq!(
            answered.status, row.status,
            "case {:?}: expected HTTP {}, got {} with body {:?}",
            row.case,
            row.status,
            answered.status,
            text(&answered)
        );
        let expected = row
            .location
            .map(|target| format!("http://{host}{target}"));
        assert_eq!(
            answered.header("location"),
            expected,
            "case {:?}: expected the Location {expected:?}, got {:?}",
            row.case,
            answered.header("location")
        );
    }

    // The REST endpoint ignores routing rules entirely: the same missing key is the object
    // plane's own refusal there.
    let read = send_bytes(&router, "GET", &format!("/{name}/missing.html"), &[], b"").await;
    assert_eq!(
        read.status, 404,
        "expected the REST endpoint to answer 404, got {} with {:?}",
        read.status,
        text(&read)
    );
    assert_eq!(
        read.error_code(),
        "NoSuchKey",
        "expected the REST endpoint's own refusal, got {:?} in {:?}",
        read.error_code(),
        text(&read)
    );
    assert_eq!(
        read.header("location"),
        None,
        "expected the REST endpoint to redirect nothing, got {:?}",
        read.header("location")
    );
}

#[tokio::test]
async fn a_bucket_with_no_website_configuration_serves_no_website() {
    let router = router();
    let name = "website-endpoint-unconfigured";
    bucket(&router, name).await;
    let answered = website(&router, "GET", &website_host(name), "/").await;
    assert_eq!(
        answered.status, 404,
        "expected 404 for a bucket that was never configured, got {} with {:?}",
        answered.status,
        text(&answered)
    );
    assert!(
        text(&answered).contains("NoSuchWebsiteConfiguration"),
        "expected the code for a bucket with no website configuration, got {:?}",
        text(&answered)
    );

    // And a website host naming no bucket at all answers the code for a bucket that does not
    // exist rather than anything of another surface.
    let absent = website(&router, "GET", &website_host("website-endpoint-absent"), "/").await;
    assert_eq!(
        absent.status, 404,
        "expected 404 for a bucket this emulator does not hold, got {} with {:?}",
        absent.status,
        text(&absent)
    );
    assert!(
        text(&absent).contains("NoSuchBucket"),
        "expected the code for a bucket that does not exist, got {:?}",
        text(&absent)
    );

    // A host whose second label merely BEGINS with the website label is not the website
    // endpoint, so nothing of that surface answers it: whatever the request then resolves to,
    // it is never this bucket's website.
    let near_miss = website(
        &router,
        "GET",
        &format!("{name}.s3-websitefoo.example.com"),
        "/",
    )
    .await;
    assert!(
        !text(&near_miss).contains("NoSuchWebsiteConfiguration"),
        "a host that is not a website endpoint must not answer the website refusal, got {:?}",
        text(&near_miss)
    );
}

#[tokio::test]
async fn a_bucket_whose_name_carries_dots_is_served_at_its_own_website_host() {
    // "A bucket name can consist of … lowercase letters, numbers, dots (.), and hyphens"
    // (Amazon S3 User Guide, *Bucket naming rules*), and such a bucket is served at
    // `<bucket>.s3-website-<Region>.amazonaws.com` like any other — so the bucket is
    // everything in front of the website label, not the host's first label alone. The rows
    // span the forms a name takes around that reading: several dot-separated parts, one dot,
    // a part that is only digits, and a plain name beside them.
    let rows: &[(&str, &str)] = &[
        ("several dotted parts", "website.dotted.example"),
        ("one dot", "website.dotted"),
        ("a numeric part", "website.2026.example"),
        ("no dot at all", "website-undotted-example"),
        // A name that itself carries a label of the website endpoint's form: the endpoint's
        // own label is the LAST one, because the Region and the DNS suffix follow it.
        ("a part shaped like the website label", "website.s3-website-us-east-1.site"),
    ];
    for (case, name) in rows {
        let router = router();
        site(&router, name, "").await;
        for (spelling, host) in [
            ("the hyphenated form", website_host(name)),
            ("the dotted form", dotted_website_host(name)),
            ("the root label's dot", format!("{}.", website_host(name))),
        ] {
            let served = website(&router, "GET", &host, "/").await;
            assert_eq!(
                served.status,
                200,
                "case {case:?} ({spelling}): the site of {name:?} must be served at {host:?}, \
                 got {} with {:?}",
                served.status,
                text(&served)
            );
            assert_eq!(
                text(&served),
                INDEX_BODY,
                "case {case:?} ({spelling}): expected the index document, got {:?}",
                text(&served)
            );
        }
    }
}

// --- the address this emulator serves the same surface at on its OWN endpoint --------------
//
// A website host is a name a client RESOLVES and then dials, and a client configured with this
// emulator's one endpoint resolves nothing else — so the surface is served on that endpoint
// too, under the reserved data-plane path this workspace addresses a local plane by. What the
// host cannot carry (WHICH bucket's site), the path carries.

/// The endpoint this emulator publishes to a client on the machine it runs on: a loopback
/// address and a port, which carries no bucket and no Region.
const EMULATOR_ENDPOINT_HOST: &str = "127.0.0.1:9297";

/// The path this emulator serves ONE bucket's website surface at on its own endpoint: the
/// reserved data-plane namespace (`_aws`), the website plane's own label (`s3-website`, the
/// label the bucket's website host carries too) and the bucket.
///
/// Spelled out here rather than taken from the crate: this address IS what a client is pointed
/// at, so the suite pins the spelling itself.
fn site_path(bucket: &str) -> String {
    format!("/_aws/s3-website/{bucket}")
}

/// One ANONYMOUS request to a bucket's website surface at the address this emulator publishes:
/// its own endpoint, the site's path, and the path within the site.
async fn published(
    router: &MultiServiceRouter,
    method: &str,
    bucket: &str,
    within: &str,
) -> Answer {
    let path = format!("{}{within}", site_path(bucket));
    website(router, method, EMULATOR_ENDPOINT_HOST, &path).await
}

#[tokio::test]
async fn the_published_address_serves_the_same_surface_as_the_buckets_website_host() {
    // The rows span the bucket-name forms the address is read over beside the request table
    // itself: a plain name, a name carrying dots, and one whose own name carries a label
    // shaped like the website endpoint's.
    let names: &[(&str, &str)] = &[
        ("a plain name", "published-website-site"),
        ("a name carrying dots", "published.website.site"),
        (
            "a name carrying a label shaped like the website endpoint's",
            "published.s3-website-us-east-1.site",
        ),
    ];
    for (name_case, name) in names {
        let router = router();
        site(&router, name, "").await;
        for row in rows() {
            let answered = published(&router, row.method, name, row.path).await;
            assert_eq!(
                answered.status,
                row.status,
                "case {:?} of {name_case:?}: expected HTTP {} at {}, got {} with body {:?}",
                row.case,
                row.status,
                site_path(name),
                answered.status,
                text(&answered)
            );
            if let Some(expected) = row.body {
                assert_eq!(
                    text(&answered),
                    expected,
                    "case {:?} of {name_case:?}: expected the body {expected:?}, got {:?}",
                    row.case,
                    text(&answered)
                );
            }
            if row.status == 200 {
                assert_eq!(
                    answered.header("content-type").as_deref(),
                    Some(HTML),
                    "case {:?} of {name_case:?}: expected the media type the object is stored \
                     under, got {:?}",
                    row.case,
                    answered.header("content-type")
                );
                assert_eq!(
                    answered.header("location"),
                    None,
                    "case {:?} of {name_case:?}: a served document carries no Location, got {:?}",
                    row.case,
                    answered.header("location")
                );
            }
            // ONE surface, two addresses: the bucket's website host answers the same request
            // the same way, so neither address is a second reading of the configuration.
            let at_host = website(&router, row.method, &website_host(name), row.path).await;
            assert_eq!(
                at_host.status, answered.status,
                "case {:?} of {name_case:?}: the website host answered HTTP {} where the \
                 published address answered {}",
                row.case, at_host.status, answered.status
            );
            assert_eq!(
                text(&at_host),
                text(&answered),
                "case {:?} of {name_case:?}: the website host answered {:?} where the published \
                 address answered {:?}",
                row.case,
                text(&at_host),
                text(&answered)
            );
        }
        // And the site root is addressed with nothing at all after the bucket, which is the
        // same directory as `/`.
        let bare = published(&router, "GET", name, "").await;
        assert_eq!(
            (bare.status, text(&bare).as_str()),
            (200, INDEX_BODY),
            "case {name_case:?}: the bare site path must serve the index document, got {} with \
             {:?}",
            bare.status,
            text(&bare)
        );
    }
}

/// The stored redirect locations the suite below spans: the three forms
/// `WebsiteRedirectLocation` admits — "the website redirect location must have a prefix of
/// 'http://' or 'https://' or '/'", which is what that member's own refusal states — and one
/// carrying a query.
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
    ("a path carrying a query", "query.html", "/new.html?from=old"),
];

/// The bytes stored BEHIND each redirecting object, which is what the REST endpoint serves.
const BEHIND_THE_REDIRECT: &[u8] = b"the bytes behind the redirect";

#[tokio::test]
async fn a_stored_redirect_answers_301_at_the_published_address_while_rest_serves_the_body() {
    let router = router();
    let name = "published-website-redirect";
    site(&router, name, "").await;
    for (case, key, location) in REDIRECTS {
        let written = put_object(
            &router,
            name,
            key,
            BEHIND_THE_REDIRECT,
            &[
                ("content-type", HTML),
                ("x-amz-website-redirect-location", location),
            ],
        )
        .await;
        assert_eq!(
            written.status, 200,
            "case {case:?}: the redirecting object must be written, got {} with {:?}",
            written.status,
            text(&written)
        );
    }

    for (case, key, location) in REDIRECTS {
        // The website surface at the address this emulator publishes: the redirect, with the
        // Location the object carries, verbatim, and no body at all.
        let answered = published(&router, "GET", name, &format!("/{key}")).await;
        assert_eq!(
            answered.status,
            301,
            "case {case:?}: expected a 301 at {}, got {} with {:?}",
            site_path(name),
            answered.status,
            text(&answered)
        );
        assert_eq!(
            answered.header("location").as_deref(),
            Some(*location),
            "case {case:?}: expected the Location the object carries, got {:?}",
            answered.header("location")
        );
        assert_eq!(
            text(&answered),
            "",
            "case {case:?}: a redirect serves no body, got {:?}",
            text(&answered)
        );
        // The same key at the bucket's website host answers the same redirect.
        let at_host = website(&router, "GET", &website_host(name), &format!("/{key}")).await;
        assert_eq!(
            (at_host.status, at_host.header("location")),
            (301, Some((*location).to_string())),
            "case {case:?}: the website host must answer the same redirect, got {} with {:?}",
            at_host.status,
            at_host.header("location")
        );

        // The REST endpoint of the SAME key serves the object's bytes: the redirect is the
        // website surface's reading of that member, not a property of the object plane.
        let read = send_bytes(&router, "GET", &format!("/{name}/{key}"), &[], b"").await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: expected the REST endpoint to serve the object, got {} with {:?}",
            read.status,
            text(&read)
        );
        assert_eq!(
            read.body.as_ref(),
            BEHIND_THE_REDIRECT,
            "case {case:?}: expected the REST endpoint to serve the stored bytes, got {:?}",
            text(&read)
        );
        assert_eq!(
            read.header("location"),
            None,
            "case {case:?}: the REST endpoint redirects nothing, got {:?}",
            read.header("location")
        );
    }
}

#[tokio::test]
async fn a_routing_rule_redirects_back_into_the_same_site_at_the_published_address() {
    let router = router();
    let name = "published-website-routing";
    site(&router, name, ROUTING_RULES).await;

    for row in routing_rows() {
        let answered = published(&router, "GET", name, row.path).await;
        assert_eq!(
            answered.status, row.status,
            "case {:?}: expected HTTP {}, got {} with body {:?}",
            row.case,
            row.status,
            answered.status,
            text(&answered)
        );
        // A rule that names no host redirects WITHIN the site, so the Location is the site's
        // own base at the address the request reached it by — otherwise the redirect would
        // lead a client to the endpoint's root, where no site is served at all.
        let expected = row.location.map(|target| {
            format!(
                "http://{EMULATOR_ENDPOINT_HOST}{}{target}",
                site_path(name)
            )
        });
        assert_eq!(
            answered.header("location"),
            expected,
            "case {:?}: expected the Location {expected:?}, got {:?}",
            row.case,
            answered.header("location")
        );
    }
}

#[tokio::test]
async fn the_published_address_serves_no_website_for_a_bucket_that_has_none() {
    let router = router();
    let name = "published-website-unconfigured";
    bucket(&router, name).await;

    // A bucket with no website configuration serves no website, at this address as at the
    // bucket's website host.
    let answered = published(&router, "GET", name, "/").await;
    assert_eq!(
        answered.status, 404,
        "expected 404 for a bucket that was never configured, got {} with {:?}",
        answered.status,
        text(&answered)
    );
    assert!(
        text(&answered).contains("NoSuchWebsiteConfiguration"),
        "expected the code for a bucket with no website configuration, got {:?}",
        text(&answered)
    );

    // And a published address naming no bucket at all answers the code for a bucket that does
    // not exist.
    let absent = published(&router, "GET", "published-website-absent", "/").await;
    assert_eq!(
        absent.status, 404,
        "expected 404 for a bucket this emulator does not hold, got {} with {:?}",
        absent.status,
        text(&absent)
    );
    assert!(
        text(&absent).contains("NoSuchBucket"),
        "expected the code for a bucket that does not exist, got {:?}",
        text(&absent)
    );

    // The REST surface of the same bucket is untouched by any of it: a missing key is still
    // the object plane's own refusal, under its own code.
    let missing = send_bytes(&router, "GET", &format!("/{name}/missing.html"), &[], b"").await;
    assert_eq!(
        missing.error_code(),
        "NoSuchKey",
        "expected the REST endpoint's own refusal, got {:?} in {:?}",
        missing.error_code(),
        text(&missing)
    );
}

// --- the same surface at the posture the emulator RUNS in -----------------------------------

/// RULE: a website endpoint authenticates nobody — "the website endpoint supports only GET and
/// HEAD requests on objects", and what it serves is what the bucket grants to EVERYONE — so
/// with request authentication ON, as the emulator runs it unless started otherwise, the SAME
/// unsigned requests reach the SAME surface and are answered the same way. A browser addressing
/// a static site sends no credential, so a surface that refused it for the credential would
/// serve no static site at all.
///
/// The rows are the served-request table and the routing table above, driven at every address
/// the surface is reached by: both published spellings of the bucket's website host and the
/// path this emulator serves the same surface at on its own endpoint. Nothing about the
/// expected answers changes with the posture, which is the point.
#[tokio::test]
async fn the_website_surface_answers_unsigned_requests_with_signatures_enforced() {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut emulator = MultiServiceRouter::new();
    // The site is stood up with enforcement off — its signed configuration calls are not the
    // requests under test — and every website request below is dispatched with it on.
    emulator.set_request_authentication(false);
    emulator.register_service(EmulatedService::new(Arc::clone(&engine)));
    let name = "website-endpoint-unsigned";
    site(&emulator, name, "").await;
    // A second site, carrying the routing rules: a rule conditioned on the error a request
    // earns would redirect the rows above that answer one, so the two tables are driven
    // against the configuration each is about.
    let routed_name = "website-endpoint-unsigned-routed";
    site(&emulator, routed_name, ROUTING_RULES).await;
    emulator.set_request_authentication(true);

    for row in rows() {
        for host in [website_host(name), dotted_website_host(name)] {
            let answered = website(&emulator, row.method, &host, row.path).await;
            assert_eq!(
                answered.status, row.status,
                "case {:?} on {host:?}: expected HTTP {}, got {} with body {:?}",
                row.case,
                row.status,
                answered.status,
                text(&answered)
            );
            if let Some(expected) = row.body {
                assert_eq!(
                    text(&answered),
                    expected,
                    "case {:?} on {host:?}: expected the body {expected:?}, got {:?}",
                    row.case,
                    text(&answered)
                );
            }
        }
        // And at the address this emulator publishes on its own endpoint, which is the one a
        // client that resolves nothing but this emulator can dial.
        let at_published = published(&emulator, row.method, name, row.path).await;
        assert_eq!(
            at_published.status, row.status,
            "case {:?} at {}: expected HTTP {}, got {} with body {:?}",
            row.case,
            site_path(name),
            row.status,
            at_published.status,
            text(&at_published)
        );
    }

    // The redirects the bucket's routing rules call for are served to the same caller.
    let host = website_host(routed_name);
    for row in routing_rows() {
        let answered = website(&emulator, "GET", &host, row.path).await;
        assert_eq!(
            answered.status, row.status,
            "case {:?}: expected HTTP {}, got {} with body {:?}",
            row.case,
            row.status,
            answered.status,
            text(&answered)
        );
        let expected = row
            .location
            .map(|target| format!("http://{host}{target}"));
        assert_eq!(
            answered.header("location"),
            expected,
            "case {:?}: expected the Location {expected:?}, got {:?}",
            row.case,
            answered.header("location")
        );
    }

    // The REST endpoint of the same bucket is NOT this surface, and at this posture it still
    // refuses the unsigned caller what the bucket grants nobody: the site's private key.
    let private = website(&emulator, "GET", "localhost", &format!("/{name}/private.html")).await;
    assert_eq!(
        private.status, 403,
        "the REST endpoint must refuse the unsigned read of a key nothing publishes, got {} \
         with body {:?}",
        private.status,
        text(&private)
    );
}
