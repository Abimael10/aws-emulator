// SPDX-License-Identifier: CC0-1.0

//! The address a workspace's Grafana console and HTTP API are announced at, across EVERY
//! projection of the workspace that carries it.
//!
//! The rule under test: `WorkspaceDescription$endpoint` and `WorkspaceSummary$endpoint` are
//! a BARE HOST — no scheme, no port, no path — whose first DNS label is the workspace's own
//! id and whose remaining labels are `grafana-workspace.<region>.amazonaws.com` for the
//! region the workspace lives in. Every operation that reports a workspace reports that one
//! value for it, so a client that read the endpoint from any one of them holds the same host
//! (`workspace_grafana_api` covers what answers there), and two workspaces never share one.
//!
//! The rows differ along the projection the endpoint was read from: `CreateWorkspace`,
//! `DescribeWorkspace`, `UpdateWorkspace`, the `ListWorkspaces` summary and
//! `DeleteWorkspace`; along the workspace, of which there are two, so a projection that
//! reported a constant or another record's address fails here; and along the region the
//! create was signed for, which is the one part of the host beside the id that varies.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_grafana_awsgrafanacontrolplane::state::LIFECYCLE_TRANSITION;
use service_com_amazonaws_grafana_awsgrafanacontrolplane::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The endpoint every request below reaches this emulator on. It is deliberately NOT the
/// host a workspace announces: the announced endpoint is composed from the workspace and
/// its region, not from the address the asking client happened to use.
const EMULATOR_HOST: &str = "127.0.0.1:9297";

/// How long a case waits for a status the clock owes a workspace before giving up. Far
/// above the transition span, so a loaded host still reaches it; nothing asserts on how
/// long it actually took.
const STATUS_DEADLINE: Duration = LIFECYCLE_TRANSITION.saturating_mul(20);

/// How often such a wait looks again.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// What a call answered: the HTTP status and the JSON body.
struct Answer {
    status: u16,
    body: Value,
}

fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(engine));
    router
}

/// A SigV4 credential scope naming this service in `region`: it is what routes a
/// control-plane request here, and it is where the region a workspace lives in comes from.
fn signed_for(region: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{region}/grafana/aws4_request, \
         SignedHeaders=host, Signature=abc"
    )
}

/// The endpoint host a workspace `id` in `region` announces, spelled out here independently
/// of the crate's own constants.
fn expected_endpoint(id: &str, region: &str) -> String {
    format!("{id}.grafana-workspace.{region}.amazonaws.com")
}

/// One control-plane call in `region`, reaching the emulator at [`EMULATOR_HOST`].
async fn call(
    router: &MultiServiceRouter,
    region: &str,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> Answer {
    let payload = body.map(|value| value.to_string()).unwrap_or_default();
    let mut builder = ::http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", EMULATOR_HOST)
        .header("authorization", signed_for(region));
    if !payload.is_empty() {
        builder = builder.header("content-type", "application/json");
    }
    let request = builder
        .body(Bytes::from(payload))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    Answer { status, body }
}

/// Create one workspace named `name` in `region`: its id, and the endpoint the create
/// announced.
async fn create_workspace(
    router: &MultiServiceRouter,
    region: &str,
    name: &str,
) -> (String, String) {
    let created = call(
        router,
        region,
        "POST",
        "/workspaces",
        Some(json!({
            "accountAccessType": "CURRENT_ACCOUNT",
            "permissionType": "SERVICE_MANAGED",
            "authenticationProviders": ["SAML"],
            "workspaceName": name,
            "workspaceRoleArn": "arn:aws:iam::000000000000:role/grafana-workspace",
        })),
    )
    .await;
    assert_eq!(
        created.status, 202,
        "case {name:?}: the workspace must be created, got {} with {:?}",
        created.status, created.body
    );
    let id = created.body["workspace"]["id"]
        .as_str()
        .expect("CreateWorkspace answers an id")
        .to_string();
    let endpoint = created.body["workspace"]["endpoint"]
        .as_str()
        .expect("CreateWorkspace answers an endpoint")
        .to_string();
    (id, endpoint)
}

/// Look again until the workspace `id` is ACTIVE — the status the clock owes a new
/// workspace — or the deadline passes. Nothing waits for a fixed time and nothing asserts
/// on elapsed time.
async fn await_active(router: &MultiServiceRouter, region: &str, id: &str) -> Answer {
    let deadline = Instant::now() + STATUS_DEADLINE;
    loop {
        let described = call(router, region, "GET", &format!("/workspaces/{id}"), None).await;
        if described.body["workspace"]["status"] == json!("ACTIVE") {
            return described;
        }
        assert!(
            Instant::now() < deadline,
            "workspace {id} never reached ACTIVE, last read {} with {:?}",
            described.status,
            described.body
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

#[tokio::test]
async fn every_projection_reports_the_one_endpoint_host_the_workspace_is_announced_at() {
    // The regions the creates are signed for: the region label is the one part of the
    // endpoint host beside the workspace id that varies.
    for region in ["eu-west-1", "us-west-2", "ap-southeast-3"] {
        let router = emulator();

        // Two workspaces, so a projection reporting a constant or the other record's
        // address cannot pass.
        let (first_id, first_created_endpoint) =
            create_workspace(&router, region, "monitoring").await;
        let (second_id, second_created_endpoint) =
            create_workspace(&router, region, "reporting").await;
        assert_ne!(
            first_created_endpoint, second_created_endpoint,
            "region {region:?}: each workspace must announce its own endpoint, both got \
             {first_created_endpoint:?}"
        );

        for (case, id, created_endpoint) in [
            ("the first workspace", &first_id, &first_created_endpoint),
            ("the second workspace", &second_id, &second_created_endpoint),
        ] {
            let want = expected_endpoint(id, region);
            assert_eq!(
                created_endpoint, &want,
                "case {case:?} in {region:?}: CreateWorkspace must announce the workspace's \
                 own endpoint host, expected {want:?}, got {created_endpoint:?}"
            );
            // A host and nothing else: a client that splices it into an address of its own
            // would otherwise build a double-scheme, double-path URL.
            for forbidden in ["://", ":", "/", "?"] {
                assert!(
                    !created_endpoint.contains(forbidden),
                    "case {case:?} in {region:?}: an endpoint is a bare host, so it carries \
                     no {forbidden:?}, got {created_endpoint:?}"
                );
            }
            assert_eq!(
                created_endpoint.split('.').next().unwrap_or_default(),
                id.as_str(),
                "case {case:?} in {region:?}: the first DNS label of the endpoint is the \
                 workspace id, got {created_endpoint:?}"
            );

            // An update is accepted only once the workspace is up, so wait for the status
            // the clock owes it rather than for a span of time.
            let described = await_active(&router, region, id).await;
            assert_eq!(
                described.status, 200,
                "case {case:?} in {region:?}: DescribeWorkspace must answer 200, got {} with \
                 {:?}",
                described.status, described.body
            );

            let updated = call(
                &router,
                region,
                "PUT",
                &format!("/workspaces/{id}"),
                Some(json!({"workspaceDescription": format!("{case} updated")})),
            )
            .await;
            assert_eq!(
                updated.status, 202,
                "case {case:?} in {region:?}: UpdateWorkspace must answer 202, got {} with {:?}",
                updated.status, updated.body
            );

            let listed = call(&router, region, "GET", "/workspaces", None).await;
            assert_eq!(
                listed.status, 200,
                "case {case:?} in {region:?}: ListWorkspaces must answer 200, got {} with {:?}",
                listed.status, listed.body
            );
            let summary = listed.body["workspaces"]
                .as_array()
                .and_then(|workspaces| {
                    workspaces
                        .iter()
                        .find(|workspace| workspace["id"] == json!(id))
                        .cloned()
                })
                .unwrap_or(Value::Null);

            let deleted = call(&router, region, "DELETE", &format!("/workspaces/{id}"), None).await;
            assert_eq!(
                deleted.status, 202,
                "case {case:?} in {region:?}: DeleteWorkspace must answer 202, got {} with {:?}",
                deleted.status, deleted.body
            );

            // One address, reported by every projection that carries it.
            for (projection, reported) in [
                ("DescribeWorkspace", &described.body["workspace"]["endpoint"]),
                ("UpdateWorkspace", &updated.body["workspace"]["endpoint"]),
                ("the ListWorkspaces summary", &summary["endpoint"]),
                ("DeleteWorkspace", &deleted.body["workspace"]["endpoint"]),
            ] {
                assert_eq!(
                    *reported,
                    json!(want),
                    "case {case:?} in {region:?} read from {projection}: expected the endpoint \
                     {want:?}, got {reported:?}"
                );
            }
        }
    }
}
