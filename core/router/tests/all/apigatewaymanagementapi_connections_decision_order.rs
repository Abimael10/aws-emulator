// SPDX-License-Identifier: CC0-1.0

//! The ORDER the decisions behind a `@connections` request are taken in, driven end to end
//! through the WHOLE assembled router.
//!
//! > **RULE: a `@connections` request passes a FIXED sequence of decisions, each taken before
//! > every later one, so a request that would fail several of them is answered by the
//! > EARLIEST. First the ENDPOINT's own stage authority: an addressing that resolves no
//! > deployed stage of any api is refused `403` `ForbiddenException`. Then the
//! > `ConnectionId`'s SHAPE: a value API Gateway's own connection-id encoding could never
//! > have minted is refused `400` `BadRequestException` naming it. Then the caller's
//! > authorization: a principal whose readable identity-based policies do not allow
//! > `execute-api:ManageConnections` on the route it addressed is refused `403`
//! > `AccessDeniedException`. Only then is the connection registry consulted — `410`
//! > `GoneException` for an id it holds nothing for, and, for a DELIVERY, `429`
//! > `LimitExceededException` for a connection whose client side buffer is full. So a caller
//! > learns nothing from an earlier refusal about what a later decision would have read.**
//!
//! Five crates have to agree for that to hold, which is why this is a router suite: API
//! Gateway V2 holds the api, the stage and the deployment that make the addressing resolve a
//! stage at all, Amazon Cognito Identity vends the session the request signs with, IAM holds
//! the document in force on it, the token service is the registry that session is recorded
//! in, and the API Gateway Management API takes the remaining decisions and words every
//! refusal. Everything is driven the way a client reaches it, so each decision is taken over
//! records the owning service really wrote.
//!
//! The rows span what the rule generalises over: the operation (all three, so every method of
//! the route), the stage the invoke path names (the deployed one and one nothing deployed),
//! the `ConnectionId` the call carries (one that could never have been minted, one of the
//! minted shape that nothing holds, a live connection whose client stopped reading, and one
//! whose client is reading), and whether the caller holds the grant — each row making every
//! LATER rung's condition true as well, so what it asserts is which decision answered.
//!
//! What this suite does NOT own is the WORDING of each refusal, which has a suite of its own:
//! the authorization sentence and the resource it reports are
//! [`crate::apigatewaymanagementapi_manage_connections_authorization`]'s, the envelope of a
//! failure and the endpoint verdict are the management API crate's `connections_error_layer`,
//! and the client-buffer refusal is its `unread_connection_refusal`.
//!
//! Assembling the router costs stack, so it runs on an explicitly sized thread for the same
//! reason `boot_serve.rs` does.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use aws_emulator_router::{build_router, InMemoryEngine};
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;
use tokio::sync::mpsc::Receiver;

use service_com_amazonaws_apigatewaymanagementapi_apigatewaymanagementapi as mgmt;

use crate::apigateway_iam_authorized_invoke::{
    as_str, credential_scope, policy_document, vended_session, BOOT_STACK_SIZE, REGION,
};

/// The stage the api below is deployed to, and a stage name nothing ever deployed — the
/// addressing whose endpoint serves nothing.
const STAGE: &str = "live";
const UNDEPLOYED_STAGE: &str = "neverdeployed";

/// The action a `@connections` call asks for, and the code IAM words an authorization failure
/// in.
const MANAGE_CONNECTIONS: &str = "execute-api:ManageConnections";
const ACCESS_DENIED: &str = "AccessDeniedException";

/// The access key id neither registry holds a record for: the account's own published
/// credential, which the setup calls are signed with.
const ACCOUNT_KEY_ID: &str = "AKIDEXAMPLE";

/// The sentence API Gateway's `execute-api` endpoint refuses a request on a stage it does not
/// serve with — fixed text naming neither the api nor the stage.
const FORBIDDEN_MESSAGE: &str = "Forbidden";

/// The sentence a delivery to a connection whose client side buffer is full is refused with:
/// fixed text naming the BUFFER rather than a request rate, which is what tells a sender
/// which of the two conditions the modelled `LimitExceededException` covers it was refused
/// for. Spelled out here rather than read from the code under test.
const CLIENT_BUFFER_FULL_MESSAGE: &str = "Too Many Requests. WebSocket client buffer full";

/// A `ConnectionId` API Gateway's own encoding could never have minted: a minted id is the
/// padded base64 of an 11-byte token, and this value is not in that alphabet at all.
const UNMINTABLE_ID: &str = "connection-id-nothing-mints";

/// How long a case waits for a delivered payload to reach the client's end of the socket:
/// generous for a loaded test machine, and bounded so a payload that never arrives fails the
/// case instead of hanging the suite.
const DELIVERY_DEADLINE: Duration = Duration::from_secs(30);

/// A registration instant in the past, so an instant the service writes later is strictly
/// greater than it.
fn connected_at() -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000)
}

/// One answered request: the status, the headers a restJson1 client resolves an error SHAPE
/// from, and the body.
struct Answered {
    status: u16,
    headers: http::HeaderMap,
    text: String,
}

impl Answered {
    /// The error shape a restJson1 failure names, read where a client reads it.
    fn error_code(&self) -> String {
        self.headers
            .get("x-amzn-errortype")
            .map(|value| String::from_utf8_lossy(value.as_bytes()).to_string())
            .unwrap_or_else(|| "<no x-amzn-errortype>".to_string())
    }

    /// The `message` member of the body, or `None` for a body that carries none.
    fn message(&self) -> Option<String> {
        serde_json::from_str::<Value>(&self.text)
            .ok()
            .and_then(|body| {
                body.get("message")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
    }

    fn summary(&self) -> String {
        format!("{} {}", self.status, self.text)
    }
}

/// Dispatch one `@connections` request as it arrives on the wire, signed by `access_key_id`.
async fn connections_call(
    router: &MultiServiceRouter,
    host: &str,
    access_key_id: &str,
    method: &str,
    path: &str,
    payload: &[u8],
) -> Answered {
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host)
        .header(
            "authorization",
            credential_scope(access_key_id, "execute-api"),
        )
        .body(bytes::Bytes::from(payload.to_vec()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let headers = response.headers().clone();
    Answered {
        status,
        headers,
        text: String::from_utf8_lossy(&response.into_body()).to_string(),
    }
}

/// One API Gateway V2 control-plane call, which must answer `expected_status`.
async fn control_plane(
    router: &MultiServiceRouter,
    what: &str,
    path: &str,
    body: &Value,
    expected_status: u16,
) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://apigateway.{REGION}.amazonaws.com{path}"))
        .header("host", format!("apigateway.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", credential_scope(ACCOUNT_KEY_ID, "apigateway"))
        .body(bytes::Bytes::from(body.to_string().into_bytes()))
        .expect("the setup request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(&response.into_body()).to_string();
    assert_eq!(
        status, expected_status,
        "setup: {what} must answer HTTP {expected_status}, got {status} {text}"
    );
    serde_json::from_str(&text).unwrap_or(Value::Null)
}

/// A WEBSOCKET api with one stage a deployment is released to, as the control plane writes
/// it. Answers the api's id, which is what its own `execute-api` invoke host names.
async fn deployed_websocket_api(router: &MultiServiceRouter, name: &str) -> String {
    let created = control_plane(
        router,
        "CreateApi",
        "/v2/apis",
        &json!({
            "name": name,
            "protocolType": "WEBSOCKET",
            "routeSelectionExpression": "$request.body.action"
        }),
        201,
    )
    .await;
    let api_id = as_str("CreateApi", &created, "apiId");
    let integration = control_plane(
        router,
        "CreateIntegration",
        &format!("/v2/apis/{api_id}/integrations"),
        &json!({ "integrationType": "MOCK" }),
        201,
    )
    .await;
    let integration_id = as_str("CreateIntegration", &integration, "integrationId");
    control_plane(
        router,
        "CreateRoute",
        &format!("/v2/apis/{api_id}/routes"),
        &json!({
            "routeKey": "$connect",
            "target": format!("integrations/{integration_id}")
        }),
        201,
    )
    .await;
    let deployment = control_plane(
        router,
        "CreateDeployment",
        &format!("/v2/apis/{api_id}/deployments"),
        &json!({}),
        201,
    )
    .await;
    let deployment_id = as_str("CreateDeployment", &deployment, "deploymentId");
    control_plane(
        router,
        "CreateStage",
        &format!("/v2/apis/{api_id}/stages"),
        &json!({ "stageName": STAGE, "deploymentId": deployment_id }),
        201,
    )
    .await;
    api_id
}

/// The `execute-api` host AWS publishes an api's own invoke endpoint on, which is where the
/// `@connections` operations are reached.
fn invoke_host(api_id: &str) -> String {
    format!("{api_id}.execute-api.{REGION}.amazonaws.com")
}

/// Open a connection carried on a LIVE channel, handing the client's end back: holding it
/// without reading is a client that stopped draining its socket.
///
/// `backlogged` fills the client's queue first, which is the condition a delivery refusal is
/// about. It is filled through the channel itself rather than by posting, so the record's own
/// delivery counters still read zero and a row's call is the only delivery it ever saw.
fn live_connection(
    engine: &Arc<InMemoryEngine>,
    case: &str,
    connection_id: &str,
    backlogged: bool,
) -> Receiver<Vec<u8>> {
    let registry = mgmt::state::registry(&**engine).expect("setup: the engine carries the registry");
    let (pushes, pushed) = tokio::sync::mpsc::channel(mgmt::state::CONNECTION_PUSH_DEPTH);
    if backlogged {
        for index in 0..mgmt::state::CONNECTION_PUSH_DEPTH {
            pushes
                .try_send(format!("waiting-{index}").into_bytes())
                .unwrap_or_else(|error| {
                    panic!("case {case:?}: the client's queue must take payload {index}: {error}")
                });
        }
    }
    assert!(
        mgmt::state::open_connection(
            &registry,
            connection_id.to_string(),
            mgmt::state::Identity {
                source_ip: "192.0.2.40".to_string(),
                user_agent: Some("ladder/1".to_string()),
            },
            connected_at(),
            None,
            Some(pushes),
        ),
        "case {case:?}: the connection {connection_id} must be opened on a live channel"
    );
    pushed
}

/// The `ConnectionId` a row's call carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Id {
    /// A value API Gateway's connection-id encoding could never have minted.
    Unmintable,
    /// Of the minted shape, held by nothing.
    Absent,
    /// A live connection whose client stopped reading, its queue already full.
    Backlogged,
    /// A live connection whose client is reading.
    Draining,
}

/// WHICH decision answered the request: the rungs of the ladder, in the order they are taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Decided {
    /// The endpoint's own stage authority.
    Endpoint,
    /// The `ConnectionId`'s shape.
    IdShape,
    /// The caller's `execute-api:ManageConnections` authorization.
    Authorization,
    /// The connection registry: it holds nothing for this id.
    Registry,
    /// The client's own buffer, which only a DELIVERY can fill.
    ClientBuffer,
    /// Nothing refused it: the operation answered.
    Served,
}

/// One row: the addressing, the id and the grant, and the decision the rule owes the answer.
struct Row {
    case: &'static str,
    /// The stage segment of the invoke path the call addresses.
    stage: &'static str,
    id: Id,
    /// Whether the caller's role carries a document allowing the action.
    granted: bool,
    decided: Decided,
}

/// Every row makes the conditions of every LATER rung true as well, so what it pins is which
/// decision answered rather than that the condition itself is noticed.
const ROWS: &[Row] = &[
    Row {
        case: "an endpoint serving no deployed stage, an unmintable id, and no grant",
        stage: UNDEPLOYED_STAGE,
        id: Id::Unmintable,
        granted: false,
        decided: Decided::Endpoint,
    },
    Row {
        case: "an endpoint serving no deployed stage, reached by a caller holding the grant \
               on a live connection",
        stage: UNDEPLOYED_STAGE,
        id: Id::Draining,
        granted: true,
        decided: Decided::Endpoint,
    },
    Row {
        case: "an unmintable id, and no grant, on the deployed stage",
        stage: STAGE,
        id: Id::Unmintable,
        granted: false,
        decided: Decided::IdShape,
    },
    Row {
        case: "no grant, on a connection nothing holds",
        stage: STAGE,
        id: Id::Absent,
        granted: false,
        decided: Decided::Authorization,
    },
    Row {
        case: "no grant, on a live connection whose client stopped reading",
        stage: STAGE,
        id: Id::Backlogged,
        granted: false,
        decided: Decided::Authorization,
    },
    Row {
        case: "the grant, on a connection nothing holds",
        stage: STAGE,
        id: Id::Absent,
        granted: true,
        decided: Decided::Registry,
    },
    Row {
        case: "the grant, on a live connection whose client stopped reading",
        stage: STAGE,
        id: Id::Backlogged,
        granted: true,
        decided: Decided::ClientBuffer,
    },
    Row {
        case: "the grant, on a live connection whose client is reading",
        stage: STAGE,
        id: Id::Draining,
        granted: true,
        decided: Decided::Served,
    },
];

/// The three operations, by the method each is reached with, the payload each sends and the
/// status each answers a call it serves.
const OPERATIONS: [(&str, &[u8], u16); 3] = [
    ("GET", b"", 200),
    ("POST", b"ladder", 200),
    ("DELETE", b"", 204),
];

/// Assert `answered` is the refusal `status`/`code`, and that its `message` is `message` —
/// `None` for a body that must carry no message member at all.
fn assert_refusal(
    case: &str,
    answered: &Answered,
    status: u16,
    code: &str,
    message: Option<&str>,
) {
    assert_eq!(
        answered.status,
        status,
        "case {case:?}: expected HTTP {status}, got {}",
        answered.summary()
    );
    assert_eq!(
        answered.error_code(),
        code,
        "case {case:?}: expected {code} in x-amzn-errortype, got {} in {}",
        answered.error_code(),
        answered.summary()
    );
    let reported = answered.message();
    assert_eq!(
        reported.as_deref(),
        message,
        "case {case:?}: expected the message {message:?}, got {reported:?}"
    );
}

#[test]
fn each_connections_decision_is_taken_before_every_later_one() {
    std::thread::Builder::new()
        .name("agwmgmt-connections-decision-order".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            // `enable_io`: a WEBSOCKET api's data plane is a socket the core opens when the
            // api is created, so the runtime this check builds one on has to bind one.
            // `enable_time`: the wait for a delivered payload is bounded by a deadline.
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_io()
                .enable_time()
                .build()
                .expect("build a current-thread runtime");
            runtime.block_on(async { the_ladder().await });
        })
        .expect("spawn the sized thread")
        .join()
        .expect("the cases run to completion");
}

async fn the_ladder() {
    let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
    let mut router = build_router(Arc::clone(&engine));
    // Signature verification belongs to the emulator, not to this check: what is under test
    // is the ORDER of the decisions about a credential, not the credential's signature.
    router.set_request_authentication(false);

    let api_id = deployed_websocket_api(&router, "connections-decision-order").await;
    let host = invoke_host(&api_id);
    // The client ends of the backlogged connections, held so those clients stay connected and
    // keep not reading for as long as the cases run.
    let mut unread: Vec<Receiver<Vec<u8>>> = Vec::new();

    for (index, row) in ROWS.iter().enumerate() {
        for (method, payload, served_status) in OPERATIONS {
            let case = format!("{method} with {}", row.case);
            // A full client buffer is a DELIVERY's refusal: the other two operations report
            // on and close such a connection as usual, so that rung is `POST`'s alone.
            let decided = match row.decided {
                Decided::ClientBuffer if method != "POST" => Decided::Served,
                other => other,
            };

            // One connection, and one id, per case: a label of its own, folded into a token
            // by the service's own encoder rather than spelled out here.
            let label = format!("{}{index}{}", if row.granted { "g" } else { "u" }, &method[..1]);
            let minted = mgmt::state::connection_id_for(label.as_bytes());
            let mut reading = None;
            match row.id {
                Id::Unmintable | Id::Absent => {}
                Id::Backlogged => unread.push(live_connection(&engine, &case, &minted, true)),
                Id::Draining => reading = Some(live_connection(&engine, &case, &minted, false)),
            }
            let connection_id = match row.id {
                Id::Unmintable => UNMINTABLE_ID.to_string(),
                _ => minted.clone(),
            };

            // The principal this case signs with: a session of a role of its own, carrying a
            // document that allows the action on every resource, or no document at all.
            let name = format!("connections-ladder-{index}-{}", method.to_lowercase());
            let document = row
                .granted
                .then(|| policy_document("Allow", MANAGE_CONNECTIONS, "*"));
            let access_key_id = vended_session(&router, &name, document).await;

            let answered = connections_call(
                &router,
                &host,
                &access_key_id,
                method,
                &format!("/{}/@connections/{connection_id}", row.stage),
                payload,
            )
            .await;

            match decided {
                Decided::Endpoint => assert_refusal(
                    &case,
                    &answered,
                    403,
                    "ForbiddenException",
                    Some(FORBIDDEN_MESSAGE),
                ),
                Decided::IdShape => assert_refusal(
                    &case,
                    &answered,
                    400,
                    "BadRequestException",
                    Some(&format!("Invalid connectionId: {connection_id}")),
                ),
                Decided::Authorization => {
                    assert_eq!(
                        answered.status,
                        403,
                        "case {case:?}: expected HTTP 403, got {}",
                        answered.summary()
                    );
                    assert_eq!(
                        answered.error_code(),
                        ACCESS_DENIED,
                        "case {case:?}: expected {ACCESS_DENIED} in x-amzn-errortype, got {}",
                        answered.error_code()
                    );
                    let message = answered.message().unwrap_or_default();
                    assert!(
                        message.starts_with("User: ")
                            && message.contains(&format!(
                                "is not authorized to perform: {MANAGE_CONNECTIONS}"
                            )),
                        "case {case:?}: expected the sentence IAM words an authorization \
                         failure in, got {message:?}"
                    );
                    // The refusal is taken before the registry is read, so it says nothing
                    // about which connections exist: the route's own parameter is what the
                    // resource names, never the id this call carried.
                    assert!(
                        !message.contains(&connection_id),
                        "case {case:?}: the refusal must not name the id {connection_id:?} the \
                         call carried, got {message:?}"
                    );
                }
                Decided::Registry => {
                    assert_refusal(&case, &answered, 410, "GoneException", None);
                }
                Decided::ClientBuffer => assert_refusal(
                    &case,
                    &answered,
                    429,
                    "LimitExceededException",
                    Some(CLIENT_BUFFER_FULL_MESSAGE),
                ),
                Decided::Served => {
                    assert_eq!(
                        answered.status,
                        served_status,
                        "case {case:?}: expected the operation's own {served_status}, got {}",
                        answered.summary()
                    );
                    // A served delivery is a delivery: the payload is at the client's end of
                    // the socket, which is what makes the last rung an answer and not a
                    // refusal that was skipped.
                    if method == "POST" {
                        if let Some(reading) = reading.as_mut() {
                            let read = tokio::time::timeout(DELIVERY_DEADLINE, reading.recv())
                                .await
                                .unwrap_or_else(|_elapsed| {
                                    panic!(
                                        "case {case:?}: the delivered payload must reach the \
                                         client within {DELIVERY_DEADLINE:?}"
                                    )
                                });
                            assert_eq!(
                                read.as_deref(),
                                Some(payload),
                                "case {case:?}: the client must read the payload as it was \
                                 sent, got {read:?}"
                            );
                        }
                    }
                }
            }

            // What a refusal leaves behind: a live connection it refused delivered nothing and
            // was not marked active, whichever decision refused it. A row addressing a LIVE
            // connection must still hold one — a refusal that closed it would be a decision
            // taken on the registry after all — while a row carrying an id nothing was opened
            // for has no record to read.
            if decided != Decided::Served {
                let stored = mgmt::state::connection(&*engine, &minted);
                match row.id {
                    Id::Backlogged | Id::Draining => assert!(
                        stored.is_some(),
                        "case {case:?}: the refused call must leave the live connection open"
                    ),
                    Id::Unmintable | Id::Absent => assert!(
                        stored.is_none(),
                        "case {case:?}: nothing was opened for this id, so nothing may hold a \
                         record of it"
                    ),
                }
                if let Some(stored) = stored {
                    assert_eq!(
                        stored.delivered_count, 0,
                        "case {case:?}: a refused call delivers nothing, got {} deliveries",
                        stored.delivered_count
                    );
                    assert_eq!(
                        stored.last_active_at, stored.connected_at,
                        "case {case:?}: a refused call must not move LastActiveAt, got {:?}",
                        stored.last_active_at
                    );
                }
            }
        }
    }
}
