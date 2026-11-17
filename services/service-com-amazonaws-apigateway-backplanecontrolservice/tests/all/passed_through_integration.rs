// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for an integration that PASSES THE REQUEST THROUGH: which calls the
//! `execute-api` data plane performs by serving the request onward, and which it refuses.
//!
//! The rule under test: an integration naming an HTTP ENDPOINT — the two types
//! `IntegrationType` gives for one, `HTTP_PROXY` "for integrating with the HTTP proxy
//! integration" and `HTTP` "for integrating an API method with an HTTP backend" — whose `uri`
//! names an `execute-api` INVOKE ENDPOINT, which is an endpoint this emulator serves itself,
//! is performed by carrying its integration request to that endpoint in process. A PROXY
//! integration maps nothing in either direction: the caller's own request is re-addressed
//! there and the status, the body and the headers the caller sees are the ones that endpoint
//! computed, whatever they are. The endpoint's own records decide them — which api (the
//! `uri`'s host, or the `x-apigw-api-id` header the integration maps onto the request), which
//! stage and resource, and that api's resource policy — so a front api in front of a private
//! one is gated on the interface endpoint its `uri` names. A `uri` naming anything else, a
//! mapping outside the forms `requestParameters` documents, and a private connection that
//! carries no traffic are each answered through the api's `API_CONFIGURATION_ERROR` gateway
//! response with no request carried at all.
//!
//! The STANDARD type's own half of that rule — the backend answer its integration responses
//! MAP, and the integration request its records build — is the second test here.
//!
//! The rows span what the rule generalises over: the integration type, the form of the
//! `uri`'s host (an api's own endpoint, an interface endpoint's own DNS name, a host outside
//! this emulator), what the onward request resolves to at that endpoint (a served method, a
//! path it does not serve, an api it does not hold), the two forms a mapped request parameter
//! takes (a static value and a method request parameter) and whether the request carries the
//! source it names, what of the caller's own request travels onward (its query string, under
//! two parameter names) and what does not (its SigV4 credential, in both the header and the
//! presigned-query form), the private connection the integration is carried over (none, one
//! this account does not hold, one still provisioning, one provisioned) and a chain that
//! leads back to itself — through the stored `uri`, through the address a mapped path
//! parameter expands it to, and through the `host` a mapped header sets.

use serde_json::{json, Value};

use crate::execute_api_data_plane::{
    deploy, integration_response, json_template, method_response, mock_resource, put_integration,
    put_method,
};
use crate::rest_api_lifecycle::{create_api, create_resource, string_member, Gateway};

/// The Region every host in this suite is addressed in, which is the one the harness's own
/// control-plane calls are signed for.
const REGION: &str = "us-east-1";

/// The stage each front api under test is deployed to.
const FRONT_STAGE: &str = "f";

/// The stage the backend api is deployed to.
const BACKEND_STAGE: &str = "back";

/// The resource the backend api serves.
const BACKEND_PATH: &str = "echo";

/// The one value the backend's `MOCK` integration answers with: what proves a request was
/// passed through to it, so a refusal must never carry it.
const NONCE: &str = "3c81fa";

/// The body the backend answers, as its response template writes it.
fn backend_body() -> String {
    format!("{{\"m\":\"{NONCE}\"}}")
}

/// An interface VPC endpoint id, in EC2's own form (`vpce-` and 17 lowercase hexadecimal
/// digits): the endpoint whose own DNS name a front api addresses the backend by.
const ENDPOINT: &str = "vpce-0123456789abcdef0";

/// A network load balancer of this account, as Elastic Load Balancing writes its ARN
/// (`loadbalancer/net/{name}/{id}`), which is what a VPC link is provisioned against.
const LOAD_BALANCER_ARN: &str =
    "arn:aws:elasticloadbalancing:us-east-1:000000000000:loadbalancer/net/passed-through/0a1b2c3d";

/// Long enough past `state::VPC_LINK_PROVISION_SPAN` that a created link has certainly
/// settled, with a margin for a slow machine.
const SETTLE_WAIT: std::time::Duration = std::time::Duration::from_millis(900);

/// A SigV4 credential scope, as a signed SDK request carries it. The signature itself is
/// the emulator's business and is not verified here (the harness runs the router with
/// request authentication off), so only its PRESENCE is what a case varies.
const SIGNED: &str =
    "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/us-east-1/execute-api/aws4_request, \
     SignedHeaders=host, Signature=0123456789abcdef";

/// The api's own invoke host, which names the api.
fn api_host(api: &str) -> String {
    format!("{api}.execute-api.{REGION}.amazonaws.com")
}

/// The endpoint-specific DNS name EC2 reports in an interface endpoint's `DnsEntries`: it
/// names the ENDPOINT rather than an api, so an api reached on it travels in a header.
fn endpoint_host(endpoint: &str) -> String {
    format!("{endpoint}.execute-api.{REGION}.vpce.amazonaws.com")
}

/// The backend api: one deployed `MOCK` method that answers [`backend_body`], as its id.
async fn backend(gateway: &Gateway) -> String {
    let (api, root) = create_api(gateway, "passed-through-backend").await;
    mock_resource(
        gateway,
        &api,
        &root,
        BACKEND_PATH,
        "GET",
        "200",
        json_template(&backend_body()),
    )
    .await;
    deploy(gateway, &api, BACKEND_STAGE).await;
    api
}

/// The `MOCK` integration every backend method here answers through: the integration, the
/// method response the status is declared by and the integration response that maps the
/// call to [`backend_body`].
async fn mock_integration(gateway: &Gateway, api: &str, resource: &str) {
    put_integration(
        gateway,
        api,
        resource,
        "GET",
        json!({
            "type": "MOCK",
            "requestTemplates": { "application/json": "{\"statusCode\": 200}" },
        }),
    )
    .await;
    method_response(gateway, api, resource, "GET", "200", json!({})).await;
    integration_response(
        gateway,
        api,
        resource,
        "GET",
        "200",
        json!({ "responseTemplates": json_template(&backend_body()) }),
    )
    .await;
}

/// A front api whose root `GET` carries `integration`, deployed to [`FRONT_STAGE`], as its
/// id. The method authorizes nobody, so the call under test is the anonymous one.
async fn front(gateway: &Gateway, name: &str, integration: Value) -> String {
    let (api, root) = create_api(gateway, name).await;
    put_method(gateway, &api, &root, "GET", json!({ "authorizationType": "NONE" })).await;
    put_integration(gateway, &api, &root, "GET", integration).await;
    deploy(gateway, &api, FRONT_STAGE).await;
    api
}

/// `CreateVpcLink` over [`LOAD_BALANCER_ARN`], as its id.
async fn vpc_link(gateway: &Gateway, name: &str) -> String {
    let answer = gateway
        .call(
            "POST",
            "/vpclinks",
            Some(json!({ "name": name, "targetArns": [LOAD_BALANCER_ARN] })),
        )
        .await;
    assert_eq!(
        answer.status, 202,
        "case CreateVpcLink({name:?}): expected 202, got {} with {:?}",
        answer.status, answer.body
    );
    string_member(&answer.body, "id", "CreateVpcLink")
}

/// One invoke of a front api's deployed stage, carrying `headers` and no body.
async fn invoke_front(
    gateway: &Gateway,
    api: &str,
    headers: &[(&str, &str)],
) -> crate::execute_api_data_plane::Invoked {
    invoke_front_at(gateway, api, "", headers).await
}

/// [`invoke_front`] with `query` on the invoke path, for the cases about what of the
/// caller's own request travels onward.
async fn invoke_front_at(
    gateway: &Gateway,
    api: &str,
    query: &str,
    headers: &[(&str, &str)],
) -> crate::execute_api_data_plane::Invoked {
    crate::execute_api_data_plane::invoke_on_host(
        gateway,
        &api_host(api),
        "GET",
        &format!("/{FRONT_STAGE}/{query}"),
        headers,
    )
    .await
}

/// What a row expects of the answer the front api serves.
#[derive(Debug)]
enum Expect {
    /// The request was passed through and the backend's own answer is what came back.
    PassedThrough,
    /// The endpoint the request was passed on to refused it with this status, so that
    /// refusal is what came back — and never the backend's body.
    Onward(u16),
    /// No request was passed on: the integration is one this emulator cannot perform, so
    /// the api's `API_CONFIGURATION_ERROR` gateway response came back.
    NotPerformed,
}

/// The status and the `message` API Gateway's `API_CONFIGURATION_ERROR` gateway response
/// carries, which is what an integration this emulator cannot perform is answered with.
const CONFIGURATION_ERROR: (u16, &str) = (500, "Internal server error");

/// Assert the answer `served` is what `expect` says, naming the case.
fn assert_served(case: &str, served: &crate::execute_api_data_plane::Invoked, expect: &Expect) {
    match expect {
        Expect::PassedThrough => {
            assert_eq!(
                served.status, 200,
                "case {case:?}: expected the backend's own 200, got {} with {:?}",
                served.status, served.body
            );
            assert_eq!(
                served.body,
                backend_body(),
                "case {case:?}: expected the backend's own body, got {:?}",
                served.body
            );
        }
        Expect::Onward(status) => {
            assert_eq!(
                served.status, *status,
                "case {case:?}: expected the endpoint's own {status}, got {} with {:?}",
                served.status, served.body
            );
            assert!(
                !served.body.contains(NONCE),
                "case {case:?}: a refused onward request carries none of the backend's \
                 body, got {:?}",
                served.body
            );
        }
        Expect::NotPerformed => {
            let (status, message) = CONFIGURATION_ERROR;
            assert_eq!(
                served.status, status,
                "case {case:?}: expected {status} with no request passed on, got {} with \
                 {:?}",
                served.status, served.body
            );
            assert_eq!(
                served.message().as_deref(),
                Some(message),
                "case {case:?}: expected the API_CONFIGURATION_ERROR body, got {:?}",
                served.body
            );
            assert!(
                !served.body.contains(NONCE),
                "case {case:?}: an integration that was not performed carries none of the \
                 backend's body, got {:?}",
                served.body
            );
        }
    }
}

/// One row of the table below: the case, the integration under test, the headers the caller
/// sends and what must come back. An alias because the tuple itself is a type complex enough
/// to be worth naming once.
type Row<'a> = (&'a str, Value, Vec<(&'a str, &'a str)>, Expect);

#[tokio::test]
async fn an_integration_naming_an_endpoint_of_this_emulator_passes_the_request_through() {
    let gateway = Gateway::new();
    let api = backend(&gateway).await;
    let onward_path = format!("/{BACKEND_STAGE}/{BACKEND_PATH}");

    // One row per thing the rule generalises over: (case, the integration, the headers the
    // caller sends, what must come back).
    let rows: Vec<Row<'_>> = vec![
        (
            "a proxy to the backend's own invoke endpoint",
            json!({
                "type": "HTTP_PROXY",
                "httpMethod": "GET",
                "uri": format!("https://{host}{onward_path}", host = api_host(&api)),
            }),
            Vec::new(),
            Expect::PassedThrough,
        ),
        (
            "a proxy to an interface endpoint's own DNS name, with the api id mapped on",
            json!({
                "type": "HTTP_PROXY",
                "httpMethod": "GET",
                "uri": format!("https://{host}{onward_path}", host = endpoint_host(ENDPOINT)),
                "requestParameters": {
                    "integration.request.header.x-apigw-api-id": format!("'{api}'"),
                },
            }),
            Vec::new(),
            Expect::PassedThrough,
        ),
        (
            "a proxy that maps the api id from a header of the method request",
            json!({
                "type": "HTTP_PROXY",
                "httpMethod": "GET",
                "uri": format!("https://{host}{onward_path}", host = endpoint_host(ENDPOINT)),
                "requestParameters": {
                    "integration.request.header.x-apigw-api-id":
                        "method.request.header.x-which-api",
                },
            }),
            vec![("x-which-api", api.as_str())],
            Expect::PassedThrough,
        ),
        (
            "a proxy to an interface endpoint's own DNS name naming no api at all",
            json!({
                "type": "HTTP_PROXY",
                "httpMethod": "GET",
                "uri": format!("https://{host}{onward_path}", host = endpoint_host(ENDPOINT)),
            }),
            Vec::new(),
            // The endpoint names the endpoint, not an api, and nothing carried the api id:
            // the endpoint serves no api of this account for that request.
            Expect::Onward(403),
        ),
        (
            "a proxy to a path the backend does not serve",
            json!({
                "type": "HTTP_PROXY",
                "httpMethod": "GET",
                "uri": format!(
                    "https://{host}/{BACKEND_STAGE}/nowhere",
                    host = api_host(&api)
                ),
            }),
            Vec::new(),
            Expect::Onward(403),
        ),
        (
            "a proxy to an api this account does not hold",
            json!({
                "type": "HTTP_PROXY",
                "httpMethod": "GET",
                "uri": format!(
                    "https://notanapi00.execute-api.{REGION}.amazonaws.com{onward_path}"
                ),
            }),
            Vec::new(),
            Expect::Onward(403),
        ),
        (
            "a proxy to a host outside this emulator",
            json!({
                "type": "HTTP_PROXY",
                "httpMethod": "GET",
                "uri": "https://example.com/back/echo",
            }),
            Vec::new(),
            Expect::NotPerformed,
        ),
        (
            // The type whose answer is MAPPED reaches the same endpoint the same way (see
            // `a_standard_http_integrations_backend_answer_is_mapped_by_the_integrations_own_responses`
            // below); this front api declares no integration response at all, so there is
            // nothing to map the backend's answer with and the call is not performed.
            "a standard HTTP integration with no integration response to map the answer",
            json!({
                "type": "HTTP",
                "httpMethod": "GET",
                "uri": format!("https://{host}{onward_path}", host = api_host(&api)),
            }),
            Vec::new(),
            Expect::NotPerformed,
        ),
        (
            "a proxy whose uri carries a mapping placeholder",
            json!({
                "type": "HTTP_PROXY",
                "httpMethod": "GET",
                "uri": format!(
                    "https://{host}/{BACKEND_STAGE}/{{proxy}}",
                    host = api_host(&api)
                ),
            }),
            Vec::new(),
            Expect::NotPerformed,
        ),
        (
            // `integration.request.{location}.{name}` names a header, a path parameter or a
            // query-string parameter of the integration request; a key naming any other
            // location is a mapping this emulator does not apply, and the request is not
            // passed on with it dropped.
            "a proxy mapping something that is no parameter of the integration request",
            json!({
                "type": "HTTP_PROXY",
                "httpMethod": "GET",
                "uri": format!("https://{host}{onward_path}", host = api_host(&api)),
                "requestParameters": {
                    "integration.request.body.who": "'nobody'",
                },
            }),
            Vec::new(),
            Expect::NotPerformed,
        ),
        (
            "a proxy mapping a header from a value this emulator does not read",
            json!({
                "type": "HTTP_PROXY",
                "httpMethod": "GET",
                "uri": format!("https://{host}{onward_path}", host = api_host(&api)),
                "requestParameters": {
                    "integration.request.header.x-apigw-api-id": "context.requestId",
                },
            }),
            Vec::new(),
            Expect::NotPerformed,
        ),
        (
            "a private proxy over a VPC link this account does not hold",
            json!({
                "type": "HTTP_PROXY",
                "httpMethod": "GET",
                "uri": format!("https://{host}{onward_path}", host = api_host(&api)),
                "connectionType": "VPC_LINK",
                "connectionId": "nosuchlink",
            }),
            Vec::new(),
            Expect::NotPerformed,
        ),
        (
            "a private proxy whose connectionId is a stage variable",
            json!({
                "type": "HTTP_PROXY",
                "httpMethod": "GET",
                "uri": format!("https://{host}{onward_path}", host = api_host(&api)),
                "connectionType": "VPC_LINK",
                "connectionId": "${stageVariables.link}",
            }),
            Vec::new(),
            Expect::NotPerformed,
        ),
    ];

    for (case, integration, headers, expect) in rows {
        let front = front(&gateway, case, integration).await;
        let served = invoke_front(&gateway, &front, &headers).await;
        assert_served(case, &served, &expect);
    }
}

/// A private integration is carried only over a link that has finished provisioning: the
/// SAME configuration is not performed while the link is `PENDING` and is performed once the
/// clock has settled it, with nothing called in between.
#[tokio::test]
async fn a_private_integration_waits_for_the_vpc_link_it_connects_through() {
    let gateway = Gateway::new();
    let api = backend(&gateway).await;
    let link = vpc_link(&gateway, "passed-through-link").await;
    let front = front(
        &gateway,
        "a private proxy over a link of this account",
        json!({
            "type": "HTTP_PROXY",
            "httpMethod": "GET",
            "uri": format!(
                "https://{host}/{BACKEND_STAGE}/{BACKEND_PATH}",
                host = api_host(&api)
            ),
            "connectionType": "VPC_LINK",
            "connectionId": link,
        }),
    )
    .await;

    // `CreateVpcLink` is "an asynchronous operation": the link is PENDING when the call
    // returns, and a link that is still provisioning carries no traffic.
    let provisioning = invoke_front(&gateway, &front, &[]).await;
    assert_served(
        "the link is still provisioning",
        &provisioning,
        &Expect::NotPerformed,
    );

    tokio::time::sleep(SETTLE_WAIT).await;
    let provisioned = invoke_front(&gateway, &front, &[]).await;
    assert_served(
        "the link has provisioned",
        &provisioned,
        &Expect::PassedThrough,
    );
}

/// What of the CALLER's own request travels onward: a proxy integration passes the request
/// through, so the query string the caller sent reaches the backend — a backend method that
/// declares a query parameter REQUIRED refuses the call that omits it and serves the one
/// that carries it, and both of those answers are what the front api serves.
///
/// Run for two parameter NAMES: an ordinary one, and one spelled like the `x-amz-` family a
/// presigned credential is carried in without being any part of that credential — which is
/// what pins the withheld set to the credential's own parameters rather than to a prefix.
#[tokio::test]
async fn the_callers_query_string_reaches_the_backend() {
    let gateway = Gateway::new();
    for required in ["who", "x-amz-meta-note"] {
        let front = requiring_query_parameter(&gateway, required).await;

        // (case, the query the caller sends, what must come back)
        let rows: [(&str, String, Expect); 3] = [
            (
                "the caller sends the parameter the backend requires",
                format!("?{required}=someone"),
                Expect::PassedThrough,
            ),
            (
                "the caller sends another parameter instead",
                "?other=someone".to_string(),
                Expect::Onward(400),
            ),
            (
                "the caller sends no query string at all",
                String::new(),
                Expect::Onward(400),
            ),
        ];
        for (case, query, expect) in rows {
            let case = format!("{case}, the parameter named {required:?}");
            let served = invoke_front_at(&gateway, &front, &query, &[]).await;
            assert_served(&case, &served, &expect);
        }
    }
}

/// A front api proxying to a backend whose method declares the query parameter `required`
/// REQUIRED and validates its request parameters, as the front api's id.
async fn requiring_query_parameter(gateway: &Gateway, required: &str) -> String {
    let (api, root) = create_api(gateway, &format!("passed-through-query-{required}")).await;
    let validator =
        crate::stage_request_validation::create_validator(gateway, &api, "parameters", false, true)
            .await;
    let resource = create_resource(gateway, &api, &root, BACKEND_PATH).await;
    let resource_id = string_member(&resource, "id", BACKEND_PATH);
    put_method(
        gateway,
        &api,
        &resource_id,
        "GET",
        json!({
            "authorizationType": "NONE",
            "requestValidatorId": validator,
            "requestParameters": { format!("method.request.querystring.{required}"): true },
        }),
    )
    .await;
    mock_integration(gateway, &api, &resource_id).await;
    deploy(gateway, &api, BACKEND_STAGE).await;

    front(
        gateway,
        &format!("a proxy to a backend requiring {required}"),
        json!({
            "type": "HTTP_PROXY",
            "httpMethod": "GET",
            "uri": format!(
                "https://{host}/{BACKEND_STAGE}/{BACKEND_PATH}",
                host = api_host(&api)
            ),
        }),
    )
    .await
}

/// A mapped request parameter whose SOURCE the request does not carry maps nothing: the
/// header is left off and the request is still passed on, which is what the endpoint's own
/// answer then shows — not the configuration error an unreadable mapping earns.
#[tokio::test]
async fn a_mapped_source_the_request_does_not_carry_leaves_the_header_off() {
    let gateway = Gateway::new();
    let api = backend(&gateway).await;
    let front = front(
        &gateway,
        "a proxy mapping the api id from a header the caller may omit",
        json!({
            "type": "HTTP_PROXY",
            "httpMethod": "GET",
            "uri": format!(
                "https://{host}/{BACKEND_STAGE}/{BACKEND_PATH}",
                host = endpoint_host(ENDPOINT)
            ),
            "requestParameters": {
                "integration.request.header.x-apigw-api-id":
                    "method.request.header.x-which-api",
            },
        }),
    )
    .await;

    let carried = invoke_front(&gateway, &front, &[("x-which-api", api.as_str())]).await;
    assert_served(
        "the caller carries the header the mapping reads",
        &carried,
        &Expect::PassedThrough,
    );

    // Nothing named the api, so the endpoint the request was passed on to serves no api of
    // this account for it — the endpoint's own refusal, which is proof the request was
    // passed on rather than refused here.
    let omitted = invoke_front(&gateway, &front, &[]).await;
    assert_served(
        "the caller omits the header the mapping reads",
        &omitted,
        &Expect::Onward(403),
    );
}

/// The caller's own SigV4 credential does not travel onward, in either form it is carried
/// in: a backend method whose `authorizationType` is `AWS_IAM` is reached UNCREDENTIALED by
/// a passed-through request, however the caller credentialed the front api, because a
/// signature over the front endpoint's address authenticates nothing at the backend's.
#[tokio::test]
async fn the_callers_own_credential_does_not_travel_onward() {
    let gateway = Gateway::new();
    let (api, root) = create_api(&gateway, "passed-through-iam-backend").await;
    let resource = create_resource(&gateway, &api, &root, BACKEND_PATH).await;
    let resource_id = string_member(&resource, "id", BACKEND_PATH);
    put_method(
        &gateway,
        &api,
        &resource_id,
        "GET",
        json!({ "authorizationType": "AWS_IAM" }),
    )
    .await;
    mock_integration(&gateway, &api, &resource_id).await;
    deploy(&gateway, &api, BACKEND_STAGE).await;

    let front = front(
        &gateway,
        "a proxy to a backend method authorized by IAM",
        json!({
            "type": "HTTP_PROXY",
            "httpMethod": "GET",
            "uri": format!(
                "https://{host}/{BACKEND_STAGE}/{BACKEND_PATH}",
                host = api_host(&api)
            ),
        }),
    )
    .await;

    // (case, the query the caller sends, the headers it sends). Both forms a SigV4
    // credential is presented in: the header, and the presigned query string.
    let rows = [
        (
            "the caller presents a credential in the header",
            "",
            vec![("authorization", SIGNED)],
        ),
        (
            "the caller presents a presigned credential in the query string",
            "?X-Amz-Algorithm=AWS4-HMAC-SHA256&X-Amz-Credential=AKIDEXAMPLE%2F20260101\
             %2Fus-east-1%2Fexecute-api%2Faws4_request&X-Amz-Signature=0123456789abcdef",
            Vec::new(),
        ),
    ];
    for (case, query, headers) in rows {
        let served = invoke_front_at(&gateway, &front, query, &headers).await;
        // The backend's own answer to an `AWS_IAM` method reached with no credential.
        assert_served(case, &served, &Expect::Onward(403));
    }
}

/// A chain of proxy integrations is followed only so far: an integration that passes the
/// request to its OWN stage is answered through the gateway response rather than followed
/// until the stack is gone.
#[tokio::test]
async fn a_chain_of_passed_through_integrations_is_bounded() {
    let gateway = Gateway::new();
    // The api's own stage: the one address that makes every hop find this same integration
    // again, which is what the bound exists for. Its id is only known after the create, so
    // this one front api is built here rather than through `front`.
    let (api, root) = create_api(&gateway, "passed-through-loop").await;
    put_method(
        &gateway,
        &api,
        &root,
        "GET",
        json!({ "authorizationType": "NONE" }),
    )
    .await;
    put_integration(
        &gateway,
        &api,
        &root,
        "GET",
        json!({
            "type": "HTTP_PROXY",
            "httpMethod": "GET",
            "uri": format!("https://{host}/{FRONT_STAGE}/", host = api_host(&api)),
        }),
    )
    .await;
    deploy(&gateway, &api, FRONT_STAGE).await;

    let served = invoke_front(&gateway, &api, &[]).await;
    assert_served(
        "an integration naming its own stage",
        &served,
        &Expect::NotPerformed,
    );
}

/// The same bound holds for a loop made by the ADDRESS the request is passed on at rather
/// than by the stored spelling: the `uri` names a host outside this emulator and carries a
/// `{proxy}` placeholder, and the value the integration maps onto it — a header the CALLER
/// sent — expands the address onto this plane's own announced path-style address for the
/// api's own stage. Such a request is served by RE-ENTERING this plane, which is what carries
/// the hop count, so the chain is bounded and answered through the api's gateway response
/// instead of leaving as an onward request that comes back in through the claim with the
/// count reset.
#[tokio::test]
async fn a_chain_whose_loop_is_made_by_an_expanded_path_parameter_is_bounded() {
    let gateway = Gateway::new();
    let other = backend(&gateway).await;
    let (api, root) = create_api(&gateway, "passed-through-expanded-loop").await;
    put_method(
        &gateway,
        &api,
        &root,
        "GET",
        json!({
            "authorizationType": "NONE",
            "requestParameters": { "method.request.header.x-loop-to": false },
        }),
    )
    .await;
    put_integration(
        &gateway,
        &api,
        &root,
        "GET",
        json!({
            "type": "HTTP_PROXY",
            "httpMethod": "GET",
            // A host nothing in this emulator serves, so the STORED spelling names no
            // endpoint of this plane at all.
            "uri": "https://backend.example.com/{proxy}",
            "requestParameters": {
                "integration.request.path.proxy": "method.request.header.x-loop-to",
            },
        }),
    )
    .await;
    deploy(&gateway, &api, FRONT_STAGE).await;

    // 1. The path-style address this emulator announces ANOTHER api's stage at: not a loop, so
    //    the re-entry SERVES it and that api's own answer comes back. This is what tells "the
    //    expanded address was served by this plane" from "every expansion is refused".
    let served = invoke_front(
        &gateway,
        &api,
        &[(
            "x-loop-to",
            &format!("_aws/execute-api/{other}/{BACKEND_STAGE}/{BACKEND_PATH}"),
        )],
    )
    .await;
    assert_served(
        "an expanded address naming another api's stage of this plane",
        &served,
        &Expect::PassedThrough,
    );

    // 2. The same expansion naming the api's OWN stage, which is the loop: every hop finds
    //    this same integration again, so the chain reaches the bound and is answered through
    //    the gateway response.
    let loop_to = format!("_aws/execute-api/{api}/{FRONT_STAGE}/");
    let served = invoke_front(&gateway, &api, &[("x-loop-to", &loop_to)]).await;
    assert_served(
        "an expanded address naming this plane's own stage",
        &served,
        &Expect::NotPerformed,
    );
}

/// The same bound holds for a loop made by the HOST the onward request carries: the `uri`
/// names a host outside this emulator, and the integration maps a header the CALLER sent onto
/// `host` — the header a service claiming an onward request reads its endpoint off, and the
/// one a mapping has the last word over. A request whose mapped host names a stage of THIS
/// plane is served by RE-ENTERING it, which is what carries the hop count, so the chain is
/// bounded instead of leaving as an onward request and coming back in through the claim with
/// the count reset.
#[tokio::test]
async fn a_chain_whose_loop_is_made_by_a_mapped_host_header_is_bounded() {
    let gateway = Gateway::new();
    // A SIBLING api deployed to a stage of the same name, serving its root `GET`: the mapped
    // host that names a stage of this plane without naming the api under test.
    let (sibling, sibling_root) = create_api(&gateway, "passed-through-mapped-host-sibling").await;
    put_method(
        &gateway,
        &sibling,
        &sibling_root,
        "GET",
        json!({ "authorizationType": "NONE" }),
    )
    .await;
    mock_integration(&gateway, &sibling, &sibling_root).await;
    deploy(&gateway, &sibling, FRONT_STAGE).await;

    let (api, root) = create_api(&gateway, "passed-through-mapped-host-loop").await;
    put_method(
        &gateway,
        &api,
        &root,
        "GET",
        json!({
            "authorizationType": "NONE",
            "requestParameters": { "method.request.header.x-send-to": false },
        }),
    )
    .await;
    put_integration(
        &gateway,
        &api,
        &root,
        "GET",
        json!({
            "type": "HTTP_PROXY",
            "httpMethod": "GET",
            // A host nothing in this emulator serves — so the stored spelling names no
            // endpoint of this plane — on the path of the stage this api is deployed to.
            // WHICH endpoint the onward request reaches is the mapped host below.
            "uri": format!("https://backend.example.com/{FRONT_STAGE}/"),
            "requestParameters": {
                "integration.request.header.host": "method.request.header.x-send-to",
            },
        }),
    )
    .await;
    deploy(&gateway, &api, FRONT_STAGE).await;

    // 1. The mapped host names ANOTHER api's stage of this plane: not a loop, so the request
    //    is served there and that api's own answer comes back. This is what tells "the mapped
    //    host decided the endpoint" from "every mapping onto host is refused".
    let served = invoke_front(&gateway, &api, &[("x-send-to", &api_host(&sibling))]).await;
    assert_served(
        "a mapped host naming another api's stage of this plane",
        &served,
        &Expect::PassedThrough,
    );

    // 2. The same mapping naming the api's OWN stage, which is the loop: every hop finds this
    //    same integration again, so the chain reaches the bound and is answered through the
    //    gateway response.
    let served = invoke_front(&gateway, &api, &[("x-send-to", &api_host(&api))]).await;
    assert_served(
        "a mapped host naming this plane's own stage",
        &served,
        &Expect::NotPerformed,
    );
}

/// The response header the header-setting backend below sets on EVERY answer it gives, and
/// the value it sets: a browser's own `Access-Control-Allow-Origin`, which is the header a
/// proxy integration passes through and a standard one carries only where the method declares
/// it. Asserted lowercased, as HTTP header names compare.
const BACKEND_HEADER: &str = "access-control-allow-origin";

/// The value that backend sets it to. Any value does: what the rows differ along is whether
/// the caller sees it at all.
const BACKEND_ORIGIN: &str = "https://origin.example";

/// The body a front api's own response template renders, which is what tells a MAPPED body
/// from the backend's own.
const MAPPED_BODY: &str = "{\"mapped\":\"by-the-front\"}";

/// The body a front api's response template renders for a backend answer its
/// `selectionPattern` matched as a 4xx, which is what tells a pattern matched against the
/// BACKEND's status from one matched against nothing.
const REFUSED_BODY: &str = "{\"mapped\":\"the-backend-refused\"}";

/// The method response declaring the backend's header, as a front api must to carry it:
/// `MethodResponse$responseParameters` is "a key-value map specifying required or optional
/// response parameters that API Gateway can send back to the caller", keyed
/// `method.response.header.{name}`.
fn declares_header() -> Value {
    json!({
        "responseParameters": { "method.response.header.Access-Control-Allow-Origin": true },
    })
}

/// The integration response mapping the backend's own value for that header onto it:
/// `IntegrationResponse$responseParameters`' mapped value "must match the pattern of
/// `integration.response.header.{name}`".
fn maps_backend_header() -> Value {
    json!({
        "responseParameters": {
            "method.response.header.Access-Control-Allow-Origin":
                "integration.response.header.Access-Control-Allow-Origin",
        },
    })
}

/// A backend api serving [`BACKEND_PATH`] with a `MOCK` that answers [`backend_body`] AND
/// sets [`BACKEND_HEADER`] on it, deployed to [`BACKEND_STAGE`], as its id.
///
/// The header is the backend's own record — its method response declares it and its
/// integration response maps the static value onto it — so what the front api does with it is
/// the only thing a row below varies.
async fn header_backend(gateway: &Gateway) -> String {
    let (api, root) = create_api(gateway, "mapped-integration-backend").await;
    let resource = create_resource(gateway, &api, &root, BACKEND_PATH).await;
    let resource = string_member(&resource, "id", BACKEND_PATH);
    put_method(gateway, &api, &resource, "GET", json!({ "authorizationType": "NONE" })).await;
    put_integration(
        gateway,
        &api,
        &resource,
        "GET",
        json!({
            "type": "MOCK",
            "requestTemplates": { "application/json": "{\"statusCode\": 200}" },
        }),
    )
    .await;
    method_response(gateway, &api, &resource, "GET", "200", declares_header()).await;
    integration_response(
        gateway,
        &api,
        &resource,
        "GET",
        "200",
        json!({
            "responseTemplates": json_template(&backend_body()),
            "responseParameters": {
                "method.response.header.Access-Control-Allow-Origin":
                    format!("'{BACKEND_ORIGIN}'"),
            },
        }),
    )
    .await;
    deploy(gateway, &api, BACKEND_STAGE).await;
    api
}

/// A front api whose root `GET` carries `integration` and the `(status, method response,
/// integration response)` triples `responses` declares, deployed to [`FRONT_STAGE`], as its
/// id.
///
/// The responses are declared BEFORE the deployment, as a client configuring a mapped
/// integration does: what the stage serves is the deployed snapshot.
async fn mapped_front(
    gateway: &Gateway,
    name: &str,
    integration: Value,
    responses: &[(&str, Value, Value)],
) -> String {
    let (api, root) = create_api(gateway, name).await;
    put_method(gateway, &api, &root, "GET", json!({ "authorizationType": "NONE" })).await;
    put_integration(gateway, &api, &root, "GET", integration).await;
    for (status, method, integration) in responses {
        method_response(gateway, &api, &root, "GET", status, method.clone()).await;
        integration_response(gateway, &api, &root, "GET", status, integration.clone()).await;
    }
    deploy(gateway, &api, FRONT_STAGE).await;
    api
}

/// What a row of the mapped table expects of the answer the front api serves.
#[derive(Debug)]
enum Answered {
    /// The integration was performed and its answer mapped: this status, this body, and
    /// this value for [`BACKEND_HEADER`] — `None` when the caller must see no such header
    /// at all.
    Mapped {
        status: u16,
        body: String,
        header: Option<String>,
    },
    /// No answer could be mapped, so the api's `API_CONFIGURATION_ERROR` came back.
    NotPerformed,
}

impl Answered {
    /// [`Answered::Mapped`] for a row whose caller sees no [`BACKEND_HEADER`].
    fn mapped(status: u16, body: &str) -> Self {
        Self::Mapped {
            status,
            body: body.to_string(),
            header: None,
        }
    }

    /// [`Answered::Mapped`] for a row whose caller sees it carrying `header`.
    fn carrying(status: u16, body: &str, header: &str) -> Self {
        Self::Mapped {
            status,
            body: body.to_string(),
            header: Some(header.to_string()),
        }
    }
}

/// One row of the mapped table: the case, the integration, the responses the front declares,
/// the headers the caller sends and what must come back.
type MappedRow<'a> = (
    &'a str,
    Value,
    Vec<(&'a str, Value, Value)>,
    Vec<(&'a str, String)>,
    Answered,
);

/// A STANDARD `HTTP` integration is performed against the endpoint its `uri` names just as a
/// proxy one is, and what separates the two is the MAPPING: the backend's answer is the thing
/// the integration's own responses map ("`selectionPattern`" is "the regular expression (regex)
/// pattern used to choose an integration response based on the response from the back end" —
/// "for all other HTTP and Amazon Web Services back ends, the HTTP status code is matched"),
/// and only the headers the method response declares and the integration response maps reach
/// the caller, where a proxy integration's answer travels whole.
///
/// The rows span what the rule generalises over: the integration type against ONE backend
/// answer (the CORS pair — the same backend header passed through by the proxy and dropped by
/// the standard type), what the mapping declares (a header mapped onto a declared method
/// response header, the same mapping with the method response declaring nothing, a response
/// template, no integration response at all), which integration response the BACKEND's status
/// selects (the default catch-all, a `selectionPattern` over the class it answered) and what
/// the integration request carries (the caller's own header, which travels through a proxy
/// integration and not through a standard one).
#[tokio::test]
async fn a_standard_http_integrations_backend_answer_is_mapped_by_the_integrations_own_responses()
{
    let gateway = Gateway::new();
    let api = header_backend(&gateway).await;
    let onward_path = format!("/{BACKEND_STAGE}/{BACKEND_PATH}");
    let backend_uri = format!("https://{host}{onward_path}", host = api_host(&api));
    // The interface endpoint's own DNS name names the ENDPOINT and no api, so what api is
    // reached there is whatever the integration request carries in `x-apigw-api-id`.
    let endpoint_uri = format!("https://{host}{onward_path}", host = endpoint_host(ENDPOINT));
    let standard = |uri: &str| json!({ "type": "HTTP", "httpMethod": "GET", "uri": uri });
    let backend_body = backend_body();
    // The api id as the CALLER carries it, for the two rows that differ only in whether the
    // integration request carries what the caller sent.
    let stated_api = vec![("x-apigw-api-id", api.clone())];

    let rows: Vec<MappedRow<'_>> = vec![
        (
            "a standard integration whose integration response maps no header",
            standard(&backend_uri),
            vec![("200", json!({}), json!({}))],
            Vec::new(),
            // The backend set the header; nothing of this api's records carries it, so the
            // caller never sees it.
            Answered::mapped(200, &backend_body),
        ),
        (
            "a proxy integration to the same backend answer",
            json!({ "type": "HTTP_PROXY", "httpMethod": "GET", "uri": &backend_uri }),
            Vec::new(),
            Vec::new(),
            // Nothing maps a passed-through response, so the backend's own header travels.
            Answered::carrying(200, &backend_body, BACKEND_ORIGIN),
        ),
        (
            "a standard integration mapping the backend's header onto a declared one",
            standard(&backend_uri),
            vec![("200", declares_header(), maps_backend_header())],
            Vec::new(),
            Answered::carrying(200, &backend_body, BACKEND_ORIGIN),
        ),
        (
            "a standard integration mapping a header its method response does not declare",
            standard(&backend_uri),
            vec![("200", json!({}), maps_backend_header())],
            Vec::new(),
            // API Gateway returns only the headers the METHOD response declares, so a
            // mapping onto an undeclared one carries nothing.
            Answered::mapped(200, &backend_body),
        ),
        (
            "a standard integration whose response template renders the body",
            standard(&backend_uri),
            vec![(
                "200",
                json!({}),
                json!({ "responseTemplates": json_template(MAPPED_BODY) }),
            )],
            Vec::new(),
            Answered::mapped(200, MAPPED_BODY),
        ),
        (
            "a standard integration whose selection pattern maps the 2xx class onto 201",
            standard(&backend_uri),
            vec![("201", json!({}), json!({ "selectionPattern": "2\\d\\d" }))],
            Vec::new(),
            // The status the caller sees is the integration response's own, which is what
            // mapping an answer means.
            Answered::mapped(201, &backend_body),
        ),
        (
            "a standard integration with no integration response for the backend's status",
            standard(&backend_uri),
            Vec::new(),
            Vec::new(),
            Answered::NotPerformed,
        ),
        (
            "a standard integration naming a host outside this emulator",
            standard(&format!("https://example.com{onward_path}")),
            vec![("200", json!({}), json!({}))],
            Vec::new(),
            // No outbound connection is made, so there is no backend answer to map.
            Answered::NotPerformed,
        ),
        (
            "a standard integration whose backend refuses the path it names",
            standard(&format!(
                "https://{host}/{BACKEND_STAGE}/nowhere",
                host = api_host(&api)
            )),
            vec![(
                "403",
                json!({}),
                json!({
                    "selectionPattern": "4\\d\\d",
                    "responseTemplates": json_template(REFUSED_BODY),
                }),
            )],
            Vec::new(),
            // The pattern is matched against the status the BACKEND answered, so the 4xx it
            // refused with selects this response rather than a 2xx one.
            Answered::mapped(403, REFUSED_BODY),
        ),
        (
            "a proxy integration carrying the api id the CALLER sent",
            json!({ "type": "HTTP_PROXY", "httpMethod": "GET", "uri": &endpoint_uri }),
            Vec::new(),
            stated_api.clone(),
            // Everything the caller sent travels on, so the endpoint resolves the api that
            // header names and the backend answers.
            Answered::carrying(200, &backend_body, BACKEND_ORIGIN),
        ),
        (
            "a standard integration where the same api id is only the CALLER's",
            standard(&endpoint_uri),
            vec![(
                "403",
                json!({}),
                json!({
                    "selectionPattern": "4\\d\\d",
                    "responseTemplates": json_template(REFUSED_BODY),
                }),
            )],
            stated_api,
            // A standard integration carries nothing of the caller's that its
            // `requestParameters` do not map, so the endpoint is reached naming no api and
            // refuses — and that refusal is the backend answer the 4xx pattern maps.
            Answered::mapped(403, REFUSED_BODY),
        ),
    ];

    for (index, (case, integration, responses, headers, expect)) in rows.iter().enumerate() {
        let front = mapped_front(
            &gateway,
            &format!("mapped-front-{index}"),
            integration.clone(),
            responses,
        )
        .await;
        let sent: Vec<(&str, &str)> = headers
            .iter()
            .map(|(name, value)| (*name, value.as_str()))
            .collect();
        let served = invoke_front(&gateway, &front, &sent).await;
        match expect {
            Answered::Mapped {
                status,
                body,
                header,
            } => {
                assert_eq!(
                    served.status, *status,
                    "case {case:?}: expected {status}, got {} with {:?}",
                    served.status, served.body
                );
                assert_eq!(
                    served.body, *body,
                    "case {case:?}: expected the body {body:?}, got {:?}",
                    served.body
                );
                assert_eq!(
                    served.header(BACKEND_HEADER),
                    header.as_deref(),
                    "case {case:?}: expected {BACKEND_HEADER} to be {header:?}, got {:?}",
                    served.header(BACKEND_HEADER)
                );
            }
            Answered::NotPerformed => {
                let (status, message) = CONFIGURATION_ERROR;
                assert_eq!(
                    served.status, status,
                    "case {case:?}: expected {status} with nothing mapped, got {} with {:?}",
                    served.status, served.body
                );
                assert_eq!(
                    served.message().as_deref(),
                    Some(message),
                    "case {case:?}: expected the API_CONFIGURATION_ERROR body, got {:?}",
                    served.body
                );
                assert!(
                    !served.body.contains(NONCE),
                    "case {case:?}: an answer that was not mapped carries none of the \
                     backend's body, got {:?}",
                    served.body
                );
            }
        }
    }
}
