// SPDX-License-Identifier: CC0-1.0

//! WHICH `Host` the ORIGIN of a CloudFront distribution is sent, end to end over HTTP across
//! the two services on one engine.
//!
//! RULE: **a CUSTOM origin is sent the host the VIEWER addressed — the name the viewer reached
//! the distribution by — exactly where the matched cache behaviour's reading of the headers
//! that reach the origin carries `Host`, and the origin's own domain name everywhere else.**
//!
//! That is what makes `Host` worth forwarding at all, and what the header behaviours spell
//! out in both directions: `OriginRequestPolicyHeaderBehavior`'s `allViewer` is "all HTTP
//! headers in viewer requests are included in requests that CloudFront sends to the origin"
//! and `allViewerExceptHostHeader` the same "except for the `Host` header"
//! (`smithy_query(query="shape",shape="OriginRequestPolicyHeaderBehavior")`), while the legacy
//! `ForwardedValues$Headers` whitelist carries the header names a behaviour with no policy
//! forwards — the list naming `*` being every one of them. An origin behind several front
//! doors can then tell which front door a request came through, which is the whole purpose of
//! the forwarded `Host`.
//!
//! Only a cross-service suite can pin it: the behaviour, its policies and the origin are
//! CloudFront's records, and the thing that REPORTS what reached the origin is an origin
//! another service serves — an API Gateway stage whose one method is a `MOCK` integration
//! reflecting its own method request (`$input.params('Host')`), so each row reads the `Host`
//! the origin received off what the viewer was served. The wire harness is
//! [`super::cloudfront_apigateway_origin_edge`]'s, reused rather than repeated:
//! [`super::cloudfront_policy_forwarding_to_the_origin`] pins WHICH parts of a viewer request
//! travel, and this suite the one header whose VALUE the forwarding decides.
//!
//! The rows differ along what the rule generalises over: which reading names the headers that
//! reach the origin (an AWS-managed origin request policy, the legacy `ForwardedValues`
//! whitelist, neither), the header behaviour that reading carries (`allViewer`,
//! `allViewerExceptHostHeader`, a whitelist that names `Host`, one that names another header,
//! the list naming every header), and — in the second test — the ADDRESSING the viewer
//! reached the edge by, since the host forwarded is the one the edge resolved the viewer to
//! rather than a copy of whatever arrived.

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::cloudfront_apigateway_origin_edge::{
    apigw, authorization, cloudfront, dispatch, emulator, member, view, wait_until_deployed,
    Answer, API_VERSION, CLOUDFRONT_HOST, REGION, STAGE,
};

/// The resource of the reflecting api, and the viewer path that reaches it through the edge.
const ECHO_PATH: &str = "echo";
const VIEWER_PATH: &str = "/echo";

/// The header the rows are about, spelled as a viewer spells it.
const HOST_HEADER: &str = "Host";

/// One header that is NOT `Host`, for the row whose whitelist names another header.
const OTHER_HEADER: &str = "X-Sim-Other";

/// The AWS-managed cache policy that caches nothing (`Managed-CachingDisabled`), so every
/// row's request reaches the origin and reports that row's own origin request.
const CACHING_DISABLED: &str = "4135ea2d-6df8-44a3-9df3-4b5a84be39ad";

/// The AWS-managed origin request policies the rows name: `Managed-AllViewer` ("policy to
/// forward all parameters in viewer requests") and `Managed-AllViewerExceptHostHeader` (the
/// same "except for the Host header"), by the ids Amazon publishes for them.
const ALL_VIEWER_POLICY: &str = "216adef6-5c7f-47e4-b989-5492eafa07d3";
const ALL_VIEWER_EXCEPT_HOST_POLICY: &str = "b689b0a8-53d0-40ab-baf2-68738e2966ac";

/// The reserved data-plane path a client that cannot resolve a distribution's own domain name
/// reaches the edge at, ahead of the distribution's id.
const EDGE_PATH_PREFIX: &str = "/_aws/cloudfront";

/// WHICH reading of the headers that reach the origin a row's behaviour carries.
enum Forwarding {
    /// A cache policy that caches nothing, and optionally an origin request policy.
    Policy(Option<&'static str>),
    /// No policy at all: the legacy `ForwardedValues`, whose `Headers` names these.
    Legacy(&'static [&'static str]),
}

/// One row: what the behaviour forwards, and whether the origin must be told the VIEWER's own
/// host rather than the origin's.
struct Row {
    case: &'static str,
    forwarding: Forwarding,
    /// Whether the origin request has to carry the host the viewer addressed.
    forwards_viewer_host: bool,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a legacy Headers whitelist that names Host",
            forwarding: Forwarding::Legacy(&[HOST_HEADER]),
            forwards_viewer_host: true,
        },
        Row {
            case: "a legacy Headers whitelist that names every header",
            forwarding: Forwarding::Legacy(&["*"]),
            forwards_viewer_host: true,
        },
        Row {
            case: "a legacy Headers whitelist that names another header",
            forwarding: Forwarding::Legacy(&[OTHER_HEADER]),
            forwards_viewer_host: false,
        },
        Row {
            case: "a legacy forwarding that names no header at all",
            forwarding: Forwarding::Legacy(&[]),
            forwards_viewer_host: false,
        },
        Row {
            case: "the AllViewer origin request policy",
            forwarding: Forwarding::Policy(Some(ALL_VIEWER_POLICY)),
            forwards_viewer_host: true,
        },
        Row {
            case: "the AllViewerExceptHostHeader origin request policy",
            forwarding: Forwarding::Policy(Some(ALL_VIEWER_EXCEPT_HOST_POLICY)),
            forwards_viewer_host: false,
        },
        Row {
            case: "a cache policy alone, with no origin request policy",
            forwarding: Forwarding::Policy(None),
            forwards_viewer_host: false,
        },
    ]
}

/// The `DefaultCacheBehavior` members a row's forwarding contributes, in the order the model
/// declares them (`smithy_query(query="shape",shape="DefaultCacheBehavior")`): the policy ids
/// first, then the legacy `ForwardedValues` and its `MinTTL`.
fn behavior_members(forwarding: &Forwarding) -> String {
    match forwarding {
        Forwarding::Policy(origin_request_policy) => {
            let mut members = format!("<CachePolicyId>{CACHING_DISABLED}</CachePolicyId>");
            if let Some(policy) = origin_request_policy {
                members.push_str(&format!(
                    "<OriginRequestPolicyId>{policy}</OriginRequestPolicyId>"
                ));
            }
            members
        }
        Forwarding::Legacy(headers) => {
            let mut members = String::from(
                "<ForwardedValues><QueryString>false</QueryString>\
                 <Cookies><Forward>none</Forward></Cookies>",
            );
            if !headers.is_empty() {
                let items: String = headers
                    .iter()
                    .map(|name| format!("<Name>{name}</Name>"))
                    .collect();
                members.push_str(&format!(
                    "<Headers><Quantity>{}</Quantity><Items>{items}</Items></Headers>",
                    headers.len()
                ));
            }
            members.push_str("</ForwardedValues><MinTTL>0</MinTTL>");
            members
        }
    }
}

/// A REST api with one `GET /echo` method whose `MOCK` integration REFLECTS the `Host` its
/// method request arrived with, deployed to [`STAGE`]. Answers the api's id.
async fn deployed_reflecting_api(router: &MultiServiceRouter) -> String {
    let created = apigw(
        router,
        "CreateRestApi",
        "POST",
        "/restapis",
        Some(json!({ "name": "cloudfront-forwarded-host-echo" })),
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
    let template = format!(r#"{{"host":"$input.params('{HOST_HEADER}')"}}"#);
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

/// The host the api stage is reached at, which is the origin's own `DomainName`.
fn origin_host(api: &str) -> String {
    format!("{api}.execute-api.{REGION}.amazonaws.com")
}

/// `cloudfront:CreateDistribution` over the api stage `api` as a CUSTOM origin, under the
/// forwarding `forwarding` names, answering the distribution's `(id, DomainName)`.
async fn create_distribution(
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
         <DomainName>{}</DomainName>\
         <OriginPath>/{STAGE}</OriginPath>\
         <CustomOriginConfig><HTTPPort>80</HTTPPort><HTTPSPort>443</HTTPSPort>\
         <OriginProtocolPolicy>https-only</OriginProtocolPolicy>\
         </CustomOriginConfig>\
         </Origin></Items></Origins>\
         <DefaultCacheBehavior><TargetOriginId>api-origin</TargetOriginId>\
         <ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy>\
         {members}\
         </DefaultCacheBehavior>\
         <Comment>which host the origin request carries</Comment>\
         <Enabled>true</Enabled>\
         </DistributionConfig>",
        origin_host(api)
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

/// The `host` the origin reported for one answer the edge served, and the answer itself for a
/// failure message.
fn reported_host(case: &str, answered: &Answer) -> String {
    assert_eq!(
        answered.status,
        200,
        "case {case:?}: expected the edge to serve the origin's answer, got {} with {:?}",
        answered.status,
        answered.text()
    );
    answered
        .json()
        .get("host")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: the origin reported no host, got {:?}",
                answered.text()
            )
        })
        .to_string()
}

/// RULE: the origin request carries the host the VIEWER addressed exactly where the matched
/// behaviour forwards the viewer's `Host`, and the origin's own domain name otherwise.
#[tokio::test]
async fn the_origin_is_sent_the_viewers_host_exactly_where_the_behaviour_forwards_it() {
    let router = emulator();
    let api = deployed_reflecting_api(&router).await;
    let origin = origin_host(&api);

    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        let (id, domain) = create_distribution(
            &router,
            case,
            &format!("forwarded-host-{index}"),
            &api,
            &row.forwarding,
        )
        .await;
        wait_until_deployed(&router, case, &id).await;
        // The viewer addresses the distribution by the name it publishes, and sends one
        // header of its own so a whitelist that names another header still forwards
        // something.
        let answered = view(
            &router,
            "https",
            &domain,
            VIEWER_PATH,
            &[(OTHER_HEADER, "other-1")],
        )
        .await;
        let reported = reported_host(case, &answered);
        let expected = if row.forwards_viewer_host {
            domain.clone()
        } else {
            origin.clone()
        };
        assert_eq!(
            reported, expected,
            "case {case:?}: expected the origin to have been sent the host {expected:?}, got \
             {reported:?}"
        );
    }
}

/// RULE, second half: the host forwarded is the one the EDGE resolved the viewer to — the
/// name the distribution publishes for a viewer that reached the edge through the emulator's
/// own endpoint, where the host the request carried names no distribution at all.
#[tokio::test]
async fn the_forwarded_host_is_the_name_the_edge_resolved_the_viewer_to() {
    let router = emulator();
    let api = deployed_reflecting_api(&router).await;
    let case = "a viewer that reached the edge through the reserved data-plane path";
    let (id, domain) = create_distribution(
        &router,
        case,
        "forwarded-host-prefixed",
        &api,
        &Forwarding::Policy(Some(ALL_VIEWER_POLICY)),
    )
    .await;
    wait_until_deployed(&router, case, &id).await;
    let answered = dispatch(
        &router,
        "GET",
        CLOUDFRONT_HOST,
        &format!("https://{CLOUDFRONT_HOST}{EDGE_PATH_PREFIX}/{id}{VIEWER_PATH}"),
        &[("authorization", &authorization("cloudfront"))],
        Bytes::new(),
    )
    .await;
    let reported = reported_host(case, &answered);
    assert_eq!(
        reported, domain,
        "case {case:?}: expected the origin to have been sent the name the distribution \
         publishes, {domain:?}, got {reported:?}"
    );
}
