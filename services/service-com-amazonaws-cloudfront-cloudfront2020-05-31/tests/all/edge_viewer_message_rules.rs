// SPDX-License-Identifier: CC0-1.0

//! What the edge decides about the VIEWER'S OWN MESSAGE, and what every answer it serves
//! carries, end to end over HTTP.
//!
//! Every assertion goes through the router, so each case exercises the claim, the resolution of
//! the distribution, the cache-behaviour selection, the enforcement CloudFront performs itself
//! and the origin fetch — the same path a viewer takes. Two rules, and neither is a property of
//! any distribution's configuration:
//!
//! * a CloudFront viewer response always NAMES the edge that served it, in a `Via` of the
//!   documented form `<protocol> <pseudonym>.cloudfront.net (CloudFront)` — the ` (CloudFront)`
//!   comment and the `.cloudfront.net` zone are fixed, while the protocol and the pseudonym are
//!   values of the serving environment — whatever produced the answer: the origin, the edge
//!   cache, or CloudFront itself;
//! * a viewer request carrying CONTENT on a method whose semantics define none (`GET` and
//!   `HEAD`: "a client SHOULD NOT generate content in a GET request", "a client MUST NOT
//!   generate content in a HEAD request", RFC 9110 §9.3.1-2) is refused by CloudFront with
//!   `403` and none of the origin's bytes, while the same content on a method that DOES define
//!   it travels to the origin and the origin's own answer is served.
//!
//! The rows differ along what each rule generalises over: for the `Via`, along what PRODUCED
//! the answer (an origin fetch, the edge cache, a page CloudFront generated for a method the
//! behaviour refuses, and the page it generates for an origin it cannot reach) and along the
//! status each of those carries; for the content rule, along the METHOD (every method the
//! behaviour allows, which here is all of them, so no method refusal can stand in for the one
//! under test) crossed with whether the request carries content at all.
//!
//! ## The origin these suites point at
//!
//! An edge fetch is a real in-process HTTP request to the origin's own `DomainName`, so a case
//! that needs an origin to ANSWER names an endpoint this emulator serves: this service's own
//! control-plane host, whose `GET /2020-05-31/distribution` answers a `DistributionList`. The
//! unreachable-origin row names a Lambda function URL host, which no service of this emulator
//! claims.
//!
//! The harness (the router, the request helpers and the poll that waits for the deployment) is
//! [`crate::distribution_lifecycle`]'s, reused rather than copied.

use bytes::Bytes;
use smithy_server_runtime::MultiServiceRouter;

use crate::distribution_lifecycle::{
    awaited_deployment, create_distribution, emulator, API_VERSION, HOST,
};

/// The AWS-managed cache policy `Managed-CachingOptimized`, whose `DefaultTTL` is a day: a
/// behaviour under it caches what it is allowed to cache.
const CACHING_OPTIMIZED: &str = "658327ea-f89d-4fab-a63d-7e88639e58f6";

/// The AWS-managed cache policy `Managed-CachingDisabled`, whose TTLs are all zero.
const CACHING_DISABLED: &str = "4135ea2d-6df8-44a3-9df3-4b5a84be39ad";

/// An origin this emulator SERVES: this service's own control-plane endpoint.
const SERVED_ORIGIN: &str = HOST;

/// A viewer path the served origin answers with a `DistributionList`.
const SERVED_PATH: &str = "/2020-05-31/distribution";

/// An origin NOTHING in this emulator serves: the host shape of a Lambda function URL.
const UNSERVED_ORIGIN: &str = "a0b1c2d3e4f5g6h7.lambda-url.us-east-1.on.aws";

/// `X-Cache` for an answer the edge cache held.
const HIT: &str = "Hit from cloudfront";

/// `X-Cache` for an answer the origin produced for this request.
const MISS: &str = "Miss from cloudfront";

/// `X-Cache` for an answer CloudFront produced itself.
const ERROR: &str = "Error from cloudfront";

/// The fixed comment a CloudFront `Via` element ends with.
const VIA_COMMENT: &str = " (CloudFront)";

/// The zone a CloudFront `Via` pseudonym is a label under.
const VIA_ZONE: &str = ".cloudfront.net";

/// What the edge answered one viewer request with.
struct Viewed {
    status: u16,
    x_cache: String,
    cf_id: String,
    via: String,
    body: String,
}

/// Send one VIEWER request: no AWS credential, addressed to `host` (which is what resolves the
/// distribution), carrying `content` as its body — exactly as a client reaches a distribution.
async fn view(
    router: &MultiServiceRouter,
    method: &str,
    host: &str,
    path: &str,
    content: &str,
) -> Viewed {
    let request = ::http::Request::builder()
        .method(method)
        .uri(path)
        .header("host", host)
        .body(Bytes::from(content.to_string()))
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
    let (x_cache, cf_id, via) = (header("x-cache"), header("x-amz-cf-id"), header("via"));
    let body = String::from_utf8_lossy(&response.into_body()).to_string();
    Viewed {
        status,
        x_cache,
        cf_id,
        via,
        body,
    }
}

/// The `DistributionConfig` of the distribution these suites serve through: one origin and a
/// default behaviour allowing `methods`.
fn config(caller_reference: &str, origin: &str, cache_policy: &str, methods: &[&str]) -> String {
    let allowed: String = methods
        .iter()
        .map(|method| format!("<Method>{method}</Method>"))
        .collect();
    format!(
        "<DistributionConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <CallerReference>{caller_reference}</CallerReference>\
         <Origins><Quantity>1</Quantity><Items><Origin>\
         <Id>primary</Id><DomainName>{origin}</DomainName>\
         <CustomOriginConfig><HTTPPort>80</HTTPPort><HTTPSPort>443</HTTPSPort>\
         <OriginProtocolPolicy>https-only</OriginProtocolPolicy></CustomOriginConfig>\
         </Origin></Items></Origins>\
         <DefaultCacheBehavior><TargetOriginId>primary</TargetOriginId>\
         <ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy>\
         <AllowedMethods><Quantity>{}</Quantity><Items>{allowed}</Items>\
         <CachedMethods><Quantity>2</Quantity>\
         <Items><Method>GET</Method><Method>HEAD</Method></Items></CachedMethods>\
         </AllowedMethods>\
         <CachePolicyId>{cache_policy}</CachePolicyId>\
         </DefaultCacheBehavior>\
         <Comment>the viewer message rules</Comment>\
         <PriceClass>PriceClass_All</PriceClass>\
         <Enabled>true</Enabled>\
         </DistributionConfig>",
        methods.len()
    )
}

/// Create one distribution, wait for the clock to deploy it, and report the domain name
/// CloudFront assigned it.
async fn deployed(
    router: &MultiServiceRouter,
    caller_reference: &str,
    origin: &str,
    cache_policy: &str,
    methods: &[&str],
) -> String {
    let created =
        create_distribution(router, &config(caller_reference, origin, cache_policy, methods)).await;
    assert_eq!(
        created.status, 201,
        "case {caller_reference}: creating the distribution answered {} with {}",
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

/// The pseudonym of a `Via` element of the documented form, or `None` when the value is not of
/// that form.
///
/// The form is checked here rather than asserted by a pattern spelled per case: the fixed parts
/// are the trailing ` (CloudFront)` comment and the `.cloudfront.net` zone of the pseudonym, the
/// protocol is a dotted version number, and the pseudonym's own label is alphanumeric.
fn via_pseudonym(value: &str) -> Option<&str> {
    let element = value.strip_suffix(VIA_COMMENT)?;
    let (protocol, received_by) = element.split_once(' ')?;
    if protocol.is_empty()
        || !protocol
            .chars()
            .all(|part| part.is_ascii_digit() || part == '.')
    {
        return None;
    }
    let label = received_by.strip_suffix(VIA_ZONE)?;
    (!label.is_empty() && label.chars().all(|part| part.is_ascii_alphanumeric())).then_some(label)
}

/// One answer this rule is asserted over: what produced it, and the status and cache outcome it
/// carries.
struct Answered {
    case: &'static str,
    /// The origin the distribution is configured with.
    origin: &'static str,
    /// The cache policy its behaviour names, which decides whether a second ask is a HIT.
    cache_policy: &'static str,
    /// The methods its behaviour allows.
    methods: &'static [&'static str],
    /// The method the viewer asks with.
    method: &'static str,
    /// How many times the viewer asks; the LAST answer is the one read.
    asks: usize,
    status: u16,
    x_cache: &'static str,
}

/// RULE: every answer the edge serves names the edge that served it, in a `Via` of the
/// documented form — whoever produced the answer.
#[tokio::test]
async fn every_answer_the_edge_serves_names_the_edge_in_its_via() {
    let answers: [Answered; 4] = [
        Answered {
            case: "an answer the origin produced",
            origin: SERVED_ORIGIN,
            cache_policy: CACHING_DISABLED,
            methods: &["GET", "HEAD"],
            method: "GET",
            asks: 1,
            status: 200,
            x_cache: MISS,
        },
        Answered {
            case: "an answer the edge cache held",
            origin: SERVED_ORIGIN,
            cache_policy: CACHING_OPTIMIZED,
            methods: &["GET", "HEAD"],
            method: "GET",
            asks: 2,
            status: 200,
            x_cache: HIT,
        },
        Answered {
            case: "a page CloudFront generated for a method the behaviour refuses",
            origin: SERVED_ORIGIN,
            cache_policy: CACHING_DISABLED,
            methods: &["GET", "HEAD"],
            method: "POST",
            asks: 1,
            status: 403,
            x_cache: ERROR,
        },
        Answered {
            case: "the page CloudFront generates for an origin it cannot reach",
            origin: UNSERVED_ORIGIN,
            cache_policy: CACHING_DISABLED,
            methods: &["GET", "HEAD"],
            method: "GET",
            asks: 1,
            status: 502,
            x_cache: ERROR,
        },
    ];

    for (index, answered) in answers.iter().enumerate() {
        let router = emulator();
        let case = answered.case;
        let domain = deployed(
            &router,
            &format!("via-{index}"),
            answered.origin,
            answered.cache_policy,
            answered.methods,
        )
        .await;
        // Every ask of this row, so a row that asks twice can show the two exchanges are named
        // apart even though one of them was answered out of the cache.
        let mut served: Vec<Viewed> = Vec::new();
        for _ in 0..answered.asks {
            served.push(view(&router, answered.method, &domain, SERVED_PATH, "").await);
        }
        let answer = served.last().expect("a row asks at least once");
        assert_eq!(
            (answer.status, answer.x_cache.as_str()),
            (answered.status, answered.x_cache),
            "case {case}: expected ({}, {:?}), got ({}, {:?}) with body {:?}",
            answered.status,
            answered.x_cache,
            answer.status,
            answer.x_cache,
            answer.body
        );
        let mut pseudonyms: Vec<String> = Vec::new();
        for answer in &served {
            let pseudonym = via_pseudonym(&answer.via).unwrap_or_else(|| {
                panic!(
                    "case {case}: expected a Via of the form \
                     \"<protocol> <pseudonym>{VIA_ZONE}{VIA_COMMENT}\", got {:?}",
                    answer.via
                )
            });
            // The pseudonym names THIS exchange: it is the identifier the edge stamped on the
            // request it answered, so a client correlating the two headers reads one value.
            assert_eq!(
                pseudonym, answer.cf_id,
                "case {case}: expected the Via to name the request the edge served ({:?}), got \
                 {:?}",
                answer.cf_id, answer.via
            );
            pseudonyms.push(pseudonym.to_string());
        }
        // Two asks are two exchanges, so the edge names them apart — including the one it
        // answered out of its cache, which is a fresh exchange over a held object.
        for (at, pseudonym) in pseudonyms.iter().enumerate() {
            for other in pseudonyms.iter().skip(at + 1) {
                assert_ne!(
                    pseudonym, other,
                    "case {case}: expected each ask to be named apart, got {pseudonym:?} twice"
                );
            }
        }
    }
}

/// Every method the model's `Method` enum admits
/// (`smithy_query(query="shape",shape="Method")`: `GET`, `HEAD`, `POST`, `PUT`, `PATCH`,
/// `OPTIONS`, `DELETE`), which is also the widest set `AllowedMethods$Quantity` admits ("7 for
/// `GET, HEAD, OPTIONS, PUT, PATCH, POST`, and `DELETE` requests") — so no row below is refused
/// for its method and the one refusal under test is the only one that can answer.
const EVERY_METHOD: &[&str] = &[
    "GET", "HEAD", "POST", "PUT", "PATCH", "OPTIONS", "DELETE",
];

/// The sentence CloudFront heads its OWN generated page with, which is what tells a page it
/// produced itself from a document an origin answered with.
const GENERATED_PAGE: &str = "The request could not be satisfied";

/// RULE: content on a method whose semantics define none is refused by CloudFront itself, and
/// content on a method that defines it reaches the origin.
#[tokio::test]
async fn content_on_a_method_that_defines_none_is_refused_at_the_edge() {
    let router = emulator();
    let domain = deployed(
        &router,
        "viewer-content",
        SERVED_ORIGIN,
        CACHING_DISABLED,
        EVERY_METHOD,
    )
    .await;

    // The rows differ along the method and along whether the request carries content:
    // (case, method, the content it carries, whether CloudFront refuses it itself)
    let cases: [(&str, &str, &str, bool); 9] = [
        ("a GET carrying content", "GET", "{\"forged\": true}", true),
        ("a GET carrying one byte", "GET", "x", true),
        ("a HEAD carrying content", "HEAD", "forged", true),
        ("a GET carrying none", "GET", "", false),
        ("a HEAD carrying none", "HEAD", "", false),
        ("a POST carrying content", "POST", "<Document/>", false),
        ("a PUT carrying content", "PUT", "<Document/>", false),
        ("a PATCH carrying content", "PATCH", "<Document/>", false),
        ("a DELETE carrying content", "DELETE", "<Document/>", false),
    ];
    for (case, method, content, refused) in cases {
        let answer = view(&router, method, &domain, SERVED_PATH, content).await;
        if refused {
            assert_eq!(
                (answer.status, answer.x_cache.as_str()),
                (403, ERROR),
                "case {case}: expected CloudFront's own 403 with X-Cache {ERROR:?}, got {} with \
                 {:?} and body {:?}",
                answer.status,
                answer.x_cache,
                answer.body
            );
            // The page is CloudFront's own, and it carries none of the origin's bytes: the
            // request never reached the origin.
            assert!(
                answer.body.contains(GENERATED_PAGE),
                "case {case}: expected the page CloudFront generates itself, got {:?}",
                answer.body
            );
            assert!(
                !answer.body.contains("DistributionList"),
                "case {case}: expected none of the origin's bytes, got {:?}",
                answer.body
            );
            continue;
        }
        // Everything else travelled to the origin, and what the viewer reads is the ORIGIN's
        // own answer — whatever status that origin chose for it, which is why what separates
        // the two is the page CloudFront would have generated instead.
        assert!(
            !answer.body.contains(GENERATED_PAGE),
            "case {case}: expected the origin's own answer, got CloudFront's page with status \
             {} and body {:?}",
            answer.status,
            answer.body
        );
    }

    // The positive control: the same GET that was refused with content is served without it.
    let served = view(&router, "GET", &domain, SERVED_PATH, "").await;
    assert_eq!(
        (served.status, served.x_cache.as_str()),
        (200, MISS),
        "case the GET that carries no content: expected the origin's 200 with X-Cache {MISS:?}, \
         got {} with {:?}",
        served.status,
        served.x_cache
    );
    assert!(
        served.body.contains("DistributionList"),
        "case the GET that carries no content: expected the origin's own bytes, got {:?}",
        served.body
    );
}
