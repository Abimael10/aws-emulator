// SPDX-License-Identifier: CC0-1.0

//! What the edge does to a viewer request on its way to a LAMBDA FUNCTION URL origin, and to
//! the answer on its way back: the CONTENT CODING it negotiates, the CACHE KEY the normalized
//! coding separates, and the parts of the request that reach the origin at all — end to end
//! over HTTP across the services on one engine.
//!
//! The topology is the one a function URL behind a distribution is ([`Topology`]): a running
//! Lambda function whose function URL is the origin's `DomainName`, a distribution in front of
//! it whose behaviour asks for automatic compression, and a viewer that reaches the
//! `DomainName` the control plane published with no AWS credential at all. The origin here
//! ECHOES the request it was handed — its path, its query string as it arrived, its cookies and
//! its headers — so what reached the origin is read off the origin's own answer rather than
//! inferred, and how often the origin was reached is read off Lambda's own `Invocations`
//! series, which is the owning service's record of it.
//!
//! The rules the rows span:
//!
//! * a behaviour whose `Compress` is true serves an answer under the coding the viewer
//!   NEGOTIATED out of the ones its cache policy enables (`EnableAcceptEncodingBrotli` /
//!   `EnableAcceptEncodingGzip`), Brotli ahead of gzip, and the bytes really are that coding's:
//!   every row DECODES them and finds the origin's own document;
//! * that normalized `Accept-Encoding` is part of the CACHE KEY — "includes the normalized
//!   header in the cache key" — so two viewers that negotiated the same coding in different
//!   words share one cached object (`X-Cache: Hit from cloudfront`, with no second origin
//!   request) while two that negotiated different codings are two objects, each fetched from
//!   the origin exactly once;
//! * the origin request carries the UNION of what the behaviour's CACHE POLICY names and what
//!   its ORIGIN REQUEST POLICY forwards — "the headers, cookies, and query strings that are
//!   included in the cache key are also included in requests that CloudFront sends to the
//!   origin" — and nothing else, so a header, a cookie or a query-string parameter named by
//!   neither policy never reaches the origin.
//!
//! The rows differ along what each rule generalises over: what the viewer accepts (one coding,
//! several in one order, a weighted refusal, the wildcard, a coding neither flag enables,
//! nothing at all), the coding the answer is served under, whether the object was already
//! cached, which PART of the request a parameter is (a header, a cookie, a query-string
//! parameter), which policy names it (the cache policy, the origin request policy, neither),
//! and whether the behaviour carries an origin request policy at all.

use serde_json::Value;

use crate::distribution_lifecycle::API_VERSION;
use crate::edge_lambda_url_origin::{element, Topology};
use crate::edge_request_shaping::{view, Viewed, HIT, MISS};

/// The handler behind the function URL: it ECHOES the request it was handed — the path, the
/// query string as it arrived, the cookies and the headers — so the viewer's answer says what
/// reached the origin. It imports only the standard library, so the report is the handler's
/// own work.
const ECHO_SOURCE: &str = r#"
import json


def lambda_handler(event, context):
    headers = event.get("headers") or {}
    return {
        "statusCode": 200,
        "headers": {"content-type": "application/json"},
        "body": json.dumps(
            {
                "marker": "ORIGIN-ECHO-OK",
                "path": event.get("rawPath", ""),
                "query": event.get("rawQueryString", ""),
                "cookies": sorted(event.get("cookies") or []),
                "headers": {name.lower(): value for name, value in headers.items()},
            }
        ),
    }
"#;

/// The marker [`ECHO_SOURCE`] puts in every answer of its own, so an answer that carries it
/// came from the origin.
const ECHO_MARKER: &str = "ORIGIN-ECHO-OK";

/// The media type [`ECHO_SOURCE`] states for its report, which is one of the types automatic
/// compression applies to.
const ECHO_MEDIA_TYPE: &str = "application/json";

/// The TTLs every cache policy below carries, in seconds: an hour, so an object the edge
/// cached is still fresh when the next row asks for it.
const TTL: i64 = 3600;

// ---------------------------------------------------------------------------
// The policies and the distribution each test stages
// ---------------------------------------------------------------------------

/// The parts of a viewer request ONE policy names: the header, cookie and query-string names
/// it lists. An empty list is a behaviour that names none of that part.
#[derive(Debug, Default, Clone, Copy)]
struct Listed<'a> {
    headers: &'a [&'a str],
    cookies: &'a [&'a str],
    query_strings: &'a [&'a str],
}

/// One `(Quantity, Items)` wrapper of names under `wrapper`, each name in the `Name` element
/// the model gives `HeaderList`, `CookieNameList` and `QueryStringNamesList`. A behaviour that
/// names none carries no wrapper at all.
fn names(wrapper: &str, values: &[&str]) -> String {
    if values.is_empty() {
        return String::new();
    }
    let items: String = values
        .iter()
        .map(|value| format!("<Name>{value}</Name>"))
        .collect();
    format!(
        "<{wrapper}><Quantity>{}</Quantity><Items>{items}</Items></{wrapper}>",
        values.len()
    )
}

/// The behaviour a list of names states: `whitelist` over the names it lists, and `none` for a
/// part the policy names nothing of.
fn behavior(values: &[&str]) -> &'static str {
    if values.is_empty() {
        "none"
    } else {
        "whitelist"
    }
}

/// Create the cache policy that keys on `keyed` with the two compression flags, and answer the
/// id it minted.
async fn cache_policy(
    topology: &Topology,
    case: &str,
    name: &str,
    keyed: Listed<'_>,
    gzip: bool,
    brotli: bool,
) -> String {
    let body = format!(
        "<CachePolicyConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <Name>{name}</Name>\
         <MinTTL>{TTL}</MinTTL><DefaultTTL>{TTL}</DefaultTTL><MaxTTL>{TTL}</MaxTTL>\
         <ParametersInCacheKeyAndForwardedToOrigin>\
         <EnableAcceptEncodingGzip>{gzip}</EnableAcceptEncodingGzip>\
         <EnableAcceptEncodingBrotli>{brotli}</EnableAcceptEncodingBrotli>\
         <HeadersConfig><HeaderBehavior>{}</HeaderBehavior>{}</HeadersConfig>\
         <CookiesConfig><CookieBehavior>{}</CookieBehavior>{}</CookiesConfig>\
         <QueryStringsConfig><QueryStringBehavior>{}</QueryStringBehavior>{}\
         </QueryStringsConfig>\
         </ParametersInCacheKeyAndForwardedToOrigin>\
         </CachePolicyConfig>",
        behavior(keyed.headers),
        names("Headers", keyed.headers),
        behavior(keyed.cookies),
        names("Cookies", keyed.cookies),
        behavior(keyed.query_strings),
        names("QueryStrings", keyed.query_strings),
    );
    let (status, raw) = topology
        .cloudfront(case, "POST", &format!("/{API_VERSION}/cache-policy"), Some(&body))
        .await;
    assert_eq!(
        status, 201,
        "setup {case}: creating the cache policy {name:?} answered {status} with {raw}"
    );
    let id = element(&raw, "Id");
    assert!(
        !id.is_empty(),
        "setup {case}: the created cache policy reports no id: {raw}"
    );
    id
}

/// Create the origin request policy that forwards `forwarded`, and answer the id it minted.
async fn origin_request_policy(
    topology: &Topology,
    case: &str,
    name: &str,
    forwarded: Listed<'_>,
) -> String {
    let body = format!(
        "<OriginRequestPolicyConfig \
         xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <Name>{name}</Name>\
         <HeadersConfig><HeaderBehavior>{}</HeaderBehavior>{}</HeadersConfig>\
         <CookiesConfig><CookieBehavior>{}</CookieBehavior>{}</CookiesConfig>\
         <QueryStringsConfig><QueryStringBehavior>{}</QueryStringBehavior>{}\
         </QueryStringsConfig>\
         </OriginRequestPolicyConfig>",
        behavior(forwarded.headers),
        names("Headers", forwarded.headers),
        behavior(forwarded.cookies),
        names("Cookies", forwarded.cookies),
        behavior(forwarded.query_strings),
        names("QueryStrings", forwarded.query_strings),
    );
    let (status, raw) = topology
        .cloudfront(
            case,
            "POST",
            &format!("/{API_VERSION}/origin-request-policy"),
            Some(&body),
        )
        .await;
    assert_eq!(
        status, 201,
        "setup {case}: creating the origin request policy {name:?} answered {status} with {raw}"
    );
    let id = element(&raw, "Id");
    assert!(
        !id.is_empty(),
        "setup {case}: the created origin request policy reports no id: {raw}"
    );
    id
}

/// One deployed distribution over the function URL `origin` whose default behaviour asks for
/// automatic compression under `cache_policy_id`, optionally naming an origin request policy:
/// the `DomainName` a viewer reaches it at.
async fn deployed(
    topology: &Topology,
    case: &str,
    caller_reference: &str,
    origin: &str,
    cache_policy_id: &str,
    origin_request_policy_id: Option<&str>,
) -> String {
    let forwarding = origin_request_policy_id
        .map(|id| format!("<OriginRequestPolicyId>{id}</OriginRequestPolicyId>"))
        .unwrap_or_default();
    let config = format!(
        "<DistributionConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <CallerReference>{caller_reference}</CallerReference>\
         <Origins><Quantity>1</Quantity><Items><Origin>\
         <Id>function-url</Id><DomainName>{origin}</DomainName>\
         <CustomOriginConfig><HTTPPort>80</HTTPPort><HTTPSPort>443</HTTPSPort>\
         <OriginProtocolPolicy>https-only</OriginProtocolPolicy>\
         <OriginSslProtocols><Quantity>1</Quantity>\
         <Items><SslProtocol>TLSv1.2</SslProtocol></Items></OriginSslProtocols>\
         </CustomOriginConfig>\
         </Origin></Items></Origins>\
         <DefaultCacheBehavior><TargetOriginId>function-url</TargetOriginId>\
         <ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy>\
         <AllowedMethods><Quantity>2</Quantity>\
         <Items><Method>GET</Method><Method>HEAD</Method></Items>\
         <CachedMethods><Quantity>2</Quantity>\
         <Items><Method>GET</Method><Method>HEAD</Method></Items></CachedMethods>\
         </AllowedMethods>\
         <Compress>true</Compress>\
         <CachePolicyId>{cache_policy_id}</CachePolicyId>{forwarding}\
         </DefaultCacheBehavior>\
         <Comment>a function url origin behind a compressing behaviour</Comment>\
         <Enabled>true</Enabled>\
         </DistributionConfig>"
    );
    let (id, _arn, domain) = topology.created_distribution(case, &config).await;
    topology.awaited_deployment(case, &id).await;
    domain
}

/// The origin's own report out of one answer the edge served, decoded under the coding the
/// answer says it is in — which is what proves the bytes really are that coding's.
fn report(answer: &Viewed, case: &str) -> Value {
    let decoded = answer.decoded(case);
    assert!(
        decoded.contains(ECHO_MARKER),
        "case {case:?}: expected the origin's own report, got {decoded:?}"
    );
    serde_json::from_str(&decoded)
        .unwrap_or_else(|error| panic!("case {case:?}: the origin's report is no document \
             ({error}): {decoded:?}"))
}

// ---------------------------------------------------------------------------
// The coding the viewer negotiated
// ---------------------------------------------------------------------------

/// RULE: a behaviour that asks for compression serves an answer under the coding the viewer
/// negotiated out of the ones its cache policy enables, Brotli ahead of gzip, and the bytes it
/// serves really are that coding's.
///
/// The rows differ along what the rule generalises over: what the viewer accepts — one coding,
/// several in one order, a coding refused with a weight, the wildcard, a coding neither flag
/// enables, and nothing at all — and the coding the answer is served under. Each row asks for a
/// path of its OWN, so every row is an answer the origin produced for it.
#[tokio::test]
async fn a_function_url_origins_answer_is_served_under_the_coding_the_viewer_negotiated() {
    let topology = Topology::new();
    let case = "a compressing behaviour over a function URL origin";
    let function = "cf-edge-url-coding";
    let origin = topology
        .public_function_url_host_running(case, function, ECHO_SOURCE)
        .await;
    let policy = cache_policy(
        &topology,
        case,
        "url-origin-both-codings",
        Listed::default(),
        true,
        true,
    )
    .await;
    let domain = deployed(
        &topology,
        case,
        "edge-lambda-url-coding",
        &origin,
        &policy,
        None,
    )
    .await;

    // (case, the path this row asks for, what the viewer accepts, the coding the answer
    // carries)
    let rows: [(&str, &str, Option<&str>, Option<&str>); 6] = [
        (
            "a viewer that accepts brotli",
            "/echo/brotli",
            Some("br"),
            Some("br"),
        ),
        (
            "a viewer that accepts several codings is a brotli viewer",
            "/echo/both",
            Some("gzip, deflate, br"),
            Some("br"),
        ),
        (
            "a viewer that refused brotli with a weight is a gzip viewer",
            "/echo/weighted",
            Some("br;q=0, gzip"),
            Some("gzip"),
        ),
        (
            "a viewer that accepts any coding is served the preferred one",
            "/echo/wildcard",
            Some("*"),
            Some("br"),
        ),
        (
            "a viewer that accepts neither coding is served the origin's own bytes",
            "/echo/identity",
            Some("identity"),
            None,
        ),
        (
            "a viewer that negotiated nothing has no coding to normalize",
            "/echo/none",
            None,
            None,
        ),
    ];

    for (case, path, accepts, coding) in rows {
        let headers: Vec<(&str, &str)> = accepts
            .map(|value| vec![("accept-encoding", value)])
            .unwrap_or_default();
        let answer = view(&topology.router, &domain, path, &headers).await;
        assert_eq!(
            (
                answer.status,
                answer.content_encoding.as_deref(),
                answer.x_cache.as_str()
            ),
            (200, coding, MISS),
            "case {case:?}: expected 200 under Content-Encoding {coding:?} with X-Cache \
             {MISS:?}, got {} under {:?} with {:?}",
            answer.status,
            answer.content_encoding,
            answer.x_cache
        );
        // The answer keeps the media type the ORIGIN stated, whatever coding the bytes
        // travelled under.
        assert_eq!(
            answer.content_type.as_deref(),
            Some(ECHO_MEDIA_TYPE),
            "case {case:?}: expected the origin's own media type, got {:?}",
            answer.content_type
        );
        let report = report(&answer, case);
        assert_eq!(
            report["path"].as_str().unwrap_or_default(),
            path,
            "case {case:?}: expected the origin to have been asked for the viewer's path, got \
             {report}"
        );
    }
}

// ---------------------------------------------------------------------------
// The normalized coding in the cache key
// ---------------------------------------------------------------------------

/// One step of the cache-key sequence: what the viewer accepts, the coding the answer carries,
/// where the answer came from, and how many origin requests the step must make.
struct CacheStep {
    case: &'static str,
    accepts: Option<&'static str>,
    coding: Option<&'static str>,
    x_cache: &'static str,
    origin_requests: f64,
}

/// RULE: the normalized `Accept-Encoding` is part of the cache key, so viewers that negotiated
/// the same coding share one cached object while viewers that negotiated different codings are
/// different objects — and each object is fetched from the origin exactly once.
///
/// Every step asks for the SAME path, so the coding is the only part of the key that differs.
/// How often the origin was reached is read off Lambda's own `Invocations` series for the
/// function behind the URL, which is the owning service's record of it rather than an absence
/// in the answer. The steps differ along the coding negotiated, the words the viewer negotiated
/// it in, and whether that object was already cached.
#[tokio::test]
async fn the_normalized_coding_separates_what_the_function_url_origin_is_asked_for() {
    let topology = Topology::new();
    let case = "the cache key over a function URL origin";
    let function = "cf-edge-url-cache-key";
    let origin = topology
        .public_function_url_host_running(case, function, ECHO_SOURCE)
        .await;
    let policy = cache_policy(
        &topology,
        case,
        "url-origin-keyed-on-coding",
        Listed::default(),
        true,
        true,
    )
    .await;
    let domain = deployed(
        &topology,
        case,
        "edge-lambda-url-cache-key",
        &origin,
        &policy,
        None,
    )
    .await;
    let path = "/echo/keyed";

    let steps: [CacheStep; 7] = [
        CacheStep {
            case: "the first brotli viewer, which the origin answers",
            accepts: Some("br"),
            coding: Some("br"),
            x_cache: MISS,
            origin_requests: 1.0,
        },
        CacheStep {
            case: "the same brotli viewer again, served from the cache",
            accepts: Some("br"),
            coding: Some("br"),
            x_cache: HIT,
            origin_requests: 0.0,
        },
        CacheStep {
            case: "a viewer that negotiated brotli in other words is that same object",
            accepts: Some("gzip, deflate, br"),
            coding: Some("br"),
            x_cache: HIT,
            origin_requests: 0.0,
        },
        CacheStep {
            case: "a gzip viewer is another object, which the origin answers",
            accepts: Some("gzip"),
            coding: Some("gzip"),
            x_cache: MISS,
            origin_requests: 1.0,
        },
        CacheStep {
            case: "the same gzip viewer again, served from the cache",
            accepts: Some("gzip"),
            coding: Some("gzip"),
            x_cache: HIT,
            origin_requests: 0.0,
        },
        CacheStep {
            case: "a viewer that negotiated nothing is a third object",
            accepts: None,
            coding: None,
            x_cache: MISS,
            origin_requests: 1.0,
        },
        CacheStep {
            case: "that viewer again, served from the cache",
            accepts: None,
            coding: None,
            x_cache: HIT,
            origin_requests: 0.0,
        },
    ];

    for step in steps {
        let before = topology.invocations(step.case, function).await;
        let headers: Vec<(&str, &str)> = step
            .accepts
            .map(|value| vec![("accept-encoding", value)])
            .unwrap_or_default();
        let answer = view(&topology.router, &domain, path, &headers).await;
        let after = topology.invocations(step.case, function).await;
        assert_eq!(
            (
                answer.status,
                answer.content_encoding.as_deref(),
                answer.x_cache.as_str()
            ),
            (200, step.coding, step.x_cache),
            "case {:?}: expected 200 under Content-Encoding {:?} with X-Cache {:?}, got {} \
             under {:?} with {:?}",
            step.case,
            step.coding,
            step.x_cache,
            answer.status,
            answer.content_encoding,
            answer.x_cache
        );
        assert_eq!(
            after - before,
            step.origin_requests,
            "case {:?}: expected {} origin request(s), but the function's invocations went \
             from {before} to {after}",
            step.case,
            step.origin_requests
        );
        let report = report(&answer, step.case);
        assert_eq!(
            report["path"].as_str().unwrap_or_default(),
            path,
            "case {:?}: expected the cached answer to be the one for the viewer's path, got \
             {report}",
            step.case
        );
    }
}

// ---------------------------------------------------------------------------
// What reaches the origin at all
// ---------------------------------------------------------------------------

/// Which part of a viewer request a row is about. All three are named by both policies with the
/// same vocabulary, and all three are read back off the origin's own echo.
#[derive(Debug, Clone, Copy)]
enum Part {
    /// A header the viewer set, echoed by name in the origin's report.
    Header,
    /// A cookie the viewer sent in its `Cookie` header, echoed as a `name=value` pair.
    Cookie,
    /// A query-string parameter, echoed inside the query string as it arrived.
    QueryString,
}

impl Part {
    /// Whether the origin's report says this `name=value` pair reached it.
    fn reached(self, report: &Value, name: &str, value: &str) -> bool {
        let pair = format!("{name}={value}");
        match self {
            Self::Header => report["headers"][name].as_str() == Some(value),
            Self::Cookie => report["cookies"].as_array().is_some_and(|cookies| {
                cookies
                    .iter()
                    .any(|cookie| cookie.as_str() == Some(pair.as_str()))
            }),
            Self::QueryString => report["query"]
                .as_str()
                .unwrap_or_default()
                .split('&')
                .any(|parameter| parameter == pair.as_str()),
        }
    }
}

/// Which policy of the behaviour names a parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NamedBy {
    /// The cache policy, so the parameter is in the cache key and therefore also in the origin
    /// request.
    CachePolicy,
    /// The origin request policy, so the parameter reaches the origin without taking part in
    /// the cache key.
    OriginRequestPolicy,
    /// Neither policy.
    Neither,
}

/// One parameter the viewer sends: which part of the request it is, its name and value, and
/// which policy names it.
struct ParameterRow {
    case: &'static str,
    part: Part,
    name: &'static str,
    value: &'static str,
    named_by: NamedBy,
}

/// The header, cookie and query-string names the CACHE POLICY keys on.
const KEYED_HEADER: &str = "x-keyed-header";
const KEYED_COOKIE: &str = "keyedcookie";
const KEYED_QUERY: &str = "keyedparam";

/// The ones the ORIGIN REQUEST POLICY forwards without keying on them.
const FORWARDED_HEADER: &str = "x-forwarded-header";
const FORWARDED_COOKIE: &str = "forwardedcookie";
const FORWARDED_QUERY: &str = "forwardedparam";

/// The ones NEITHER policy names.
const UNLISTED_HEADER: &str = "x-unlisted-header";
const UNLISTED_COOKIE: &str = "unlistedcookie";
const UNLISTED_QUERY: &str = "unlistedparam";

/// Every parameter the one viewer request below carries: each part of the request, once per
/// policy that might name it.
const PARAMETERS: [ParameterRow; 9] = [
    ParameterRow {
        case: "a header the cache policy keys on",
        part: Part::Header,
        name: KEYED_HEADER,
        value: "keyed-header-value",
        named_by: NamedBy::CachePolicy,
    },
    ParameterRow {
        case: "a header the origin request policy forwards",
        part: Part::Header,
        name: FORWARDED_HEADER,
        value: "forwarded-header-value",
        named_by: NamedBy::OriginRequestPolicy,
    },
    ParameterRow {
        case: "a header neither policy names",
        part: Part::Header,
        name: UNLISTED_HEADER,
        value: "unlisted-header-value",
        named_by: NamedBy::Neither,
    },
    ParameterRow {
        case: "a cookie the cache policy keys on",
        part: Part::Cookie,
        name: KEYED_COOKIE,
        value: "keyed-cookie-value",
        named_by: NamedBy::CachePolicy,
    },
    ParameterRow {
        case: "a cookie the origin request policy forwards",
        part: Part::Cookie,
        name: FORWARDED_COOKIE,
        value: "forwarded-cookie-value",
        named_by: NamedBy::OriginRequestPolicy,
    },
    ParameterRow {
        case: "a cookie neither policy names",
        part: Part::Cookie,
        name: UNLISTED_COOKIE,
        value: "unlisted-cookie-value",
        named_by: NamedBy::Neither,
    },
    ParameterRow {
        case: "a query-string parameter the cache policy keys on",
        part: Part::QueryString,
        name: KEYED_QUERY,
        value: "1",
        named_by: NamedBy::CachePolicy,
    },
    ParameterRow {
        case: "a query-string parameter the origin request policy forwards",
        part: Part::QueryString,
        name: FORWARDED_QUERY,
        value: "2",
        named_by: NamedBy::OriginRequestPolicy,
    },
    ParameterRow {
        case: "a query-string parameter neither policy names",
        part: Part::QueryString,
        name: UNLISTED_QUERY,
        value: "3",
        named_by: NamedBy::Neither,
    },
];

/// The viewer path every configuration below is asked for.
const SHAPED_PATH: &str = "/echo/shaped";

/// The one viewer request each configuration is sent: the path, its query string and the
/// headers (the `Cookie` among them) that carry [`PARAMETERS`].
fn viewer_request() -> (String, Vec<(&'static str, String)>) {
    let query: Vec<String> = PARAMETERS
        .iter()
        .filter(|row| matches!(row.part, Part::QueryString))
        .map(|row| format!("{}={}", row.name, row.value))
        .collect();
    let cookie: Vec<String> = PARAMETERS
        .iter()
        .filter(|row| matches!(row.part, Part::Cookie))
        .map(|row| format!("{}={}", row.name, row.value))
        .collect();
    let mut headers: Vec<(&'static str, String)> = PARAMETERS
        .iter()
        .filter(|row| matches!(row.part, Part::Header))
        .map(|row| (row.name, row.value.to_string()))
        .collect();
    headers.push(("cookie", cookie.join("; ")));
    (
        format!("{SHAPED_PATH}?{}", query.join("&")),
        headers,
    )
}

/// RULE: the origin request carries the union of what the behaviour's cache policy names and
/// what its origin request policy forwards, and nothing else — so a header, a cookie or a
/// query-string parameter named by neither never reaches the origin.
///
/// The one viewer request carries nine parameters: each part of a request (a header, a cookie,
/// a query-string parameter) named by the cache policy, by the origin request policy and by
/// neither. It is sent to two configurations, which differ along the policies in force: one
/// where each policy names one parameter of each part, and one whose cache policy names nothing
/// at all and which carries no origin request policy — where every one of the nine is withheld,
/// whichever name it has. What reached the origin is read off the origin's own echo.
#[tokio::test]
async fn a_parameter_neither_policy_names_never_reaches_a_function_url_origin() {
    let topology = Topology::new();
    let case = "the origin request over a function URL origin";
    let function = "cf-edge-url-shaping";
    let origin = topology
        .public_function_url_host_running(case, function, ECHO_SOURCE)
        .await;

    let keyed = Listed {
        headers: &[KEYED_HEADER],
        cookies: &[KEYED_COOKIE],
        query_strings: &[KEYED_QUERY],
    };
    let forwarded = Listed {
        headers: &[FORWARDED_HEADER],
        cookies: &[FORWARDED_COOKIE],
        query_strings: &[FORWARDED_QUERY],
    };
    let naming = cache_policy(&topology, case, "url-origin-naming", keyed, false, true).await;
    let forwarding = origin_request_policy(&topology, case, "url-origin-forwarding", forwarded)
        .await;
    let naming_domain = deployed(
        &topology,
        case,
        "edge-lambda-url-naming",
        &origin,
        &naming,
        Some(&forwarding),
    )
    .await;

    let silent = cache_policy(
        &topology,
        case,
        "url-origin-silent",
        Listed::default(),
        false,
        true,
    )
    .await;
    let silent_domain = deployed(
        &topology,
        case,
        "edge-lambda-url-silent",
        &origin,
        &silent,
        None,
    )
    .await;

    // (case, the domain, whether a policy of this configuration can name a parameter at all)
    let configurations: [(&str, &str, bool); 2] = [
        (
            "a behaviour whose two policies each name one parameter of each part",
            naming_domain.as_str(),
            true,
        ),
        (
            "a behaviour whose cache policy names nothing and which has no origin request \
             policy",
            silent_domain.as_str(),
            false,
        ),
    ];

    let (path_and_query, headers) = viewer_request();
    for (configuration, domain, names_anything) in configurations {
        let stated: Vec<(&str, &str)> = headers
            .iter()
            .map(|(name, value)| (*name, value.as_str()))
            .collect();
        let answer = view(&topology.router, domain, &path_and_query, &stated).await;
        assert_eq!(
            (answer.status, answer.x_cache.as_str()),
            (200, MISS),
            "case {configuration:?}: expected the origin's own answer with X-Cache {MISS:?}, \
             got {} with {:?}",
            answer.status,
            answer.x_cache
        );
        let report = report(&answer, configuration);
        assert_eq!(
            report["path"].as_str().unwrap_or_default(),
            SHAPED_PATH,
            "case {configuration:?}: expected the origin to have been asked for the viewer's \
             path, got {report}"
        );
        for row in &PARAMETERS {
            let expected = names_anything && row.named_by != NamedBy::Neither;
            let reached = row.part.reached(&report, row.name, row.value);
            assert_eq!(
                reached, expected,
                "case {configuration:?} / {:?} ({:?} {}): expected reached == {expected}, got \
                 {reached} in {report}",
                row.case, row.part, row.name
            );
            // A parameter that is withheld is withheld whole: its NAME reaches the origin
            // nowhere, in no part of the request it was sent in.
            if !expected {
                assert!(
                    !report.to_string().contains(row.name),
                    "case {configuration:?} / {:?}: the withheld {} must reach the origin in \
                     no form, got {report}",
                    row.case,
                    row.name
                );
            }
        }
    }
}
