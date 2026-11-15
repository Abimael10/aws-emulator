// SPDX-License-Identifier: CC0-1.0

//! A viewer that asks for PART of an object at a distribution's edge, end to end over HTTP.
//!
//! The rule under test is HTTP's own (RFC 9110 §14), which the edge applies to the
//! representation it is about to serve — the entry its cache held, or the answer the origin
//! has just produced:
//!
//! * a `Range` of the `bytes` unit that selects bytes of the object is answered `206` with
//!   exactly those bytes, `Content-Range: bytes <first>-<last>/<total>` and a `Content-Length`
//!   of the window;
//! * a range that is valid and selects NONE of the object — its first byte position is at or
//!   past the end, or it asks for the last zero bytes — is refused `416` carrying none of the
//!   object's bytes. Serving the whole object to such a viewer is the over-permissive answer
//!   this suite exists to rule out;
//! * a `Range` the edge does not act on — another unit, an invalid specifier, more than one
//!   range — is ignored, and the complete object is served `200`.
//!
//! Every window is stated relative to the length of the object the edge actually holds, which
//! the cold fetch reads: the rule is about the request and that length, so no case carries a
//! literal size of its own.
//!
//! ## The origin this suite points at
//!
//! An edge fetch is a real in-process HTTP request to the origin's own `DomainName`, so the
//! origin named here is an endpoint this emulator serves: this service's own control-plane
//! host, whose `GET /2020-05-31/distribution` answers a `DistributionList`. Nothing about the
//! edge is special-cased for it — it is an origin like any other.

use bytes::Bytes;
use smithy_server_runtime::MultiServiceRouter;

use super::distribution_lifecycle::{
    awaited_deployment, create_distribution, emulator, API_VERSION, HOST,
};

/// The AWS-managed cache policy `Managed-CachingOptimized`, whose `DefaultTTL` is a day: a
/// behaviour under it caches what it is allowed to cache, so a window is taken from the entry
/// the cache holds.
const CACHING_OPTIMIZED: &str = "658327ea-f89d-4fab-a63d-7e88639e58f6";

/// The AWS-managed cache policy `Managed-CachingDisabled`, whose TTLs are all zero: a
/// behaviour under it caches nothing, so every request goes to the origin and a window is
/// taken from what the origin has just answered.
const CACHING_DISABLED: &str = "4135ea2d-6df8-44a3-9df3-4b5a84be39ad";

/// An origin this emulator SERVES: this service's own control-plane endpoint.
const SERVED_ORIGIN: &str = HOST;

/// A viewer path the served origin answers with a `DistributionList`.
const SERVED_PATH: &str = "/2020-05-31/distribution";

/// `X-Cache` for an answer the edge cache held.
const HIT: &str = "Hit from cloudfront";

/// `X-Cache` for an answer the origin produced for this request.
const MISS: &str = "Miss from cloudfront";

/// What one viewer request was answered with.
struct Viewed {
    status: u16,
    x_cache: String,
    content_range: String,
    content_length: String,
    body: Vec<u8>,
}

/// What the edge is expected to do with one `Range` header.
#[derive(Debug)]
enum Expect {
    /// `206` carrying the bytes `first..=last` of the object.
    Window { first: usize, last: usize },
    /// `416` carrying none of the object.
    Refused,
    /// The header is not acted on: `200` carrying the whole object.
    Whole,
}

/// Send one VIEWER request: no AWS credential, addressed to `host` — which is what resolves
/// the distribution — optionally carrying a `Range`, exactly as a browser or a media player
/// reaches a distribution.
async fn view(
    router: &MultiServiceRouter,
    host: &str,
    path: &str,
    range: Option<&str>,
) -> Viewed {
    let mut builder = ::http::Request::builder()
        .method("GET")
        .uri(path)
        .header("host", host);
    if let Some(range) = range {
        builder = builder.header("range", range);
    }
    let request = builder
        .body(Bytes::new())
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
    let (x_cache, content_range, content_length) = (
        header("x-cache"),
        header("content-range"),
        header("content-length"),
    );
    Viewed {
        status,
        x_cache,
        content_range,
        content_length,
        body: response.into_body().to_vec(),
    }
}

/// The `DistributionConfig` this suite serves through: one origin and a default behaviour
/// that allows and caches `GET`, under `cache_policy` and, where one is named, the origin
/// request policy `origin_request_policy` — which decides what of the viewer's request
/// reaches the origin.
fn config(caller_reference: &str, cache_policy: &str, origin_request_policy: &str) -> String {
    let forwarding = if origin_request_policy.is_empty() {
        String::new()
    } else {
        format!("<OriginRequestPolicyId>{origin_request_policy}</OriginRequestPolicyId>")
    };
    format!(
        "<DistributionConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <CallerReference>{caller_reference}</CallerReference>\
         <Origins><Quantity>1</Quantity><Items><Origin>\
         <Id>primary</Id><DomainName>{SERVED_ORIGIN}</DomainName>\
         <CustomOriginConfig><HTTPPort>80</HTTPPort><HTTPSPort>443</HTTPSPort>\
         <OriginProtocolPolicy>https-only</OriginProtocolPolicy></CustomOriginConfig>\
         </Origin></Items></Origins>\
         <DefaultCacheBehavior><TargetOriginId>primary</TargetOriginId>\
         <ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy>\
         <AllowedMethods><Quantity>2</Quantity>\
         <Items><Method>GET</Method><Method>HEAD</Method></Items>\
         <CachedMethods><Quantity>2</Quantity>\
         <Items><Method>GET</Method><Method>HEAD</Method></Items></CachedMethods>\
         </AllowedMethods>\
         <CachePolicyId>{cache_policy}</CachePolicyId>{forwarding}\
         </DefaultCacheBehavior>\
         <CacheBehaviors><Quantity>0</Quantity></CacheBehaviors>\
         <Comment>edge range requests</Comment>\
         <PriceClass>PriceClass_All</PriceClass>\
         <Enabled>true</Enabled>\
         </DistributionConfig>"
    )
}

/// Create one distribution and wait for the clock to deploy it: the domain name CloudFront
/// assigned it, which is the name a viewer reaches it by.
async fn deployed(router: &MultiServiceRouter, caller_reference: &str, policy: &str) -> String {
    deployed_under(router, caller_reference, policy, "").await
}

/// [`deployed`], with the behaviour also naming an origin request policy — for the rule about
/// what the ORIGIN is asked for.
async fn deployed_under(
    router: &MultiServiceRouter,
    caller_reference: &str,
    policy: &str,
    origin_request_policy: &str,
) -> String {
    let created = create_distribution(
        router,
        &config(caller_reference, policy, origin_request_policy),
    )
    .await;
    assert_eq!(
        created.status, 201,
        "creating the distribution answered {}: {}",
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

/// Assert one case: the status, the bytes and the two headers a partial answer states.
fn assert_case(case: &str, object: &[u8], header: &str, expected: &Expect, answer: &Viewed) {
    let total = object.len();
    match expected {
        Expect::Window { first, last } => {
            let window = &object[*first..=*last];
            assert_eq!(
                answer.status, 206,
                "case {case:?} ({header:?} over {total} bytes): expected 206, got {} (body \
                 {} bytes)",
                answer.status,
                answer.body.len()
            );
            assert_eq!(
                answer.body, window,
                "case {case:?} ({header:?}): expected exactly bytes {first}..={last}, got {} \
                 bytes starting {:?}",
                answer.body.len(),
                answer.body.iter().take(8).collect::<Vec<_>>()
            );
            assert_eq!(
                answer.content_range,
                format!("bytes {first}-{last}/{total}"),
                "case {case:?} ({header:?}): expected the window and the object's length, got \
                 {:?}",
                answer.content_range
            );
            assert_eq!(
                answer.content_length,
                window.len().to_string(),
                "case {case:?} ({header:?}): expected a Content-Length of the window, got {:?}",
                answer.content_length
            );
        }
        Expect::Refused => {
            assert_eq!(
                answer.status, 416,
                "case {case:?} ({header:?} over {total} bytes): expected 416, got {} \
                 (body {} bytes) — an edge that widens an unsatisfiable range into the whole \
                 object is over-permissive",
                answer.status,
                answer.body.len()
            );
            assert!(
                answer.body.is_empty(),
                "case {case:?} ({header:?}): expected none of the object's bytes, got {} bytes",
                answer.body.len()
            );
            assert_eq!(
                answer.content_range,
                format!("bytes */{total}"),
                "case {case:?} ({header:?}): expected the object's length alone, got {:?}",
                answer.content_range
            );
        }
        Expect::Whole => {
            assert_eq!(
                answer.status, 200,
                "case {case:?} ({header:?} over {total} bytes): expected the whole object \
                 under 200, got {} (body {} bytes)",
                answer.status,
                answer.body.len()
            );
            assert_eq!(
                answer.body, object,
                "case {case:?} ({header:?}): expected the whole object, got {} bytes",
                answer.body.len()
            );
            assert!(
                answer.content_range.is_empty(),
                "case {case:?} ({header:?}): a complete answer states no Content-Range, got {:?}",
                answer.content_range
            );
        }
    }
}

/// The three outcomes, over an object the edge cache holds: a window, a refusal, and a
/// `Range` the edge does not act on. The rows differ along where the window sits, how it is
/// spelled (both ends, an open end, a suffix), how far past the end it reaches, and whether
/// the header is one HTTP defines at all.
#[tokio::test]
async fn the_edge_answers_a_byte_range_out_of_the_object_it_holds() {
    let router = emulator();
    let domain = deployed(&router, "edge-range-cached", CACHING_OPTIMIZED).await;

    // The cold fetch is what puts the object in the cache, and its bytes are the
    // representation every window below is stated against.
    let cold = view(&router, &domain, SERVED_PATH, None).await;
    assert_eq!(
        (cold.status, cold.x_cache.as_str()),
        (200, MISS),
        "expected the origin's 200 with X-Cache {MISS:?}, got {} with {:?}",
        cold.status,
        cold.x_cache
    );
    let object = cold.body.clone();
    let total = object.len();
    assert!(
        total > 32,
        "the origin answered {total} bytes, which is too short to take a window of"
    );

    let cases: [(&str, String, Expect); 17] = [
        (
            "a window inside the object",
            "bytes=10-19".to_string(),
            Expect::Window { first: 10, last: 19 },
        ),
        (
            "the first byte alone",
            "bytes=0-0".to_string(),
            Expect::Window { first: 0, last: 0 },
        ),
        (
            "the last byte alone",
            format!("bytes={}-{}", total - 1, total - 1),
            Expect::Window {
                first: total - 1,
                last: total - 1,
            },
        ),
        (
            "an open end runs to the last byte",
            "bytes=5-".to_string(),
            Expect::Window {
                first: 5,
                last: total - 1,
            },
        ),
        (
            "a suffix of the object",
            "bytes=-16".to_string(),
            Expect::Window {
                first: total - 16,
                last: total - 1,
            },
        ),
        (
            "a suffix longer than the object is the whole object",
            format!("bytes=-{}", total * 2),
            Expect::Window {
                first: 0,
                last: total - 1,
            },
        ),
        (
            "the whole object spelled as a range",
            format!("bytes=0-{}", total - 1),
            Expect::Window {
                first: 0,
                last: total - 1,
            },
        ),
        (
            "a last byte past the end is clamped to it",
            format!("bytes={}-{}", total - 4, total + 99),
            Expect::Window {
                first: total - 4,
                last: total - 1,
            },
        ),
        (
            "the first byte one past the end",
            format!("bytes={}-{}", total, total + 9),
            Expect::Refused,
        ),
        (
            "a window far past the end",
            format!("bytes={}-{}", total * 2, total * 2 + 9),
            Expect::Refused,
        ),
        ("the last zero bytes", "bytes=-0".to_string(), Expect::Refused),
        (
            "a last byte below the first",
            "bytes=19-10".to_string(),
            Expect::Whole,
        ),
        (
            "a unit that is not bytes",
            "items=0-9".to_string(),
            Expect::Whole,
        ),
        (
            "more than one range",
            "bytes=0-9,20-29".to_string(),
            Expect::Whole,
        ),
        (
            // A position with more digits than a machine counts bytes in still names a byte no
            // object holds, so it is refused rather than served whole.
            "a first byte past every object",
            "bytes=99999999999999999999-".to_string(),
            Expect::Refused,
        ),
        (
            "a last byte past every object is the end of this one",
            format!("bytes={}-99999999999999999999", total - 4),
            Expect::Window {
                first: total - 4,
                last: total - 1,
            },
        ),
        (
            "a suffix longer than every object is the whole of this one",
            "bytes=-99999999999999999999".to_string(),
            Expect::Window {
                first: 0,
                last: total - 1,
            },
        ),
    ];

    for (case, header, expected) in &cases {
        let answer = view(&router, &domain, SERVED_PATH, Some(header)).await;
        assert_eq!(
            answer.x_cache, HIT,
            "case {case:?} ({header:?}): expected the window to come from the edge cache, got \
             X-Cache {:?} (status {})",
            answer.x_cache, answer.status
        );
        assert_case(case, &object, header, expected, &answer);
    }

    // And the object itself is untouched by every window taken of it: an unconditional GET
    // still answers all of it.
    let whole = view(&router, &domain, SERVED_PATH, None).await;
    assert_case("no Range at all", &object, "", &Expect::Whole, &whole);
}

/// The same rule on a MISS: a behaviour that caches nothing goes to the origin every time,
/// and the window is taken from the answer the origin has just produced.
#[tokio::test]
async fn the_edge_answers_a_byte_range_out_of_what_the_origin_just_answered() {
    let router = emulator();
    let domain = deployed(&router, "edge-range-uncached", CACHING_DISABLED).await;

    let cold = view(&router, &domain, SERVED_PATH, None).await;
    assert_eq!(
        (cold.status, cold.x_cache.as_str()),
        (200, MISS),
        "expected the origin's 200 with X-Cache {MISS:?}, got {} with {:?}",
        cold.status,
        cold.x_cache
    );
    let object = cold.body.clone();
    let total = object.len();

    let cases: [(&str, String, Expect); 3] = [
        (
            "a window inside the object the origin answered",
            "bytes=0-9".to_string(),
            Expect::Window { first: 0, last: 9 },
        ),
        (
            "a suffix of the object the origin answered",
            "bytes=-8".to_string(),
            Expect::Window {
                first: total - 8,
                last: total - 1,
            },
        ),
        (
            "a window past the end of the object the origin answered",
            format!("bytes={}-{}", total + 1, total + 10),
            Expect::Refused,
        ),
    ];

    for (case, header, expected) in &cases {
        let answer = view(&router, &domain, SERVED_PATH, Some(header)).await;
        assert_eq!(
            answer.x_cache, MISS,
            "case {case:?} ({header:?}): this behaviour caches nothing, so the answer comes \
             from the origin; got X-Cache {:?} (status {})",
            answer.x_cache, answer.status
        );
        assert_case(case, &object, header, expected, &answer);
    }
}

/// A behaviour whose ORIGIN REQUEST POLICY forwards every viewer header still asks the origin
/// for the COMPLETE object: the edge evaluates the range itself, so a viewer's window never
/// becomes what the cache holds for the object.
///
/// Observed where it is observable — through the answers: the very first request this
/// distribution ever serves carries a `Range`, and the window it is answered with is this
/// edge's own (`206` and exactly those bytes); an unconditional GET right after it is a cache
/// HIT answering ALL of the object, which an entry holding one viewer's window could not do;
/// and a range past the end is still refused.
#[tokio::test]
async fn a_forwarding_origin_request_policy_still_caches_the_whole_object() {
    let router = emulator();
    // `Managed-AllViewer`, "Policy to forward all parameters in viewer requests", whose id is
    // the one `MANAGED_ORIGIN_REQUEST_POLICIES` publishes.
    const ALL_VIEWER: &str = "216adef6-5c7f-47e4-b989-5492eafa07d3";
    let domain = deployed_under(
        &router,
        "edge-range-forwarded",
        CACHING_OPTIMIZED,
        ALL_VIEWER,
    )
    .await;

    // The first request of all, carrying a window: a MISS, so what the origin answered is what
    // the window was taken from and what the cache now holds.
    let window = view(&router, &domain, SERVED_PATH, Some("bytes=0-9")).await;
    assert_eq!(
        (window.status, window.x_cache.as_str()),
        (206, MISS),
        "expected the cold ranged request to be answered 206 from the origin's answer, got {} \
         with X-Cache {:?}",
        window.status,
        window.x_cache
    );
    assert_eq!(
        window.body.len(),
        10,
        "expected exactly the 10 bytes asked for, got {}",
        window.body.len()
    );

    // The whole object, out of the cache the ranged request filled: a window is not what was
    // held.
    let whole = view(&router, &domain, SERVED_PATH, None).await;
    assert_eq!(
        (whole.status, whole.x_cache.as_str()),
        (200, HIT),
        "expected the cached object to be answered whole, got {} with X-Cache {:?}",
        whole.status,
        whole.x_cache
    );
    assert!(
        whole.body.len() > window.body.len() && whole.body.starts_with(&window.body),
        "expected the entry to hold the complete object the window came from, got {} bytes \
         against a {}-byte window",
        whole.body.len(),
        window.body.len()
    );
    assert!(
        whole.content_range.is_empty(),
        "expected a complete answer to state no Content-Range, got {:?}",
        whole.content_range
    );

    // And the refusal is computed against that complete object.
    let total = whole.body.len();
    let refused = view(
        &router,
        &domain,
        SERVED_PATH,
        Some(&format!("bytes={total}-{}", total + 9)),
    )
    .await;
    assert_case(
        "the first byte one past the end of the cached object",
        &whole.body,
        "past the end",
        &Expect::Refused,
        &refused,
    );
}
