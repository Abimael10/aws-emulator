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
//! What the method behind such a request may PERFORM is the second test's rule: a declaration
//! naming THAT SITE has its integration performed, so a Lambda proxy behind it runs and the
//! answer is the handler's own — with the `Origin` in the event it was handed and the
//! `Access-Control-*` headers the handler itself set — and so does a request whose credential
//! the stage will AUTHENTICATE, whichever site it names, because `Origin` is no authorization
//! input of a method.
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
//! every other suite here drives and whose answers this change leaves untouched. The second test
//! spans what the rule generalises over for a method whose integration names a BACKEND: whose
//! declaration names the site (the preflight's, the wildcard's, none), the origin the page was
//! loaded from, the `authorizationType` the method declares, and whether the request carries a
//! SigV4 credential.

use base64::Engine as _;
use bytes::Bytes;
use serde_json::json;
use smithy_server_runtime::MultiServiceRouter;

use crate::execute_api_data_plane::{integration_response, method_response, put_integration,
                                    put_method};
use crate::proxy_binary_media_types::{
    apigw, authorization, dispatch, emulator, lambda, member, package, wait_until_active,
    FUNCTION_TIMEOUT, HANDLER, ROLE,
};
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
            case: "a preflight from an origin that declaration does not name, which the api's own \
                   records answer with the declared value all the same",
            verb: "OPTIONS",
            path: "/named",
            headers: other_page,
            served: Some((200, "")),
            mapped: named_preflight,
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

// ---------------------------------------------------------------------------------------------
// The request that FOLLOWS the preflight, on a method whose integration names a BACKEND: what a
// browser-based client of a CORS-configured api actually sends once the preflight has answered.
// ---------------------------------------------------------------------------------------------

/// The resource whose `AWS_IAM` `GET` proxies to the function, and whose `MOCK` `OPTIONS`
/// carries the CORS declaration — the shape AWS's own "enable CORS" leaves on a REST resource.
const PROXIED_PATH: &str = "/items";

/// A second resource with the same proxy method and NO declaration of any kind: nothing of the
/// api says its answers are for any page.
const UNDECLARED_PATH: &str = "/private";

/// A third resource whose declaration names EVERY origin — what AWS's own console "enable CORS"
/// writes by default — in front of a proxy method that authenticates nobody: the shape a page
/// from any site could otherwise drive.
const OPEN_PATH: &str = "/open";

/// The function the proxied methods are integrated with.
const PROXY_FUNCTION: &str = "cross-origin-proxy-handler";

/// What the handler puts in its body and in a header of its own, so an answer that came from it
/// is told from one the api's records rendered.
const HANDLER_MARKER: &str = "cross-origin-proxy";

/// The header the handler asks the browser to expose, which is a header only the handler sets.
const EXPOSED_HEADER: &str = "x-app-marker";

/// What the handler reports as the origin it saw when the event carried none, which is what a
/// request from a client that sets no browser header must be answered.
const NO_ORIGIN_SEEN: &str = "none";

/// The handler every proxied method runs: it reports the `Origin` THE EVENT CARRIED and sets its
/// own `Access-Control-*` headers from it, which is what a Lambda proxy handler of a
/// CORS-configured api does — the api's records declare none of these headers for this method,
/// so an answer carrying them can only have come from the handler, and one naming the request's
/// own origin can only have come from an event that carried it.
const PROXY_HANDLER_SOURCE: &str = r#"
import json


def lambda_handler(event, context):
    headers = event.get("headers") or {}
    seen = headers.get("origin") or headers.get("Origin")
    return {
        "statusCode": 200,
        "headers": {
            "Content-Type": "application/json",
            "Access-Control-Allow-Origin": seen or "none",
            "Access-Control-Expose-Headers": "x-app-marker",
            "x-app-marker": "cross-origin-proxy",
        },
        "body": json.dumps({"marker": "cross-origin-proxy", "seen": {"origin": seen}}),
    }
"#;

/// What the api's records owe one row.
#[derive(Clone, Copy, Debug)]
enum Answered {
    /// The method's own integration answered: the handler's `200`, the origin the event carried
    /// and the `Access-Control-*` headers the handler itself set.
    ByTheHandler,
    /// The api's own records answered the preflight: `200`, an empty body and the declared
    /// header mappings, whose `Access-Control-Allow-Origin` is this literal.
    ByTheDeclaration(&'static str),
    /// The STAGE refused it itself, through the api's own gateway response: this status and this
    /// sentence under the `message` member, with no `Access-Control-*` header.
    ByTheStage(u16, &'static str),
    /// Nothing of the api answered it: the front door's refusal, with no answer of the api's own
    /// and no `Access-Control-*` header.
    NotServed,
}

/// One request a browser (or, where it carries no browser header, an SDK client) sends to the
/// deployed stage.
#[derive(Clone, Copy, Debug)]
struct Proxied {
    case: &'static str,
    verb: &'static str,
    path: &'static str,
    /// The origin the page was loaded from, or `None` for a client that sets no browser header
    /// at all.
    origin: Option<&'static str>,
    /// Whether the request carries a SigV4 credential, which is the one authorization input an
    /// `AWS_IAM` method has.
    signed: bool,
    answered: Answered,
}

/// What the stage answers a request on an `AWS_IAM` method that carries no credential at all.
const MISSING_TOKEN: &str = "Missing Authentication Token";

/// RULE: a cross-origin request to a resource whose own deployed records NAME the site the page
/// was loaded from is answered by the METHOD'S OWN INTEGRATION, exactly as the same request from
/// any other client is — the `Origin` is no authorization input of the method, it travels to the
/// integration like every other header, and the `Access-Control-*` headers the browser reads are
/// the ones the integration's own answer carries; whether those name the page is the browser's
/// decision, not the endpoint's. What decides authorization is the method's `authorizationType`:
/// an `AWS_IAM` method admits a request that carries a SigV4 credential and refuses one that
/// carries none, with the sentence that refusal has, whichever origin the page was loaded from.
/// A declaration naming EVERY origin names no site, so the integration behind such a method is
/// not performed for a page at all, and neither is one of a resource that declares nothing; a
/// request whose credential the stage AUTHENTICATES is performed whichever site it names, which
/// this emulator does only where it accepts access keys at all.
///
/// The rows span what that rule generalises over: what the resource declares and on which method
/// (the page's own origin on the preflight, which is where AWS's own "enable CORS" puts it, the
/// wildcard, and nothing at all), the origin the page was loaded from (one the declaration names
/// and one it does not), the verb (the one the declaration lists, the preflight itself, and a
/// deployed verb it does not list — which the endpoint does not read either), the
/// `authorizationType` the method behind it declares (`AWS_IAM` and `NONE`), whether the request
/// carries a credential, and the same requests from a client that sets no browser header, which
/// is what every other suite of this crate drives.
///
/// This emulator accepts no access key — request authentication is off, as in every other suite
/// of this crate — so no row here is served on the strength of the credential it carries: every
/// cross-origin row that IS served is served because the records name its site.
/// The signed call from a client holding this emulator's own credentials, which is served
/// whichever site it names, is pinned against an emulator that verifies signatures in
/// `core/router/tests/all/apigateway_rest_cors_signed_cross_origin_invoke.rs`.
///
/// Driven on a MULTI-THREADED runtime, as the assembled emulator serves a request: the stage's
/// request awaits an invocation this same process answers.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cross_origin_request_is_answered_by_its_methods_own_integration() {
    let router = emulator();
    let function_arn = given_proxy_function(&router).await;
    let api = deployed_proxy_api(&router, &function_arn).await;

    let rows = [
        Proxied {
            case: "a signed cross-origin GET to the resource the preflight's declaration is on",
            verb: "GET",
            path: PROXIED_PATH,
            origin: Some(PAGE_ORIGIN),
            signed: true,
            answered: Answered::ByTheHandler,
        },
        Proxied {
            case: "the same request from a client that sets no browser header",
            verb: "GET",
            path: PROXIED_PATH,
            origin: None,
            signed: true,
            answered: Answered::ByTheHandler,
        },
        Proxied {
            case: "the same cross-origin GET carrying no credential at all",
            verb: "GET",
            path: PROXIED_PATH,
            origin: Some(PAGE_ORIGIN),
            signed: false,
            answered: Answered::ByTheStage(REFUSED_STATUS, MISSING_TOKEN),
        },
        Proxied {
            case: "the same unsigned GET from a client that sets no browser header",
            verb: "GET",
            path: PROXIED_PATH,
            origin: None,
            signed: false,
            answered: Answered::ByTheStage(REFUSED_STATUS, MISSING_TOKEN),
        },
        Proxied {
            case: "the preflight the browser sends before that GET",
            verb: "OPTIONS",
            path: PROXIED_PATH,
            origin: Some(PAGE_ORIGIN),
            signed: false,
            answered: Answered::ByTheDeclaration(PAGE_ORIGIN),
        },
        Proxied {
            case: "a signed cross-origin GET from an origin the declaration does not name, which \
                   this emulator cannot tell from a page because it verifies no signature",
            verb: "GET",
            path: PROXIED_PATH,
            origin: Some(OTHER_ORIGIN),
            signed: true,
            answered: Answered::NotServed,
        },
        Proxied {
            case: "the preflight from that same origin, answered the declared value whoever \
                   asked",
            verb: "OPTIONS",
            path: PROXIED_PATH,
            origin: Some(OTHER_ORIGIN),
            signed: false,
            answered: Answered::ByTheDeclaration(PAGE_ORIGIN),
        },
        Proxied {
            case: "a signed cross-origin DELETE, a deployed verb the preflight's declaration \
                   does not list — which the endpoint does not read either, the browser being \
                   what reads a preflight's method list",
            verb: "DELETE",
            path: PROXIED_PATH,
            origin: Some(PAGE_ORIGIN),
            signed: true,
            answered: Answered::ByTheHandler,
        },
        Proxied {
            case: "the same DELETE from a client that sets no browser header",
            verb: "DELETE",
            path: PROXIED_PATH,
            origin: None,
            signed: true,
            answered: Answered::ByTheHandler,
        },
        Proxied {
            case: "a signed cross-origin GET to a resource that declares nothing",
            verb: "GET",
            path: UNDECLARED_PATH,
            origin: Some(PAGE_ORIGIN),
            signed: true,
            answered: Answered::NotServed,
        },
        Proxied {
            case: "the same request to that resource with no browser header",
            verb: "GET",
            path: UNDECLARED_PATH,
            origin: None,
            signed: true,
            answered: Answered::ByTheHandler,
        },
        // The resource whose declaration names EVERY origin, in front of a proxy method that
        // authenticates nobody: a page from any site may READ what such a resource answers, which
        // is what the wildcard says, but naming every origin names no site — so nothing of the
        // api says a page from a site the developer never named may make this emulator PERFORM
        // the call behind it, and it is not served.
        Proxied {
            case: "a cross-origin GET to a wildcard-declared proxy method that authenticates \
                   nobody",
            verb: "GET",
            path: OPEN_PATH,
            origin: Some(PAGE_ORIGIN),
            signed: false,
            answered: Answered::NotServed,
        },
        Proxied {
            case: "the same from another site again",
            verb: "GET",
            path: OPEN_PATH,
            origin: Some(OTHER_ORIGIN),
            signed: false,
            answered: Answered::NotServed,
        },
        Proxied {
            case: "the preflight of that resource, answered the wildcard it declares",
            verb: "OPTIONS",
            path: OPEN_PATH,
            origin: Some(OTHER_ORIGIN),
            signed: false,
            answered: Answered::ByTheDeclaration(EVERY_ORIGIN),
        },
        Proxied {
            case: "the same GET from a client that sets no browser header, which is unaffected",
            verb: "GET",
            path: OPEN_PATH,
            origin: None,
            signed: false,
            answered: Answered::ByTheHandler,
        },
    ];

    for row in rows {
        let case = row.case;
        let mut headers: Vec<(&str, &str)> = Vec::new();
        if let Some(origin) = row.origin {
            headers.push(("origin", origin));
            headers.push(("sec-fetch-site", "cross-site"));
        }
        let credential = authorization("execute-api");
        if row.signed {
            headers.push(("authorization", credential.as_str()));
        }
        let answer = dispatch(
            &router,
            row.verb,
            &format!("{api}.execute-api.{REGION}.amazonaws.com"),
            &format!("/{STAGE}{}", row.path),
            &headers,
            Bytes::new(),
        )
        .await;
        let body = answer.text();
        let allow_origin = answer.header("access-control-allow-origin");
        match row.answered {
            Answered::ByTheHandler => {
                assert_eq!(
                    answer.status, 200,
                    "case {case:?}: expected the handler's own 200, got {} with {body:?}",
                    answer.status
                );
                let reported = answer.json();
                assert_eq!(
                    reported.get("marker").and_then(serde_json::Value::as_str),
                    Some(HANDLER_MARKER),
                    "case {case:?}: expected the handler's own body, got {body:?}"
                );
                let seen = reported
                    .get("seen")
                    .and_then(|seen| seen.get("origin"))
                    .and_then(serde_json::Value::as_str);
                assert_eq!(
                    seen, row.origin,
                    "case {case:?}: expected the event to carry the request's own Origin \
                     {:?}, got {seen:?}",
                    row.origin
                );
                assert_eq!(
                    allow_origin.as_deref(),
                    Some(row.origin.unwrap_or(NO_ORIGIN_SEEN)),
                    "case {case:?}: expected the handler's own Access-Control-Allow-Origin, got \
                     {allow_origin:?}"
                );
                assert_eq!(
                    answer.header("access-control-expose-headers").as_deref(),
                    Some(EXPOSED_HEADER),
                    "case {case:?}: expected the handler's own Access-Control-Expose-Headers, \
                     got {:?}",
                    answer.header("access-control-expose-headers")
                );
                assert_eq!(
                    answer.header(EXPOSED_HEADER).as_deref(),
                    Some(HANDLER_MARKER),
                    "case {case:?}: expected the header the handler set, got {:?}",
                    answer.header(EXPOSED_HEADER)
                );
            }
            Answered::ByTheDeclaration(declared_origin) => {
                assert_eq!(
                    (answer.status, body.as_str()),
                    (200, ""),
                    "case {case:?}: expected the declared preflight answer, got {} with \
                     {body:?}",
                    answer.status
                );
                for (name, expected) in [
                    ("access-control-allow-origin", declared_origin),
                    ("access-control-allow-methods", PROXIED_METHODS),
                    ("access-control-allow-headers", DECLARED_HEADERS),
                ] {
                    let actual = answer.header(name);
                    assert_eq!(
                        actual.as_deref(),
                        Some(expected),
                        "case {case:?}: expected the declared {name} {expected:?}, got \
                         {actual:?}"
                    );
                }
            }
            Answered::ByTheStage(status, sentence) => {
                assert_eq!(
                    answer.status, status,
                    "case {case:?}: expected the stage's own {status}, got {} with {body:?}",
                    answer.status
                );
                assert_eq!(
                    answer.json().get("message").and_then(serde_json::Value::as_str),
                    Some(sentence),
                    "case {case:?}: expected the sentence {sentence:?}, got {body:?}"
                );
                assert!(
                    !body.contains(HANDLER_MARKER),
                    "case {case:?}: expected the integration not to have been performed, got \
                     {body:?}"
                );
                assert_eq!(
                    allow_origin, None,
                    "case {case:?}: expected no Access-Control-Allow-Origin on the stage's own \
                     refusal, got {allow_origin:?}"
                );
            }
            Answered::NotServed => {
                assert_eq!(
                    answer.status, REFUSED_STATUS,
                    "case {case:?}: expected the stage not to serve it ({REFUSED_STATUS}), got \
                     {} with {body:?}",
                    answer.status
                );
                assert!(
                    !body.contains(HANDLER_MARKER) && !body.contains(MISSING_TOKEN),
                    "case {case:?}: expected no answer of the api's own, got {body:?}"
                );
                assert_eq!(
                    allow_origin, None,
                    "case {case:?}: expected no Access-Control-Allow-Origin, got \
                     {allow_origin:?}"
                );
            }
        }
    }
}

/// The method list the proxied resource's `OPTIONS` method declares: the verb the real request
/// uses and the preflight itself, which is what AWS's own "enable CORS" deploys — and NOT the
/// `DELETE` the resource also deploys, which is the verb no declaration of it grants.
const PROXIED_METHODS: &str = "GET,OPTIONS";

/// Create the function every proxied method is integrated with, grant API Gateway the
/// invocation, and answer its ARN once Lambda reports it runnable.
async fn given_proxy_function(gateway: &MultiServiceRouter) -> String {
    let created = lambda(
        gateway,
        "POST",
        "/2015-03-31/functions",
        Some(json!({
            "FunctionName": PROXY_FUNCTION,
            "Role": ROLE,
            "Runtime": "python3.12",
            "Handler": HANDLER,
            "Timeout": FUNCTION_TIMEOUT,
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD
                    .encode(package(PROXY_HANDLER_SOURCE)),
            },
        })),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "setup CreateFunction: expected HTTP 201 from Lambda, got {} with {:?}",
        created.status,
        created.text()
    );
    let arn = member("CreateFunction", &created.json(), "FunctionArn");
    wait_until_active(gateway, PROXY_FUNCTION).await;
    // The grant that admits the invocation, naming no `SourceArn`, so it admits the call from
    // every method of the api below.
    let granted = lambda(
        gateway,
        "POST",
        &format!("/2015-03-31/functions/{PROXY_FUNCTION}/policy"),
        Some(json!({
            "StatementId": "apigw-cross-origin",
            "Action": "lambda:InvokeFunction",
            "Principal": "apigateway.amazonaws.com",
        })),
    )
    .await;
    assert_eq!(
        granted.status,
        201,
        "setup AddPermission: expected HTTP 201 from Lambda, got {} with {:?}",
        granted.status,
        granted.text()
    );
    arn
}

/// Stand up and deploy the api the rows drive: one resource carrying the CORS contract AWS's own
/// "enable CORS" deploys — a `MOCK` `OPTIONS` whose declared mappings name the page's origin and
/// the verbs it grants, beside an `AWS_IAM` `GET` and a `DELETE` whose `AWS_PROXY` integrations
/// name the function and declare nothing of their own — a second resource with the same proxy
/// methods and no declaration at all, and a third whose declaration names EVERY origin in front
/// of a proxy `GET` that authenticates nobody. Answers the api id.
async fn deployed_proxy_api(gateway: &MultiServiceRouter, function_arn: &str) -> String {
    let created = apigw(
        gateway,
        "CreateRestApi",
        "POST",
        "/restapis",
        Some(json!({ "name": "cross-origin-proxy-invoke" })),
        201,
    )
    .await;
    let api = member("CreateRestApi", &created, "id");
    let root = member("CreateRestApi", &created, "rootResourceId");

    for (path_part, declared) in [
        (PROXIED_PATH.trim_start_matches('/'), Some(PAGE_ORIGIN)),
        (UNDECLARED_PATH.trim_start_matches('/'), None),
        (OPEN_PATH.trim_start_matches('/'), Some(EVERY_ORIGIN)),
    ] {
        let resource = apigw(
            gateway,
            "CreateResource",
            "POST",
            &format!("/restapis/{api}/resources/{root}"),
            Some(json!({ "pathPart": path_part })),
            201,
        )
        .await;
        let resource = member("CreateResource", &resource, "id");
        if path_part == OPEN_PATH.trim_start_matches('/') {
            // The wildcard-declared resource's one method authenticates NOBODY, which is the
            // shape a page from any site would otherwise be able to drive.
            proxy_method(gateway, &api, &resource, "GET", "NONE", function_arn).await;
        } else {
            // The method the browser's real request reaches: `AWS_IAM`, so the only
            // authorization input is the SigV4 credential, behind the proxy integration that
            // invokes the handler.
            proxy_method(gateway, &api, &resource, "GET", "AWS_IAM", function_arn).await;
            // A second deployed verb, so a request naming a verb the declaration does not name
            // has a method to reach.
            proxy_method(gateway, &api, &resource, "DELETE", "NONE", function_arn).await;
        }
        let Some(origin) = declared else {
            continue;
        };
        declared_preflight(gateway, &api, &resource, origin).await;
    }

    apigw(
        gateway,
        "CreateDeployment",
        "POST",
        &format!("/restapis/{api}/deployments"),
        Some(json!({ "stageName": STAGE })),
        201,
    )
    .await;
    api
}

/// `PutMethod` + `PutIntegration` for one `AWS_PROXY` method on `resource`, declaring no
/// response of its own: the handler behind it is what sets the headers its answer carries.
async fn proxy_method(
    gateway: &MultiServiceRouter,
    api: &str,
    resource: &str,
    verb: &str,
    authorization_type: &str,
    function_arn: &str,
) {
    let method = format!("/restapis/{api}/resources/{resource}/methods/{verb}");
    apigw(
        gateway,
        "PutMethod",
        "PUT",
        &method,
        Some(json!({ "authorizationType": authorization_type })),
        201,
    )
    .await;
    apigw(
        gateway,
        "PutIntegration",
        "PUT",
        &format!("{method}/integration"),
        Some(json!({
            "type": "AWS_PROXY",
            // `integrationHttpMethod` travels under its `jsonName`, which is what
            // `smithy_query(query="operation",operation="PutIntegration")` reports.
            "httpMethod": "POST",
            "uri": format!(
                "arn:aws:apigateway:{REGION}:lambda:path/2015-03-31/functions/\
                 {function_arn}/invocations"
            ),
        })),
        201,
    )
    .await;
}

/// The `MOCK` `OPTIONS` method a browser's preflight reaches, carrying the resource's CORS
/// declaration: `origin` as the origin its answers are for, and the methods and headers it
/// names, each mapped by the integration response and declared by the method response of the
/// same status.
async fn declared_preflight(
    gateway: &MultiServiceRouter,
    api: &str,
    resource: &str,
    origin: &str,
) {
    let method = format!("/restapis/{api}/resources/{resource}/methods/OPTIONS");
    apigw(
        gateway,
        "PutMethod(OPTIONS)",
        "PUT",
        &method,
        Some(json!({ "authorizationType": "NONE" })),
        201,
    )
    .await;
    apigw(
        gateway,
        "PutIntegration(OPTIONS)",
        "PUT",
        &format!("{method}/integration"),
        Some(json!({
            "type": "MOCK",
            "requestTemplates": { "application/json": "{\"statusCode\": 200}" },
        })),
        201,
    )
    .await;
    apigw(
        gateway,
        "PutMethodResponse(OPTIONS)",
        "PUT",
        &format!("{method}/responses/200"),
        Some(json!({
            "responseParameters": {
                "method.response.header.Access-Control-Allow-Origin": false,
                "method.response.header.Access-Control-Allow-Methods": false,
                "method.response.header.Access-Control-Allow-Headers": false,
            },
        })),
        201,
    )
    .await;
    apigw(
        gateway,
        "PutIntegrationResponse(OPTIONS)",
        "PUT",
        &format!("{method}/integration/responses/200"),
        Some(json!({
            "responseTemplates": { "application/json": "" },
            "responseParameters": {
                "method.response.header.Access-Control-Allow-Origin":
                    format!("'{origin}'"),
                "method.response.header.Access-Control-Allow-Methods":
                    format!("'{PROXIED_METHODS}'"),
                "method.response.header.Access-Control-Allow-Headers":
                    format!("'{DECLARED_HEADERS}'"),
            },
        })),
        201,
    )
    .await;
}
