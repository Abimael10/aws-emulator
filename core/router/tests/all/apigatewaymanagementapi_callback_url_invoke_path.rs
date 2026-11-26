// SPDX-License-Identifier: CC0-1.0

//! The WHOLE invoke path of a `@connections` callback URL, driven through the assembled router.
//!
//! > **RULE: an `execute-api` `@connections` request is served only when the endpoint it was
//! > addressed through — the host together with the WHOLE path in front of the modelled
//! > `/@connections/{ConnectionId}` binding — resolves to a stage some api has DEPLOYED.** That
//! > binding is served on the complete path to a deployed stage, so on an api's own invoke host
//! > the path IS the stage name ("the stage name is typically appended to this URI to form a
//! > complete path to a deployed API stage", the `Api$ApiEndpoint` doc of the API Gateway V2
//! > model) and on a custom domain name it IS a base path that domain maps to a stage — the
//! > root base path of a mapping carrying no key included. An addressing carrying no path at
//! > all names no stage, and one carrying anything BEYOND what names the stage addresses a path
//! > under that stage rather than the stage's own endpoint: neither resolves a stage, and every
//! > addressing that resolves none is refused `403` `ForbiddenException` with the body
//! > `{"message": "Forbidden"}` BEFORE the request is dispatched — before the `ConnectionId`'s
//! > own form is read and before the connection registry is consulted.
//!
//! WHY the order and the status matter: `403` means "you may not invoke this endpoint" and is
//! the only answer that names a misconfigured callback URL. A client told `400`
//! `BadRequestException` ("Invalid connectionId: …") instead blames its own id computation and
//! re-mints ids that were fine, and one told `410` `GoneException` prunes live subscriber rows.
//! Serving such an addressing at all is the other half of the same defect: the per-stage
//! boundary is then not observable, because a path the endpoint does not serve reaches the
//! registry of a stage it never named.
//!
//! Two services, one engine, one router, so this is a router suite: the apis, the stages, the
//! deployment released to each stage and the base paths a custom domain maps are API Gateway
//! V2's records (signing name `apigateway`), while the refusal and the registry answers are the
//! API Gateway Management API's (signing name `execute-api`).
//!
//! The rows span what the rule generalises over, host form against path form: the api's own
//! invoke host, the regional `execute-api` address a client on this machine reaches, a custom
//! domain mapping a one-segment base path, one mapping a two-segment base path and one mapping
//! its root — each with the path that names the stage, with a path one segment under it, with a
//! nested path under it, and with no path at all. The sibling suite
//! `apigatewaymanagementapi_callback_url_forms` pins the host forms and the stageless
//! addressing; this one pins the whole path.
//!
//! Assembling the router costs stack, so it runs on an explicitly sized thread for the same
//! reason `boot_serve.rs` does.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use aws_emulator_router::{build_router, InMemoryEngine};
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_apigatewaymanagementapi_apigatewaymanagementapi as mgmt;

/// Keep this at or above the size `boot_serve.rs` uses.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The region the api and the domain names below are created and addressed in.
const REGION: &str = "us-east-1";

/// A SigV4 credential scope naming the API Gateway control plane's signing name, which the V2
/// endpoint shares.
const CONTROL_PLANE_AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260101/us-east-1/apigateway/aws4_request, \
     SignedHeaders=host, Signature=0123456789abcdef";

/// A SigV4 credential scope naming the `execute-api` signing name an `@connections` caller
/// signs with.
const EXECUTE_API_AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260101/us-east-1/execute-api/aws4_request, \
     SignedHeaders=host, Signature=0123456789abcdef";

/// The stage the api deploys, and so the one segment its own invoke endpoint carries.
const STAGE: &str = "prod";

/// A stage name the api never creates.
const UNDEPLOYED_STAGE: &str = "staging";

/// A custom domain name mapping ONE base path to the deployed stage, and that base path.
const MAPPED_DOMAIN: &str = "sockets.invoke-path.example.com";
const MAPPED_BASE_PATH: &str = "socket";

/// A custom domain name whose mapping carries NO key, so the stage it names is mapped on the
/// domain's ROOT — the base path that is the empty path.
const ROOT_MAPPED_DOMAIN: &str = "root.invoke-path.example.com";

/// A custom domain name mapping a base path of TWO segments — "the API mapping key" is a path,
/// not one segment — and that base path.
const MULTI_MAPPED_DOMAIN: &str = "deep.invoke-path.example.com";
const MULTI_BASE_PATH: &str = "v1/socket";

/// The identity the registered connection carries.
const SOURCE_IP: &str = "198.51.100.41";
const USER_AGENT: &str = "fan-out/2.0";

/// A `ConnectionId` of the shape API Gateway ISSUES that no connection owns: the id form the
/// three operations answer `GoneException` for on a properly addressed, deployed stage. Written
/// as a client writes it in a path segment, with the base64 padding percent-encoded.
const UNKNOWN_CONNECTION_ID: &str = "Fabc1cWIPHcCJ7A%3D";

/// A value API Gateway's own encoding could never have produced, so it names no connection
/// anywhere: the id form a deployed stage refuses `400 BadRequestException` for.
const MALFORMED_CONNECTION_ID: &str = "not-a-connection-id";

/// The sentence API Gateway's `execute-api` endpoint refuses an addressing it does not serve
/// with — fixed text, naming neither the api nor the stage.
const FORBIDDEN_MESSAGE: &str = "Forbidden";

/// The three operations the model binds under `/@connections/{ConnectionId}`, by the method
/// each is reached with and the payload it carries.
const OPERATIONS: [(&str, &[u8]); 3] = [("GET", b""), ("POST", b"fan-out"), ("DELETE", b"")];

/// One callback-URL addressing: the host its URL names, the whole path in front of the modelled
/// binding, and whether that endpoint serves the call.
///
/// The host is built from the api id the emulator minted, since that is what the api's own
/// invoke host carries; the path is built from the stage and base-path constants above.
struct Form {
    case: &'static str,
    host: fn(&str) -> String,
    prefix: fn() -> String,
    served: bool,
}

/// The addressings, spanning the host form against the whole invoke path.
const FORMS: &[Form] = &[
    Form {
        case: "the documented callback URL: the api's own invoke host and its deployed stage",
        host: |api_id| format!("{api_id}.execute-api.{REGION}.amazonaws.com"),
        prefix: || format!("/{STAGE}"),
        served: true,
    },
    Form {
        case: "the api's own invoke host, one segment UNDER the deployed stage",
        host: |api_id| format!("{api_id}.execute-api.{REGION}.amazonaws.com"),
        prefix: || format!("/{STAGE}/{MAPPED_BASE_PATH}"),
        served: false,
    },
    Form {
        case: "the api's own invoke host, a nested path under the deployed stage",
        host: |api_id| format!("{api_id}.execute-api.{REGION}.amazonaws.com"),
        prefix: || format!("/{STAGE}/v1/{MAPPED_BASE_PATH}"),
        served: false,
    },
    Form {
        case: "the api's own invoke host and a stage nothing deployed",
        host: |api_id| format!("{api_id}.execute-api.{REGION}.amazonaws.com"),
        prefix: || format!("/{UNDEPLOYED_STAGE}"),
        served: false,
    },
    Form {
        case: "the regional execute-api address a client on this machine reaches, carrying the \
               deployed stage",
        host: |_| format!("execute-api.{REGION}.amazonaws.com"),
        prefix: || format!("/{STAGE}"),
        served: true,
    },
    Form {
        case: "the regional execute-api address, one segment under the deployed stage",
        host: |_| format!("execute-api.{REGION}.amazonaws.com"),
        prefix: || format!("/{STAGE}/{MAPPED_BASE_PATH}"),
        served: false,
    },
    Form {
        case: "a custom domain on the base path it maps to the deployed stage",
        host: |_| MAPPED_DOMAIN.to_string(),
        prefix: || format!("/{MAPPED_BASE_PATH}"),
        served: true,
    },
    Form {
        case: "that custom domain, one segment under the base path it maps",
        host: |_| MAPPED_DOMAIN.to_string(),
        prefix: || format!("/{MAPPED_BASE_PATH}/{STAGE}"),
        served: false,
    },
    Form {
        case: "that custom domain on the deployed stage's own name, which it maps nothing on",
        host: |_| MAPPED_DOMAIN.to_string(),
        prefix: || format!("/{STAGE}"),
        served: false,
    },
    Form {
        case: "that custom domain on its root, which it maps nothing on",
        host: |_| MAPPED_DOMAIN.to_string(),
        prefix: String::new,
        served: false,
    },
    Form {
        case: "a custom domain whose mapping carries no key, addressed on the root it maps",
        host: |_| ROOT_MAPPED_DOMAIN.to_string(),
        prefix: String::new,
        served: true,
    },
    Form {
        case: "that root-mapped custom domain, one segment under the root it maps",
        host: |_| ROOT_MAPPED_DOMAIN.to_string(),
        prefix: || format!("/{STAGE}"),
        served: false,
    },
    Form {
        case: "a custom domain on the two-segment base path it maps to the deployed stage",
        host: |_| MULTI_MAPPED_DOMAIN.to_string(),
        prefix: || format!("/{MULTI_BASE_PATH}"),
        served: true,
    },
    Form {
        case: "that domain on the first segment of the base path it maps, which maps nothing",
        host: |_| MULTI_MAPPED_DOMAIN.to_string(),
        prefix: || format!("/{}", MULTI_BASE_PATH.split('/').next().unwrap_or_default()),
        served: false,
    },
    Form {
        case: "that domain, one segment under the two-segment base path it maps",
        host: |_| MULTI_MAPPED_DOMAIN.to_string(),
        prefix: || format!("/{MULTI_BASE_PATH}/{STAGE}"),
        served: false,
    },
];

/// The instant the connection below was established.
fn connected_at() -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000)
}

/// One answer off the wire: the status, the error SHAPE a restJson1 client resolves and the
/// body text.
struct Answer {
    status: u16,
    error_type: String,
    body: String,
}

impl Answer {
    /// The answer as one line, for a failing assertion to name.
    fn rendered(&self) -> String {
        format!("{} {} {}", self.status, self.error_type, self.body)
    }

    /// The `message` member of the body, or `"<no message>"` when it carries none.
    fn message(&self) -> String {
        serde_json::from_str::<Value>(&self.body)
            .ok()
            .as_ref()
            .and_then(|document| document.get("message"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| "<no message>".to_string())
    }
}

/// Dispatch one request through the assembled router.
async fn call(
    router: &MultiServiceRouter,
    authorization: &str,
    host: &str,
    method: &str,
    path: &str,
    body: Vec<u8>,
) -> Answer {
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host)
        .header("content-type", "application/json")
        .header("authorization", authorization)
        .body(bytes::Bytes::from(body))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    Answer {
        status: response.status().as_u16(),
        error_type: response
            .headers()
            .get("x-amzn-errortype")
            .map(|value| String::from_utf8_lossy(value.as_bytes()).to_string())
            .unwrap_or_else(|| "<absent>".to_string()),
        body: String::from_utf8_lossy(response.body().as_ref()).to_string(),
    }
}

/// One `@connections` call at `host` through `prefix`.
async fn connections_call(
    router: &MultiServiceRouter,
    host: &str,
    prefix: &str,
    method: &str,
    connection_id: &str,
    payload: &[u8],
) -> Answer {
    call(
        router,
        EXECUTE_API_AUTHORIZATION,
        host,
        method,
        &format!("{prefix}/@connections/{connection_id}"),
        payload.to_vec(),
    )
    .await
}

/// One API Gateway V2 control-plane write, as JSON. Every write of this service answers a 2xx
/// of its own (`CreateApi` 201, `CreateApiMapping` 200), so the status is asserted as the class
/// rather than as one code.
async fn control_plane(
    router: &MultiServiceRouter,
    what: &str,
    path: &str,
    body: &Value,
) -> Value {
    let answered = call(
        router,
        CONTROL_PLANE_AUTHORIZATION,
        &format!("apigateway.{REGION}.amazonaws.com"),
        "POST",
        path,
        body.to_string().into_bytes(),
    )
    .await;
    assert!(
        (200..300).contains(&answered.status),
        "case {what:?}: expected a 2xx from the control plane, got {}",
        answered.rendered()
    );
    serde_json::from_str(&answered.body).unwrap_or_else(|e| {
        panic!(
            "case {what:?}: {what} must answer JSON ({e}): {}",
            answered.body
        )
    })
}

/// A required string member of a control-plane answer.
fn as_str(what: &str, body: &Value, member: &str) -> String {
    body.get(member)
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| panic!("case {what:?}: {what} must carry {member:?}, got {body}"))
}

/// A WEBSOCKET api with one route, a released deployment and [`STAGE`] serving it: the api id
/// the emulator minted, which is what its invoke host carries.
async fn deployed_websocket_api(router: &MultiServiceRouter) -> String {
    let api = control_plane(
        router,
        "CreateApi",
        "/v2/apis",
        &json!({
            "name": "callback-url-invoke-path",
            "protocolType": "WEBSOCKET",
            "routeSelectionExpression": "$request.body.action"
        }),
    )
    .await;
    let api_id = as_str("CreateApi", &api, "apiId");

    let integration = control_plane(
        router,
        "CreateIntegration",
        &format!("/v2/apis/{api_id}/integrations"),
        &json!({ "integrationType": "MOCK" }),
    )
    .await;
    let integration_id = as_str("CreateIntegration", &integration, "integrationId");

    control_plane(
        router,
        "CreateRoute",
        &format!("/v2/apis/{api_id}/routes"),
        &json!({ "routeKey": "$connect", "target": format!("integrations/{integration_id}") }),
    )
    .await;

    let deployment = control_plane(
        router,
        "CreateDeployment",
        &format!("/v2/apis/{api_id}/deployments"),
        &json!({}),
    )
    .await;
    let deployment_id = as_str("CreateDeployment", &deployment, "deploymentId");

    control_plane(
        router,
        "CreateStage",
        &format!("/v2/apis/{api_id}/stages"),
        &json!({ "stageName": STAGE, "deploymentId": deployment_id }),
    )
    .await;

    api_id
}

/// A custom domain name of this account mapping `key` — or its ROOT, when `key` is `None` — to
/// `api_id`'s deployed stage.
async fn domain_mapping_the_stage(
    router: &MultiServiceRouter,
    domain_name: &str,
    key: Option<&str>,
    api_id: &str,
) {
    control_plane(
        router,
        "CreateDomainName",
        "/v2/domainnames",
        // One configuration left to its defaults, which is what makes the name servable.
        &json!({ "domainName": domain_name, "domainNameConfigurations": [{}] }),
    )
    .await;
    let mut request = json!({ "apiId": api_id, "stage": STAGE });
    if let Some(key) = key {
        request["apiMappingKey"] = Value::String(key.to_string());
    }
    control_plane(
        router,
        "CreateApiMapping",
        &format!("/v2/domainnames/{domain_name}/apimappings"),
        &request,
    )
    .await;
}

#[test]
fn only_the_whole_invoke_path_of_a_deployed_stage_is_served() {
    std::thread::Builder::new()
        .name("connections-callback-url-invoke-path".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            // `enable_io`: a WEBSOCKET api's data plane and a custom domain name's front door
            // are both SOCKETS the core opens when the resource is created, so the runtime this
            // check creates them on has to be able to bind one.
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_io()
                .build()
                .expect("build a current-thread runtime");
            runtime.block_on(async {
                let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
                let mut router = build_router(Arc::clone(&engine));
                // Signature verification belongs to the emulator, not to this check.
                router.set_request_authentication(false);

                let api_id = deployed_websocket_api(&router).await;
                domain_mapping_the_stage(
                    &router,
                    MAPPED_DOMAIN,
                    Some(MAPPED_BASE_PATH),
                    &api_id,
                )
                .await;
                domain_mapping_the_stage(&router, ROOT_MAPPED_DOMAIN, None, &api_id).await;
                domain_mapping_the_stage(
                    &router,
                    MULTI_MAPPED_DOMAIN,
                    Some(MULTI_BASE_PATH),
                    &api_id,
                )
                .await;

                // A LIVE connection, minted through the one place an id is built, so every
                // refusal below is about the ENDPOINT and not about the connection.
                let connection_id = mgmt::state::connection_id_for(b"callback-url-invoke-path");
                assert!(
                    mgmt::state::register_connection(
                        &*engine,
                        &connection_id,
                        mgmt::state::Identity {
                            source_ip: SOURCE_IP.to_string(),
                            user_agent: Some(USER_AGENT.to_string()),
                        },
                        connected_at(),
                    ),
                    "the registration seam must register {connection_id:?}"
                );

                for form in FORMS {
                    let host = (form.host)(&api_id);
                    let prefix = (form.prefix)();
                    if form.served {
                        // A served addressing answers from the registry: the connection is
                        // reported, which is what says the endpoint resolved the stage that
                        // holds it.
                        let answered =
                            connections_call(&router, &host, &prefix, "GET", &connection_id, b"")
                                .await;
                        assert_eq!(
                            answered.status,
                            200,
                            "case {:?}: an endpoint resolving the deployed stage must report \
                             the connection, got {}",
                            form.case,
                            answered.rendered()
                        );
                        continue;
                    }

                    // Every operation, and both id forms: the endpoint is judged before the
                    // `ConnectionId` is read, so the id makes no difference to the answer.
                    for (method, payload) in OPERATIONS {
                        for (id_form, connection) in [
                            ("the live connection's own id", connection_id.as_str()),
                            (
                                "an id that could never have been minted",
                                MALFORMED_CONNECTION_ID,
                            ),
                        ] {
                            let case = format!("{method} with {id_form} through {}", form.case);
                            let answered = connections_call(
                                &router, &host, &prefix, method, connection, payload,
                            )
                            .await;
                            assert_eq!(
                                answered.status,
                                403,
                                "case {case:?}: an endpoint that resolves no deployed stage must \
                                 answer 403, got {}",
                                answered.rendered()
                            );
                            assert_eq!(
                                answered.error_type, "ForbiddenException",
                                "case {case:?}: expected the modelled ForbiddenException, got {}",
                                answered.rendered()
                            );
                            assert_eq!(
                                answered.message(),
                                FORBIDDEN_MESSAGE,
                                "case {case:?}: expected the sentence API Gateway refuses an \
                                 endpoint with, got {}",
                                answered.rendered()
                            );
                        }
                    }

                    // The refusal was taken in FRONT of the registry: the connection is still
                    // registered (no DELETE reached it), nothing was delivered to it, and its
                    // `LastActiveAt` has not moved.
                    let held = mgmt::state::connection(&*engine, &connection_id)
                        .unwrap_or_else(|| {
                            panic!(
                                "case {:?}: a refused call must leave the connection registered",
                                form.case
                            )
                        });
                    assert_eq!(
                        held.delivered_count, 0,
                        "case {:?}: a refused call must deliver nothing, got {} deliveries",
                        form.case, held.delivered_count
                    );
                    assert_eq!(
                        held.last_active_at, held.connected_at,
                        "case {:?}: a refused call must not move LastActiveAt, got {:?}",
                        form.case, held.last_active_at
                    );
                }

                // And an addressing that DOES resolve the stage keeps answering the
                // `ConnectionId` rules, on the api's own invoke host and on the base path the
                // custom domain maps alike: a value that could never have been issued is a bad
                // request that names it, while an id of the issued form the registry does not
                // hold is the memberless Gone.
                for (case, host, prefix) in [
                    (
                        "the api's own invoke host",
                        format!("{api_id}.execute-api.{REGION}.amazonaws.com"),
                        format!("/{STAGE}"),
                    ),
                    (
                        "the base path a custom domain maps",
                        MAPPED_DOMAIN.to_string(),
                        format!("/{MAPPED_BASE_PATH}"),
                    ),
                ] {
                    let refused = connections_call(
                        &router,
                        &host,
                        &prefix,
                        "GET",
                        MALFORMED_CONNECTION_ID,
                        b"",
                    )
                    .await;
                    assert_eq!(
                        refused.status, 400,
                        "case {case:?}: a malformed id on a resolved stage is a bad request, got \
                         {}",
                        refused.rendered()
                    );
                    assert_eq!(
                        refused.error_type, "BadRequestException",
                        "case {case:?}: expected BadRequestException, got {}",
                        refused.rendered()
                    );
                    assert_eq!(
                        refused.message(),
                        format!("Invalid connectionId: {MALFORMED_CONNECTION_ID}"),
                        "case {case:?}: the refusal must name the id the request sent, got {}",
                        refused.rendered()
                    );

                    let gone = connections_call(
                        &router,
                        &host,
                        &prefix,
                        "GET",
                        UNKNOWN_CONNECTION_ID,
                        b"",
                    )
                    .await;
                    assert_eq!(
                        gone.status, 410,
                        "case {case:?}: an unknown id of the issued form on a resolved stage is \
                         gone, got {}",
                        gone.rendered()
                    );
                    assert_eq!(
                        gone.error_type, "GoneException",
                        "case {case:?}: expected GoneException, got {}",
                        gone.rendered()
                    );
                    assert_eq!(
                        gone.message(),
                        "<no message>",
                        "case {case:?}: GoneException declares no members, so its body carries \
                         none, got {}",
                        gone.rendered()
                    );
                }
            });
        })
        .expect("spawn the callback-url-invoke-path thread")
        .join()
        .expect("the callback-url-invoke-path check must pass");
}
