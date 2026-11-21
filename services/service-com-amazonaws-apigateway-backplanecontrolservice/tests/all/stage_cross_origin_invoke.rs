// SPDX-License-Identifier: CC0-1.0

//! The cross-origin requests a BROWSER makes to a deployed stage: the `OPTIONS` preflight it
//! sends before a cross-origin call, and the real request that follows it.
//!
//! RULE under test: a deployed stage answers a request a browser sent from another site where
//! the RESOURCE it addresses DECLARES THAT ORIGIN in the api's own deployed records — a static
//! `Access-Control-Allow-Origin` mapping on a response one of that resource's methods serves,
//! naming the wildcard or that exact origin — and then answers it exactly as it answers any
//! other caller: the matched method's status, body and declared `Access-Control-*` header
//! mappings, each header carrying the value the integration response DECLARES rather than
//! anything taken from the request. It also answers a request no deployed method serves, which
//! the stage refuses itself. A request to a resource whose methods declare no such origin is not
//! served by the stage at all, and neither is one whose only declaration is the wildcard — which
//! names no site in particular — to a method whose integration names a backend.
//!
//! The rows span what that rule generalises over: what the resource deploys (a `MOCK` `OPTIONS`
//! and `GET` declaring the wildcard, the same declaring one named origin, a resource declaring
//! nothing, a resource declaring nothing whose method's integration names a backend, and a
//! resource declaring the WILDCARD whose method's integration names a backend), the
//! origin the page was loaded from
//! (one a declaration names and one it does not), how the request reads as another site's (an
//! `Origin` alone, as an older browser's preflight carries it, and an `Origin` beside a
//! browser's own `cross-site` classification), the verb (the preflight, the real request, and a
//! verb the resource deploys no method for), and the addressing the client reaches the stage by
//! (the api's own `execute-api` host and the path-style address this emulator announces on its
//! own endpoint) — beside the same calls made with no browser header at all, which is the client
//! every other suite here drives and whose answers this change leaves untouched.

use bytes::Bytes;
use serde_json::json;

use crate::execute_api_data_plane::{integration_response, method_response, put_integration,
                                    put_method};
use crate::rest_api_lifecycle::{create_api, create_resource, string_member, Gateway};

/// The region the api's own invoke host is addressed in; any region is served.
const REGION: &str = "us-east-1";

/// The stage the api is deployed to.
const STAGE: &str = "live";

/// The origin the page under test was loaded from.
const PAGE_ORIGIN: &str = "https://app.example";

/// Another origin, which only the wildcard declaration names.
const OTHER_ORIGIN: &str = "https://other.example";

/// The `Access-Control-Allow-Origin` the `/any` resource's responses declare: the wildcard,
/// which names every origin. Served VERBATIM, so an answer that echoed the request's own
/// `Origin` instead of the declared mapping fails every `/any` case below.
const EVERY_ORIGIN: &str = "*";

/// The method list the `OPTIONS` methods' integration responses declare.
const DECLARED_METHODS: &str = "GET,OPTIONS";

/// The header list the `OPTIONS` methods' integration responses declare.
const DECLARED_HEADERS: &str = "Content-Type,X-Probe";

/// The body each resource's `MOCK` `GET` serves, from its own response template.
fn body_of(resource: &str) -> String {
    format!("{{\"resource\":\"{resource}\"}}")
}

/// What API Gateway answers a request to a resource path whose method is not deployed:
/// `403` `{"message":"Missing Authentication Token"}`.
const NO_SUCH_METHOD_BODY: &str = "{\"message\":\"Missing Authentication Token\"}";

/// What the emulator's front door answers a request a browser sent from another site that the
/// stage does not serve: `403`, and no answer of the api's own.
const REFUSED_STATUS: u16 = 403;

/// What API Gateway answers a call whose integration it could not perform, which is what the
/// `/wildbackend` resource's HTTP backend is for a client the stage DOES serve: this emulator
/// opens no connection to an upstream of its own.
const CONFIGURATION_ERROR_STATUS: u16 = 500;
const CONFIGURATION_ERROR_BODY: &str = "{\"message\":\"Internal server error\"}";

/// One request this suite sends to the stage.
struct Call<'c> {
    /// The case name every assertion names.
    case: &'c str,
    /// The verb.
    verb: &'c str,
    /// The resource path within the stage.
    path: &'c str,
    /// The headers the client sends beside the host.
    headers: &'c [(&'c str, &'c str)],
    /// The status the stage must answer, and the body it must answer verbatim, for a request
    /// the stage serves; `None` for a request it does not serve, which must be refused
    /// [`REFUSED_STATUS`] with no `Access-Control-*` header and none of the api's own bodies.
    served: Option<(u16, &'c str)>,
    /// The `Access-Control-*` headers the answer must carry, by lowercased name; exactly these
    /// and no others.
    mapped: &'c [(&'c str, &'c str)],
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stage_answers_the_cross_origin_requests_its_records_declare() {
    let gateway = Gateway::new();
    let api = deployed_api(&gateway).await;

    // The three ways a request reads as another site's, beside the client that sets no browser
    // header at all.
    let page: &[(&str, &str)] = &[("origin", PAGE_ORIGIN)];
    let page_cross_site: &[(&str, &str)] = &[
        ("origin", PAGE_ORIGIN),
        ("sec-fetch-site", "cross-site"),
        ("access-control-request-method", "GET"),
        ("access-control-request-headers", "x-probe"),
    ];
    let other_page: &[(&str, &str)] = &[
        ("origin", OTHER_ORIGIN),
        ("sec-fetch-site", "cross-site"),
        ("access-control-request-method", "GET"),
    ];
    let no_browser_headers: &[(&str, &str)] = &[];

    let any_preflight: &[(&str, &str)] = &[
        ("access-control-allow-origin", EVERY_ORIGIN),
        ("access-control-allow-methods", DECLARED_METHODS),
        ("access-control-allow-headers", DECLARED_HEADERS),
    ];
    let any_get: &[(&str, &str)] = &[("access-control-allow-origin", EVERY_ORIGIN)];
    let named_preflight: &[(&str, &str)] = &[
        ("access-control-allow-origin", PAGE_ORIGIN),
        ("access-control-allow-methods", DECLARED_METHODS),
        ("access-control-allow-headers", DECLARED_HEADERS),
    ];
    let named_get: &[(&str, &str)] = &[("access-control-allow-origin", PAGE_ORIGIN)];

    let any_body = body_of("any");
    let named_body = body_of("named");
    let nocors_body = body_of("nocors");
    let calls = [
        Call {
            case: "a preflight carrying an Origin alone, to a wildcard declaration",
            verb: "OPTIONS",
            path: "/any",
            headers: page,
            served: Some((200, "")),
            mapped: any_preflight,
        },
        Call {
            case: "a preflight a browser classified cross-site",
            verb: "OPTIONS",
            path: "/any",
            headers: page_cross_site,
            served: Some((200, "")),
            mapped: any_preflight,
        },
        Call {
            case: "a preflight from another origin, which the wildcard also names",
            verb: "OPTIONS",
            path: "/any",
            headers: other_page,
            served: Some((200, "")),
            mapped: any_preflight,
        },
        Call {
            case: "the real cross-origin request the preflight was for",
            verb: "GET",
            path: "/any",
            headers: page_cross_site,
            served: Some((200, &any_body)),
            mapped: any_get,
        },
        Call {
            case: "a preflight from a client that sets no browser header",
            verb: "OPTIONS",
            path: "/any",
            headers: no_browser_headers,
            served: Some((200, "")),
            mapped: any_preflight,
        },
        Call {
            case: "the same request with no browser header",
            verb: "GET",
            path: "/any",
            headers: no_browser_headers,
            served: Some((200, &any_body)),
            mapped: any_get,
        },
        Call {
            case: "a cross-site verb the resource deploys no method for",
            verb: "DELETE",
            path: "/any",
            headers: page_cross_site,
            served: Some((REFUSED_STATUS, NO_SUCH_METHOD_BODY)),
            mapped: &[],
        },
        Call {
            case: "a preflight from the origin a named declaration names",
            verb: "OPTIONS",
            path: "/named",
            headers: page_cross_site,
            served: Some((200, "")),
            mapped: named_preflight,
        },
        Call {
            case: "a preflight from an origin that declaration does not name",
            verb: "OPTIONS",
            path: "/named",
            headers: other_page,
            served: None,
            mapped: &[],
        },
        Call {
            case: "a cross-origin request from the origin the named declaration names",
            verb: "GET",
            path: "/named",
            headers: page_cross_site,
            served: Some((200, &named_body)),
            mapped: named_get,
        },
        Call {
            case: "a preflight to a resource that declares nothing and deploys no OPTIONS \
                   method",
            verb: "OPTIONS",
            path: "/nocors",
            headers: page_cross_site,
            served: Some((REFUSED_STATUS, NO_SUCH_METHOD_BODY)),
            mapped: &[],
        },
        Call {
            case: "a cross-origin request to that resource's own deployed method, which \
                   declares no origin",
            verb: "GET",
            path: "/nocors",
            headers: page_cross_site,
            served: None,
            mapped: &[],
        },
        Call {
            case: "the same request from a client that sets no browser header",
            verb: "GET",
            path: "/nocors",
            headers: no_browser_headers,
            served: Some((200, &nocors_body)),
            mapped: &[],
        },
        Call {
            case: "an OPTIONS with no browser header, to a resource carrying no OPTIONS method",
            verb: "OPTIONS",
            path: "/nocors",
            headers: no_browser_headers,
            served: Some((REFUSED_STATUS, NO_SUCH_METHOD_BODY)),
            mapped: &[],
        },
        Call {
            case: "a cross-origin request to a resource declaring nothing whose integration \
                   names a backend",
            verb: "GET",
            path: "/backend",
            headers: page_cross_site,
            served: None,
            mapped: &[],
        },
        Call {
            case: "a cross-origin request admitted only by the wildcard to a method whose \
                   integration names a backend",
            verb: "GET",
            path: "/wildbackend",
            headers: page_cross_site,
            served: None,
            mapped: &[],
        },
        Call {
            case: "the same request from a client that sets no browser header",
            verb: "GET",
            path: "/wildbackend",
            headers: no_browser_headers,
            served: Some((CONFIGURATION_ERROR_STATUS, CONFIGURATION_ERROR_BODY)),
            mapped: &[],
        },
    ];

    // Both addressings a client reaches a deployed stage by: the api's own `execute-api` host,
    // and the path-style address this emulator announces on its own endpoint.
    let own_host = format!("{api}.execute-api.{REGION}.amazonaws.com");
    let emulator_endpoint = "127.0.0.1:39461";
    let addressings: [(&str, &str, String); 2] = [
        (
            "the api's own execute-api host",
            &own_host,
            format!("https://{own_host}/{STAGE}"),
        ),
        (
            "the emulator's own endpoint",
            emulator_endpoint,
            format!("https://{emulator_endpoint}/_aws/execute-api/{api}/{STAGE}"),
        ),
    ];

    for (addressing, host, base) in addressings {
        for call in &calls {
            let case = format!("{addressing}: {}", call.case);
            let mut builder = http::Request::builder()
                .method(call.verb)
                .uri(format!("{base}{}", call.path))
                .header("host", host);
            for (name, value) in call.headers {
                builder = builder.header(*name, *value);
            }
            let request = builder
                .body(Bytes::new())
                .expect("the test request is well formed");
            let response = gateway.send(request).await;
            let status = response.status().as_u16();
            let headers = response.headers().clone();
            let body = String::from_utf8_lossy(&response.into_body()).into_owned();
            match call.served {
                Some((expected_status, expected_body)) => {
                    assert_eq!(
                        status, expected_status,
                        "case {case:?}: expected status {expected_status}, got {status} with \
                         {body:?} and {headers:?}"
                    );
                    assert_eq!(
                        body, expected_body,
                        "case {case:?}: expected the body {expected_body:?}, got {body:?}"
                    );
                }
                None => {
                    assert_eq!(
                        status, REFUSED_STATUS,
                        "case {case:?}: expected the stage not to serve it ({REFUSED_STATUS}), \
                         got {status} with {body:?} and {headers:?}"
                    );
                    for answer in [
                        any_body.as_str(),
                        named_body.as_str(),
                        nocors_body.as_str(),
                        NO_SUCH_METHOD_BODY,
                    ] {
                        assert!(
                            !body.contains(answer),
                            "case {case:?}: expected no answer of the api's own, got {body:?}"
                        );
                    }
                }
            }
            for (name, expected) in call.mapped {
                let actual = headers.get(*name).and_then(|value| value.to_str().ok());
                assert_eq!(
                    actual,
                    Some(*expected),
                    "case {case:?}: expected the declared {name} {expected:?}, got {actual:?}"
                );
            }
            let mut served: Vec<String> = headers
                .keys()
                .map(|name| name.as_str().to_string())
                .filter(|name| name.starts_with("access-control-"))
                .collect();
            served.sort();
            let mut declared: Vec<String> = call
                .mapped
                .iter()
                .map(|(name, _)| (*name).to_string())
                .collect();
            declared.sort();
            assert_eq!(
                served, declared,
                "case {case:?}: expected exactly the declared Access-Control-* headers \
                 {declared:?}, got {served:?}"
            );
        }
    }
}

/// Stand up and deploy the api every case above drives: three `MOCK` resources differing in the
/// `Access-Control-Allow-Origin` their responses declare — the wildcard on `/any`, one named
/// origin on `/named`, none at all on `/nocors` — and a `/backend` resource whose method's
/// integration names a backend rather than answering from the records. Answers the api id.
async fn deployed_api(gateway: &Gateway) -> String {
    let (api, root) = create_api(gateway, "cross-origin-invoke").await;
    for (resource, declared) in [
        ("any", Some(EVERY_ORIGIN)),
        ("named", Some(PAGE_ORIGIN)),
        ("nocors", None),
    ] {
        let created = create_resource(gateway, &api, &root, resource).await;
        let created = string_member(&created, "id", "CreateResource");
        cors_resource(gateway, &api, &created, resource, declared).await;
    }
    backend_resource(gateway, &api, &root).await;
    wildcard_backend_resource(gateway, &api, &root).await;

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
    api
}

/// One resource's methods: a `MOCK` `GET` serving its own body, and — when the resource
/// declares an origin — a `MOCK` `OPTIONS` of the shape a CORS preflight is answered by, each
/// method response declaring the headers its integration response maps.
///
/// `declared` is the `Access-Control-Allow-Origin` literal the two methods map, or `None` for a
/// resource that declares no origin and deploys no `OPTIONS` method, which is a resource whose
/// CORS contract was never configured.
async fn cors_resource(
    gateway: &Gateway,
    api: &str,
    resource: &str,
    name: &str,
    declared: Option<&str>,
) {
    mock_method(gateway, api, resource, "GET").await;
    let Some(origin) = declared else {
        method_response(gateway, api, resource, "GET", "200", json!({})).await;
        integration_response(
            gateway,
            api,
            resource,
            "GET",
            "200",
            json!({ "responseTemplates": { "application/json": body_of(name) } }),
        )
        .await;
        return;
    };
    method_response(
        gateway,
        api,
        resource,
        "GET",
        "200",
        json!({
            "responseParameters": {
                "method.response.header.Access-Control-Allow-Origin": false,
            },
        }),
    )
    .await;
    integration_response(
        gateway,
        api,
        resource,
        "GET",
        "200",
        json!({
            "responseTemplates": { "application/json": body_of(name) },
            "responseParameters": {
                "method.response.header.Access-Control-Allow-Origin": format!("'{origin}'"),
            },
        }),
    )
    .await;

    // The `OPTIONS` method a browser's preflight reaches: a MOCK integration that selects the
    // 200 integration response, whose empty response template is the empty body AWS serves,
    // and whose three declared header mappings are the resource's CORS contract.
    mock_method(gateway, api, resource, "OPTIONS").await;
    method_response(
        gateway,
        api,
        resource,
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
        gateway,
        api,
        resource,
        "OPTIONS",
        "200",
        json!({
            "responseTemplates": { "application/json": "" },
            "responseParameters": {
                "method.response.header.Access-Control-Allow-Origin": format!("'{origin}'"),
                "method.response.header.Access-Control-Allow-Methods":
                    format!("'{DECLARED_METHODS}'"),
                "method.response.header.Access-Control-Allow-Headers":
                    format!("'{DECLARED_HEADERS}'"),
            },
        }),
    )
    .await;
}

/// The `/backend` resource: one unauthorized `GET` whose integration names an HTTP backend and
/// which declares no origin at all, so it is the resource a cross-origin request is NOT served
/// for.
async fn backend_resource(gateway: &Gateway, api: &str, root: &str) {
    let created = create_resource(gateway, api, root, "backend").await;
    let created = string_member(&created, "id", "CreateResource(backend)");
    put_method(
        gateway,
        api,
        &created,
        "GET",
        json!({ "authorizationType": "NONE" }),
    )
    .await;
    put_integration(
        gateway,
        api,
        &created,
        "GET",
        json!({
            "type": "HTTP_PROXY",
            "httpMethod": "GET",
            "uri": "https://backend.example/items",
        }),
    )
    .await;
}

/// The `/wildbackend` resource: one unauthorized `GET` whose integration names an HTTP backend
/// and whose responses declare the WILDCARD origin. A declaration naming every origin names no
/// site in particular, so it admits only what the api's own records answer by themselves: this
/// resource is the one a cross-origin request is NOT served for even though it declares an
/// origin that covers the page.
async fn wildcard_backend_resource(gateway: &Gateway, api: &str, root: &str) {
    let created = create_resource(gateway, api, root, "wildbackend").await;
    let created = string_member(&created, "id", "CreateResource(wildbackend)");
    put_method(
        gateway,
        api,
        &created,
        "GET",
        json!({ "authorizationType": "NONE" }),
    )
    .await;
    put_integration(
        gateway,
        api,
        &created,
        "GET",
        json!({
            "type": "HTTP",
            "httpMethod": "GET",
            "uri": "https://backend.example/items",
        }),
    )
    .await;
    method_response(
        gateway,
        api,
        &created,
        "GET",
        "200",
        json!({
            "responseParameters": {
                "method.response.header.Access-Control-Allow-Origin": false,
            },
        }),
    )
    .await;
    integration_response(
        gateway,
        api,
        &created,
        "GET",
        "200",
        json!({
            "responseParameters": {
                "method.response.header.Access-Control-Allow-Origin": format!("'{EVERY_ORIGIN}'"),
            },
        }),
    )
    .await;
}

/// `PutMethod` + `PutIntegration` for one unauthorized `MOCK` method, the integration a CORS
/// method is wired to: the request template selects the 200 integration response, whose own
/// template is the body served.
async fn mock_method(gateway: &Gateway, api: &str, resource: &str, verb: &str) {
    put_method(
        gateway,
        api,
        resource,
        verb,
        json!({ "authorizationType": "NONE" }),
    )
    .await;
    put_integration(
        gateway,
        api,
        resource,
        verb,
        json!({
            "type": "MOCK",
            "requestTemplates": { "application/json": "{\"statusCode\": 200}" },
        }),
    )
    .await;
}
