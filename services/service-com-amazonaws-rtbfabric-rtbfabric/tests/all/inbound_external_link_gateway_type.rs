// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the precondition the
//! `/responder-gateway/{gatewayId}/inbound-external-link` surface enforces, driven
//! over the wire through the router the production emulator uses.
//!
//! The rules under test are about state, not about one recorded request:
//!
//! * a link from outside the fabric is taken only by a responder gateway whose stored
//!   `gatewayType` is `EXTERNAL` — the one type that has the public inbound endpoint
//!   such a link is named under — and a gateway of any other type, or of none at all,
//!   is refused with the service's fixed wording, having stored nothing;
//! * the name the service assigns an accepted link is the link's own id as the
//!   leftmost label under exactly the `externalInboundEndpoint` the gateway itself
//!   reports, so the two readings of one stored gateway agree.
//!
//! The table spans what the rules generalise over: every member of the modelled
//! `GatewayType` enum plus a create that names none
//! (`smithy_query(query="shape", shape="GatewayType")`), crossed with both forms a
//! responder gateway is reached by (a caller-stated `domainName` and a managed
//! endpoint), crossed with the optional members a create carries (none, attributes,
//! several tags).

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_rtbfabric_rtbfabric::ops::types::GATEWAY_TYPE_VALUES;
use service_com_amazonaws_rtbfabric_rtbfabric::state::{
    INBOUND_EXTERNAL_LINK_EXTERNAL_ONLY_MESSAGE, LINK_STATUS_PENDING_CREATION,
};
use service_com_amazonaws_rtbfabric_rtbfabric::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region the test signs for; it is also the zone an assigned name sits under.
const REGION: &str = "us-west-2";

/// The one `GatewayType` member that takes links from outside the fabric, from
/// `GATEWAY_TYPE_VALUES`.
const EXTERNAL: &str = "EXTERNAL";

fn text(body: &Value, member: &str) -> String {
    body[member].as_str().unwrap_or_default().to_string()
}

/// One answered call: the status, the wire error code (absent on a success) and the
/// parsed body.
struct Answer {
    status: u16,
    code: String,
    body: Value,
}

impl Answer {
    /// The `message` the error envelope states.
    fn message(&self) -> String {
        text(&self.body, "message")
    }
}

/// How a row's responder gateway is reached. `CreateResponderGateway` takes either a
/// caller-stated domain name or a managed endpoint, and the precondition under test
/// is about the gateway's TYPE, so the table states both forms and the rule has to
/// answer the same way for each.
#[derive(Debug, Clone, Copy)]
enum Reach {
    DomainName(&'static str),
    ManagedEndpoint,
}

/// The two forms, each named for the case message.
const REACHES: &[(&str, Reach)] = &[
    (
        "reached on a caller-stated domainName",
        Reach::DomainName("responder.example.com"),
    ),
    (
        "reached through a managed endpoint",
        Reach::ManagedEndpoint,
    ),
];

/// `LinkLogSettings` with its one required member, as
/// `smithy_query(query="shape", shape="LinkLogSettings")` declares it.
fn log_settings(error_log: f64, filter_log: f64) -> Value {
    json!({ "applicationLogs": { "sampling": { "errorLog": error_log, "filterLog": filter_log } } })
}

/// The `autoScalingGroups` arm of `ManagedEndpointConfiguration`, with the two members
/// `smithy_query(query="shape", shape="AutoScalingGroupsConfiguration")` marks
/// required.
fn managed_endpoint() -> Value {
    json!({
        "autoScalingGroups": {
            "autoScalingGroupNames": ["bidders-asg"],
            "roleArn": "arn:aws:iam::123456789012:role/rtbfabric-responder"
        }
    })
}

/// The create bodies the table drives, differing in the optional members a create
/// carries: the rule is about the addressed gateway, so none of these may change the
/// answer.
fn link_bodies(token: &str) -> Vec<(&'static str, Value)> {
    vec![
        (
            "only the required members",
            json!({ "clientToken": token, "logSettings": log_settings(100.0, 0.0) }),
        ),
        (
            "attributes stated",
            json!({
                "clientToken": token,
                "logSettings": log_settings(0.0, 100.0),
                "attributes": { "customerProvidedId": "exchange-north-1" },
            }),
        ),
        (
            "three tags",
            json!({
                "clientToken": token,
                "logSettings": log_settings(12.5, 37.25),
                "tags": { "team": "rtb", "env": "test", "owner": "fabric" },
            }),
        ),
    ]
}

/// The one emulated service, behind the same `MultiServiceRouter` the production
/// router assembles, so these calls take the real deserialize -> validate -> handle
/// -> serialize path.
struct Fabric {
    router: MultiServiceRouter,
}

impl Fabric {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.register_service(EmulatedService::new(engine));
        Self { router }
    }

    /// Send one restJson1 request and return what came back.
    async fn send(&self, method: &str, path: &str, body: Option<Value>) -> Answer {
        let bytes = match body {
            Some(body) => Bytes::from(serde_json::to_vec(&body).expect("serialize the body")),
            None => Bytes::new(),
        };
        let request = ::http::Request::builder()
            .method(method)
            .uri(format!("https://rtbfabric.{REGION}.amazonaws.com{path}"))
            .header("content-type", "application/json")
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/rtbfabric/aws4_request, \
                     SignedHeaders=host, Signature=00"
                ),
            )
            .body(bytes)
            .expect("build the request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let code = response
            .headers()
            .get("x-amzn-errortype")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();
        let body = if response.body().is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(response.body()).unwrap_or(Value::Null)
        };
        Answer { status, code, body }
    }

    /// Create a responder gateway of `gateway_type` (`None` naming none at all),
    /// reached the way `reach` states, and return its minted id.
    async fn responder_gateway(
        &self,
        case: &str,
        gateway_type: Option<&str>,
        reach: Reach,
    ) -> String {
        let mut body = json!({
            "vpcId": "vpc-0123456789abcdef0",
            "subnetIds": ["subnet-0123456789abcdef0"],
            "securityGroupIds": ["sg-0123456789abcdef0"],
            "port": 443,
            "protocol": "HTTPS",
            "listenerConfig": { "protocols": ["HTTPS"] },
            "clientToken": "token-responder",
        });
        match reach {
            Reach::DomainName(domain_name) => body["domainName"] = json!(domain_name),
            Reach::ManagedEndpoint => {
                body["managedEndpointConfiguration"] = managed_endpoint();
            }
        }
        if let Some(gateway_type) = gateway_type {
            body["gatewayType"] = json!(gateway_type);
        }
        let answered = self.send("POST", "/responder-gateway", Some(body)).await;
        assert_eq!(
            answered.status, 200,
            "case {case:?}: expected CreateResponderGateway to answer 200, got {} {:?}",
            answered.status, answered.body
        );
        text(&answered.body, "gatewayId")
    }

    /// `POST /responder-gateway/{gatewayId}/inbound-external-link`.
    async fn create_inbound_external_link(&self, gateway_id: &str, body: Value) -> Answer {
        self.send(
            "POST",
            &format!("/responder-gateway/{gateway_id}/inbound-external-link"),
            Some(body),
        )
        .await
    }

    /// `GET /responder-gateway/{gatewayId}`, asserting it resolved.
    async fn responder_gateway_read(&self, case: &str, gateway_id: &str) -> Value {
        let answered = self
            .send("GET", &format!("/responder-gateway/{gateway_id}"), None)
            .await;
        assert_eq!(
            answered.status, 200,
            "case {case:?}: expected GetResponderGateway to answer 200, got {} {:?}",
            answered.status, answered.body
        );
        answered.body
    }
}

/// Every `GatewayType` the rule has to answer for: each member the model declares,
/// plus a create that states none — the member is optional, so "no type at all" is an
/// input of its own.
fn gateway_types() -> Vec<Option<&'static str>> {
    GATEWAY_TYPE_VALUES
        .iter()
        .map(|value| Some(*value))
        .chain([None])
        .collect()
}

#[tokio::test]
async fn an_inbound_external_link_is_taken_only_by_an_external_gateway() {
    for gateway_type in gateway_types() {
        for (reach_case, reach) in REACHES {
            for (body_case, body) in link_bodies("token-link") {
                let case = format!(
                    "gatewayType {:?}, {reach_case}, {body_case}",
                    gateway_type.unwrap_or("none at all")
                );
                let fabric = Fabric::new();
                let gateway_id = fabric.responder_gateway(&case, gateway_type, *reach).await;

                let answered = fabric.create_inbound_external_link(&gateway_id, body).await;

                if gateway_type == Some(EXTERNAL) {
                    assert_eq!(
                        answered.status, 200,
                        "case {case:?}: expected CreateInboundExternalLink to answer 200, got {} {:?}",
                        answered.status, answered.body
                    );
                    assert_eq!(
                        text(&answered.body, "status"),
                        LINK_STATUS_PENDING_CREATION,
                        "case {case:?}: expected a fresh link to report an accepted creation, got {:?}",
                        answered.body["status"]
                    );
                    let gateway = fabric.responder_gateway_read(&case, &gateway_id).await;
                    assert_eq!(
                        gateway["totalLinksCount"],
                        json!(1),
                        "case {case:?}: expected the gateway to count the link it took, got {:?}",
                        gateway["totalLinksCount"]
                    );
                    continue;
                }

                assert_eq!(
                    answered.status, 400,
                    "case {case:?}: expected the create to be refused with 400, got {} {:?}",
                    answered.status, answered.body
                );
                assert_eq!(
                    answered.code, "ValidationException",
                    "case {case:?}: expected ValidationException, got {:?}",
                    answered.code
                );
                assert_eq!(
                    answered.message(),
                    INBOUND_EXTERNAL_LINK_EXTERNAL_ONLY_MESSAGE,
                    "case {case:?}: expected {INBOUND_EXTERNAL_LINK_EXTERNAL_ONLY_MESSAGE:?}, got {:?}",
                    answered.message()
                );
                assert!(
                    !answered.message().contains(&gateway_id),
                    "case {case:?}: expected fixed wording naming nothing from the request, got {:?}",
                    answered.message()
                );

                // The refused call stored nothing, and the gateway it addressed has
                // no inbound endpoint for a link to be named under in the first
                // place: the two readings of one stored gateway agree.
                let gateway = fabric.responder_gateway_read(&case, &gateway_id).await;
                assert!(
                    gateway["externalInboundEndpoint"].is_null(),
                    "case {case:?}: expected no externalInboundEndpoint, got {:?}",
                    gateway["externalInboundEndpoint"]
                );
                assert_eq!(
                    gateway["totalLinksCount"],
                    json!(0),
                    "case {case:?}: expected the refused create to have stored no link, got {:?}",
                    gateway["totalLinksCount"]
                );
            }
        }
    }
}

#[tokio::test]
async fn an_accepted_links_name_sits_under_the_endpoint_its_gateway_reports() {
    for (reach_case, reach) in REACHES {
        for (body_case, body) in link_bodies("token-link") {
            let case = format!("an EXTERNAL gateway {reach_case}, {body_case}");
            let fabric = Fabric::new();
            let gateway_id = fabric
                .responder_gateway(&case, Some(EXTERNAL), *reach)
                .await;

            let answered = fabric.create_inbound_external_link(&gateway_id, body).await;
            assert_eq!(
                answered.status, 200,
                "case {case:?}: expected CreateInboundExternalLink to answer 200, got {} {:?}",
                answered.status, answered.body
            );
            let link_id = text(&answered.body, "linkId");
            let domain_name = text(&answered.body, "domainName");

            // The name the service assigned THIS link is the link's own id as the
            // leftmost label under the gateway's inbound endpoint — the very name the
            // gateway's own reader reports — so neither reading of the gateway can
            // name a zone the other denies.
            let gateway = fabric.responder_gateway_read(&case, &gateway_id).await;
            let endpoint = text(&gateway, "externalInboundEndpoint");
            assert_eq!(
                domain_name,
                format!("{link_id}.{endpoint}"),
                "case {case:?}: expected the link's name under the reported endpoint {endpoint:?}, got {domain_name:?}"
            );

            // And the read reports the name that was stored, not one recomputed per
            // call.
            let read = fabric
                .send(
                    "GET",
                    &format!("/responder-gateway/{gateway_id}/inbound-external-link/{link_id}"),
                    None,
                )
                .await;
            assert_eq!(
                read.status, 200,
                "case {case:?}: expected GetInboundExternalLink to answer 200, got {} {:?}",
                read.status, read.body
            );
            assert_eq!(
                text(&read.body, "domainName"),
                domain_name,
                "case {case:?}: expected the read to report the name the create assigned, got {:?}",
                read.body["domainName"]
            );
        }
    }
}
