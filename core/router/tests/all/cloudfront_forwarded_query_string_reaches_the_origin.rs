// SPDX-License-Identifier: CC0-1.0

//! WHAT the query string of a viewer request looks like when it arrives at the ORIGIN of a
//! CloudFront distribution, end to end over HTTP across the two services on one engine.
//!
//! RULE: **where the matched cache behaviour's forwarding carries the query string, the origin
//! request carries that viewer request's OWN parameters as the viewer spelled them — every
//! selected `name=value` pair, with its own value, in the order it was sent — and where the
//! behaviour carries no query string the origin is sent none of it.**
//!
//! `ForwardedValues$QueryString` states both halves of that: "if you specify true for
//! `QueryString` and you don't specify any values for `QueryStringCacheKeys`, CloudFront
//! forwards all query string parameters to the origin and caches based on all query string
//! parameters"; "if you specify true for `QueryString` and you specify one or more values for
//! `QueryStringCacheKeys`, CloudFront forwards all query string parameters to the origin, but
//! it only caches based on the query string parameters that you specify"; and "if you specify
//! false for `QueryString`, CloudFront doesn't forward any query string parameters to the
//! origin" (`smithy_query(query="shape",shape="ForwardedValues")`). A behaviour stating the
//! same thing with POLICIES is the other spelling of one reading — "the headers, cookies, and
//! query strings that are included in the cache key are also included in requests that
//! CloudFront sends to the origin", beside what an origin request policy forwards — so a
//! distribution carrying `Managed-CachingDisabled` with `Managed-AllViewer` has to hand the
//! origin the same parameters as the legacy member that forwards them.
//!
//! Only a cross-service suite can pin it: the behaviour and the origin are CloudFront's
//! records, and the thing that REPORTS what reached the origin is an origin another service
//! serves — an API Gateway stage whose one method is a `MOCK` integration reflecting its own
//! method request (`$input.params(...)`), so each row reads the parameters the origin request
//! arrived with off what the viewer was served. The wire harness is
//! [`super::cloudfront_apigateway_origin_edge`]'s, reused rather than repeated: that sibling
//! pins WHICH parts of a request a legacy `ForwardedValues` carries and
//! [`super::cloudfront_policy_forwarding_to_the_origin`] which parts the two policies carry,
//! each over one parameter of one name; this suite varies the QUERY STRING itself.
//!
//! The rows differ along what the rule generalises over: how MANY parameters the viewer sent,
//! the ORDER it sent them in, whether a value is empty, whether its characters are
//! percent-encoded, whether one name carries two values, and whether a parameter was sent with
//! no value at all — against each of the three forwardings the rule reads (the legacy member
//! carrying the query string, the same member withholding it, and the policy pair that carries
//! it). The second test varies what the cache key NARROWS to, which changes nothing about what
//! is forwarded.

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::cloudfront_apigateway_origin_edge::{
    apigw, cloudfront, emulator, member, view, wait_until_deployed, Answer, API_VERSION, REGION,
    STAGE,
};

/// The resource of the reflecting api, and the viewer path that reaches it through the edge.
const ECHO_PATH: &str = "echo";
const VIEWER_PATH: &str = "/echo";

/// The three query-string names the reflecting origin reports a value for, and the one it
/// reports for a parameter sent with no value at all.
const ALPHA: &str = "alpha";
const BETA: &str = "beta";
const GAMMA: &str = "gamma";
const FLAG: &str = "flag";

/// The one viewer header the rows send, which the legacy rows whitelist by name: a row's own
/// value, so two rows are never one cached object.
const TAG_HEADER: &str = "X-Sim-Tag";

/// `X-Cache` for an answer the ORIGIN produced for this very request, and for one the edge
/// cache held from an earlier one.
const MISS: &str = "Miss from cloudfront";
const HIT: &str = "Hit from cloudfront";

/// The AWS-managed cache policy that caches nothing (`Managed-CachingDisabled`) and the
/// managed origin request policy that forwards "all parameters in viewer requests"
/// (`Managed-AllViewer`), by the ids Amazon publishes for them.
const CACHING_DISABLED: &str = "4135ea2d-6df8-44a3-9df3-4b5a84be39ad";
const ALL_VIEWER_POLICY: &str = "216adef6-5c7f-47e4-b989-5492eafa07d3";

/// How long an answer the edge MAY hold stays resident in the second test: long enough that no
/// row can outlive it, and nothing waits for it to pass.
const CACHED_TTL_SECONDS: u32 = 3600;

/// WHICH forwarding a distribution's default cache behaviour states.
enum Forwarding {
    /// The legacy `ForwardedValues`: whether it carries the query string, which parameters it
    /// narrows the CACHE KEY to, and the header names it whitelists.
    Legacy {
        query_string: bool,
        cache_keys: &'static [&'static str],
        headers: &'static [&'static str],
        ttl_seconds: u32,
    },
    /// The managed policy pair: a cache policy that caches nothing beside an origin request
    /// policy that forwards every parameter of a viewer request.
    Policies,
}

/// The `DefaultCacheBehavior` members a forwarding contributes, in the order the model
/// declares them (`smithy_query(query="shape",shape="DefaultCacheBehavior")`): the policy ids
/// first, then the legacy `ForwardedValues` with `MinTTL`, `DefaultTTL` and `MaxTTL`.
fn behavior_members(forwarding: &Forwarding) -> String {
    match forwarding {
        Forwarding::Policies => format!(
            "<CachePolicyId>{CACHING_DISABLED}</CachePolicyId>\
             <OriginRequestPolicyId>{ALL_VIEWER_POLICY}</OriginRequestPolicyId>"
        ),
        Forwarding::Legacy {
            query_string,
            cache_keys,
            headers,
            ttl_seconds,
        } => {
            let mut members = format!(
                "<ForwardedValues><QueryString>{query_string}</QueryString>\
                 <Cookies><Forward>none</Forward></Cookies>"
            );
            members.push_str(&name_list("Headers", headers));
            members.push_str(&name_list("QueryStringCacheKeys", cache_keys));
            members.push_str(&format!(
                "</ForwardedValues><MinTTL>0</MinTTL>\
                 <DefaultTTL>{ttl_seconds}</DefaultTTL><MaxTTL>{ttl_seconds}</MaxTTL>"
            ));
            members
        }
    }
}

/// One `(Quantity, Items)` list of names, or nothing at all when the forwarding names none.
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

/// A REST api with one `GET /echo` method whose `MOCK` integration REFLECTS the query-string
/// parameters and the one header its method request arrived with, deployed to [`STAGE`].
/// Answers the api's id.
async fn deployed_reflecting_api(router: &MultiServiceRouter) -> String {
    let created = apigw(
        router,
        "CreateRestApi",
        "POST",
        "/restapis",
        Some(json!({ "name": "cloudfront-forwarded-query-echo" })),
        201,
    )
    .await;
    let api = member("CreateRestApi", &created, "id");
    let root = member("CreateRestApi", &created, "rootResourceId");
    let created = apigw(
        router,
        "CreateResource",
        "POST",
        &format!("/restapis/{api}/resources/{root}"),
        Some(json!({ "pathPart": ECHO_PATH })),
        201,
    )
    .await;
    let resource = member("CreateResource", &created, "id");
    apigw(
        router,
        "PutMethod",
        "PUT",
        &format!("/restapis/{api}/resources/{resource}/methods/GET"),
        Some(json!({ "authorizationType": "NONE" })),
        201,
    )
    .await;
    apigw(
        router,
        "PutIntegration",
        "PUT",
        &format!("/restapis/{api}/resources/{resource}/methods/GET/integration"),
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
        &format!("/restapis/{api}/resources/{resource}/methods/GET/responses/200"),
        Some(json!({})),
        201,
    )
    .await;
    // The response template reads the METHOD REQUEST's own parameters, so the document the
    // stage answers carries exactly the query string and header the origin request arrived
    // with.
    let template = format!(
        "{{\"{ALPHA}\":\"$input.params('{ALPHA}')\",\
         \"{BETA}\":\"$input.params('{BETA}')\",\
         \"{GAMMA}\":\"$input.params('{GAMMA}')\",\
         \"{FLAG}\":\"$input.params('{FLAG}')\",\
         \"tag\":\"$input.params('{TAG_HEADER}')\"}}"
    );
    apigw(
        router,
        "PutIntegrationResponse",
        "PUT",
        &format!("/restapis/{api}/resources/{resource}/methods/GET/integration/responses/200"),
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

/// `cloudfront:CreateDistribution` over the api stage `api` as a custom origin, under
/// `forwarding`, answering the distribution's `(id, DomainName)` once CloudFront reports it
/// `Deployed`.
async fn deployed_distribution(
    router: &MultiServiceRouter,
    case: &str,
    caller_reference: &str,
    api: &str,
    forwarding: &Forwarding,
) -> (String, String) {
    let members = behavior_members(forwarding);
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
         {members}\
         </DefaultCacheBehavior>\
         <Comment>which query string the origin request carries</Comment>\
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
    wait_until_deployed(router, case, &id).await;
    (id, domain)
}

/// The value the reflecting origin reported under `name`, empty when it reported none.
fn reported(case: &str, answered: &Answer, name: &str) -> String {
    assert_eq!(
        answered.status,
        200,
        "case {case:?}: expected the edge to serve the origin's answer, got {} with {:?}",
        answered.status,
        answered.text()
    );
    answered
        .json()
        .get(name)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// One row of the query-string table: the query string the viewer sends, and the value the
/// origin has to report for each of the four names when the behaviour forwards it.
struct QueryRow {
    case: &'static str,
    query: &'static str,
    alpha: &'static str,
    beta: &'static str,
    gamma: &'static str,
    flag: &'static str,
}

fn query_rows() -> &'static [QueryRow] {
    &[
        QueryRow {
            case: "one parameter",
            query: "alpha=a1",
            alpha: "a1",
            beta: "",
            gamma: "",
            flag: "",
        },
        QueryRow {
            case: "three parameters, each under its own name",
            query: "alpha=a2&beta=b2&gamma=g2",
            alpha: "a2",
            beta: "b2",
            gamma: "g2",
            flag: "",
        },
        QueryRow {
            case: "the same three parameters sent in another order",
            query: "gamma=g3&alpha=a3&beta=b3",
            alpha: "a3",
            beta: "b3",
            gamma: "g3",
            flag: "",
        },
        QueryRow {
            case: "a parameter present with an empty value",
            query: "alpha=&beta=b4",
            alpha: "",
            beta: "b4",
            gamma: "",
            flag: "",
        },
        QueryRow {
            case: "a value whose characters are percent-encoded",
            query: "alpha=a%20b%26c&beta=b5",
            alpha: "a b&c",
            beta: "b5",
            gamma: "",
            flag: "",
        },
        QueryRow {
            case: "one name sent twice, its first value first",
            query: "alpha=first6&alpha=second6&beta=b6",
            alpha: "first6",
            beta: "b6",
            gamma: "",
            flag: "",
        },
        QueryRow {
            case: "the same name sent twice in the other order",
            query: "alpha=second6&alpha=first6&beta=b7",
            alpha: "second6",
            beta: "b7",
            gamma: "",
            flag: "",
        },
        QueryRow {
            case: "a parameter sent with no value at all, beside one with a value",
            query: "flag&beta=b8",
            alpha: "",
            beta: "b8",
            gamma: "",
            flag: "",
        },
    ]
}

/// RULE: a behaviour that forwards the query string hands the origin that request's own
/// parameters, as the viewer spelled them, whatever their count, order and form; one that
/// forwards none hands it none of them.
#[tokio::test]
async fn the_origin_is_asked_for_the_query_string_the_behaviour_forwards() {
    let router = emulator();
    let api = deployed_reflecting_api(&router).await;

    // The three forwardings the rule reads, each a distribution of its own: the legacy member
    // carrying the query string, the same member withholding it, and the managed policy pair
    // that carries it. `forwards` says whether the origin has to see the row's parameters.
    let configurations: &[(&str, Forwarding, bool)] = &[
        (
            "a legacy ForwardedValues that forwards the query string",
            Forwarding::Legacy {
                query_string: true,
                cache_keys: &[],
                headers: &[TAG_HEADER],
                ttl_seconds: 0,
            },
            true,
        ),
        (
            "a legacy ForwardedValues that forwards no query string",
            Forwarding::Legacy {
                query_string: false,
                cache_keys: &[],
                headers: &[TAG_HEADER],
                ttl_seconds: 0,
            },
            false,
        ),
        (
            "a cache policy that caches nothing beside the AllViewer origin request policy",
            Forwarding::Policies,
            true,
        ),
    ];

    for (configuration, (forwarding_case, forwarding, forwards)) in
        configurations.iter().enumerate()
    {
        let (_id, domain) = deployed_distribution(
            &router,
            forwarding_case,
            &format!("forwarded-query-{configuration}"),
            &api,
            forwarding,
        )
        .await;

        for (index, row) in query_rows().iter().enumerate() {
            let case = format!("{forwarding_case}: {}", row.case);
            // A tag of this row's own, so no two requests of one distribution are the same
            // object and every answer below is the one the ORIGIN produced for that request.
            let tag = format!("tag-{index}");
            let answered = view(
                &router,
                "https",
                &domain,
                &format!("{VIEWER_PATH}?{}", row.query),
                &[(TAG_HEADER, &tag)],
            )
            .await;
            assert_eq!(
                answered.header("x-cache"),
                MISS,
                "case {case:?}: expected the answer to be the origin's own for this request, \
                 got X-Cache {:?}",
                answered.header("x-cache")
            );
            let actual = (
                reported(&case, &answered, ALPHA),
                reported(&case, &answered, BETA),
                reported(&case, &answered, GAMMA),
                reported(&case, &answered, FLAG),
            );
            let expected = if *forwards {
                (
                    row.alpha.to_string(),
                    row.beta.to_string(),
                    row.gamma.to_string(),
                    row.flag.to_string(),
                )
            } else {
                (String::new(), String::new(), String::new(), String::new())
            };
            assert_eq!(
                actual, expected,
                "case {case:?}: the viewer sent {:?}, so the origin had to be asked for \
                 {expected:?}, got {actual:?} in {:?}",
                row.query,
                answered.text()
            );
            // The whitelisted viewer header travels on the same origin request, with this
            // row's own value, whichever forwarding the behaviour states it under.
            let reported_tag = reported(&case, &answered, "tag");
            assert_eq!(
                reported_tag, tag,
                "case {case:?}: expected the origin to have been sent the header value \
                 {tag:?}, got {reported_tag:?}"
            );
        }
    }
}

/// One row of the narrowed-cache-key table: the query string a viewer sends, what the edge has
/// to report the answer as, and the parameters the ORIGIN was told on the fetch that produced
/// it.
struct KeyRow {
    case: &'static str,
    query: &'static str,
    x_cache: &'static str,
    origin_alpha: &'static str,
    origin_beta: &'static str,
}

/// RULE, the half `QueryStringCacheKeys` states: naming cache keys narrows the CACHE KEY
/// alone — "CloudFront forwards all query string parameters to the origin, but it only caches
/// based on the query string parameters that you specify" — so a parameter the key excludes
/// still reaches the origin, and two requests differing only in it are ONE cached object
/// answered by the first one's origin fetch.
#[tokio::test]
async fn named_query_string_cache_keys_narrow_the_key_and_forward_every_parameter() {
    let router = emulator();
    let api = deployed_reflecting_api(&router).await;
    let case = "a legacy ForwardedValues that keys on one parameter and forwards both";
    let (_id, domain) = deployed_distribution(
        &router,
        case,
        "forwarded-query-cache-keys",
        &api,
        &Forwarding::Legacy {
            query_string: true,
            cache_keys: &[ALPHA],
            headers: &[],
            ttl_seconds: CACHED_TTL_SECONDS,
        },
    )
    .await;

    // The rows are made in order: each says what the edge answers the request with, and which
    // request's forwarded values that answer carries.
    let rows: &[KeyRow] = &[
        KeyRow {
            case: "the first request for this object",
            query: "alpha=k1&beta=v1",
            x_cache: MISS,
            origin_alpha: "k1",
            origin_beta: "v1",
        },
        KeyRow {
            case: "a request differing only in the parameter the key excludes",
            query: "alpha=k1&beta=v2",
            x_cache: HIT,
            origin_alpha: "k1",
            origin_beta: "v1",
        },
        KeyRow {
            case: "a request differing in the parameter the key names, which is another object",
            query: "alpha=k2&beta=v3",
            x_cache: MISS,
            origin_alpha: "k2",
            origin_beta: "v3",
        },
        KeyRow {
            case: "that other object again, however the excluded parameter differs",
            query: "alpha=k2&beta=v4",
            x_cache: HIT,
            origin_alpha: "k2",
            origin_beta: "v3",
        },
    ];

    for row in rows {
        let case = format!("{case}: {}", row.case);
        let answered = view(
            &router,
            "https",
            &domain,
            &format!("{VIEWER_PATH}?{}", row.query),
            &[],
        )
        .await;
        assert_eq!(
            answered.header("x-cache"),
            row.x_cache,
            "case {case:?}: expected X-Cache {:?}, got {:?}",
            row.x_cache,
            answered.header("x-cache")
        );
        let actual = (
            reported(&case, &answered, ALPHA),
            reported(&case, &answered, BETA),
        );
        let expected = (
            row.origin_alpha.to_string(),
            row.origin_beta.to_string(),
        );
        assert_eq!(
            actual, expected,
            "case {case:?}: the viewer sent {:?}, so the answer had to carry the origin's \
             {expected:?}, got {actual:?} in {:?}",
            row.query,
            answered.text()
        );
    }
}
