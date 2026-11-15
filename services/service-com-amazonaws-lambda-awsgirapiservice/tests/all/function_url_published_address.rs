// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the ADDRESS a function URL is PUBLISHED at: a consumer handed
//! `FunctionUrlConfig$FunctionUrl` reaches the function by that address's AUTHORITY alone.
//!
//! The rule under test, one level above the case that filed the issue (a CloudFront
//! distribution whose `Origin$DomainName` was the host of a published function URL):
//!
//! > A function URL is published at an address that IDENTIFIES it — the `host:port` of the
//! > listener opened for that one URL — because a consumer of a published endpoint carries
//! > the HOST and supplies the request path itself: an origin's `DomainName`, an
//! > integration's host, an operator's `curl https://<host>/<path>`. So a request to that
//! > authority reaches the function the URL is attached to whatever path it carries, and the
//! > path it carries is the path WITHIN the URL that the handler is told about. An address
//! > published for one URL never serves another's, and an address whose configuration is
//! > gone is still this service's to answer: the caller is told it is not admitted, not that
//! > no service is registered there.
//!
//! The rows differ along what the rule generalises over: the path the consumer supplies (the
//! root, a nested path, a path carrying a query, a path spelled like the reserved
//! data-plane addressing of another URL, and one spelled like a bare url id), the method,
//! which of two published URLs on one engine is addressed, and the qualifier the URL is
//! attached to.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::function_url_data_plane::{
    control, given_configured_url, given_function, grant, router_on, send, Answer, Endpoint,
};

/// The handler every function below runs: it reports the path the event carried, so the
/// answer is the RUN's and it says which path within the URL the request reached.
fn reporting_handler(marker: &str) -> String {
    format!(
        r#"
def lambda_handler(event, context):
    return {{"marker": "{marker}", "reached": event["rawPath"], "query": event.get("rawQueryString", "")}}
"#
    )
}

/// The grant that makes an `AuthType=NONE` endpoint public, which is what admits the
/// credential-less caller a consumer of a published endpoint is.
async fn given_public_url(
    router: &MultiServiceRouter,
    function: &str,
    qualifier: Option<&str>,
) -> Endpoint {
    let endpoint = given_configured_url(router, function, qualifier, "NONE", &Value::Null).await;
    grant(
        router,
        function,
        qualifier,
        &json!({
            "StatementId": "public-url",
            "Action": "lambda:InvokeFunctionUrl",
            "Principal": "*",
            "FunctionUrlAuthType": "NONE",
        }),
    )
    .await;
    endpoint
}

/// One request a CONSUMER of the published endpoint makes: addressed at the authority it was
/// handed, carrying a path of its own and no credential at all.
async fn consumer(
    router: &MultiServiceRouter,
    endpoint: &Endpoint,
    method: &str,
    path: &str,
) -> Answer {
    send(router, method, &endpoint.host, path, &[], &[]).await
}

/// One row: the path a consumer supplies, the method it uses, and the path within the URL the
/// handler must be told about.
struct Row {
    case: &'static str,
    method: &'static str,
    path: &'static str,
    reached: &'static str,
    query: &'static str,
}

/// RULE: a request to the AUTHORITY a function URL was published at reaches that URL's
/// function, with the path the request carried as the path within the URL.
#[tokio::test]
async fn a_published_function_url_is_reached_by_its_authority_alone() {
    let engine = std::sync::Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    given_function(&router, "url-published", &reporting_handler("PUBLISHED-ONE")).await;
    let endpoint = given_public_url(&router, "url-published", None).await;
    assert!(
        endpoint
            .host
            .rsplit_once(':')
            .is_some_and(|(host, port)| !host.is_empty() && port.parse::<u16>().is_ok()),
        "setup: expected the URL to be published at a host:port a consumer can name, got {:?}",
        endpoint.host
    );

    let rows: &[Row] = &[
        Row {
            case: "the endpoint's own root",
            method: "GET",
            path: "/",
            reached: "/",
            query: "",
        },
        Row {
            case: "a nested path the consumer supplied",
            method: "GET",
            path: "/orders/42",
            reached: "/orders/42",
            query: "",
        },
        Row {
            case: "a path carrying a query",
            method: "GET",
            path: "/orders?page=2&nonce=abc",
            reached: "/orders",
            query: "page=2&nonce=abc",
        },
        Row {
            case: "a path spelled like the reserved addressing of another URL",
            method: "GET",
            path: "/_aws/lambda-url/elsewhere/orders",
            reached: "/_aws/lambda-url/elsewhere/orders",
            query: "",
        },
        Row {
            case: "a path spelled like a bare url id",
            method: "GET",
            path: "/0123456789abcdef0123456789abcdef",
            reached: "/0123456789abcdef0123456789abcdef",
            query: "",
        },
        Row {
            case: "another method on the same address",
            method: "POST",
            path: "/orders",
            reached: "/orders",
            query: "",
        },
    ];

    for row in rows {
        let case = row.case;
        let answer = consumer(&router, &endpoint, row.method, row.path).await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: the published address must reach the function, got {}",
            answer.summary()
        );
        let report = answer.json();
        assert_eq!(
            report.get("marker").and_then(Value::as_str),
            Some("PUBLISHED-ONE"),
            "case {case:?}: expected the function's own answer, got {}",
            answer.summary()
        );
        assert_eq!(
            report.get("reached").and_then(Value::as_str),
            Some(row.reached),
            "case {case:?}: expected the handler to be told the path {:?}, got {}",
            row.reached,
            answer.summary()
        );
        assert_eq!(
            report.get("query").and_then(Value::as_str),
            Some(row.query),
            "case {case:?}: expected the handler to be told the query {:?}, got {}",
            row.query,
            answer.summary()
        );
    }
}

/// RULE, the half a single endpoint cannot show: each published address serves the ONE URL it
/// was opened for, so two function URLs on one engine — here on two functions, and on a
/// function's unpublished version and an alias of it — are two addresses that never answer
/// for each other.
#[tokio::test]
async fn each_published_address_serves_only_its_own_function_url() {
    let engine = std::sync::Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    given_function(&router, "url-published-one", &reporting_handler("ONE")).await;
    given_function(&router, "url-published-two", &reporting_handler("TWO")).await;
    // A URL on an ALIAS of the second function: the same function, a different URL, so a
    // different address.
    let published = control(
        &router,
        "POST",
        "/2015-03-31/functions/url-published-two/versions",
        &Value::Null,
    )
    .await;
    assert_eq!(
        published.status,
        201,
        "setup: PublishVersion must succeed, got {}",
        published.summary()
    );
    let version = published
        .json()
        .get("Version")
        .and_then(Value::as_str)
        .unwrap_or("1")
        .to_string();
    let aliased = control(
        &router,
        "POST",
        "/2015-03-31/functions/url-published-two/aliases",
        &json!({ "Name": "live", "FunctionVersion": version }),
    )
    .await;
    assert_eq!(
        aliased.status,
        201,
        "setup: CreateAlias must succeed, got {}",
        aliased.summary()
    );

    let first = given_public_url(&router, "url-published-one", None).await;
    let second = given_public_url(&router, "url-published-two", None).await;
    let third = given_public_url(&router, "url-published-two", Some("live")).await;
    for (case, expected, endpoint) in [
        ("the first function's URL", "ONE", &first),
        ("the second function's URL", "TWO", &second),
        ("the alias's own URL of the second function", "TWO", &third),
    ] {
        let answer = consumer(&router, endpoint, "GET", "/probe").await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: the published address must reach its own function, got {}",
            answer.summary()
        );
        assert_eq!(
            answer.json().get("marker").and_then(Value::as_str),
            Some(expected),
            "case {case:?}: expected the answer of the function this address was published \
             for, got {}",
            answer.summary()
        );
    }
    let addresses = [&first.host, &second.host, &third.host];
    for (index, address) in addresses.iter().enumerate() {
        for (other, another) in addresses.iter().enumerate() {
            assert!(
                index == other || address != another,
                "case \"two published function URLs\": each URL must be published at an \
                 address of its own, got {address:?} twice"
            );
        }
    }

    // The ADDRESS decides, not the path: a consumer supplying a path spelled like ANOTHER
    // held URL's own addressing is served by the URL this address was published for, because
    // that path is a path within it.
    let crossed = consumer(
        &router,
        &first,
        "GET",
        &format!("/_aws/lambda-url/{}/probe", second.url_id),
    )
    .await;
    assert_eq!(
        crossed.status, 200,
        "case \"a path naming another URL's addressing\": the address must still serve its \
         own function, got {}",
        crossed.summary()
    );
    assert_eq!(
        crossed.json().get("marker").and_then(Value::as_str),
        Some("ONE"),
        "case \"a path naming another URL's addressing\": expected the function this address \
         was published for, got {}",
        crossed.summary()
    );
    assert_eq!(
        crossed.json().get("reached").and_then(Value::as_str),
        Some(format!("/_aws/lambda-url/{}/probe", second.url_id).as_str()),
        "case \"a path naming another URL's addressing\": expected the whole path to be the \
         path within this URL, got {}",
        crossed.summary()
    );
}

/// RULE, the part a delete decides: the address a function URL was published at stays this
/// service's to answer once the configuration behind it is gone, so a caller reading the
/// endpoint it was handed is told it is not admitted — never that no service is registered
/// there, which is the front door's answer for an address nobody serves.
#[tokio::test]
async fn a_published_address_answers_for_a_configuration_that_is_gone() {
    let engine = std::sync::Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    given_function(&router, "url-published-gone", &reporting_handler("GONE")).await;
    let endpoint = given_public_url(&router, "url-published-gone", None).await;
    let served = consumer(&router, &endpoint, "GET", "/probe").await;
    assert_eq!(
        served.status,
        200,
        "case \"the positive control\": the published address must serve first, got {}",
        served.summary()
    );

    let deleted = control(
        &router,
        "DELETE",
        "/2021-10-31/functions/url-published-gone/url",
        &Value::Null,
    )
    .await;
    assert_eq!(
        deleted.status,
        204,
        "DeleteFunctionUrlConfig must succeed, got {}",
        deleted.summary()
    );
    let after = consumer(&router, &endpoint, "GET", "/probe").await;
    assert_eq!(
        after.status,
        403,
        "case \"after the delete\": the published address must admit nobody, got {}",
        after.summary()
    );
    assert_eq!(
        after.json(),
        json!({ "Message": Value::Null }),
        "case \"after the delete\": expected the refusal of a caller this endpoint does not \
         admit, got {}",
        after.text()
    );
    assert!(
        !after.text().contains("GONE"),
        "case \"after the delete\": expected none of the function's bytes, got {}",
        after.text()
    );
}
