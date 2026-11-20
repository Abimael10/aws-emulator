// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the gateway-pair + link lifecycle: `CreateRequesterGateway`,
//! `CreateResponderGateway`, `GetRequesterGateway`, `GetResponderGateway`,
//! `CreateLink`, `GetLink`, `ListLinks`, `DeleteLink`,
//! `CreateOutboundExternalLink` and `GetOutboundExternalLink`, driven over the wire
//! through the router the production emulator uses.
//!
//! Every assertion here is about state, not about one recorded request: a create is
//! only correct if a later read returns what it stored, and a delete is only correct
//! if the next reader sees it. The tables span the inputs the rules generalise over
//! (how many subnets and security groups, whether a description, tags, attributes or
//! the optional link members are given, the port and protocol, the form and length of
//! an external link's public endpoint, the published bounds of the modelled ranges),
//! so a handler that answered one shape of request would fail on the next row.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_rtbfabric_rtbfabric::ops::types::{
    CONNECTIVITY_TYPE_VALUES, LINK_DIRECTION_VALUES, LINK_STATUS_VALUES, PROTOCOL_VALUES,
    REQUESTER_GATEWAY_STATUS_VALUES, RESPONDER_GATEWAY_STATUS_VALUES,
};
use service_com_amazonaws_rtbfabric_rtbfabric::state::{
    GATEWAY_QUOTA, PROVISIONING_DELAY, RESOURCE_NOT_FOUND_MESSAGE, TAGS_PER_RESOURCE_QUOTA,
};
use service_com_amazonaws_rtbfabric_rtbfabric::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region the test signs for; it is also the zone the gateways' assigned
/// domain names must sit under.
const REGION: &str = "us-west-2";

/// The one emulated service, behind the same `MultiServiceRouter` the production
/// router assembles, so these calls take the real deserialize -> validate ->
/// handle -> serialize path.
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

    /// Send one restJson1 request and return `(status, parsed body)`.
    async fn send(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        let bytes = match body {
            Some(body) => {
                Bytes::from(serde_json::to_vec(&body).expect("serialize the request body"))
            }
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
        let parsed = if response.body().is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(response.body()).unwrap_or(Value::Null)
        };
        (status, parsed)
    }
}

/// How a row's responder gateway is reached. `CreateResponderGateway`'s own
/// documentation says "A domain name or managed endpoint is required", so each
/// row picks one of the two accepted forms.
enum ResponderEndpoint {
    DomainName(&'static str),
    ManagedEndpoint,
}

/// One row of the lifecycle table.
///
/// The rules under test ("a create persists; a read reports what was persisted;
/// the link counts are derived") hold for every input, so the rows differ along
/// each axis the inputs offer rather than only in the names of things.
struct Row {
    name: &'static str,
    vpc_id: &'static str,
    subnet_ids: &'static [&'static str],
    security_group_ids: &'static [&'static str],
    /// `GetRequesterGateway`/`GetResponderGateway` model `description` with the
    /// narrower `^[A-Za-z0-9 ]+$`, so the rows that set one keep to that.
    description: Option<&'static str>,
    tags: &'static [(&'static str, &'static str)],
    port: i64,
    protocol: &'static str,
    responder_endpoint: ResponderEndpoint,
    /// A `GatewayType`, from `GATEWAY_TYPE_VALUES`.
    gateway_type: Option<&'static str>,
    /// A `ClientRoutingPolicy`, from `CLIENT_ROUTING_POLICY_VALUES`.
    client_routing_policy: Option<&'static str>,
    link_tags: &'static [(&'static str, &'static str)],
    http_responder_allowed: Option<bool>,
    timeout_in_millis: Option<i64>,
    customer_provided_id: Option<&'static str>,
}

/// Ids here satisfy the modelled member constraints, which are narrower than the
/// shorthand an issue report uses: `SubnetId` is `^subnet-\w{8,17}$` (len 15..24)
/// and `SecurityGroupId` is `^sg-[0-9a-f]{8,40}$` (len 11..43), from
/// `smithy_query(query="shape", shape="SubnetIdList,SecurityGroupIdList")`.
/// `port` sits inside the modelled range 1..65535 and `timeoutInMillis` inside
/// 100..5000, both exercised at their published bounds.
const ROWS: &[Row] = &[
    Row {
        name: "only the required members",
        vpc_id: "vpc-0123456789abcdef0",
        subnet_ids: &["subnet-0123456789abcdef0"],
        security_group_ids: &["sg-0123456789abcdef0"],
        description: None,
        tags: &[],
        port: 1,
        protocol: "HTTP",
        responder_endpoint: ResponderEndpoint::DomainName("responder.example.com"),
        gateway_type: None,
        client_routing_policy: None,
        link_tags: &[],
        http_responder_allowed: None,
        timeout_in_millis: None,
        customer_provided_id: None,
    },
    Row {
        name: "every optional member, several subnets and groups",
        vpc_id: "vpc-abcdef0123456789a",
        subnet_ids: &[
            "subnet-0123456789abcdef0",
            "subnet-00000000",
            "subnet-fedcba9876543210f",
        ],
        security_group_ids: &["sg-00000000", "sg-abcdef0123456789abcdef0123456789abcdef01"],
        description: Some("Spot bidding fabric 2"),
        tags: &[("env", "test"), ("team", "ads")],
        port: 65535,
        protocol: "HTTPS",
        responder_endpoint: ResponderEndpoint::ManagedEndpoint,
        gateway_type: Some("EXTERNAL"),
        client_routing_policy: Some("AVAILABILITY_ZONE_AFFINITY"),
        link_tags: &[("team", "ads")],
        http_responder_allowed: Some(true),
        timeout_in_millis: Some(100),
        customer_provided_id: Some("edge-fabric-1"),
    },
    Row {
        name: "the other end of each bound",
        vpc_id: "vpc-00000000",
        subnet_ids: &["subnet-11111111", "subnet-22222222"],
        security_group_ids: &["sg-99999999"],
        description: Some("Display exchange"),
        tags: &[("cost-center", "4711")],
        port: 443,
        protocol: "HTTPS",
        responder_endpoint: ResponderEndpoint::DomainName("bidder.ads.example.com"),
        gateway_type: Some("INTERNAL"),
        client_routing_policy: Some("ANY_AVAILABILITY_ZONE"),
        link_tags: &[("env", "test"), ("owner", "fabric")],
        http_responder_allowed: Some(false),
        timeout_in_millis: Some(5000),
        customer_provided_id: None,
    },
];

impl Row {
    fn requester_body(&self, client_token: &str) -> Value {
        let mut body = serde_json::Map::new();
        body.insert("vpcId".to_string(), json!(self.vpc_id));
        body.insert("subnetIds".to_string(), json!(self.subnet_ids));
        body.insert(
            "securityGroupIds".to_string(),
            json!(self.security_group_ids),
        );
        body.insert("clientToken".to_string(), json!(client_token));
        if let Some(description) = self.description {
            body.insert("description".to_string(), json!(description));
        }
        if !self.tags.is_empty() {
            body.insert("tags".to_string(), tags_json(self.tags));
        }
        Value::Object(body)
    }

    fn responder_body(&self, client_token: &str) -> Value {
        let mut body = serde_json::Map::new();
        body.insert("vpcId".to_string(), json!(self.vpc_id));
        body.insert("subnetIds".to_string(), json!(self.subnet_ids));
        body.insert(
            "securityGroupIds".to_string(),
            json!(self.security_group_ids),
        );
        body.insert("port".to_string(), json!(self.port));
        body.insert("protocol".to_string(), json!(self.protocol));
        body.insert(
            "listenerConfig".to_string(),
            json!({ "protocols": [self.protocol] }),
        );
        body.insert("clientToken".to_string(), json!(client_token));
        match self.responder_endpoint {
            ResponderEndpoint::DomainName(domain_name) => {
                body.insert("domainName".to_string(), json!(domain_name));
            }
            ResponderEndpoint::ManagedEndpoint => {
                body.insert(
                    "managedEndpointConfiguration".to_string(),
                    managed_endpoint(),
                );
            }
        }
        if let Some(gateway_type) = self.gateway_type {
            body.insert("gatewayType".to_string(), json!(gateway_type));
        }
        if let Some(policy) = self.client_routing_policy {
            body.insert("clientRoutingPolicy".to_string(), json!(policy));
        }
        if let Some(description) = self.description {
            body.insert("description".to_string(), json!(description));
        }
        if !self.tags.is_empty() {
            body.insert("tags".to_string(), tags_json(self.tags));
        }
        Value::Object(body)
    }

    /// What `GetResponderGateway` must report for `domainName`.
    fn expected_domain_name(&self) -> Value {
        match self.responder_endpoint {
            ResponderEndpoint::DomainName(domain_name) => json!(domain_name),
            ResponderEndpoint::ManagedEndpoint => Value::Null,
        }
    }

    /// What `GetResponderGateway` must report for `managedEndpointConfiguration`.
    fn expected_managed_endpoint(&self) -> Value {
        match self.responder_endpoint {
            ResponderEndpoint::DomainName(_) => Value::Null,
            ResponderEndpoint::ManagedEndpoint => managed_endpoint(),
        }
    }

    fn link_body(&self, peer_gateway_id: &str) -> Value {
        let mut body = serde_json::Map::new();
        body.insert("peerGatewayId".to_string(), json!(peer_gateway_id));
        body.insert("logSettings".to_string(), log_settings());
        if !self.link_tags.is_empty() {
            body.insert("tags".to_string(), tags_json(self.link_tags));
        }
        if let Some(allowed) = self.http_responder_allowed {
            body.insert("httpResponderAllowed".to_string(), json!(allowed));
        }
        if let Some(timeout) = self.timeout_in_millis {
            body.insert("timeoutInMillis".to_string(), json!(timeout));
        }
        if let Some(customer_provided_id) = self.customer_provided_id {
            body.insert(
                "attributes".to_string(),
                json!({ "customerProvidedId": customer_provided_id }),
            );
        }
        Value::Object(body)
    }
}

fn tags_json(tags: &[(&str, &str)]) -> Value {
    Value::Object(
        tags.iter()
            .map(|(key, value)| ((*key).to_string(), json!(value)))
            .collect(),
    )
}

/// `LinkLogSettings` with its one required member, as
/// `smithy_query(query="shape", shape="LinkLogSettings")` declares it.
fn log_settings() -> Value {
    json!({ "applicationLogs": { "sampling": { "errorLog": 100.0, "filterLog": 0.0 } } })
}

/// The `autoScalingGroups` arm of `ManagedEndpointConfiguration`, with the two
/// members `smithy_query(query="shape", shape="AutoScalingGroupsConfiguration")`
/// marks required (`roleArn` matching `^arn:aws:iam::[0-9]{12}:role/.{1,2048}$`).
fn managed_endpoint() -> Value {
    json!({
        "autoScalingGroups": {
            "autoScalingGroupNames": ["bidders-asg"],
            "roleArn": "arn:aws:iam::123456789012:role/rtbfabric-responder"
        }
    })
}

fn text(body: &Value, member: &str) -> String {
    body[member].as_str().unwrap_or_default().to_string()
}

/// `GatewayId`: len 8..32, `^rtb-gw-[a-z0-9-]{1,25}$`.
fn is_gateway_id(value: &str) -> bool {
    let Some(suffix) = value.strip_prefix("rtb-gw-") else {
        return false;
    };
    (8..=32).contains(&value.len())
        && (1..=25).contains(&suffix.len())
        && suffix
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// `LinkId`: len 6..30, `^link-[a-z0-9-]{1,25}$`.
fn is_link_id(value: &str) -> bool {
    let Some(suffix) = value.strip_prefix("link-") else {
        return false;
    };
    (6..=30).contains(&value.len())
        && (1..=25).contains(&suffix.len())
        && suffix
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// One label of `DomainName`: `[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?`.
fn is_dns_label(label: &str) -> bool {
    (1..=63).contains(&label.len())
        && label
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !label.starts_with('-')
        && !label.ends_with('-')
}

/// `DomainName`: len 1..255, two or more labels joined by dots.
fn is_domain_name(value: &str) -> bool {
    let labels: Vec<&str> = value.split('.').collect();
    (1..=255).contains(&value.len()) && labels.len() >= 2 && labels.iter().all(|l| is_dns_label(l))
}

#[tokio::test]
async fn the_gateway_pair_and_link_lifecycle_round_trips_through_state() {
    for row in ROWS {
        let case = row.name;
        let fabric = Fabric::new();

        let (status, requester) = fabric
            .send(
                "POST",
                "/requester-gateway",
                Some(row.requester_body("token-requester")),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected CreateRequesterGateway to answer 200, got {status} {requester:?}"
        );
        let requester_id = text(&requester, "gatewayId");
        assert!(
            is_gateway_id(&requester_id),
            "case {case:?}: expected a minted gatewayId matching ^rtb-gw-[a-z0-9-]{{1,25}}$, got {requester_id:?}"
        );
        let requester_domain = text(&requester, "domainName");
        assert!(
            is_domain_name(&requester_domain),
            "case {case:?}: expected a generated domainName matching the modelled pattern, got {requester_domain:?}"
        );
        assert!(
            requester_domain.contains(REGION),
            "case {case:?}: expected the domainName to sit under the addressed region {REGION}, got {requester_domain:?}"
        );
        let requester_status = text(&requester, "status");
        assert!(
            REQUESTER_GATEWAY_STATUS_VALUES.contains(&requester_status.as_str()),
            "case {case:?}: expected a modelled RequesterGatewayStatus, got {requester_status:?}"
        );

        let (status, responder) = fabric
            .send(
                "POST",
                "/responder-gateway",
                Some(row.responder_body("token-responder")),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected CreateResponderGateway to answer 200, got {status} {responder:?}"
        );
        let responder_id = text(&responder, "gatewayId");
        assert!(
            is_gateway_id(&responder_id),
            "case {case:?}: expected a minted gatewayId for the responder, got {responder_id:?}"
        );
        assert_ne!(
            requester_id, responder_id,
            "case {case:?}: expected two distinct gateways, both answered {responder_id:?}"
        );
        let responder_status = text(&responder, "status");
        assert!(
            RESPONDER_GATEWAY_STATUS_VALUES.contains(&responder_status.as_str()),
            "case {case:?}: expected a modelled ResponderGatewayStatus, got {responder_status:?}"
        );
        assert_eq!(
            responder["listenerConfig"],
            json!({ "protocols": [row.protocol] }),
            "case {case:?}: expected the persisted listenerConfig back, got {:?}",
            responder["listenerConfig"]
        );
        assert_eq!(
            responder["clientRoutingPolicy"],
            row.client_routing_policy
                .map_or(Value::Null, |policy| json!(policy)),
            "case {case:?}: expected the persisted clientRoutingPolicy back, got {:?}",
            responder["clientRoutingPolicy"]
        );
        // Only an EXTERNAL gateway accepts inbound external links, so only it
        // has a public inbound endpoint.
        if row.gateway_type == Some("EXTERNAL") {
            let endpoint = text(&responder, "externalInboundEndpoint");
            assert!(
                is_domain_name(&endpoint),
                "case {case:?}: expected an EXTERNAL responder gateway to carry an externalInboundEndpoint matching the modelled pattern, got {endpoint:?}"
            );
        } else {
            assert!(
                responder["externalInboundEndpoint"].is_null(),
                "case {case:?}: expected no externalInboundEndpoint for gatewayType {:?}, got {:?}",
                row.gateway_type,
                responder["externalInboundEndpoint"]
            );
        }

        // The requester gateway reads back exactly what was stored, with no links yet.
        let (status, read) = fabric
            .send("GET", &format!("/requester-gateway/{requester_id}"), None)
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected GetRequesterGateway to answer 200, got {status} {read:?}"
        );
        assert_eq!(
            text(&read, "gatewayId"),
            requester_id,
            "case {case:?}: expected GetRequesterGateway to report {requester_id:?}, got {:?}",
            read["gatewayId"]
        );
        assert_eq!(
            text(&read, "vpcId"),
            row.vpc_id,
            "case {case:?}: expected the stored vpcId, got {:?}",
            read["vpcId"]
        );
        assert_eq!(
            read["subnetIds"],
            json!(row.subnet_ids),
            "case {case:?}: expected the stored subnetIds, got {:?}",
            read["subnetIds"]
        );
        assert_eq!(
            read["securityGroupIds"],
            json!(row.security_group_ids),
            "case {case:?}: expected the stored securityGroupIds, got {:?}",
            read["securityGroupIds"]
        );
        assert_eq!(
            read["domainName"],
            json!(requester_domain),
            "case {case:?}: expected GetRequesterGateway to report the domainName the create answered, got {:?}",
            read["domainName"]
        );
        match row.description {
            Some(description) => assert_eq!(
                read["description"],
                json!(description),
                "case {case:?}: expected the stored description, got {:?}",
                read["description"]
            ),
            None => assert!(
                read["description"].is_null(),
                "case {case:?}: expected no description, got {:?}",
                read["description"]
            ),
        }
        if row.tags.is_empty() {
            assert!(
                read["tags"].is_null(),
                "case {case:?}: expected no tags, got {:?}",
                read["tags"]
            );
        } else {
            assert_eq!(
                read["tags"],
                tags_json(row.tags),
                "case {case:?}: expected the stored tags, got {:?}",
                read["tags"]
            );
        }
        assert_eq!(
            read["totalLinksCount"],
            json!(0),
            "case {case:?}: expected totalLinksCount 0 before any link, got {:?}",
            read["totalLinksCount"]
        );
        assert_eq!(
            read["activeLinksCount"],
            json!(0),
            "case {case:?}: expected activeLinksCount 0 before any link, got {:?}",
            read["activeLinksCount"]
        );
        assert!(
            read["createdAt"].as_f64().unwrap_or_default() > 0.0,
            "case {case:?}: expected a createdAt timestamp, got {:?}",
            read["createdAt"]
        );

        // And so does the responder gateway, including its port and protocol.
        let (status, read_responder) = fabric
            .send("GET", &format!("/responder-gateway/{responder_id}"), None)
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected GetResponderGateway to answer 200, got {status} {read_responder:?}"
        );
        assert_eq!(
            read_responder["port"],
            json!(row.port),
            "case {case:?}: expected the stored port, got {:?}",
            read_responder["port"]
        );
        assert_eq!(
            text(&read_responder, "protocol"),
            row.protocol,
            "case {case:?}: expected the stored protocol, got {:?}",
            read_responder["protocol"]
        );
        assert!(
            PROTOCOL_VALUES.contains(&text(&read_responder, "protocol").as_str()),
            "case {case:?}: expected a modelled Protocol, got {:?}",
            read_responder["protocol"]
        );
        assert_eq!(
            read_responder["domainName"],
            row.expected_domain_name(),
            "case {case:?}: expected GetResponderGateway to report the domainName exactly as it was given, got {:?}",
            read_responder["domainName"]
        );
        assert_eq!(
            read_responder["managedEndpointConfiguration"],
            row.expected_managed_endpoint(),
            "case {case:?}: expected the stored managedEndpointConfiguration, got {:?}",
            read_responder["managedEndpointConfiguration"]
        );
        assert_eq!(
            read_responder["gatewayType"],
            row.gateway_type.map_or(Value::Null, |kind| json!(kind)),
            "case {case:?}: expected the stored gatewayType, got {:?}",
            read_responder["gatewayType"]
        );
        assert_eq!(
            read_responder["clientRoutingPolicy"],
            row.client_routing_policy
                .map_or(Value::Null, |policy| json!(policy)),
            "case {case:?}: expected the stored clientRoutingPolicy, got {:?}",
            read_responder["clientRoutingPolicy"]
        );
        assert_eq!(
            read_responder["externalInboundEndpoint"], responder["externalInboundEndpoint"],
            "case {case:?}: expected GetResponderGateway to report the externalInboundEndpoint the create answered, got {:?}",
            read_responder["externalInboundEndpoint"]
        );

        // The link: a fresh request between the two stored gateways.
        let (status, link) = fabric
            .send(
                "POST",
                &format!("/gateway/{requester_id}/create-link"),
                Some(row.link_body(&responder_id)),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected CreateLink to answer 200, got {status} {link:?}"
        );
        let link_id = text(&link, "linkId");
        assert!(
            is_link_id(&link_id),
            "case {case:?}: expected a minted linkId matching ^link-[a-z0-9-]{{1,25}}$, got {link_id:?}"
        );
        let link_status = text(&link, "status");
        assert!(
            LINK_STATUS_VALUES.contains(&link_status.as_str()),
            "case {case:?}: expected a modelled LinkStatus, got {link_status:?}"
        );
        assert!(
            link_status != "ACTIVE" && link_status != "DELETED" && link_status != "REJECTED",
            "case {case:?}: expected a fresh link request to be non-terminal, got {link_status:?}"
        );
        assert_eq!(
            text(&link, "gatewayId"),
            requester_id,
            "case {case:?}: expected CreateLink to report the calling gateway, got {:?}",
            link["gatewayId"]
        );
        assert_eq!(
            text(&link, "peerGatewayId"),
            responder_id,
            "case {case:?}: expected CreateLink to report the peer, got {:?}",
            link["peerGatewayId"]
        );
        assert_eq!(
            text(&link, "direction"),
            "REQUEST",
            "case {case:?}: expected a link opened from a requester gateway to be REQUEST, got {:?}",
            link["direction"]
        );
        assert!(
            LINK_DIRECTION_VALUES.contains(&text(&link, "direction").as_str()),
            "case {case:?}: expected a modelled LinkDirection, got {:?}",
            link["direction"]
        );
        assert!(
            CONNECTIVITY_TYPE_VALUES.contains(&text(&link, "connectivityType").as_str()),
            "case {case:?}: expected a modelled ConnectivityType, got {:?}",
            link["connectivityType"]
        );
        assert_eq!(
            link["customerProvidedId"],
            row.customer_provided_id.map_or(Value::Null, |id| json!(id)),
            "case {case:?}: expected the customerProvidedId carried in attributes, got {:?}",
            link["customerProvidedId"]
        );

        // GetLink proves the request was STORED, not echoed: `tags`,
        // `httpResponderAllowed` and `timeoutInMillis` are not in CreateLink's
        // response shape at all, so they can only come from state.
        let (status, read_link) = fabric
            .send(
                "GET",
                &format!("/gateway/{requester_id}/link/{link_id}"),
                None,
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected GetLink to answer 200, got {status} {read_link:?}"
        );
        assert_eq!(
            text(&read_link, "linkId"),
            link_id,
            "case {case:?}: expected GetLink to report {link_id:?}, got {:?}",
            read_link["linkId"]
        );
        assert_eq!(
            read_link["httpResponderAllowed"],
            row.http_responder_allowed
                .map_or(Value::Null, |allowed| json!(allowed)),
            "case {case:?}: expected httpResponderAllowed read back from state, got {:?}",
            read_link["httpResponderAllowed"]
        );
        assert_eq!(
            read_link["timeoutInMillis"],
            row.timeout_in_millis
                .map_or(Value::Null, |timeout| json!(timeout)),
            "case {case:?}: expected timeoutInMillis read back from state, got {:?}",
            read_link["timeoutInMillis"]
        );
        if row.link_tags.is_empty() {
            assert!(
                read_link["tags"].is_null(),
                "case {case:?}: expected no link tags, got {:?}",
                read_link["tags"]
            );
        } else {
            assert_eq!(
                read_link["tags"],
                tags_json(row.link_tags),
                "case {case:?}: expected the link tags read back from state, got {:?}",
                read_link["tags"]
            );
        }
        assert_eq!(
            read_link["logSettings"],
            log_settings(),
            "case {case:?}: expected the stored logSettings, got {:?}",
            read_link["logSettings"]
        );

        // The link is addressable from its other end too: both gateways are ends
        // of the one stored record.
        let (status, from_peer) = fabric
            .send(
                "GET",
                &format!("/gateway/{responder_id}/link/{link_id}"),
                None,
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected GetLink from the peer end to answer 200, got {status} {from_peer:?}"
        );
        assert_eq!(
            from_peer["peerGatewayId"], read_link["peerGatewayId"],
            "case {case:?}: expected the stored orientation from either end, got {:?}",
            from_peer["peerGatewayId"]
        );

        // Both gateways now count the one link.
        for (kind, path, gateway_id) in [
            (
                "requester",
                format!("/requester-gateway/{requester_id}"),
                &requester_id,
            ),
            (
                "responder",
                format!("/responder-gateway/{responder_id}"),
                &responder_id,
            ),
        ] {
            let (status, read) = fabric.send("GET", &path, None).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected the {kind} gateway {gateway_id} to answer 200, got {status} {read:?}"
            );
            assert_eq!(
                read["totalLinksCount"],
                json!(1),
                "case {case:?}: expected the {kind} gateway's totalLinksCount to be 1, got {:?}",
                read["totalLinksCount"]
            );
            assert_eq!(
                read["activeLinksCount"],
                json!(0),
                "case {case:?}: expected the {kind} gateway's activeLinksCount to be 0 while the request is outstanding, got {:?}",
                read["activeLinksCount"]
            );
        }

        // Delete it, and the next reader sees it gone.
        let (status, deleted) = fabric
            .send(
                "DELETE",
                &format!("/gateway/{requester_id}/link/{link_id}"),
                None,
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected DeleteLink to answer 200, got {status} {deleted:?}"
        );
        assert_eq!(
            text(&deleted, "linkId"),
            link_id,
            "case {case:?}: expected DeleteLink to report {link_id:?}, got {:?}",
            deleted["linkId"]
        );
        assert_eq!(
            text(&deleted, "status"),
            "DELETED",
            "case {case:?}: expected the terminal DELETED status, got {:?}",
            deleted["status"]
        );

        let (status, gone) = fabric
            .send(
                "GET",
                &format!("/gateway/{requester_id}/link/{link_id}"),
                None,
            )
            .await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected GetLink on a deleted link to answer 404, got {status} {gone:?}"
        );

        let (status, read) = fabric
            .send("GET", &format!("/requester-gateway/{requester_id}"), None)
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected GetRequesterGateway to answer 200, got {status} {read:?}"
        );
        assert_eq!(
            read["totalLinksCount"],
            json!(0),
            "case {case:?}: expected totalLinksCount back to 0 after the delete, got {:?}",
            read["totalLinksCount"]
        );
    }
}

#[tokio::test]
async fn a_repeated_client_token_returns_the_first_gateway_and_a_changed_one_is_refused() {
    for row in ROWS {
        let case = row.name;
        let fabric = Fabric::new();

        for (operation, path, body) in [
            (
                "CreateRequesterGateway",
                "/requester-gateway",
                row.requester_body("token-a") as Value,
            ),
            (
                "CreateResponderGateway",
                "/responder-gateway",
                row.responder_body("token-a"),
            ),
        ] {
            let (status, first) = fabric.send("POST", path, Some(body.clone())).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected {operation} to answer 200, got {status} {first:?}"
            );
            let (status, retry) = fabric.send("POST", path, Some(body)).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected the {operation} retry to answer 200, got {status} {retry:?}"
            );
            assert_eq!(
                text(&retry, "gatewayId"),
                text(&first, "gatewayId"),
                "case {case:?}: expected the same clientToken to answer {operation}'s first gatewayId {:?}, got {:?}",
                first["gatewayId"],
                retry["gatewayId"]
            );
        }

        // Same token, different parameters: the documented contract is a failure,
        // and the only client-input error these operations model is
        // ValidationException (400).
        let mut changed = row.requester_body("token-a");
        changed["vpcId"] = json!("vpc-ffffffffffffffff0");
        let (status, refused) = fabric
            .send("POST", "/requester-gateway", Some(changed))
            .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected a reused clientToken with changed parameters to be refused with 400, got {status} {refused:?}"
        );
    }
}

#[tokio::test]
async fn a_link_needs_two_stored_gateways_and_only_one_link_per_pair() {
    let row = &ROWS[1];
    let fabric = Fabric::new();

    let (_, requester) = fabric
        .send(
            "POST",
            "/requester-gateway",
            Some(row.requester_body("token-requester")),
        )
        .await;
    let requester_id = text(&requester, "gatewayId");
    let (_, responder) = fabric
        .send(
            "POST",
            "/responder-gateway",
            Some(row.responder_body("token-responder")),
        )
        .await;
    let responder_id = text(&responder, "gatewayId");

    // Neither end may be an id the service never minted, whichever end it is.
    let unknown = "rtb-gw-does-not-exist";
    for (case, gateway_id, peer_gateway_id) in [
        ("unknown peer", requester_id.as_str(), unknown),
        ("unknown caller", unknown, responder_id.as_str()),
        ("both unknown", unknown, unknown),
    ] {
        let (status, body) = fabric
            .send(
                "POST",
                &format!("/gateway/{gateway_id}/create-link"),
                Some(row.link_body(peer_gateway_id)),
            )
            .await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected CreateLink against an unstored gateway to answer 404, got {status} {body:?}"
        );
    }

    // A gateway cannot link to itself.
    let (status, body) = fabric
        .send(
            "POST",
            &format!("/gateway/{requester_id}/create-link"),
            Some(row.link_body(&requester_id)),
        )
        .await;
    assert_eq!(
        status, 400,
        "expected a self-link to be refused with 400, got {status} {body:?}"
    );

    // The first link is created, a second one for the same pair conflicts.
    let (status, body) = fabric
        .send(
            "POST",
            &format!("/gateway/{requester_id}/create-link"),
            Some(row.link_body(&responder_id)),
        )
        .await;
    assert_eq!(
        status, 200,
        "expected the first CreateLink to answer 200, got {status} {body:?}"
    );
    let (status, body) = fabric
        .send(
            "POST",
            &format!("/gateway/{requester_id}/create-link"),
            Some(row.link_body(&responder_id)),
        )
        .await;
    assert_eq!(
        status, 409,
        "expected a second link for the same gateway pair to conflict with 409, got {status} {body:?}"
    );

    // A link is one record with two ends, so the reverse orientation is the same
    // pair and conflicts too.
    let (status, body) = fabric
        .send(
            "POST",
            &format!("/gateway/{responder_id}/create-link"),
            Some(row.link_body(&requester_id)),
        )
        .await;
    assert_eq!(
        status, 409,
        "expected a link for the same pair in the reverse orientation to conflict with 409, got {status} {body:?}"
    );
}

#[tokio::test]
async fn a_responder_gateway_needs_a_domain_name_or_a_managed_endpoint() {
    for row in ROWS {
        let case = row.name;
        let fabric = Fabric::new();

        // Neither member: refused, because the operation's documentation makes
        // one of the two required and neither is `@required` on its own.
        let mut neither = row.responder_body("token-neither");
        let object = neither.as_object_mut().expect("a JSON object body");
        object.remove("domainName");
        object.remove("managedEndpointConfiguration");
        let (status, refused) = fabric
            .send("POST", "/responder-gateway", Some(neither))
            .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected a responder gateway with neither domainName nor managedEndpointConfiguration to be refused with 400, got {status} {refused:?}"
        );

        // Either member alone: accepted.
        let mut with_domain = row.responder_body("token-domain");
        let object = with_domain.as_object_mut().expect("a JSON object body");
        object.remove("managedEndpointConfiguration");
        object.insert("domainName".to_string(), json!("responder.example.com"));
        let (status, accepted) = fabric
            .send("POST", "/responder-gateway", Some(with_domain))
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected a responder gateway with only a domainName to be accepted, got {status} {accepted:?}"
        );

        let mut with_endpoint = row.responder_body("token-endpoint");
        let object = with_endpoint.as_object_mut().expect("a JSON object body");
        object.remove("domainName");
        object.insert(
            "managedEndpointConfiguration".to_string(),
            managed_endpoint(),
        );
        let (status, accepted) = fabric
            .send("POST", "/responder-gateway", Some(with_endpoint))
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected a responder gateway with only a managedEndpointConfiguration to be accepted, got {status} {accepted:?}"
        );
    }
}

#[tokio::test]
async fn unknown_gateways_and_links_are_not_found_and_a_second_delete_is_not_a_server_error() {
    let row = &ROWS[0];
    let fabric = Fabric::new();

    for (case, method, path) in [
        (
            "requester gateway",
            "GET",
            "/requester-gateway/rtb-gw-unknown",
        ),
        (
            "responder gateway",
            "GET",
            "/responder-gateway/rtb-gw-unknown",
        ),
        (
            "link on an unknown gateway",
            "GET",
            "/gateway/rtb-gw-unknown/link/link-unknown",
        ),
        (
            "delete a link on an unknown gateway",
            "DELETE",
            "/gateway/rtb-gw-unknown/link/link-unknown",
        ),
    ] {
        let (status, body) = fabric.send(method, path, None).await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected {method} {path} to answer 404, got {status} {body:?}"
        );
    }

    let (_, requester) = fabric
        .send(
            "POST",
            "/requester-gateway",
            Some(row.requester_body("token-requester")),
        )
        .await;
    let requester_id = text(&requester, "gatewayId");
    let (_, responder) = fabric
        .send(
            "POST",
            "/responder-gateway",
            Some(row.responder_body("token-responder")),
        )
        .await;
    let responder_id = text(&responder, "gatewayId");
    let (_, link) = fabric
        .send(
            "POST",
            &format!("/gateway/{requester_id}/create-link"),
            Some(row.link_body(&responder_id)),
        )
        .await;
    let link_id = text(&link, "linkId");

    let path = format!("/gateway/{requester_id}/link/{link_id}");
    let (status, _) = fabric.send("DELETE", &path, None).await;
    assert_eq!(status, 200, "expected the first delete to answer 200");
    let (status, body) = fabric.send("DELETE", &path, None).await;
    assert_eq!(
        status, 404,
        "expected a second delete of the same link to answer 404, never a server error, got {status} {body:?}"
    );
}

#[tokio::test]
async fn the_modelled_constraints_are_refused_at_their_bounds() {
    let row = &ROWS[0];
    let fabric = Fabric::new();

    // One past each published bound of the modelled ranges, plus an id that does
    // not match its pattern. All are refused before any handler runs.
    let mut port_low = row.responder_body("token-port-low");
    port_low["port"] = json!(0);
    let mut port_high = row.responder_body("token-port-high");
    port_high["port"] = json!(65536);
    let mut bad_vpc = row.requester_body("token-vpc");
    bad_vpc["vpcId"] = json!("vpc-not-hex");
    let mut bad_subnet = row.requester_body("token-subnet");
    bad_subnet["subnetIds"] = json!(["subnet-1"]);

    for (case, path, body) in [
        ("port 0", "/responder-gateway", port_low),
        ("port 65536", "/responder-gateway", port_high),
        ("vpcId off pattern", "/requester-gateway", bad_vpc),
        ("subnetId too short", "/requester-gateway", bad_subnet),
    ] {
        let (status, answered) = fabric.send("POST", path, Some(body)).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected a request outside the modelled constraints to answer 400, got {status} {answered:?}"
        );
    }

    let (_, requester) = fabric
        .send(
            "POST",
            "/requester-gateway",
            Some(row.requester_body("token-requester")),
        )
        .await;
    let requester_id = text(&requester, "gatewayId");
    let (_, responder) = fabric
        .send(
            "POST",
            "/responder-gateway",
            Some(row.responder_body("token-responder")),
        )
        .await;
    let responder_id = text(&responder, "gatewayId");

    for (case, timeout) in [("timeout 99", 99_i64), ("timeout 5001", 5001_i64)] {
        let mut body = row.link_body(&responder_id);
        body["timeoutInMillis"] = json!(timeout);
        let (status, answered) = fabric
            .send(
                "POST",
                &format!("/gateway/{requester_id}/create-link"),
                Some(body),
            )
            .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected a timeoutInMillis outside 100..5000 to answer 400, got {status} {answered:?}"
        );
    }

    // And the bounds themselves are accepted.
    for (case, timeout) in [("timeout 100", 100_i64), ("timeout 5000", 5000_i64)] {
        let fabric = Fabric::new();
        let (_, requester) = fabric
            .send(
                "POST",
                "/requester-gateway",
                Some(row.requester_body("token-requester")),
            )
            .await;
        let requester_id = text(&requester, "gatewayId");
        let (_, responder) = fabric
            .send(
                "POST",
                "/responder-gateway",
                Some(row.responder_body("token-responder")),
            )
            .await;
        let responder_id = text(&responder, "gatewayId");
        let mut body = row.link_body(&responder_id);
        body["timeoutInMillis"] = json!(timeout);
        let (status, answered) = fabric
            .send(
                "POST",
                &format!("/gateway/{requester_id}/create-link"),
                Some(body),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected a timeoutInMillis on the published bound to be accepted, got {status} {answered:?}"
        );
    }
}

#[tokio::test]
async fn the_store_refuses_to_grow_past_its_own_quota() {
    let row = &ROWS[0];
    let fabric = Fabric::new();

    // Fill the requester-gateway store to the emulator's own bound. Nothing
    // releases a gateway record yet (DeleteRequesterGateway is still a stub), so
    // this quota is what keeps caller-driven state growth finite.
    for n in 0..GATEWAY_QUOTA {
        let (status, body) = fabric
            .send(
                "POST",
                "/requester-gateway",
                Some(row.requester_body(&format!("token-{n}"))),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {n}: expected a create below the quota to answer 200, got {status} {body:?}"
        );
    }

    let (status, refused) = fabric
        .send(
            "POST",
            "/requester-gateway",
            Some(row.requester_body("token-past-the-quota")),
        )
        .await;
    assert_eq!(
        status, 402,
        "expected the create past the store's quota of {GATEWAY_QUOTA} to be refused with the modelled ServiceQuotaExceededException (402), got {status} {refused:?}"
    );
    assert_eq!(
        refused["__type"],
        json!("ServiceQuotaExceededException"),
        "expected the modelled error code, got {:?}",
        refused["__type"]
    );

    // A retry of a token already recorded still answers from state: the quota
    // refuses new resources, not idempotent replays.
    let (status, replay) = fabric
        .send(
            "POST",
            "/requester-gateway",
            Some(row.requester_body("token-0")),
        )
        .await;
    assert_eq!(
        status, 200,
        "expected an idempotent replay to answer 200 even at the quota, got {status} {replay:?}"
    );
}

#[tokio::test]
async fn a_pending_resource_settles_once_the_clock_has_made_the_transition_due() {
    let row = &ROWS[2];
    let fabric = Fabric::new();

    let (_, requester) = fabric
        .send(
            "POST",
            "/requester-gateway",
            Some(row.requester_body("token-requester")),
        )
        .await;
    let requester_id = text(&requester, "gatewayId");
    let (_, responder) = fabric
        .send(
            "POST",
            "/responder-gateway",
            Some(row.responder_body("token-responder")),
        )
        .await;
    let responder_id = text(&responder, "gatewayId");
    let (_, link) = fabric
        .send(
            "POST",
            &format!("/gateway/{requester_id}/create-link"),
            Some(row.link_body(&responder_id)),
        )
        .await;
    let link_id = text(&link, "linkId");

    // Straight after the create, the transient status is still observable.
    let (_, pending) = fabric
        .send("GET", &format!("/requester-gateway/{requester_id}"), None)
        .await;
    assert_eq!(
        text(&pending, "status"),
        "PENDING_CREATION",
        "expected the gateway to still be PENDING_CREATION straight after the create, got {:?}",
        pending["status"]
    );

    // Once the clock has made the transition due, every reader sees the settled
    // status — nothing had to be polled for it to become due.
    tokio::time::sleep(PROVISIONING_DELAY + Duration::from_millis(250)).await;

    for (case, path, expected) in [
        (
            "requester gateway",
            format!("/requester-gateway/{requester_id}"),
            "ACTIVE",
        ),
        (
            "responder gateway",
            format!("/responder-gateway/{responder_id}"),
            "ACTIVE",
        ),
        (
            "link",
            format!("/gateway/{requester_id}/link/{link_id}"),
            "REQUESTED",
        ),
    ] {
        let (status, body) = fabric.send("GET", &path, None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200, got {status} {body:?}"
        );
        assert_eq!(
            text(&body, "status"),
            expected,
            "case {case:?}: expected the settled status {expected:?}, got {:?}",
            body["status"]
        );
    }

    // The settled link now counts as requested against the responder gateway.
    let (_, read_responder) = fabric
        .send("GET", &format!("/responder-gateway/{responder_id}"), None)
        .await;
    assert_eq!(
        read_responder["linksRequestedCount"],
        json!(1),
        "expected the outstanding link request to be counted, got {:?}",
        read_responder["linksRequestedCount"]
    );
}

// ---------------------------------------------------------------------------
// `ListLinks`: enumerating the links one gateway is an end of.
//
// The rule under test is about state, not about one recorded request: the
// listing reports exactly the stored links the addressed gateway is an end of,
// each rendered as that link's own reader renders it, in a stable total order, a
// page of at most `maxResults` at a time, handing out a `nextToken` only while
// links remain and resuming exactly after the page that issued it.
// ---------------------------------------------------------------------------

/// The page a `ListLinks` request that states no `maxResults` gets, as the
/// handler documents it; the model bounds neither a default nor a maximum for
/// that member.
const LIST_LINKS_DEFAULT_PAGE: usize =
    service_com_amazonaws_rtbfabric_rtbfabric::ops::list_links::DEFAULT_PAGE_SIZE;

/// How long a poll waits for a status the clock owes before it gives up. Far
/// above any span this service owes a transition, so a slow or loaded host
/// changes how many times the loop looks, never whether it succeeds.
const SETTLE_DEADLINE: Duration = Duration::from_secs(60);

/// How long a poll waits between two looks.
const SETTLE_POLL: Duration = Duration::from_millis(20);

/// The status the clock owes a link whose request this service has taken.
const SETTLED_LINK_STATUS: &str = "REQUESTED";

/// The members `ListLinksResponseStructure` and `GetLinkResponse` share, from
/// `smithy_query(query="shape", shape="ListLinksResponseStructure")`: a listed
/// entry and that link's own reading describe ONE resource, so each of these must
/// agree.
const SHARED_LINK_MEMBERS: &[&str] = &[
    "gatewayId",
    "peerGatewayId",
    "status",
    "createdAt",
    "updatedAt",
    "direction",
    "flowModules",
    "pendingFlowModules",
    "attributes",
    "logSettings",
    "connectivityType",
    "linkId",
    "tags",
];

/// A gateway and the links it is an end of, over the wire.
struct Hub {
    /// The requester gateway every link was opened from.
    gateway_id: String,
    /// Every link's `(linkId, peerGatewayId)`, sorted by link id — the order the
    /// listing must enumerate them in.
    links: Vec<(String, String)>,
}

impl Hub {
    fn link_ids(&self) -> Vec<String> {
        self.links.iter().map(|(id, _)| id.clone()).collect()
    }
}

impl Fabric {
    /// A requester gateway and `count` links opened from it, each to a responder
    /// gateway of its own: only one live link joins a pair, so a gateway with
    /// several links has several peers.
    async fn hub_with_links(&self, case: &str, row: &Row, count: usize, token: &str) -> Hub {
        let (status, requester) = self
            .send(
                "POST",
                "/requester-gateway",
                Some(row.requester_body(&format!("{token}-requester"))),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected CreateRequesterGateway to answer 200, got {status} {requester:?}"
        );
        let gateway_id = text(&requester, "gatewayId");

        let mut links = Vec::new();
        for n in 0..count {
            let (status, responder) = self
                .send(
                    "POST",
                    "/responder-gateway",
                    Some(row.responder_body(&format!("{token}-responder-{n}"))),
                )
                .await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected CreateResponderGateway #{n} to answer 200, got {status} {responder:?}"
            );
            let peer_id = text(&responder, "gatewayId");

            let (status, link) = self
                .send(
                    "POST",
                    &format!("/gateway/{gateway_id}/create-link"),
                    Some(row.link_body(&peer_id)),
                )
                .await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected CreateLink #{n} to answer 200, got {status} {link:?}"
            );
            links.push((text(&link, "linkId"), peer_id));
        }
        links.sort();
        Hub { gateway_id, links }
    }

    /// Read `path` until it reports `expected` as its `status`, and answer with
    /// that reading.
    ///
    /// The test waits for the CONDITION rather than for a span: it looks again
    /// until the status holds, giving up only once [`SETTLE_DEADLINE`] passed.
    /// Every reader that waits for a status the clock owes shares this one loop,
    /// so the polling rule is stated once.
    async fn poll_until_status(&self, case: &str, path: &str, expected: &str) -> Value {
        let deadline = std::time::Instant::now() + SETTLE_DEADLINE;
        loop {
            let (status, body) = self.send("GET", path, None).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected the read of {path:?} to answer 200, got {status} {body:?}"
            );
            if text(&body, "status") == expected {
                return body;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "case {case:?}: expected {path:?} to reach {expected:?}, got {:?}",
                body["status"]
            );
            tokio::time::sleep(SETTLE_POLL).await;
        }
    }

    /// Read a link through `GetLink` until it reports the status the clock owes
    /// it, and answer with that reading.
    ///
    /// Comparing a listing against this reading is only meaningful once nothing
    /// is owed, because a settle moves `status` and `updatedAt` together.
    async fn poll_settled_link(&self, case: &str, gateway_id: &str, link_id: &str) -> Value {
        let path = format!("/gateway/{gateway_id}/link/{link_id}");
        self.poll_until_status(case, &path, SETTLED_LINK_STATUS)
            .await
    }

    /// One `ListLinks` call, as `(the entries, the nextToken)`.
    async fn list_links(
        &self,
        case: &str,
        gateway_id: &str,
        max_results: Option<i64>,
        token: Option<&str>,
    ) -> (Vec<Value>, Option<String>) {
        let mut query: Vec<String> = Vec::new();
        if let Some(max_results) = max_results {
            query.push(format!("maxResults={max_results}"));
        }
        if let Some(token) = token {
            query.push(format!("nextToken={token}"));
        }
        let path = match query.is_empty() {
            true => format!("/gateway/{gateway_id}/links/"),
            false => format!("/gateway/{gateway_id}/links/?{}", query.join("&")),
        };

        let (status, body) = self.send("GET", &path, None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected ListLinks to answer 200 for {path:?}, got {status} {body:?}"
        );
        let entries = body["links"]
            .as_array()
            .unwrap_or_else(|| {
                panic!("case {case:?}: expected links to be a list for {path:?}, got {body:?}")
            })
            .clone();
        (entries, body["nextToken"].as_str().map(str::to_string))
    }

    /// Walk the whole listing of `gateway_id` at `max_results` per call.
    async fn walk_links(
        &self,
        case: &str,
        gateway_id: &str,
        max_results: Option<i64>,
    ) -> Vec<(Vec<Value>, Option<String>)> {
        let mut pages = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let page = self
                .list_links(case, gateway_id, max_results, token.as_deref())
                .await;
            token = page.1.clone();
            pages.push(page);
            if token.is_none() {
                break;
            }
            assert!(
                pages.len() <= 64,
                "case {case:?}: the walk did not terminate, {} page(s) so far",
                pages.len()
            );
        }
        pages
    }
}

/// One row of the `ListLinks` pagination table: how many links the addressed
/// gateway is an end of, the page size the walk asks for, and which shape of
/// request built the resources.
struct ListRow {
    name: &'static str,
    links: usize,
    /// `None` exercises the page a request that states no `maxResults` gets. The
    /// model declares no range for the member, so a value the other listings of
    /// this service could not carry reaches the handler here.
    max_results: Option<i64>,
    /// An index into [`ROWS`], so the listed entries differ in content as well as
    /// in number: tags, attributes and the optional link members present or not.
    shape: usize,
}

/// The rows differ along what the rule generalises over: how many links the
/// gateway holds relative to a page (none, fewer, exactly one page, several
/// pages with a short last one), the page size (unstated, one, a divisor, far
/// above the store, and a page of no results at all) and the shape of the links
/// themselves.
const LIST_ROWS: &[ListRow] = &[
    ListRow {
        name: "a gateway with no links",
        links: 0,
        max_results: None,
        shape: 0,
    },
    ListRow {
        name: "fewer links than the default page",
        links: 3,
        max_results: None,
        shape: 1,
    },
    ListRow {
        name: "exactly the default page",
        links: LIST_LINKS_DEFAULT_PAGE,
        max_results: None,
        shape: 0,
    },
    ListRow {
        name: "several default pages with a short last one",
        links: LIST_LINKS_DEFAULT_PAGE + 3,
        max_results: None,
        shape: 2,
    },
    ListRow {
        name: "one link per page",
        links: 4,
        max_results: Some(1),
        shape: 1,
    },
    ListRow {
        name: "a page size that divides the store exactly",
        links: 6,
        max_results: Some(3),
        shape: 2,
    },
    ListRow {
        name: "a page far larger than the store, which no modelled maximum caps",
        links: 2,
        max_results: Some(500),
        shape: 1,
    },
    ListRow {
        name: "a page of no results, which asks for nothing to continue from",
        links: 3,
        max_results: Some(0),
        shape: 0,
    },
];

impl ListRow {
    /// The page this row's request really gets: its own `maxResults` when that
    /// states a page at all, else the default.
    fn page_size(&self) -> usize {
        self.max_results
            .filter(|requested| *requested > 0)
            .and_then(|requested| usize::try_from(requested).ok())
            .unwrap_or(LIST_LINKS_DEFAULT_PAGE)
    }
}

#[tokio::test]
async fn the_link_listing_reports_every_link_of_the_gateway_exactly_once_in_pages() {
    for list_row in LIST_ROWS {
        let case = list_row.name;
        let fabric = Fabric::new();
        let row = &ROWS[list_row.shape];
        let hub = fabric
            .hub_with_links(case, row, list_row.links, "list")
            .await;

        // Nothing is owed before the comparison: a settle moves `status` and
        // `updatedAt`, so a listing taken across one would differ from the
        // reading for a reason that is not this rule.
        let mut readings = Vec::new();
        for (link_id, _) in &hub.links {
            readings.push(
                fabric
                    .poll_settled_link(case, &hub.gateway_id, link_id)
                    .await,
            );
        }

        let page_size = list_row.page_size();
        let pages = fabric
            .walk_links(case, &hub.gateway_id, list_row.max_results)
            .await;

        let expected_pages = list_row.links.div_euclid(page_size)
            + usize::from(list_row.links.rem_euclid(page_size) != 0 || list_row.links == 0);
        assert_eq!(
            pages.len(),
            expected_pages,
            "case {case:?}: expected {expected_pages} page(s) for {} link(s) at {page_size} per page, got {}",
            list_row.links,
            pages.len()
        );

        let mut seen = 0usize;
        for (index, (entries, token)) in pages.iter().enumerate() {
            assert!(
                entries.len() <= page_size,
                "case {case:?}: page {index} holds {} entries, more than the {page_size} the request allows: {entries:?}",
                entries.len()
            );
            seen += entries.len();
            let remaining = list_row.links.saturating_sub(seen);
            assert_eq!(
                token.is_some(),
                remaining > 0,
                "case {case:?}: with {remaining} of {} link(s) still unread, expected a nextToken present={}, got {token:?}",
                list_row.links,
                remaining > 0
            );
        }

        let walked: Vec<Value> = pages
            .iter()
            .flat_map(|(entries, _)| entries.iter().cloned())
            .collect();
        let walked_ids: Vec<String> = walked.iter().map(|entry| text(entry, "linkId")).collect();
        assert_eq!(
            walked_ids,
            hub.link_ids(),
            "case {case:?}: expected the walk to report every stored link exactly once in id order, got {walked_ids:?}"
        );

        // Every entry describes the stored record, member for member, as that
        // link's own reader describes it — so the listing reports state rather
        // than a second rendering of it.
        for (entry, reading) in walked.iter().zip(readings.iter()) {
            for member in SHARED_LINK_MEMBERS {
                assert_eq!(
                    entry[member], reading[member],
                    "case {case:?}: expected the listed {member:?} to be what GetLink reports ({:?}), got {:?}",
                    reading[member], entry[member]
                );
            }
            assert!(
                entry["publicEndpoint"].is_null(),
                "case {case:?}: a link opened through CreateLink has no public endpoint, got {:?}",
                entry["publicEndpoint"]
            );
        }
        for ((link_id, peer_id), entry) in hub.links.iter().zip(walked.iter()) {
            assert_eq!(
                text(entry, "peerGatewayId"),
                *peer_id,
                "case {case:?}: expected the link {link_id:?} to report its stored peer, got {entry:?}"
            );
        }
    }
}

#[tokio::test]
async fn the_link_listing_is_scoped_to_the_gateway_addressed_and_changes_nothing() {
    let case = "a store holding two gateways' links";
    let fabric = Fabric::new();
    let row = &ROWS[0];
    let hub = fabric.hub_with_links(case, row, 3, "scoped-hub").await;
    let other = fabric.hub_with_links(case, row, 2, "scoped-other").await;

    let (entries, token) = fabric.list_links(case, &hub.gateway_id, None, None).await;
    let listed: Vec<String> = entries.iter().map(|e| text(e, "linkId")).collect();
    assert_eq!(
        listed,
        hub.link_ids(),
        "case {case:?}: expected only the addressed gateway's links, got {listed:?}"
    );
    assert!(
        token.is_none(),
        "case {case:?}: a page that exhausts the enumeration hands out no token, got {token:?}"
    );
    for (foreign_id, _) in &other.links {
        assert!(
            !listed.contains(foreign_id),
            "case {case:?}: expected another gateway's link {foreign_id:?} to be absent, got {listed:?}"
        );
    }

    // A link joins two gateways, so it is listed from either end: each peer is an
    // end of exactly the one link it was created with.
    for (link_id, peer_id) in &hub.links {
        let (from_peer, _) = fabric.list_links(case, peer_id, None, None).await;
        let peer_ids: Vec<String> = from_peer.iter().map(|e| text(e, "linkId")).collect();
        assert_eq!(
            peer_ids,
            vec![link_id.clone()],
            "case {case:?}: expected the peer {peer_id:?} to list exactly its own link, got {peer_ids:?}"
        );
    }

    // A stored gateway that is an end of no link answers an empty list, never a
    // refusal: the gateway exists and holds nothing.
    let (status, lone) = fabric
        .send(
            "POST",
            "/requester-gateway",
            Some(row.requester_body("scoped-lone")),
        )
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected CreateRequesterGateway to answer 200, got {status} {lone:?}"
    );
    let lone_id = text(&lone, "gatewayId");
    let (empty, token) = fabric.list_links(case, &lone_id, None, None).await;
    assert!(
        empty.is_empty() && token.is_none(),
        "case {case:?}: expected an empty page and no token for a gateway with no links, got {empty:?} {token:?}"
    );

    // A gateway this service never stored is the not-found every other read of
    // this service answers with, not an empty page.
    let (status, refused) = fabric
        .send("GET", "/gateway/rtb-gw-unknown/links/", None)
        .await;
    assert_eq!(
        status, 404,
        "case {case:?}: expected an unknown gateway to be refused with 404, got {status} {refused:?}"
    );

    // The listing is a read: the same call answers the same body, and what it
    // reported is still readable afterwards.
    let (again, _) = fabric.list_links(case, &hub.gateway_id, None, None).await;
    assert_eq!(
        again, entries,
        "case {case:?}: expected a repeated listing to answer identically, got {again:?} after {entries:?}"
    );

    // What the store releases, the listing stops reporting — and only that.
    let (dropped_id, _) = hub.links[1].clone();
    let (status, deleted) = fabric
        .send(
            "DELETE",
            &format!("/gateway/{}/link/{dropped_id}", hub.gateway_id),
            None,
        )
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected DeleteLink to answer 200, got {status} {deleted:?}"
    );
    let (after, _) = fabric.list_links(case, &hub.gateway_id, None, None).await;
    let after_ids: Vec<String> = after.iter().map(|e| text(e, "linkId")).collect();
    let expected: Vec<String> = hub
        .link_ids()
        .into_iter()
        .filter(|id| *id != dropped_id)
        .collect();
    assert_eq!(
        after_ids, expected,
        "case {case:?}: expected the released link {dropped_id:?} to be gone and the rest kept, got {after_ids:?}"
    );
}

#[tokio::test]
async fn a_link_listing_token_this_operation_never_issued_is_refused() {
    let case = "an unrecognised pagination token";
    let fabric = Fabric::new();
    let row = &ROWS[0];
    let hub = fabric.hub_with_links(case, row, 4, "token").await;

    // A token another listing of this service issued is not this walk's to
    // resume from: the issuing operation travels inside the token. Two requester
    // gateways are stored, so a page of one leaves that listing more to report.
    let (status, second_requester) = fabric
        .send(
            "POST",
            "/requester-gateway",
            Some(row.requester_body("token-second-requester")),
        )
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected a second CreateRequesterGateway to answer 200, got {status} {second_requester:?}"
    );
    let (status, requester_page) = fabric
        .send("GET", "/requester-gateways?maxResults=1", None)
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected ListRequesterGateways to answer 200, got {status} {requester_page:?}"
    );
    let foreign_token = text(&requester_page, "nextToken");
    assert!(
        !foreign_token.is_empty(),
        "case {case:?}: expected ListRequesterGateways to hand out a token with gateways left, got {requester_page:?}"
    );

    for (token_case, token) in [
        ("not hex at all", "not-a-token".to_string()),
        ("an odd number of hex digits", "abc".to_string()),
        ("hex carrying no cursor", "6162".to_string()),
        // Hex of "link-0000000000000001" alone: a cursor with no issuing
        // operation and no issuing instant is not a token this listing hands out.
        (
            "hex of a bare link id",
            "6c696e6b2d30303030303030303030303030303031".to_string(),
        ),
        ("far longer than any issued token", "61".repeat(256)),
        ("another listing's token", foreign_token),
    ] {
        let (status, body) = fabric
            .send(
                "GET",
                &format!("/gateway/{}/links/?nextToken={token}", hub.gateway_id),
                None,
            )
            .await;
        assert_eq!(
            status, 400,
            "case {token_case:?}: expected {token:?} to be refused with 400, got {status} {body:?}"
        );
    }

    // Its own token still resumes the walk, exactly after the page that issued it.
    let (first, token) = fabric.list_links(case, &hub.gateway_id, Some(2), None).await;
    let token = token.unwrap_or_else(|| panic!("case {case:?}: expected a token after a short page"));
    let (second, _) = fabric
        .list_links(case, &hub.gateway_id, Some(2), Some(&token))
        .await;
    let walked: Vec<String> = first
        .iter()
        .chain(second.iter())
        .map(|entry| text(entry, "linkId"))
        .collect();
    assert_eq!(
        walked,
        hub.link_ids(),
        "case {case:?}: expected the two pages to cover every link once, got {walked:?}"
    );
}

// ---------------------------------------------------------------------------
// Outbound external links: `CreateOutboundExternalLink` and
// `GetOutboundExternalLink`.
//
// A link to an endpoint OUTSIDE the fabric has one end in a requester gateway
// and one at a public address, so it carries no peer gateway, nothing accepts
// it, and it is addressed only through the gateway it was opened from. The same
// rule as above holds over it: the create persists what it was given and the
// read reports exactly that, including the members the create's own response
// shape does not carry.
// ---------------------------------------------------------------------------

/// The status the create of an outbound external link answers with: nothing
/// accepts such a link, so it starts at the `LinkStatus` member that reports an
/// accepted creation.
const EXTERNAL_LINK_PENDING_STATUS: &str = "PENDING_CREATION";

/// The status the clock owes that link once its creation is through.
const EXTERNAL_LINK_SETTLED_STATUS: &str = "ACTIVE";

/// The `ConnectivityType` member an OUTBOUND external link carries: its traffic
/// leaves the fabric for a public endpoint, from `CONNECTIVITY_TYPE_VALUES`.
const EXTERNAL_LINK_CONNECTIVITY_TYPE: &str = "PUBLIC_EGRESS";

/// `LinkLogSettings` whose sampling percentages are `error` and `filter`, each
/// inside the modelled 0..100 range (`smithy_query(query="shape",
/// shape="LinkApplicationLogSampling")`).
fn log_settings_at(error: f64, filter: f64) -> Value {
    json!({ "applicationLogs": { "sampling": { "errorLog": error, "filterLog": filter } } })
}

/// Whether two JSON values state the same thing, comparing numbers by value so a
/// percentage the wire carries as `100.0` matches one written `100`.
fn same_json(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Number(left), Value::Number(right)) => match (left.as_f64(), right.as_f64()) {
            (Some(left), Some(right)) => left == right,
            _ => left == right,
        },
        (Value::Array(left), Value::Array(right)) => {
            left.len() == right.len()
                && left
                    .iter()
                    .zip(right.iter())
                    .all(|(left, right)| same_json(left, right))
        }
        (Value::Object(left), Value::Object(right)) => {
            left.len() == right.len()
                && left
                    .iter()
                    .all(|(key, left)| right.get(key).is_some_and(|right| same_json(left, right)))
        }
        _ => left == right,
    }
}

/// The route one outbound external link is read and deleted at, as the model binds
/// it.
fn outbound_external_link_path(gateway_id: &str, link_id: &str) -> String {
    format!("/requester-gateway/{gateway_id}/outbound-external-link/{link_id}")
}

/// One row of the outbound-external-link table.
struct ExternalRow {
    case: &'static str,
    /// A `URL`: len 0..255, `^(https|http)://.+$`.
    public_endpoint: String,
    /// A `LinkAttributes`, or `Value::Null` for a link that carries none.
    attributes: Value,
    /// A `LinkLogSettings`, which the create requires.
    log_settings: Value,
    /// A `TagsMap`, or `Value::Null` for a link that carries no tags.
    tags: Value,
}

impl ExternalRow {
    fn create_body(&self, token: &str) -> Value {
        let mut body = json!({
            "clientToken": token,
            "publicEndpoint": self.public_endpoint,
            "logSettings": self.log_settings,
        });
        let object = body.as_object_mut().expect("a JSON object body");
        if !self.attributes.is_null() {
            object.insert("attributes".to_string(), self.attributes.clone());
        }
        if !self.tags.is_null() {
            object.insert("tags".to_string(), self.tags.clone());
        }
        body
    }
}

/// The outbound-external-link table. The rows differ along what the rule
/// generalises over: the form and the length of the endpoint (scheme, host, port,
/// path, up to the published 255-character bound of `URL`), which members of
/// `attributes` are stated and whether any are, how many tags the link carries,
/// and the sampling percentages, including each published bound of that range.
fn external_rows() -> Vec<ExternalRow> {
    vec![
        ExternalRow {
            case: "an https endpoint, no attributes, no tags, sampling at both bounds",
            public_endpoint: "https://bidder.example.com/rtb".to_string(),
            attributes: Value::Null,
            log_settings: log_settings_at(100.0, 0.0),
            tags: Value::Null,
        },
        ExternalRow {
            case: "an http endpoint with a port, a customer-provided id, one tag",
            public_endpoint: "http://198.51.100.10:8080/bid".to_string(),
            attributes: json!({ "customerProvidedId": "exchange-west-1" }),
            log_settings: log_settings_at(12.5, 37.25),
            tags: tags_json(&[("team", "rtb")]),
        },
        ExternalRow {
            // `ResponderErrorMaskingForHttpCode`: `httpCode` is
            // `^DEFAULT|4XX|5XX|\d{3}$`, `action` is NO_BID or PASSTHROUGH and
            // `loggingTypes` holds one or two of NONE, METRIC, RESPONSE
            // (`smithy_query(query="shape",
            // shape="ResponderErrorMaskingForHttpCode")`).
            case: "error masking for one HTTP code, three tags",
            public_endpoint: "https://a.example.com/".to_string(),
            attributes: json!({
                "responderErrorMasking": [{
                    "httpCode": "5XX",
                    "action": "NO_BID",
                    "loggingTypes": ["METRIC"],
                    "responseLoggingPercentage": 42.5,
                }],
            }),
            log_settings: log_settings_at(0.0, 100.0),
            tags: tags_json(&[("team", "rtb"), ("env", "test"), ("owner", "fabric")]),
        },
        ExternalRow {
            case: "error masking for two codes with both logging types, and an id",
            public_endpoint: "https://bidder.example.com/openrtb/2.5/bid?partner=west".to_string(),
            attributes: json!({
                "customerProvidedId": "exchange_east-2",
                "responderErrorMasking": [
                    {
                        "httpCode": "DEFAULT",
                        "action": "PASSTHROUGH",
                        "loggingTypes": ["METRIC", "RESPONSE"],
                        "responseLoggingPercentage": 100.0,
                    },
                    {
                        "httpCode": "503",
                        "action": "NO_BID",
                        "loggingTypes": ["NONE"],
                    },
                ],
            }),
            log_settings: log_settings_at(99.9, 0.1),
            tags: tags_json(&[("env", "prod")]),
        },
        ExternalRow {
            // The published upper bound of `URL`'s length: 25 + 230 = 255.
            case: "an endpoint exactly at the published length bound",
            public_endpoint: format!("https://long.example.com/{}", "a".repeat(230)),
            attributes: json!({ "customerProvidedId": "a" }),
            log_settings: log_settings_at(50.0, 50.0),
            tags: Value::Null,
        },
    ]
}

impl Fabric {
    /// A stored requester gateway, as `CreateRequesterGateway` mints one.
    async fn requester_gateway(&self, case: &str, token: &str) -> String {
        let (status, body) = self
            .send("POST", "/requester-gateway", Some(ROWS[0].requester_body(token)))
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected CreateRequesterGateway to answer 200, got {status} {body:?}"
        );
        text(&body, "gatewayId")
    }

    /// `POST /requester-gateway/{gatewayId}/outbound-external-link`.
    async fn create_outbound_external_link(
        &self,
        gateway_id: &str,
        body: Value,
    ) -> (u16, Value) {
        self.send(
            "POST",
            &format!("/requester-gateway/{gateway_id}/outbound-external-link"),
            Some(body),
        )
        .await
    }

    /// `GET /requester-gateway/{gatewayId}/outbound-external-link/{linkId}`.
    async fn read_outbound_external_link(
        &self,
        gateway_id: &str,
        link_id: &str,
    ) -> (u16, Value) {
        self.send(
            "GET",
            &outbound_external_link_path(gateway_id, link_id),
            None,
        )
        .await
    }

    /// Read the outbound external link until it reports the status the clock owes
    /// it, and answer with that reading — through the same polling loop every
    /// other reader of a clock-owed status uses.
    async fn poll_settled_outbound_external_link(
        &self,
        case: &str,
        gateway_id: &str,
        link_id: &str,
    ) -> Value {
        let path = outbound_external_link_path(gateway_id, link_id);
        self.poll_until_status(case, &path, EXTERNAL_LINK_SETTLED_STATUS)
            .await
    }
}

#[tokio::test]
async fn an_outbound_external_link_round_trips_through_state() {
    for row in external_rows() {
        let case = row.case;
        let fabric = Fabric::new();
        let gateway_id = fabric.requester_gateway(case, "token-requester").await;

        let (status, created) = fabric
            .create_outbound_external_link(&gateway_id, row.create_body("token-link"))
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected CreateOutboundExternalLink to answer 200, got {status} {created:?}"
        );
        let link_id = text(&created, "linkId");
        assert_eq!(
            text(&created, "gatewayId"),
            gateway_id,
            "case {case:?}: expected the create to report the gateway it was called on, got {created:?}"
        );
        assert!(
            is_link_id(&link_id),
            "case {case:?}: expected a minted linkId matching the modelled pattern, got {link_id:?}"
        );
        assert_eq!(
            text(&created, "status"),
            EXTERNAL_LINK_PENDING_STATUS,
            "case {case:?}: expected the create to report an accepted creation, got {created:?}"
        );

        // The read reports what the create PERSISTED, not what the read was
        // given: every member below reached state through the create's body.
        let (status, read) = fabric
            .read_outbound_external_link(&gateway_id, &link_id)
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected GetOutboundExternalLink to answer 200, got {status} {read:?}"
        );
        assert_eq!(
            (text(&read, "gatewayId"), text(&read, "linkId")),
            (gateway_id.clone(), link_id.clone()),
            "case {case:?}: expected the read to report the link that was stored, got {read:?}"
        );
        assert_eq!(
            text(&read, "publicEndpoint"),
            row.public_endpoint,
            "case {case:?}: expected the stored publicEndpoint, got {:?}",
            read["publicEndpoint"]
        );
        assert!(
            same_json(&read["attributes"], &row.attributes),
            "case {case:?}: expected the stored attributes {:?}, got {:?}",
            row.attributes,
            read["attributes"]
        );
        assert!(
            same_json(&read["logSettings"], &row.log_settings),
            "case {case:?}: expected the stored logSettings {:?}, got {:?}",
            row.log_settings,
            read["logSettings"]
        );
        assert!(
            same_json(&read["tags"], &row.tags),
            "case {case:?}: expected the stored tags {:?}, got {:?}",
            row.tags,
            read["tags"]
        );
        assert_eq!(
            text(&read, "connectivityType"),
            EXTERNAL_LINK_CONNECTIVITY_TYPE,
            "case {case:?}: expected the connectivity type of an outbound external link, got {read:?}"
        );
        assert!(
            CONNECTIVITY_TYPE_VALUES.contains(&text(&read, "connectivityType").as_str())
                && LINK_STATUS_VALUES.contains(&text(&read, "status").as_str()),
            "case {case:?}: expected modelled ConnectivityType and LinkStatus members, got {read:?}"
        );
        // No module flow is configured until `UpdateLinkModuleFlow` runs, and the
        // read reports that rather than leaving the member out.
        for member in ["flowModules", "pendingFlowModules"] {
            assert_eq!(
                read[member],
                json!([]),
                "case {case:?}: expected {member} to be reported empty, got {:?}",
                read[member]
            );
        }
        let created_at = read["createdAt"].as_f64();
        assert!(
            created_at.is_some() && read["updatedAt"].as_f64() >= created_at,
            "case {case:?}: expected createdAt and updatedAt to be reported, got {:?} and {:?}",
            read["createdAt"],
            read["updatedAt"]
        );

        // The gateway's own reader counts it: an external link is one of that
        // gateway's links, and `totalLinksCount` is derived by scanning them.
        let (status, gateway) = fabric
            .send("GET", &format!("/requester-gateway/{gateway_id}"), None)
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected GetRequesterGateway to answer 200, got {status} {gateway:?}"
        );
        assert_eq!(
            gateway["totalLinksCount"],
            json!(1),
            "case {case:?}: expected the gateway to count its one outbound external link, got {:?}",
            gateway["totalLinksCount"]
        );

        // And the link is one record of this gateway's: the store holds exactly
        // the one the create minted.
        let (status, again) = fabric
            .read_outbound_external_link(&gateway_id, &link_id)
            .await;
        assert_eq!(
            (status, same_json(&again, &read)),
            (200, true),
            "case {case:?}: expected a repeated read to report the same link, got {status} {again:?}"
        );
    }
}

#[tokio::test]
async fn an_outbound_external_link_settles_to_the_status_the_clock_owes_it() {
    let case = "a link whose creation the clock has made due";
    let fabric = Fabric::new();
    let gateway_id = fabric.requester_gateway(case, "token-requester").await;
    let row = &external_rows()[1];

    let (status, created) = fabric
        .create_outbound_external_link(&gateway_id, row.create_body("token-link"))
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the create to answer 200, got {status} {created:?}"
    );
    let link_id = text(&created, "linkId");

    let settled = fabric
        .poll_settled_outbound_external_link(case, &gateway_id, &link_id)
        .await;
    assert_eq!(
        text(&settled, "publicEndpoint"),
        row.public_endpoint,
        "case {case:?}: expected the settle to leave the stored endpoint alone, got {settled:?}"
    );
    assert!(
        settled["updatedAt"].as_f64() >= settled["createdAt"].as_f64(),
        "case {case:?}: expected the settle to date the record at the instant it was owed, got {:?} and {:?}",
        settled["createdAt"],
        settled["updatedAt"]
    );

    // A later read answers the same thing: the settle is a change the clock had
    // already made due, not one a read makes.
    let again = fabric
        .poll_settled_outbound_external_link(case, &gateway_id, &link_id)
        .await;
    assert!(
        same_json(&again, &settled),
        "case {case:?}: expected a repeated read to report the same link, got {again:?}"
    );
}

#[tokio::test]
async fn an_outbound_external_link_resolves_only_through_the_gateway_that_opened_it() {
    let case = "ids that resolve to no stored outbound external link";
    let fabric = Fabric::new();
    let row = &external_rows()[0];

    let gateway_id = fabric.requester_gateway(case, "token-requester").await;
    let other_gateway_id = fabric.requester_gateway(case, "token-other").await;
    let (status, responder) = fabric
        .send(
            "POST",
            "/responder-gateway",
            Some(ROWS[0].responder_body("token-responder")),
        )
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected CreateResponderGateway to answer 200, got {status} {responder:?}"
    );
    let responder_id = text(&responder, "gatewayId");

    let (status, created) = fabric
        .create_outbound_external_link(&gateway_id, row.create_body("token-link"))
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the create to answer 200, got {status} {created:?}"
    );
    let link_id = text(&created, "linkId");

    // An outbound external link is opened FROM a requester gateway, so a create
    // on a gateway this store holds no requester gateway for is refused — and a
    // responder gateway of that id is no requester gateway.
    for (what, create_on) in [
        ("a gateway nothing created", "rtb-gw-nothing".to_string()),
        ("a responder gateway", responder_id.clone()),
    ] {
        let (status, refused) = fabric
            .create_outbound_external_link(&create_on, row.create_body("token-refused"))
            .await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected a create on {what} to answer 404, got {status} {refused:?}"
        );
        assert_eq!(
            text(&refused, "message"),
            RESOURCE_NOT_FOUND_MESSAGE,
            "case {case:?}: expected the service's not-found sentence for {what}, got {refused:?}"
        );
    }

    // A read misses whenever the pair names no stored link — including this
    // link's own id read through ANOTHER gateway, which is what keeps one
    // gateway's links out of another's.
    for (what, read_gateway, read_link) in [
        (
            "a gateway nothing created",
            "rtb-gw-nothing".to_string(),
            link_id.clone(),
        ),
        (
            "a link nothing minted",
            gateway_id.clone(),
            "link-nothing".to_string(),
        ),
        (
            "this link's id under another requester gateway",
            other_gateway_id.clone(),
            link_id.clone(),
        ),
        (
            "a responder gateway's id",
            responder_id.clone(),
            link_id.clone(),
        ),
    ] {
        let (status, refused) = fabric
            .read_outbound_external_link(&read_gateway, &read_link)
            .await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected the read of {what} to answer 404, got {status} {refused:?}"
        );
        assert_eq!(
            text(&refused, "message"),
            RESOURCE_NOT_FOUND_MESSAGE,
            "case {case:?}: expected the service's not-found sentence for {what}, got {refused:?}"
        );
    }

    // A create may assign at most as many tags as a resource holds — the same
    // per-resource bound `TagResource` enforces — and one past it is refused
    // before anything is stored.
    let mut over_quota = row.create_body("token-tags");
    over_quota["tags"] = Value::Object(
        (0..=TAGS_PER_RESOURCE_QUOTA)
            .map(|n| (format!("key-{n}"), json!("v")))
            .collect(),
    );
    let (status, refused) = fabric
        .create_outbound_external_link(&gateway_id, over_quota)
        .await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected one tag past the per-resource bound to be refused with 400, got {status} {refused:?}"
    );
    let mut at_quota = row.create_body("token-tags-at-bound");
    at_quota["tags"] = Value::Object(
        (0..TAGS_PER_RESOURCE_QUOTA)
            .map(|n| (format!("key-{n}"), json!("v")))
            .collect(),
    );
    let (status, accepted) = fabric
        .create_outbound_external_link(&gateway_id, at_quota)
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected a create exactly at the per-resource tag bound to be served, got {status} {accepted:?}"
    );

    // The link that IS stored still reads back, so the refusals above are the
    // pair missing rather than the read path being closed.
    let (status, read) = fabric
        .read_outbound_external_link(&gateway_id, &link_id)
        .await;
    assert_eq!(
        (status, text(&read, "linkId")),
        (200, link_id.clone()),
        "case {case:?}: expected the stored link to read back, got {status} {read:?}"
    );

    // A link cannot outlive the gateway it was opened from: once that gateway is
    // released, nothing reports the link either.
    let (status, deleted) = fabric
        .send("DELETE", &format!("/requester-gateway/{gateway_id}"), None)
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected DeleteRequesterGateway to answer 200, got {status} {deleted:?}"
    );
    let (status, gone) = fabric
        .read_outbound_external_link(&gateway_id, &link_id)
        .await;
    assert_eq!(
        status, 404,
        "case {case:?}: expected the link of a released gateway to be gone, got {status} {gone:?}"
    );
}

#[tokio::test]
async fn a_repeated_client_token_answers_with_the_first_external_link() {
    for row in external_rows() {
        let case = row.case;
        let fabric = Fabric::new();
        let gateway_id = fabric.requester_gateway(case, "token-requester").await;

        let (status, first) = fabric
            .create_outbound_external_link(&gateway_id, row.create_body("token-link"))
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the first create to answer 200, got {status} {first:?}"
        );
        let (status, replay) = fabric
            .create_outbound_external_link(&gateway_id, row.create_body("token-link"))
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the replayed create to answer 200, got {status} {replay:?}"
        );
        assert_eq!(
            text(&replay, "linkId"),
            text(&first, "linkId"),
            "case {case:?}: expected the replay to answer with the first link, got {replay:?}"
        );

        // The replay minted nothing: the one link the first call stored is what
        // the gateway holds.
        let (status, read) = fabric
            .read_outbound_external_link(&gateway_id, &text(&first, "linkId"))
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the one stored link to read back, got {status} {read:?}"
        );

        // The same token with different parameters is refused rather than
        // silently answering for parameters the caller did not send.
        let mut changed = row.create_body("token-link");
        changed["publicEndpoint"] = json!("https://changed.example.com/rtb");
        let (status, refused) = fabric
            .create_outbound_external_link(&gateway_id, changed)
            .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected a reused clientToken with changed parameters to be refused with 400, got {status} {refused:?}"
        );
    }
}

