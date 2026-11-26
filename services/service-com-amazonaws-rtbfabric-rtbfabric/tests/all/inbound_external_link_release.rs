// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for `DeleteInboundExternalLink`, the release half of the
//! `/responder-gateway/{gatewayId}/inbound-external-link` surface, driven over the
//! wire through the router the production emulator uses.
//!
//! The rule under test is about state, not about one recorded request: the delete
//! releases the ONE record the (`gatewayId`, `linkId`) pair addresses — the link, the
//! tags it carried and the client-token record its create wrote — answers with that
//! id and the terminal `LinkStatus`, leaves every other link of the gateway alone,
//! and refuses a pair naming no stored link with the modelled not-found, having
//! released nothing.
//!
//! The table spans what the rule generalises over: how many links the gateway holds,
//! which shape of create the released link was stored from (the optional members and
//! the tags it carries), and whether the clock had already settled the link before it
//! was released.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_rtbfabric_rtbfabric::ops::types::LINK_STATUS_VALUES;
use service_com_amazonaws_rtbfabric_rtbfabric::state::{
    LINK_STATUS_ACTIVE, RESOURCE_NOT_FOUND_MESSAGE, TAGGING_UNRESOLVED_CODE,
    TAGGING_UNRESOLVED_MESSAGE, TAGGING_UNRESOLVED_STATUS,
};
use service_com_amazonaws_rtbfabric_rtbfabric::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

use crate::resource_tagging::{link_arn, tags_path};

/// The region the test signs for, and so the region every ARN it builds names. The
/// same Region the tagging suite's ARN helpers are written for.
const REGION: &str = "us-west-2";

/// The one `GatewayType` that has the public inbound endpoint an inbound external
/// link is named under, from `GATEWAY_TYPE_VALUES`; it is the only type this surface
/// exists for.
const EXTERNAL: &str = "EXTERNAL";

/// The terminal status a released link reports, a member of `LinkStatus`
/// (`smithy_query(query="shape", shape="LinkStatus")`).
const RELEASED_STATUS: &str = "DELETED";

/// How long a poll waits for a status the clock owes before it gives up. Far above
/// any span this service owes a transition, so a slow or loaded host changes how many
/// times the loop looks, never whether it succeeds.
const SETTLE_DEADLINE: Duration = Duration::from_secs(60);

/// How long a poll waits between two looks.
const SETTLE_POLL: Duration = Duration::from_millis(20);

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

/// `LinkLogSettings` with its one required member, as
/// `smithy_query(query="shape", shape="LinkLogSettings")` declares it.
fn log_settings(error_log: f64, filter_log: f64) -> Value {
    json!({ "applicationLogs": { "sampling": { "errorLog": error_log, "filterLog": filter_log } } })
}

/// One shape of `CreateInboundExternalLink` request, which the release has to behave
/// the same way for.
struct LinkShape {
    /// A `LinkAttributes`, or `Value::Null` for a link that carries none.
    attributes: Value,
    /// A `LinkLogSettings`, which the create requires.
    log_settings: Value,
    /// A `TagsMap`, or `Value::Null` for a link that carries no tags.
    tags: Value,
}

impl LinkShape {
    fn create_body(&self, token: &str) -> Value {
        let mut body = json!({
            "clientToken": token,
            "logSettings": self.log_settings.clone(),
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

    /// The tags a stored link created from this shape reports.
    fn expected_tags(&self) -> Value {
        if self.tags.is_null() {
            json!({})
        } else {
            self.tags.clone()
        }
    }
}

/// The create shapes the release table draws its links from: a link carrying nothing
/// optional, one carrying attributes, one carrying several tags, and one carrying
/// both.
fn link_shapes() -> Vec<LinkShape> {
    vec![
        LinkShape {
            attributes: Value::Null,
            log_settings: log_settings(100.0, 0.0),
            tags: Value::Null,
        },
        LinkShape {
            attributes: json!({ "customerProvidedId": "exchange-north-1" }),
            log_settings: log_settings(0.0, 100.0),
            tags: Value::Null,
        },
        LinkShape {
            attributes: Value::Null,
            log_settings: log_settings(12.5, 37.25),
            tags: json!({ "team": "rtb", "env": "test", "owner": "fabric" }),
        },
        LinkShape {
            attributes: json!({ "customerProvidedId": "exchange-south-9" }),
            log_settings: log_settings(50.0, 50.0),
            tags: json!({ "only": "one" }),
        },
    ]
}

/// One row of the release table.
struct ReleaseRow {
    case: &'static str,
    /// Which shape of [`link_shapes`] the released link is created from, so the table
    /// spans a link carrying no tags, one tag and several, with and without
    /// attributes.
    shape: usize,
    /// How many inbound external links the gateway holds. The first is released;
    /// every other one has to survive.
    held: usize,
    /// Whether to wait for the status the clock owes the link before releasing it, so
    /// the table spans a link still settling and one already settled.
    settled: bool,
}

/// The release table. The rows differ along what the rule generalises over: how many
/// links the gateway holds, which shape of create the released link was stored from,
/// and whether the clock had already settled it.
const RELEASE_ROWS: &[ReleaseRow] = &[
    ReleaseRow {
        case: "the gateway's only link, carrying nothing optional, still settling",
        shape: 0,
        held: 1,
        settled: false,
    },
    ReleaseRow {
        case: "the gateway's only link, carrying attributes, once the clock has settled it",
        shape: 1,
        held: 1,
        settled: true,
    },
    ReleaseRow {
        case: "one of three links, the released one carrying three tags",
        shape: 2,
        held: 3,
        settled: false,
    },
    ReleaseRow {
        case: "one of two links, the released one carrying one tag, once settled",
        shape: 3,
        held: 2,
        settled: true,
    },
];

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

    /// An `EXTERNAL` responder gateway, which is the only kind this surface exists
    /// for, and its minted id.
    async fn responder_gateway(&self, case: &str, token: &str) -> String {
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
                    "gatewayType": EXTERNAL,
                    "domainName": "responder.example.com",
                    "clientToken": token,
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

    /// A requester gateway, whose id addresses no inbound-external-link surface at
    /// all.
    async fn requester_gateway(&self, case: &str, token: &str) -> String {
        let answered = self
            .send(
                "POST",
                "/requester-gateway",
                Some(json!({
                    "vpcId": "vpc-0123456789abcdef0",
                    "subnetIds": ["subnet-0123456789abcdef0"],
                    "securityGroupIds": ["sg-0123456789abcdef0"],
                    "clientToken": token,
                })),
            )
            .await;
        assert_eq!(
            answered.status, 200,
            "case {case:?}: expected CreateRequesterGateway to answer 200, got {} {:?}",
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

    /// `GET /responder-gateway/{gatewayId}/inbound-external-link/{linkId}`.
    async fn read_inbound_external_link(&self, gateway_id: &str, link_id: &str) -> Answer {
        self.send("GET", &inbound_external_link_path(gateway_id, link_id), None)
            .await
    }

    /// `DELETE /responder-gateway/{gatewayId}/inbound-external-link/{linkId}`.
    async fn delete_inbound_external_link(&self, gateway_id: &str, link_id: &str) -> Answer {
        self.send(
            "DELETE",
            &inbound_external_link_path(gateway_id, link_id),
            None,
        )
        .await
    }

    /// Look again until the link reports the status the clock owes it, or a generous
    /// deadline passes. The wait is for the CONDITION, never for a fixed time.
    async fn poll_settled(&self, case: &str, gateway_id: &str, link_id: &str) {
        let deadline = std::time::Instant::now() + SETTLE_DEADLINE;
        loop {
            let read = self.read_inbound_external_link(gateway_id, link_id).await;
            assert_eq!(
                read.status, 200,
                "case {case:?}: expected GetInboundExternalLink to answer 200 while polling, got {} {:?}",
                read.status, read.body
            );
            if text(&read.body, "status") == LINK_STATUS_ACTIVE {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "case {case:?}: expected the link to reach {LINK_STATUS_ACTIVE:?}, last read {:?}",
                read.body["status"]
            );
            tokio::time::sleep(SETTLE_POLL).await;
        }
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

/// The route one inbound external link is addressed at, as the model binds it.
fn inbound_external_link_path(gateway_id: &str, link_id: &str) -> String {
    format!("/responder-gateway/{gateway_id}/inbound-external-link/{link_id}")
}

#[tokio::test]
async fn deleting_an_inbound_external_link_releases_the_record_it_addresses() {
    let shapes = link_shapes();
    for row in RELEASE_ROWS {
        let case = row.case;
        let fabric = Fabric::new();
        let gateway_id = fabric.responder_gateway(case, "token-responder").await;

        // Every link this gateway holds, the one to be released first, each from a
        // different shape of create so no two are the same request.
        let mut link_ids: Vec<String> = Vec::new();
        for n in 0..row.held {
            let shape = &shapes[(row.shape + n) % shapes.len()];
            let created = fabric
                .create_inbound_external_link(
                    &gateway_id,
                    shape.create_body(&format!("token-link-{n}")),
                )
                .await;
            assert_eq!(
                created.status, 200,
                "case {case:?}: expected the create of link #{n} to answer 200, got {} {:?}",
                created.status, created.body
            );
            link_ids.push(text(&created.body, "linkId"));
        }
        let released = link_ids[0].clone();
        let shape = &shapes[row.shape];

        if row.settled {
            fabric.poll_settled(case, &gateway_id, &released).await;
        }

        // The tags the create assigned are reported for the link while it is stored:
        // a tag map is not a store of its own, it lives on the record.
        let arn = link_arn(&gateway_id, &released);
        let listed = fabric.send("GET", &tags_path(&arn), None).await;
        assert_eq!(
            listed.status, 200,
            "case {case:?}: expected ListTagsForResource on the stored link to answer 200, got {} {:?}",
            listed.status, listed.body
        );
        assert_eq!(
            listed.body["tags"],
            shape.expected_tags(),
            "case {case:?}: expected the link's stored tags {:?}, got {:?}",
            shape.expected_tags(),
            listed.body["tags"]
        );

        let answered = fabric
            .delete_inbound_external_link(&gateway_id, &released)
            .await;
        assert_eq!(
            answered.status, 200,
            "case {case:?}: expected DeleteInboundExternalLink to answer 200, got {} {:?}",
            answered.status, answered.body
        );
        assert_eq!(
            text(&answered.body, "linkId"),
            released,
            "case {case:?}: expected the delete to report the link it released, got {:?}",
            answered.body
        );
        assert_eq!(
            text(&answered.body, "status"),
            RELEASED_STATUS,
            "case {case:?}: expected the terminal status of a released link, got {:?}",
            answered.body
        );
        assert!(
            LINK_STATUS_VALUES.contains(&text(&answered.body, "status").as_str()),
            "case {case:?}: expected a modelled LinkStatus member, got {:?}",
            answered.body
        );

        // The reader no longer resolves it, with this service's fixed not-found
        // sentence...
        let gone = fabric
            .read_inbound_external_link(&gateway_id, &released)
            .await;
        assert_eq!(
            gone.status, 404,
            "case {case:?}: expected the released link to be unreadable, got {} {:?}",
            gone.status, gone.body
        );
        assert_eq!(
            text(&gone.body, "message"),
            RESOURCE_NOT_FOUND_MESSAGE,
            "case {case:?}: expected the service's not-found sentence, got {:?}",
            gone.body
        );
        // ...and nor does the tagging surface, because the tags went with the record
        // rather than outliving it.
        let unresolved = fabric.send("GET", &tags_path(&arn), None).await;
        assert_eq!(
            unresolved.status, TAGGING_UNRESOLVED_STATUS,
            "case {case:?}: expected the released link's ARN to resolve to nothing, got {} {:?}",
            unresolved.status, unresolved.body
        );
        assert_eq!(
            unresolved.code, TAGGING_UNRESOLVED_CODE,
            "case {case:?}: expected this surface's unresolved-ARN code, got {:?}",
            unresolved.code
        );
        assert_eq!(
            text(&unresolved.body, "message"),
            TAGGING_UNRESOLVED_MESSAGE,
            "case {case:?}: expected this surface's unresolved-ARN wording, got {:?}",
            unresolved.body
        );

        // Only the addressed link went: every other link of this gateway still reads
        // back, and the gateway counts exactly those that remain.
        for (n, surviving) in link_ids[1..].iter().enumerate() {
            let read = fabric
                .read_inbound_external_link(&gateway_id, surviving)
                .await;
            assert_eq!(
                (read.status, text(&read.body, "linkId")),
                (200, surviving.clone()),
                "case {case:?}: expected link #{} to survive the release, got {} {:?}",
                n + 1,
                read.status,
                read.body
            );
        }
        let gateway = fabric.responder_gateway_read(case, &gateway_id).await;
        assert_eq!(
            gateway["totalLinksCount"],
            json!(row.held - 1),
            "case {case:?}: expected the gateway to count only the links it still holds, got {:?}",
            gateway["totalLinksCount"]
        );

        // The client token that minted the released link is forgotten with it: the
        // same token mints a NEW link rather than replaying an id the store no longer
        // holds.
        let reused = fabric
            .create_inbound_external_link(&gateway_id, shape.create_body("token-link-0"))
            .await;
        assert_eq!(
            reused.status, 200,
            "case {case:?}: expected the retried create to answer 200, got {} {:?}",
            reused.status, reused.body
        );
        let minted = text(&reused.body, "linkId");
        assert_ne!(
            minted, released,
            "case {case:?}: expected the retry to mint a new link, got the released id"
        );
        let read = fabric
            .read_inbound_external_link(&gateway_id, &minted)
            .await;
        assert_eq!(
            (read.status, text(&read.body, "linkId")),
            (200, minted.clone()),
            "case {case:?}: expected the newly minted link to resolve, got {} {:?}",
            read.status,
            read.body
        );
    }
}

#[tokio::test]
async fn deleting_an_inbound_external_link_refuses_a_pair_that_names_no_stored_link() {
    let case = "pairs that address no stored inbound external link";
    let fabric = Fabric::new();
    let shapes = link_shapes();
    let shape = &shapes[0];

    let gateway_id = fabric.responder_gateway(case, "token-responder").await;
    let other_gateway_id = fabric.responder_gateway(case, "token-other").await;
    let requester_id = fabric.requester_gateway(case, "token-requester").await;

    let created = fabric
        .create_inbound_external_link(&gateway_id, shape.create_body("token-released"))
        .await;
    assert_eq!(
        created.status, 200,
        "case {case:?}: expected the create to answer 200, got {} {:?}",
        created.status, created.body
    );
    let released = text(&created.body, "linkId");
    let answered = fabric
        .delete_inbound_external_link(&gateway_id, &released)
        .await;
    assert_eq!(
        answered.status, 200,
        "case {case:?}: expected the first delete to answer 200, got {} {:?}",
        answered.status, answered.body
    );

    let kept = fabric
        .create_inbound_external_link(&gateway_id, shape.create_body("token-kept"))
        .await;
    assert_eq!(
        kept.status, 200,
        "case {case:?}: expected the second create to answer 200, got {} {:?}",
        kept.status, kept.body
    );
    let kept_id = text(&kept.body, "linkId");

    // Every id below satisfies the modelled patterns, so the refusal is the handler's
    // not-found rather than a constraint refusal. A link with one end outside the
    // fabric is addressed only through the gateway it was created on, so a stored
    // link's id under another gateway names no link at all — and neither does one a
    // delete has already released.
    for (what, delete_gateway, delete_link) in [
        (
            "the link a delete already released",
            gateway_id.clone(),
            released.clone(),
        ),
        (
            "a gateway nothing created",
            "rtb-gw-nothing".to_string(),
            kept_id.clone(),
        ),
        (
            "a link nothing minted",
            gateway_id.clone(),
            "link-nothing".to_string(),
        ),
        (
            "a stored link's id under another responder gateway",
            other_gateway_id.clone(),
            kept_id.clone(),
        ),
        (
            "a requester gateway's id",
            requester_id.clone(),
            kept_id.clone(),
        ),
    ] {
        let refused = fabric
            .delete_inbound_external_link(&delete_gateway, &delete_link)
            .await;
        assert_eq!(
            refused.status, 404,
            "case {case:?}: expected the delete of {what} to answer 404, got {} {:?}",
            refused.status, refused.body
        );
        assert_eq!(
            refused.code, "ResourceNotFoundException",
            "case {case:?}: expected the modelled error code for {what}, got {:?}",
            refused.code
        );
        assert_eq!(
            text(&refused.body, "message"),
            RESOURCE_NOT_FOUND_MESSAGE,
            "case {case:?}: expected the service's not-found sentence for {what}, got {:?}",
            refused.body
        );
    }

    // A refused delete releases nothing: the link that IS stored still reads back.
    let read = fabric
        .read_inbound_external_link(&gateway_id, &kept_id)
        .await;
    assert_eq!(
        (read.status, text(&read.body, "linkId")),
        (200, kept_id.clone()),
        "case {case:?}: expected the stored link to survive every refused delete, got {} {:?}",
        read.status,
        read.body
    );
}
