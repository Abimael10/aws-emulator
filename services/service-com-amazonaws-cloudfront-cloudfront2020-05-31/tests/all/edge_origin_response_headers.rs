// SPDX-License-Identifier: CC0-1.0

//! What an answer the edge serves carries from the ORIGIN's own response — end to end over
//! HTTP, with Amazon S3 mounted beside CloudFront on one engine so the origin's answer is a
//! real object store's.
//!
//! RULE: an answer the edge serves on an origin's behalf carries the origin's own response
//! headers through to the viewer — the object's validators, its cache directives, its
//! disposition and its user metadata are the origin's statements about the object, and the
//! edge relays them — except the headers that describe one HTTP connection (the hop-by-hop
//! ones), the length of a body the edge may re-encode for this viewer, and the two the edge
//! states itself about the request it just served, which are CloudFront's own.
//!
//! The same holds for an answer served out of the EDGE CACHE: a cached entry holds what the
//! origin said about the object, so a hit and a miss cannot disagree about it.
//!
//! The rows span what the rule generalises over: the KIND of header the origin stated — a
//! validator CloudFront mints nothing of its own for, a cache directive, a media type, a
//! disposition, and two pieces of user metadata whose names the object itself chose — beside
//! the headers CloudFront states rather than relays. The object is staged through Amazon S3's
//! own `PutObject`, so every value under test is one an origin really answered with and none
//! of them is written by the code under test.
//!
//! The harness here (the two services on one engine, the staged bucket, the published origin
//! access control and the viewer client that reads every header of an answer) is
//! `pub(crate)`, so [`crate::edge_function_associations`] stages its origin the same way
//! rather than carrying a second copy of it.

use std::sync::Arc;

use bytes::Bytes;
use smithy_server_runtime::{EmbeddedEmulator, MultiServiceRouter};

use crate::distribution_lifecycle::{awaited_deployment, call, API_VERSION};

/// The Region the origin bucket is created in, and so the Region an S3 origin's `DomainName`
/// addresses it at.
pub(crate) const REGION: &str = "us-east-1";

/// The service principal an origin access control makes CloudFront's origin request as, and
/// so the principal the origin bucket's policy grants the read to.
pub(crate) const CLOUDFRONT_PRINCIPAL: &str = "cloudfront.amazonaws.com";

/// The AWS-managed cache policy `Managed-CachingOptimized`, whose `DefaultTTL` is a day: a
/// behaviour under it holds in the edge cache what it is allowed to cache.
pub(crate) const CACHING_OPTIMIZED: &str = "658327ea-f89d-4fab-a63d-7e88639e58f6";

/// `X-Cache` for an answer the origin produced for this request, and for one the edge cache
/// held.
pub(crate) const MISS: &str = "Miss from cloudfront";
pub(crate) const HIT: &str = "Hit from cloudfront";

// ---------------------------------------------------------------------------
// The emulator: CloudFront plus the object store its origin lives in
// ---------------------------------------------------------------------------

/// CloudFront and Amazon S3 over ONE shared engine: the distribution the viewer reaches and
/// the object store its origin is are the same emulator's, so the answer the edge relays is
/// one an origin really produced.
pub(crate) struct Emulator {
    pub(crate) router: MultiServiceRouter,
    callback: EmbeddedEmulator,
}

impl Emulator {
    /// Both services mounted on one router over one engine.
    pub(crate) fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(
            service_com_amazonaws_cloudfront_cloudfront2020_05_31::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
            Arc::clone(&engine),
        ));
        let callback = EmbeddedEmulator::from_router(router.clone());
        Self { router, callback }
    }

    /// The Amazon S3 client, built by the one shared constructor over the in-process callback,
    /// so the bucket this suite stages is the one the edge's origin fetch reads.
    fn s3(&self) -> ::aws_sdk_s3::Client {
        service_common::s3_client::s3_client(&self.callback, REGION)
    }

    /// Create `bucket`. It carries no public access of any kind: what reaches the object is
    /// the grant [`Emulator::granted_to_cloudfront`] writes.
    pub(crate) async fn staged_bucket(&self, case: &str, bucket: &str) {
        self.s3()
            .create_bucket()
            .bucket(bucket)
            .send()
            .await
            .unwrap_or_else(|error| {
                panic!("case {case:?}: creating {bucket} must succeed, got {error:?}")
            });
    }

    /// Put `body` at `key` in `bucket`, under the statements about the object `Staged` carries.
    pub(crate) async fn staged_object(&self, case: &str, bucket: &str, staged: &Staged) {
        let mut put = self
            .s3()
            .put_object()
            .bucket(bucket)
            .key(staged.key)
            .body(::aws_sdk_s3::primitives::ByteStream::from(
                staged.body.as_bytes().to_vec(),
            ));
        if let Some(content_type) = staged.content_type {
            put = put.content_type(content_type);
        }
        if let Some(cache_control) = staged.cache_control {
            put = put.cache_control(cache_control);
        }
        if let Some(disposition) = staged.content_disposition {
            put = put.content_disposition(disposition);
        }
        for (name, value) in staged.metadata {
            put = put.metadata(*name, *value);
        }
        put.send().await.unwrap_or_else(|error| {
            panic!(
                "case {case:?}: putting {bucket}/{} must succeed, got {error:?}",
                staged.key
            )
        });
    }

    /// The `ETag` Amazon S3 itself reports for `bucket`/`key`: read from the ORIGIN, so what
    /// the viewer is held to is the origin's own value rather than one this suite wrote.
    pub(crate) async fn origin_etag(&self, case: &str, bucket: &str, key: &str) -> String {
        self.s3()
            .head_object()
            .bucket(bucket)
            .key(key)
            .send()
            .await
            .unwrap_or_else(|error| {
                panic!("case {case:?}: reading {bucket}/{key} must succeed, got {error:?}")
            })
            .e_tag()
            .unwrap_or_default()
            .to_string()
    }

    /// Put the policy AWS publishes for an origin access control on `bucket`: the CloudFront
    /// service principal may read its objects, for the one distribution `arn` names.
    pub(crate) async fn granted_to_cloudfront(&self, case: &str, bucket: &str, arn: &str) {
        let document = format!(
            "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\
             \"Sid\":\"AllowCloudFrontServicePrincipal\",\"Effect\":\"Allow\",\
             \"Principal\":{{\"Service\":\"{CLOUDFRONT_PRINCIPAL}\"}},\
             \"Action\":\"s3:GetObject\",\"Resource\":\"arn:aws:s3:::{bucket}/*\",\
             \"Condition\":{{\"StringEquals\":{{\"AWS:SourceArn\":\"{arn}\"}}}}}}]}}"
        );
        self.s3()
            .put_bucket_policy()
            .bucket(bucket)
            .policy(document)
            .send()
            .await
            .unwrap_or_else(|error| {
                panic!("case {case:?}: the policy of {bucket} must be stored, got {error:?}")
            });
    }
}

/// One object an origin bucket holds, and what the origin says about it.
#[derive(Debug, Clone)]
pub(crate) struct Staged {
    pub(crate) key: &'static str,
    pub(crate) body: &'static str,
    pub(crate) content_type: Option<&'static str>,
    pub(crate) cache_control: Option<&'static str>,
    pub(crate) content_disposition: Option<&'static str>,
    /// The user metadata the object carries, which Amazon S3 answers under the
    /// `x-amz-meta-` prefix its model binds the map to.
    pub(crate) metadata: &'static [(&'static str, &'static str)],
}

impl Staged {
    /// An object stating nothing but its bytes.
    pub(crate) const fn plain(key: &'static str, body: &'static str) -> Self {
        Self {
            key,
            body,
            content_type: None,
            cache_control: None,
            content_disposition: None,
            metadata: &[],
        }
    }
}

/// The host a bucket of this Region is addressed at virtual-hosted style, which is what an S3
/// origin's `DomainName` carries.
pub(crate) fn origin_domain(bucket: &str) -> String {
    format!("{bucket}.s3.{REGION}.amazonaws.com")
}

/// `CreateOriginAccessControl` for an `s3` origin CloudFront signs every request to, answering
/// the control's id.
pub(crate) async fn create_origin_access_control(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
) -> String {
    let created = call(
        router,
        "POST",
        &format!("/{API_VERSION}/origin-access-control"),
        Some(&format!(
            "<OriginAccessControlConfig \
             xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
             <Name>{name}</Name>\
             <SigningProtocol>sigv4</SigningProtocol>\
             <SigningBehavior>always</SigningBehavior>\
             <OriginAccessControlOriginType>s3</OriginAccessControlOriginType>\
             </OriginAccessControlConfig>"
        )),
        None,
    )
    .await;
    assert_eq!(
        created.status, 201,
        "case {case:?}: CreateOriginAccessControl answered {}: {}",
        created.status, created.raw
    );
    created
        .root()
        .find_text("Id")
        .expect("the created origin access control reports an id")
        .to_string()
}

/// `CreateDistribution` over the S3 origin `bucket` fronted by the origin access control
/// `control`, with `behavior_extra` inside its default cache behaviour; answers the
/// distribution's `(Id, DomainName, ARN)`.
pub(crate) async fn create_distribution_over_bucket(
    router: &MultiServiceRouter,
    case: &str,
    caller_reference: &str,
    bucket: &str,
    control: &str,
    behavior_extra: &str,
) -> (String, String, String) {
    let created = call(
        router,
        "POST",
        &format!("/{API_VERSION}/distribution"),
        Some(&format!(
            "<DistributionConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
             <CallerReference>{caller_reference}</CallerReference>\
             <Origins><Quantity>1</Quantity><Items><Origin>\
             <Id>origin-bucket</Id><DomainName>{}</DomainName>\
             <S3OriginConfig><OriginAccessIdentity></OriginAccessIdentity></S3OriginConfig>\
             <OriginAccessControlId>{control}</OriginAccessControlId>\
             </Origin></Items></Origins>\
             <DefaultCacheBehavior><TargetOriginId>origin-bucket</TargetOriginId>\
             <ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy>\
             <AllowedMethods><Quantity>2</Quantity>\
             <Items><Method>GET</Method><Method>HEAD</Method></Items></AllowedMethods>\
             {behavior_extra}\
             </DefaultCacheBehavior>\
             <Comment>{caller_reference}</Comment>\
             <Enabled>true</Enabled>\
             </DistributionConfig>",
            origin_domain(bucket)
        )),
        None,
    )
    .await;
    assert_eq!(
        created.status, 201,
        "case {case:?}: CreateDistribution answered {}: {}",
        created.status, created.raw
    );
    let read = |member: &str| {
        created
            .root()
            .find_text(member)
            .unwrap_or_else(|| panic!("case {case:?}: the created distribution reports no {member}"))
            .to_string()
    };
    (read("Id"), read("DomainName"), read("ARN"))
}

// ---------------------------------------------------------------------------
// Viewing
// ---------------------------------------------------------------------------

/// What the edge answered one viewer request with, every header included: the sibling
/// `edge_signed_access` viewer reports the cache outcome alone, and the rule under test here
/// is about the headers themselves.
#[derive(Debug)]
pub(crate) struct Viewed {
    pub(crate) status: u16,
    /// Every response header, lowercased, in the order the answer carried them.
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) body: String,
}

impl Viewed {
    /// The first value the answer carries for `name`, or `None` when it carries none.
    pub(crate) fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(held, _)| held == name)
            .map(|(_, value)| value.as_str())
    }

    /// `X-Cache`, which is what the edge says produced this answer.
    pub(crate) fn x_cache(&self) -> &str {
        self.header("x-cache").unwrap_or_default()
    }
}

/// Send one VIEWER request: no AWS credential, addressed to `host` — which is what resolves
/// the distribution — exactly as a browser reaches one.
pub(crate) async fn view(
    router: &MultiServiceRouter,
    method: &str,
    host: &str,
    path_and_query: &str,
    headers: &[(&str, &str)],
) -> Viewed {
    let mut builder = ::http::Request::builder()
        .method(method)
        .uri(path_and_query)
        .header("host", host);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let request = builder
        .body(Bytes::new())
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let headers = response
        .headers()
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_ascii_lowercase(),
                String::from_utf8_lossy(value.as_bytes()).to_string(),
            )
        })
        .collect();
    let body = String::from_utf8_lossy(&response.into_body()).to_string();
    Viewed {
        status,
        headers,
        body,
    }
}

// ---------------------------------------------------------------------------
// The object under test
// ---------------------------------------------------------------------------

/// The object every row below reads: it states each kind of thing an origin says about an
/// object, so one fetch produces every header the rule is asserted over.
const OBJECT: Staged = Staged {
    key: "reports/quarter.txt",
    body: "the origin's own bytes",
    content_type: Some("text/plain"),
    cache_control: Some("max-age=120"),
    content_disposition: Some("inline; filename=\"quarter.txt\""),
    metadata: &[
        ("object-note", "stated by the origin"),
        ("object-owner", "the reporting team"),
    ],
};

/// What the viewer must see for one header, and why.
#[derive(Debug, Clone, Copy)]
enum Expect {
    /// The origin stated this header, so the viewer receives the ORIGIN's value.
    Origin(&'static str),
    /// CloudFront states this header itself, so the viewer receives CLOUDFRONT's value and
    /// never an origin's. The value is this view's own cache outcome, which differs between
    /// the answer the origin produced and the one the cache held.
    CacheOutcome,
    /// Nothing states this header, so the answer carries none.
    Absent,
}

/// The table: one row per header the rule decides, spanning the kinds of statement an origin
/// makes about an object and the headers CloudFront owns.
fn rows() -> Vec<(&'static str, &'static str, Expect)> {
    vec![
        (
            "the media type the origin stated",
            "content-type",
            Expect::Origin("text/plain"),
        ),
        (
            "the cache directive the origin stated",
            "cache-control",
            Expect::Origin("max-age=120"),
        ),
        (
            "the disposition the origin stated",
            "content-disposition",
            Expect::Origin("inline; filename=\"quarter.txt\""),
        ),
        (
            "a piece of the object's user metadata",
            "x-amz-meta-object-note",
            Expect::Origin("stated by the origin"),
        ),
        (
            "another piece of it, under its own name",
            "x-amz-meta-object-owner",
            Expect::Origin("the reporting team"),
        ),
        (
            "the cache outcome, which CloudFront states itself",
            "x-cache",
            Expect::CacheOutcome,
        ),
        (
            "a hop-by-hop header, which describes one connection",
            "connection",
            Expect::Absent,
        ),
        (
            "a header no one stated",
            "x-not-stated-by-anyone",
            Expect::Absent,
        ),
    ]
}

/// Every header the origin stated about the object reaches the viewer, on the answer the
/// ORIGIN produced and on the one the EDGE CACHE held, while the headers CloudFront states
/// itself are CloudFront's.
#[tokio::test]
async fn the_edge_serves_the_origins_own_response_headers() {
    let emulator = Emulator::new();
    let case = "an object stating every kind of header";
    let bucket = "edge-origin-headers";
    emulator.staged_bucket(case, bucket).await;
    emulator.staged_object(case, bucket, &OBJECT).await;
    let control = create_origin_access_control(&emulator.router, case, "edge-origin-headers").await;
    // A CACHING behaviour, so the second view of the object is answered out of the edge cache
    // and the rule can be asserted on a hit as well as on a miss.
    let (id, domain, arn) = create_distribution_over_bucket(
        &emulator.router,
        case,
        "edge-origin-headers",
        bucket,
        &control,
        &format!("<CachePolicyId>{CACHING_OPTIMIZED}</CachePolicyId>"),
    )
    .await;
    emulator.granted_to_cloudfront(case, bucket, &arn).await;
    awaited_deployment(&emulator.router, case, &id).await;

    let etag = emulator.origin_etag(case, bucket, OBJECT.key).await;
    assert!(
        !etag.is_empty(),
        "case {case:?}: the origin must report an entity tag for the staged object"
    );

    let path = format!("/{}", OBJECT.key);
    // The first view goes to the origin, the second comes out of the edge cache: the rule holds
    // on both, which is what says the cache holds what the origin said about the object.
    for (outcome, expected_cache) in [("the origin's answer", MISS), ("the cached answer", HIT)] {
        let viewed = view(&emulator.router, "GET", &domain, &path, &[]).await;
        assert_eq!(
            viewed.status, 200,
            "case {outcome:?}: expected HTTP 200 from the edge, got {} with {:?}",
            viewed.status, viewed.body
        );
        assert_eq!(
            viewed.body, OBJECT.body,
            "case {outcome:?}: expected the origin's bytes {:?}, got {:?}",
            OBJECT.body, viewed.body
        );
        assert_eq!(
            viewed.x_cache(),
            expected_cache,
            "case {outcome:?}: expected X-Cache {expected_cache:?}, got {:?}",
            viewed.x_cache()
        );
        for (row, header, expected) in rows() {
            let actual = viewed.header(header);
            let expected = match expected {
                Expect::Origin(value) => Some(value),
                // The cache outcome is the one this view really has, whichever row states it.
                Expect::CacheOutcome => Some(expected_cache),
                Expect::Absent => None,
            };
            assert_eq!(
                actual, expected,
                "case {outcome:?}/{row:?} ({header:?}): expected {expected:?}, got {actual:?} \
                 (headers {:?})",
                viewed.headers
            );
        }
        // The ENTITY TAG the origin minted for the object reaches the viewer too, and it is the
        // origin's own value — the one Amazon S3 reports for that object — not one CloudFront
        // invented.
        let served = viewed.header("etag");
        assert_eq!(
            served,
            Some(etag.as_str()),
            "case {outcome:?}: expected the origin's entity tag {etag:?}, got {served:?} \
             (headers {:?})",
            viewed.headers
        );
        // And the identifier of the request the edge served is the EDGE's, on every answer.
        assert!(
            viewed
                .header("x-amz-cf-id")
                .is_some_and(|value| !value.is_empty()),
            "case {outcome:?}: expected the edge's own request identifier, got {:?}",
            viewed.headers
        );
    }
}
