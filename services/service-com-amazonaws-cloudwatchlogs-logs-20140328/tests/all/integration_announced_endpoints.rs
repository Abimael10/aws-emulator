// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the ENDPOINTS a read of a CloudWatch Logs integration reports for the
//! OpenSearch Service resources its setup created: the application that hosts the vended logs
//! dashboards, and the collection those dashboards query.
//!
//! RULE: an endpoint a service reports for a resource is not a label a caller reads back — it is
//! an address the caller then DIALS — so it has to lead back to THIS emulator: the address this
//! emulator answers on for the client that asked (the emulator's own endpoint the request
//! reached, else the address this process is served at), with the resource named in the PATH this
//! service SERVES it at, so a client that dials the announced address is answered by this
//! emulator. A host AWS publishes its own endpoints at stands only where this emulator has no
//! address of its own to announce in its place. Either way the value is in the form both
//! endpoint members' `@pattern` admits (`^https://[\.\-_/#:A-Za-z0-9]+\.com$` on
//! `OpenSearchApplicationEndpoint` and `OpenSearchCollectionEndpoint`, from
//! `smithy_query(query="shape",shape="OpenSearchApplicationEndpoint,OpenSearchCollectionEndpoint")`),
//! names its own resource and the Region that resource lives in, and does not move between two
//! reads of one integration.
//!
//! The rows span what the rule generalises over: the address the client reached this emulator at
//! (a loopback IP with a port, the loopback NAME, a bracketed IPv6 literal — which these members'
//! own pattern cannot carry, so there is no own address to announce — and one of AWS's own
//! endpoint hosts), whether this emulator is served on an address of its own, and the integration
//! name the endpoints are derived from (including one carrying characters no host label may
//! hold).
//!
//! Every case drives the service the way a client does — `POST /` with
//! `X-Amz-Target: Logs_20140328.<Operation>` through the router that mounts it, carrying the
//! `Host` the client reached — and reads the endpoints back through `GetIntegration`, so a value
//! no handler computed from the stored integration cannot pass.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_cloudwatchlogs_logs_20140328::state::{
    INTEGRATION_STATUS_ACTIVE as ACTIVE_STATUS, INTEGRATION_TYPE_OPENSEARCH,
};
use service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService;

/// The operations under test.
const PUT: &str = "PutIntegration";
const GET: &str = "GetIntegration";

/// The Region every call in this suite is signed for, which is the Region the resources an
/// integration's setup creates live in.
const REGION: &str = "us-west-2";

/// What this emulator must be ANNOUNCED as, once a row has created its integration.
#[derive(Clone, Copy, Debug)]
enum Announced {
    /// An address of this emulator's own: that authority, with the resource's published name
    /// carried in the path.
    OwnAddress(&'static str),
    /// No address of this emulator's own exists, so the resource's own AWS endpoint host is
    /// announced instead — the one case where a host AWS publishes endpoints at is the address
    /// to hand back.
    AwsEndpointHost,
}

/// One case: who asked, how this emulator is served, the integration the row creates and the
/// address that client has to be announced.
#[derive(Clone, Copy, Debug)]
struct Row {
    case: &'static str,
    /// The `Host` the control-plane call carries — the address this client reached the emulator
    /// at.
    reached: &'static str,
    /// The base URL this emulator is served at, or `None` for one served on no port at all.
    served_at: Option<&'static str>,
    /// The `integrationName` this row creates, which the resources' names are derived from.
    name: &'static str,
    announced: Announced,
}

/// The cases the rule is asserted over.
const ROWS: &[Row] = &[
    Row {
        case: "a client on the emulator's loopback address",
        reached: "127.0.0.1:9297",
        served_at: None,
        name: "dash",
        announced: Announced::OwnAddress("127.0.0.1:9297"),
    },
    Row {
        case: "a client on the loopback name",
        reached: "localhost:9297",
        served_at: None,
        name: "Ops.team-logs_2/dash#1",
        announced: Announced::OwnAddress("localhost:9297"),
    },
    Row {
        case: "a client on a bracketed IPv6 loopback literal, which the member cannot carry",
        reached: "[::1]:9297",
        served_at: None,
        name: "a",
        announced: Announced::AwsEndpointHost,
    },
    Row {
        case: "a client on the emulator's own endpoint, on an emulator served elsewhere too",
        reached: "127.0.0.1:9297",
        served_at: Some("https://127.0.0.1:9555"),
        name: "reached-wins",
        announced: Announced::OwnAddress("127.0.0.1:9297"),
    },
    Row {
        case: "a client that resolves AWS's own hostnames, on an emulator served on a port",
        reached: "logs.us-west-2.amazonaws.com",
        served_at: Some("https://127.0.0.1:9555"),
        name: "served-port",
        announced: Announced::OwnAddress("127.0.0.1:9555"),
    },
    Row {
        case: "a client that resolves AWS's own hostnames, on an emulator served on no port",
        reached: "logs.us-west-2.amazonaws.com",
        served_at: None,
        name: "no-own-address",
        announced: Announced::AwsEndpointHost,
    },
];

/// A router whose `OperationContext::endpoint` is `served_at`; `None` leaves the router's
/// unserved base, which names no port at all.
fn router(served_at: Option<&str>) -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(engine));
    if let Some(base) = served_at {
        router.set_url_base(
            smithy_server_runtime::EndpointUrl::parse(base).expect("a base URL the router takes"),
        );
    }
    router
}

/// One call, as a client that reached this emulator at `reached` makes it.
async fn call(
    router: &MultiServiceRouter,
    reached: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", reached)
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("Logs_20140328.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/logs/aws4_request, \
                 SignedHeaders=host;x-amz-target, Signature=0123456789abcdef"
            ),
        )
        .body(bytes::Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a valid request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    let parsed = if payload.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&payload).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// The IAM role `dataSourceRoleArn` names.
fn role_arn() -> String {
    format!(
        "arn:aws:iam::{}:role/CloudWatchOpenSearch",
        engine::emulator_account_id()
    )
}

/// A `PutIntegration` body creating `name`.
fn put_body(name: &str) -> Value {
    json!({
        "integrationName": name,
        "integrationType": INTEGRATION_TYPE_OPENSEARCH,
        "resourceConfig": {"openSearchResourceConfig": {
            "dataSourceRoleArn": role_arn(),
            "dashboardViewerPrincipals": [role_arn()],
            "retentionDays": 7,
        }},
    })
}

/// The two endpoints a read reports for an integration: the application's and the collection's.
async fn endpoints(router: &MultiServiceRouter, reached: &str, name: &str) -> (String, String) {
    let (status, reported) = call(router, reached, GET, &json!({"integrationName": name})).await;
    assert_eq!(
        status, 200,
        "case {name}: the integration reads back, got {status} {reported}"
    );
    let details = reported
        .get("integrationDetails")
        .and_then(|details| details.get("openSearchIntegrationDetails"))
        .cloned()
        .unwrap_or_else(|| panic!("case {name}: expected openSearchIntegrationDetails, got {reported}"));
    let endpoint = |resource: &str, member: &str| -> String {
        details
            .get(resource)
            .and_then(|reported| reported.get(member))
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("case {name}: expected {resource}.{member}, got {details}"))
            .to_string()
    };
    (
        endpoint("application", "applicationEndpoint"),
        endpoint("collection", "collectionEndpoint"),
    )
}

/// Whether `endpoint` is a value the modelled endpoint pattern admits:
/// `^https://[\.\-_/#:A-Za-z0-9]+\.com$`, applied character by character rather than through a
/// second regex engine.
fn in_the_modelled_endpoint_form(endpoint: &str) -> bool {
    let Some(rest) = endpoint.strip_prefix("https://") else {
        return false;
    };
    !rest.is_empty()
        && rest.ends_with(".com")
        && rest
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || ".-_/#:".contains(character))
}

/// The authority of an announced endpoint — what stands between `https://` and the first `/` —
/// and the path behind it.
fn split(case: &str, endpoint: &str) -> (String, String) {
    let rest = endpoint
        .strip_prefix("https://")
        .unwrap_or_else(|| panic!("case {case}: an announced endpoint carries its scheme, got {endpoint:?}"));
    match rest.split_once('/') {
        Some((authority, path)) => (authority.to_string(), format!("/{path}")),
        None => (rest.to_string(), "/".to_string()),
    }
}

/// DIAL an announced endpoint the way a client holding it does: its own host in the `Host`
/// header, its own path as the request target, and `route` appended to that path — so an address
/// this emulator does not serve cannot pass.
async fn dial(
    router: &MultiServiceRouter,
    case: &str,
    endpoint: &str,
    route: &str,
) -> (u16, Value) {
    let (host, path) = split(case, endpoint);
    let request = http::Request::builder()
        .method("GET")
        .uri(format!("{path}{route}"))
        .header("host", host)
        .body(bytes::Bytes::new())
        .expect("a valid request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    let parsed = if payload.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&payload).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// The status `GetIntegration` reports for an integration.
async fn integration_status(router: &MultiServiceRouter, reached: &str, name: &str) -> String {
    let (_, reported) = call(router, reached, GET, &json!({"integrationName": name})).await;
    reported
        .get("integrationStatus")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// Wait until the integration `name` has finished provisioning, polling for the CONDITION
/// rather than for a span: its setup settles on the clock the service itself stores, and only
/// then are the OpenSearch Service resources it created there.
async fn settled(router: &MultiServiceRouter, reached: &str, name: &str) {
    for _ in 0..600 {
        if integration_status(router, reached, name).await == ACTIVE_STATUS {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("case {name}: the integration never finished provisioning");
}

/// RULE: both endpoints a read reports are addresses THIS emulator answers on for the client
/// that asked, and a host of AWS's own stands only where there is no own address to announce.
#[tokio::test]
async fn both_reported_endpoints_are_addresses_this_emulator_answers_on() {
    for row in ROWS {
        let router = router(row.served_at);
        let (status, created) = call(&router, row.reached, PUT, &put_body(row.name)).await;
        assert_eq!(
            status, 200,
            "case {:?}: the integration is created, got {status} {created}",
            row.case
        );

        let (application, collection) = endpoints(&router, row.reached, row.name).await;
        for (resource, endpoint) in [("application", &application), ("collection", &collection)] {
            assert!(
                in_the_modelled_endpoint_form(endpoint),
                "case {:?}: expected the {resource} endpoint in the modelled form, got \
                 {endpoint:?}",
                row.case
            );
            let (authority, path) = split(row.case, endpoint);
            match row.announced {
                Announced::OwnAddress(expected) => {
                    assert_eq!(
                        authority, expected,
                        "case {:?}: expected the {resource} endpoint on the address this \
                         emulator answers on, got {endpoint:?}",
                        row.case
                    );
                    assert!(
                        !service_common::data_plane_addressing::addresses_an_aws_endpoint(
                            service_common::data_plane_addressing::host_only(&authority)
                        ),
                        "case {:?}: an announced address must not be a host AWS publishes its \
                         own endpoints at, got {endpoint:?}",
                        row.case
                    );
                    // The resource's own published name is what the path carries, so the
                    // address names the resource and the Region it lives in.
                    assert!(
                        path.contains(&format!(".{REGION}.")),
                        "case {:?}: expected the {resource} endpoint to name its Region, got \
                         {endpoint:?}",
                        row.case
                    );
                }
                Announced::AwsEndpointHost => {
                    assert!(
                        service_common::data_plane_addressing::addresses_an_aws_endpoint(
                            service_common::data_plane_addressing::host_only(&authority)
                        ),
                        "case {:?}: with no own address to announce, the {resource} endpoint is \
                         the resource's own AWS endpoint host, got {endpoint:?}",
                        row.case
                    );
                    assert!(
                        authority.contains(&format!(".{REGION}.")),
                        "case {:?}: expected the {resource} endpoint to name its Region, got \
                         {endpoint:?}",
                        row.case
                    );
                    assert_eq!(
                        path, "/",
                        "case {:?}: a host-style endpoint carries no path, got {endpoint:?}",
                        row.case
                    );
                }
            }
        }

        // The two resources are reached at two addresses, each carrying its own resource's
        // identity rather than the integration's bare name.
        assert_ne!(
            application, collection,
            "case {:?}: the application and the collection are two resources, got {application:?} \
             for both",
            row.case
        );

        // A READ CHANGES NOTHING: a second read, by the same client, reports the same pair.
        let (again, collection_again) = endpoints(&router, row.reached, row.name).await;
        assert_eq!(
            (again, collection_again),
            (application.clone(), collection.clone()),
            "case {:?}: expected the same endpoints on a second read",
            row.case
        );

        // AND IT IS SERVED: a client that dials an address of this emulator's reaches the
        // resource the address names, answered from the stored integration.
        if matches!(row.announced, Announced::OwnAddress(_)) {
            settled(&router, row.reached, row.name).await;
            for (resource, endpoint) in [
                ("application", &application),
                ("collection", &collection),
            ] {
                let (status, answered) = dial(&router, row.case, endpoint, "").await;
                assert_eq!(
                    status, 200,
                    "case {:?}: expected this emulator to answer at the announced {resource} \
                     endpoint {endpoint:?}, got {status} {answered}",
                    row.case
                );
                assert_eq!(
                    answered.get("integrationName").and_then(Value::as_str),
                    Some(row.name),
                    "case {:?}: expected the {resource} this address names, got {answered}",
                    row.case
                );
                assert_eq!(
                    answered
                        .get(format!("{resource}Endpoint"))
                        .and_then(Value::as_str),
                    Some(endpoint.as_str()),
                    "case {:?}: expected the resource to report the address it is reached at, \
                     got {answered}",
                    row.case
                );
                // A route beneath the endpoint that this emulator does not serve is answered
                // 501, never an empty success.
                let (status, answered) = dial(&router, row.case, endpoint, "/_search").await;
                assert_eq!(
                    status, 501,
                    "case {:?}: expected an unserved route under the {resource} endpoint to be \
                     refused as unimplemented, got {status} {answered}",
                    row.case
                );
            }
        }
    }
}

/// RULE (what the served address reports): the plane answers from the STORED integration, so an
/// address whose integration the account no longer holds is answered `404` by this emulator
/// rather than by nothing at all.
#[tokio::test]
async fn an_address_whose_integration_is_gone_is_answered_as_a_resource_that_is_not_there() {
    const REACHED: &str = "127.0.0.1:9297";
    let router = router(None);
    let (status, created) = call(&router, REACHED, PUT, &put_body("deleted")).await;
    assert_eq!(status, 200, "the integration is created, got {created}");
    settled(&router, REACHED, "deleted").await;
    let (application, collection) = endpoints(&router, REACHED, "deleted").await;

    let (status, answered) = dial(&router, "before the delete", &application, "").await;
    assert_eq!(
        status, 200,
        "the announced application endpoint is served while the integration stands, got \
         {status} {answered}"
    );

    let (status, removed) = call(
        &router,
        REACHED,
        "DeleteIntegration",
        &json!({"integrationName": "deleted", "integrationType": INTEGRATION_TYPE_OPENSEARCH}),
    )
    .await;
    assert_eq!(status, 200, "the integration is deleted, got {status} {removed}");

    for (resource, endpoint) in [("application", &application), ("collection", &collection)] {
        let (status, answered) = dial(&router, "after the delete", endpoint, "").await;
        assert_eq!(
            status, 404,
            "expected the {resource} address of a deleted integration to report a resource that \
             is not there, got {status} {answered}"
        );
        let said = answered
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let (_, path) = split("after the delete", endpoint);
        let published = path.rsplit('/').next().unwrap_or_default();
        assert!(
            said.contains(published),
            "expected the refusal to name the resource that is not there, got {said:?}"
        );
    }
}

/// RULE (the other dimension): the announced address is resolved by the CREATE and kept, so
/// every later reader of one integration reports the one address — and two integrations are
/// reached at two addresses, because each endpoint carries its own resource's identity.
#[tokio::test]
async fn the_announced_address_is_the_one_the_create_resolved_and_distinguishes_integrations() {
    let first = router(None);
    let (status, created) = call(&first, "127.0.0.1:9297", PUT, &put_body("alpha")).await;
    assert_eq!(status, 200, "the first integration is created, got {created}");

    // Another client, reaching this emulator on another of its own authorities, is handed the
    // address the create resolved: an endpoint a client keeps does not move between two reads.
    let (as_created, _) = endpoints(&first, "127.0.0.1:9297", "alpha").await;
    let (as_read, _) = endpoints(&first, "localhost:9297", "alpha").await;
    assert_eq!(
        as_read, as_created,
        "expected the address the create resolved on every later read, got {as_read:?} after \
         {as_created:?}"
    );

    // One integration per account, so the second one gets its own store.
    let second = router(None);
    let (status, created) = call(&second, "127.0.0.1:9297", PUT, &put_body("beta")).await;
    assert_eq!(status, 200, "the second integration is created, got {created}");
    let (beta_application, beta_collection) = endpoints(&second, "127.0.0.1:9297", "beta").await;
    let (alpha_application, alpha_collection) = endpoints(&first, "127.0.0.1:9297", "alpha").await;
    assert_ne!(
        alpha_application, beta_application,
        "expected two application endpoints, got {alpha_application:?} for both"
    );
    assert_ne!(
        alpha_collection, beta_collection,
        "expected two collection endpoints, got {alpha_collection:?} for both"
    );
}
