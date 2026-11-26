// SPDX-License-Identifier: CC0-1.0

//! What the edge cache holds, as the OBJECT'S SIZE varies: an Amazon S3 origin's objects
//! fetched twice through a distribution, end to end over HTTP with Amazon S3 mounted beside
//! CloudFront on one engine.
//!
//! RULE: whether the edge may serve an answer out of its cache is decided by the matched cache
//! behaviour — the status the origin answered and the TTL the behaviour's cache policy grants —
//! and never by how many bytes the answer carries. So the second viewer request for an object
//! the behaviour caches is answered `X-Cache: Hit from cloudfront` with the same bytes the
//! first one was answered `Miss from cloudfront` with, whatever the object's size: `X-Cache` is
//! the only signal a client has for telling an edge-cache answer from an origin fetch, and a
//! multi-megabyte object is exactly what a real distribution mostly serves.
//!
//! The rows differ along the one thing the rule generalises over — the SIZE of the object —
//! spanning a handful of bytes, a size under a mebibyte, exactly a mebibyte, one byte past it,
//! and the several megabytes a media object runs to. The cache itself stays bounded, by the
//! bytes it holds as a whole rather than by any one object
//! (`crate::state::MAX_EDGE_CACHE_BYTES`): it evicts to make room for a new object instead of
//! refusing to hold it.
//!
//! The harness (the two planes over one engine, the staged bucket, the origin access control
//! and the bucket's grant to CloudFront) is [`crate::edge_signed_s3_origin`]'s, reused rather
//! than copied; this suite's distribution requires no signature of a viewer, so what each
//! request exercises is the cache rather than admission.

use smithy_server_runtime::MultiServiceRouter;

use crate::distribution_lifecycle::{awaited_deployment, call, API_VERSION};
use crate::edge_signed_s3_origin::{
    create_origin_access_control, grant, origin_domain, Emulator,
};

/// The AWS-managed cache policy `Managed-CachingOptimized`, whose `DefaultTTL` is a day: a
/// behaviour under it holds what it may hold. Its id is the one `MANAGED_CACHE_POLICIES`
/// publishes.
const CACHING_OPTIMIZED: &str = "658327ea-f89d-4fab-a63d-7e88639e58f6";

/// `X-Cache` for an answer the origin produced for this request.
const MISS: &str = "Miss from cloudfront";

/// `X-Cache` for an answer the edge cache held.
const HIT: &str = "Hit from cloudfront";

/// The bucket the objects of every row live in.
const BUCKET: &str = "large-object-cache-origin";

/// One row: what the object is called and how many bytes it carries.
#[derive(Debug, Clone, Copy)]
struct Row {
    case: &'static str,
    key: &'static str,
    bytes: usize,
}

/// A mebibyte, the size the rows are placed around.
const MEBIBYTE: usize = 1024 * 1024;

/// The object sizes this suite spans: from a handful of bytes to the several megabytes a media
/// object runs to, including the mebibyte boundary itself and the byte past it.
const ROWS: &[Row] = &[
    Row {
        case: "a handful of bytes",
        key: "tiny.txt",
        bytes: 11,
    },
    Row {
        case: "sixty-four kibibytes",
        key: "small.txt",
        bytes: 64 * 1024,
    },
    Row {
        case: "exactly a mebibyte",
        key: "mebibyte.txt",
        bytes: MEBIBYTE,
    },
    Row {
        case: "one byte past a mebibyte",
        key: "past-a-mebibyte.txt",
        bytes: MEBIBYTE + 1,
    },
    Row {
        case: "three mebibytes",
        key: "three-mebibytes.txt",
        bytes: 3 * MEBIBYTE,
    },
];

/// The bytes of the object a row stages: a deterministic, non-repeating-per-byte pattern, so a
/// body served from the cache that is the right LENGTH but not the right bytes fails the row.
fn object_bytes(row: &Row) -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    (0..row.bytes)
        .map(|at| char::from(ALPHABET[(at * 7 + row.key.len()) % ALPHABET.len()]))
        .collect()
}

/// What the edge answered one viewer request with, with the body kept as the BYTES it was: an
/// object compared as text could pass on bytes no client would have been served.
struct Viewed {
    status: u16,
    x_cache: String,
    body: Vec<u8>,
}

/// Send one VIEWER request: no credential of any kind, addressed to the distribution's own
/// domain name, which is what resolves it.
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
        body: response.into_body().to_vec(),
    }
}

/// `CreateDistribution` over the S3 origin `bucket` fronted by the origin access control
/// `control`, under the caching cache policy and requiring nothing of a viewer; answers
/// `(Id, DomainName, ARN)`.
async fn create_distribution(
    router: &MultiServiceRouter,
    case: &str,
    caller_reference: &str,
    bucket: &str,
    control: &str,
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
             <Items><Method>GET</Method><Method>HEAD</Method></Items>\
             <CachedMethods><Quantity>2</Quantity>\
             <Items><Method>GET</Method><Method>HEAD</Method></Items></CachedMethods>\
             </AllowedMethods>\
             <CachePolicyId>{CACHING_OPTIMIZED}</CachePolicyId>\
             </DefaultCacheBehavior>\
             <Comment>large object edge cache</Comment>\
             <Enabled>true</Enabled>\
             </DistributionConfig>",
            origin_domain(bucket)
        )),
        None,
    )
    .await;
    assert_eq!(
        created.status, 201,
        "case {case}: CreateDistribution answered {}: {}",
        created.status, created.raw
    );
    let read = |member: &str| {
        created
            .root()
            .find_text(member)
            .unwrap_or_else(|| panic!("case {case}: the created distribution reports no {member}"))
            .to_string()
    };
    (read("Id"), read("DomainName"), read("ARN"))
}

/// RULE: an object the behaviour caches is a MISS the first time and a HIT the second, with
/// byte-identical bodies, whatever its size.
#[tokio::test]
async fn an_object_is_served_from_the_edge_cache_whatever_its_size() {
    let emulator = Emulator::new();
    let case = "large object cache";

    // One bucket holding every row's object, one origin access control in front of it and one
    // distribution over it: the rows differ in the object they fetch and in nothing else.
    let staged: Vec<(Row, String)> = ROWS.iter().map(|row| (*row, object_bytes(row))).collect();
    for (row, body) in &staged {
        emulator
            .staged_object(row.case, BUCKET, row.key, body)
            .await;
    }
    let control = create_origin_access_control(&emulator.setup, case, "large-object-cache").await;
    let (id, domain, arn) = create_distribution(
        &emulator.setup,
        case,
        "ref-large-object-cache",
        BUCKET,
        &control,
    )
    .await;
    // The grant is written AFTER the distribution exists, because it is scoped to that
    // distribution's own ARN — the order a client has to follow.
    emulator
        .staged_policy(case, BUCKET, &grant(BUCKET, &arn))
        .await;
    awaited_deployment(&emulator.setup, case, &id).await;

    for (row, body) in &staged {
        let case = row.case;
        let path = format!("/{}", row.key);
        let first = view(&emulator.viewers, &domain, &path).await;
        assert_eq!(
            (first.status, first.x_cache.as_str()),
            (200, MISS),
            "case {case}: expected the origin's 200 under X-Cache {MISS:?} for {} bytes, got {} \
             with {:?} ({} body bytes)",
            row.bytes,
            first.status,
            first.x_cache,
            first.body.len()
        );
        assert_eq!(
            first.body,
            body.as_bytes(),
            "case {case}: expected the object's own {} bytes from the origin, got {} bytes",
            row.bytes,
            first.body.len()
        );

        let second = view(&emulator.viewers, &domain, &path).await;
        assert_eq!(
            (second.status, second.x_cache.as_str()),
            (200, HIT),
            "case {case}: expected the edge cache's 200 under X-Cache {HIT:?} for {} bytes, got \
             {} with {:?} ({} body bytes)",
            row.bytes,
            second.status,
            second.x_cache,
            second.body.len()
        );
        assert_eq!(
            second.body, first.body,
            "case {case}: expected the cached answer to carry the same {} bytes the origin \
             answered, got {} bytes",
            row.bytes,
            second.body.len()
        );
    }
}
