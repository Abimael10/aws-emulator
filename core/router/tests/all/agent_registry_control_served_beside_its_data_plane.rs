// SPDX-License-Identifier: CC0-1.0

//! The Agent Registry CONTROL plane is reachable while the Agent Registry DATA plane is
//! mounted beside it, although the two share one SigV4 signing name.
//!
//! The rule under test: a SigV4 signing name is not a service key — several AWS services
//! may share one — so a request is dispatched to the service whose own model binds a ROUTE
//! (an HTTP method AND a URI) for it at the endpoint its resolved routing key names, and
//! mounting a peer that shares the signing name changes no answer. The two Agent Registry
//! models share the signing name `agent-registry` and the ARN namespace `agent-registry`,
//! are addressed at the endpoint prefixes `agent-registry-control` and `agent-registry`,
//! and bind no identical method + URI pattern.
//!
//! [`core/router/tests/all/agent_registry_signing_name_routing.rs`] pins which of the two
//! CLAIMS a request. These suites pin what a caller observes:
//!
//! * [`every_bound_agent_registry_route_answers_the_same_whether_its_peer_is_mounted`]
//!   compares, for every route either model binds, the answer from a router carrying only
//!   the owning plane with the answer from a router carrying both. The rows span the owning
//!   plane, the method (POST, GET, PATCH, DELETE), the path depth at which the two models'
//!   URIs diverge, the greedy `{resourceArn+}` label, a route whose first path segment both
//!   models publish, and whether the endpoint is named by the credential scope or — on an
//!   unsigned request — by the host.
//! * [`the_control_plane_serves_its_workflow_with_the_data_plane_mounted`] drives the
//!   control plane's own workflow — create a registry, read it back, create two records,
//!   submit one, list them under each status, delete both and then the registry — through a
//!   router carrying both planes, over three registry configurations and three record types,
//!   so the surface is exercised and not merely routed.
//!
//! No answer here may be one of the router's own cross-protocol refusals: `AmbiguousService`
//! (several claimants named an operation) or `UnknownService` (none claimed it). Neither
//! code appears in any AWS model, and both arrive in an `awsJson` envelope although both
//! planes speak `restJson1`.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// Percent-encode one URI path segment the way an SDK encodes a non-greedy `@httpLabel`, so
/// an identifier given in the ARN form the model allows (which carries `:` and `/`) arrives
/// escaped. This crate's one encoder, shared rather than copied.
use crate::cloudfront_key_value_store_handshake::segment as encode_label;

/// The region in the credential scope every signed row carries; a minted ARN must carry it.
const REGION: &str = "us-west-2";

/// A SigV4 `Authorization` header whose credential scope names the signing name both
/// services share. The signature is never checked: these routers run with request
/// authentication off.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDTESTAGENTREGISTRY/20260101/us-west-2/agent-registry/aws4_request, \
     SignedHeaders=host;x-amz-date, Signature=0123456789abcdef";

/// The host that names the control plane's endpoint, and the one that names the data
/// plane's. They are the two `endpointPrefix` values the models publish.
const CONTROL_HOST: &str = "agent-registry-control.us-west-2.amazonaws.com";
const DATA_HOST: &str = "agent-registry.us-west-2.amazonaws.com";

/// The router's own cross-protocol routing failures, which no request either model binds a
/// route for may be answered with.
const ROUTER_REFUSALS: [&str; 2] = ["AmbiguousService", "UnknownService"];

/// A registry id of `RegistryId`'s shape (`^[a-zA-Z0-9]{12,16}$`) and a record id of
/// `RegistryRecordId`'s (`^[a-zA-Z0-9]{12}$`), for the path labels the rows address. They
/// name nothing: a row asserts WHICH service answers, not what it found.
const REGISTRY_ID: &str = "aaaaaaaaaaaa";
const RECORD_ID: &str = "bbbbbbbbbbbb";

/// How long a wait for a transition the clock owes (a registry becoming `READY`, a record
/// becoming `DRAFT`) may take before a test gives up. Generous on purpose: the wait is on
/// the CONDITION, so a loaded host only polls more times.
const TRANSITION_DEADLINE: Duration = Duration::from_secs(30);

/// How long a poll loop pauses between two looks at a resource.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// Which plane owns a route, as a row states it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Plane {
    Control,
    Data,
}

type Control =
    service_com_amazonaws_agentregistrycontrol_agentregistrycontrol::EmulatedService<
        engine::InMemoryEngine,
    >;
type Data =
    service_com_amazonaws_agentregistry_agentregistry::EmulatedService<engine::InMemoryEngine>;

/// A router carrying only `plane`, over its own engine.
fn only(plane: Plane) -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    match plane {
        Plane::Control => {
            router.register_service(Control::new(engine));
        }
        Plane::Data => {
            router.register_service(Data::new(engine));
        }
    }
    router
}

/// A router carrying BOTH planes over one shared engine, exactly as `router/src/lib.rs`
/// mounts them.
fn both_planes() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(Control::new(Arc::clone(&engine)));
    router.register_service(Data::new(engine));
    router
}

/// One HTTP answer, reduced to what these tests assert on.
struct Reply {
    status: u16,
    /// The wire error code, read from `x-amzn-errortype` and from the body's `__type` (the
    /// router's own refusals set only the latter), with any shape-id prefix dropped. Empty
    /// on an answer that carries no error.
    code: String,
    body: Value,
}

impl Reply {
    fn str_field(&self, name: &str) -> &str {
        self.body[name].as_str().unwrap_or_default()
    }

    /// What a row reports for the identity of the answer: a service's own answers differ in
    /// state, so two routers are compared on the status and the error code alone.
    fn outcome(&self) -> (u16, &str) {
        (self.status, self.code.as_str())
    }
}

/// Dispatch one request. `signed` chooses whether it carries a credential scope at all: an
/// unsigned request's routing key comes from the host instead.
async fn call(
    router: &MultiServiceRouter,
    method: &str,
    host: &str,
    path: &str,
    body: Option<Value>,
    signed: bool,
) -> Reply {
    let payload = match &body {
        Some(value) => Bytes::from(serde_json::to_vec(value).expect("serializable body")),
        None => Bytes::new(),
    };
    let mut builder = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host)
        .header("content-type", "application/json");
    if signed {
        builder = builder.header("authorization", AUTHORIZATION);
    }
    let request = builder.body(payload).expect("a valid request");

    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let header_code = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let bytes = response.into_body();
    let body: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    let code = if header_code.is_empty() {
        body.get("__type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    } else {
        header_code
    };
    let code = code
        .rsplit(['#', '/'])
        .next()
        .unwrap_or_default()
        .to_string();
    Reply { status, code, body }
}

/// The id at the end of a minted ARN.
fn id_of(arn: &str) -> &str {
    arn.rsplit('/').next().unwrap_or(arn)
}

/// The ARN of a registry, built the way every emulated ARN is
/// (`engine::emulator_arn`), in the namespace BOTH models publish
/// (`arnNamespace: agent-registry`). It is both the greedy `{resourceArn+}` label the
/// tagging routes bind and the shape a minted registry ARN has.
fn registry_arn(registry_id: &str) -> String {
    ::engine::emulator_arn(ARN_NAMESPACE, REGION, &format!("registry/{registry_id}"))
}

/// The ARN namespace both models publish, which is also the signing name they share.
const ARN_NAMESPACE: &str = "agent-registry";

/// A `descriptors` value per `RecordType`, so each row of a record table carries a
/// descriptor of its own shape. Member names are `Descriptors`' own
/// (`smithy_query(query="shape", shape="Descriptors")`).
fn descriptors_for(record_type: &str) -> Value {
    match record_type {
        "MCP" => json!({
            "mcpServer": {
                "data": r#"{"name":"com.example/checkout","description":"Checkout tools","version":"1.4.2"}"#,
                "dataSchemaVersion": "2025-06-18"
            }
        }),
        "AGENT" => {
            json!({ "a2aAgentCard": { "data": "{\"name\":\"checkout\"}", "dataSchemaVersion": "0.2.1" } })
        }
        _ => json!({ "custom": { "data": "{\"anything\":true}" } }),
    }
}

/// One row of the route table: who owns the route, and the request that addresses it.
struct Route {
    case: &'static str,
    owner: Plane,
    method: &'static str,
    host: &'static str,
    path: String,
    body: Option<Value>,
    signed: bool,
}

/// Every route either model binds, as each crate's `OPERATIONS.md` lists it from its
/// model's `smithy.api#http` bindings. A request that names a resource names one that does
/// not exist: the row asserts WHICH service answers, which is settled before any handler
/// looks at state.
fn bound_routes() -> Vec<Route> {
    let signed_control = |case: &'static str,
                          method: &'static str,
                          path: String,
                          body: Option<Value>| Route {
        case,
        owner: Plane::Control,
        method,
        host: CONTROL_HOST,
        path,
        body,
        signed: true,
    };
    let signed_data =
        |case: &'static str, method: &'static str, path: String, body: Option<Value>| Route {
            case,
            owner: Plane::Data,
            method,
            host: DATA_HOST,
            path,
            body,
            signed: true,
        };
    let tags_path = format!("/tags/{}", registry_arn(REGISTRY_ID));
    vec![
        // The control plane's routes: the shallow collection path, the labelled resource
        // path under three methods, the record paths one and two segments deeper, the
        // `-list` POST routes, and the greedy `{resourceArn+}` label under three methods.
        signed_control(
            "CreateRegistry",
            "POST",
            "/registries".to_string(),
            Some(json!({ "name": "peer-mounted-probe" })),
        ),
        signed_control(
            "ListRegistries",
            "POST",
            "/registries-list".to_string(),
            Some(json!({})),
        ),
        signed_control(
            "GetRegistry",
            "GET",
            format!("/registries/{REGISTRY_ID}"),
            None,
        ),
        signed_control(
            "UpdateRegistry",
            "PATCH",
            format!("/registries/{REGISTRY_ID}"),
            Some(json!({ "description": "probe" })),
        ),
        signed_control(
            "DeleteRegistry",
            "DELETE",
            format!("/registries/{REGISTRY_ID}"),
            None,
        ),
        signed_control(
            "CreateRegistryRecord",
            "POST",
            format!("/registries/{REGISTRY_ID}/records"),
            Some(json!({
                "name": "probe-record",
                "recordType": "CUSTOM",
                "descriptors": descriptors_for("CUSTOM"),
            })),
        ),
        signed_control(
            "ListRegistryRecords",
            "POST",
            format!("/registries/{REGISTRY_ID}/records-list"),
            Some(json!({})),
        ),
        signed_control(
            "GetRegistryRecord",
            "GET",
            format!("/registries/{REGISTRY_ID}/records/{RECORD_ID}"),
            None,
        ),
        signed_control(
            "UpdateRegistryRecord",
            "PATCH",
            format!("/registries/{REGISTRY_ID}/records/{RECORD_ID}"),
            Some(json!({ "description": "probe" })),
        ),
        signed_control(
            "DeleteRegistryRecord",
            "DELETE",
            format!("/registries/{REGISTRY_ID}/records/{RECORD_ID}"),
            None,
        ),
        signed_control(
            "SubmitRegistryRecordForApproval",
            "POST",
            format!("/registries/{REGISTRY_ID}/records/{RECORD_ID}/submit-for-approval"),
            None,
        ),
        signed_control(
            "UpdateRegistryRecordStatus",
            "PATCH",
            format!("/registries/{REGISTRY_ID}/records/{RECORD_ID}/status"),
            Some(json!({ "status": "APPROVED", "statusReason": "probe" })),
        ),
        signed_control(
            "TagResource",
            "POST",
            tags_path.clone(),
            Some(json!({ "tags": { "team": "agents" } })),
        ),
        signed_control("ListTagsForResource", "GET", tags_path.clone(), None),
        signed_control(
            "UntagResource",
            "DELETE",
            format!("{tags_path}?tagKeys=team"),
            None,
        ),
        // The data plane's routes, including the one whose FIRST path segment the control
        // plane publishes too — the two models diverge only at the third.
        signed_data(
            "SearchDiscoverableRegistryRecords",
            "POST",
            "/discoverable-records-search".to_string(),
            Some(json!({
                "searchQuery": "checkout",
                "registryIds": [REGISTRY_ID],
                "maxResults": 10,
            })),
        ),
        signed_data(
            "BatchGetDiscoverableRegistryRecord",
            "POST",
            "/discoverable-records-batch".to_string(),
            Some(json!({ "entries": [{ "registryId": REGISTRY_ID, "recordIds": [RECORD_ID] }] })),
        ),
        signed_data(
            "ListDiscoverableRegistryRecords",
            "POST",
            format!("/registries/{REGISTRY_ID}/discoverable-records-list"),
            Some(json!({})),
        ),
        // The same two path spaces addressed UNSIGNED, where the routing key is the host
        // label (each plane's `endpointPrefix`) rather than the credential scope.
        Route {
            case: "GetRegistry, unsigned, on the control host",
            owner: Plane::Control,
            method: "GET",
            host: CONTROL_HOST,
            path: format!("/registries/{REGISTRY_ID}"),
            body: None,
            signed: false,
        },
        Route {
            case: "ListDiscoverableRegistryRecords, unsigned, on the data host",
            owner: Plane::Data,
            method: "POST",
            host: DATA_HOST,
            path: format!("/registries/{REGISTRY_ID}/discoverable-records-list"),
            body: Some(json!({})),
            signed: false,
        },
    ]
}

#[tokio::test]
async fn every_bound_agent_registry_route_answers_the_same_whether_its_peer_is_mounted() {
    for route in bound_routes() {
        let Route {
            case,
            owner,
            method,
            host,
            path,
            body,
            signed,
        } = route;
        // A fresh router per row on each side, so the two answers are compared over the
        // same state and no row depends on an earlier one.
        let alone = call(&only(owner), method, host, &path, body.clone(), signed).await;
        let beside_its_peer = call(&both_planes(), method, host, &path, body, signed).await;

        for refusal in ROUTER_REFUSALS {
            assert_ne!(
                beside_its_peer.code, refusal,
                "case {case:?} ({method} {path} on {host}): a route {owner:?} binds must be \
                 served by it, got the router's own {refusal} at {} with {:?}",
                beside_its_peer.status, beside_its_peer.body
            );
        }
        // Status 500 exactly, which is the status the router's ambiguity refusal carries;
        // the 501 a scaffolded operation of this service answers with (`UpdateRegistry` and
        // `ListRegistries` still carry their scaffold headers, each owned by its own stub
        // issue) is this service ANSWERING, which is what these rows assert.
        assert_ne!(
            beside_its_peer.status, 500,
            "case {case:?} ({method} {path} on {host}): no bound route answers 500, the \
             status the router's ambiguity refusal carries, got {} {:?} with {:?}",
            beside_its_peer.status, beside_its_peer.code, beside_its_peer.body
        );
        assert_eq!(
            beside_its_peer.outcome(),
            alone.outcome(),
            "case {case:?} ({method} {path} on {host}): mounting the peer plane that shares \
             the signing name must change no answer; {owner:?} alone answered {:?}, with the \
             peer mounted {:?} carrying {:?}",
            alone.outcome(),
            beside_its_peer.outcome(),
            beside_its_peer.body
        );
    }
}

/// Create a registry with `body` through the both-planes router and wait for the creation
/// the clock owes to come due, so it is `READY`. The wait is on the CONDITION, never on a
/// fixed span. Returns the registry's id.
async fn ready_registry(router: &MultiServiceRouter, case: &str, body: Value) -> String {
    let created = call(router, "POST", CONTROL_HOST, "/registries", Some(body), true).await;
    assert_eq!(
        created.status,
        202,
        "case {case:?}: CreateRegistry answers 202 with the control plane's own output, got \
         {} {:?} with {:?}",
        created.status,
        created.code,
        created.body
    );
    let registry_arn = created.str_field("registryArn").to_string();
    let registry_id = id_of(&registry_arn).to_string();
    assert_eq!(
        registry_arn,
        self::registry_arn(&registry_id),
        "case {case:?}: CreateRegistry mints a registryArn in the namespace and region its \
         model publishes, got {registry_arn:?}"
    );

    let started = std::time::Instant::now();
    loop {
        let got = call(
            router,
            "GET",
            CONTROL_HOST,
            &format!("/registries/{}", encode_label(&registry_id)),
            None,
            true,
        )
        .await;
        if got.str_field("status") == "READY" {
            assert_eq!(
                got.str_field("registryArn"),
                registry_arn,
                "case {case:?}: GetRegistry reports the ARN CreateRegistry minted, got {:?}",
                got.body
            );
            return registry_id;
        }
        assert!(
            started.elapsed() < TRANSITION_DEADLINE,
            "case {case:?}: a created registry becomes READY, last answer {} {:?} with {:?}",
            got.status,
            got.code,
            got.body
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Look at a record until it reports `wanted`, or give up at the deadline. The wait is on
/// the CONDITION — a transition the clock owes, or a status another call wrote.
async fn record_reaching(
    router: &MultiServiceRouter,
    case: &str,
    registry_id: &str,
    record_id: &str,
    wanted: &str,
) -> Reply {
    let started = std::time::Instant::now();
    loop {
        let got = call(
            router,
            "GET",
            CONTROL_HOST,
            &format!(
                "/registries/{}/records/{}",
                encode_label(registry_id),
                encode_label(record_id)
            ),
            None,
            true,
        )
        .await;
        if got.str_field("status") == wanted {
            return got;
        }
        assert!(
            started.elapsed() < TRANSITION_DEADLINE,
            "case {case:?}: the record never reached {wanted}, last answer {} {:?} with {:?}",
            got.status,
            got.code,
            got.body
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Create a record of `record_type` named `name` in `registry_id`, wait for the creation
/// the clock owes to come due so it is `DRAFT`, and check the read reports what the create
/// persisted. Returns the record's id.
async fn draft_record(
    router: &MultiServiceRouter,
    case: &str,
    registry_id: &str,
    name: &str,
    record_type: &str,
) -> String {
    let created = call(
        router,
        "POST",
        CONTROL_HOST,
        &format!("/registries/{}/records", encode_label(registry_id)),
        Some(json!({
            "name": name,
            "recordType": record_type,
            "descriptors": descriptors_for(record_type),
        })),
        true,
    )
    .await;
    assert_eq!(
        created.status, 202,
        "case {case:?}: CreateRegistryRecord answers 202 for {name:?}, got {} {:?} with {:?}",
        created.status, created.code, created.body
    );
    let record_arn = created.str_field("recordArn").to_string();
    let record_id = id_of(&record_arn).to_string();
    assert_eq!(
        record_arn,
        format!("{}/record/{record_id}", registry_arn(registry_id)),
        "case {case:?}: a record's ARN is minted under its registry's, got {record_arn:?}"
    );

    let draft = record_reaching(router, case, registry_id, &record_id, "DRAFT").await;
    assert_eq!(
        (draft.str_field("name"), draft.str_field("recordType")),
        (name, record_type),
        "case {case:?}: GetRegistryRecord reports the name and type the create persisted, \
         got {:?}",
        draft.body
    );
    record_id
}

/// The ids `ListRegistryRecords` reports for `registry_id` under the status filter `status`,
/// in the order the answer carries them.
async fn records_with_status(
    router: &MultiServiceRouter,
    case: &str,
    registry_id: &str,
    status: &str,
) -> Vec<String> {
    let listed = call(
        router,
        "POST",
        CONTROL_HOST,
        &format!("/registries/{}/records-list", encode_label(registry_id)),
        // `status` is a `RegistryRecordFilter` name this operation's input declares
        // (`smithy_query(query="shape", shape="RegistryRecordFilter")`).
        Some(json!({ "filters": [{ "name": "status", "values": [status] }] })),
        true,
    )
    .await;
    assert_eq!(
        listed.status, 200,
        "case {case:?}: ListRegistryRecords answers 200 under the {status} filter, got {} \
         {:?} with {:?}",
        listed.status, listed.code, listed.body
    );
    listed.body["registryRecords"]
        .as_array()
        .map(|records| {
            records
                .iter()
                .filter_map(|record| record["recordId"].as_str())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

#[tokio::test]
async fn the_control_plane_serves_its_workflow_with_the_data_plane_mounted() {
    // The rows differ along the registry's approval configuration and the record's type, so
    // the workflow is driven over more than one shape of input. The status a submission
    // lands in is the registry's own rule: `APPROVE_ALL` approves on submission, anything
    // else waits for manual review (`smithy_query(query="shape",
    // shape="ApprovalConfiguration,AutoApprovalRule")`).
    let cases: Vec<(&str, Value, &str, &str)> = vec![
        (
            "manual review",
            json!({ "name": "control-plane-workflow" }),
            "CUSTOM",
            "PENDING_APPROVAL",
        ),
        (
            "an empty auto-approval rule list",
            json!({
                "name": "control-plane-workflow-empty-rules",
                "approvalConfiguration": { "autoApprovalRules": [] }
            }),
            "AGENT",
            "PENDING_APPROVAL",
        ),
        (
            "auto-approved on submission",
            json!({
                "name": "control-plane-workflow-auto",
                "approvalConfiguration": { "autoApprovalRules": ["APPROVE_ALL"] }
            }),
            "MCP",
            "APPROVED",
        ),
    ];

    for (case, registry_body, record_type, submitted_status) in cases {
        // One router carrying BOTH planes: every call below resolves the routing key
        // `agent-registry`, which both services see.
        let router = both_planes();
        let registry_id = ready_registry(&router, case, registry_body).await;

        // Two records in the same registry: one is submitted below, the other is left in
        // `DRAFT`, so the status filter the listing is driven with has to separate them and
        // cannot pass against a handler that ignores `filters`.
        let record_id =
            draft_record(&router, case, &registry_id, "checkout-tools", record_type).await;
        let still_draft =
            draft_record(&router, case, &registry_id, "left-in-draft", "CUSTOM").await;

        let submitted = call(
            &router,
            "POST",
            CONTROL_HOST,
            &format!(
                "/registries/{}/records/{}/submit-for-approval",
                encode_label(&registry_id),
                encode_label(&record_id)
            ),
            None,
            true,
        )
        .await;
        assert_eq!(
            (submitted.status, submitted.str_field("status")),
            (202, submitted_status),
            "case {case:?}: SubmitRegistryRecordForApproval answers 202 with the status the \
             registry's approval configuration gives, got {} {:?}",
            submitted.status,
            submitted.body
        );

        // The listing under the submitted record's status reports it and excludes the one
        // left in `DRAFT`; the listing under `DRAFT` reports the other one and excludes the
        // submitted one. Either way the filter decides what comes back.
        assert_eq!(
            records_with_status(&router, case, &registry_id, submitted_status).await,
            vec![record_id.clone()],
            "case {case:?}: ListRegistryRecords under the {submitted_status} filter reports \
             the submitted record alone, with {still_draft:?} left in DRAFT"
        );
        assert_eq!(
            records_with_status(&router, case, &registry_id, "DRAFT").await,
            vec![still_draft.clone()],
            "case {case:?}: ListRegistryRecords under the DRAFT filter reports the record \
             still in DRAFT alone, with {record_id:?} at {submitted_status}"
        );

        // A registry that still holds a record cannot be deleted, so the records go first:
        // the workflow's own precondition, driven here through the shared-name router.
        for record in [&record_id, &still_draft] {
            let deleted_record = call(
                &router,
                "DELETE",
                CONTROL_HOST,
                &format!(
                    "/registries/{}/records/{}",
                    encode_label(&registry_id),
                    encode_label(record)
                ),
                None,
                true,
            )
            .await;
            assert_eq!(
                deleted_record.status, 200,
                "case {case:?}: DeleteRegistryRecord answers 200 for {record:?}, got {} {:?} \
                 with {:?}",
                deleted_record.status, deleted_record.code, deleted_record.body
            );
        }

        let deleted_registry = call(
            &router,
            "DELETE",
            CONTROL_HOST,
            &format!("/registries/{}", encode_label(&registry_id)),
            None,
            true,
        )
        .await;
        assert_eq!(
            (deleted_registry.status, deleted_registry.str_field("status")),
            (202, "DELETING"),
            "case {case:?}: DeleteRegistry answers 202 and reports the status the deletion \
             entered, got {} {:?}",
            deleted_registry.status,
            deleted_registry.body
        );
    }
}
