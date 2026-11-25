// SPDX-License-Identifier: CC0-1.0

//! What a create answers when its `@idempotencyToken` is presented a second
//! time: the status the clock has already made due on the stored record, not the
//! one the first call happened to leave behind.
//!
//! The rule under test is about the clock and the store rather than one recorded
//! request: a handler that reports a stored resource's status materialises the
//! status transitions the clock has already made due before it answers, so the
//! status a retry reports is decided by this call's own instant — never by
//! whether some unrelated call happened to settle the record first.
//!
//! The table spans every create of this service whose retry path answers with a
//! status read off a stored record, which is every record kind that carries a
//! clock-owed transition and every transition one of them is owed:
//! `PENDING_CREATION` -> `ACTIVE` for a requester gateway, for a responder
//! gateway and for an external link of either direction,
//! `CREATION_IN_PROGRESS` -> `ACTIVE` for a link routing rule, and
//! `PENDING_ASSOCIATION` -> `ASSOCIATED` for a certificate association. Every row
//! is driven on both sides of the clock: a retry the transition is not due on yet
//! reports the transient status the create left, and the next retry at or after
//! the due instant reports the settled one — with no other call in between, so
//! nothing but the clock can have moved the record.
//!
//! The due instant is state the create itself wrote, so a row moves it to put
//! the clock on either side of it instead of waiting out a span of time; the one
//! wait the suite does is for the clock to pass an instant the service chose.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_rtbfabric_rtbfabric::state::{
    ComAmazonawsRtbfabricRtbfabricState, CERTIFICATE_STATUS_ASSOCIATED,
    CERTIFICATE_STATUS_PENDING_ASSOCIATION, GATEWAY_STATUS_ACTIVE,
    GATEWAY_STATUS_PENDING_CREATION, LINK_STATUS_ACTIVE, LINK_STATUS_PENDING_CREATION,
    RULE_STATUS_ACTIVE, RULE_STATUS_CREATION_IN_PROGRESS,
};
use service_com_amazonaws_rtbfabric_rtbfabric::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region the test signs for, and so the region every ARN it names.
const REGION: &str = "us-west-2";

/// How long a wait for the clock to reach an instant the service chose is given
/// before the test gives up. Far above any span this service owes a transition,
/// so a slow or loaded host changes how many times the loop looks, never whether
/// it succeeds.
const CLOCK_DEADLINE: Duration = Duration::from_secs(60);

/// How long the wait pauses between two looks at the clock.
const POLL_PAUSE: Duration = Duration::from_millis(20);

/// How far ahead a row puts a record's due instant to stand on the near side of
/// the clock. Far beyond any run of this suite, so the transition cannot come due
/// while the row is checking what a retry answers before it does.
const WELL_AHEAD: Duration = Duration::from_secs(3_600);

/// An ACM certificate ARN inside the modelled `AcmCertificateArn` bounds
/// (`smithy_query(query="shape", shape="AcmCertificateArn")`).
const CERTIFICATE_ARN: &str =
    "arn:aws:acm:us-west-2:123456789012:certificate/9f8e7d6c-5b4a-3210-9f8e-7d6c5b4a3210";

fn text(body: &Value, member: &str) -> String {
    body[member].as_str().unwrap_or_default().to_string()
}

/// Everything an answer carries except the status: the members a retry repeats
/// unchanged whichever side of the clock it falls on.
fn identity(body: &Value) -> Value {
    let mut identity = body.clone();
    if let Some(members) = identity.as_object_mut() {
        members.remove("status");
    }
    identity
}

/// `LinkLogSettings` with its one required member, as
/// `smithy_query(query="shape", shape="LinkLogSettings")` declares it.
fn log_settings() -> Value {
    json!({ "applicationLogs": { "sampling": { "errorLog": 100.0, "filterLog": 0.0 } } })
}

/// A `CreateRequesterGateway` body whose members satisfy the modelled patterns.
fn requester_gateway_body(client_token: &str) -> Value {
    json!({
        "vpcId": "vpc-0123456789abcdef0",
        "subnetIds": ["subnet-0123456789abcdef0"],
        "securityGroupIds": ["sg-0123456789abcdef0"],
        "clientToken": client_token,
    })
}

/// A `CreateResponderGateway` body whose members satisfy the modelled patterns,
/// of `gateway_type` when one is named.
fn responder_gateway_body(client_token: &str, gateway_type: Option<&str>) -> Value {
    let mut body = json!({
        "vpcId": "vpc-0123456789abcdef0",
        "subnetIds": ["subnet-0123456789abcdef0"],
        "securityGroupIds": ["sg-0123456789abcdef0"],
        "port": 443,
        "protocol": "HTTPS",
        "listenerConfig": { "protocols": ["HTTPS"] },
        "domainName": "responder.example.com",
        "clientToken": client_token,
    });
    if let Some(gateway_type) = gateway_type {
        body["gatewayType"] = json!(gateway_type);
    }
    body
}

/// One answered call: the HTTP status and the parsed body.
struct Answer {
    status: u16,
    body: Value,
}

/// A stored record's live status and the instant the clock owes its transition,
/// read straight off the store.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Live {
    status: String,
    settles_at: Option<SystemTime>,
}

impl Live {
    fn new(status: &str, settles_at: Option<SystemTime>) -> Self {
        Self {
            status: status.to_string(),
            settles_at,
        }
    }

    /// The reading a settled record gives: the status it reached, with nothing
    /// further owed on it.
    fn settled(status: &str) -> Self {
        Self::new(status, None)
    }
}

/// Which stored record a row's create wrote, and how the store holds it.
#[derive(Debug, Clone)]
enum Pending {
    RequesterGateway {
        gateway_id: String,
    },
    ResponderGateway {
        gateway_id: String,
    },
    OutboundExternalLink {
        gateway_id: String,
        link_id: String,
    },
    InboundExternalLink {
        gateway_id: String,
        link_id: String,
    },
    RoutingRule {
        link_id: String,
        rule_id: String,
    },
    CertificateAssociation {
        gateway_id: String,
        acm_certificate_arn: String,
    },
}

impl Pending {
    /// This record's status and due instant, or `None` when the store holds no
    /// such record.
    fn live(&self, store: &ComAmazonawsRtbfabricRtbfabricState) -> Option<Live> {
        match self {
            Self::RequesterGateway { gateway_id } => store
                .requester_gateways
                .get(gateway_id)
                .map(|gateway| Live::new(&gateway.status, gateway.settles_at)),
            Self::ResponderGateway { gateway_id } => store
                .responder_gateways
                .get(gateway_id)
                .map(|gateway| Live::new(&gateway.status, gateway.settles_at)),
            Self::OutboundExternalLink {
                gateway_id,
                link_id,
            } => store
                .find_outbound_external_link(gateway_id, link_id)
                .map(|link| Live::new(&link.status, link.settles_at)),
            Self::InboundExternalLink {
                gateway_id,
                link_id,
            } => store
                .find_inbound_external_link(gateway_id, link_id)
                .map(|link| Live::new(&link.status, link.settles_at)),
            Self::RoutingRule { link_id, rule_id } => store
                .routing_rules
                .get(&(link_id.clone(), rule_id.clone()))
                .map(|rule| Live::new(&rule.status, rule.settles_at)),
            Self::CertificateAssociation {
                gateway_id,
                acm_certificate_arn,
            } => store
                .certificate_associations
                .get(&(gateway_id.clone(), acm_certificate_arn.clone()))
                .map(|association| Live::new(&association.status, association.settles_at)),
        }
    }

    /// Put `due` on this record as the instant the clock makes its outstanding
    /// transition due, answering whether the store held the record at all.
    fn set_due(&self, store: &mut ComAmazonawsRtbfabricRtbfabricState, due: SystemTime) -> bool {
        let settles_at = match self {
            Self::RequesterGateway { gateway_id } => store
                .requester_gateways
                .get_mut(gateway_id)
                .map(|gateway| &mut gateway.settles_at),
            Self::ResponderGateway { gateway_id } => store
                .responder_gateways
                .get_mut(gateway_id)
                .map(|gateway| &mut gateway.settles_at),
            Self::OutboundExternalLink {
                gateway_id,
                link_id,
            } => store
                .find_outbound_external_link_mut(gateway_id, link_id)
                .map(|link| &mut link.settles_at),
            Self::InboundExternalLink {
                gateway_id,
                link_id,
            } => store
                .find_inbound_external_link_mut(gateway_id, link_id)
                .map(|link| &mut link.settles_at),
            Self::RoutingRule { link_id, rule_id } => store
                .routing_rules
                .get_mut(&(link_id.clone(), rule_id.clone()))
                .map(|rule| &mut rule.settles_at),
            Self::CertificateAssociation {
                gateway_id,
                acm_certificate_arn,
            } => store
                .certificate_associations
                .get_mut(&(gateway_id.clone(), acm_certificate_arn.clone()))
                .map(|association| &mut association.settles_at),
        };
        match settles_at {
            Some(slot) => {
                *slot = Some(due);
                true
            }
            None => false,
        }
    }
}

/// One row of the table: the create a retry repeats, what the first call
/// answered, the record it wrote, and the two statuses the clock stands between.
struct Row<'a> {
    case: &'a str,
    path: String,
    body: Value,
    created: Answer,
    pending: Pending,
    created_status: &'a str,
    settled_status: &'a str,
}

/// The one emulated service, behind the same `MultiServiceRouter` the production
/// router assembles, so these calls take the real deserialize -> validate ->
/// handle -> serialize path.
struct Fabric {
    router: MultiServiceRouter,
    /// The same shared engine the router hands every handler, so a row can read a
    /// record's live status and due instant without making a call that would
    /// settle it.
    engine: Arc<engine::InMemoryEngine>,
}

impl Fabric {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        Self { router, engine }
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

    /// Send one create and assert it was accepted.
    async fn create(&self, case: &str, path: &str, body: &Value) -> Answer {
        let answered = self.send("POST", path, Some(body.clone())).await;
        assert_eq!(
            answered.status, 200,
            "case {case:?}: expected POST {path} to answer 200, got {} {:?}",
            answered.status, answered.body
        );
        answered
    }

    /// The record's live status and due instant. Taking the state lock is not a
    /// call into the service, so nothing is settled here: that is what lets a row
    /// tell a status the store already held from one a retry had to materialise.
    fn live(&self, case: &str, pending: &Pending) -> Live {
        let state = self.engine.state::<ComAmazonawsRtbfabricRtbfabricState>();
        let store = state.lock();
        pending
            .live(&store)
            .unwrap_or_else(|| panic!("case {case:?}: expected the store to hold {pending:?}"))
    }

    /// Move the record's due instant to `due`.
    fn set_due(&self, case: &str, pending: &Pending, due: SystemTime) {
        let state = self.engine.state::<ComAmazonawsRtbfabricRtbfabricState>();
        let mut store = state.lock();
        assert!(
            pending.set_due(&mut store, due),
            "case {case:?}: expected the store to hold {pending:?}"
        );
    }
}

/// Wait for the clock to reach `due`, looking at it again in a loop rather than
/// sleeping out a span: the deadline is generous, so how fast the host runs
/// changes how many times the loop looks and never whether the wait succeeds.
async fn wait_for_the_clock(case: &str, due: SystemTime) {
    let give_up_at = Instant::now() + CLOCK_DEADLINE;
    while SystemTime::now() < due {
        assert!(
            Instant::now() < give_up_at,
            "case {case:?}: the clock did not reach the instant the service made the transition due at"
        );
        tokio::time::sleep(POLL_PAUSE).await;
    }
}

/// Drive one row: a retry before the due instant reports the transient status,
/// and the next retry, at or after it, reports the settled one — with no other
/// call touching the store in between.
async fn a_retry_reports_the_clock(fabric: &Fabric, row: &Row<'_>) {
    let Row {
        case,
        path,
        body,
        created,
        pending,
        created_status,
        settled_status,
    } = row;

    assert_eq!(
        text(&created.body, "status"),
        *created_status,
        "case {case:?}: expected the create to report {created_status:?}, got {:?}",
        created.body
    );
    let owed = fabric.live(case, pending);
    assert_eq!(
        owed.status, *created_status,
        "case {case:?}: expected the create to store {created_status:?}, got {owed:?}"
    );
    let due = owed.settles_at.unwrap_or_else(|| {
        panic!("case {case:?}: expected the create to leave a due instant on the record, got {owed:?}")
    });

    // The near side of the clock: nothing is owed yet, so the retry reports the
    // status the record holds and moves nothing.
    let not_due_yet = due + WELL_AHEAD;
    fabric.set_due(case, pending, not_due_yet);
    let early = fabric.create(case, path, body).await;
    assert_eq!(
        text(&early.body, "status"),
        *created_status,
        "case {case:?}: expected a retry the clock owes nothing on to report {created_status:?}, got {:?}",
        early.body
    );
    assert_eq!(
        identity(&early.body),
        identity(&created.body),
        "case {case:?}: expected the retry to answer for the same resource, got {:?} after {:?}",
        early.body,
        created.body
    );
    let after_early = fabric.live(case, pending);
    assert_eq!(
        after_early,
        Live::new(created_status, Some(not_due_yet)),
        "case {case:?}: expected the retry to leave the record as it stood, got {after_early:?}"
    );

    // The far side: the due instant the create chose is restored, the clock is
    // waited out, and the retry is the FIRST call to reach the store after it —
    // so the settled status it reports can only have come from the clock.
    fabric.set_due(case, pending, due);
    wait_for_the_clock(case, due).await;
    let retry = fabric.create(case, path, body).await;
    assert_eq!(
        text(&retry.body, "status"),
        *settled_status,
        "case {case:?}: expected the retry at or after the due instant to report {settled_status:?}, got {:?}",
        retry.body
    );
    assert_eq!(
        identity(&retry.body),
        identity(&created.body),
        "case {case:?}: expected the retry to answer for the same resource, got {:?} after {:?}",
        retry.body,
        created.body
    );
    let after_retry = fabric.live(case, pending);
    assert_eq!(
        after_retry,
        Live::settled(settled_status),
        "case {case:?}: expected the retry to materialise the transition on the record, got {after_retry:?}"
    );

    // Every later retry answers exactly the same, so the status came from the
    // record and the clock rather than from the traffic that reached the store.
    let again = fabric.create(case, path, body).await;
    assert_eq!(
        again.body, retry.body,
        "case {case:?}: expected a further retry to answer {:?}, got {:?}",
        retry.body, again.body
    );
}

/// `CreateRequesterGateway`: a gateway created `PENDING_CREATION` and owed
/// `ACTIVE`.
async fn requester_gateway_row(fabric: &Fabric) -> Row<'static> {
    let case = "a requester gateway";
    let path = "/requester-gateway".to_string();
    let body = requester_gateway_body("retry-requester-gateway");
    let created = fabric.create(case, &path, &body).await;
    let gateway_id = text(&created.body, "gatewayId");
    Row {
        case,
        path,
        body,
        created,
        pending: Pending::RequesterGateway { gateway_id },
        created_status: GATEWAY_STATUS_PENDING_CREATION,
        settled_status: GATEWAY_STATUS_ACTIVE,
    }
}

/// `CreateResponderGateway`: the other gateway kind, on the same transition.
async fn responder_gateway_row(fabric: &Fabric) -> Row<'static> {
    let case = "a responder gateway";
    let path = "/responder-gateway".to_string();
    let body = responder_gateway_body("retry-responder-gateway", None);
    let created = fabric.create(case, &path, &body).await;
    let gateway_id = text(&created.body, "gatewayId");
    Row {
        case,
        path,
        body,
        created,
        pending: Pending::ResponderGateway { gateway_id },
        created_status: GATEWAY_STATUS_PENDING_CREATION,
        settled_status: GATEWAY_STATUS_ACTIVE,
    }
}

/// `CreateOutboundExternalLink`: a link from a requester gateway to an endpoint
/// outside the fabric.
async fn outbound_external_link_row(fabric: &Fabric) -> Row<'static> {
    let case = "an outbound external link";
    let gateway = fabric
        .create(
            case,
            "/requester-gateway",
            &requester_gateway_body("retry-outbound-gateway"),
        )
        .await;
    let gateway_id = text(&gateway.body, "gatewayId");
    let path = format!("/requester-gateway/{gateway_id}/outbound-external-link");
    let body = json!({
        "clientToken": "retry-outbound-external-link",
        "publicEndpoint": "https://bidder.example.com/rtb",
        "logSettings": log_settings(),
    });
    let created = fabric.create(case, &path, &body).await;
    let link_id = text(&created.body, "linkId");
    Row {
        case,
        path,
        body,
        created,
        pending: Pending::OutboundExternalLink {
            gateway_id,
            link_id,
        },
        created_status: LINK_STATUS_PENDING_CREATION,
        settled_status: LINK_STATUS_ACTIVE,
    }
}

/// `CreateInboundExternalLink`: a link arriving at a responder gateway from
/// outside the fabric.
async fn inbound_external_link_row(fabric: &Fabric) -> Row<'static> {
    let case = "an inbound external link";
    let gateway = fabric
        .create(
            case,
            "/responder-gateway",
            &responder_gateway_body("retry-inbound-gateway", Some("EXTERNAL")),
        )
        .await;
    let gateway_id = text(&gateway.body, "gatewayId");
    let path = format!("/responder-gateway/{gateway_id}/inbound-external-link");
    let body = json!({
        "clientToken": "retry-inbound-external-link",
        "logSettings": log_settings(),
    });
    let created = fabric.create(case, &path, &body).await;
    let link_id = text(&created.body, "linkId");
    Row {
        case,
        path,
        body,
        created,
        pending: Pending::InboundExternalLink {
            gateway_id,
            link_id,
        },
        created_status: LINK_STATUS_PENDING_CREATION,
        settled_status: LINK_STATUS_ACTIVE,
    }
}

/// `CreateLinkRoutingRule`: a rule on a link between two gateways, created
/// `CREATION_IN_PROGRESS` and owed `ACTIVE`.
async fn routing_rule_row(fabric: &Fabric) -> Row<'static> {
    let case = "a link routing rule";
    let requester = fabric
        .create(
            case,
            "/requester-gateway",
            &requester_gateway_body("retry-rule-requester"),
        )
        .await;
    let responder = fabric
        .create(
            case,
            "/responder-gateway",
            &responder_gateway_body("retry-rule-responder", None),
        )
        .await;
    let requester_id = text(&requester.body, "gatewayId");
    let responder_id = text(&responder.body, "gatewayId");
    let link = fabric
        .create(
            case,
            &format!("/gateway/{requester_id}/create-link"),
            &json!({ "peerGatewayId": responder_id, "logSettings": log_settings() }),
        )
        .await;
    let link_id = text(&link.body, "linkId");
    let path = format!("/responder-gateway/{responder_id}/link/{link_id}/routing-rule");
    let body = json!({
        "clientToken": "retry-routing-rule",
        "priority": 7,
        "conditions": { "pathPrefix": "/bid" },
    });
    let created = fabric.create(case, &path, &body).await;
    let rule_id = text(&created.body, "ruleId");
    Row {
        case,
        path,
        body,
        created,
        pending: Pending::RoutingRule { link_id, rule_id },
        created_status: RULE_STATUS_CREATION_IN_PROGRESS,
        settled_status: RULE_STATUS_ACTIVE,
    }
}

/// `AssociateCertificate`: the one record of the six whose identity is the pair
/// the caller named rather than an id the service minted.
async fn certificate_association_row(fabric: &Fabric) -> Row<'static> {
    let case = "a certificate association";
    let gateway = fabric
        .create(
            case,
            "/responder-gateway",
            &responder_gateway_body("retry-certificate-gateway", Some("EXTERNAL")),
        )
        .await;
    let gateway_id = text(&gateway.body, "gatewayId");
    let path = format!("/responder-gateway/{gateway_id}/certificate");
    let body = json!({
        "acmCertificateArn": CERTIFICATE_ARN,
        "clientToken": "retry-certificate-association",
    });
    let created = fabric.create(case, &path, &body).await;
    Row {
        case,
        path,
        body,
        created,
        pending: Pending::CertificateAssociation {
            gateway_id,
            acm_certificate_arn: CERTIFICATE_ARN.to_string(),
        },
        created_status: CERTIFICATE_STATUS_PENDING_ASSOCIATION,
        settled_status: CERTIFICATE_STATUS_ASSOCIATED,
    }
}

/// Every create whose retry path reports a stored status answers the status the
/// clock has made due at the instant of the retry.
///
/// Each row is built and driven before the next one is built, so the record under
/// test is the newest in the store and no later call has had a chance to settle
/// it.
#[tokio::test]
async fn a_retry_of_a_create_reports_the_status_the_clock_has_made_due() {
    let fabric = Fabric::new();

    let row = requester_gateway_row(&fabric).await;
    a_retry_reports_the_clock(&fabric, &row).await;

    let row = responder_gateway_row(&fabric).await;
    a_retry_reports_the_clock(&fabric, &row).await;

    let row = outbound_external_link_row(&fabric).await;
    a_retry_reports_the_clock(&fabric, &row).await;

    let row = inbound_external_link_row(&fabric).await;
    a_retry_reports_the_clock(&fabric, &row).await;

    let row = routing_rule_row(&fabric).await;
    a_retry_reports_the_clock(&fabric, &row).await;

    let row = certificate_association_row(&fabric).await;
    a_retry_reports_the_clock(&fabric, &row).await;
}
