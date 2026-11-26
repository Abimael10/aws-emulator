// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for `GetInboundExternalLink` as a REPORT of the stored inbound
//! external link, driven over the wire through the router the production emulator
//! uses.
//!
//! The rule under test is about state, not about one recorded request: the read
//! answers with the link record as the store holds it at the instant of the call —
//! the `domainName` the create assigned, the `attributes`, `logSettings` and `tags`
//! the writes left on that record, the status the clock has brought it to — and it
//! changes nothing a later caller sees, so two readings at one instant agree and the
//! members no call has touched never move.
//!
//! The table spans what the rule generalises over: whether the create states
//! `attributes` and which of its members, the sampling percentages (including each
//! published bound of the `Percentage` range), how many tags the record starts with,
//! how many a later `TagResource` assigns and whether it adds or replaces, and how
//! many of them a later `UntagResource` removes — down to a record left holding none
//! at all.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use serde_json::{json, Value};
use service_com_amazonaws_rtbfabric_rtbfabric::ops::types::{
    CONNECTIVITY_TYPE_VALUES, LINK_STATUS_VALUES,
};
use service_com_amazonaws_rtbfabric_rtbfabric::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

use crate::resource_tagging::{link_arn, tags_path};

/// The Region the test signs for; it is also the zone an assigned name sits under
/// and the Region every ARN this suite builds names.
const REGION: &str = "us-west-2";

/// The `LinkStatus` member a fresh inbound external link is stored at, and the one
/// the clock owes it — both from `LINK_STATUS_VALUES` (this service's modelled
/// `LinkStatus`), which every reading below is also checked against.
const PENDING_CREATION: &str = "PENDING_CREATION";
const ACTIVE: &str = "ACTIVE";

/// The `ConnectivityType` member a link whose traffic arrives from outside the
/// fabric carries, from `CONNECTIVITY_TYPE_VALUES`.
const EXTERNAL_INBOUND: &str = "EXTERNAL_INBOUND";

/// How long a reader waits for the status the clock owes the record. Generous: the
/// suite waits for the CONDITION, and the deadline only stops a hung loop.
const SETTLE_DEADLINE: Duration = Duration::from_secs(60);

/// How long a reader waits between two looks at the same record.
const SETTLE_POLL: Duration = Duration::from_millis(20);

/// One answered call: the status and the parsed body.
struct Answer {
    status: u16,
    body: Value,
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
        let body = if response.body().is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(response.body()).unwrap_or(Value::Null)
        };
        Answer { status, body }
    }

    /// A stored responder gateway of the `EXTERNAL` type — the one type the
    /// inbound-external-link surface exists for — and its minted id.
    async fn external_responder_gateway(&self, case: &str) -> String {
        let answered = self
            .send(
                "POST",
                "/responder-gateway",
                Some(json!({
                    "vpcId": "vpc-0123456789abcdef0",
                    "subnetIds": ["subnet-0123456789abcdef0"],
                    "securityGroupIds": ["sg-0123456789abcdef0"],
                    "port": 443,
                    "protocol": "HTTPS",
                    "listenerConfig": { "protocols": ["HTTPS"] },
                    "domainName": "responder.example.com",
                    "gatewayType": "EXTERNAL",
                    "clientToken": "token-responder",
                })),
            )
            .await;
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

    /// `GET /responder-gateway/{gatewayId}/inbound-external-link/{linkId}`, asserting
    /// it resolved.
    async fn read(&self, case: &str, gateway_id: &str, link_id: &str) -> Value {
        let answered = self
            .send(
                "GET",
                &format!("/responder-gateway/{gateway_id}/inbound-external-link/{link_id}"),
                None,
            )
            .await;
        assert_eq!(
            answered.status, 200,
            "case {case:?}: expected GetInboundExternalLink to answer 200, got {} {:?}",
            answered.status, answered.body
        );
        answered.body
    }

    /// Read the link until it reports the status the clock owes it, waiting for that
    /// CONDITION rather than for a span and giving up only once [`SETTLE_DEADLINE`]
    /// has passed.
    async fn poll_until_active(&self, case: &str, gateway_id: &str, link_id: &str) -> Value {
        let deadline = Instant::now() + SETTLE_DEADLINE;
        loop {
            let read = self.read(case, gateway_id, link_id).await;
            if text(&read, "status") == ACTIVE {
                return read;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: expected the link to report {ACTIVE:?} once the clock had made \
                 the transition due, got {:?}",
                read["status"]
            );
            tokio::time::sleep(SETTLE_POLL).await;
        }
    }

    /// `ListTagsForResource` for the link, as a sorted map.
    async fn listed_tags(&self, case: &str, resource_arn: &str) -> BTreeMap<String, String> {
        let answered = self.send("GET", &tags_path(resource_arn), None).await;
        assert_eq!(
            answered.status, 200,
            "case {case:?}: expected ListTagsForResource to answer 200, got {} {:?}",
            answered.status, answered.body
        );
        tag_map(&answered.body["tags"])
    }
}

fn text(body: &Value, member: &str) -> String {
    body[member].as_str().unwrap_or_default().to_string()
}

/// A `tags` member as a sorted map, so two tag sets compare independently of the
/// order they were written in. A member the answer leaves out is a record holding no
/// tags, which is the empty map.
fn tag_map(tags: &Value) -> BTreeMap<String, String> {
    match tags.as_object() {
        Some(object) => object
            .iter()
            .filter_map(|(key, value)| Some((key.clone(), value.as_str()?.to_string())))
            .collect(),
        None => BTreeMap::new(),
    }
}

fn tags_json(tags: &[(&str, &str)]) -> Value {
    Value::Object(
        tags.iter()
            .map(|(key, value)| ((*key).to_string(), json!(value)))
            .collect(),
    )
}

/// The members of the read no call in this suite changes: an assignment or a removal
/// of tags, and the status transition the clock owes, may not move any of them.
fn untouched(body: &Value) -> Value {
    json!({
        "gatewayId": body["gatewayId"],
        "linkId": body["linkId"],
        "domainName": body["domainName"],
        "connectivityType": body["connectivityType"],
        "attributes": body["attributes"],
        "logSettings": body["logSettings"],
        "flowModules": body["flowModules"],
        "pendingFlowModules": body["pendingFlowModules"],
        "createdAt": body["createdAt"],
    })
}

/// `LinkLogSettings` with its one required member, as `smithy_query(query="shape",
/// shape="LinkLogSettings")` declares it. The percentages are a `Percentage`, whose
/// published range is 0..100.
fn log_settings(error_log: f64, filter_log: f64) -> Value {
    json!({ "applicationLogs": { "sampling": { "errorLog": error_log, "filterLog": filter_log } } })
}

/// One row of the table.
struct Row {
    name: &'static str,
    /// A `LinkAttributes`, or `Value::Null` for a link that carries none.
    attributes: Value,
    /// The `LinkLogSettings` the create requires.
    log_settings: Value,
    /// The tags the create persists on the link.
    created: &'static [(&'static str, &'static str)],
    /// The batch a later `TagResource` assigns.
    assigned: &'static [(&'static str, &'static str)],
    /// The keys a later `UntagResource` names; never empty, because `tagKeys` is
    /// `@required` with a minimum of one member.
    removed: &'static [&'static str],
}

impl Row {
    fn create_body(&self) -> Value {
        let mut body = json!({
            "clientToken": "token-link",
            "logSettings": self.log_settings,
        });
        let object = body.as_object_mut().expect("a JSON object body");
        if !self.attributes.is_null() {
            object.insert("attributes".to_string(), self.attributes.clone());
        }
        if !self.created.is_empty() {
            object.insert("tags".to_string(), tags_json(self.created));
        }
        body
    }

    fn created_tags(&self) -> BTreeMap<String, String> {
        self.created
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect()
    }

    /// The map the record holds once `TagResource` has run: the assignment adds its
    /// keys and replaces the value of a key already there.
    fn after_assignment(&self) -> BTreeMap<String, String> {
        let mut tags = self.created_tags();
        for (key, value) in self.assigned {
            tags.insert((*key).to_string(), (*value).to_string());
        }
        tags
    }

    /// The map the record holds once `UntagResource` has removed its keys.
    fn after_removal(&self) -> BTreeMap<String, String> {
        let mut tags = self.after_assignment();
        for key in self.removed {
            tags.remove(*key);
        }
        tags
    }
}

/// The table. The rows differ along what the rule generalises over: whether the
/// create states `attributes` and which members of it, the sampling percentages
/// (each published bound of the range among them), how many tags the record starts
/// with, whether the later assignment adds or replaces, and how much of the map the
/// removal takes away — the first row stripping it back to no tags at all.
fn rows() -> Vec<Row> {
    vec![
        Row {
            name: "no attributes, no tags, two assigned and both removed",
            attributes: Value::Null,
            log_settings: log_settings(100.0, 0.0),
            created: &[],
            assigned: &[("team", "rtb"), ("env", "test")],
            removed: &["team", "env"],
        },
        Row {
            name: "a customer id, one tag, one replaced and one added, the added one removed",
            attributes: json!({ "customerProvidedId": "exchange-north-1" }),
            log_settings: log_settings(12.5, 37.25),
            created: &[("team", "rtb")],
            assigned: &[("team", "fabric"), ("stage", "gamma")],
            removed: &["stage", "never-assigned"],
        },
        Row {
            name: "error masking for two codes, three tags, one assigned and two removed",
            // `ResponderErrorMaskingForHttpCode`: `httpCode` is
            // `^DEFAULT|4XX|5XX|\d{3}$`, `action` is NO_BID or PASSTHROUGH and
            // `loggingTypes` holds members of NONE, METRIC, RESPONSE
            // (`smithy_query(query="shape", shape="ResponderErrorMaskingForHttpCode")`).
            attributes: json!({
                "customerProvidedId": "exchange_south-2",
                "responderErrorMasking": [
                    {
                        "httpCode": "5XX",
                        "action": "PASSTHROUGH",
                        "loggingTypes": ["METRIC", "RESPONSE"],
                        "responseLoggingPercentage": 100.0,
                    },
                    {
                        "httpCode": "429",
                        "action": "NO_BID",
                        "loggingTypes": ["NONE"],
                    },
                ],
            }),
            log_settings: log_settings(0.0, 100.0),
            created: &[("team", "rtb"), ("env", "test"), ("owner", "fabric")],
            assigned: &[("owner", "exchange")],
            removed: &["team", "env"],
        },
    ]
}

/// The `UntagResource` path: the resource in the label, the keys in the repeated
/// `tagKeys` query parameter the model binds, each percent-encoded the way an AWS
/// SDK encodes an `@httpQuery` value.
fn untag_path(resource_arn: &str, tag_keys: &[&str]) -> String {
    let query: Vec<String> = tag_keys
        .iter()
        .map(|key| format!("tagKeys={}", utf8_percent_encode(key, NON_ALPHANUMERIC)))
        .collect();
    format!("{}?{}", tags_path(resource_arn), query.join("&"))
}

#[tokio::test]
async fn the_read_reports_the_inbound_external_link_as_state_holds_it() {
    for row in rows() {
        let case = row.name;
        let fabric = Fabric::new();
        let gateway_id = fabric.external_responder_gateway(case).await;

        let created = fabric
            .create_inbound_external_link(&gateway_id, row.create_body())
            .await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: expected CreateInboundExternalLink to answer 200, got {} {:?}",
            created.status, created.body
        );
        let link_id = text(&created.body, "linkId");
        let domain_name = text(&created.body, "domainName");

        // The read answers about the record the create stored, with the members the
        // create's own response shape does not carry read back from state.
        let read = fabric.read(case, &gateway_id, &link_id).await;
        assert_eq!(
            (text(&read, "gatewayId"), text(&read, "linkId")),
            (gateway_id.clone(), link_id.clone()),
            "case {case:?}: expected the read to report the stored link, got {read:?}"
        );
        assert_eq!(
            text(&read, "domainName"),
            domain_name,
            "case {case:?}: expected the name the create assigned, got {:?}",
            read["domainName"]
        );
        assert_eq!(
            text(&read, "connectivityType"),
            EXTERNAL_INBOUND,
            "case {case:?}: expected the connectivity type of an inbound external link, got {:?}",
            read["connectivityType"]
        );
        assert!(
            CONNECTIVITY_TYPE_VALUES.contains(&text(&read, "connectivityType").as_str()),
            "case {case:?}: expected a modelled ConnectivityType member, got {:?}",
            read["connectivityType"]
        );
        assert_eq!(
            read["attributes"], row.attributes,
            "case {case:?}: expected the stored attributes {:?}, got {:?}",
            row.attributes, read["attributes"]
        );
        assert_eq!(
            read["logSettings"], row.log_settings,
            "case {case:?}: expected the stored logSettings {:?}, got {:?}",
            row.log_settings, read["logSettings"]
        );
        assert_eq!(
            tag_map(&read["tags"]),
            row.created_tags(),
            "case {case:?}: expected the tags the create persisted {:?}, got {:?}",
            row.created_tags(),
            read["tags"]
        );
        // Nothing configures the flow of a link with one end outside the fabric, so
        // both module lists are reported empty rather than left out.
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
        let status = text(&read, "status");
        assert!(
            LINK_STATUS_VALUES.contains(&status.as_str()) && [PENDING_CREATION, ACTIVE].contains(&status.as_str()),
            "case {case:?}: expected a fresh link to report the status it was stored at or the one the clock owes it, got {status:?}"
        );

        // The status the clock owes the record comes due; the read is what reports
        // it, and nothing else about the record moves with it.
        let settled = fabric.poll_until_active(case, &gateway_id, &link_id).await;
        assert_eq!(
            untouched(&settled),
            untouched(&read),
            "case {case:?}: expected the settle to leave every other member alone, got {settled:?}"
        );

        // A read changes nothing a later caller sees: two readings of one settled
        // record are the same answer, member for member.
        let again = fabric.read(case, &gateway_id, &link_id).await;
        assert_eq!(
            again, settled,
            "case {case:?}: expected a repeated read to report the same link, got {again:?}"
        );

        // What a later write leaves on the record is what the read reports: the
        // tagging surface writes to the link's own tag map, so the two readings of
        // that one map agree.
        let resource_arn = link_arn(&gateway_id, &link_id);
        let assigned = fabric
            .send(
                "POST",
                &tags_path(&resource_arn),
                Some(json!({ "tags": tags_json(row.assigned) })),
            )
            .await;
        assert_eq!(
            assigned.status, 204,
            "case {case:?}: expected TagResource to answer 204, got {} {:?}",
            assigned.status, assigned.body
        );
        let read = fabric.read(case, &gateway_id, &link_id).await;
        assert_eq!(
            tag_map(&read["tags"]),
            row.after_assignment(),
            "case {case:?}: expected the assignment's map {:?}, got {:?}",
            row.after_assignment(),
            read["tags"]
        );
        assert_eq!(
            fabric.listed_tags(case, &resource_arn).await,
            tag_map(&read["tags"]),
            "case {case:?}: expected the link's own reader and the tag listing to agree, got {:?}",
            read["tags"]
        );
        assert_eq!(
            untouched(&read),
            untouched(&settled),
            "case {case:?}: expected a tag assignment to move nothing else, got {read:?}"
        );
        assert_eq!(
            text(&read, "status"),
            ACTIVE,
            "case {case:?}: expected the settled status to hold, got {:?}",
            read["status"]
        );

        let removed = fabric
            .send("DELETE", &untag_path(&resource_arn, row.removed), None)
            .await;
        assert_eq!(
            removed.status, 204,
            "case {case:?}: expected UntagResource to answer 204, got {} {:?}",
            removed.status, removed.body
        );
        let read = fabric.read(case, &gateway_id, &link_id).await;
        assert_eq!(
            tag_map(&read["tags"]),
            row.after_removal(),
            "case {case:?}: expected what the removal left {:?}, got {:?}",
            row.after_removal(),
            read["tags"]
        );
        if row.after_removal().is_empty() {
            // A record the removal left holding no tags reports none at all, exactly
            // as one created without any does.
            assert!(
                read["tags"].as_object().is_none_or(|map| map.is_empty()),
                "case {case:?}: expected a record left holding no tags to report none, got {:?}",
                read["tags"]
            );
        }
        assert_eq!(
            fabric.listed_tags(case, &resource_arn).await,
            row.after_removal(),
            "case {case:?}: expected the tag listing to agree with the read, got {:?}",
            read["tags"]
        );
        assert_eq!(
            untouched(&read),
            untouched(&settled),
            "case {case:?}: expected a tag removal to move nothing else, got {read:?}"
        );
    }
}
