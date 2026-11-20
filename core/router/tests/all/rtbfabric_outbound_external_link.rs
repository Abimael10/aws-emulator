// SPDX-License-Identifier: CC0-1.0

//! RTB Fabric's outbound-external-link surface on the ASSEMBLED emulator.
//!
//! RULE: the routes a service's model binds its operations to are claimed by that
//! service when every service of the emulator is mounted together, so a request a
//! client sends to `/requester-gateway/{gatewayId}/outbound-external-link[/{linkId}]`
//! reaches RTB Fabric's own handlers and is answered from RTB Fabric's state — not by
//! another service that happens to bind a similar path, and not by the router's own
//! `UnknownService`/`AmbiguousService`.
//!
//! That is a fact about the WHOLE service set rather than about one crate, so it is
//! checked here, through `aws_emulator_router::build_router` — the same assembly the
//! emulator binary serves. The table that spans the inputs the create and the read
//! generalise over lives with the owning crate's own suites.
//!
//! Assembling the router costs stack (see `boot_serve.rs`), so it is built — and the
//! requests are driven — on an explicitly sized thread.

use std::sync::Arc;

use aws_emulator_router::{build_router, InMemoryEngine};
use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// Keep this at or above `boot_serve.rs`'s stack: the same assembly runs here.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The Region every request here is signed for.
const REGION: &str = "us-west-2";

/// The `ConnectivityType` member an outbound external link carries, which is one of
/// the members this read can only take from RTB Fabric's own state.
const EGRESS: &str = "PUBLIC_EGRESS";

/// One restJson1 request to the assembled emulator, as `(status, parsed body)`.
async fn call(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let bytes = match body {
        Some(body) => Bytes::from(serde_json::to_vec(&body).expect("serialize the request body")),
        None => Bytes::new(),
    };
    let request = ::http::Request::builder()
        .method(method)
        .uri(format!("https://rtbfabric.{REGION}.amazonaws.com{path}"))
        .header("host", format!("rtbfabric.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/rtbfabric/aws4_request, \
                 SignedHeaders=host, Signature=not-verified"
            ),
        )
        .body(bytes)
        .expect("build the request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// A member of a JSON object as text, empty when it is absent or not a string.
fn text(body: &Value, member: &str) -> String {
    body.get(member)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

#[test]
fn the_outbound_external_link_surface_is_served_from_rtbfabric_state_on_the_assembled_emulator() {
    std::thread::Builder::new()
        .name("rtbfabric-outbound-external-link".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
            let mut router = build_router(engine);
            router.set_request_authentication(false);

            let runtime = tokio::runtime::Builder::new_current_thread()
                .build()
                .expect("a current-thread runtime");
            runtime.block_on(async {
                // The ids satisfy the modelled member constraints: `VpcId` is
                // `^vpc-[a-f0-9]{8,17}$`, `SubnetId` is `^subnet-\w{8,17}$` and
                // `SecurityGroupId` is `^sg-[0-9a-f]{8,40}$`.
                let gateway_body = |token: &str| {
                    json!({
                        "clientToken": token,
                        "vpcId": "vpc-0a1b2c3d4e5f6a7b8",
                        "subnetIds": ["subnet-0a1b2c3d4e5f6a7b8"],
                        "securityGroupIds": ["sg-0a1b2c3d4e5f6a7b8"],
                    })
                };
                let (status, gateway) = call(
                    &router,
                    "POST",
                    "/requester-gateway",
                    Some(gateway_body("token-requester")),
                )
                .await;
                assert_eq!(
                    status, 200,
                    "expected CreateRequesterGateway to be served, got {status} {gateway:?}"
                );
                let gateway_id = text(&gateway, "gatewayId");
                let (status, other) = call(
                    &router,
                    "POST",
                    "/requester-gateway",
                    Some(gateway_body("token-other")),
                )
                .await;
                assert_eq!(
                    status, 200,
                    "expected a second CreateRequesterGateway to be served, got {status} {other:?}"
                );
                let other_gateway_id = text(&other, "gatewayId");

                let public_endpoint = "https://bidder.example.com/openrtb";
                let (status, created) = call(
                    &router,
                    "POST",
                    &format!("/requester-gateway/{gateway_id}/outbound-external-link"),
                    Some(json!({
                        "clientToken": "token-link",
                        "publicEndpoint": public_endpoint,
                        "logSettings": {
                            "applicationLogs": {
                                "sampling": { "errorLog": 25.5, "filterLog": 4.5 }
                            }
                        },
                        "attributes": { "customerProvidedId": "assembled-exchange" },
                        "tags": { "team": "rtb" },
                    })),
                )
                .await;
                assert_eq!(
                    status, 200,
                    "expected CreateOutboundExternalLink to be claimed by rtbfabric and served, \
                     got {status} {created:?}"
                );
                let link_id = text(&created, "linkId");

                // The read is answered from what the create persisted in RTB
                // Fabric's store: these members travel in no part of this request.
                let path =
                    format!("/requester-gateway/{gateway_id}/outbound-external-link/{link_id}");
                let (status, read) = call(&router, "GET", &path, None).await;
                assert_eq!(
                    status, 200,
                    "expected GetOutboundExternalLink to be served, got {status} {read:?}"
                );
                assert_eq!(
                    (
                        text(&read, "gatewayId"),
                        text(&read, "linkId"),
                        text(&read, "publicEndpoint"),
                        text(&read, "connectivityType"),
                    ),
                    (
                        gateway_id.clone(),
                        link_id.clone(),
                        public_endpoint.to_string(),
                        EGRESS.to_string(),
                    ),
                    "expected the assembled emulator to report the stored link, got {read:?}"
                );
                assert_eq!(
                    read["tags"],
                    json!({ "team": "rtb" }),
                    "expected the stored tags, got {:?}",
                    read["tags"]
                );
                assert_eq!(
                    read["attributes"],
                    json!({ "customerProvidedId": "assembled-exchange" }),
                    "expected the stored attributes, got {:?}",
                    read["attributes"]
                );

                // And the same route under another gateway resolves to nothing,
                // which is RTB Fabric's own not-found rather than a route nobody
                // claimed.
                let (status, refused) = call(
                    &router,
                    "GET",
                    &format!(
                        "/requester-gateway/{other_gateway_id}/outbound-external-link/{link_id}"
                    ),
                    None,
                )
                .await;
                assert_eq!(
                    status, 404,
                    "expected the link to resolve only through the gateway that opened it, \
                     got {status} {refused:?}"
                );
                assert_eq!(
                    text(&refused, "__type"),
                    "ResourceNotFoundException",
                    "expected rtbfabric's own modelled not-found, got {refused:?}"
                );
            });
        })
        .expect("spawn the sized thread")
        .join()
        .expect("the sized thread runs to completion");
}
