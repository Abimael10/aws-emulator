// SPDX-License-Identifier: CC0-1.0

//! The announced `wss://` socket driven the way a client of AWS dials it: TLS first, then
//! the RFC 6455 opening handshake and the frames over the encrypted stream.
//!
//! The rule these cases are a slice of:
//!
//! > The address a WEBSOCKET api announces as its `ApiEndpoint` is one a WebSocket client
//! > library can DIAL as it stands — the scheme it carries included. So the socket it names
//! > completes the opening handshake for a client that negotiated TLS first, dispatches the
//! > frames that follow to the routes the api deployed, and carries the backend's own reply
//! > back: a handler that posts to the `@connections` endpoint ITS OWN event names reaches
//! > this api's connection, and the payload arrives on the socket the client holds.
//!
//! Every other suite of this data plane dials the announced socket in the clear, which the
//! same socket also serves (the encryption is negotiated in band); these two drive the half a
//! client of AWS uses, with a real TLS client, and the leg a WebSocket application exists for
//! — the reply the BACKEND pushes.
//!
//! [`every_reporter_announces_the_uri_a_client_dials_and_that_uri_carries_the_frames`] holds
//! the same rule over the whole control plane, because `ApiEndpoint` is reported by every
//! operation that reports an api and a client reads it from whichever one it called:
//!
//! > The `ApiEndpoint` of a WEBSOCKET api is the one URI that api's data plane is reached at,
//! > reported identically by every operation that reports the api, and "the stage name is
//! > typically appended to this URI to form a complete path to a deployed API stage"
//! > (`Api$ApiEndpoint`). So that URI TAKEN AS GIVEN — the scheme it carries dialled as that
//! > scheme, the authority it carries dialled as that authority — with the stage name
//! > appended, completes the RFC 6455 opening handshake with `101 Switching Protocols` and
//! > then carries the frames of that api's deployed stage.
//!
//! The harness mounts four services on one engine, because that reply crosses all of them:
//! this service's socket, the function AWS Lambda runs, the `@connections` endpoint
//! ApiGatewayManagementApi serves and the log group CloudWatch Logs holds. The control-plane
//! requests go through `api_lifecycle`'s one signed caller and the log reads through the
//! access-log suite's one reader, so nothing about either wire is restated here.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;
use tokio::net::TcpStream;

use crate::api_lifecycle::call_in_region;
use crate::http_api_access_logging::delivered_to;
use crate::http_api_backend_dispatch::given_function;

/// The Region every request below is signed for — the one the Lambda harness creates its
/// functions in, so the api and the function it names live in the same Region.
const REGION: &str = "us-east-1";

/// The stage every case deploys.
const STAGE: &str = "prod";

/// The four services this suite spans, over ONE engine. The access-log suite's harness mounts
/// three of them ([`crate::http_api_access_logging::emulator`]); the `@connections` endpoint a
/// backend pushes its reply through is the fourth, which is why this one is its own.
fn emulator() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_apigatewayv2_apigatewayv2::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_apigatewaymanagementapi_apigatewaymanagementapi::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    (router, engine)
}

/// One control-plane request that must answer `201`, signed by the one caller this crate's
/// suites share.
async fn created(emulator: &MultiServiceRouter, what: &str, uri: &str, body: Value) -> Value {
    let (status, answered) = call_in_region(emulator, REGION, "POST", uri, Some(body)).await;
    assert_eq!(status, 201, "{what} must answer 201, got {status} {answered}");
    answered
}

/// A handler that answers the event it was given.
fn echoing_handler() -> String {
    "import json\n\
     \n\
     \n\
     def lambda_handler(event, context):\n\
     \x20   print(\"handled a call\")\n\
     \x20   return {\"statusCode\": 200, \"body\": json.dumps({\"event\": event})}\n"
        .to_string()
}

/// A handler that pushes its answer BACK to the connection through the `@connections`
/// endpoint the event names, the way a WebSocket backend replies.
fn pushing_handler() -> String {
    "import json\n\
     import boto3\n\
     \n\
     \n\
     def lambda_handler(event, context):\n\
     \x20   ctx = event[\"requestContext\"]\n\
     \x20   endpoint = \"https://\" + ctx[\"domainName\"] + \"/\" + ctx[\"stage\"]\n\
     \x20   print(\"pushing to \" + endpoint)\n\
     \x20   client = boto3.client(\"apigatewaymanagementapi\", endpoint_url=endpoint)\n\
     \x20   client.post_to_connection(\n\
     \x20       ConnectionId=ctx[\"connectionId\"],\n\
     \x20       Data=json.dumps({\"pushed\": True}).encode(),\n\
     \x20   )\n\
     \x20   return {\"statusCode\": 200}\n"
        .to_string()
}

/// The messages the function's own log group holds, read through the one reader this crate's
/// suites share ([`crate::http_api_access_logging::delivered_to`]).
async fn log_messages(emulator: &MultiServiceRouter, group: &str) -> Vec<String> {
    delivered_to(emulator, group).await.unwrap_or_default()
}

/// One route of the api a case builds: the key it serves, the integration body it fronts, the
/// integration responses that integration holds, and whether it is a TWO-WAY route — "to
/// define a two-way route, create a route response".
struct Route {
    key: &'static str,
    integration: Value,
    responses: Vec<Value>,
    two_way: bool,
}

/// The integration body of a route fronting the Lambda function `function`.
fn fronting(function: &str) -> Value {
    json!({
        "integrationType": "AWS_PROXY",
        "integrationUri": function,
        "integrationMethod": "POST"
    })
}

/// Create a WEBSOCKET api holding `routes`, with `stage` created and a deployment released to
/// it, and answer `(the api id, CreateApi's own answer)` — whose `apiEndpoint` is the URI this
/// api announces.
async fn given_socket(
    emulator: &MultiServiceRouter,
    name: &str,
    stage: &str,
    routes: Vec<Route>,
) -> (String, Value) {
    let api = created(
        emulator,
        "CreateApi",
        "/v2/apis",
        json!({
            "name": name,
            "protocolType": "WEBSOCKET",
            "routeSelectionExpression": "$request.body.action"
        }),
    )
    .await;
    let api_id = api
        .get("apiId")
        .and_then(Value::as_str)
        .expect("CreateApi answers an apiId")
        .to_string();

    for route in routes {
        let integration = created(
            emulator,
            "CreateIntegration",
            &format!("/v2/apis/{api_id}/integrations"),
            route.integration,
        )
        .await;
        let integration_id = integration
            .get("integrationId")
            .and_then(Value::as_str)
            .expect("CreateIntegration answers an integrationId")
            .to_string();
        for response in route.responses {
            created(
                emulator,
                "CreateIntegrationResponse",
                &format!("/v2/apis/{api_id}/integrations/{integration_id}/integrationresponses"),
                response,
            )
            .await;
        }
        let declared = created(
            emulator,
            "CreateRoute",
            &format!("/v2/apis/{api_id}/routes"),
            json!({ "routeKey": route.key, "target": format!("integrations/{integration_id}") }),
        )
        .await;
        if route.two_way {
            let route_id = declared
                .get("routeId")
                .and_then(Value::as_str)
                .expect("CreateRoute answers a routeId")
                .to_string();
            created(
                emulator,
                "CreateRouteResponse",
                &format!("/v2/apis/{api_id}/routes/{route_id}/routeresponses"),
                json!({ "routeResponseKey": "$default" }),
            )
            .await;
        }
    }
    created(
        emulator,
        "CreateStage",
        &format!("/v2/apis/{api_id}/stages"),
        json!({ "stageName": stage }),
    )
    .await;
    created(
        emulator,
        "CreateDeployment",
        &format!("/v2/apis/{api_id}/deployments"),
        json!({ "stageName": stage }),
    )
    .await;
    (api_id, api)
}

/// The URI `api` reports as its `ApiEndpoint`.
fn announced_endpoint(api: &Value) -> String {
    api.get("apiEndpoint")
        .and_then(Value::as_str)
        .expect("an api reports an apiEndpoint")
        .to_string()
}

#[tokio::test]
async fn a_backend_pushes_its_reply_back_through_the_announced_endpoint() {
    let case = "a backend that replies through @connections";
    let (emulator, engine) = emulator();
    let admitting =
        given_function(&emulator, &engine, case, "socket-admit", &echoing_handler()).await;
    let pushing =
        given_function(&emulator, &engine, case, "socket-push", &pushing_handler()).await;
    let (_api_id, api) = given_socket(
        &emulator,
        "push-socket-api",
        STAGE,
        vec![
            Route {
                key: "$connect",
                integration: fronting(&admitting),
                responses: Vec::new(),
                two_way: false,
            },
            Route {
                key: "$default",
                integration: fronting(&pushing),
                responses: Vec::new(),
                two_way: false,
            },
        ],
    )
    .await;
    let mut socket = dial(case, &announced_endpoint(&api), STAGE).await;

    let text = exchanged(case, &mut socket, &json!({ "action": "register" })).await;
    assert!(
        text.contains("pushed"),
        "case {case:?}: expected the backend's own payload, got {text:?}"
    );
    let messages = log_messages(&emulator, "/aws/lambda/socket-push").await;
    assert!(
        messages.iter().any(|line| line.contains("pushing to")),
        "case {case:?}: the function's log group must hold what the run logged, got {messages:?}"
    );
}

/// Dial the URI `announced` AS GIVEN — the scheme it carries, the authority it carries — with
/// `stage` appended to it, and complete the opening handshake there.
///
/// Nothing about the address is spelled out here beyond the one assertion that it is loopback,
/// which is what keeps a case from reaching anything but this machine's own listener: the
/// scheme, the host and the port all come from the value the control plane reported.
async fn dial(
    case: &str,
    announced: &str,
    stage: &str,
) -> tokio_tungstenite::WebSocketStream<ClientStream> {
    let (scheme, rest) = announced.split_once("://").unwrap_or_else(|| {
        panic!("case {case:?}: an announced endpoint is an absolute URI, got {announced:?}")
    });
    assert_eq!(
        scheme, "wss",
        "case {case:?}: a WEBSOCKET api is announced with the secure WebSocket scheme a \
         client dials it by, got {announced:?}"
    );
    let authority = rest.split('/').next().unwrap_or(rest);
    let (host, port) = authority.rsplit_once(':').unwrap_or_else(|| {
        panic!("case {case:?}: the announced socket names a host:port, got {announced:?}")
    });
    assert_eq!(
        host, "127.0.0.1",
        "case {case:?}: a case dials the emulator's own loopback listener and nothing else, \
         got {announced:?}"
    );
    let port: u16 = port.parse().unwrap_or_else(|_| {
        panic!("case {case:?}: the announced port must parse, got {announced:?}")
    });
    let stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap_or_else(|error| {
        panic!("case {case:?}: the announced socket {announced:?} must accept a connection, got {error}")
    });
    let tls = upgrade_to_tls(stream).await;
    let url = format!("{announced}/{stage}");
    let (socket, response) =
        tokio::time::timeout(HANDSHAKE_DEADLINE, tokio_tungstenite::client_async(&url, tls))
            .await
            .unwrap_or_else(|_| {
                panic!("case {case:?}: the handshake at {url:?} must be answered")
            })
            .unwrap_or_else(|error| {
                panic!("case {case:?}: the handshake at {url:?} must complete, got {error}")
            });
    assert_eq!(
        response.status().as_u16(),
        SWITCHING_PROTOCOLS,
        "case {case:?}: a deployed stage reached at the announced URI {url:?} must answer \
         {SWITCHING_PROTOCOLS}"
    );
    socket
}

/// Send one message over `socket` and read the text frame the api answers it with.
async fn exchanged(
    case: &str,
    socket: &mut tokio_tungstenite::WebSocketStream<ClientStream>,
    message: &Value,
) -> String {
    use futures_util::{SinkExt, StreamExt};

    socket
        .send(tokio_tungstenite::tungstenite::Message::Text(
            message.to_string().into(),
        ))
        .await
        .unwrap_or_else(|error| panic!("case {case:?}: the frame must be sent, got {error}"));
    let answered = tokio::time::timeout(REPLY_DEADLINE, socket.next())
        .await
        .unwrap_or_else(|_| {
            panic!("case {case:?}: the api must answer {message} over the same socket")
        });
    match answered {
        Some(Ok(tokio_tungstenite::tungstenite::Message::Text(text))) => text.as_str().to_string(),
        other => panic!("case {case:?}: expected the route's reply as a text frame, got {other:?}"),
    }
}

/// How long a case waits for the opening handshake to be answered, and for the reply the
/// serving route owes. Both waits are for a CONDITION — an answer — and are given generous
/// room so a loaded machine still reaches it; nothing here reads the clock or asserts on
/// elapsed time.
const HANDSHAKE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(60);
const REPLY_DEADLINE: std::time::Duration = std::time::Duration::from_secs(120);

/// The status a completed opening handshake carries (RFC 6455 §4.1).
const SWITCHING_PROTOCOLS: u16 = 101;

/// The stream a case's client holds once its TLS handshake has completed.
pub(crate) type ClientStream = tokio_rustls::client::TlsStream<TcpStream>;

#[tokio::test]
async fn the_announced_socket_completes_a_tls_handshake_and_carries_frames() {
    let case = "a client that dials the announced wss:// address";
    let (emulator, engine) = emulator();
    let function =
        given_function(&emulator, &engine, case, "socket-echo", &echoing_handler()).await;
    let (_api_id, api) = given_socket(
        &emulator,
        "tls-socket-api",
        STAGE,
        vec![
            Route {
                key: "$connect",
                integration: fronting(&function),
                responses: Vec::new(),
                two_way: false,
            },
            Route {
                key: "$default",
                integration: fronting(&function),
                responses: Vec::new(),
                two_way: true,
            },
        ],
    )
    .await;
    let mut socket = dial(case, &announced_endpoint(&api), STAGE).await;

    let text = exchanged(case, &mut socket, &json!({ "action": "ping" })).await;
    assert!(
        text.contains("event"),
        "case {case:?}: expected the function's own document as the route's reply, got {text:?}"
    );

    // What a client debugging the backend reads: the function's own log group.
    let group = "/aws/lambda/socket-echo";
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let mut messages = log_messages(&emulator, group).await;
    while messages.is_empty() && std::time::Instant::now() < deadline {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        messages = log_messages(&emulator, group).await;
    }
    assert!(
        !messages.is_empty(),
        "the function's log group must hold what the run logged, got {messages:?}"
    );
}

/// Run the client side of a TLS handshake on `conn`, accepting the emulator's own
/// self-signed listener certificate.
pub(crate) async fn upgrade_to_tls(conn: TcpStream) -> tokio_rustls::client::TlsStream<TcpStream> {
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(Arc::clone(&provider))
        .with_safe_default_protocol_versions()
        .expect("the provider supports the default protocol versions")
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptTheEmulatorsCertificate(provider)))
        .with_no_client_auth();
    let connector = tokio_rustls::TlsConnector::from(Arc::new(config));
    let server_name =
        rustls::pki_types::ServerName::try_from("localhost").expect("localhost is a server name");
    connector
        .connect(server_name, conn)
        .await
        .expect("the server must complete the in-band TLS handshake")
}

/// See [`upgrade_to_tls`].
#[derive(Debug)]
struct AcceptTheEmulatorsCertificate(Arc<rustls::crypto::CryptoProvider>);

impl rustls::client::danger::ServerCertVerifier for AcceptTheEmulatorsCertificate {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

// ---------------------------------------------------------------------------------------
// The announced URI taken AS GIVEN, read off every operation that reports the api.
// ---------------------------------------------------------------------------------------

/// The operation a client read `ApiEndpoint` off.
///
/// `Api$ApiEndpoint` is a member of every operation that reports an api, and a client reads it
/// from whichever one it happened to call — the create that minted the api, a later read, or
/// an update. The value is that api's one announced address, so each of them reports the same
/// dialable URI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reporter {
    CreateApi,
    GetApi,
    GetApis,
    UpdateApi,
}

/// Every operation that reports an api, in the order a row reads them.
const REPORTERS: [Reporter; 4] = [
    Reporter::CreateApi,
    Reporter::GetApi,
    Reporter::GetApis,
    Reporter::UpdateApi,
];

/// The `apiEndpoint` `reporter` answers for `api_id`, read off that operation's own answer.
async fn announced_by(
    emulator: &MultiServiceRouter,
    case: &str,
    reporter: Reporter,
    api_id: &str,
    created: &Value,
) -> String {
    let reported = match reporter {
        Reporter::CreateApi => created.clone(),
        Reporter::GetApi => {
            let (status, fetched) =
                call_in_region(emulator, REGION, "GET", &format!("/v2/apis/{api_id}"), None).await;
            assert_eq!(
                status, 200,
                "case {case:?}: GetApi must answer 200, got {status} {fetched}"
            );
            fetched
        }
        Reporter::GetApis => {
            let (status, listed) = call_in_region(emulator, REGION, "GET", "/v2/apis", None).await;
            assert_eq!(
                status, 200,
                "case {case:?}: GetApis must answer 200, got {status} {listed}"
            );
            listed["items"]
                .as_array()
                .and_then(|items| {
                    items
                        .iter()
                        .find(|item| item.get("apiId").and_then(Value::as_str) == Some(api_id))
                        .cloned()
                })
                .unwrap_or_else(|| {
                    panic!("case {case:?}: GetApis must list the api {api_id}, got {listed}")
                })
        }
        Reporter::UpdateApi => {
            let (status, updated) = call_in_region(
                emulator,
                REGION,
                "PATCH",
                &format!("/v2/apis/{api_id}"),
                Some(json!({ "description": "read the announced endpoint back" })),
            )
            .await;
            assert_eq!(
                status, 201,
                "case {case:?}: UpdateApi must answer 201, got {status} {updated}"
            );
            updated
        }
    };
    reported
        .get("apiEndpoint")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!("case {case:?}: {reporter:?} must report an apiEndpoint, got {reported}")
        })
        .to_string()
}

/// The integration a row's `$default` route fronts: a MOCK that synthesizes `200`, so the call
/// has a status for its responses to be selected by without any backend.
fn mock_integration() -> Value {
    json!({
        "integrationType": "MOCK",
        "requestTemplates": { "$default": "{\"statusCode\": 200}" }
    })
}

/// The integration response that `200` selects. It renders the CALL's own `$context` — the api,
/// the stage and the route that served it — so the frame a client reads back NAMES what served
/// it, which is what makes "the announced URI reached this api's deployed stage" an assertion
/// on the answer rather than on the address alone.
fn context_response() -> Value {
    json!({
        "integrationResponseKey": "/2\\d\\d/",
        "responseTemplates": {
            "$default": "{\"apiId\":\"$context.apiId\",\"stage\":\"$context.stage\",\
                          \"routeKey\":\"$context.routeKey\"}"
        }
    })
}

/// The longest `StageName` the control plane accepts: "Stage names can only contain
/// alphanumeric characters, hyphens, and underscores. Maximum length is 128 characters"
/// (`CreateStage$StageName`). A row appends it to the announced URI, so the path the client
/// builds is exercised at the published bound and not only at a short name.
fn longest_stage_name() -> String {
    "s".repeat(128)
}

/// One row: the operation the client read the endpoint off, and the stage name it appended.
struct AnnouncedRow {
    case: &'static str,
    reporter: Reporter,
    stage: String,
}

/// Whichever operation reported it, the `ApiEndpoint` of a WEBSOCKET api is the one URI that
/// api is reached at, and a client that takes that URI AS GIVEN, appends the stage name and
/// opens an RFC 6455 handshake there is answered `101` and then exchanges the deployed stage's
/// own frames.
///
/// The rows span what the rule generalises over: which operation reported the value (the
/// create that minted the api, two reads — one of the api and one of the collection — and an
/// update), and the stage name appended to it across the forms a stage name takes
/// (alphanumeric, a hyphen, an underscore, and the published maximum length). The recorded
/// case is ONE row: a client that created the api and read the endpoint off that answer.
#[tokio::test]
async fn every_reporter_announces_the_uri_a_client_dials_and_that_uri_carries_the_frames() {
    let rows = vec![
        AnnouncedRow {
            case: "the recorded case: the client read the endpoint off CreateApi",
            reporter: Reporter::CreateApi,
            stage: "prod".to_string(),
        },
        AnnouncedRow {
            case: "the client read the endpoint back off GetApi, onto a hyphenated stage",
            reporter: Reporter::GetApi,
            stage: "beta-2".to_string(),
        },
        AnnouncedRow {
            case: "the client found the api in GetApis, onto a stage named with an underscore",
            reporter: Reporter::GetApis,
            stage: "Stage_1".to_string(),
        },
        AnnouncedRow {
            case: "the client read the endpoint off UpdateApi, onto the longest stage name",
            reporter: Reporter::UpdateApi,
            stage: longest_stage_name(),
        },
    ];

    for (index, row) in rows.into_iter().enumerate() {
        let case = row.case;
        // One emulator per row, so the collection a row reads holds exactly its own api.
        let (emulator, _engine) = emulator();
        // No `$connect` route, so nothing stands in front of the handshake; one two-way
        // `$default` route whose reply names the api, the stage and the route that served it.
        let (api_id, created_api) = given_socket(
            &emulator,
            &format!("announced-uri-api-{index}"),
            &row.stage,
            vec![Route {
                key: "$default",
                integration: mock_integration(),
                responses: vec![context_response()],
                two_way: true,
            }],
        )
        .await;

        // Every operation that reports the api must report the one announced URI, and the row
        // then dials the one ITS reporter answered.
        let mut reported: Vec<(Reporter, String)> = Vec::new();
        for reporter in REPORTERS {
            let value = announced_by(&emulator, case, reporter, &api_id, &created_api).await;
            if let Some((first, expected)) = reported.first() {
                assert_eq!(
                    &value, expected,
                    "case {case:?}: {reporter:?} must report the one endpoint {first:?} \
                     announced, expected {expected:?}, got {value:?}"
                );
            }
            reported.push((reporter, value));
        }
        let announced = reported
            .iter()
            .find_map(|(reporter, value)| (*reporter == row.reporter).then_some(value.as_str()))
            .unwrap_or_else(|| panic!("case {case:?}: the row's reporter is one of REPORTERS"));

        let mut socket = dial(case, announced, &row.stage).await;
        let text = exchanged(case, &mut socket, &json!({ "action": "ping" })).await;
        let served: Value = serde_json::from_str(&text).unwrap_or_else(|error| {
            panic!("case {case:?}: the reply must be the response's rendered document, got \
                    {text:?} ({error})")
        });
        for (what, member, expected) in [
            ("the api that served the frame", "apiId", api_id.clone()),
            ("the stage it was dispatched on", "stage", row.stage.clone()),
            ("the route that served it", "routeKey", "$default".to_string()),
        ] {
            assert_eq!(
                served.get(member).and_then(Value::as_str),
                Some(expected.as_str()),
                "case {case:?}: expected {what} at {member} to be {expected:?}, got {served}"
            );
        }
    }
}
