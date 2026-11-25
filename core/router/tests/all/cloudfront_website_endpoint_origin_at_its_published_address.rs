// SPDX-License-Identifier: CC0-1.0

//! A distribution whose origin is an Amazon S3 WEBSITE ENDPOINT, fetched by a viewer that
//! DIALS the address the distribution published, on the WHOLE assembled emulator.
//!
//! Two rules are under test, and both need CloudFront and Amazon S3 at once:
//!
//! * a viewer that dials the address a distribution publishes is served what the matched cache
//!   behaviour's origin answers for the path it asked for — for an Amazon S3 website endpoint
//!   addressed over plain HTTP, the index document of the addressed directory;
//! * the read that endpoint makes is ANONYMOUS, and an anonymous read is decided against the
//!   REQUEST's own condition context: a bucket policy that grants `s3:GetObject` to everyone
//!   only while the request states a particular `Referer` — `aws:Referer`, the key AWS's own
//!   static-website guidance restricts a site to its distribution with — admits exactly the
//!   requests stating it, which here are the ones whose origin carries that header.
//!
//! Only a suite on the ASSEMBLED emulator can state them: which service claims the origin
//! request the edge makes to a bucket's website host is decided by the production mount, and
//! the viewer here speaks HTTP/1.1 on a socket rather than being dispatched in process, which
//! is what a client handed a published name does.
//!
//! The rows differ along what the rules generalise over: the path the viewer asks for (the
//! site's root, a nested directory, an object by key), the request made twice — which is what a
//! client polling a freshly deployed distribution does — and, for the conditioned site, what
//! the origin request states for the header the grant is written over (the value it names,
//! another value, nothing at all).

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use smithy_server_runtime::MultiServiceRouter;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::cloudfront_oac_s3_origin_edge::edge_listener_authority;

/// The Region the buckets are created in, and so the Region their website hosts name.
const REGION: &str = "us-east-1";

/// The bucket of the site every request may read, and the bucket of the site only a request
/// stating the secret below may read.
const PUBLIC_BUCKET: &str = "router-cloudfront-website-origin";
const CONDITIONED_BUCKET: &str = "router-cloudfront-website-referer";

/// The value the conditioned bucket's policy names in its `aws:Referer` clause, and the one a
/// row states to show that another value is not it. Neither is part of any requirement: what
/// the rows pin is that the STATED value decides.
const GRANTED_REFERER: &str = "router-website-secret";
const OTHER_REFERER: &str = "router-website-other";

/// The media type the sites' documents are stored under, and the suffix their website
/// configurations name as the index document of a directory.
const HTML: &str = "text/html";
const INDEX_SUFFIX: &str = "index.html";

/// The documents of the sites.
const INDEX_BODY: &str = "<html>the published site index</html>";
const NESTED_BODY: &str = "<html>the published nested index</html>";

/// The code Amazon S3 refuses a read nothing grants with, which the website endpoint reports
/// in the HTML page it answers an error with.
const ACCESS_DENIED: &str = "AccessDenied";

/// How long a distribution is given to report itself `Deployed`, and how often that is read.
/// GENEROUS: what is waited for is the CONDITION, so this only decides when a distribution
/// that never settles is reported as a failure.
const DEPLOYS_WITHIN: Duration = Duration::from_secs(120);
const POLL_EVERY: Duration = Duration::from_millis(50);

/// How long the dialled viewer waits to connect, and for the whole answer. Both far past what
/// either step needs: what a row asserts is WHAT the published address answers, never how
/// quickly.
const DIAL_WITHIN: Duration = Duration::from_secs(60);
const ANSWER_WITHIN: Duration = Duration::from_secs(120);

/// The whole emulator over one engine, exactly as the binary assembles it — so which service
/// claims each request, the edge's own origin request to a bucket's website host included, is
/// decided by the production mount.
fn router() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine = aws_emulator_router::new_engine();
    let mut router = aws_emulator_router::build_router(Arc::clone(&engine));
    // Signature verification is the front door's concern, not this suite's: a browser fetching
    // a static site through a distribution signs nothing.
    router.set_request_authentication(false);
    (router, engine)
}

/// A SigV4 `Authorization` whose credential scope names `service`, which is what routes a
/// setup call and names its Region. The signature is never checked here.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One answer: the status and the body.
struct Answer {
    status: u16,
    body: String,
}

/// Dispatch one request in process and read the answer.
async fn dispatch(
    router: &MultiServiceRouter,
    method: &str,
    host: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> Answer {
    let mut builder = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let request = builder
        .body(Bytes::copy_from_slice(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    Answer {
        status: response.status().as_u16(),
        body: String::from_utf8_lossy(response.body()).into_owned(),
    }
}

/// One call against Amazon S3's REST surface, path-style, which is how a site is published.
async fn s3(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> Answer {
    let host = format!("s3.{REGION}.amazonaws.com");
    let authorization = authorization("s3");
    let mut all: Vec<(&str, &str)> = vec![("authorization", authorization.as_str())];
    all.extend_from_slice(headers);
    dispatch(router, method, &host, path, &all, body).await
}

/// One CloudFront control-plane call.
async fn cloudfront(router: &MultiServiceRouter, method: &str, path: &str, body: &[u8]) -> Answer {
    let host = format!("cloudfront.{REGION}.amazonaws.com");
    let authorization = authorization("cloudfront");
    dispatch(
        router,
        method,
        &host,
        path,
        &[
            ("authorization", authorization.as_str()),
            ("content-type", "application/xml"),
        ],
        body,
    )
    .await
}

/// The text of the first `<name>` element of an XML document.
fn element(document: &str, name: &str) -> String {
    document
        .split_once(&format!("<{name}>"))
        .and_then(|(_, rest)| rest.split_once(&format!("</{name}>")))
        .map_or(String::new(), |(text, _)| text.to_string())
}

/// The policy that publishes every object of `bucket` to every principal, with the `aws:Referer`
/// clause `conditioned` asks for.
fn site_policy(bucket: &str, conditioned: bool) -> String {
    let condition = if conditioned {
        format!(",\"Condition\":{{\"StringEquals\":{{\"aws:Referer\":\"{GRANTED_REFERER}\"}}}}")
    } else {
        String::new()
    };
    format!(
        "{{\"Version\":\"2012-10-17\",\"Statement\":[\
         {{\"Sid\":\"PublicRead\",\"Effect\":\"Allow\",\"Principal\":\"*\",\
         \"Action\":\"s3:GetObject\",\"Resource\":\"arn:aws:s3:::{bucket}/*\"{condition}}}]}}"
    )
}

/// Stand up one static site: the bucket, the settings and policy that publish it, its website
/// configuration and its two index documents.
async fn published_site(router: &MultiServiceRouter, bucket: &str, conditioned: bool) {
    let created = s3(router, "PUT", &format!("/{bucket}"), &[], b"").await;
    assert_eq!(
        created.status, 200,
        "CreateBucket of {bucket} must succeed, got {} with {:?}",
        created.status, created.body
    );
    let unblocked = s3(
        router,
        "PUT",
        &format!("/{bucket}?publicAccessBlock"),
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
        unblocked.status, 200,
        "PutPublicAccessBlock on {bucket} must succeed, got {} with {:?}",
        unblocked.status, unblocked.body
    );
    let policy = s3(
        router,
        "PUT",
        &format!("/{bucket}?policy"),
        &[("content-type", "application/json")],
        site_policy(bucket, conditioned).as_bytes(),
    )
    .await;
    assert_eq!(
        policy.status, 204,
        "PutBucketPolicy on {bucket} must succeed, got {} with {:?}",
        policy.status, policy.body
    );
    let configured = s3(
        router,
        "PUT",
        &format!("/{bucket}?website"),
        &[("content-type", "application/xml")],
        format!(
            "<WebsiteConfiguration>\
             <IndexDocument><Suffix>{INDEX_SUFFIX}</Suffix></IndexDocument>\
             </WebsiteConfiguration>"
        )
        .as_bytes(),
    )
    .await;
    assert_eq!(
        configured.status, 200,
        "PutBucketWebsite on {bucket} must succeed, got {} with {:?}",
        configured.status, configured.body
    );
    for (key, body) in [
        (INDEX_SUFFIX.to_string(), INDEX_BODY),
        (format!("pages/{INDEX_SUFFIX}"), NESTED_BODY),
    ] {
        let written = s3(
            router,
            "PUT",
            &format!("/{bucket}/{key}"),
            &[("content-type", HTML)],
            body.as_bytes(),
        )
        .await;
        assert_eq!(
            written.status, 200,
            "PutObject of {bucket}/{key} must succeed, got {} with {:?}",
            written.status, written.body
        );
    }
}

/// `CreateDistribution` over `bucket`'s website endpoint as a CUSTOM origin addressed with HTTP
/// only — the configuration AWS's own guidance gives such an origin — with the LEGACY
/// forwarding members a client that names no cache policy sends, and the origin custom header
/// `stated_referer` where the row states one. Answers `(Id, DomainName)`.
async fn created_distribution(
    router: &MultiServiceRouter,
    reference: &str,
    bucket: &str,
    stated_referer: Option<&str>,
) -> (String, String) {
    let website_host = format!("{bucket}.s3-website-{REGION}.amazonaws.com");
    let custom_headers = stated_referer
        .map(|value| {
            format!(
                "<CustomHeaders><Quantity>1</Quantity><Items><OriginCustomHeader>\
                 <HeaderName>Referer</HeaderName><HeaderValue>{value}</HeaderValue>\
                 </OriginCustomHeader></Items></CustomHeaders>"
            )
        })
        .unwrap_or_default();
    let body = format!(
        "<DistributionConfig xmlns=\"http://cloudfront.amazonaws.com/doc/2020-05-31/\">\
         <CallerReference>{reference}</CallerReference>\
         <Origins><Quantity>1</Quantity><Items><Origin>\
         <Id>website-origin</Id><DomainName>{website_host}</DomainName>{custom_headers}\
         <CustomOriginConfig><HTTPPort>80</HTTPPort><HTTPSPort>443</HTTPSPort>\
         <OriginProtocolPolicy>http-only</OriginProtocolPolicy></CustomOriginConfig>\
         </Origin></Items></Origins>\
         <DefaultCacheBehavior><TargetOriginId>website-origin</TargetOriginId>\
         <ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy>\
         <ForwardedValues><QueryString>false</QueryString>\
         <Cookies><Forward>none</Forward></Cookies></ForwardedValues>\
         <MinTTL>0</MinTTL></DefaultCacheBehavior>\
         <Comment>a static site served from an s3 website endpoint</Comment>\
         <Enabled>true</Enabled></DistributionConfig>"
    );
    let created = cloudfront(router, "POST", "/2020-05-31/distribution", body.as_bytes()).await;
    assert_eq!(
        created.status, 201,
        "CreateDistribution {reference} must succeed, got {} with {:?}",
        created.status, created.body
    );
    let id = element(&created.body, "Id");
    let domain = element(&created.body, "DomainName");
    assert!(
        !id.is_empty() && !domain.is_empty(),
        "the created distribution {reference} reports no Id or DomainName: {:?}",
        created.body
    );
    awaited_deployment(router, &id).await;
    (id, domain)
}

/// Read `GetDistribution` until CloudFront reports the distribution `Deployed`, which is the
/// state a viewer request is served in.
async fn awaited_deployment(router: &MultiServiceRouter, id: &str) {
    let deadline = Instant::now() + DEPLOYS_WITHIN;
    loop {
        let read = cloudfront(router, "GET", &format!("/2020-05-31/distribution/{id}"), b"").await;
        let status = element(&read.body, "Status");
        if status == "Deployed" {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the distribution was still {status:?} when the wait ran out: {:?}",
            read.body
        );
        tokio::time::sleep(POLL_EVERY).await;
    }
}

/// Dial the address the published name resolves to and send one HTTP/1.1 request stating that
/// name as the host it addressed, answering the whole message that comes back.
///
/// This is the client a viewer handed a published name is: it speaks the protocol on a socket
/// rather than going through the router, which is what makes the published name the thing
/// under test. Only the PORT is taken from the authority, and the loopback address is
/// named explicitly, so nothing here opens a connection to anywhere else.
async fn dialled(authority: &str, host: &str, path: &str) -> String {
    let port = authority
        .parse::<SocketAddr>()
        .unwrap_or_else(|error| panic!("the published address {authority} is no address: {error}"))
        .port();
    let mut stream = tokio::time::timeout(
        DIAL_WITHIN,
        tokio::net::TcpStream::connect((Ipv4Addr::LOCALHOST, port)),
    )
    .await
    .unwrap_or_else(|_| panic!("the published address {authority} did not accept a connection"))
    .unwrap_or_else(|error| {
        panic!("the published address {authority} refused a connection: {error}")
    });
    let request = format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .await
        .unwrap_or_else(|error| panic!("the request to {authority} was not sent: {error}"));
    let mut answer = Vec::new();
    let read = tokio::time::timeout(ANSWER_WITHIN, stream.read_to_end(&mut answer))
        .await
        .unwrap_or_else(|_| panic!("the published address {authority} did not answer in time"));
    read.unwrap_or_else(|error| panic!("the answer from {authority} was not read: {error}"));
    String::from_utf8_lossy(&answer).into_owned()
}

/// One row of the first table: the path the viewer asks for and the document the site answers
/// it with.
struct PathRow {
    case: &'static str,
    path: &'static str,
    body: &'static str,
}

#[tokio::test]
async fn a_website_endpoint_origin_is_served_to_a_viewer_dialling_the_published_address() {
    let (router, engine) = router();
    published_site(&router, PUBLIC_BUCKET, false).await;
    let (id, domain) =
        created_distribution(&router, "router-website-public", PUBLIC_BUCKET, None).await;
    let address = edge_listener_authority(&engine, &id)
        .expect("this emulator opened an address for the distribution");

    let rows = [
        PathRow {
            case: "the root of the site",
            path: "/",
            body: INDEX_BODY,
        },
        PathRow {
            case: "a nested directory, whose own index document is served",
            path: "/pages/",
            body: NESTED_BODY,
        },
        PathRow {
            case: "an object of the site, addressed by key",
            path: "/index.html",
            body: INDEX_BODY,
        },
    ];

    for row in rows {
        // Twice: a client polling a freshly deployed distribution asks again, and the second
        // answer carries the same document whether it came from the origin or the edge cache.
        for attempt in 1..=2 {
            let served = dialled(&address, &domain, row.path).await;
            assert!(
                served.starts_with("HTTP/1.1 200"),
                "case {:?} (attempt {attempt}): expected the site's document from {domain}, \
                 got {served:?}",
                row.case
            );
            assert!(
                served.contains(row.body),
                "case {:?} (attempt {attempt}): expected an answer carrying {:?}, got {served:?}",
                row.case,
                row.body
            );
        }
    }
}

/// One row of the second table: what the distribution's origin states for the header the
/// bucket's grant is written over, and what the site then serves.
struct RefererRow {
    case: &'static str,
    reference: &'static str,
    /// The value the origin's own `CustomHeaders` state for `Referer`, if any.
    stated: Option<&'static str>,
    status: u16,
    /// A fragment the answer's body must carry.
    body: &'static str,
}

#[tokio::test]
async fn a_referer_conditioned_site_admits_exactly_the_origin_requests_stating_that_referer() {
    let (router, engine) = router();
    published_site(&router, CONDITIONED_BUCKET, true).await;

    let rows = [
        RefererRow {
            case: "the origin states the referer the grant names",
            reference: "router-website-granted",
            stated: Some(GRANTED_REFERER),
            status: 200,
            body: INDEX_BODY,
        },
        RefererRow {
            case: "the origin states another referer",
            reference: "router-website-other",
            stated: Some(OTHER_REFERER),
            status: 403,
            body: ACCESS_DENIED,
        },
        RefererRow {
            case: "the origin states no referer at all",
            reference: "router-website-none",
            stated: None,
            status: 403,
            body: ACCESS_DENIED,
        },
    ];

    for row in rows {
        let (id, domain) = created_distribution(
            &router,
            row.reference,
            CONDITIONED_BUCKET,
            row.stated,
        )
        .await;
        let address = edge_listener_authority(&engine, &id)
            .expect("this emulator opened an address for the distribution");
        let served = dialled(&address, &domain, "/").await;
        assert!(
            served.starts_with(&format!("HTTP/1.1 {}", row.status)),
            "case {:?}: expected HTTP {} from {domain}, got {served:?}",
            row.case,
            row.status
        );
        assert!(
            served.contains(row.body),
            "case {:?}: expected an answer carrying {:?}, got {served:?}",
            row.case,
            row.body
        );
        if row.status != 200 {
            assert!(
                !served.contains(INDEX_BODY),
                "case {:?}: the refusal carries the site's bytes: {served:?}",
                row.case
            );
        }
    }
}
