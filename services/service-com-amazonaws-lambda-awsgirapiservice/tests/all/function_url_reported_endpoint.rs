// SPDX-License-Identifier: CC0-1.0

//! WHICH endpoint a function URL invocation tells its handler about, driven end to end over
//! this service's own data plane through the production `MultiServiceRouter`.
//!
//! RULE under test: the endpoint the payload-format-2.0 event reports — `headers.host` and
//! `requestContext.domainName` beside it — is the ADDRESS this service announced for that
//! function URL: "the HTTP URL endpoint for your function"
//! (`smithy_query(query="doc",shape="FunctionUrlConfig",member="FunctionUrl")`), whole, with
//! the port that address carries. A handler reconstructs its own URL from that value,
//! enforces a host allow-list with it and signs callbacks from it, so what it names has to be
//! the endpoint a client was handed and dialed — AWS's own announced host carries no port, so
//! the authority IS the host there, while this emulator announces the same resource at the
//! `host:port` it answers on and a value with that port dropped names an address nothing can
//! reach.
//!
//! The rows differ along what the rule generalises over: the address the URL is PUBLISHED at
//! (the emulator's own loopback endpoint, which carries a port, and the documented AWS host,
//! which carries none), the addressing a client reached it BY (the published authority itself
//! and the local path-style spelling of the same endpoint), and the path within the URL. The
//! last assertion of each row RE-DIALS the endpoint the handler was told about, which is what
//! the value is for: an address a handler composes has to be one a request reaches that same
//! function URL at.
//!
//! The harness (`Endpoint`, `given_function`, `given_configured_url_at`, `grant`, `router_on`,
//! `send`, `statement`) is the one `function_url_data_plane` builds; `tests/all/main.rs` is
//! this binary's crate root, so it is reached through `crate::function_url_data_plane`.

use serde_json::Value;
use smithy_server_runtime::MultiServiceRouter;

use crate::function_url_data_plane::{
    given_configured_url_at, given_function, grant, router_on, send, statement, Endpoint,
};

/// The control-plane endpoint of this service, which a client that resolves AWS's own hosts
/// reaches it at, as the shared harness addresses it.
const SERVICE_HOST: &str = "lambda.us-east-1.amazonaws.com";

/// The emulator's own loopback endpoint, which a client that resolves no AWS hostname is
/// configured with — and which a URL created through it is therefore published at.
const OWN_ENDPOINT: &str = "127.0.0.1:9297";

/// The handler every row runs: it answers the two members the event reports the endpoint in,
/// so every assertion below is about a document the RUN produced.
fn reporting_handler() -> String {
    r#"
import json


def lambda_handler(event, context):
    return {
        "statusCode": 200,
        "headers": {"content-type": "application/json"},
        "body": json.dumps(
            {
                "host": event["headers"].get("host"),
                "domainName": event["requestContext"]["domainName"],
                "rawPath": event["rawPath"],
            }
        ),
    }
"#
    .to_string()
}

/// Create a PUBLIC function URL on `function` through a request addressed at `host`, and
/// answer the endpoint it announces.
///
/// `AuthType=NONE` bypasses IAM authentication and the function's resource-based policy is
/// what admits the anonymous caller, so a public endpoint is that `AuthType` plus the grant to
/// the wildcard principal.
async fn given_public_url(router: &MultiServiceRouter, function: &str, host: &str) -> Endpoint {
    let endpoint =
        given_configured_url_at(router, host, function, None, "NONE", &Value::Null).await;
    grant(
        router,
        function,
        None,
        &statement("public-url", "lambda:InvokeFunctionUrl", "*", "NONE"),
    )
    .await;
    endpoint
}

/// The document one GET answered, through the addressing `addressed_host` / `uri` names.
async fn reported(
    router: &MultiServiceRouter,
    case: &str,
    addressed_host: &str,
    uri: &str,
) -> Value {
    let answer = send(router, "GET", addressed_host, uri, &[], &[]).await;
    assert_eq!(
        answer.status,
        200,
        "case {case:?}: the function URL must serve {uri:?} at {addressed_host:?}, got {}",
        answer.summary()
    );
    answer.json()
}

#[tokio::test]
async fn the_event_reports_the_endpoint_the_url_is_published_at_whole() {
    // (case, the endpoint the create was addressed at, the function's name)
    let rows: &[(&str, &str, &str)] = &[
        (
            "a URL published at the emulator's own endpoint, which carries a port",
            OWN_ENDPOINT,
            "url-endpoint-local",
        ),
        (
            "a URL published at the documented host, which carries none",
            SERVICE_HOST,
            "url-endpoint-documented",
        ),
    ];

    for (case, created_at, function) in rows {
        let engine = std::sync::Arc::new(engine::InMemoryEngine::new());
        let router = router_on(&engine);
        given_function(&router, function, &reporting_handler()).await;
        let endpoint = given_public_url(&router, function, created_at).await;
        let published = endpoint.announced_authority().to_string();

        // The two addressings of one endpoint: the published authority a consumer dials, and
        // the local path-style spelling a client that rewrote the announced URL carries.
        let within = "probe/one";
        let addressings: Vec<(String, String, String)> = vec![
            (
                format!("{case} reached at its published authority"),
                published.clone(),
                format!("{}{within}", endpoint.base_path),
            ),
            (
                format!("{case} reached by the local path-style spelling"),
                OWN_ENDPOINT.to_string(),
                format!("/_aws/lambda-url/{}/{within}", endpoint.url_id),
            ),
        ];
        for (addressed_case, host, uri) in addressings {
            let document = reported(&router, &addressed_case, &host, &uri).await;
            for member in ["host", "domainName"] {
                let value = document.get(member).and_then(Value::as_str);
                assert_eq!(
                    value,
                    Some(published.as_str()),
                    "case {addressed_case:?}: expected {member} to name the published endpoint \
                     {published:?}, got {value:?}"
                );
            }
        }

        // What the value is FOR: the handler composes its own URL from the endpoint it was
        // told about, and that address reaches this same function URL.
        let document = reported(
            &router,
            case,
            &published,
            &format!("{}composed", endpoint.base_path),
        )
        .await;
        let told = document
            .get("domainName")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("case {case:?}: the handler must report a domainName"))
            .to_string();
        let composed = reported(
            &router,
            &format!("{case}, re-dialed at the endpoint the handler was told about"),
            &told,
            &format!("{}composed/again", endpoint.base_path),
        )
        .await;
        assert_eq!(
            composed.get("domainName").and_then(Value::as_str),
            Some(published.as_str()),
            "case {case:?}: the address the handler composes must reach the same function URL, \
             got {composed}"
        );
    }
}
