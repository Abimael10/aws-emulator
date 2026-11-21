// SPDX-License-Identifier: CC0-1.0

//! The FORMS of the `@connections` callback URL a WebSocket backend can be configured with,
//! driven through the WHOLE assembled router.
//!
//! > **RULE: an `execute-api` `@connections` request is served only when the endpoint it was
//! > addressed through — its host together with the whole invoke path in front of the modelled
//! > `/@connections/{ConnectionId}` binding — resolves to a stage some api has DEPLOYED.** The
//! > first segment of an invoke path is the stage name ("the stage name is typically appended
//! > to this URI to form a complete path to a deployed API stage", the `Api$ApiEndpoint` doc of
//! > the API Gateway V2 model), so a callback URL that carries no stage segment names no stage
//! > at all. Every addressing that resolves no deployed stage is refused `403`
//! > `ForbiddenException` with the body `{"message": "Forbidden"}` BEFORE the request is
//! > dispatched — before the `ConnectionId`'s own form is read and before the connection
//! > registry is consulted — while a properly addressed, deployed stage keeps answering
//! > exactly what the modelled binding decides.
//!
//! WHY the order matters: `403` means "you may not invoke this endpoint" and is the only
//! answer that names a misconfigured callback URL. A client told `400 BadRequestException`
//! ("Invalid connectionId: …") instead blames its own id computation and re-mints ids that
//! were fine, and one told `410 GoneException` prunes live subscriber rows — so a callback URL
//! AWS will not serve has to be distinguishable from a bad id and from a closed socket.
//!
//! Two services, one engine, one router, so this is a router suite: the apis, the stages and
//! the deployment released to each stage are API Gateway V2's records (signing name
//! `apigateway`), while the refusal and the registry answers are the API Gateway Management
//! API's (signing name `execute-api`).
//!
//! The rows span what the rule generalises over: the host form (the api's own invoke host, the
//! regional `execute-api` address a client on this machine reaches, a custom domain) and the
//! invoke path form (the deployed stage, a stage nothing deployed, a two-segment base path,
//! no stage segment at all, and a bare path separator) — each over all three operations.
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

/// The region the api below is created and addressed in.
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

/// The stage the api deploys, and so the one segment its callback URL carries.
const STAGE: &str = "prod";

/// A stage name the api never creates, for the row addressed through one.
const UNDEPLOYED_STAGE: &str = "staging";

/// The identity the registered connection carries.
const SOURCE_IP: &str = "198.51.100.23";
const USER_AGENT: &str = "fan-out/1.0";

/// A `ConnectionId` of the shape API Gateway ISSUES that no connection owns: the id form the
/// three operations answer `GoneException` for on a properly addressed, deployed stage.
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

/// One callback-URL form a backend can be configured with: the host its URL names, the invoke
/// path in front of the modelled binding, and whether that endpoint serves the call.
///
/// Both are built from the api id the emulator minted, since that is what the api's own invoke
/// host carries.
struct Form {
    case: &'static str,
    host: fn(&str) -> String,
    prefix: fn(&str) -> String,
    served: bool,
}

/// The forms, spanning the host and the invoke path independently.
const FORMS: &[Form] = &[
    Form {
        case: "the documented callback URL: the api's own invoke host and its deployed stage",
        host: |api_id| format!("{api_id}.execute-api.{REGION}.amazonaws.com"),
        prefix: |_| format!("/{STAGE}"),
        served: true,
    },
    Form {
        case: "the regional execute-api address a client on this machine reaches, carrying the \
               deployed stage",
        host: |_| format!("execute-api.{REGION}.amazonaws.com"),
        prefix: |_| format!("/{STAGE}"),
        served: true,
    },
    Form {
        case: "the api's own invoke host with the stage left off, which is what a client that \
               never overrode the SDK's endpoint sends",
        host: |api_id| format!("{api_id}.execute-api.{REGION}.amazonaws.com"),
        prefix: |_| String::new(),
        served: false,
    },
    Form {
        case: "no stage segment, with a bare path separator in front of the binding",
        host: |api_id| format!("{api_id}.execute-api.{REGION}.amazonaws.com"),
        prefix: |_| "/".to_string(),
        served: false,
    },
    Form {
        case: "the regional execute-api address with no stage segment",
        host: |_| format!("execute-api.{REGION}.amazonaws.com"),
        prefix: |_| String::new(),
        served: false,
    },
    Form {
        case: "a stage this api never deployed",
        host: |api_id| format!("{api_id}.execute-api.{REGION}.amazonaws.com"),
        prefix: |_| format!("/{UNDEPLOYED_STAGE}"),
        served: false,
    },
    Form {
        case: "a custom domain whose base path maps no stage",
        host: |_| "sockets.example.com".to_string(),
        prefix: |_| "/v1/socket".to_string(),
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

/// One API Gateway V2 control-plane call, as JSON.
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
    assert_eq!(
        answered.status,
        201,
        "case {what:?}: expected HTTP 201 from the control plane, got {}",
        answered.rendered()
    );
    serde_json::from_str(&answered.body)
        .unwrap_or_else(|e| panic!("case {what:?}: {what} must answer JSON ({e}): {}", answered.body))
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
            "name": "callback-url-forms",
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

#[test]
fn only_a_callback_url_that_names_a_deployed_stage_is_served() {
    std::thread::Builder::new()
        .name("connections-callback-url-forms".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            // `enable_io`: a WEBSOCKET api's data plane is a SOCKET the core opens when the api
            // is created, so the runtime this check creates one on has to be able to bind one.
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

                // A LIVE connection, minted through the one place an id is built, so every
                // refusal below is about the ENDPOINT and not about the connection.
                let connection_id = mgmt::state::connection_id_for(b"callback-url-forms");
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
                    let prefix = (form.prefix)(&api_id);
                    if form.served {
                        // A served form answers from the registry: the connection is reported.
                        let answered =
                            connections_call(&router, &host, &prefix, "GET", &connection_id, b"")
                                .await;
                        assert_eq!(
                            answered.status,
                            200,
                            "case {:?}: an endpoint serving the deployed stage must report the \
                             connection, got {}",
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
                            ("an id that could never have been minted", MALFORMED_CONNECTION_ID),
                            ("an id of the issued form nothing holds", UNKNOWN_CONNECTION_ID),
                        ] {
                            let case = format!("{method} with {id_form} through {}", form.case);
                            let answered =
                                connections_call(&router, &host, &prefix, method, connection, payload)
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

                // And the documented form keeps answering the `ConnectionId` rules: a value
                // that could never have been issued is a bad request that names it, while an id
                // of the issued form the registry does not hold is the memberless Gone.
                let host = format!("{api_id}.execute-api.{REGION}.amazonaws.com");
                let prefix = format!("/{STAGE}");
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
                    "case \"a malformed id on the deployed stage\": expected 400, got {}",
                    refused.rendered()
                );
                assert_eq!(
                    refused.error_type, "BadRequestException",
                    "case \"a malformed id on the deployed stage\": expected \
                     BadRequestException, got {}",
                    refused.rendered()
                );
                assert_eq!(
                    refused.message(),
                    format!("Invalid connectionId: {MALFORMED_CONNECTION_ID}"),
                    "case \"a malformed id on the deployed stage\": the refusal must name the id \
                     the request sent, got {}",
                    refused.rendered()
                );

                let gone =
                    connections_call(&router, &host, &prefix, "GET", UNKNOWN_CONNECTION_ID, b"")
                        .await;
                assert_eq!(
                    gone.status, 410,
                    "case \"an unknown id of the issued form on the deployed stage\": expected \
                     410, got {}",
                    gone.rendered()
                );
                assert_eq!(
                    gone.error_type, "GoneException",
                    "case \"an unknown id of the issued form on the deployed stage\": expected \
                     GoneException, got {}",
                    gone.rendered()
                );
                assert_eq!(
                    gone.message(),
                    "<no message>",
                    "case \"an unknown id of the issued form on the deployed stage\": \
                     GoneException declares no members, so its body carries none, got {}",
                    gone.rendered()
                );
            });
        })
        .expect("spawn the callback-url-forms thread")
        .join()
        .expect("the callback-url-forms check must pass");
}
