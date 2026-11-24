// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for `UpdateLinkModuleFlow`: the module flow a link runs.
//!
//! The rules under test are about state, not about one recorded request. The
//! modules a call states are the WHOLE flow — they replace whatever flow the
//! link had taken, in the order stated, and a flow of no modules clears it —
//! and a flow reaches service exactly when the link carrying it does, so it is
//! reported as the link's pending flow until the clock brings the link up and
//! as the flow in service afterwards. A retry under the same `clientToken` is
//! answered from the record, the same token restating other modules is refused,
//! a link in a status nothing moves it out of cannot be configured, a flow past
//! the modules one link holds is refused, and an id that names no link on the
//! addressed gateway is not found.
//!
//! The tables span what those rules generalise over: how many modules a flow
//! holds, which of a module's optional members the call states, the order the
//! modules are stated in, the empty flow, which of the link's two ends
//! addresses the call, whether the link is up or still moving through its own
//! lifecycle, every member of the modelled `LinkStatus` enum, and the published
//! bound on a flow's size together with one module past it.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_rtbfabric_rtbfabric::ops::types::LINK_STATUS_VALUES;
use service_com_amazonaws_rtbfabric_rtbfabric::state::{
    ComAmazonawsRtbfabricRtbfabricState, FLOW_MODULES_PER_LINK_QUOTA, LINK_STATUS_ACTIVE,
    LINK_STATUS_DELETED, LINK_STATUS_FAILED, LINK_STATUS_PENDING_REQUEST,
    LINK_STATUS_PENDING_UPDATE, LINK_STATUS_REJECTED, LINK_STATUS_REQUESTED,
    RESOURCE_NOT_FOUND_MESSAGE,
};
use service_com_amazonaws_rtbfabric_rtbfabric::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region the test signs for.
const REGION: &str = "us-west-2";

/// How long a poll waits for a status the clock owes before it gives up. Far
/// above any span this service owes a transition, so a slow or loaded host
/// changes how many times the loop looks, never whether it succeeds.
const POLL_DEADLINE: Duration = Duration::from_secs(60);

/// How long a poll waits between two looks.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// `LinkLogSettings` with its one required member, as
/// `smithy_query(query="shape", shape="LinkLogSettings")` declares it.
fn log_settings() -> Value {
    json!({ "applicationLogs": { "sampling": { "errorLog": 100.0, "filterLog": 0.0 } } })
}

fn text(body: &Value, member: &str) -> String {
    body[member].as_str().unwrap_or_default().to_string()
}

/// The module names of a reported `ModuleConfigurationList`, in the order the
/// list reports them.
fn names(modules: &Value) -> Vec<String> {
    modules
        .as_array()
        .map(|modules| {
            modules
                .iter()
                .map(|module| text(module, "name"))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

/// The one emulated service, behind the same `MultiServiceRouter` the production
/// router assembles, so these calls take the real deserialize -> validate ->
/// handle -> serialize path.
struct Fabric {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
}

impl Fabric {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        Self { router, engine }
    }

    /// Send one restJson1 request and return `(status, parsed body)`.
    async fn send(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        let (status, _, answered) = self.send_with_code(method, path, body).await;
        (status, answered)
    }

    /// [`send`](Self::send), with the wire error code from `x-amzn-errortype`: a
    /// refusal is a status AND a code, and a caller's SDK dispatches on the code.
    async fn send_with_code(
        &self,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> (u16, String, Value) {
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
        let parsed = if response.body().is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(response.body()).unwrap_or(Value::Null)
        };
        (status, code, parsed)
    }

    /// A requester gateway and a responder gateway, both over the wire, as
    /// `(the requester's id, the responder's id)`.
    async fn gateway_pair(&self, token: &str) -> (String, String) {
        let (status, requester) = self
            .send(
                "POST",
                "/requester-gateway",
                Some(json!({
                    "vpcId": "vpc-0123456789abcdef0",
                    "subnetIds": ["subnet-0123456789abcdef0"],
                    "securityGroupIds": ["sg-0123456789abcdef0"],
                    "clientToken": format!("{token}-requester"),
                })),
            )
            .await;
        assert_eq!(status, 200, "CreateRequesterGateway: {status} {requester:?}");
        let (status, responder) = self
            .send(
                "POST",
                "/responder-gateway",
                Some(json!({
                    "vpcId": "vpc-0123456789abcdef0",
                    "subnetIds": ["subnet-0123456789abcdef0"],
                    "securityGroupIds": ["sg-0123456789abcdef0"],
                    "port": 443,
                    "protocol": "HTTPS",
                    "domainName": "responder.example.com",
                    "clientToken": format!("{token}-responder"),
                })),
            )
            .await;
        assert_eq!(status, 200, "CreateResponderGateway: {status} {responder:?}");
        (text(&requester, "gatewayId"), text(&responder, "gatewayId"))
    }

    /// A requester gateway, a responder gateway and a link opened from the
    /// requester to the responder, all over the wire.
    async fn linked_pair(&self, token: &str) -> Pair {
        let (requester_id, responder_id) = self.gateway_pair(token).await;
        let (status, link) = self
            .send(
                "POST",
                &format!("/gateway/{requester_id}/create-link"),
                Some(json!({
                    "peerGatewayId": responder_id,
                    "logSettings": log_settings(),
                })),
            )
            .await;
        assert_eq!(status, 200, "CreateLink: {status} {link:?}");
        Pair {
            requester_id,
            responder_id,
            link_id: text(&link, "linkId"),
        }
    }

    /// The link of [`linked_pair`](Self::linked_pair), brought up: the peer
    /// accepts the request and the clock then makes `ACTIVE` due.
    async fn active_link(&self, token: &str) -> Pair {
        let pair = self.linked_pair(token).await;
        let (status, accepted) = self
            .send(
                "POST",
                &format!(
                    "/gateway/{}/link/{}/accept",
                    pair.responder_id, pair.link_id
                ),
                Some(json!({ "logSettings": log_settings() })),
            )
            .await;
        assert_eq!(status, 200, "AcceptLink: {status} {accepted:?}");
        self.poll_link_status(&pair.requester_id, &pair.link_id, LINK_STATUS_ACTIVE)
            .await;
        pair
    }

    /// Read the link through `GetLink`, asserting the read itself answered.
    async fn link(&self, case: &str, gateway_id: &str, link_id: &str) -> Value {
        let path = format!("/gateway/{gateway_id}/link/{link_id}");
        let (status, body) = self.send("GET", &path, None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected GetLink on {path:?} to answer 200, got {status} {body:?}"
        );
        body
    }

    /// Read the link through `GetLink` until it reports `expected`, and answer
    /// with that reading.
    ///
    /// A settled status is owed by the clock, so the test waits for the
    /// CONDITION rather than for a span: it looks again until the status holds,
    /// giving up only once [`POLL_DEADLINE`] has passed.
    async fn poll_link_status(&self, gateway_id: &str, link_id: &str, expected: &str) -> Value {
        let path = format!("/gateway/{gateway_id}/link/{link_id}");
        let deadline = Instant::now() + POLL_DEADLINE;
        loop {
            let (status, body) = self.send("GET", &path, None).await;
            assert_eq!(
                status, 200,
                "expected GetLink on {path:?} to answer 200, got {status} {body:?}"
            );
            if text(&body, "status") == expected {
                return body;
            }
            assert!(
                Instant::now() < deadline,
                "expected {path:?} to reach {expected:?}, got {:?}",
                body["status"]
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    /// Call `UpdateLinkModuleFlow` over the wire.
    async fn configure_flow(
        &self,
        gateway_id: &str,
        link_id: &str,
        client_token: &str,
        modules: &Value,
    ) -> (u16, String, Value) {
        self.send_with_code(
            "POST",
            &format!("/gateway/{gateway_id}/link/{link_id}/module-flow"),
            Some(json!({ "clientToken": client_token, "modules": modules })),
        )
        .await
    }

    /// The stored record of the one link, as `(its status, the module names of
    /// the flow in service, the module names of the flow awaiting it)`.
    ///
    /// Read straight out of the store rather than over the wire: nothing but a
    /// call moves a record on, so what the store holds the instant after a call
    /// is exactly what that call left, whatever the host's speed.
    fn stored_flow(&self) -> (String, Vec<String>, Option<Vec<String>>) {
        let state = self.engine.state::<ComAmazonawsRtbfabricRtbfabricState>();
        let store = state.lock();
        let link = store.links.values().next().expect("the stored link");
        let stated = |modules: &[service_com_amazonaws_rtbfabric_rtbfabric::ops::types::ModuleConfiguration]| {
            modules
                .iter()
                .map(|module| module.name.clone())
                .collect::<Vec<_>>()
        };
        (
            link.status.clone(),
            stated(&link.flow_modules),
            link.pending_module_flow.as_deref().map(stated),
        )
    }

    /// Put `status` on every stored link.
    ///
    /// The statuses a link reaches beyond its own request and acceptance are
    /// driven by operations this crate has not implemented yet, so the state is
    /// set directly to cover the whole modelled enum rather than only the part
    /// the wire reaches today.
    fn force_link_status(&self, status: &str) {
        let state = self.engine.state::<ComAmazonawsRtbfabricRtbfabricState>();
        let mut store = state.lock();
        for link in store.links.values_mut() {
            link.status = status.to_string();
            link.settles_at = None;
        }
    }
}

/// The gateway pair and the link between them, as the service minted them.
struct Pair {
    requester_id: String,
    responder_id: String,
    link_id: String,
}

/// One flow row: the modules a call states, and which of the link's two ends
/// states them.
struct FlowRow {
    name: &'static str,
    /// `true` for the end the link was NOT opened from: either end addresses
    /// the same record.
    from_peer: bool,
    modules: Value,
}

/// The flows the tables drive. The members of a `ModuleConfiguration` are those
/// `smithy_query(query="shape", shape="ModuleConfiguration")` declares: a
/// required `name`, and the optional `version`, `dependsOn` and
/// `moduleParameters`; `moduleParameters` is a union whose members are `noBid`,
/// `openRtbAttribute` and `rateLimiter`.
fn flow_rows() -> Vec<FlowRow> {
    vec![
        FlowRow {
            name: "one module, its required name only",
            from_peer: false,
            modules: json!([{ "name": "Rate Limiter" }]),
        },
        FlowRow {
            name: "one module with a version and rate-limiter parameters",
            from_peer: true,
            modules: json!([{
                "name": "rate-limit",
                "version": "v1-2",
                "moduleParameters": { "rateLimiter": { "tps": 1500.0 } },
            }]),
        },
        FlowRow {
            name: "three modules, each depending on the one before",
            from_peer: false,
            modules: json!([
                { "name": "Ingest" },
                { "name": "Filter", "dependsOn": ["Ingest"], "version": "2" },
                {
                    "name": "No Bid",
                    "dependsOn": ["Filter"],
                    "moduleParameters": {
                        "noBid": { "reason": "budget", "reasonCode": 3, "passThroughPercentage": 25.0 },
                    },
                },
            ]),
        },
        FlowRow {
            name: "the same three modules, stated in another order",
            from_peer: true,
            modules: json!([
                {
                    "name": "No Bid",
                    "dependsOn": ["Filter"],
                    "moduleParameters": {
                        "noBid": { "reason": "budget", "reasonCode": 3, "passThroughPercentage": 25.0 },
                    },
                },
                { "name": "Ingest" },
                { "name": "Filter", "dependsOn": ["Ingest"], "version": "2" },
            ]),
        },
        FlowRow {
            name: "a module with open-RTB attribute parameters",
            from_peer: false,
            modules: json!([{
                "name": "Attribute Filter",
                "moduleParameters": {
                    "openRtbAttribute": {
                        "filterType": "EXCLUDE",
                        "filterConfiguration": [{
                            "criteria": [{ "path": "imp.banner.w", "values": ["300", "728"] }],
                        }],
                        "action": { "headerTag": { "name": "x-tag", "value": "blocked" } },
                        "holdbackPercentage": 0.0,
                    },
                },
            }]),
        },
    ]
}

#[tokio::test]
async fn a_flow_a_live_link_takes_is_in_service_once_the_clock_brings_the_link_back_up() {
    for row in flow_rows() {
        let case = row.name;
        let fabric = Fabric::new();
        let pair = fabric.active_link("token").await;
        let (addressed, other) = if row.from_peer {
            (&pair.responder_id, &pair.requester_id)
        } else {
            (&pair.requester_id, &pair.responder_id)
        };

        // A link nobody has configured runs no flow and has none awaiting it.
        let before = fabric.link(case, addressed, &pair.link_id).await;
        for member in ["flowModules", "pendingFlowModules"] {
            assert_eq!(
                before[member],
                json!([]),
                "case {case:?}: expected no {member} before the flow is configured, got {:?}",
                before[member]
            );
        }

        let (status, code, answered) = fabric
            .configure_flow(addressed, &pair.link_id, "flow", &row.modules)
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected UpdateLinkModuleFlow to answer 200, got {status} {code:?} {answered:?}"
        );
        // The link is carrying traffic, so the change is applied to a live link
        // and reported as pending.
        assert_eq!(
            text(&answered, "status"),
            LINK_STATUS_PENDING_UPDATE,
            "case {case:?}: expected the live link's update to be PENDING_UPDATE, got {:?}",
            answered["status"]
        );
        assert_eq!(
            text(&answered, "linkId"),
            pair.link_id,
            "case {case:?}: expected link {:?}, got {:?}",
            pair.link_id,
            answered["linkId"]
        );
        assert_eq!(
            text(&answered, "gatewayId").as_str(),
            addressed.as_str(),
            "case {case:?}: expected the addressed gateway {addressed:?}, got {:?}",
            answered["gatewayId"]
        );

        // The flow has been taken and is not in service yet: the record holds it
        // as the flow awaiting the link, and the flow in service is untouched.
        let (stored_status, in_service, awaiting) = fabric.stored_flow();
        assert_eq!(
            stored_status, LINK_STATUS_PENDING_UPDATE,
            "case {case:?}: expected the record to stand in PENDING_UPDATE, got {stored_status:?}"
        );
        assert!(
            in_service.is_empty(),
            "case {case:?}: expected the flow in service to be untouched, got {in_service:?}"
        );
        assert_eq!(
            awaiting.as_deref(),
            Some(names(&row.modules).as_slice()),
            "case {case:?}: expected the stated flow to await the link, got {awaiting:?}"
        );

        // The clock owes the link its way back up, and the flow reaches service
        // with it: both ends then read the stated flow, in the stated order, and
        // nothing is awaiting the link any more.
        for reader in [addressed, other] {
            let settled = fabric
                .poll_link_status(reader, &pair.link_id, LINK_STATUS_ACTIVE)
                .await;
            assert_eq!(
                settled["flowModules"], row.modules,
                "case {case:?}: expected {reader:?} to report the stated flow in service, got {:?}",
                settled["flowModules"]
            );
            assert_eq!(
                settled["pendingFlowModules"],
                json!([]),
                "case {case:?}: expected nothing awaiting the link, got {:?}",
                settled["pendingFlowModules"]
            );
        }

        // The modules a later call states are the whole flow, so a flow of no
        // modules clears the one in service.
        let (status, code, cleared) = fabric
            .configure_flow(addressed, &pair.link_id, "cleared", &json!([]))
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the empty flow to answer 200, got {status} {code:?} {cleared:?}"
        );
        let settled = fabric
            .poll_link_status(addressed, &pair.link_id, LINK_STATUS_ACTIVE)
            .await;
        for member in ["flowModules", "pendingFlowModules"] {
            assert_eq!(
                settled[member],
                json!([]),
                "case {case:?}: expected the empty flow to clear {member}, got {:?}",
                settled[member]
            );
        }
    }
}

#[tokio::test]
async fn a_flow_stated_before_the_link_is_up_comes_into_service_with_the_link() {
    // Whether the clock has settled PENDING_REQUEST -> REQUESTED before the flow
    // is stated: a link still moving through its own lifecycle takes the flow
    // either way.
    for settle_first in [false, true] {
        let case = if settle_first {
            "a settled request takes a flow"
        } else {
            "an unsettled request takes a flow"
        };
        let fabric = Fabric::new();
        let pair = fabric.linked_pair("token").await;
        if settle_first {
            fabric
                .poll_link_status(&pair.requester_id, &pair.link_id, LINK_STATUS_REQUESTED)
                .await;
        }
        let modules = json!([
            { "name": "Ingest" },
            { "name": "Rate Limiter", "dependsOn": ["Ingest"], "version": "v1" },
        ]);

        let (status, code, answered) = fabric
            .configure_flow(&pair.requester_id, &pair.link_id, "flow", &modules)
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected UpdateLinkModuleFlow to answer 200, got {status} {code:?} {answered:?}"
        );
        // The link is not up, so nothing about its own lifecycle changes: it
        // keeps the status that lifecycle owes it.
        let reported = text(&answered, "status");
        assert!(
            [LINK_STATUS_PENDING_REQUEST, LINK_STATUS_REQUESTED].contains(&reported.as_str()),
            "case {case:?}: expected the link to keep its outstanding status, got {reported:?}"
        );

        // Nothing moves a link out of an outstanding request but an answer to
        // it, so the flow stays the one awaiting the link for every reader.
        let awaiting = fabric.link(case, &pair.responder_id, &pair.link_id).await;
        assert_eq!(
            awaiting["pendingFlowModules"], modules,
            "case {case:?}: expected the stated flow to await the link, got {:?}",
            awaiting["pendingFlowModules"]
        );
        assert_eq!(
            awaiting["flowModules"],
            json!([]),
            "case {case:?}: expected no flow in service yet, got {:?}",
            awaiting["flowModules"]
        );

        // The peer accepts the request, so the clock brings the link up — and
        // the flow it took before comes into service with it.
        let (status, accepted) = fabric
            .send(
                "POST",
                &format!(
                    "/gateway/{}/link/{}/accept",
                    pair.responder_id, pair.link_id
                ),
                Some(json!({ "logSettings": log_settings() })),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected AcceptLink to answer 200, got {status} {accepted:?}"
        );
        let settled = fabric
            .poll_link_status(&pair.requester_id, &pair.link_id, LINK_STATUS_ACTIVE)
            .await;
        assert_eq!(
            settled["flowModules"], modules,
            "case {case:?}: expected the taken flow in service once the link is up, got {:?}",
            settled["flowModules"]
        );
        assert_eq!(
            settled["pendingFlowModules"],
            json!([]),
            "case {case:?}: expected nothing awaiting the link, got {:?}",
            settled["pendingFlowModules"]
        );
    }
}

#[tokio::test]
async fn a_retried_client_token_is_answered_from_the_record_and_a_restated_flow_is_refused() {
    let fabric = Fabric::new();
    let pair = fabric.linked_pair("token").await;
    // An outstanding request is settled first, so no transition is owed and
    // every answer below is the record's own.
    fabric
        .poll_link_status(&pair.requester_id, &pair.link_id, LINK_STATUS_REQUESTED)
        .await;
    let first = json!([{ "name": "Ingest" }, { "name": "Filter", "dependsOn": ["Ingest"] }]);
    let other = json!([{ "name": "Ingest" }]);

    let (status, code, answered) = fabric
        .configure_flow(&pair.requester_id, &pair.link_id, "retried", &first)
        .await;
    assert_eq!(
        status, 200,
        "expected the first call to answer 200, got {status} {code:?} {answered:?}"
    );

    // The same token carrying the same flow asks for a change the link has
    // already taken, so it is answered from the record rather than applied again.
    let (status, code, repeat) = fabric
        .configure_flow(&pair.requester_id, &pair.link_id, "retried", &first)
        .await;
    assert_eq!(
        status, 200,
        "expected the retry to answer 200, got {status} {code:?} {repeat:?}"
    );
    assert_eq!(
        repeat, answered,
        "expected the retry to answer the first call's record, got {repeat:?}"
    );

    // The same token restating other modules would answer for a flow the caller
    // did not send, so it is refused — and the record keeps the flow it took.
    let (status, code, refused) = fabric
        .configure_flow(&pair.requester_id, &pair.link_id, "retried", &other)
        .await;
    assert_eq!(
        status, 400,
        "expected a mismatched retry to be refused, got {status} {refused:?}"
    );
    assert_eq!(
        code, "ValidationException",
        "expected ValidationException, got {code:?}"
    );
    assert_eq!(
        text(&refused, "message"),
        "clientToken retried was already used for a UpdateLinkModuleFlow request with \
         different parameters",
        "expected the mismatch to be stated, got {:?}",
        refused["message"]
    );
    let held = fabric
        .link("a refused retry changes nothing", &pair.requester_id, &pair.link_id)
        .await;
    assert_eq!(
        held["pendingFlowModules"], first,
        "expected the refused retry to leave the taken flow, got {:?}",
        held["pendingFlowModules"]
    );

    // A token of its own is a request of its own, so the flow it states replaces
    // the one the link had taken.
    let (status, code, replaced) = fabric
        .configure_flow(&pair.requester_id, &pair.link_id, "fresh", &other)
        .await;
    assert_eq!(
        status, 200,
        "expected a fresh token to answer 200, got {status} {code:?} {replaced:?}"
    );
    let held = fabric
        .link("a fresh token replaces the flow", &pair.requester_id, &pair.link_id)
        .await;
    assert_eq!(
        held["pendingFlowModules"], other,
        "expected the restated flow to replace the taken one, got {:?}",
        held["pendingFlowModules"]
    );
}

#[tokio::test]
async fn only_a_link_in_a_status_nothing_moves_it_out_of_refuses_a_flow() {
    // Every member of the modelled `LinkStatus` enum, so the rule is stated over
    // the whole enum rather than over the statuses the wire reaches today.
    for status_member in LINK_STATUS_VALUES {
        let case = *status_member;
        let fabric = Fabric::new();
        let pair = fabric.linked_pair("token").await;
        fabric.force_link_status(case);
        let modules = json!([{ "name": "Rate Limiter" }]);

        let (status, code, body) = fabric
            .configure_flow(&pair.requester_id, &pair.link_id, "flow", &modules)
            .await;

        let terminal = [
            LINK_STATUS_REJECTED,
            LINK_STATUS_FAILED,
            LINK_STATUS_DELETED,
        ]
        .contains(&case);
        if terminal {
            assert_eq!(
                status, 409,
                "case {case:?}: expected a terminal link to refuse the flow, got {status} {body:?}"
            );
            assert_eq!(
                code, "ConflictException",
                "case {case:?}: expected ConflictException, got {code:?}"
            );
            assert_eq!(
                text(&body, "message"),
                format!(
                    "link {} on gateway {} is {case} and cannot be configured",
                    pair.link_id, pair.requester_id
                ),
                "case {case:?}: expected the status that refuses it to be stated, got {:?}",
                body["message"]
            );
            let (_, in_service, awaiting) = fabric.stored_flow();
            assert!(
                in_service.is_empty() && awaiting.is_none(),
                "case {case:?}: expected the refusal to store no flow, got {in_service:?} {awaiting:?}"
            );
            continue;
        }

        assert_eq!(
            status, 200,
            "case {case:?}: expected the link to take the flow, got {status} {code:?} {body:?}"
        );
        // A link that is up is answered PENDING_UPDATE, because the change is
        // applied under live traffic; any other status is the link's own
        // lifecycle, which this call does not move it out of.
        let expected = if [LINK_STATUS_ACTIVE, LINK_STATUS_PENDING_UPDATE].contains(&case) {
            LINK_STATUS_PENDING_UPDATE
        } else {
            case
        };
        assert_eq!(
            text(&body, "status"),
            expected,
            "case {case:?}: expected {expected:?}, got {:?}",
            body["status"]
        );
        let (_, _, awaiting) = fabric.stored_flow();
        assert_eq!(
            awaiting.as_deref(),
            Some(names(&modules).as_slice()),
            "case {case:?}: expected the stated flow to be stored, got {awaiting:?}"
        );
    }
}

#[tokio::test]
async fn an_id_that_names_no_link_on_the_addressed_gateway_is_not_found() {
    let fabric = Fabric::new();
    let pair = fabric.linked_pair("token").await;
    // A third gateway, which is no end of that link.
    let (outsider_id, _) = fabric.gateway_pair("outsider").await;

    for (case, gateway_id, link_id) in [
        (
            "a link id no record carries",
            pair.requester_id.clone(),
            "link-no-such-link".to_string(),
        ),
        (
            "a gateway that is no end of the link",
            outsider_id,
            pair.link_id.clone(),
        ),
        (
            "a gateway no record carries",
            "rtb-gw-no-such-gateway".to_string(),
            pair.link_id.clone(),
        ),
    ] {
        let (status, code, body) = fabric
            .configure_flow(&gateway_id, &link_id, "flow", &json!([{ "name": "Ingest" }]))
            .await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected 404, got {status} {code:?} {body:?}"
        );
        assert_eq!(
            code, "ResourceNotFoundException",
            "case {case:?}: expected ResourceNotFoundException, got {code:?}"
        );
        assert_eq!(
            text(&body, "message"),
            RESOURCE_NOT_FOUND_MESSAGE,
            "case {case:?}: expected the service's fixed not-found wording, got {:?}",
            body["message"]
        );
    }
}

#[tokio::test]
async fn a_flow_is_taken_up_to_the_bound_on_one_links_modules_and_refused_past_it() {
    // The bound itself, and one module past it: the published bound is the
    // largest flow a link takes.
    for (case, count) in [
        ("the bound itself", FLOW_MODULES_PER_LINK_QUOTA),
        ("one module past the bound", FLOW_MODULES_PER_LINK_QUOTA + 1),
    ] {
        let fabric = Fabric::new();
        let pair = fabric.linked_pair("token").await;
        let modules = Value::Array(
            (0..count)
                .map(|index| json!({ "name": format!("Module {index}") }))
                .collect(),
        );

        let (status, code, body) = fabric
            .configure_flow(&pair.requester_id, &pair.link_id, "flow", &modules)
            .await;

        if count > FLOW_MODULES_PER_LINK_QUOTA {
            assert_eq!(
                status, 402,
                "case {case:?}: expected the flow to be refused, got {status} {body:?}"
            );
            assert_eq!(
                code, "ServiceQuotaExceededException",
                "case {case:?}: expected ServiceQuotaExceededException, got {code:?}"
            );
            assert_eq!(
                text(&body, "message"),
                format!(
                    "this emulator holds a limit of {FLOW_MODULES_PER_LINK_QUOTA} modules in a \
                     link's module flow"
                ),
                "case {case:?}: expected the bound to be stated, got {:?}",
                body["message"]
            );
            let (_, _, awaiting) = fabric.stored_flow();
            assert!(
                awaiting.is_none(),
                "case {case:?}: expected the refusal to store no flow, got {awaiting:?}"
            );
            continue;
        }

        assert_eq!(
            status, 200,
            "case {case:?}: expected the flow to be taken, got {status} {code:?} {body:?}"
        );
        let held = fabric.link(case, &pair.requester_id, &pair.link_id).await;
        assert_eq!(
            held["pendingFlowModules"], modules,
            "case {case:?}: expected the whole flow to be stored, got {:?}",
            held["pendingFlowModules"]
        );
    }
}
