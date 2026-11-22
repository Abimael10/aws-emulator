// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the `/tags/{resourceArn}` surface: `TagResource`,
//! `ListTagsForResource` and `UntagResource`, driven over the wire through the router
//! the production emulator uses.
//!
//! The rule under test is about state, not about one recorded request: a tagging call
//! addresses the ONE stored record its `resourceArn` names — a requester gateway, a
//! responder gateway, or a link the store holds, read through a gateway the link
//! belongs to — so the listing reports exactly the tags the writes left on that
//! record, an assignment replaces the value of a key already there, a removal names
//! keys rather than requiring them, and an ARN naming no stored resource is refused by
//! all three alike.
//!
//! The table spans what the rule generalises over: which kind of resource the ARN
//! names (gateway of either kind, a link between two gateways from either end, an
//! external link of either direction), how many tags the record starts with, how many
//! the call assigns, whether an assignment adds or replaces, and how many keys a
//! removal names (including one the record never carried). The tag keys and values
//! span the modelled `TagKey` pattern's punctuation and both published length bounds
//! of `TagKey` (1..128) and `TagValue` (0..1600), from `smithy_query(query="shape",
//! shape="TagsMap,TagKeyList")`.

use std::collections::BTreeMap;
use std::sync::Arc;

use bytes::Bytes;
use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use serde_json::{json, Value};
use service_com_amazonaws_rtbfabric_rtbfabric::state::TAGS_PER_RESOURCE_QUOTA;
use service_com_amazonaws_rtbfabric_rtbfabric::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region the test signs for, and so the region every ARN it builds names.
const REGION: &str = "us-west-2";

/// The answer a tagging call naming no stored resource must carry: the status, the
/// wire error code and the whole message, written out here rather than read back from
/// the crate under test, so a handler that moved to another refusal fails this suite.
/// AWS answers an unresolvable rtbfabric tagging ARN this way rather than with the
/// `ResourceNotFoundException` the documentation suggests.
const UNRESOLVED_STATUS: u16 = 500;
const UNRESOLVED_CODE: &str = "InternalServerErrorException";
const UNRESOLVED_MESSAGE: &str = "Internal server error.";

/// One answered call: the status, the wire error code (absent on a success) and the
/// parsed body.
struct Answer {
    status: u16,
    code: String,
    body: Value,
}

/// The one emulated service, behind the same `MultiServiceRouter` the production
/// router assembles, so these calls take the real deserialize -> validate -> handle ->
/// serialize path.
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

    /// Create a requester gateway carrying `tags` and return its minted id.
    async fn requester_gateway(&self, case: &str, tags: &[(String, String)]) -> String {
        let mut body = json!({
            "vpcId": "vpc-0123456789abcdef0",
            "subnetIds": ["subnet-0123456789abcdef0"],
            "securityGroupIds": ["sg-0123456789abcdef0"],
            "clientToken": "token-requester",
        });
        if !tags.is_empty() {
            body["tags"] = tags_json(tags);
        }
        let answered = self.send("POST", "/requester-gateway", Some(body)).await;
        assert_eq!(
            answered.status, 200,
            "case {case:?}: expected CreateRequesterGateway to answer 200, got {} {:?}",
            answered.status, answered.body
        );
        text(&answered.body, "gatewayId")
    }

    /// Create a responder gateway carrying `tags` and return its minted id.
    async fn responder_gateway(&self, case: &str, tags: &[(String, String)]) -> String {
        let mut body = json!({
            "vpcId": "vpc-0123456789abcdef0",
            "subnetIds": ["subnet-0123456789abcdef0"],
            "securityGroupIds": ["sg-0123456789abcdef0"],
            "port": 443,
            "protocol": "HTTPS",
            "listenerConfig": { "protocols": ["HTTPS"] },
            "domainName": "responder.example.com",
            "clientToken": "token-responder",
        });
        if !tags.is_empty() {
            body["tags"] = tags_json(tags);
        }
        let answered = self.send("POST", "/responder-gateway", Some(body)).await;
        assert_eq!(
            answered.status, 200,
            "case {case:?}: expected CreateResponderGateway to answer 200, got {} {:?}",
            answered.status, answered.body
        );
        text(&answered.body, "gatewayId")
    }

    /// Open a link from `gateway_id` to `peer_gateway_id`, carrying `tags`, and return
    /// its minted id.
    async fn link(
        &self,
        case: &str,
        gateway_id: &str,
        peer_gateway_id: &str,
        tags: &[(String, String)],
    ) -> String {
        let mut body = json!({
            "peerGatewayId": peer_gateway_id,
            "logSettings": { "applicationLogs": { "sampling": { "errorLog": 100.0, "filterLog": 0.0 } } },
        });
        if !tags.is_empty() {
            body["tags"] = tags_json(tags);
        }
        let answered = self
            .send(
                "POST",
                &format!("/gateway/{gateway_id}/create-link"),
                Some(body),
            )
            .await;
        assert_eq!(
            answered.status, 200,
            "case {case:?}: expected CreateLink to answer 200, got {} {:?}",
            answered.status, answered.body
        );
        text(&answered.body, "linkId")
    }

    /// Open an outbound external link from the requester gateway `gateway_id`,
    /// carrying `tags`, and return its minted id.
    async fn outbound_external_link(
        &self,
        case: &str,
        gateway_id: &str,
        tags: &[(String, String)],
    ) -> String {
        let mut body = json!({
            "clientToken": "token-outbound-external-link",
            "publicEndpoint": "https://bidder.example.com/rtb",
            "logSettings": { "applicationLogs": { "sampling": { "errorLog": 100.0, "filterLog": 0.0 } } },
        });
        if !tags.is_empty() {
            body["tags"] = tags_json(tags);
        }
        let answered = self
            .send(
                "POST",
                &format!("/requester-gateway/{gateway_id}/outbound-external-link"),
                Some(body),
            )
            .await;
        assert_eq!(
            answered.status, 200,
            "case {case:?}: expected CreateOutboundExternalLink to answer 200, got {} {:?}",
            answered.status, answered.body
        );
        text(&answered.body, "linkId")
    }

    /// Create an inbound external link on the responder gateway `gateway_id`,
    /// carrying `tags`, and return its minted id.
    async fn inbound_external_link(
        &self,
        case: &str,
        gateway_id: &str,
        tags: &[(String, String)],
    ) -> String {
        let mut body = json!({
            "clientToken": "token-inbound-external-link",
            "logSettings": { "applicationLogs": { "sampling": { "errorLog": 0.0, "filterLog": 100.0 } } },
        });
        if !tags.is_empty() {
            body["tags"] = tags_json(tags);
        }
        let answered = self
            .send(
                "POST",
                &format!("/responder-gateway/{gateway_id}/inbound-external-link"),
                Some(body),
            )
            .await;
        assert_eq!(
            answered.status, 200,
            "case {case:?}: expected CreateInboundExternalLink to answer 200, got {} {:?}",
            answered.status, answered.body
        );
        text(&answered.body, "linkId")
    }

    /// `ListTagsForResource`, asserting it succeeded, as a sorted map.
    async fn listed_tags(&self, case: &str, resource_arn: &str) -> BTreeMap<String, String> {
        let answered = self.send("GET", &tags_path(resource_arn), None).await;
        assert_eq!(
            answered.status, 200,
            "case {case:?}: expected ListTagsForResource on {resource_arn:?} to answer 200, got {} {:?}",
            answered.status, answered.body
        );
        tags_map(case, &answered.body["tags"])
    }
}

/// Percent-encode a value the way an AWS SDK serializes a non-greedy `@httpLabel` or
/// an `@httpQuery` value: the maintained `percent-encoding` crate's escaping over
/// everything outside the alphanumerics, so an ARN's `:` and `/` arrive as ONE path
/// segment and a tag key carrying `+` or `/` survives the query string.
fn encoded(value: &str) -> String {
    utf8_percent_encode(value, NON_ALPHANUMERIC).to_string()
}

/// The `/tags/{resourceArn}` path for `resource_arn`.
fn tags_path(resource_arn: &str) -> String {
    format!("/tags/{}", encoded(resource_arn))
}

/// The `UntagResource` path: the resource in the label, the keys in the repeated
/// `tagKeys` query parameter the model binds.
fn untag_path(resource_arn: &str, tag_keys: &[String]) -> String {
    let query: Vec<String> = tag_keys
        .iter()
        .map(|key| format!("tagKeys={}", encoded(key)))
        .collect();
    format!("{}?{}", tags_path(resource_arn), query.join("&"))
}

/// The ARN of a gateway of either kind, in the account and Region this emulator
/// serves: `engine::emulator_arn` carries `engine::mock_account_id`, which is the
/// account every ARN it mints names.
fn gateway_arn(gateway_id: &str) -> String {
    ::engine::emulator_arn("rtbfabric", REGION, &format!("gateway/{gateway_id}"))
}

/// The ARN of a link, addressed from one of its two ends.
fn link_arn(gateway_id: &str, link_id: &str) -> String {
    ::engine::emulator_arn(
        "rtbfabric",
        REGION,
        &format!("gateway/{gateway_id}/link/{link_id}"),
    )
}

fn tags_json(tags: &[(String, String)]) -> Value {
    Value::Object(
        tags.iter()
            .map(|(key, value)| (key.clone(), json!(value)))
            .collect(),
    )
}

/// The `TagResource` body: the model binds `tags` as a member of the request
/// structure, so the map travels under that key rather than as the whole body.
fn tag_resource_body(tags: &[(String, String)]) -> Value {
    json!({ "tags": tags_json(tags) })
}

fn text(body: &Value, member: &str) -> String {
    body[member].as_str().unwrap_or_default().to_string()
}

/// A `tags` member as a sorted map, so two tag sets compare independently of the
/// order they were written in.
fn tags_map(case: &str, tags: &Value) -> BTreeMap<String, String> {
    let object = tags
        .as_object()
        .unwrap_or_else(|| panic!("case {case:?}: expected a tags map, got {tags:?}"));
    object
        .iter()
        .map(|(key, value)| {
            (
                key.clone(),
                value
                    .as_str()
                    .unwrap_or_else(|| {
                        panic!("case {case:?}: expected the tag {key:?} to be a string, got {value:?}")
                    })
                    .to_string(),
            )
        })
        .collect()
}

fn tag(key: &str, value: &str) -> (String, String) {
    (key.to_string(), value.to_string())
}

/// Which stored resource a row's ARN names, and from which end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Target {
    RequesterGateway,
    ResponderGateway,
    /// The link, addressed through the gateway that opened it.
    LinkFromRequester,
    /// The same link, addressed through its peer: one record, two ends.
    LinkFromResponder,
    /// A link whose far end is an address outside the fabric, addressed through the
    /// requester gateway it was opened from.
    OutboundExternalLink,
    /// A link whose traffic arrives from outside the fabric, addressed through the
    /// responder gateway it was created on.
    InboundExternalLink,
}

/// One row of the tagging table.
struct Row {
    name: &'static str,
    target: Target,
    /// The tags the create persisted on the record, before any tagging call.
    created: Vec<(String, String)>,
    /// The batch `TagResource` assigns.
    assigned: Vec<(String, String)>,
    /// The keys `UntagResource` names; never empty, because `tagKeys` is `@required`
    /// with a minimum of one member.
    removed: Vec<String>,
}

impl Row {
    /// The tags the record must hold once the assignment landed: the created ones,
    /// with an assigned key replacing the value of a key already there.
    fn after_assignment(&self) -> BTreeMap<String, String> {
        let mut tags: BTreeMap<String, String> = self.created.iter().cloned().collect();
        tags.extend(self.assigned.iter().cloned());
        tags
    }

    /// The tags the record must hold once the removal landed.
    fn after_removal(&self) -> BTreeMap<String, String> {
        let mut tags = self.after_assignment();
        for key in &self.removed {
            tags.remove(key);
        }
        tags
    }
}

/// A key of the maximum modelled `TagKey` length (128).
fn longest_key() -> String {
    format!("k{}", "9".repeat(127))
}

/// A value of 1600 characters, the maximum modelled `TagValue` length.
fn longest_value() -> String {
    "v".repeat(1600)
}

/// The rows differ along each axis the rule generalises over: the resource kind and
/// the end it is addressed from, how many tags the record starts with, how many the
/// call assigns, whether an assignment adds or replaces, and how many keys a removal
/// names.
fn rows() -> Vec<Row> {
    vec![
        Row {
            name: "a requester gateway tagged for the first time",
            target: Target::RequesterGateway,
            created: Vec::new(),
            assigned: vec![tag("env", "prod")],
            removed: vec!["env".to_string()],
        },
        Row {
            name: "a requester gateway whose created tag is replaced",
            target: Target::RequesterGateway,
            created: vec![tag("env", "staging"), tag("team", "ads")],
            // The modelled `TagKey` pattern admits `+-=._:/@`, so one key spans that
            // punctuation and one value is empty (`TagValue` length min 0).
            assigned: vec![
                tag("env", "prod"),
                tag("cost:center/ads+rtb=1", "bid.engine@eu"),
                tag("empty", ""),
            ],
            removed: vec!["team".to_string(), "never-set".to_string()],
        },
        Row {
            name: "a responder gateway at both published tag-length bounds",
            target: Target::ResponderGateway,
            created: vec![tag("k", "")],
            assigned: vec![
                (longest_key(), longest_value()),
                tag("k", "a"),
            ],
            removed: vec![longest_key()],
        },
        Row {
            name: "a link addressed through the gateway that opened it",
            target: Target::LinkFromRequester,
            created: vec![tag("stage", "canary")],
            assigned: vec![tag("owner", "bidding"), tag("stage", "stable")],
            removed: vec!["owner".to_string()],
        },
        Row {
            name: "the same link addressed through its peer end",
            target: Target::LinkFromResponder,
            created: Vec::new(),
            assigned: vec![tag("owner", "bidding")],
            removed: vec!["owner".to_string(), "owner".to_string()],
        },
        Row {
            name: "an outbound external link whose created tag is replaced",
            target: Target::OutboundExternalLink,
            created: vec![tag("tier", "gold")],
            assigned: vec![
                tag("tier", "silver"),
                tag("cost:center/ads+rtb=1", "bid.engine@eu"),
            ],
            removed: vec!["tier".to_string()],
        },
        Row {
            name: "an outbound external link tagged for the first time, at both bounds",
            target: Target::OutboundExternalLink,
            created: Vec::new(),
            assigned: vec![(longest_key(), longest_value()), tag("env", "prod")],
            removed: vec![longest_key(), "never-set".to_string()],
        },
        Row {
            name: "an inbound external link whose created tags are extended",
            target: Target::InboundExternalLink,
            created: vec![tag("team", "ads"), tag("stage", "canary")],
            assigned: vec![tag("owner", "bidding"), tag("empty", "")],
            removed: vec!["stage".to_string(), "owner".to_string()],
        },
    ]
}

/// Create the row's resource and answer its ARN, plus the path a reader of that same
/// record answers on, so the tags can be proved to live on the resource itself.
async fn provision(fabric: &Fabric, row: &Row) -> (String, String) {
    let case = row.name;
    match row.target {
        Target::RequesterGateway => {
            let gateway_id = fabric.requester_gateway(case, &row.created).await;
            (
                gateway_arn(&gateway_id),
                format!("/requester-gateway/{gateway_id}"),
            )
        }
        Target::ResponderGateway => {
            let gateway_id = fabric.responder_gateway(case, &row.created).await;
            (
                gateway_arn(&gateway_id),
                format!("/responder-gateway/{gateway_id}"),
            )
        }
        Target::LinkFromRequester | Target::LinkFromResponder => {
            let requester_id = fabric.requester_gateway(case, &[]).await;
            let responder_id = fabric.responder_gateway(case, &[]).await;
            let link_id = fabric
                .link(case, &requester_id, &responder_id, &row.created)
                .await;
            let end = match row.target {
                Target::LinkFromResponder => &responder_id,
                _ => &requester_id,
            };
            (
                link_arn(end, &link_id),
                format!("/gateway/{end}/link/{link_id}"),
            )
        }
        Target::OutboundExternalLink => {
            let gateway_id = fabric.requester_gateway(case, &[]).await;
            let link_id = fabric
                .outbound_external_link(case, &gateway_id, &row.created)
                .await;
            (
                link_arn(&gateway_id, &link_id),
                format!("/requester-gateway/{gateway_id}/outbound-external-link/{link_id}"),
            )
        }
        Target::InboundExternalLink => {
            let gateway_id = fabric.responder_gateway(case, &[]).await;
            let link_id = fabric
                .inbound_external_link(case, &gateway_id, &row.created)
                .await;
            (
                link_arn(&gateway_id, &link_id),
                format!("/responder-gateway/{gateway_id}/inbound-external-link/{link_id}"),
            )
        }
    }
}

#[tokio::test]
async fn the_tagging_surface_round_trips_the_tags_of_every_taggable_resource() {
    for row in rows() {
        let case = row.name;
        let fabric = Fabric::new();
        let (resource_arn, reader_path) = provision(&fabric, &row).await;

        // What the create persisted is what the listing reports — including no tags at
        // all, which is the empty map rather than a missing member.
        let listed = fabric.listed_tags(case, &resource_arn).await;
        let created: BTreeMap<String, String> = row.created.iter().cloned().collect();
        assert_eq!(
            listed, created,
            "case {case:?}: expected the listing to report the tags the create persisted, got {listed:?}"
        );

        let assigned = fabric
            .send(
                "POST",
                &tags_path(&resource_arn),
                Some(tag_resource_body(&row.assigned)),
            )
            .await;
        assert_eq!(
            assigned.status, 204,
            "case {case:?}: expected TagResource to answer 204, got {} {:?}",
            assigned.status, assigned.body
        );

        let listed = fabric.listed_tags(case, &resource_arn).await;
        assert_eq!(
            listed,
            row.after_assignment(),
            "case {case:?}: expected the assignment to add its keys and replace the value of one already there, got {listed:?}"
        );

        // The tags live on the resource's OWN record: the reader of that resource
        // reports the same map, so the tagging surface wrote nothing of its own.
        let read = fabric.send("GET", &reader_path, None).await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: expected the resource at {reader_path:?} to still be readable, got {} {:?}",
            read.status, read.body
        );
        assert_eq!(
            tags_map(case, &read.body["tags"]),
            row.after_assignment(),
            "case {case:?}: expected the resource's own reader to report the assigned tags, got {:?}",
            read.body["tags"]
        );

        let removed = fabric
            .send("DELETE", &untag_path(&resource_arn, &row.removed), None)
            .await;
        assert_eq!(
            removed.status, 204,
            "case {case:?}: expected UntagResource to answer 204, got {} {:?}",
            removed.status, removed.body
        );

        let listed = fabric.listed_tags(case, &resource_arn).await;
        assert_eq!(
            listed,
            row.after_removal(),
            "case {case:?}: expected the removal to drop exactly the keys it named, got {listed:?}"
        );

        // The listing is a read: the same call answers the same thing, and the
        // resource is still there afterwards.
        let again = fabric.listed_tags(case, &resource_arn).await;
        assert_eq!(
            again,
            row.after_removal(),
            "case {case:?}: expected a repeated listing to answer identically, got {again:?}"
        );
        let read = fabric.send("GET", &reader_path, None).await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: expected the resource to outlive the tagging calls, got {} {:?}",
            read.status, read.body
        );
    }
}

#[tokio::test]
async fn an_arn_naming_no_stored_resource_is_refused_by_every_tagging_operation() {
    let fabric = Fabric::new();
    let requester_id = fabric.requester_gateway("refusals", &[]).await;
    let responder_id = fabric.responder_gateway("refusals", &[]).await;
    let link_id = fabric
        .link("refusals", &requester_id, &responder_id, &[])
        .await;

    // Each ARN below is well formed (the modelled pattern admits it) and names no
    // resource this store holds.
    let cases: Vec<(&str, String)> = vec![
        (
            "a gateway id never minted",
            gateway_arn("rtb-gw-never-created"),
        ),
        (
            "a link id never minted, on a stored gateway",
            link_arn(&requester_id, "link-never-created"),
        ),
        (
            "a stored link addressed through a gateway that is not one of its ends",
            link_arn("rtb-gw-never-created", &link_id),
        ),
        (
            "a routing rule, which no operation stores yet",
            ::engine::emulator_arn(
                "rtbfabric",
                REGION,
                &format!("gateway/{requester_id}/link/{link_id}/routing-rule/rule-1"),
            ),
        ),
        (
            "a gateway of another Region",
            ::engine::emulator_arn(
                "rtbfabric",
                "eu-west-1",
                &format!("gateway/{requester_id}"),
            ),
        ),
        (
            "a gateway of another account",
            format!("arn:aws:rtbfabric:{REGION}:999999999999:gateway/{requester_id}"),
        ),
    ];

    for (case, resource_arn) in cases {
        for (operation, method, path) in [
            ("ListTagsForResource", "GET", tags_path(&resource_arn)),
            ("TagResource", "POST", tags_path(&resource_arn)),
            (
                "UntagResource",
                "DELETE",
                untag_path(&resource_arn, &["env".to_string()]),
            ),
        ] {
            let body = (method == "POST").then(|| tag_resource_body(&[tag("env", "prod")]));
            let answered = fabric.send(method, &path, body).await;
            assert_eq!(
                answered.status, UNRESOLVED_STATUS,
                "case {case:?}: expected {operation} to answer {UNRESOLVED_STATUS}, got {} {:?}",
                answered.status, answered.body
            );
            assert_eq!(
                answered.code, UNRESOLVED_CODE,
                "case {case:?}: expected {operation} to answer the code {UNRESOLVED_CODE}, got {:?}",
                answered.code
            );
            assert_eq!(
                text(&answered.body, "message"),
                UNRESOLVED_MESSAGE,
                "case {case:?}: expected {operation} to state {UNRESOLVED_MESSAGE:?}, got {:?}",
                answered.body
            );
        }
    }

    // The refused calls wrote nothing: the resources that DO exist are untagged still.
    for (case, resource_arn) in [
        ("the stored requester gateway", gateway_arn(&requester_id)),
        ("the stored responder gateway", gateway_arn(&responder_id)),
        ("the stored link", link_arn(&requester_id, &link_id)),
    ] {
        let listed = fabric.listed_tags(case, &resource_arn).await;
        assert!(
            listed.is_empty(),
            "case {case:?}: expected the refused calls to have written no tag, got {listed:?}"
        );
    }
}

#[tokio::test]
async fn an_arn_outside_the_modelled_pattern_is_refused_as_a_bad_request() {
    let fabric = Fabric::new();

    // The modelled `RtbTaggableResourceArn` pattern pins the partition, the vendor and
    // the resource form, so a value outside it is a bad REQUEST — refused with 400
    // before any handler runs — and not a resource that happens to be missing.
    for (case, resource_arn) in [
        ("an ARN of another vendor", "arn:aws:ec2:us-west-2:000000000000:gateway/rtb-gw-1"),
        ("an ARN of another partition", "arn:aws-cn:rtbfabric:us-west-2:000000000000:gateway/rtb-gw-1"),
        ("an account field that is not 12 digits", "arn:aws:rtbfabric:us-west-2:12345:gateway/rtb-gw-1"),
        ("a resource field naming no gateway", "arn:aws:rtbfabric:us-west-2:000000000000:link/link-1"),
        ("not an ARN at all", "rtb-gw-1"),
    ] {
        let answered = fabric.send("GET", &tags_path(resource_arn), None).await;
        assert_eq!(
            answered.status, 400,
            "case {case:?}: expected {resource_arn:?} to be refused with 400, got {} {:?}",
            answered.status, answered.body
        );
        assert_eq!(
            answered.code, "ValidationException",
            "case {case:?}: expected a ValidationException, got {:?}",
            answered.code
        );
    }
}

#[tokio::test]
async fn one_resource_holds_at_most_its_quota_of_tags() {
    let fabric = Fabric::new();
    let case = "the per-resource tag bound";
    let gateway_id = fabric.requester_gateway(case, &[]).await;
    let resource_arn = gateway_arn(&gateway_id);

    // Exactly the bound is accepted.
    let full: Vec<(String, String)> = (0..TAGS_PER_RESOURCE_QUOTA)
        .map(|n| tag(&format!("key-{n}"), "v"))
        .collect();
    let answered = fabric
        .send("POST", &tags_path(&resource_arn), Some(tag_resource_body(&full)))
        .await;
    assert_eq!(
        answered.status, 204,
        "case {case:?}: expected {TAGS_PER_RESOURCE_QUOTA} tags to be accepted, got {} {:?}",
        answered.status, answered.body
    );

    // One key past it is refused, and nothing of that call is written.
    let answered = fabric
        .send(
            "POST",
            &tags_path(&resource_arn),
            Some(tag_resource_body(&[tag("one-too-many", "v")])),
        )
        .await;
    assert_eq!(
        answered.status, 400,
        "case {case:?}: expected the tag past the bound to be refused with 400, got {} {:?}",
        answered.status, answered.body
    );
    let listed = fabric.listed_tags(case, &resource_arn).await;
    assert_eq!(
        listed.len(),
        TAGS_PER_RESOURCE_QUOTA,
        "case {case:?}: expected the resource to still hold {TAGS_PER_RESOURCE_QUOTA} tags, got {}",
        listed.len()
    );
    assert!(
        !listed.contains_key("one-too-many"),
        "case {case:?}: expected the refused call to have written nothing, got {listed:?}"
    );

    // Replacing the value of a key already there is not another tag, so it is
    // accepted at the bound.
    let answered = fabric
        .send(
            "POST",
            &tags_path(&resource_arn),
            Some(tag_resource_body(&[tag("key-0", "replaced")])),
        )
        .await;
    assert_eq!(
        answered.status, 204,
        "case {case:?}: expected replacing a value at the bound to be accepted, got {} {:?}",
        answered.status, answered.body
    );
    let listed = fabric.listed_tags(case, &resource_arn).await;
    assert_eq!(
        listed.get("key-0").map(String::as_str),
        Some("replaced"),
        "case {case:?}: expected the replaced value, got {:?}",
        listed.get("key-0")
    );

    // And once a key is removed, one more fits again.
    let answered = fabric
        .send(
            "DELETE",
            &untag_path(&resource_arn, &["key-0".to_string()]),
            None,
        )
        .await;
    assert_eq!(
        answered.status, 204,
        "case {case:?}: expected UntagResource to answer 204, got {} {:?}",
        answered.status, answered.body
    );
    let answered = fabric
        .send(
            "POST",
            &tags_path(&resource_arn),
            Some(tag_resource_body(&[tag("one-more", "v")])),
        )
        .await;
    assert_eq!(
        answered.status, 204,
        "case {case:?}: expected a tag to fit again after a removal, got {} {:?}",
        answered.status, answered.body
    );
}

#[tokio::test]
async fn a_link_is_tagged_only_through_a_gateway_that_link_belongs_to() {
    let fabric = Fabric::new();
    let case = "a link addressed through the wrong gateway";

    // One store holding a link of each kind: between two gateways, outbound to an
    // address outside the fabric, and inbound from outside it.
    let requester_id = fabric.requester_gateway(case, &[]).await;
    let responder_id = fabric.responder_gateway(case, &[]).await;
    let peer_link_id = fabric
        .link(case, &requester_id, &responder_id, &[tag("kind", "peer")])
        .await;
    let outbound_link_id = fabric
        .outbound_external_link(case, &requester_id, &[tag("kind", "outbound")])
        .await;
    let inbound_link_id = fabric
        .inbound_external_link(case, &responder_id, &[tag("kind", "inbound")])
        .await;

    // Each link answers through a gateway it belongs to, with the tags its own create
    // assigned.
    for (kind, gateway_id, link_id) in [
        ("a link between two gateways", &requester_id, &peer_link_id),
        ("the same link from its peer end", &responder_id, &peer_link_id),
        (
            "an outbound external link",
            &requester_id,
            &outbound_link_id,
        ),
        ("an inbound external link", &responder_id, &inbound_link_id),
    ] {
        let listed = fabric.listed_tags(kind, &link_arn(gateway_id, link_id)).await;
        assert!(
            listed.contains_key("kind"),
            "case {kind:?}: expected the tags the create assigned, got {listed:?}"
        );
    }

    // An external link is addressed only through the gateway it was created on, so
    // the gateway that holds no such link names no record at all — the same answer as
    // a link id nothing minted.
    for (wrong, resource_arn) in [
        (
            "an outbound external link through the responder gateway",
            link_arn(&responder_id, &outbound_link_id),
        ),
        (
            "an inbound external link through the requester gateway",
            link_arn(&requester_id, &inbound_link_id),
        ),
    ] {
        for (operation, method, path) in [
            ("ListTagsForResource", "GET", tags_path(&resource_arn)),
            ("TagResource", "POST", tags_path(&resource_arn)),
            (
                "UntagResource",
                "DELETE",
                untag_path(&resource_arn, &["kind".to_string()]),
            ),
        ] {
            let body = (method == "POST").then(|| tag_resource_body(&[tag("env", "prod")]));
            let answered = fabric.send(method, &path, body).await;
            assert_eq!(
                answered.status, UNRESOLVED_STATUS,
                "case {wrong:?}: expected {operation} to answer {UNRESOLVED_STATUS}, got {} {:?}",
                answered.status, answered.body
            );
            assert_eq!(
                answered.code, UNRESOLVED_CODE,
                "case {wrong:?}: expected {operation} to answer the code {UNRESOLVED_CODE}, got {:?}",
                answered.code
            );
            assert_eq!(
                text(&answered.body, "message"),
                UNRESOLVED_MESSAGE,
                "case {wrong:?}: expected {operation} to state {UNRESOLVED_MESSAGE:?}, got {:?}",
                answered.body
            );
        }
    }

    // Those refused calls wrote nothing: each link still holds exactly the one tag
    // its create assigned.
    for (kind, gateway_id, link_id, expected) in [
        (
            "an outbound external link",
            &requester_id,
            &outbound_link_id,
            "outbound",
        ),
        (
            "an inbound external link",
            &responder_id,
            &inbound_link_id,
            "inbound",
        ),
    ] {
        let listed = fabric.listed_tags(kind, &link_arn(gateway_id, link_id)).await;
        assert_eq!(
            listed.get("kind").map(String::as_str),
            Some(expected),
            "case {kind:?}: expected the refused calls to have written nothing, got {listed:?}"
        );
        assert_eq!(
            listed.len(),
            1,
            "case {kind:?}: expected the one tag the create assigned, got {listed:?}"
        );
    }
}
