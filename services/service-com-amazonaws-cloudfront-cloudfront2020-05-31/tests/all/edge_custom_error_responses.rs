// SPDX-License-Identifier: CC0-1.0

//! The CUSTOM ERROR RESPONSES a distribution states, end to end over HTTP with Amazon S3
//! mounted beside CloudFront on one engine.
//!
//! RULE: `DistributionConfig$CustomErrorResponses` "controls whether CloudFront replaces HTTP
//! status codes in the 4xx and 5xx range with custom error messages before returning the
//! response to the viewer" and "how long CloudFront caches HTTP status codes in the 4xx and 5xx
//! range" — so when the answer the edge reached on an origin's behalf carries a status an entry
//! names in its `ErrorCode`, the viewer receives that entry's `ResponseCode` with the bytes of
//! the page its `ResponsePagePath` names (fetched through the behaviour whose path pattern
//! matches that path), and the error itself is held for the entry's `ErrorCachingMinTTL` rather
//! than for whatever the cache policy would hold. A status no entry names is served exactly as
//! the origin gave it.
//!
//! Only a suite that mounts the object store can state it: the error being rewritten has to be
//! an ORIGIN's own, and the page has to be an object a client really put somewhere — both of
//! which are Amazon S3's to produce here. The status the origin answers for a key it does not
//! hold is read off a CONTROL distribution that states no custom error response at all, so no
//! row here fixes what that status is.
//!
//! The rows span what the rule generalises over: the status the entry names and the one it
//! replaces it with, which page it names, a path the origin serves (so the configuration
//! replaces nothing), a status the configuration does not name, and the caching duration — zero
//! against a long span, which is what decides whether a later request still sees the error after
//! the origin has gained the object.

use std::sync::Arc;

use smithy_server_runtime::{EmbeddedEmulator, MultiServiceRouter};

use crate::distribution_lifecycle::{awaited_deployment, call, API_VERSION};
use crate::edge_signed_s3_origin::mounted;

/// The Region the origin bucket is created in, and so the Region the S3 origin's `DomainName`
/// addresses it at.
const REGION: &str = "us-east-1";

/// The bucket every distribution below fetches from.
const BUCKET: &str = "cloudfront-custom-error-origin";

/// The object that IS there, so a row can state what the configuration leaves alone.
const PRESENT_KEY: &str = "page.txt";
const PRESENT_BODY: &str = "the object itself";

/// The two custom error pages, and their bytes.
const FORBIDDEN_PAGE: &str = "errors/forbidden.html";
const FORBIDDEN_PAGE_BODY: &str = "<html>the branded forbidden page</html>";
const MISSING_PAGE: &str = "errors/missing.html";
const MISSING_PAGE_BODY: &str = "<html>the branded missing page</html>";

/// The keys the bucket does NOT hold: one per distribution, so no row is answered out of
/// another's cache entry.
const ABSENT_KEY: &str = "absent/obj.txt";
const ABSENT_UNCONFIGURED_KEY: &str = "absent/unconfigured.txt";
const LATER_HELD_KEY: &str = "absent/later-held.txt";
const LATER_FRESH_KEY: &str = "absent/later-fresh.txt";

/// The bytes the origin gains for the two keys the caching rows ask for twice.
const LATER_BODY: &str = "the object the origin gained";

/// The AWS-managed cache policy `Managed-CachingOptimized`, whose `DefaultTTL` is a day: under
/// it the edge may hold what it is allowed to hold, so what decides how long an ERROR is held is
/// the configuration's own `ErrorCachingMinTTL`.
const CACHING_OPTIMIZED: &str = "658327ea-f89d-4fab-a63d-7e88639e58f6";

/// The statuses Amazon S3 can answer a read of a key it does not hold with: `404` when the
/// reader may list the bucket and `403` when it may not (Amazon S3 "Error responses"). Which one
/// this origin gives is read off the control distribution rather than fixed here, and the
/// configuration under test names both.
const FORBIDDEN: u16 = 403;
const NOT_FOUND: u16 = 404;

/// The `ResponseCode` each entry replaces its status with: a `200` for the forbidden page, which
/// is the substitution `ResponseCode` documents ("you might want to return a 200 status code
/// (OK) and static website so your customers don't know that your website is down"), and a `410`
/// for the missing page, so the two rows are told apart by the status as well as by the bytes.
const FORBIDDEN_RESPONSE_CODE: u16 = 200;
const NOT_FOUND_RESPONSE_CODE: u16 = 410;

/// `X-Cache` for an answer CloudFront itself produced, which a custom error response is.
const ERROR: &str = "Error from cloudfront";

/// A member Amazon S3's error document carries, so a row can say the body it received is the
/// ORIGIN's raw error rather than a page.
const ORIGIN_ERROR_ENVELOPE: &str = "<Code>";

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
    /// so the objects this suite stages are the ones the edge's origin fetch reads.
    fn s3(&self) -> ::aws_sdk_s3::Client {
        service_common::s3_client::s3_client(&self.callback, REGION)
    }

    /// The bucket, the object, the two error pages, and the policy that lets the edge's unsigned
    /// fetch read them.
    async fn staged_site(&self) {
        let s3 = self.s3();
        s3.create_bucket()
            .bucket(BUCKET)
            .send()
            .await
            .unwrap_or_else(|error| panic!("creating {BUCKET} must succeed, got {error:?}"));
        for (key, body, media_type) in [
            (PRESENT_KEY, PRESENT_BODY, "text/plain"),
            (FORBIDDEN_PAGE, FORBIDDEN_PAGE_BODY, "text/html"),
            (MISSING_PAGE, MISSING_PAGE_BODY, "text/html"),
        ] {
            self.put(key, body, media_type).await;
        }
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

    /// Put one object in the bucket.
    async fn put(&self, key: &str, body: &str, media_type: &str) {
        self.s3()
            .put_object()
            .bucket(BUCKET)
            .key(key)
            .content_type(media_type)
            .body(::aws_sdk_s3::primitives::ByteStream::from(
                body.as_bytes().to_vec(),
            ))
            .send()
            .await
            .unwrap_or_else(|error| panic!("putting {BUCKET}/{key} must succeed, got {error:?}"));
    }
}

/// What the edge answered one viewer request with.
struct Viewed {
    status: u16,
    x_cache: String,
    body: String,
}

/// Send one VIEWER request: no AWS credential, addressed to `host`, which is what resolves the
/// distribution.
async fn view(router: &MultiServiceRouter, host: &str, path: &str) -> Viewed {
    let request = ::http::Request::builder()
        .method("GET")
        .uri(path)
        .header("host", host)
        .body(::bytes::Bytes::new())
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let x_cache = response
        .headers()
        .get("x-cache")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    Viewed {
        status,
        x_cache,
        body: String::from_utf8_lossy(&response.into_body()).to_string(),
    }
}

/// One `CustomErrorResponse` element.
fn entry(error_code: u16, page: &str, response_code: u16, caching_ttl: u64) -> String {
    format!(
        "<CustomErrorResponse><ErrorCode>{error_code}</ErrorCode>\
         <ResponsePagePath>/{page}</ResponsePagePath>\
         <ResponseCode>{response_code}</ResponseCode>\
         <ErrorCachingMinTTL>{caching_ttl}</ErrorCachingMinTTL></CustomErrorResponse>"
    )
}

/// The `CustomErrorResponses` element naming both statuses a read of an absent key can earn,
/// each with its own page and its own replacement status, held for `caching_ttl` seconds.
fn custom_error_responses(caching_ttl: u64) -> String {
    format!(
        "<CustomErrorResponses><Quantity>2</Quantity><Items>{}{}</Items></CustomErrorResponses>",
        entry(
            FORBIDDEN,
            FORBIDDEN_PAGE,
            FORBIDDEN_RESPONSE_CODE,
            caching_ttl
        ),
        entry(
            NOT_FOUND,
            MISSING_PAGE,
            NOT_FOUND_RESPONSE_CODE,
            caching_ttl
        ),
    )
}

/// `CreateDistribution` over the bucket's REST endpoint, carrying `custom_errors` as its
/// `CustomErrorResponses`; answers the distribution's `DomainName` once the clock has deployed
/// it.
async fn deployed(
    router: &MultiServiceRouter,
    caller_reference: &str,
    custom_errors: &str,
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
             <CachePolicyId>{CACHING_OPTIMIZED}</CachePolicyId>\
             </DefaultCacheBehavior>\
             <CacheBehaviors><Quantity>0</Quantity></CacheBehaviors>\
             {custom_errors}\
             <Comment>custom error responses at the edge</Comment>\
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

/// The page and the replacement status the configuration states for the status the origin
/// answered.
fn stated_for(origin_status: u16) -> (u16, &'static str) {
    match origin_status {
        FORBIDDEN => (FORBIDDEN_RESPONSE_CODE, FORBIDDEN_PAGE_BODY),
        NOT_FOUND => (NOT_FOUND_RESPONSE_CODE, MISSING_PAGE_BODY),
        other => panic!(
            "the origin answered {other} for a key it does not hold, which is neither of the \
             statuses Amazon S3 documents for that read ({FORBIDDEN}, {NOT_FOUND}) — the \
             configuration under test names those two"
        ),
    }
}

/// The replacement: the status the configuration names is answered with its `ResponseCode` and
/// the bytes of its page, a path the origin serves is answered as the origin gave it, and a
/// status the configuration does not name is too.
#[tokio::test]
async fn the_edge_replaces_the_status_and_the_page_the_configuration_names() {
    let emulator = Emulator::new();
    emulator.staged_site().await;
    // The CONTROL: the same origin with no custom error response at all, which is what reads the
    // status the origin answers for a key it does not hold.
    let control = deployed(&emulator.router, "custom-error-control", "").await;
    let raw = view(&emulator.router, &control, &format!("/{ABSENT_KEY}")).await;
    assert!(
        raw.body.contains(ORIGIN_ERROR_ENVELOPE),
        "expected the origin's own error document through a distribution that states no custom \
         error response, got {} / {:?}",
        raw.status,
        raw.body
    );
    let (expected_status, expected_page) = stated_for(raw.status);

    let configured = deployed(
        &emulator.router,
        "custom-error-configured",
        &custom_error_responses(300),
    )
    .await;
    let errored = view(&emulator.router, &configured, &format!("/{ABSENT_KEY}")).await;
    assert_eq!(
        (errored.status, errored.body.as_str()),
        (expected_status, expected_page),
        "case \"the origin's {} replaced by the configuration\": expected {expected_status} with \
         the page the configuration names, got {} / {:?}",
        raw.status,
        errored.status,
        errored.body
    );
    assert_eq!(
        errored.x_cache, ERROR,
        "case \"the origin's {} replaced by the configuration\": CloudFront produced this \
         answer, so it reports {ERROR:?}; got {:?}",
        raw.status, errored.x_cache
    );

    // A path the origin SERVES: the configuration covers no status this answer carries.
    let served = view(&emulator.router, &configured, &format!("/{PRESENT_KEY}")).await;
    assert_eq!(
        (served.status, served.body.as_str()),
        (200, PRESENT_BODY),
        "case \"a path the origin serves\": expected the object itself, got {} / {:?}",
        served.status,
        served.body
    );

    // A distribution whose configuration names the OTHER status alone leaves this error as the
    // origin gave it: an entry covers the status it names and no other.
    let other_status = if raw.status == FORBIDDEN {
        NOT_FOUND
    } else {
        FORBIDDEN
    };
    let unconfigured = deployed(
        &emulator.router,
        "custom-error-other-status",
        &format!(
            "<CustomErrorResponses><Quantity>1</Quantity><Items>{}</Items>\
             </CustomErrorResponses>",
            entry(other_status, MISSING_PAGE, NOT_FOUND_RESPONSE_CODE, 300)
        ),
    )
    .await;
    let passed_through = view(
        &emulator.router,
        &unconfigured,
        &format!("/{ABSENT_UNCONFIGURED_KEY}"),
    )
    .await;
    assert_eq!(
        passed_through.status, raw.status,
        "case \"a status the configuration does not name\": expected the origin's own {}, got {} \
         / {:?}",
        raw.status, passed_through.status, passed_through.body
    );
    assert!(
        passed_through.body.contains(ORIGIN_ERROR_ENVELOPE),
        "case \"a status the configuration does not name\": expected the origin's own error \
         document, got {:?}",
        passed_through.body
    );
}

/// `ErrorCachingMinTTL`: the error is held for the span the entry states, so a later request
/// within it is answered without asking the origin again — and an entry that states no span at
/// all holds nothing, so the same later request sees the object the origin has since gained.
#[tokio::test]
async fn the_error_is_held_for_the_caching_duration_the_configuration_states() {
    let emulator = Emulator::new();
    emulator.staged_site().await;
    let held = deployed(
        &emulator.router,
        "custom-error-held",
        &custom_error_responses(3_600),
    )
    .await;
    let fresh = deployed(
        &emulator.router,
        "custom-error-not-held",
        &custom_error_responses(0),
    )
    .await;

    // Both distributions earn the error first, over keys the origin does not hold.
    let first_held = view(&emulator.router, &held, &format!("/{LATER_HELD_KEY}")).await;
    let first_fresh = view(&emulator.router, &fresh, &format!("/{LATER_FRESH_KEY}")).await;
    let (expected_status, expected_page) = stated_for(
        // The status the ORIGIN answered is the one the replacement was keyed on, which the
        // replacement itself no longer carries: both distributions state the same two entries,
        // so the pair of them tells which status that was.
        if first_held.status == FORBIDDEN_RESPONSE_CODE {
            FORBIDDEN
        } else {
            NOT_FOUND
        },
    );
    for (case, answer) in [("held", &first_held), ("not held", &first_fresh)] {
        assert_eq!(
            (answer.status, answer.body.as_str()),
            (expected_status, expected_page),
            "case {case:?}: expected the custom error response first, got {} / {:?}",
            answer.status,
            answer.body
        );
    }

    // The origin now holds both keys.
    emulator.put(LATER_HELD_KEY, LATER_BODY, "text/plain").await;
    emulator.put(LATER_FRESH_KEY, LATER_BODY, "text/plain").await;

    // The distribution that holds the error for an hour still answers it; the one that holds it
    // for no time at all asks the origin again and serves what it now has.
    let again_held = view(&emulator.router, &held, &format!("/{LATER_HELD_KEY}")).await;
    assert_eq!(
        (again_held.status, again_held.body.as_str()),
        (expected_status, expected_page),
        "case \"an error held for an hour\": expected the error to be held, got {} / {:?}",
        again_held.status,
        again_held.body
    );
    let again_fresh = view(&emulator.router, &fresh, &format!("/{LATER_FRESH_KEY}")).await;
    assert_eq!(
        (again_fresh.status, again_fresh.body.as_str()),
        (200, LATER_BODY),
        "case \"an error held for no time at all\": expected the object the origin has since \
         gained, got {} / {:?}",
        again_fresh.status,
        again_fresh.body
    );
}
