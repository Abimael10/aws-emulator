// SPDX-License-Identifier: CC0-1.0

//! The Amazon Route 53 health check a global endpoint's failover policy names, over the wire
//! (`awsJson1_1`, `POST /` with `X-Amz-Target: AWSEvents.<Operation>`) through the real
//! `MultiServiceRouter`, with Route 53 mounted beside EventBridge on one engine.
//!
//! The RULE asserted here: a write of a global endpoint RESOLVES the health check its
//! failover policy names in Route 53 before it stores anything — "you also create a Amazon
//! Route 53 health check that will tell EventBridge to route events to the secondary Region
//! when an 'unhealthy' state is encountered" (`CreateEndpoint`) — so a policy naming a health
//! check that service does not hold is refused with HTTP 400 `ValidationException` whose
//! message names the ARN the request carried, and the refusal leaves NO endpoint behind: the
//! read that follows reports it missing and the listing has no entry for it.
//!
//! The rows span what the rule generalises over rather than one recorded case: health checks
//! of different configurations really created through Route 53 (an HTTP endpoint probe and a
//! TCP one), ids of the forms the modelled ARN pattern admits that were never created, and
//! the id of a health check that was created and then DELETED — the case where the reference
//! was once good. The same rule is asserted on both writers of a failover policy, since
//! `UpdateEndpoint` can repoint one at another health check.
//!
//! Route 53 is driven through its own generated client over the in-process transport, the way
//! `route53_zone_records` in the Cloud Map crate drives it, so the health checks these rows
//! reference are the ones that service really holds.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::{EmbeddedEmulator, MultiServiceRouter};

use service_com_amazonaws_eventbridge_awsevents::EmulatedService;

/// The Region every EventBridge request below is signed for.
pub(crate) const REGION: &str = "eu-central-1";

/// One `awsJson1_1` call to EventBridge over the router, addressed to the service's own AWS
/// host.
///
/// `pub(crate)` so a sibling suite drives the service through the SAME harness rather than
/// growing a second copy of it.
pub(crate) async fn events(
    router: &MultiServiceRouter,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    events_addressed(
        router,
        &format!("events.{REGION}.amazonaws.com"),
        operation,
        body,
    )
    .await
}

/// The same call, addressed to `host` — which is what decides whether the caller reached one
/// of this emulator's OWN addresses or resolved the service's AWS host.
///
/// `pub(crate)` for the same reason [`events`] is.
pub(crate) async fn events_addressed(
    router: &MultiServiceRouter,
    host: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", host)
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("AWSEvents.{operation}"))
        // The credential scope is what routes the call to this service, which is what an
        // SDK's own signed request carries.
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTESTTESTTEST/20260101/{REGION}/events/\
                 aws4_request, SignedHeaders=host, Signature=0000"
            ),
        )
        .body(bytes::Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    let parsed = if payload.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice::<Value>(&payload).unwrap_or_else(|error| {
            panic!(
                "{operation} answered a body that is not JSON ({error}): {:?}",
                String::from_utf8_lossy(&payload)
            )
        })
    };
    (status, parsed)
}

/// The Route 53 client a health check is created and deleted through: the emulator's own
/// Route 53, reached in process and addressed as the global service it is.
///
/// The existence check under test reads Route 53's own store, so the health checks these rows
/// reference are the ones that service really holds.
fn route53(router: &MultiServiceRouter) -> ::aws_sdk_route53::Client {
    let callback = EmbeddedEmulator::from_router(router.clone());
    let config = ::aws_sdk_route53::Config::builder()
        .behavior_version(::aws_sdk_route53::config::BehaviorVersion::latest())
        .http_client(callback.http_client())
        .credentials_provider(callback.aws_credentials_provider())
        .region(::aws_sdk_route53::config::Region::new("us-east-1".to_string()))
        .endpoint_url("https://route53.amazonaws.com")
        .retry_config(::aws_sdk_route53::config::retry::RetryConfig::disabled())
        .timeout_config(::aws_sdk_route53::config::timeout::TimeoutConfig::disabled())
        .identity_cache(::aws_sdk_route53::config::IdentityCache::no_cache())
        .stalled_stream_protection(
            ::aws_sdk_route53::config::StalledStreamProtectionConfig::disabled(),
        )
        .build();
    ::aws_sdk_route53::Client::from_conf(config)
}

/// A router with EventBridge and Route 53 mounted on ONE shared engine.
///
/// `pub(crate)` for the same reason [`events`] is.
pub(crate) fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(engine.clone()));
    router.register_service(
        service_com_amazonaws_route53_awsdnsv20130401::EmulatedService::new(engine),
    );
    router
}

/// Create one health check through Route 53's own `CreateHealthCheck` and answer the id it
/// minted.
///
/// `pub(crate)` so a sibling suite references a health check that really exists.
pub(crate) async fn create_health_check(
    router: &MultiServiceRouter,
    caller_reference: &str,
    config: ::aws_sdk_route53::types::HealthCheckConfig,
) -> String {
    let created = route53(router)
        .create_health_check()
        .caller_reference(caller_reference)
        .health_check_config(config)
        .send()
        .await
        .unwrap_or_else(|error| {
            panic!(
                "case {caller_reference:?}: CreateHealthCheck answered {}",
                ::aws_smithy_types::error::display::DisplayErrorContext(&error)
            )
        });
    created
        .health_check()
        .unwrap_or_else(|| {
            panic!("case {caller_reference:?}: the reply must carry the health check it created")
        })
        .id()
        .to_string()
}

/// Delete one health check through Route 53's own `DeleteHealthCheck`.
async fn delete_health_check(router: &MultiServiceRouter, id: &str) {
    route53(router)
        .delete_health_check()
        .health_check_id(id)
        .send()
        .await
        .unwrap_or_else(|error| {
            panic!(
                "DeleteHealthCheck {id:?} answered {}",
                ::aws_smithy_types::error::display::DisplayErrorContext(&error)
            )
        });
}

/// The ARN of a Route 53 health check, in the form `Primary$HealthCheck` is modelled with
/// (`^arn:aws([a-z]|\-)*:route53:::healthcheck/[\-a-z0-9]+$`).
///
/// `pub(crate)` for the same reason [`events`] is.
pub(crate) fn health_check_arn(id: &str) -> String {
    format!("arn:aws:route53:::healthcheck/{id}")
}

/// The ARN of the bus `name` in `region`, in the `NonPartnerEventBusArn` form the model gives
/// `EndpointEventBus$EventBusArn`.
fn bus_arn(region: &str, name: &str) -> String {
    format!(
        "arn:aws:events:{region}:{}:event-bus/{name}",
        engine::emulator_account_id()
    )
}

/// A `CreateEndpoint` request for `name` whose failover policy watches `health_check_arn`.
///
/// `pub(crate)` for the same reason [`events`] is.
pub(crate) fn create_request(name: &str, health_check_arn: &str) -> Value {
    json!({
        "Name": name,
        "RoutingConfig": {
            "FailoverConfig": {
                "Primary": { "HealthCheck": health_check_arn },
                "Secondary": { "Route": SECONDARY_REGION },
            },
        },
        "EventBuses": [
            { "EventBusArn": bus_arn(REGION, "orders") },
            { "EventBusArn": bus_arn(SECONDARY_REGION, "orders") },
        ],
        "ReplicationConfig": { "State": "DISABLED" },
    })
}

/// The secondary Region of every endpoint below, which is not the Region the requests are
/// signed for.
pub(crate) const SECONDARY_REGION: &str = "us-west-2";

/// The wire error code of a failure body (`__type` on awsJson), without its namespace.
pub(crate) fn error_code(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .map(|name| name.rsplit('#').next().unwrap_or(name).to_string())
        .unwrap_or_default()
}

/// What EventBridge answers for a failover policy whose health check does not exist. Stated
/// here as the sentence a client reads, rather than read back out of the implementation.
fn no_such_health_check(arn: &str) -> String {
    format!("A health check with ARN {arn} does not exist.")
}

/// A health-check configuration of the `HTTP` type: an endpoint probe.
///
/// `pub(crate)` so a sibling suite's endpoints watch a health check built the same way.
pub(crate) fn http_check() -> ::aws_sdk_route53::types::HealthCheckConfig {
    health_check_config(::aws_sdk_route53::types::HealthCheckType::Http, "192.0.2.17", 80)
}

/// A health-check configuration of the `TCP` type, so the rows differ in what the health
/// check IS and not only in its id.
fn tcp_check() -> ::aws_sdk_route53::types::HealthCheckConfig {
    health_check_config(::aws_sdk_route53::types::HealthCheckType::Tcp, "198.51.100.4", 8443)
}

/// One endpoint-probing health-check configuration: the type it probes with, and the address
/// and port it probes.
fn health_check_config(
    kind: ::aws_sdk_route53::types::HealthCheckType,
    ip_address: &str,
    port: i32,
) -> ::aws_sdk_route53::types::HealthCheckConfig {
    ::aws_sdk_route53::types::HealthCheckConfig::builder()
        .r#type(kind)
        .ip_address(ip_address)
        .port(port)
        .build()
        .expect("the health-check configuration is well formed")
}

/// A create whose failover policy names a health check Route 53 HOLDS is accepted and reads
/// back; one naming a health check it does not hold is refused, and nothing is stored.
#[tokio::test]
async fn a_create_resolves_its_health_check_before_it_stores_the_endpoint() {
    let router = emulator();
    // Health checks that really exist, created through Route 53 itself.
    let http_watched = create_health_check(&router, "events-endpoint-http", http_check()).await;
    let tcp_watched = create_health_check(&router, "events-endpoint-tcp", tcp_check()).await;
    // One created and then deleted: the reference was good once, and is not any more.
    let deleted_check =
        create_health_check(&router, "events-endpoint-deleted", http_check()).await;
    delete_health_check(&router, &deleted_check).await;

    // (case, the endpoint name, the health-check ARN its policy watches, whether the create
    // is accepted)
    let rows: &[(&str, &str, String, bool)] = &[
        (
            "a health check Route 53 holds",
            "http-watched",
            health_check_arn(&http_watched),
            true,
        ),
        (
            "a health check of another type Route 53 holds",
            "tcp-watched",
            health_check_arn(&tcp_watched),
            true,
        ),
        (
            "an id of the modelled form that was never created",
            "never-created",
            health_check_arn("11111111-1111-1111-1111-111111111111"),
            false,
        ),
        (
            "a hyphenated id that was never created",
            "also-never-created",
            health_check_arn("health-check-7"),
            false,
        ),
        (
            "a hex id that was never created",
            "hex-never-created",
            health_check_arn("0123456789abcdef"),
            false,
        ),
        (
            "the id of a health check that was deleted",
            "watched-a-deleted-check",
            health_check_arn(&deleted_check),
            false,
        ),
    ];

    for (case, name, arn, accepted) in rows {
        let (status, answered) = events(&router, "CreateEndpoint", &create_request(name, arn)).await;
        if *accepted {
            assert_eq!(
                status, 200,
                "case {case:?}: expected the create to be accepted, got {status} {answered}"
            );
            let (status, read) = events(&router, "DescribeEndpoint", &json!({ "Name": name })).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected the endpoint to read back, got {status} {read}"
            );
            assert_eq!(
                read["RoutingConfig"]["FailoverConfig"]["Primary"]["HealthCheck"],
                json!(arn),
                "case {case:?}: a read reports the health check the create stored, got {read}"
            );
            continue;
        }

        assert_eq!(
            status, 400,
            "case {case:?}: expected the create to be refused with 400, got {status} {answered}"
        );
        assert_eq!(
            error_code(&answered),
            "ValidationException",
            "case {case:?}: expected a ValidationException, got {answered}"
        );
        let expected = no_such_health_check(arn);
        assert_eq!(
            answered["message"],
            json!(expected),
            "case {case:?}: expected the message {expected:?}, got {answered}"
        );

        // The refusal left NO endpoint behind.
        let (status, read) = events(&router, "DescribeEndpoint", &json!({ "Name": name })).await;
        assert_eq!(
            status, 400,
            "case {case:?}: a refused create stores nothing, so the read must refuse too, \
             got {status} {read}"
        );
        assert_eq!(
            error_code(&read),
            "ResourceNotFoundException",
            "case {case:?}: expected the read to report the endpoint missing, got {read}"
        );
    }

    // Only the endpoints whose health check resolved are listed.
    let (status, listed) = events(&router, "ListEndpoints", &json!({})).await;
    assert_eq!(status, 200, "ListEndpoints answered {status} {listed}");
    let names: Vec<&str> = listed["Endpoints"]
        .as_array()
        .map(|endpoints| {
            endpoints
                .iter()
                .filter_map(|endpoint| endpoint["Name"].as_str())
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(
        names,
        vec!["http-watched", "tcp-watched"],
        "expected only the endpoints whose health check exists to be listed, got {listed}"
    );
}

/// `UpdateEndpoint` resolves a failover policy it carries the same way, and a refused update
/// leaves the stored policy exactly as it was.
#[tokio::test]
async fn an_update_resolves_a_health_check_it_repoints_the_policy_at() {
    let router = emulator();
    let held = create_health_check(&router, "events-endpoint-update-held", http_check()).await;
    let repointed =
        create_health_check(&router, "events-endpoint-update-second", tcp_check()).await;
    let name = "repointed";
    let (status, created) =
        events(&router, "CreateEndpoint", &create_request(name, &health_check_arn(&held))).await;
    assert_eq!(status, 200, "CreateEndpoint answered {status} {created}");

    // (case, the health-check ARN the update repoints the policy at, whether it is accepted)
    let rows: &[(&str, String, bool)] = &[
        (
            "an id that was never created",
            health_check_arn("22222222-2222-2222-2222-222222222222"),
            false,
        ),
        (
            "another health check Route 53 holds",
            health_check_arn(&repointed),
            true,
        ),
    ];

    let mut in_force = health_check_arn(&held);
    for (case, arn, accepted) in rows {
        let request = json!({
            "Name": name,
            "RoutingConfig": {
                "FailoverConfig": {
                    "Primary": { "HealthCheck": arn },
                    "Secondary": { "Route": SECONDARY_REGION },
                },
            },
        });
        let (status, answered) = events(&router, "UpdateEndpoint", &request).await;
        if *accepted {
            assert_eq!(
                status, 200,
                "case {case:?}: expected the update to be accepted, got {status} {answered}"
            );
            in_force = arn.clone();
        } else {
            assert_eq!(
                status, 400,
                "case {case:?}: expected the update to be refused with 400, got {status} \
                 {answered}"
            );
            assert_eq!(
                error_code(&answered),
                "ValidationException",
                "case {case:?}: expected a ValidationException, got {answered}"
            );
            let expected = no_such_health_check(arn);
            assert_eq!(
                answered["message"],
                json!(expected),
                "case {case:?}: expected the message {expected:?}, got {answered}"
            );
        }

        let (status, read) = events(&router, "DescribeEndpoint", &json!({ "Name": name })).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeEndpoint answered {status} {read}"
        );
        assert_eq!(
            read["RoutingConfig"]["FailoverConfig"]["Primary"]["HealthCheck"],
            json!(in_force),
            "case {case:?}: expected the policy in force to watch {in_force:?}, got {read}"
        );
    }

    // An update names an endpoint FIRST: one that is not there is reported missing, whatever
    // else the write would have been wrong about.
    let absent = json!({
        "Name": "never-created",
        "RoutingConfig": {
            "FailoverConfig": {
                "Primary": { "HealthCheck": health_check_arn("33333333-3333-3333-3333-333333333333") },
                "Secondary": { "Route": SECONDARY_REGION },
            },
        },
    });
    let (status, answered) = events(&router, "UpdateEndpoint", &absent).await;
    assert_eq!(
        status, 400,
        "expected an update of an absent endpoint to be refused, got {status} {answered}"
    );
    assert_eq!(
        error_code(&answered),
        "ResourceNotFoundException",
        "expected the absent endpoint to be reported missing rather than its health check, \
         got {answered}"
    );
}
