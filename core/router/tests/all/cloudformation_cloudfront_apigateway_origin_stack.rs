// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation stack that fronts one REST API's deployed stage with SEVERAL CloudFront
//! distributions over an API Gateway CUSTOM origin — the shape an edge-in-front-of-a-REST-API
//! template takes — deploys, and the edge of each distribution relays what that origin answers
//! the path it reached, a refusal included.
//!
//! Two rules are pinned here, and neither is observable in one crate alone:
//!
//! 1. **An `AWS::CloudFront::Distribution` deploys from the declaration ALONE: every member
//!    CloudFront's API demands of the configuration document that the resource type publishes
//!    as an optional property is supplied by the resource HANDLER, at the value the resource
//!    behaves by when a template names none.** A custom origin's `HTTPPort` and `HTTPSPort`
//!    are `required: true` on the wire
//!    (`smithy_query(query="shape",shape="CustomOriginConfig")`) while the resource type
//!    documents them as optional with the defaults `80` and `443`, so a template that states
//!    only a protocol policy — or only the HTTPS port, as an origin reached over TLS alone is
//!    written — must still reach `CREATE_COMPLETE`, with the distribution CloudFront holds
//!    carrying both ports. Nothing about the rule is particular to ONE distribution of a
//!    stack: every one the template declares deploys the same way.
//! 2. **A distribution relays the ORIGIN's own answer — its status and its body — to the
//!    viewer, whatever that status is, so what the origin makes of the path the origin request
//!    carried is what the viewer sees.** The path is the viewer's own path appended to the
//!    origin's `OriginPath`, so over one deployed stage the distribution whose origin names
//!    the stage in `OriginPath` is served the method's integration response, and one that
//!    names no `OriginPath` sends the stage-less path, which the api refuses — and that
//!    refusal, not a refusal of CloudFront's own, is what reaches the viewer.
//!
//! Three crates have to agree for both to hold (CloudFormation renders the configuration and
//! makes the creates, CloudFront judges the document, holds it and performs the origin fetch,
//! API Gateway holds the api and answers that fetch), so the suite lives here rather than in
//! any one of them.
//!
//! The rows differ along what the rules generalise over: the stage's name, whether the method
//! sits on the api's ROOT resource or on a resource under it, which ports the custom origin
//! declares (the HTTPS one alone, or neither), which TLS protocols it lists (one, several, or
//! none at all), and the status the integration response maps to — a success and a client
//! error, so the relay is pinned on both sides of the 2xx line.
//!
//! Every expected value is computed here from what the row declares; the ids and domain names
//! are CloudFront's and API Gateway's to mint and come out of the stack's own OUTPUTS, which
//! is what a client deploying the stack reads. What the stage-less path must answer is taken
//! from the ORIGIN itself, dialled directly, so the relay is asserted against the answer being
//! relayed rather than against a transcribed body.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;
use std::sync::Arc;

use super::cloudformation_cloudfront_handler_supplied_config_members::{
    cloudformation, resource_reasons, settled_stack,
};
use super::cloudfront_apigateway_origin_edge::{
    cloudfront, dispatch, view, wait_until_deployed, Answer, API_VERSION, REGION,
};
use super::query_wire::{block_of, opt_element, outputs_of};

/// The `StackStatus` both rules are stated about.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The origin id the single origin of every distribution carries, and the one the behaviour
/// targets.
const ORIGIN_ID: &str = "api-origin";

/// The logical ids the two distributions are declared under: the one whose origin names the
/// stage in its `OriginPath`, and the one that names none.
const FRONTED: &str = "FrontedCdn";
const BARE: &str = "BareCdn";

/// The ports a custom origin is reached on when its declaration names neither — the defaults
/// the resource type documents for `CustomOriginConfig.HTTPPort` and `.HTTPSPort`, which the
/// resource handler is what states.
const DEFAULT_HTTP_PORT: &str = "80";
const DEFAULT_HTTPS_PORT: &str = "443";

/// The emulator as it is ASSEMBLED, on one engine: the api CloudFormation creates is the api
/// the origin fetch reaches, and the distributions it creates are the ones the viewers dial.
///
/// The whole assembly rather than a hand-written list of the three services it exercises, so
/// what is driven is the set `core/router/src/lib.rs` really mounts and no list here can drift
/// from it.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The signature is never checked here, as in every other suite of this crate; the
    // credential scope is what routes a setup call, and a VIEWER request carries no credential
    // at all.
    router.set_request_authentication(false);
    router
}

/// One row of the table: the topology the template declares, what the custom origin states of
/// itself, and what the stage answers.
struct Row {
    case: &'static str,
    /// The stage the deployment is served under, which is also the fronted origin's
    /// `OriginPath`.
    stage: &'static str,
    /// The path part of the resource the method sits on, or `None` for the api's ROOT resource
    /// — the two places a method can be put.
    path_part: Option<&'static str>,
    /// The `HTTPSPort` the custom origin declares, or `None` for a declaration that states
    /// NEITHER port. `HTTPPort` is declared by no row: it is the member the recorded template
    /// leaves out.
    ///
    /// A row that declares one declares the port the endpoint is actually served on, because
    /// every row's edge then FETCHES that origin; that the handler keeps a port a template
    /// states at some other value is pinned, on the stored configuration alone, by
    /// [`super::cloudformation_cloudfront_handler_supplied_config_members`]'s own rows.
    https_port: Option<u16>,
    /// The `OriginSSLProtocols` the custom origin lists, empty for a declaration that lists
    /// none.
    ssl_protocols: &'static [&'static str],
    /// The status the integration response maps to, which is the status the stage and then the
    /// edge answer.
    status: u16,
    /// The document the integration response's `application/json` response template renders,
    /// which is the body both answer.
    body: &'static str,
}

/// The stage-relative path of the row's method: `/` for a method on the root resource.
fn method_path(row: &Row) -> String {
    row.path_part
        .map_or_else(|| "/".to_string(), |part| format!("/{part}"))
}

/// The `HTTPSPort` the row's origin must report: the one it declared, or the documented
/// default when it declared none.
fn expected_https_port(row: &Row) -> String {
    row.https_port
        .map_or_else(|| DEFAULT_HTTPS_PORT.to_string(), |port| port.to_string())
}

/// The template the row deploys: the api, the resource its method may sit under, the method
/// with a `MOCK` integration and the response either side of it, the deployment of them, the
/// stage serving that deployment, and the two distributions over that stage as a custom origin
/// — one naming the stage in its `OriginPath`, one naming none.
fn template(row: &Row, stack: &str) -> String {
    let status = row.status.to_string();
    let (resource_id, mut resources) = match row.path_part {
        Some(part) => (
            json!({ "Ref": "Resource" }),
            json!({
                "Resource": {
                    "Type": "AWS::ApiGateway::Resource",
                    "Properties": {
                        "RestApiId": { "Ref": "Api" },
                        "ParentId": { "Fn::GetAtt": ["Api", "RootResourceId"] },
                        "PathPart": part
                    }
                }
            }),
        ),
        None => (json!({ "Fn::GetAtt": ["Api", "RootResourceId"] }), json!({})),
    };

    let declared = resources.as_object_mut().expect("a JSON object");
    declared.insert(
        "Api".to_string(),
        json!({
            "Type": "AWS::ApiGateway::RestApi",
            "Properties": {
                "Name": format!("{stack}-api"),
                "EndpointConfiguration": { "Types": ["REGIONAL"] }
            }
        }),
    );
    declared.insert(
        "Method".to_string(),
        json!({
            "Type": "AWS::ApiGateway::Method",
            "Properties": {
                "RestApiId": { "Ref": "Api" },
                "ResourceId": resource_id,
                "HttpMethod": "GET",
                "AuthorizationType": "NONE",
                "MethodResponses": [{ "StatusCode": status }],
                "Integration": {
                    "Type": "MOCK",
                    // A MOCK has no backend: the `statusCode` its rendered request template
                    // names is the integration response the call maps to.
                    "RequestTemplates": {
                        "application/json": format!(r#"{{"statusCode":{status}}}"#)
                    },
                    "IntegrationResponses": [{
                        "StatusCode": status,
                        "ResponseTemplates": { "application/json": row.body }
                    }]
                }
            }
        }),
    );
    declared.insert(
        "Deployment".to_string(),
        json!({
            "Type": "AWS::ApiGateway::Deployment",
            "DependsOn": ["Method"],
            "Properties": { "RestApiId": { "Ref": "Api" } }
        }),
    );
    declared.insert(
        "Stage".to_string(),
        json!({
            "Type": "AWS::ApiGateway::Stage",
            "Properties": {
                "RestApiId": { "Ref": "Api" },
                "DeploymentId": { "Ref": "Deployment" },
                "StageName": row.stage
            }
        }),
    );
    for (logical_id, origin_path) in [(FRONTED, Some(format!("/{}", row.stage))), (BARE, None)] {
        declared.insert(
            logical_id.to_string(),
            json!({
                "Type": "AWS::CloudFront::Distribution",
                "DependsOn": ["Stage"],
                "Properties": {
                    "DistributionConfig": {
                        "Enabled": true,
                        "Comment": format!("{logical_id} over the api stage"),
                        "Origins": [origin(row, origin_path)],
                        "DefaultCacheBehavior": {
                            "TargetOriginId": ORIGIN_ID,
                            "ViewerProtocolPolicy": "allow-all",
                            // A behaviour that names no cache policy keys on its own legacy
                            // ForwardedValues, and a zero MinTTL sends every viewer request on
                            // to the origin.
                            "ForwardedValues": {
                                "QueryString": false,
                                "Cookies": { "Forward": "none" }
                            },
                            "MinTTL": 0
                        }
                    }
                }
            }),
        );
    }

    let mut outputs = json!({
        "ApiId": { "Value": { "Ref": "Api" } }
    });
    let published = outputs.as_object_mut().expect("a JSON object");
    for logical_id in [FRONTED, BARE] {
        published.insert(
            format!("{logical_id}Id"),
            json!({ "Value": { "Fn::GetAtt": [logical_id, "Id"] } }),
        );
        published.insert(
            format!("{logical_id}Domain"),
            json!({ "Value": { "Fn::GetAtt": [logical_id, "DomainName"] } }),
        );
    }

    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": resources,
        "Outputs": outputs
    })
    .to_string()
}

/// The one origin a distribution of the row declares: the api's own execute-api endpoint as a
/// CUSTOM origin, stating the ports and the TLS protocols the row names and never `HTTPPort`.
fn origin(row: &Row, origin_path: Option<String>) -> Value {
    let mut custom = json!({
        // Every AWS endpoint of this emulator is served over TLS alone, which is the scheme
        // the origin fetch has to use.
        "OriginProtocolPolicy": "https-only"
    });
    let members = custom.as_object_mut().expect("a JSON object");
    if let Some(port) = row.https_port {
        members.insert("HTTPSPort".to_string(), json!(port));
    }
    if !row.ssl_protocols.is_empty() {
        members.insert("OriginSSLProtocols".to_string(), json!(row.ssl_protocols));
    }
    let mut origin = json!({
        "Id": ORIGIN_ID,
        "DomainName": { "Fn::Sub": "${Api}.execute-api.${AWS::Region}.amazonaws.com" },
        "CustomOriginConfig": custom
    });
    if let Some(path) = origin_path {
        origin["OriginPath"] = json!(path);
    }
    origin
}

const ROWS: &[Row] = &[
    Row {
        case: "a method on the api's ROOT resource, the origin stating only the HTTPS port and \
               one TLS protocol",
        stage: "prod",
        path_part: None,
        https_port: Some(443),
        ssl_protocols: &["TLSv1.2"],
        status: 200,
        body: r#"{"edge":"through-the-origin-path"}"#,
    },
    Row {
        case: "a method on a resource under the root, the origin stating NEITHER port and no \
               TLS protocol",
        stage: "gamma",
        path_part: Some("health"),
        https_port: None,
        ssl_protocols: &[],
        status: 202,
        body: r#"{"accepted":true,"checks":2}"#,
    },
    Row {
        case: "an integration response mapped to a CLIENT ERROR, the origin stating two TLS \
               protocols",
        stage: "v1",
        path_part: Some("items"),
        https_port: Some(443),
        ssl_protocols: &["TLSv1.1", "TLSv1.2"],
        status: 404,
        body: r#"{"message":"no such item"}"#,
    },
];

#[tokio::test]
async fn a_stack_of_api_gateway_fronting_distributions_deploys_and_each_edge_relays_its_origin() {
    let router = build_router();

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let stack = format!("cloudfront-api-origin-{index}");
        let body = template(row, &stack);

        let (status, answered) = cloudformation(
            &router,
            "CreateStack",
            &[("StackName", &stack), ("TemplateBody", &body)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {answered}"
        );

        let described = settled_stack(&router, &stack, case).await;
        let reported = opt_element(&described, "StackStatus").unwrap_or_default();
        assert_eq!(
            reported,
            CREATE_COMPLETE,
            "case {case:?}: expected the stack to reach {CREATE_COMPLETE}, got {reported:?} \
             \u{2014} {}",
            resource_reasons(&router, &stack).await
        );

        let outputs = outputs_of(&described);
        let published = |name: &str| {
            outputs.get(name).cloned().unwrap_or_else(|| {
                panic!("case {case:?}: the stack published no {name} output in {described}")
            })
        };
        let api_id = published("ApiId");

        // RULE 1: each declared distribution is one CloudFront HOLDS, and the configuration it
        // holds carries both of the ports the API demands — the declared HTTPS one, and the
        // HTTP one no row declares at all.
        for logical_id in [FRONTED, BARE] {
            let id = published(&format!("{logical_id}Id"));
            let config = held_configuration(&router, case, logical_id, &id).await;
            let origins = block_of(&config, "Origins").unwrap_or_default();
            let custom = block_of(origins, "CustomOriginConfig").unwrap_or_else(|| {
                panic!("case {case:?}: {logical_id} reports no CustomOriginConfig in {origins}")
            });
            for (member, expected) in [
                ("HTTPPort", DEFAULT_HTTP_PORT.to_string()),
                ("HTTPSPort", expected_https_port(row)),
            ] {
                let held = opt_element(custom, member);
                assert_eq!(
                    held.as_deref(),
                    Some(expected.as_str()),
                    "case {case:?}: expected {logical_id}'s origin to report {member} \
                     {expected:?}, got {held:?} in {custom}"
                );
            }
            // A declaration naming no `OriginPath` is one CloudFront resolves to the EMPTY path
            // (`resolve_origin`, in that crate's `state/configuration.rs`), so the element is
            // there either way and the reading is exact rather than defaulted: an absent member
            // is a different answer from an empty one and fails here.
            let expected_origin_path = if logical_id == FRONTED {
                format!("/{}", row.stage)
            } else {
                String::new()
            };
            let held = opt_element(origins, "OriginPath");
            assert_eq!(
                held.as_deref(),
                Some(expected_origin_path.as_str()),
                "case {case:?}: expected {logical_id}'s origin to report OriginPath \
                 {expected_origin_path:?}, got {held:?} in {origins}"
            );
        }

        // What the ORIGIN itself answers each of the two paths an origin request can carry:
        // the stage-prefixed one the fronted origin sends, and the stage-LESS one the bare
        // origin sends. These are the answers the two edges must relay.
        let origin_host = format!("{api_id}.execute-api.{REGION}.amazonaws.com");
        let path = method_path(row);
        let with_stage = if path == "/" {
            format!("/{}", row.stage)
        } else {
            format!("/{}{path}", row.stage)
        };
        let served = origin_answer(&router, &origin_host, &with_stage).await;
        let expected: Value = serde_json::from_str(row.body).expect("the row's own document");
        assert_eq!(
            (served.status, served.json()),
            (row.status, expected.clone()),
            "case {case:?}: expected the deployed stage at {with_stage} to answer HTTP {} with \
             {expected}, got {} with {:?}",
            row.status,
            served.status,
            served.text()
        );
        let refused = origin_answer(&router, &origin_host, &path).await;
        assert_eq!(
            refused.status, 403,
            "case {case:?}: expected the api to refuse the stage-less path {path} with HTTP \
             403, got {} with {:?}",
            refused.status,
            refused.text()
        );

        // RULE 2: each edge relays the answer its own origin request got, status and body.
        for (logical_id, relayed) in [(FRONTED, &served), (BARE, &refused)] {
            let id = published(&format!("{logical_id}Id"));
            let domain = published(&format!("{logical_id}Domain"));
            wait_until_deployed(&router, case, &id).await;
            let viewed = view(&router, "https", &domain, &path, &[]).await;
            assert_eq!(
                (viewed.status, viewed.text()),
                (relayed.status, relayed.text()),
                "case {case:?}: expected {logical_id} at https://{domain}{path} to relay its \
                 origin's HTTP {} with body {:?}, got {} with {:?}",
                relayed.status,
                relayed.text(),
                viewed.status,
                viewed.text()
            );
        }
    }
}

/// The `DistributionConfig` CloudFront holds for `id`, read back through that service's own
/// `GetDistribution` — the owning service's record, which is the only evidence a stack really
/// created the distribution it published an id for.
async fn held_configuration(
    router: &MultiServiceRouter,
    case: &str,
    logical_id: &str,
    id: &str,
) -> String {
    let read = cloudfront(
        router,
        "GET",
        &format!("/{API_VERSION}/distribution/{id}"),
        ::bytes::Bytes::new(),
    )
    .await;
    assert_eq!(
        read.status,
        200,
        "case {case:?}: expected HTTP 200 from GetDistribution for {logical_id} ({id}), got {} \
         with {:?}",
        read.status,
        read.text()
    );
    block_of(&read.text(), "DistributionConfig")
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: GetDistribution for {logical_id} carries no DistributionConfig: \
                 {:?}",
                read.text()
            )
        })
        .to_string()
}

/// One request to the api's own execute-api endpoint, exactly as the origin fetch makes it:
/// addressed at the api's endpoint host, over HTTPS, carrying no credential — the method the
/// rows declare is `AuthorizationType: NONE`, and a viewer of a distribution sends none.
async fn origin_answer(router: &MultiServiceRouter, host: &str, path: &str) -> Answer {
    dispatch(
        router,
        "GET",
        host,
        &format!("https://{host}{path}"),
        &[],
        ::bytes::Bytes::new(),
    )
    .await
}
