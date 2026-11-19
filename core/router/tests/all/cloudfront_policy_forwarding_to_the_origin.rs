// SPDX-License-Identifier: CC0-1.0

//! WHICH parts of a viewer request reach the ORIGIN of a CloudFront distribution, end to end
//! over HTTP across the two services on one engine.
//!
//! RULE: for each of the three parts of a viewer request a policy names — the HTTP headers, the
//! cookies and the URL query strings — the request CloudFront sends to the origin carries
//! exactly the names the matched cache behaviour's CACHE POLICY includes in the cache key
//! together with the names its ORIGIN REQUEST POLICY forwards, and a name NEITHER policy
//! carries is withheld from the origin. The two policies compose and neither constrains the
//! other: every `*Behavior` member of a cache policy documents that "even when this field is set
//! to `none`, any cookies that are listed in an `OriginRequestPolicy` _are_ included in origin
//! requests" (and the same sentence for headers and for query strings —
//! `smithy_query(query="shape",shape="CachePolicyCookiesConfig")` and its two twins), while
//! "the headers, cookies, and query strings that are included in the cache key are also
//! included in requests that CloudFront sends to the origin". So what an origin request policy
//! adds takes no part in the CACHE KEY either: two requests differing only in a part the cache
//! policy does not name are ONE cached object served by ONE origin fetch, and the values that
//! reached the origin on that fetch are the ones the first of them carried.
//!
//! Only a cross-service suite can pin this: the behaviour and its two policies are CloudFront's
//! records, and the thing that REPORTS what reached the origin is an origin another service
//! serves — an API Gateway stage whose one method is a `MOCK` integration reflecting its own
//! method request (`$input.params(...)`), so each row reads the origin request off what the
//! viewer was served. The wire harness is [`super::cloudfront_apigateway_origin_edge`]'s, reused
//! rather than repeated: that sibling pins the legacy `ForwardedValues` half of the same
//! decision, and [`super::cloudfront_edge_generated_headers`] the `CloudFront-*` headers the
//! edge generates for an origin request rather than receives from a viewer.
//!
//! The rows differ along what the rule generalises over: the PART of the request (a header, a
//! cookie, a query string), which policy names that part (the cache policy, the origin request
//! policy, both, neither), and the behaviour each policy carries for it (`none`, `whitelist`,
//! `allExcept`, and `all` / `allViewer` — the values
//! `smithy_query(query="shape",shape="OriginRequestPolicyCookieBehavior")`,
//! `...="OriginRequestPolicyHeaderBehavior"` and their cache-policy twins admit).

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::cloudfront_apigateway_origin_edge::{
    apigw, cloudfront, emulator, member, view, wait_until_deployed, API_VERSION, REGION, STAGE,
};

/// The resource of the reflecting api, and the viewer path that reaches it through the edge.
const ECHO_PATH: &str = "echo";
const VIEWER_PATH: &str = "/echo";

/// The `DefaultTTL` the cached policy below holds an answer for: long enough that no row can
/// outlive it, and nothing waits for it to pass.
const CACHED_TTL_SECONDS: u32 = 3600;

/// The `DefaultTTL` of a policy that caches NOTHING, so each row's one request is answered by
/// the origin and what it reports is that row's own request.
const UNCACHED_TTL_SECONDS: u32 = 0;

/// `X-Cache` for an answer the origin produced for this request, and for one the edge cache
/// held.
const MISS: &str = "Miss from cloudfront";
const HIT: &str = "Hit from cloudfront";

/// The behaviours the policies' `*Behavior` members admit, spelled as the model spells them.
const NONE: &str = "none";
const WHITELIST: &str = "whitelist";
const ALL_EXCEPT: &str = "allExcept";
const ALL: &str = "all";

/// `allViewer`, which is how `OriginRequestPolicyHeaderBehavior` spells "all HTTP headers in
/// viewer requests are included in requests that CloudFront sends to the origin".
const ALL_VIEWER: &str = "allViewer";

/// The three HEADER names every viewer below sends: the one a cache policy may key on, the one
/// an origin request policy may forward, and the one neither names.
const KEYED_HEADER: &str = "X-Sim-Keyed";
const FORWARDED_HEADER: &str = "X-Sim-Forwarded";
const UNLISTED_HEADER: &str = "X-Sim-Unlisted";

/// The field line a viewer carries its cookies in (RFC 9110 / RFC 6265), and the three COOKIE
/// names it carries there.
const COOKIE: &str = "Cookie";
const KEYED_COOKIE: &str = "sim_keyed";
const FORWARDED_COOKIE: &str = "sim_forwarded";
const UNLISTED_COOKIE: &str = "sim_unlisted";

/// The three QUERY STRING names, which a policy names exactly as the viewer spells them.
const KEYED_QUERY: &str = "simkeyed";
const FORWARDED_QUERY: &str = "simforwarded";
const UNLISTED_QUERY: &str = "simunlisted";

/// What the viewer sends for each of the nine (part, role) pairs: distinct values, so a value
/// that reached the origin says exactly which part under which name carried it.
const KEYED_HEADER_VALUE: &str = "h-keyed-1";
const FORWARDED_HEADER_VALUE: &str = "h-forwarded-1";
const UNLISTED_HEADER_VALUE: &str = "h-unlisted-1";
const KEYED_COOKIE_VALUE: &str = "c-keyed-1";
const FORWARDED_COOKIE_VALUE: &str = "c-forwarded-1";
const UNLISTED_COOKIE_VALUE: &str = "c-unlisted-1";
const KEYED_QUERY_VALUE: &str = "q-keyed-1";
const FORWARDED_QUERY_VALUE: &str = "q-forwarded-1";
const UNLISTED_QUERY_VALUE: &str = "q-unlisted-1";

// ---------------------------------------------------------------------------
// The policies a row configures
// ---------------------------------------------------------------------------

/// What one policy says about ONE part of a viewer request: the behaviour it carries, and the
/// names its list carries.
#[derive(Clone, Copy)]
struct Names {
    behavior: &'static str,
    listed: &'static [&'static str],
}

/// A behaviour that names nothing of its part, and lists nothing.
const NAMES_NONE: Names = Names {
    behavior: NONE,
    listed: &[],
};

/// What one policy says about all three parts.
#[derive(Clone, Copy)]
struct PolicyShape {
    headers: Names,
    cookies: Names,
    query_strings: Names,
}

/// A policy that names none of the three parts.
const SHAPE_NONE: PolicyShape = PolicyShape {
    headers: NAMES_NONE,
    cookies: NAMES_NONE,
    query_strings: NAMES_NONE,
};

/// The `<Headers>` / `<Cookies>` / `<QueryStrings>` wrapper of a name list, in the shape the
/// model gives it (`Quantity` and an `Items` list of `Name`). A behaviour that lists no name
/// leaves the wrapper out entirely, as `none`, `all` and `allViewer` do.
fn name_list(wrapper: &str, names: &[&str]) -> String {
    if names.is_empty() {
        return String::new();
    }
    let items: String = names
        .iter()
        .map(|name| format!("<Name>{name}</Name>"))
        .collect();
    format!(
        "<{wrapper}><Quantity>{}</Quantity><Items>{items}</Items></{wrapper}>",
        names.len()
    )
}

/// `cloudfront:CreateCachePolicy` over `shape`, answering the id it minted.
///
/// `DefaultTTL` and `MaxTTL` are `ttl`: zero for a policy that caches nothing, so every viewer
/// request reaches the origin, and [`CACHED_TTL_SECONDS`] where a row needs the edge to HOLD
/// what it served.
async fn create_cache_policy(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    shape: PolicyShape,
    ttl: u32,
) -> String {
    let body = format!(
        "<CachePolicyConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <Name>{name}</Name>\
         <DefaultTTL>{ttl}</DefaultTTL><MaxTTL>{ttl}</MaxTTL><MinTTL>0</MinTTL>\
         <ParametersInCacheKeyAndForwardedToOrigin>\
         <EnableAcceptEncodingGzip>false</EnableAcceptEncodingGzip>\
         <HeadersConfig><HeaderBehavior>{}</HeaderBehavior>{}</HeadersConfig>\
         <CookiesConfig><CookieBehavior>{}</CookieBehavior>{}</CookiesConfig>\
         <QueryStringsConfig><QueryStringBehavior>{}</QueryStringBehavior>{}\
         </QueryStringsConfig>\
         </ParametersInCacheKeyAndForwardedToOrigin>\
         </CachePolicyConfig>",
        shape.headers.behavior,
        name_list("Headers", shape.headers.listed),
        shape.cookies.behavior,
        name_list("Cookies", shape.cookies.listed),
        shape.query_strings.behavior,
        name_list("QueryStrings", shape.query_strings.listed),
    );
    minted_id(router, case, "CreateCachePolicy", "cache-policy", body).await
}

/// `cloudfront:CreateOriginRequestPolicy` over `shape`, answering the id it minted.
async fn create_origin_request_policy(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    shape: PolicyShape,
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
        shape.headers.behavior,
        name_list("Headers", shape.headers.listed),
        shape.cookies.behavior,
        name_list("Cookies", shape.cookies.listed),
        shape.query_strings.behavior,
        name_list("QueryStrings", shape.query_strings.listed),
    );
    minted_id(router, case, "CreateOriginRequestPolicy", "origin-request-policy", body).await
}

/// Make one CloudFront create under `path` and answer the `Id` it minted — the one reading both
/// policy creates above share.
async fn minted_id(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    path: &str,
    body: String,
) -> String {
    let created = cloudfront(
        router,
        "POST",
        &format!("/{API_VERSION}/{path}"),
        Bytes::from(body),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "setup {case:?}: {operation} must succeed, got {} with {:?}",
        created.status,
        created.text()
    );
    let id = created.element("Id");
    assert!(
        !id.is_empty(),
        "setup {case:?}: {operation} reports no Id: {:?}",
        created.text()
    );
    id
}

// ---------------------------------------------------------------------------
// The origin that REPORTS what reached it, and the distribution in front of it
// ---------------------------------------------------------------------------

/// A REST api with one `GET /echo` method whose `MOCK` integration REFLECTS the nine parts
/// under test, deployed to [`STAGE`]. Answers the api's id.
///
/// The response template reads the METHOD REQUEST's own parameters, so the document the stage
/// answers carries exactly what the origin request arrived with — and the empty string for a
/// name the origin request did not carry at all.
async fn deployed_reflecting_api(router: &MultiServiceRouter) -> String {
    let created = apigw(
        router,
        "CreateRestApi",
        "POST",
        "/restapis",
        Some(json!({ "name": "cloudfront-forwarding-echo" })),
        201,
    )
    .await;
    let api = member("CreateRestApi", &created, "id");
    let root = member("CreateRestApi", &created, "rootResourceId");
    let resource = apigw(
        router,
        "CreateResource",
        "POST",
        &format!("/restapis/{api}/resources/{root}"),
        Some(json!({ "pathPart": ECHO_PATH })),
        201,
    )
    .await;
    let echo = member("CreateResource", &resource, "id");
    apigw(
        router,
        "PutMethod",
        "PUT",
        &format!("/restapis/{api}/resources/{echo}/methods/GET"),
        Some(json!({ "authorizationType": "NONE" })),
        201,
    )
    .await;
    apigw(
        router,
        "PutIntegration",
        "PUT",
        &format!("/restapis/{api}/resources/{echo}/methods/GET/integration"),
        Some(json!({
            "type": "MOCK",
            "requestTemplates": { "application/json": r#"{"statusCode":200}"# },
        })),
        201,
    )
    .await;
    apigw(
        router,
        "PutMethodResponse",
        "PUT",
        &format!("/restapis/{api}/resources/{echo}/methods/GET/responses/200"),
        Some(json!({})),
        201,
    )
    .await;
    let template = format!(
        "{{\"hkeyed\":\"$input.params('{KEYED_HEADER}')\",\
         \"hforwarded\":\"$input.params('{FORWARDED_HEADER}')\",\
         \"hunlisted\":\"$input.params('{UNLISTED_HEADER}')\",\
         \"cookie\":\"$input.params('{COOKIE}')\",\
         \"qkeyed\":\"$input.params('{KEYED_QUERY}')\",\
         \"qforwarded\":\"$input.params('{FORWARDED_QUERY}')\",\
         \"qunlisted\":\"$input.params('{UNLISTED_QUERY}')\"}}"
    );
    apigw(
        router,
        "PutIntegrationResponse",
        "PUT",
        &format!("/restapis/{api}/resources/{echo}/methods/GET/integration/responses/200"),
        Some(json!({
            "statusCode": "200",
            "responseTemplates": { "application/json": template },
        })),
        201,
    )
    .await;
    apigw(
        router,
        "CreateDeployment",
        "POST",
        &format!("/restapis/{api}/deployments"),
        Some(json!({ "stageName": STAGE })),
        201,
    )
    .await;
    api
}

/// `cloudfront:CreateDistribution` over the api stage `api` as a custom origin, whose one
/// behaviour names `cache_policy` and `origin_request_policy`. Answers the distribution's
/// `(id, DomainName)`.
async fn create_distribution(
    router: &MultiServiceRouter,
    case: &str,
    caller_reference: &str,
    api: &str,
    cache_policy: &str,
    origin_request_policy: &str,
) -> (String, String) {
    let body = format!(
        "<DistributionConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <CallerReference>{caller_reference}</CallerReference>\
         <Origins><Quantity>1</Quantity><Items><Origin>\
         <Id>api-origin</Id>\
         <DomainName>{api}.execute-api.{REGION}.amazonaws.com</DomainName>\
         <OriginPath>/{STAGE}</OriginPath>\
         <CustomOriginConfig><HTTPPort>80</HTTPPort><HTTPSPort>443</HTTPSPort>\
         <OriginProtocolPolicy>https-only</OriginProtocolPolicy>\
         </CustomOriginConfig>\
         </Origin></Items></Origins>\
         <DefaultCacheBehavior><TargetOriginId>api-origin</TargetOriginId>\
         <ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy>\
         <CachePolicyId>{cache_policy}</CachePolicyId>\
         <OriginRequestPolicyId>{origin_request_policy}</OriginRequestPolicyId>\
         </DefaultCacheBehavior>\
         <Comment>what the policies forward to the origin</Comment>\
         <Enabled>true</Enabled>\
         </DistributionConfig>"
    );
    let created = cloudfront(
        router,
        "POST",
        &format!("/{API_VERSION}/distribution"),
        Bytes::from(body),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "setup {case:?}: CreateDistribution must succeed, got {} with {:?}",
        created.status,
        created.text()
    );
    let (id, domain) = (created.element("Id"), created.element("DomainName"));
    assert!(
        !id.is_empty() && !domain.is_empty(),
        "setup {case:?}: the created distribution reports no Id or DomainName: {:?}",
        created.text()
    );
    (id, domain)
}

// ---------------------------------------------------------------------------
// Reading the origin request off what the viewer was served
// ---------------------------------------------------------------------------

/// What the ORIGIN was told on the fetch behind one served viewer request, as the origin itself
/// reported it: the empty string for a part the origin request did not carry.
struct Reported {
    x_cache: String,
    headers: [String; 3],
    /// The whole `Cookie` field line the origin received, empty for an origin request carrying
    /// none.
    cookie: String,
    query_strings: [String; 3],
}

/// Drive ONE viewer request carrying all nine parts — the three headers, the three cookies in
/// one field line, and the three query strings with the unlisted one BETWEEN the other two —
/// and read [`Reported`] off what it was answered.
async fn reflected(
    router: &MultiServiceRouter,
    case: &str,
    domain: &str,
    header_values: [&str; 3],
    cookie_values: [&str; 3],
    query_values: [&str; 3],
) -> Reported {
    let cookie_line = format!(
        "{KEYED_COOKIE}={}; {FORWARDED_COOKIE}={}; {UNLISTED_COOKIE}={}",
        cookie_values[0], cookie_values[1], cookie_values[2]
    );
    let path = format!(
        "{VIEWER_PATH}?{KEYED_QUERY}={}&{UNLISTED_QUERY}={}&{FORWARDED_QUERY}={}",
        query_values[0], query_values[2], query_values[1]
    );
    let sent: [(&str, &str); 4] = [
        (KEYED_HEADER, header_values[0]),
        (FORWARDED_HEADER, header_values[1]),
        (UNLISTED_HEADER, header_values[2]),
        (COOKIE, cookie_line.as_str()),
    ];
    let answer = view(router, "https", domain, &path, &sent).await;
    assert_eq!(
        answer.status,
        200,
        "case {case:?}: expected the origin's answer through the edge, got {} with {:?}",
        answer.status,
        answer.text()
    );
    let body = answer.json();
    let read = |name: &str| {
        body.get(name)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    Reported {
        x_cache: answer.header("x-cache"),
        headers: [read("hkeyed"), read("hforwarded"), read("hunlisted")],
        cookie: read("cookie"),
        query_strings: [read("qkeyed"), read("qforwarded"), read("qunlisted")],
    }
}

/// One of the three ROLES a name has in a row: the name a cache policy may key on, the one an
/// origin request policy may forward, and the one neither names.
struct Role {
    what: &'static str,
    header: &'static str,
    header_value: &'static str,
    cookie: &'static str,
    cookie_value: &'static str,
    query: &'static str,
    query_value: &'static str,
    /// Where this role's values sit in a viewer's triples and in [`Reported`].
    at: usize,
}

const ROLES: [Role; 3] = [
    Role {
        what: "the name the cache policy may key on",
        header: KEYED_HEADER,
        header_value: KEYED_HEADER_VALUE,
        cookie: KEYED_COOKIE,
        cookie_value: KEYED_COOKIE_VALUE,
        query: KEYED_QUERY,
        query_value: KEYED_QUERY_VALUE,
        at: 0,
    },
    Role {
        what: "the name the origin request policy may forward",
        header: FORWARDED_HEADER,
        header_value: FORWARDED_HEADER_VALUE,
        cookie: FORWARDED_COOKIE,
        cookie_value: FORWARDED_COOKIE_VALUE,
        query: FORWARDED_QUERY,
        query_value: FORWARDED_QUERY_VALUE,
        at: 1,
    },
    Role {
        what: "the name neither policy carries",
        header: UNLISTED_HEADER,
        header_value: UNLISTED_HEADER_VALUE,
        cookie: UNLISTED_COOKIE,
        cookie_value: UNLISTED_COOKIE_VALUE,
        query: UNLISTED_QUERY,
        query_value: UNLISTED_QUERY_VALUE,
        at: 2,
    },
];

/// One row of the forwarding table: the two policies the behaviour names, and whether each
/// role's header, cookie and query string then reaches the origin.
struct ForwardingRow {
    case: &'static str,
    cache: PolicyShape,
    origin: PolicyShape,
    /// Indexed by [`Role::at`].
    arrives: [bool; 3],
}

/// RULE: the origin request carries the UNION of the parts the cache policy keys on and the
/// parts the origin request policy forwards — for headers, for cookies and for query strings
/// alike — and a name neither policy carries is withheld from the origin.
#[tokio::test]
async fn the_origin_sees_the_union_of_what_the_two_policies_name_and_nothing_else() {
    let router = emulator();
    let api = deployed_reflecting_api(&router).await;

    let keyed = PolicyShape {
        headers: Names {
            behavior: WHITELIST,
            listed: &[KEYED_HEADER],
        },
        cookies: Names {
            behavior: WHITELIST,
            listed: &[KEYED_COOKIE],
        },
        query_strings: Names {
            behavior: WHITELIST,
            listed: &[KEYED_QUERY],
        },
    };
    let forwards = PolicyShape {
        headers: Names {
            behavior: WHITELIST,
            listed: &[FORWARDED_HEADER],
        },
        cookies: Names {
            behavior: WHITELIST,
            listed: &[FORWARDED_COOKIE],
        },
        query_strings: Names {
            behavior: WHITELIST,
            listed: &[FORWARDED_QUERY],
        },
    };
    let forwards_everything = PolicyShape {
        // `allViewer` is this member's spelling of `all`; the other two parts say `all`.
        headers: Names {
            behavior: ALL_VIEWER,
            listed: &[],
        },
        cookies: Names {
            behavior: ALL,
            listed: &[],
        },
        query_strings: Names {
            behavior: ALL,
            listed: &[],
        },
    };
    let forwards_all_except = PolicyShape {
        headers: Names {
            behavior: ALL_EXCEPT,
            listed: &[UNLISTED_HEADER],
        },
        cookies: Names {
            behavior: ALL_EXCEPT,
            listed: &[UNLISTED_COOKIE],
        },
        query_strings: Names {
            behavior: ALL_EXCEPT,
            listed: &[UNLISTED_QUERY],
        },
    };
    let keys_all_except = PolicyShape {
        // `CachePolicyHeaderBehavior` admits `none` and `whitelist` alone — it has no
        // `allExcept` — so the header part of this shape names its two names outright.
        headers: Names {
            behavior: WHITELIST,
            listed: &[KEYED_HEADER, FORWARDED_HEADER],
        },
        cookies: Names {
            behavior: ALL_EXCEPT,
            listed: &[UNLISTED_COOKIE],
        },
        query_strings: Names {
            behavior: ALL_EXCEPT,
            listed: &[UNLISTED_QUERY],
        },
    };

    let rows: &[ForwardingRow] = &[
        ForwardingRow {
            case: "each policy whitelists one name: both travel, the third does not",
            cache: keyed,
            origin: forwards,
            arrives: [true, true, false],
        },
        ForwardingRow {
            case: "neither policy names anything: the origin sees none of the three",
            cache: SHAPE_NONE,
            origin: SHAPE_NONE,
            arrives: [false, false, false],
        },
        ForwardingRow {
            case: "the cache policy alone names a part, which reaches the origin because the \
                   cache key does",
            cache: keyed,
            origin: SHAPE_NONE,
            arrives: [true, false, false],
        },
        ForwardingRow {
            case: "the origin request policy alone names a part, which reaches the origin \
                   although the cache key excludes it",
            cache: SHAPE_NONE,
            origin: forwards,
            arrives: [false, true, false],
        },
        ForwardingRow {
            case: "the origin request policy forwards everything the viewer sent",
            cache: SHAPE_NONE,
            origin: forwards_everything,
            arrives: [true, true, true],
        },
        ForwardingRow {
            case: "the origin request policy forwards everything but the name its allExcept \
                   lists",
            cache: SHAPE_NONE,
            origin: forwards_all_except,
            arrives: [true, true, false],
        },
        ForwardingRow {
            case: "the cache policy keys on, and so forwards, everything but the name its \
                   allExcept lists",
            cache: keys_all_except,
            origin: SHAPE_NONE,
            arrives: [true, true, false],
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let cache = create_cache_policy(
            &router,
            case,
            &format!("forwarding-cache-{index}"),
            row.cache,
            UNCACHED_TTL_SECONDS,
        )
        .await;
        let origin = create_origin_request_policy(
            &router,
            case,
            &format!("forwarding-origin-{index}"),
            row.origin,
        )
        .await;
        let (id, domain) = create_distribution(
            &router,
            case,
            &format!("forwarding-{index}"),
            &api,
            &cache,
            &origin,
        )
        .await;
        wait_until_deployed(&router, case, &id).await;

        let reported = reflected(
            &router,
            case,
            &domain,
            [
                KEYED_HEADER_VALUE,
                FORWARDED_HEADER_VALUE,
                UNLISTED_HEADER_VALUE,
            ],
            [
                KEYED_COOKIE_VALUE,
                FORWARDED_COOKIE_VALUE,
                UNLISTED_COOKIE_VALUE,
            ],
            [
                KEYED_QUERY_VALUE,
                FORWARDED_QUERY_VALUE,
                UNLISTED_QUERY_VALUE,
            ],
        )
        .await;
        assert_eq!(
            reported.x_cache, MISS,
            "case {case:?}: this policy caches nothing, so the answer must be the origin's own \
             for this request, got X-Cache {:?}",
            reported.x_cache
        );

        for role in &ROLES {
            let arrives = row.arrives[role.at];
            let expected_header = if arrives { role.header_value } else { "" };
            assert_eq!(
                reported.headers[role.at], expected_header,
                "case {case:?}: {} ({}): expected the origin to be told {expected_header:?}, \
                 got {:?}",
                role.what, role.header, reported.headers[role.at]
            );
            let expected_query = if arrives { role.query_value } else { "" };
            assert_eq!(
                reported.query_strings[role.at], expected_query,
                "case {case:?}: {} ({}): expected the origin to be told {expected_query:?}, \
                 got {:?}",
                role.what, role.query, reported.query_strings[role.at]
            );
            // The cookies travel inside ONE field line, so what a row asserts is WHICH pairs
            // that line carries: the order the edge puts them in is its own and no rule of
            // this one.
            let pair = format!("{}={}", role.cookie, role.cookie_value);
            if arrives {
                assert!(
                    reported.cookie.contains(&pair),
                    "case {case:?}: {} ({}): expected the origin's Cookie line to carry \
                     {pair:?}, got {:?}",
                    role.what,
                    role.cookie,
                    reported.cookie
                );
            } else {
                assert!(
                    !reported.cookie.contains(role.cookie),
                    "case {case:?}: {} ({}): expected the origin's Cookie line to carry nothing \
                     of it, got {:?}",
                    role.what,
                    role.cookie,
                    reported.cookie
                );
            }
        }
        if row.arrives == [false, false, false] {
            assert_eq!(
                reported.cookie, "",
                "case {case:?}: a request whose every cookie is withheld must reach the origin \
                 with no Cookie field line at all, got {:?}",
                reported.cookie
            );
        }
    }
}

/// One row of the cache-key table: one viewer request against the one distribution, and what the
/// edge must answer it from.
struct KeyRow {
    case: &'static str,
    /// The value this viewer sends for the forwarded header, the forwarded cookie and the
    /// forwarded query string alike.
    forwarded: &'static str,
    /// The value it sends for the query string the cache key names.
    keyed_query: &'static str,
    x_cache: &'static str,
    /// The forwarded value the ORIGIN was told on the fetch this answer came from — the first
    /// request's, for an answer the cache held.
    origin_told: &'static str,
}

/// RULE: a part the cache policy does not name takes no part in the cache key, however the
/// origin request policy forwards it — so two requests differing only in such a part are one
/// cached object served by one origin fetch, and the values that reached the origin are the
/// ones the request behind that fetch carried.
#[tokio::test]
async fn the_parts_an_origin_request_policy_forwards_take_no_part_in_the_cache_key() {
    let router = emulator();
    let api = deployed_reflecting_api(&router).await;
    let case = "a cache key naming one query string, an origin request policy forwarding the rest";
    let cache = create_cache_policy(
        &router,
        case,
        "key-one-query-string",
        PolicyShape {
            headers: NAMES_NONE,
            cookies: NAMES_NONE,
            query_strings: Names {
                behavior: WHITELIST,
                listed: &[KEYED_QUERY],
            },
        },
        CACHED_TTL_SECONDS,
    )
    .await;
    let origin = create_origin_request_policy(
        &router,
        case,
        "forward-what-the-key-excludes",
        PolicyShape {
            headers: Names {
                behavior: WHITELIST,
                listed: &[FORWARDED_HEADER],
            },
            cookies: Names {
                behavior: WHITELIST,
                listed: &[FORWARDED_COOKIE],
            },
            query_strings: Names {
                behavior: WHITELIST,
                listed: &[FORWARDED_QUERY],
            },
        },
    )
    .await;
    let (id, domain) = create_distribution(
        &router,
        case,
        "cache-key-independence",
        &api,
        &cache,
        &origin,
    )
    .await;
    wait_until_deployed(&router, case, &id).await;

    // The rows differ along what the rule generalises over: which part of the request the two
    // viewers differ in (one the cache key excludes but the origin receives, one the cache key
    // names), and whether the object was already cached.
    let rows: &[KeyRow] = &[
        KeyRow {
            case: "the first viewer, which the origin answers",
            forwarded: "v1",
            keyed_query: "k1",
            x_cache: MISS,
            origin_told: "v1",
        },
        KeyRow {
            case: "a viewer differing only in the parts the cache key excludes, which is the \
                   same object",
            forwarded: "v2",
            keyed_query: "k1",
            x_cache: HIT,
            origin_told: "v1",
        },
        KeyRow {
            case: "a viewer differing in the query string the cache key names, which is another \
                   object",
            forwarded: "v2",
            keyed_query: "k2",
            x_cache: MISS,
            origin_told: "v2",
        },
        KeyRow {
            case: "that other object again, however the forwarded parts differ",
            forwarded: "v3",
            keyed_query: "k2",
            x_cache: HIT,
            origin_told: "v2",
        },
    ];

    for row in rows {
        let case = row.case;
        let reported = reflected(
            &router,
            case,
            &domain,
            [KEYED_HEADER_VALUE, row.forwarded, UNLISTED_HEADER_VALUE],
            [KEYED_COOKIE_VALUE, row.forwarded, UNLISTED_COOKIE_VALUE],
            [row.keyed_query, row.forwarded, UNLISTED_QUERY_VALUE],
        )
        .await;
        assert_eq!(
            reported.x_cache, row.x_cache,
            "case {case:?}: expected X-Cache {:?}, got {:?}",
            row.x_cache, reported.x_cache
        );
        assert_eq!(
            reported.headers[1], row.origin_told,
            "case {case:?}: expected the origin to have been told {:?} for {FORWARDED_HEADER}, \
             got {:?}",
            row.origin_told, reported.headers[1]
        );
        assert_eq!(
            reported.query_strings[1], row.origin_told,
            "case {case:?}: expected the origin to have been told {:?} for {FORWARDED_QUERY}, \
             got {:?}",
            row.origin_told, reported.query_strings[1]
        );
        let pair = format!("{FORWARDED_COOKIE}={}", row.origin_told);
        assert!(
            reported.cookie.contains(&pair),
            "case {case:?}: expected the origin's Cookie line to carry {pair:?}, got {:?}",
            reported.cookie
        );
        // Neither policy names the other two names, so neither reaches the origin on any of
        // these fetches.
        assert_eq!(
            (
                reported.headers[0].as_str(),
                reported.headers[2].as_str(),
                reported.query_strings[2].as_str()
            ),
            ("", "", ""),
            "case {case:?}: expected the origin to be told nothing of the names neither policy \
             carries, got {:?}",
            reported.headers
        );
        for withheld in [KEYED_COOKIE, UNLISTED_COOKIE] {
            assert!(
                !reported.cookie.contains(withheld),
                "case {case:?}: expected the origin's Cookie line to carry nothing of \
                 {withheld:?}, got {:?}",
                reported.cookie
            );
        }
    }
}
