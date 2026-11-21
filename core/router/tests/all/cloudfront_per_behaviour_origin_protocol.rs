// SPDX-License-Identifier: CC0-1.0

//! ONE CloudFront distribution with SEVERAL custom origins over the same API Gateway REST
//! stage, end to end across the two services on one engine.
//!
//! RULE: the cache behaviour selected for a viewer request names the ONE origin the edge
//! fetches — `CacheBehavior$TargetOriginId` is "the value of `ID` for the origin that you
//! want CloudFront to route requests to when they match this cache behavior", and the
//! default behaviour's is what applies when "the request for an object does not match the
//! path pattern for any cache behaviors" — and the fetch is made over the scheme THAT
//! origin's own `CustomOriginConfig$OriginProtocolPolicy` names. So within one
//! distribution an `https-only` origin relays its answer to the viewer while an
//! `http-only` origin addressed at an endpoint that answers TLS alone is an origin
//! connection CloudFront cannot complete, which the viewer is answered `502`/`504` for and
//! none of the origin's body: a downgraded origin fails closed instead of quietly serving
//! what its sibling serves.
//!
//! Only a cross-service suite can pin it: the origins and the behaviours are CloudFront's
//! records, and the thing that answers a fetch the edge does complete is API Gateway's
//! deployed stage. `cloudfront_apigateway_origin_edge` beside it pins the protocol policy
//! one origin at a time, with the policy being the only thing that differs between
//! distributions; this one pins the per-BEHAVIOUR selection, where the very same
//! distribution holds both policies at once and the viewer's path is what decides which of
//! them governs the fetch.
//!
//! The rows span what the rule generalises over: whether the DEFAULT behaviour or an
//! ORDERED one matched, which origin the matched behaviour names, and which protocol policy
//! that origin carries — every combination of the two, over paths the stage itself serves,
//! so a leg that is not served is the origin connection failing and nothing else.
//!
//! WHICH behaviour a viewer path matches is the other half of the same rule, and the forms
//! `CacheBehavior$PathPattern` is written in are spanned by
//! [`super::cloudfront_apigateway_behavior_origin_protocol`] beside this suite, which reads
//! them through this one's harness: its origins, its distribution builder and its reader of an
//! answer, raised to the crate below for that reason.

use std::collections::HashMap;

use bytes::Bytes;
use serde_json::{json, Value};

use super::cloudfront_apigateway_origin_edge::{
    apigw, cloudfront, emulator, member, view, wait_until_deployed, Answer, API_VERSION, REGION,
    STAGE,
};

/// The AWS-managed cache policy `Managed-CachingDisabled`, whose TTLs are all zero: every
/// viewer request under it reaches the origin, so each row's answer is the origin's own
/// rather than an entry a previous row left in the edge cache.
const CACHING_DISABLED: &str = "4135ea2d-6df8-44a3-9df3-4b5a84be39ad";

/// The id of the origin the distribution addresses over TLS, and of the one it addresses
/// over plaintext. Both name the SAME stage, so the only thing that differs between the two
/// legs is the protocol policy the matched behaviour's origin carries.
pub(crate) const SECURE_ORIGIN: &str = "stage-over-tls";
pub(crate) const PLAINTEXT_ORIGIN: &str = "stage-over-plaintext";

/// The resource paths the stage serves, each answering a document that names the path it was
/// served for.
pub(crate) const PROBE: &str = "probe";
pub(crate) const UNDER_BROKEN: &str = "broken/probe";
pub(crate) const UNDER_MIRROR: &str = "mirror/probe";

/// The member of the stage's document that names which resource answered.
const LABEL: &str = "label";

/// What the viewer has to be answered for one row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Expected {
    /// The origin's own answer, carrying the document the named resource serves.
    Served(&'static str),
    /// No origin response exists, which CloudFront relays as a bad gateway or a gateway
    /// timeout.
    OriginUnreachable,
}

/// One row: which distribution the viewer dials, the path it asks for, and what it is
/// answered.
struct Row {
    case: &'static str,
    /// Whether the distribution's DEFAULT behaviour names the TLS origin (the other one
    /// names the plaintext origin).
    secure_default: bool,
    path: &'static str,
    expected: Expected,
}

const ROWS: &[Row] = &[
    Row {
        case: "the default behaviour, naming the https-only origin",
        secure_default: true,
        path: "/probe",
        expected: Expected::Served(PROBE),
    },
    Row {
        case: "an ordered behaviour naming the http-only origin",
        secure_default: true,
        path: "/broken/probe",
        expected: Expected::OriginUnreachable,
    },
    Row {
        case: "an ordered behaviour naming the https-only origin",
        secure_default: true,
        path: "/mirror/probe",
        expected: Expected::Served(UNDER_MIRROR),
    },
    Row {
        case: "the default behaviour, naming the http-only origin",
        secure_default: false,
        path: "/probe",
        expected: Expected::OriginUnreachable,
    },
    Row {
        case: "an ordered behaviour naming the https-only origin, under a plaintext default",
        secure_default: false,
        path: "/mirror/probe",
        expected: Expected::Served(UNDER_MIRROR),
    },
    Row {
        case: "an ordered behaviour naming the http-only origin, under a plaintext default",
        secure_default: false,
        path: "/broken/probe",
        expected: Expected::OriginUnreachable,
    },
];

/// A REST api whose every path in `paths` is a `GET` with a `MOCK` integration answering a
/// document that NAMES that path, deployed to [`STAGE`]. Answers the api's id.
///
/// Every resource is created before the deployment, so the one stage serves all of them: a
/// path a row's origin fetch cannot reach is therefore a path the stage itself would have
/// served.
pub(crate) async fn deployed_api_serving(
    router: &smithy_server_runtime::MultiServiceRouter,
    paths: &[&str],
) -> String {
    let created = apigw(
        router,
        "CreateRestApi",
        "POST",
        "/restapis",
        Some(json!({ "name": "cloudfront-per-behaviour-origin" })),
        201,
    )
    .await;
    let api = member("CreateRestApi", &created, "id");
    let root = member("CreateRestApi", &created, "rootResourceId");

    // The resource tree, walked one segment at a time so a shared prefix is created once.
    let mut resources: HashMap<String, String> = HashMap::new();
    for path in paths {
        let mut parent = root.clone();
        let mut walked = String::new();
        for segment in path.split('/') {
            walked = if walked.is_empty() {
                segment.to_string()
            } else {
                format!("{walked}/{segment}")
            };
            parent = match resources.get(&walked) {
                Some(id) => id.clone(),
                None => {
                    let resource = apigw(
                        router,
                        "CreateResource",
                        "POST",
                        &format!("/restapis/{api}/resources/{parent}"),
                        Some(json!({ "pathPart": segment })),
                        201,
                    )
                    .await;
                    let id = member("CreateResource", &resource, "id");
                    resources.insert(walked.clone(), id.clone());
                    id
                }
            };
        }
        let methods = format!("/restapis/{api}/resources/{parent}/methods/GET");
        apigw(
            router,
            "PutMethod",
            "PUT",
            &methods,
            Some(json!({ "authorizationType": "NONE" })),
            201,
        )
        .await;
        apigw(
            router,
            "PutIntegration",
            "PUT",
            &format!("{methods}/integration"),
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
            &format!("{methods}/responses/200"),
            Some(json!({})),
            201,
        )
        .await;
        apigw(
            router,
            "PutIntegrationResponse",
            "PUT",
            &format!("{methods}/integration/responses/200"),
            Some(json!({
                "statusCode": "200",
                "responseTemplates": {
                    "application/json": format!(r#"{{"{LABEL}":"{path}"}}"#),
                },
            })),
            201,
        )
        .await;
    }
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

/// One `<Origin>` over the api stage `api`, addressed over the protocol `policy` names.
fn origin(id: &str, api: &str, policy: &str) -> String {
    format!(
        "<Origin><Id>{id}</Id>\
         <DomainName>{api}.execute-api.{REGION}.amazonaws.com</DomainName>\
         <OriginPath>/{STAGE}</OriginPath>\
         <CustomOriginConfig><HTTPPort>80</HTTPPort><HTTPSPort>443</HTTPSPort>\
         <OriginProtocolPolicy>{policy}</OriginProtocolPolicy>\
         </CustomOriginConfig></Origin>"
    )
}

/// `cloudfront:CreateDistribution` over BOTH origins of the api stage `api`, with the
/// default behaviour naming `default_origin` and one ordered behaviour per
/// `(PathPattern, TargetOriginId)` row of `behaviors`. Answers the distribution's
/// `(id, DomainName)`.
pub(crate) async fn create_distribution(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    caller_reference: &str,
    api: &str,
    default_origin: &str,
    behaviors: &[(&str, &str)],
) -> (String, String) {
    let ordered: String = behaviors
        .iter()
        .map(|(pattern, target)| {
            format!(
                "<CacheBehavior><PathPattern>{pattern}</PathPattern>\
                 <TargetOriginId>{target}</TargetOriginId>\
                 <ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy>\
                 <CachePolicyId>{CACHING_DISABLED}</CachePolicyId>\
                 </CacheBehavior>"
            )
        })
        .collect();
    let body = format!(
        "<DistributionConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <CallerReference>{caller_reference}</CallerReference>\
         <Origins><Quantity>2</Quantity><Items>{}{}</Items></Origins>\
         <DefaultCacheBehavior><TargetOriginId>{default_origin}</TargetOriginId>\
         <ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy>\
         <CachePolicyId>{CACHING_DISABLED}</CachePolicyId>\
         </DefaultCacheBehavior>\
         <CacheBehaviors><Quantity>{}</Quantity><Items>{ordered}</Items></CacheBehaviors>\
         <Comment>one api gateway stage behind two custom origins</Comment>\
         <Enabled>true</Enabled>\
         </DistributionConfig>",
        origin(SECURE_ORIGIN, api, "https-only"),
        origin(PLAINTEXT_ORIGIN, api, "http-only"),
        behaviors.len(),
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
        "case {case:?}: CreateDistribution must succeed, got {} with {:?}",
        created.status,
        created.text()
    );
    let (id, domain) = (created.element("Id"), created.element("DomainName"));
    assert!(
        !id.is_empty() && !domain.is_empty(),
        "case {case:?}: the created distribution reports no Id or DomainName: {:?}",
        created.text()
    );
    (id, domain)
}

/// The document the stage answered, as the label it carries.
fn label(answer: &Answer) -> String {
    answer
        .json()
        .get(LABEL)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// Dial one viewer path of the distribution published at `domain` and read the answer against
/// what `expected` says. One reader, because every row of this suite and of
/// [`super::cloudfront_apigateway_behavior_origin_protocol`] asks the same question of an
/// answer.
pub(crate) async fn check_leg(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    domain: &str,
    path: &str,
    expected: Expected,
) {
    let answer = view(router, "https", domain, path, &[]).await;
    match expected {
        Expected::Served(served) => {
            assert_eq!(
                answer.status, 200,
                "case {case:?}: expected the stage's answer through the edge, got {} with {:?}",
                answer.status,
                answer.text()
            );
            assert_eq!(
                label(&answer),
                served,
                "case {case:?}: expected the document {served:?} serves, got {:?}",
                answer.text()
            );
            assert_eq!(
                answer.header("x-cache"),
                "Miss from cloudfront",
                "case {case:?}: expected the answer to be reported as the origin's, got \
                 X-Cache {:?}",
                answer.header("x-cache")
            );
        }
        Expected::OriginUnreachable => {
            assert!(
                matches!(answer.status, 502 | 504),
                "case {case:?}: expected a bad gateway or a gateway timeout for an origin \
                 connection CloudFront cannot complete, got {} with {:?}",
                answer.status,
                answer.text()
            );
            assert!(
                label(&answer).is_empty() && !answer.text().contains(LABEL),
                "case {case:?}: an origin CloudFront could not reach must leak none of the \
                 stage's document, got {:?}",
                answer.text()
            );
            assert_eq!(
                answer.header("x-cache"),
                "Error from cloudfront",
                "case {case:?}: expected the answer to be reported as CloudFront's own, got \
                 X-Cache {:?}",
                answer.header("x-cache")
            );
        }
    }
}

#[tokio::test]
async fn the_matched_behaviour_names_the_origin_whose_protocol_policy_governs_the_fetch() {
    let router = emulator();
    let api = deployed_api_serving(&router, &[PROBE, UNDER_BROKEN, UNDER_MIRROR]).await;

    // Two distributions over the same pair of origins, differing only in which origin their
    // DEFAULT behaviour names — so the default behaviour is exercised over both protocol
    // policies, and the ordered behaviours over both as well.
    let secure_default = create_distribution(
        &router,
        "a distribution whose default behaviour names the https-only origin",
        "per-behaviour-origin-secure-default",
        &api,
        SECURE_ORIGIN,
        &[
            ("broken/*", PLAINTEXT_ORIGIN),
            ("mirror/*", SECURE_ORIGIN),
        ],
    )
    .await;
    let plaintext_default = create_distribution(
        &router,
        "a distribution whose default behaviour names the http-only origin",
        "per-behaviour-origin-plaintext-default",
        &api,
        PLAINTEXT_ORIGIN,
        &[
            ("broken/*", PLAINTEXT_ORIGIN),
            ("mirror/*", SECURE_ORIGIN),
        ],
    )
    .await;
    for (id, _domain) in [&secure_default, &plaintext_default] {
        wait_until_deployed(&router, "the distributions this suite serves from", id).await;
    }

    for row in ROWS {
        let case = row.case;
        let (_id, domain) = if row.secure_default {
            &secure_default
        } else {
            &plaintext_default
        };
        check_leg(&router, case, domain, row.path, row.expected).await;
    }
}
