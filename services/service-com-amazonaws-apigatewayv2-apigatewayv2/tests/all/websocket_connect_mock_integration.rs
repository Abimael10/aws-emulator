// SPDX-License-Identifier: CC0-1.0

//! WHAT MAY DECIDE a WEBSOCKET api's CONNECTION REQUEST, driven over the socket the api
//! announces: the `$connect` route bound to the LOOPBACK integration type, and the same
//! integration serving every other route of the same api.
//!
//! The rule these cases assert, one level above the recorded case (one WebSocket api, one MOCK
//! integration with an integration response, a `$connect` route and a `whoami` route over it,
//! one deployed stage):
//!
//! > **A WEBSOCKET connection request is admitted or refused by the reply the `$connect`
//! > route's integration produced, and that reply is a BACKEND's: every `IntegrationType` but
//! > one names a backend the gateway calls and reads an answer back from. `MOCK` is the
//! > exception — "for integrating the route or method request with API Gateway as a 'loopback'
//! > endpoint without invoking any backend" — so a `$connect` route bound to one has no
//! > backend to decide the connection with: the opening handshake is REFUSED with the
//! > gateway's own `500`, the connection is not switched to the WebSocket protocol, and that
//! > holds however the integration's request templates and integration responses are written.
//! > The refusal is of the PAIRING and not of the type: the same loopback integration goes on
//! > serving every route that is not `$connect`, and a `$connect` route re-pointed at an
//! > integration that does name a backend upgrades on that backend's own `2xx`.**
//!
//! `MOCK` is quoted from `Integration$IntegrationType`, which also gives `AWS_PROXY` as "for
//! integrating the route or method request with a Lambda function or other AWS service
//! action" (`smithy_query(query="shape", shape="Integration")`). Neither the model nor its
//! documentation ties the two to a route key — `Route$RouteKey` and `Integration` are
//! independent resources, and the opening handshake is no modelled operation at all — so what
//! the rows below are a slice of is the recorded behaviour of the `execute-api` data plane.
//!
//! The rows span what the rule generalises over: whether the loopback integration carries a
//! request template at all, the status that template renders (a `2xx`, a `4xx`, a quoted
//! status), the key the template map is keyed by (`$default`, the JSON media type), how many
//! integration responses it holds and in which key form (none, `$default`, a `/2\d\d/`
//! pattern, a `/4\d{2}/` pattern, a pattern matching one status), and whether the selected
//! response renders a payload. Every one of them is a handshake REFUSED on `$connect` and a
//! route SERVED anywhere else.
//!
//! The handshake's other statuses (an undeployed stage, a stage the api does not hold) are
//! `websocket_transport_data_plane.rs`'s, the authorization in front of `$connect` is
//! `websocket_connect_authorization.rs`'s, and the answers a `$connect` route fronting a
//! FUNCTION earns are `websocket_lambda_routes.rs`'s; none is restated here.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::api_lifecycle::{as_str, call, emulator};
use crate::http_api_backend_dispatch::{emulator_with_lambda, given_function};
use crate::websocket_transport_data_plane::{
    create_api, create_integration, create_integration_response, create_route, create_stage,
    exchanged_reply, handshake_at,
};

/// The stage every api below releases its deployment to.
const STAGE: &str = "prod";

/// The status a completed opening handshake carries (RFC 6455 §4.1).
const SWITCHING_PROTOCOLS: u16 = 101;

/// The status the gateway refuses an opening handshake whose connection request nothing
/// decided. Fixed text: it names nothing of the request.
const INTERNAL_SERVER_ERROR: u16 = 500;

/// The `RouteKey` the rows' non-`$connect` route carries: a selector value of the api's
/// `RouteSelectionExpression`, which is what a message addresses it by.
const MESSAGE_ROUTE_KEY: &str = "whoami";

/// One row: a loopback integration, in one of the shapes its records take.
struct Row {
    case: &'static str,
    /// The `CreateIntegration` body the integration is created with.
    integration: Value,
    /// The `CreateIntegrationResponse` bodies it then holds.
    responses: Vec<Value>,
    /// The payload a MESSAGE served by this integration reads back, when its records render
    /// one — the reply the selected response's own `ResponseTemplates` entry renders.
    message_reply: Option<&'static str>,
}

/// Every shape a loopback integration's records take, as the module docs lay the span out.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a loopback with no request template and no integration response",
            integration: json!({ "integrationType": "MOCK" }),
            responses: Vec::new(),
            message_reply: None,
        },
        Row {
            case: "a loopback with no request template and a $default integration response",
            integration: json!({ "integrationType": "MOCK" }),
            responses: vec![json!({
                "integrationResponseKey": "$default",
                "responseTemplates": { "$default": r#"{"served":"no-template"}"# }
            })],
            message_reply: Some(r#"{"served":"no-template"}"#),
        },
        Row {
            // The documented form of a loopback's request template, and the patterned key
            // the status it renders selects: the configuration a client writes when it means
            // the connection to be admitted.
            case: "a loopback whose template renders a 2xx and holds the /2\\d\\d/ response \
                   that status selects",
            integration: json!({
                "integrationType": "MOCK",
                "requestTemplates": { "$default": "{\"statusCode\": 200}" }
            }),
            responses: vec![json!({ "integrationResponseKey": "/2\\d\\d/" })],
            message_reply: None,
        },
        Row {
            case: "a loopback whose template renders a 2xx and whose $default response \
                   renders a payload",
            integration: json!({
                "integrationType": "MOCK",
                "requestTemplates": { "$default": "{\"statusCode\": 200}" }
            }),
            responses: vec![json!({
                "integrationResponseKey": "$default",
                "responseTemplates": { "$default": r#"{"served":"default-key"}"# }
            })],
            message_reply: Some(r#"{"served":"default-key"}"#),
        },
        Row {
            case: "a loopback whose template is keyed by the JSON media type",
            integration: json!({
                "integrationType": "MOCK",
                "requestTemplates": { "application/json": "{\"statusCode\": 200}" }
            }),
            responses: vec![json!({
                "integrationResponseKey": "/2\\d\\d/",
                "responseTemplates": { "$default": r#"{"served":"media-type-key"}"# }
            })],
            message_reply: Some(r#"{"served":"media-type-key"}"#),
        },
        Row {
            case: "a loopback whose template renders a quoted status and holds the pattern \
                   that status selects",
            integration: json!({
                "integrationType": "MOCK",
                "requestTemplates": { "$default": "{\"statusCode\": \"201\"}" }
            }),
            responses: vec![json!({
                // `IntegrationResponseKey` is "a regular expression pattern of format
                // '/<pattern>/' or … $default", which the control plane enforces, so the
                // status a literal key would name is written as the pattern that matches it.
                "integrationResponseKey": "/201/",
                "responseTemplates": { "$default": r#"{"served":"quoted-status"}"# }
            })],
            message_reply: Some(r#"{"served":"quoted-status"}"#),
        },
        Row {
            case: "a loopback whose template renders a 4xx and holds the /4\\d{2}/ response \
                   and a $default one",
            integration: json!({
                "integrationType": "MOCK",
                "requestTemplates": { "$default": "{\"statusCode\": 404}" }
            }),
            responses: vec![
                json!({
                    "integrationResponseKey": "/4\\d{2}/",
                    "responseTemplates": { "$default": r#"{"served":"not-found"}"# }
                }),
                json!({ "integrationResponseKey": "$default" }),
            ],
            message_reply: Some(r#"{"served":"not-found"}"#),
        },
    ]
}

/// Create the row's integration on `api_id`, with the integration responses it holds, and
/// answer its `IntegrationId`.
async fn loopback_integration(
    emulator: &MultiServiceRouter,
    row: &Row,
    api_id: &str,
) -> String {
    let integration_id =
        create_integration(emulator, row.case, api_id, row.integration.clone()).await;
    for response in &row.responses {
        create_integration_response(
            emulator,
            row.case,
            api_id,
            &integration_id,
            response.clone(),
        )
        .await;
    }
    integration_id
}

/// Every shape of loopback integration, bound to `$connect` on a deployed stage: the opening
/// handshake is refused with the gateway's own status and the connection is not switched.
#[tokio::test]
async fn a_connect_route_bound_to_a_loopback_integration_refuses_the_handshake() {
    for row in rows() {
        let emulator = emulator();
        let api = create_api(&emulator, row.case, "connect-loopback-api").await;
        let connect = loopback_integration(&emulator, &row, &api.api_id).await;
        create_route(&emulator, row.case, &api.api_id, "$connect", &connect, false).await;
        // A second route over its OWN loopback integration, so the api under test is the
        // recorded one: a `$connect` route and a message route, both loopback-backed.
        let message = loopback_integration(&emulator, &row, &api.api_id).await;
        create_route(
            &emulator,
            row.case,
            &api.api_id,
            MESSAGE_ROUTE_KEY,
            &message,
            true,
        )
        .await;
        create_stage(&emulator, row.case, &api.api_id, STAGE, true).await;

        let answered = handshake_at(row.case, &api.authority, &format!("/{STAGE}")).await;
        match answered {
            Err(status) => assert_eq!(
                status, INTERNAL_SERVER_ERROR,
                "case {:?}: a $connect route whose integration invokes no backend decides no \
                 connection request, so the handshake must be refused \
                 {INTERNAL_SERVER_ERROR}, got {status}",
                row.case
            ),
            Ok(_socket) => panic!(
                "case {:?}: expected the handshake refused {INTERNAL_SERVER_ERROR} with no \
                 upgrade, got {SWITCHING_PROTOCOLS} and a switched connection",
                row.case
            ),
        }
    }
}

/// The other half of the rule: each of those same integrations, bound to a route that is NOT
/// the connection request's, goes on serving — the handshake completes and the message reads
/// back the payload that integration's selected response renders.
#[tokio::test]
async fn the_same_loopback_integration_serves_every_route_that_is_not_connect() {
    for row in rows() {
        let emulator = emulator();
        let api = create_api(&emulator, row.case, "message-loopback-api").await;
        // The SAME integration the row bound to `$connect` above, on a route that is not the
        // connection request's — and no `$connect` route at all, so nothing stands in front
        // of the handshake.
        let message = loopback_integration(&emulator, &row, &api.api_id).await;
        create_route(
            &emulator,
            row.case,
            &api.api_id,
            MESSAGE_ROUTE_KEY,
            &message,
            true,
        )
        .await;
        create_stage(&emulator, row.case, &api.api_id, STAGE, true).await;

        let mut socket = handshake_at(row.case, &api.authority, &format!("/{STAGE}"))
            .await
            .unwrap_or_else(|status| {
                panic!(
                    "case {:?}: an api holding no $connect route has nothing to refuse the \
                     connection, so the handshake must complete {SWITCHING_PROTOCOLS}, got a \
                     handshake refused {status}",
                    row.case
                )
            });
        let Some(expected) = row.message_reply else {
            continue;
        };
        let answered = exchanged_reply(
            row.case,
            &mut socket,
            &json!({ "action": MESSAGE_ROUTE_KEY }).to_string(),
        )
        .await;
        assert_eq!(
            answered, expected,
            "case {:?}: expected the payload this loopback's selected response renders \
             ({expected:?}), got {answered:?}",
            row.case
        );
    }
}

/// The `$connect` handler the re-pointed route fronts: it admits every connection request,
/// which is the `2xx` a backend decides the handshake with.
const ADMITTING_HANDLER: &str = "def lambda_handler(event, context):\n    \
                                 return {'statusCode': 200}\n";

/// How long the re-pointed leg keeps offering the handshake, and how often: a loaded host can
/// answer the gateway's own `500` while the function's runtime is still starting, so the wait
/// is for the UPGRADE — a condition — and nothing here asserts on the clock. The span is
/// generous and bounds no behaviour this case asserts on.
const DECISION_WAIT: std::time::Duration = std::time::Duration::from_secs(120);
const LOOK_INTERVAL: std::time::Duration = std::time::Duration::from_millis(100);

/// The recorded control, on ONE api: the handshake its loopback-backed `$connect` route is
/// refused, and the handshake the SAME api answers once that route is re-pointed at an
/// integration that names a backend.
///
/// It is what isolates the refusal to the integration's own type: nothing of the api, the
/// stage, the route key or the client changes between the two legs.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_connect_route_re_pointed_at_a_backend_upgrades_the_same_handshake() {
    let case = "one api whose $connect route is re-pointed from a loopback onto a function";
    let (emulator, engine) = emulator_with_lambda();
    let api = create_api(&emulator, case, "connect-re-pointed-api").await;
    let loopback = create_integration(
        &emulator,
        case,
        &api.api_id,
        json!({
            "integrationType": "MOCK",
            "requestTemplates": { "$default": "{\"statusCode\": 200}" }
        }),
    )
    .await;
    create_integration_response(
        &emulator,
        case,
        &api.api_id,
        &loopback,
        json!({ "integrationResponseKey": "/2\\d\\d/" }),
    )
    .await;
    // The route is created here rather than through the shared helper because the leg below
    // addresses it by the `RouteId` the service minted for it.
    let (status, created) = call(
        &emulator,
        "POST",
        &format!("/v2/apis/{}/routes", api.api_id),
        Some(json!({
            "routeKey": "$connect",
            "target": format!("integrations/{loopback}")
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateRoute must answer 201, got {created}"
    );
    let route_id = as_str(&created, "routeId");
    create_stage(&emulator, case, &api.api_id, STAGE, true).await;

    let refused = handshake_at(case, &api.authority, &format!("/{STAGE}")).await;
    match refused {
        Err(status) => assert_eq!(
            status, INTERNAL_SERVER_ERROR,
            "case {case:?}: the loopback-backed $connect route must have its handshake \
             refused {INTERNAL_SERVER_ERROR}, got {status}"
        ),
        Ok(_socket) => panic!(
            "case {case:?}: expected the loopback-backed $connect route's handshake refused \
             {INTERNAL_SERVER_ERROR}, got {SWITCHING_PROTOCOLS}"
        ),
    }

    // The same api, with `$connect` re-pointed at a function and the deployment that freezes
    // it released to the same stage.
    let function =
        given_function(&emulator, &engine, case, "connect-admitting", ADMITTING_HANDLER).await;
    let proxy = create_integration(
        &emulator,
        case,
        &api.api_id,
        json!({ "integrationType": "AWS_PROXY", "integrationUri": function }),
    )
    .await;
    let (status, patched) = call(
        &emulator,
        "PATCH",
        &format!("/v2/apis/{}/routes/{route_id}", api.api_id),
        Some(json!({ "target": format!("integrations/{proxy}") })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: UpdateRoute must answer 201, got {patched}"
    );
    let (status, deployment) = call(
        &emulator,
        "POST",
        &format!("/v2/apis/{}/deployments", api.api_id),
        Some(json!({ "stageName": STAGE })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateDeployment must answer 201, got {deployment}"
    );

    // The upgrade the backend's own `2xx` earns, offered until it arrives: the first
    // invocation of a function starts its runtime, and until it has answered the gateway has
    // no decision to admit the connection on.
    let deadline = std::time::Instant::now() + DECISION_WAIT;
    loop {
        match handshake_at(case, &api.authority, &format!("/{STAGE}")).await {
            Ok(_socket) => break,
            Err(status) => {
                assert!(
                    status == INTERNAL_SERVER_ERROR && std::time::Instant::now() < deadline,
                    "case {case:?}: a $connect route whose function admits the connection must \
                     complete the handshake {SWITCHING_PROTOCOLS}, got a handshake refused \
                     {status}"
                );
                tokio::time::sleep(LOOK_INTERVAL).await;
            }
        }
    }
}
