// SPDX-License-Identifier: CC0-1.0

//! A deployed `OPTIONS` method behind a `MOCK` integration: the CORS preflight a browser
//! sends is answered from the api's own records.

use serde_json::json;

use crate::execute_api_data_plane::{integration_response, method_response, put_integration,
                                    put_method};
use crate::rest_api_lifecycle::{create_api, create_resource, string_member, Gateway};

const REGION: &str = "us-east-1";
const STAGE: &str = "live";

#[tokio::test]
async fn a_mock_options_method_answers_the_preflight() {
    let gateway = Gateway::new();
    let (api, root) = create_api(&gateway, "cors-preflight").await;
    let resource = create_resource(&gateway, &api, &root, "items").await;
    let resource = string_member(&resource, "id", "CreateResource(items)");

    put_method(
        &gateway,
        &api,
        &resource,
        "OPTIONS",
        json!({ "authorizationType": "NONE" }),
    )
    .await;
    put_integration(
        &gateway,
        &api,
        &resource,
        "OPTIONS",
        json!({
            "type": "MOCK",
            "requestTemplates": { "application/json": "{\"statusCode\": 200}" },
        }),
    )
    .await;
    method_response(
        &gateway,
        &api,
        &resource,
        "OPTIONS",
        "200",
        json!({
            "responseParameters": {
                "method.response.header.Access-Control-Allow-Origin": false,
                "method.response.header.Access-Control-Allow-Methods": false,
                "method.response.header.Access-Control-Allow-Headers": false,
            },
        }),
    )
    .await;
    integration_response(
        &gateway,
        &api,
        &resource,
        "OPTIONS",
        "200",
        json!({
            "responseTemplates": { "application/json": "" },
            "responseParameters": {
                "method.response.header.Access-Control-Allow-Origin": "'*'",
                "method.response.header.Access-Control-Allow-Methods": "'OPTIONS,GET'",
                "method.response.header.Access-Control-Allow-Headers": "'Content-Type'",
            },
        }),
    )
    .await;
    let deployed = gateway
        .call(
            "POST",
            &format!("/restapis/{api}/deployments"),
            Some(json!({ "stageName": STAGE })),
        )
        .await;
    assert_eq!(
        deployed.status, 201,
        "case CreateDeployment: expected 201, got {} with {:?}",
        deployed.status, deployed.body
    );

    let host = format!("{api}.execute-api.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("OPTIONS")
        .uri(format!("https://{host}/{STAGE}/items"))
        .header("host", &host)
        .header("access-control-request-method", "GET")
        .header("access-control-request-headers", "content-type")
        .body(bytes::Bytes::new())
        .expect("the test request is well formed");
    let response = gateway.send(request).await;
    let status = response.status().as_u16();
    let headers = response.headers().clone();
    let body = String::from_utf8_lossy(&response.into_body()).into_owned();
    let header = |name: &str| {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
    };
    assert_eq!(
        status, 200,
        "case \"the preflight\": expected 200, got {status} with {body:?} and {headers:?}"
    );
    assert_eq!(
        header("access-control-allow-origin").as_deref(),
        Some("*"),
        "case \"the preflight\": expected the mapped origin header, got {headers:?}"
    );
}

/// The origin the page under test was loaded from, which the `OPTIONS` method's integration
/// response declares for the whole resource.
const PAGE_ORIGIN: &str = "https://app.sim2real.example";

/// The stage the CORS-enabled resource below is deployed to.
const CORS_STAGE: &str = "test";

/// The `Access-Control-*` headers the `OPTIONS` method's integration response maps, as the
/// header a browser reads and the LITERAL the mapping is stored with: each is "a static value
/// enclosed within a pair of single quotes" (`PutIntegrationResponse$responseParameters`), and
/// what a caller is served is that value with the quotes stripped.
///
/// The four names are the ones AWS's own "Enabling CORS for a REST API resource" flow declares,
/// and they span the value shapes a preflight carries: an origin, a verb list, a header list
/// and a bare number.
const DECLARED_PREFLIGHT_HEADERS: &[(&str, &str)] = &[
    ("access-control-allow-origin", PAGE_ORIGIN),
    ("access-control-allow-methods", "GET,OPTIONS"),
    ("access-control-allow-headers", "Content-Type,Authorization"),
    ("access-control-max-age", "600"),
];

/// The body the resource's `GET` method serves from its own response template, which declares
/// no `Access-Control-*` mapping of its own — as a method whose backend carries those headers
/// does.
const GET_BODY: &str = "{\"items\":[]}";

/// RULE: a deployed stage reads a request's `Origin` for nothing about which answer it serves —
/// a request a browser sent from another site is answered by the matched method, its
/// integration and its declared header mappings exactly as any other caller's is, each mapped
/// header carrying the integration response's own literal with its surrounding single quotes
/// stripped — and the api's own deployed records admit such a page where the RESOURCE it
/// addresses declares that origin, on any of its deployed methods.
///
/// The rows span what that rule generalises over: the verb (the `OPTIONS` preflight, whose own
/// method carries the declaration, and the real request that follows it, whose method declares
/// nothing of its own), how the request reads as another site's (an `Origin` alone, as an older
/// browser's preflight carries it, and an `Origin` beside a browser's own `cross-site`
/// classification), and the addressing the client reaches the stage by (the api's own
/// `execute-api` host and the path-style address this emulator announces on its own endpoint).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cors_enabled_resource_answers_every_cross_origin_call_its_records_declare() {
    let gateway = Gateway::new();
    let api = cors_enabled_api(&gateway).await;

    let origin_alone: &[(&str, &str)] = &[
        ("origin", PAGE_ORIGIN),
        ("access-control-request-method", "GET"),
        ("access-control-request-headers", "content-type"),
    ];
    let classified: &[(&str, &str)] = &[
        ("origin", PAGE_ORIGIN),
        ("sec-fetch-site", "cross-site"),
        ("access-control-request-method", "GET"),
        ("access-control-request-headers", "content-type"),
    ];

    let own_host = format!("{api}.execute-api.{REGION}.amazonaws.com");
    let endpoint = "127.0.0.1:39462";
    let addressings: [(&str, &str, String); 2] = [
        (
            "the api's own execute-api host",
            &own_host,
            format!("https://{own_host}/{CORS_STAGE}"),
        ),
        (
            "the emulator's own endpoint",
            endpoint,
            format!("https://{endpoint}/_aws/execute-api/{api}/{CORS_STAGE}"),
        ),
    ];

    for (addressing, host, base) in addressings {
        for (how, browser_headers) in [
            ("an Origin alone", origin_alone),
            ("an Origin a browser classified cross-site", classified),
        ] {
            for (verb, expected_body, expected_headers) in [
                ("OPTIONS", "", DECLARED_PREFLIGHT_HEADERS),
                // The real request's method maps no response header, so its answer carries
                // none: what the resource DECLARES admits the page, and what each METHOD maps
                // is what that method serves.
                ("GET", GET_BODY, &[] as &[(&str, &str)]),
            ] {
                let case = format!("{addressing}, {how}, {verb} /items");
                let mut builder = http::Request::builder()
                    .method(verb)
                    .uri(format!("{base}/items"))
                    .header("host", host);
                for (name, value) in browser_headers {
                    builder = builder.header(*name, *value);
                }
                let response = gateway
                    .send(
                        builder
                            .body(bytes::Bytes::new())
                            .expect("the test request is well formed"),
                    )
                    .await;
                let status = response.status().as_u16();
                let headers = response.headers().clone();
                let body = String::from_utf8_lossy(&response.into_body()).into_owned();
                assert_eq!(
                    status, 200,
                    "case {case:?}: expected the method's own 200, got {status} with {body:?} \
                     and {headers:?}"
                );
                assert_eq!(
                    body, expected_body,
                    "case {case:?}: expected the body {expected_body:?}, got {body:?}"
                );
                for (name, expected) in expected_headers {
                    let actual = headers.get(*name).and_then(|value| value.to_str().ok());
                    assert_eq!(
                        actual,
                        Some(*expected),
                        "case {case:?}: expected the declared {name} {expected:?} with no \
                         quotes, got {actual:?}"
                    );
                }
                let mut served: Vec<String> = headers
                    .keys()
                    .map(|name| name.as_str().to_string())
                    .filter(|name| name.starts_with("access-control-"))
                    .collect();
                served.sort();
                let mut declared: Vec<String> = expected_headers
                    .iter()
                    .map(|(name, _)| (*name).to_string())
                    .collect();
                declared.sort();
                assert_eq!(
                    served, declared,
                    "case {case:?}: expected exactly the mapped Access-Control-* headers \
                     {declared:?}, got {served:?}"
                );
            }
        }
    }
}

/// The api every row above drives: one `/items` resource carrying the `MOCK` `OPTIONS` method
/// AWS's CORS flow adds — whose integration response maps the four `Access-Control-*` headers —
/// and a `GET` method that declares no header mapping of its own, deployed to one stage.
/// Answers the api id.
async fn cors_enabled_api(gateway: &Gateway) -> String {
    let (api, root) = create_api(gateway, "cors-enabled-resource").await;
    let resource = create_resource(gateway, &api, &root, "items").await;
    let resource = string_member(&resource, "id", "CreateResource(items)");

    for verb in ["OPTIONS", "GET"] {
        put_method(
            gateway,
            &api,
            &resource,
            verb,
            json!({ "authorizationType": "NONE" }),
        )
        .await;
        put_integration(
            gateway,
            &api,
            &resource,
            verb,
            json!({
                "type": "MOCK",
                "requestTemplates": { "application/json": "{\"statusCode\": 200}" },
            }),
        )
        .await;
    }

    let declared: serde_json::Map<String, serde_json::Value> = DECLARED_PREFLIGHT_HEADERS
        .iter()
        .map(|(name, _)| (method_response_header(name), serde_json::Value::Bool(false)))
        .collect();
    let mapped: serde_json::Map<String, serde_json::Value> = DECLARED_PREFLIGHT_HEADERS
        .iter()
        .map(|(name, value)| {
            (
                method_response_header(name),
                serde_json::Value::String(format!("'{value}'")),
            )
        })
        .collect();
    method_response(
        gateway,
        &api,
        &resource,
        "OPTIONS",
        "200",
        json!({ "responseParameters": declared }),
    )
    .await;
    integration_response(
        gateway,
        &api,
        &resource,
        "OPTIONS",
        "200",
        json!({
            "responseTemplates": { "application/json": "" },
            "responseParameters": mapped,
        }),
    )
    .await;

    // The real request's method declares no mapping at all: what admits a page to it is its
    // RESOURCE's declaration above.
    method_response(gateway, &api, &resource, "GET", "200", json!({})).await;
    integration_response(
        gateway,
        &api,
        &resource,
        "GET",
        "200",
        json!({ "responseTemplates": { "application/json": GET_BODY } }),
    )
    .await;

    let deployed = gateway
        .call(
            "POST",
            &format!("/restapis/{api}/deployments"),
            Some(json!({ "stageName": CORS_STAGE })),
        )
        .await;
    assert_eq!(
        deployed.status, 201,
        "case CreateDeployment: expected 201, got {} with {:?}",
        deployed.status, deployed.body
    );
    api
}

/// The `responseParameters` key one response header is declared and mapped under:
/// `method.response.header.<Name>`, with the header name in the spelling a browser reads it
/// by.
fn method_response_header(lowercased: &str) -> String {
    let name: String = lowercased
        .split('-')
        .map(|word| {
            let mut characters = word.chars();
            match characters.next() {
                Some(first) => format!("{}{}", first.to_ascii_uppercase(), characters.as_str()),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join("-");
    format!("method.response.header.{name}")
}
