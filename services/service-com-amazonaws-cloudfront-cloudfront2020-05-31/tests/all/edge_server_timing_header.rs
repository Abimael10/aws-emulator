// SPDX-License-Identifier: CC0-1.0

//! The `Server-Timing` header a response headers policy asks for, end to end over HTTP.
//!
//! The rule: `ServerTimingHeadersConfig$Enabled` "determines whether CloudFront adds the
//! `Server-Timing` header to HTTP responses that it sends in response to requests that match a
//! cache behavior that's associated with this response headers policy", and `SamplingRate` is
//! "a number 0–100 (inclusive) that specifies the percentage of responses that you want
//! CloudFront to add the `Server-Timing` header to" — 100 adds it to every response, 50 to
//! half of them, and a rate the policy does not state narrows the enable to nothing, so the
//! header travels on every response. What the header carries are the METRICS of the exchange
//! it travels on: "you can see which cache layer served a cache hit, or the first byte latency
//! from the origin when there was a cache miss", so the metrics name this request's own id,
//! what the edge cache did with it and the latencies the edge measured.
//!
//! The rows differ along what the rule generalises over: whether the group is enabled, the
//! sampling rate (both published bounds and a rate in between), whether the policy states a
//! rate at all, whether the policy states the group at all, and whether the answer came from
//! the origin or out of the edge cache.
//!
//! The harness is the lifecycle suites', reused rather than copied.

use bytes::Bytes;
use smithy_server_runtime::MultiServiceRouter;

use crate::distribution_lifecycle::{
    awaited_deployment, create_distribution, emulator, API_VERSION, HOST,
};
use crate::response_headers_policy_lifecycle::{
    create_policy as create_response_headers_policy, Policy as ResponseHeadersPolicy, ServerTiming,
};

/// The AWS-managed cache policy `Managed-CachingDisabled`, whose TTLs are all zero: a
/// behaviour under it caches nothing, so every request is answered from the origin.
///
/// `pub(crate)` so the sibling suite over the same header reads the same managed policy rather
/// than restating its id.
pub(crate) const CACHING_DISABLED: &str = "4135ea2d-6df8-44a3-9df3-4b5a84be39ad";

/// The AWS-managed cache policy `Managed-CachingOptimized`, whose `DefaultTTL` is a day: a
/// behaviour under it may answer a second request out of the edge cache.
///
/// `pub(crate)` for the reason [`CACHING_DISABLED`] is.
pub(crate) const CACHING_OPTIMIZED: &str = "658327ea-f89d-4fab-a63d-7e88639e58f6";

/// An origin this emulator SERVES: this service's own control-plane endpoint.
const SERVED_ORIGIN: &str = HOST;

/// A viewer path the served origin answers.
///
/// `pub(crate)` so a sibling suite asks the same origin for the same object.
pub(crate) const SERVED_PATH: &str = "/2020-05-31/distribution";

/// The response header the group writes.
const SERVER_TIMING: &str = "server-timing";

/// What the edge answered one viewer request with, of the parts these rules read.
///
/// `pub(crate)`, with [`view`], so a sibling suite over the same header reads one answer the
/// same way.
pub(crate) struct Viewed {
    pub(crate) status: u16,
    /// The `Server-Timing` header, or the empty string when the answer carried none.
    pub(crate) timing: String,
    /// The request id the edge stamped, which the header's own `cdn-rid` metric reports.
    pub(crate) cf_id: String,
    /// `X-Cache`, which says whether the edge cache held the answer.
    pub(crate) x_cache: String,
}

/// Send one VIEWER request, addressed to `host` — which is what resolves the distribution.
pub(crate) async fn view(
    router: &MultiServiceRouter,
    host: &str,
    method: &str,
    path: &str,
) -> Viewed {
    let request = ::http::Request::builder()
        .method(method)
        .uri(path)
        .header("host", host)
        .body(Bytes::new())
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let header = |name: &str| {
        response
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string()
    };
    Viewed {
        status: response.status().as_u16(),
        timing: header(SERVER_TIMING),
        cf_id: header("x-amz-cf-id"),
        x_cache: header("x-cache"),
    }
}

/// Create a response headers policy stating the `Server-Timing` group `timing` and report its
/// id.
///
/// `pub(crate)` so a sibling suite attaches the same group through the same route.
pub(crate) async fn timing_policy(
    router: &MultiServiceRouter,
    name: &str,
    timing: Option<ServerTiming>,
) -> String {
    let policy = ResponseHeadersPolicy {
        case: "the server timing policy",
        name: name.to_string(),
        comment: Some("the server timing suite".to_string()),
        server_timing: timing,
        ..ResponseHeadersPolicy::default()
    };
    let created = create_response_headers_policy(router, &policy.xml()).await;
    assert_eq!(
        created.status, 201,
        "creating the response headers policy {name} answered {} with {}",
        created.status, created.raw
    );
    created
        .root()
        .find_text("Id")
        .expect("the created response headers policy reports an id")
        .to_string()
}

/// Create one distribution attaching `response_headers_policy` under the cache policy
/// `cache_policy`, let the clock deploy it, and report the domain name a viewer addresses.
///
/// `pub(crate)` so a sibling suite drives the same configuration rather than copying it.
pub(crate) async fn deployed(
    router: &MultiServiceRouter,
    reference: &str,
    cache_policy: &str,
    response_headers_policy: &str,
) -> String {
    let config = format!(
        "<DistributionConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <CallerReference>{reference}</CallerReference>\
         <Origins><Quantity>1</Quantity><Items><Origin>\
         <Id>only</Id><DomainName>{SERVED_ORIGIN}</DomainName>\
         <CustomOriginConfig><HTTPPort>80</HTTPPort><HTTPSPort>443</HTTPSPort>\
         <OriginProtocolPolicy>https-only</OriginProtocolPolicy></CustomOriginConfig>\
         </Origin></Items></Origins>\
         <DefaultCacheBehavior><TargetOriginId>only</TargetOriginId>\
         <ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy>\
         <CachePolicyId>{cache_policy}</CachePolicyId>\
         <ResponseHeadersPolicyId>{response_headers_policy}</ResponseHeadersPolicyId>\
         </DefaultCacheBehavior>\
         <Comment>server timing</Comment>\
         <PriceClass>PriceClass_All</PriceClass>\
         <Enabled>true</Enabled>\
         </DistributionConfig>"
    );
    let created = create_distribution(router, &config).await;
    assert_eq!(
        created.status, 201,
        "creating the distribution {reference} answered {} with {}",
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
    awaited_deployment(router, reference, &id).await;
    domain
}

/// The metric names one `Server-Timing` value carries, in the order it lists them: each member
/// up to its first `;`, trimmed.
///
/// `pub(crate)` so a sibling suite reads the same header the same way.
pub(crate) fn metric_names(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(|member| {
            member
                .trim()
                .split(';')
                .next()
                .unwrap_or_default()
                .to_string()
        })
        .filter(|name| !name.is_empty())
        .collect()
}

/// The value of one metric's parameter (`dur` or `desc`) in `value`, or `None`.
///
/// `pub(crate)` for the reason [`metric_names`] is.
pub(crate) fn metric_parameter(value: &str, metric: &str, parameter: &str) -> Option<String> {
    value
        .split(',')
        .map(str::trim)
        .find(|member| member.starts_with(&format!("{metric};")))?
        .split(';')
        .skip(1)
        .find_map(|part| {
            part.trim()
                .strip_prefix(&format!("{parameter}="))
                .map(|found| found.trim_matches('"').to_string())
        })
}

/// A policy that ENABLES the group at the whole rate adds the header to every answer, and the
/// metrics it carries are this exchange's own.
#[tokio::test]
async fn an_enabled_group_adds_the_metrics_of_the_exchange() {
    let router = emulator();
    let policy = timing_policy(
        &router,
        "timing-every-response",
        Some(ServerTiming {
            enabled: true,
            sampling_rate: Some(100.0),
        }),
    )
    .await;
    let domain = deployed(&router, "timing-every", CACHING_DISABLED, &policy).await;

    // Several requests, because "every response" is the rule: each one carries the header, and
    // each one carries its OWN request id.
    let mut seen_ids: Vec<String> = Vec::new();
    for attempt in 0..3 {
        let case = format!("request {attempt}");
        let answer = view(&router, &domain, "GET", SERVED_PATH).await;
        assert_eq!(
            answer.status, 200,
            "case {case}: expected the request to be served, got {}",
            answer.status
        );
        assert!(
            !answer.timing.is_empty(),
            "case {case}: expected a Server-Timing header, got none"
        );
        let names = metric_names(&answer.timing);
        assert!(
            names.contains(&"cdn-rid".to_string()),
            "case {case}: expected the request-id metric, got {:?}",
            answer.timing
        );
        assert_eq!(
            metric_parameter(&answer.timing, "cdn-rid", "desc"),
            Some(answer.cf_id.clone()),
            "case {case}: expected the request id the edge stamped ({:?}), got {:?}",
            answer.cf_id,
            answer.timing
        );
        // The answer came from the origin, so the cache missed and an upstream leg happened.
        assert!(
            names.contains(&"cdn-cache-miss".to_string()),
            "case {case}: expected the cache-miss metric for an answer the origin produced, \
             got {:?}",
            answer.timing
        );
        assert!(
            names.contains(&"cdn-upstream-fbl".to_string())
                && names.contains(&"cdn-upstream-layer".to_string()),
            "case {case}: expected the upstream metrics for an answer the origin produced, \
             got {:?}",
            answer.timing
        );
        // Every duration the header carries is a whole number of milliseconds.
        for metric in ["cdn-upstream-fbl", "cdn-downstream-fbl"] {
            let duration = metric_parameter(&answer.timing, metric, "dur").unwrap_or_else(|| {
                panic!("case {case}: expected a {metric} duration in {:?}", answer.timing)
            });
            assert!(
                duration.parse::<u64>().is_ok(),
                "case {case}: expected {metric} to carry whole milliseconds, got \
                 {duration:?}"
            );
        }
        assert!(
            !seen_ids.contains(&answer.cf_id),
            "case {case}: the edge reported a request id it had already used: {:?}",
            answer.cf_id
        );
        seen_ids.push(answer.cf_id);
    }
}

/// The cache metric says what the EDGE CACHE did with this request: a second request for the
/// same object under a caching policy is a hit, and the upstream metrics belong to the miss
/// alone.
#[tokio::test]
async fn the_cache_metric_reports_what_the_edge_cache_did() {
    let router = emulator();
    let policy = timing_policy(
        &router,
        "timing-cache-outcome",
        Some(ServerTiming {
            enabled: true,
            sampling_rate: Some(100.0),
        }),
    )
    .await;
    let domain = deployed(&router, "timing-cache", CACHING_OPTIMIZED, &policy).await;

    // (case, which request this is, the metric it must carry, the X-Cache it must report).
    let first = view(&router, &domain, "GET", SERVED_PATH).await;
    assert_eq!(
        first.status, 200,
        "the first request must be served, got {}",
        first.status
    );
    assert!(
        metric_names(&first.timing).contains(&"cdn-cache-miss".to_string()),
        "the first request is a miss, got {:?} with X-Cache {:?}",
        first.timing,
        first.x_cache
    );

    let second = view(&router, &domain, "GET", SERVED_PATH).await;
    assert_eq!(
        second.status, 200,
        "the second request must be served, got {}",
        second.status
    );
    assert_eq!(
        second.x_cache, "Hit from cloudfront",
        "the second request must come out of the edge cache, got {:?}",
        second.x_cache
    );
    let names = metric_names(&second.timing);
    assert!(
        names.contains(&"cdn-cache-hit".to_string()),
        "the answer the cache held reports a hit, got {:?}",
        second.timing
    );
    assert!(
        !names.contains(&"cdn-upstream-fbl".to_string()),
        "an answer the cache held had no upstream leg, so it reports no upstream latency: \
         {:?}",
        second.timing
    );
}

/// The PUBLISHED address serves the header to a viewer that dials it: the metrics travel the
/// whole way out to a client that reaches the distribution by the name it publishes, not only
/// through the router.
#[tokio::test]
async fn the_published_address_serves_the_header_to_a_viewer_that_dials_it() {
    let router = emulator();
    let policy = timing_policy(
        &router,
        "timing-dialled",
        Some(ServerTiming {
            enabled: true,
            sampling_rate: Some(100.0),
        }),
    )
    .await;
    let domain = deployed(&router, "timing-dialled", CACHING_DISABLED, &policy).await;

    let raw = crate::published_edge_address::dial(&domain, "GET", SERVED_PATH).await;
    let lowercase = raw.to_ascii_lowercase();
    assert!(
        lowercase.contains("server-timing:"),
        "expected the dialled answer to carry the Server-Timing header, got {raw:?}"
    );
    assert!(
        lowercase.contains("cdn-rid"),
        "expected the metrics of the exchange in that header, got {raw:?}"
    );
}

/// The published address serves the header on the `https://` a viewer dials too: the
/// encryption of that one address is negotiated in band, so a stock TLS client handed the
/// published `DomainName` reads the same metrics there.
///
/// The dial runs on a blocking thread, so the listener it addresses — a task of this same
/// runtime — is served while this call waits for the answer.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_published_address_serves_the_header_over_tls() {
    let router = emulator();
    let policy = timing_policy(
        &router,
        "timing-tls",
        Some(ServerTiming {
            enabled: true,
            sampling_rate: Some(100.0),
        }),
    )
    .await;
    let domain = deployed(&router, "timing-tls", CACHING_DISABLED, &policy).await;
    let url = format!("https://{domain}{SERVED_PATH}");

    let (status, timing) = ::tokio::task::spawn_blocking(move || {
        let client = ::reqwest::blocking::Client::builder()
            .danger_accept_invalid_certs(true)
            // Generous: nothing here measures the wait, only what came back.
            .timeout(::std::time::Duration::from_secs(60))
            .build()
            .expect("a TLS client");
        let answered = client
            .get(&url)
            .send()
            .unwrap_or_else(|error| panic!("the published address did not answer {url}: {error}"));
        let status = answered.status().as_u16();
        let timing = answered
            .headers()
            .get(SERVER_TIMING)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();
        (status, timing)
    })
    .await
    .expect("the dial finished");

    assert_eq!(
        status, 200,
        "expected the TLS dial of the published address to be served, got {status}"
    );
    assert!(
        timing.contains("cdn-rid"),
        "expected the metrics of the exchange in the Server-Timing header, got {timing:?}"
    );
}

/// WHICH responses carry the header is the sampling rate's to say: the whole rate adds it to
/// every response, zero to none, a rate in between to that share of them, and a group that is
/// not enabled — or a policy that states none — adds it to nothing.
#[tokio::test]
async fn the_sampling_rate_decides_which_responses_carry_the_header() {
    let router = emulator();

    // (case, the group the policy states, how many of eight responses must carry the header).
    let rows: Vec<(&str, Option<ServerTiming>, usize)> = vec![
        (
            "enabled at the whole rate",
            Some(ServerTiming {
                enabled: true,
                sampling_rate: Some(100.0),
            }),
            8,
        ),
        (
            "enabled at half the rate",
            Some(ServerTiming {
                enabled: true,
                sampling_rate: Some(50.0),
            }),
            4,
        ),
        (
            "enabled at a quarter of the rate",
            Some(ServerTiming {
                enabled: true,
                sampling_rate: Some(25.0),
            }),
            2,
        ),
        (
            "enabled at a rate with decimal places",
            Some(ServerTiming {
                enabled: true,
                sampling_rate: Some(12.5),
            }),
            1,
        ),
        (
            "enabled at the zero rate",
            Some(ServerTiming {
                enabled: true,
                sampling_rate: Some(0.0),
            }),
            0,
        ),
        (
            "enabled with no rate stated",
            Some(ServerTiming {
                enabled: true,
                sampling_rate: None,
            }),
            8,
        ),
        (
            "not enabled at the whole rate",
            Some(ServerTiming {
                enabled: false,
                sampling_rate: Some(100.0),
            }),
            0,
        ),
        ("no group at all", None, 0),
    ];

    for (index, (case, timing, expected)) in rows.into_iter().enumerate() {
        let policy = timing_policy(&router, &format!("timing-rate-{index}"), timing).await;
        let domain = deployed(
            &router,
            &format!("timing-rate-{index}"),
            CACHING_DISABLED,
            &policy,
        )
        .await;

        let mut carried = 0usize;
        for attempt in 0..8 {
            let answer = view(&router, &domain, "GET", SERVED_PATH).await;
            assert_eq!(
                answer.status, 200,
                "case {case} (request {attempt}): expected the request to be served, got {}",
                answer.status
            );
            if !answer.timing.is_empty() {
                carried += 1;
            }
        }
        assert_eq!(
            carried, expected,
            "case {case}: expected {expected} of eight responses to carry the header, got \
             {carried}"
        );
    }
}
