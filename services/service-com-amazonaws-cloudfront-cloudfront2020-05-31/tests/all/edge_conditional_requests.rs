// SPDX-License-Identifier: CC0-1.0

//! A viewer that already holds a copy of the object, end to end over HTTP with Amazon S3
//! mounted beside CloudFront on one engine.
//!
//! RULE: the edge serves the origin's validators through to the viewer, and judges a later
//! CONDITIONAL request against the representation it is about to serve — the entry its cache
//! held or the answer the origin has just produced — answering `304 Not Modified` with none of
//! the object's bytes when the copy the viewer holds is the current one (RFC 9110 §13:
//! `If-None-Match` by weak comparison over a list and the `*` that names any current
//! representation, decided on its own, and `If-Modified-Since` read only in its absence, at
//! one-second resolution, ignored when its date is later than the server's clock), and serving
//! the object whenever the condition holds.
//!
//! Only a suite that mounts the object store can state it: an entity tag and a last-modified
//! date are things the ORIGIN says about an object, so the validators judged here are the ones
//! Amazon S3 really stated for bytes a client really put in a bucket — nothing in this suite
//! writes a validator of its own.
//!
//! The rows span what the rule generalises over: which validator the viewer states, how the
//! entity tag is spelled (the object's own, another representation's, the wildcard, a list, the
//! weak marker), where the date sits relative to the object's own last-modified time and
//! relative to the clock, both conditions together, and whether the answer the condition is
//! judged against came out of the edge cache or from the origin.
//!
//! One case of the date rule cannot be stated through a live origin — a date strictly between
//! the instant the object was stored and the edge's own clock, the object here having been
//! stored in this same second — so the span of the date comparison is pinned where the clock is
//! a parameter, in `service::edge::conditional`'s own tests.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use smithy_server_runtime::{EmbeddedEmulator, MultiServiceRouter};

use crate::distribution_lifecycle::{awaited_deployment, call, API_VERSION};
use crate::edge_signed_s3_origin::mounted;

/// The Region the origin bucket is created in, and so the Region the S3 origin's `DomainName`
/// addresses it at.
const REGION: &str = "us-east-1";

/// The bucket both distributions below fetch from.
const BUCKET: &str = "cloudfront-conditional-origin";

/// The object every case is judged against, and its bytes.
const KEY: &str = "conditional/obj.txt";
const BODY: &str = "the object a conditional viewer already holds";

/// An entity tag of NO representation of that object: a well-formed quoted tag the origin
/// cannot have stated, since S3 states the digest of the bytes.
const ANOTHER_REPRESENTATION: &str = "\"00000000000000000000000000000000\"";

/// The AWS-managed cache policy `Managed-CachingOptimized`, whose `DefaultTTL` is a day: the
/// behaviour under it answers a second request for the same key out of the edge cache.
const CACHING_OPTIMIZED: &str = "658327ea-f89d-4fab-a63d-7e88639e58f6";

/// The AWS-managed cache policy `Managed-CachingDisabled`, whose TTLs are all zero: every
/// request under it goes to the origin, so the condition is judged against an answer the origin
/// has just produced.
const CACHING_DISABLED: &str = "4135ea2d-6df8-44a3-9df3-4b5a84be39ad";

/// `X-Cache` for an answer the edge cache held, and for one the origin produced.
const HIT: &str = "Hit from cloudfront";
const MISS: &str = "Miss from cloudfront";

/// The status a viewer whose copy is current receives, and the one that carries the object.
const NOT_MODIFIED: u16 = 304;
const OK: u16 = 200;

// ---------------------------------------------------------------------------
// The emulator: CloudFront plus the object store its origin lives in
// ---------------------------------------------------------------------------

/// CloudFront and Amazon S3 over ONE shared engine, mounted through the one two-service
/// constructor this target has. A viewer presents no credential, so this suite's plane is the
/// unauthenticated one and the origin's own policy is what admits the edge's fetch.
struct Emulator {
    router: MultiServiceRouter,
    callback: EmbeddedEmulator,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let router = mounted(&engine, false);
        let callback = EmbeddedEmulator::from_router(router.clone());
        Self { router, callback }
    }

    /// The Amazon S3 client, built by the one shared constructor over the in-process callback,
    /// so the object this suite stages is the one the edge's origin fetch reads.
    fn s3(&self) -> ::aws_sdk_s3::Client {
        service_common::s3_client::s3_client(&self.callback, REGION)
    }

    /// The bucket, the object and the policy that lets the edge's unsigned fetch read it.
    async fn staged_object(&self) {
        let s3 = self.s3();
        s3.create_bucket()
            .bucket(BUCKET)
            .send()
            .await
            .unwrap_or_else(|error| panic!("creating {BUCKET} must succeed, got {error:?}"));
        s3.put_object()
            .bucket(BUCKET)
            .key(KEY)
            .content_type("text/plain")
            .body(::aws_sdk_s3::primitives::ByteStream::from(
                BODY.as_bytes().to_vec(),
            ))
            .send()
            .await
            .unwrap_or_else(|error| panic!("putting {BUCKET}/{KEY} must succeed, got {error:?}"));
        s3.put_public_access_block()
            .bucket(BUCKET)
            .public_access_block_configuration(
                ::aws_sdk_s3::types::PublicAccessBlockConfiguration::builder()
                    .block_public_acls(false)
                    .ignore_public_acls(false)
                    .block_public_policy(false)
                    .restrict_public_buckets(false)
                    .build(),
            )
            .send()
            .await
            .unwrap_or_else(|error| {
                panic!("the public access settings of {BUCKET} must be stored, got {error:?}")
            });
        s3.put_bucket_policy()
            .bucket(BUCKET)
            .policy(format!(
                "{{\"Version\":\"2012-10-17\",\"Statement\":[\
                 {{\"Sid\":\"PublicRead\",\"Effect\":\"Allow\",\"Principal\":\"*\",\
                 \"Action\":\"s3:GetObject\",\"Resource\":\"arn:aws:s3:::{BUCKET}/*\"}}]}}"
            ))
            .send()
            .await
            .unwrap_or_else(|error| panic!("the policy of {BUCKET} must be stored, got {error:?}"));
    }
}

/// What the edge answered one viewer request with.
struct Viewed {
    status: u16,
    x_cache: String,
    etag: String,
    last_modified: String,
    content_type: String,
    body: Vec<u8>,
}

/// Send one VIEWER request: no AWS credential, addressed to `host` — which is what resolves the
/// distribution — carrying `headers`, exactly as a browser revalidating its cache does.
async fn view(
    router: &MultiServiceRouter,
    host: &str,
    path: &str,
    headers: &[(&str, &str)],
) -> Viewed {
    let mut builder = ::http::Request::builder()
        .method("GET")
        .uri(path)
        .header("host", host);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let request = builder
        .body(::bytes::Bytes::new())
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let header = |name: &str| {
        response
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string()
    };
    let (x_cache, etag, last_modified, content_type) = (
        header("x-cache"),
        header("etag"),
        header("last-modified"),
        header("content-type"),
    );
    Viewed {
        status,
        x_cache,
        etag,
        last_modified,
        content_type,
        body: response.into_body().to_vec(),
    }
}

/// `CreateDistribution` over the bucket's REST endpoint under `cache_policy`; answers the
/// distribution's `DomainName` once the clock has deployed it.
async fn deployed(
    router: &MultiServiceRouter,
    caller_reference: &str,
    cache_policy: &str,
) -> String {
    let created = call(
        router,
        "POST",
        &format!("/{API_VERSION}/distribution"),
        Some(&format!(
            "<DistributionConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
             <CallerReference>{caller_reference}</CallerReference>\
             <Origins><Quantity>1</Quantity><Items><Origin>\
             <Id>origin-bucket</Id><DomainName>{BUCKET}.s3.{REGION}.amazonaws.com</DomainName>\
             <S3OriginConfig><OriginAccessIdentity></OriginAccessIdentity></S3OriginConfig>\
             </Origin></Items></Origins>\
             <DefaultCacheBehavior><TargetOriginId>origin-bucket</TargetOriginId>\
             <ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy>\
             <AllowedMethods><Quantity>2</Quantity>\
             <Items><Method>GET</Method><Method>HEAD</Method></Items>\
             <CachedMethods><Quantity>2</Quantity>\
             <Items><Method>GET</Method><Method>HEAD</Method></Items></CachedMethods>\
             </AllowedMethods>\
             <CachePolicyId>{cache_policy}</CachePolicyId>\
             </DefaultCacheBehavior>\
             <CacheBehaviors><Quantity>0</Quantity></CacheBehaviors>\
             <Comment>conditional requests at the edge</Comment>\
             <Enabled>true</Enabled>\
             </DistributionConfig>"
        )),
        None,
    )
    .await;
    assert_eq!(
        created.status, 201,
        "CreateDistribution answered {}: {}",
        created.status, created.raw
    );
    let id = created
        .root()
        .find_text("Id")
        .expect("the created distribution reports an id")
        .to_string();
    let domain = created
        .root()
        .find_text("DomainName")
        .expect("the created distribution reports a domain name")
        .to_string();
    awaited_deployment(router, caller_reference, &id).await;
    domain
}

/// What one row expects of the edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expect {
    /// The copy the viewer holds is current: `304` carrying none of the object.
    Current,
    /// The condition holds: the object itself.
    Served,
}

/// One row of the table: what it states, the conditional headers the viewer sends, and what the
/// edge is to answer.
struct Row {
    case: &'static str,
    headers: Vec<(&'static str, String)>,
    expected: Expect,
}

/// The conditional headers of one row, built from the validators the ORIGIN stated for the
/// object — so no row carries a validator of its own.
fn rows(etag: &str, last_modified: &str) -> Vec<Row> {
    let since = |shift: i64| {
        let stated = ::httpdate::parse_http_date(last_modified)
            .expect("the origin states its last-modified time as an HTTP-date");
        let shifted = if shift >= 0 {
            stated + Duration::from_secs(u64::try_from(shift).unwrap_or_default())
        } else {
            stated - Duration::from_secs(u64::try_from(-shift).unwrap_or_default())
        };
        ::httpdate::fmt_http_date(shifted)
    };
    let none_match = |value: String| vec![("if-none-match", value)];
    let modified_since = |value: String| vec![("if-modified-since", value)];
    vec![
        Row {
            case: "the object's own entity tag",
            headers: none_match(etag.to_string()),
            expected: Expect::Current,
        },
        Row {
            case: "the entity tag of another representation",
            headers: none_match(ANOTHER_REPRESENTATION.to_string()),
            expected: Expect::Served,
        },
        Row {
            case: "the wildcard, which names any current representation",
            headers: none_match("*".to_string()),
            expected: Expect::Current,
        },
        Row {
            case: "a list holding the object's own entity tag",
            headers: none_match(format!("{ANOTHER_REPRESENTATION}, {etag}")),
            expected: Expect::Current,
        },
        Row {
            case: "a list holding no entity tag of the object",
            headers: none_match(format!("{ANOTHER_REPRESENTATION}, \"another\"")),
            expected: Expect::Served,
        },
        Row {
            case: "the object's own entity tag marked weak",
            headers: none_match(format!("W/{etag}")),
            expected: Expect::Current,
        },
        Row {
            case: "the instant the object was last modified at",
            headers: modified_since(since(0)),
            expected: Expect::Current,
        },
        Row {
            case: "an instant before the object was last modified",
            headers: modified_since(since(-3600)),
            expected: Expect::Served,
        },
        Row {
            case: "a date later than the edge's own clock",
            headers: modified_since(::httpdate::fmt_http_date(
                SystemTime::now() + Duration::from_secs(86_400),
            )),
            expected: Expect::Served,
        },
        Row {
            case: "a value that is no HTTP-date at all",
            headers: modified_since("yesterday".to_string()),
            expected: Expect::Served,
        },
        Row {
            case: "an entity tag that does not hold, beside a date that would answer not-modified",
            headers: vec![
                ("if-none-match", ANOTHER_REPRESENTATION.to_string()),
                ("if-modified-since", since(0)),
            ],
            expected: Expect::Served,
        },
        Row {
            case: "the object's own entity tag, beside a date that would serve the object",
            headers: vec![
                ("if-none-match", etag.to_string()),
                ("if-modified-since", since(-3600)),
            ],
            expected: Expect::Current,
        },
    ]
}

/// Assert one row: the status, the bytes, and the validators a `304` keeps.
fn assert_row(case: &str, expected: Expect, answer: &Viewed) {
    match expected {
        Expect::Current => {
            assert_eq!(
                answer.status, NOT_MODIFIED,
                "case {case:?}: expected {NOT_MODIFIED}, got {} with {} bytes",
                answer.status,
                answer.body.len()
            );
            assert!(
                answer.body.is_empty(),
                "case {case:?}: expected none of the object's bytes, got {:?}",
                String::from_utf8_lossy(&answer.body)
            );
            assert!(
                !answer.etag.is_empty(),
                "case {case:?}: expected the representation's entity tag to be kept, got none"
            );
            assert!(
                answer.content_type.is_empty(),
                "case {case:?}: a 304 carries no body, so it states no media type; got {:?}",
                answer.content_type
            );
        }
        Expect::Served => {
            assert_eq!(
                answer.status, OK,
                "case {case:?}: expected {OK} with the object, got {} with {} bytes",
                answer.status,
                answer.body.len()
            );
            assert_eq!(
                answer.body,
                BODY.as_bytes(),
                "case {case:?}: expected the object's own bytes, got {:?}",
                String::from_utf8_lossy(&answer.body)
            );
        }
    }
}

/// The whole rule over an answer the EDGE CACHE holds: the cold fetch states the validators,
/// and every row below is judged by the edge against the entry that fetch filled.
#[tokio::test]
async fn the_edge_answers_a_conditional_request_against_the_object_it_holds() {
    let emulator = Emulator::new();
    emulator.staged_object().await;
    let domain = deployed(&emulator.router, "edge-conditional-cached", CACHING_OPTIMIZED).await;

    // The cold fetch: the origin's own answer, carrying the validators the rows are built from.
    let cold = view(&emulator.router, &domain, &format!("/{KEY}"), &[]).await;
    assert_eq!(
        (cold.status, cold.x_cache.as_str()),
        (OK, MISS),
        "expected the origin's {OK} with X-Cache {MISS:?}, got {} with {:?}",
        cold.status,
        cold.x_cache
    );
    assert!(
        cold.etag.starts_with('"') && cold.etag.ends_with('"') && cold.etag.len() > 2,
        "expected the origin's quoted entity tag to reach the viewer, got {:?} (headers: \
         last-modified {:?})",
        cold.etag,
        cold.last_modified
    );
    assert!(
        ::httpdate::parse_http_date(&cold.last_modified).is_ok(),
        "expected the origin's last-modified date to reach the viewer, got {:?}",
        cold.last_modified
    );

    for row in rows(&cold.etag, &cold.last_modified) {
        let stated: Vec<(&str, &str)> = row
            .headers
            .iter()
            .map(|(name, value)| (*name, value.as_str()))
            .collect();
        let answer = view(&emulator.router, &domain, &format!("/{KEY}"), &stated).await;
        assert_eq!(
            answer.x_cache, HIT,
            "case {:?}: expected the answer to come from the edge cache, got X-Cache {:?} \
             (status {})",
            row.case, answer.x_cache, answer.status
        );
        assert_row(row.case, row.expected, &answer);
    }

    // The entry still holds the COMPLETE representation: a viewer that states no condition is
    // answered all of it, so no revalidation became what the cache holds.
    let whole = view(&emulator.router, &domain, &format!("/{KEY}"), &[]).await;
    assert_eq!(
        (whole.status, whole.body.as_slice(), whole.etag.as_str()),
        (OK, BODY.as_bytes(), cold.etag.as_str()),
        "expected the whole object under its own entity tag, got {} with {} bytes and {:?}",
        whole.status,
        whole.body.len(),
        whole.etag
    );
}

/// The same rule where the ORIGIN produced the answer: a behaviour that caches nothing goes to
/// the origin every time, and the condition is judged against what it has just answered.
#[tokio::test]
async fn the_edge_answers_a_conditional_request_against_what_the_origin_just_answered() {
    let emulator = Emulator::new();
    emulator.staged_object().await;
    let domain = deployed(
        &emulator.router,
        "edge-conditional-uncached",
        CACHING_DISABLED,
    )
    .await;

    let cold = view(&emulator.router, &domain, &format!("/{KEY}"), &[]).await;
    assert_eq!(
        (cold.status, cold.x_cache.as_str()),
        (OK, MISS),
        "expected the origin's {OK} with X-Cache {MISS:?}, got {} with {:?}",
        cold.status,
        cold.x_cache
    );

    for row in rows(&cold.etag, &cold.last_modified) {
        let stated: Vec<(&str, &str)> = row
            .headers
            .iter()
            .map(|(name, value)| (*name, value.as_str()))
            .collect();
        let answer = view(&emulator.router, &domain, &format!("/{KEY}"), &stated).await;
        assert_eq!(
            answer.x_cache, MISS,
            "case {:?}: this behaviour caches nothing, so the answer comes from the origin; \
             got X-Cache {:?} (status {})",
            row.case, answer.x_cache, answer.status
        );
        assert_row(row.case, row.expected, &answer);
    }
}
