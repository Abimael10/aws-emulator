// SPDX-License-Identifier: CC0-1.0

//! What a WEBSOCKET stage's access-log record of a CONNECTION REQUEST reports as
//! `$context.status`.
//!
//! The rule under test, one level above the recorded case:
//!
//! > An access-log record's `$context.status` is the status of the ROUTE's response to the
//! > call the record is about. For a connection request that is the status the `$connect`
//! > route's integration answered — whatever it is: a `2xx` admits the connection and is the
//! > status logged for it, a status outside `2xx` refuses the handshake and is the status
//! > logged for it — and, for an api holding no `$connect` route at all, the success of a
//! > route execution nothing refused. The `101` of the RFC 6455 opening handshake is the
//! > TRANSPORT's switch-protocols status, is no route's response, and is never what a record
//! > reports.
//!
//! Only a cross-service suite can pin this: the api, its stage and that stage's logging
//! configuration are this service's, the `$connect` route's answer belongs to a function AWS
//! Lambda holds and runs, and the group the records land in is CloudWatch Logs' — all three
//! on ONE engine. Every assertion reads the record back through CloudWatch Logs' own reader,
//! so a record that did not reach that store cannot pass, and the session itself is driven
//! over a REAL connection to the socket the api announces.
//!
//! The rows differ along what the rule generalises over: the STATUS the `$connect` route
//! answers — each class a reply can name, both bounds of the success class the admission rule
//! reads (`src/service/websocket/dispatch.rs::is_success_status`) and the first status past
//! it — and so along the OUTCOME the client reads, a completed upgrade or a refusal. The api
//! that holds no `$connect` route at all is the second test below.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::api_lifecycle::{as_str, call_in_region};
use crate::http_api_access_logging::{create_log_group, delivered_to, emulator, log_group_arn};
use crate::http_api_backend_dispatch::given_function;
use crate::websocket_transport_data_plane::{handshake_at, ClientSocket};

/// The Region every request below is signed for and every ARN names.
const REGION: &str = "us-east-1";

/// The stage each case deploys.
const STAGE: &str = "prod";

/// The format each stage renders its records by: the members a connection request's record is
/// read by here, with the `$context.requestId` every `AccessLogSettings` format must name.
const FORMAT: &str = concat!(
    r#"{"requestId":"$context.requestId","eventType":"$context.eventType","#,
    r#""routeKey":"$context.routeKey","status":"$context.status"}"#
);

/// How long a case waits for the records of a session, and how often it looks: a record is
/// delivered after the call it is about was answered, so it is read by polling the group
/// rather than by waiting a fixed time.
const RECORD_WAIT: Duration = Duration::from_secs(60);
const LOOK_INTERVAL: Duration = Duration::from_millis(50);

/// The query-string parameter the `$connect` handler below reads the status it answers from,
/// so ONE function serves every row: a connection request's event carries the handshake's
/// query string (`$connect` is the only call that does), which is what lets each row ask for
/// its own status.
const STATUS_PARAMETER: &str = "answer";

/// The `$connect` handler every row shares: it answers the status that row's handshake asked
/// for, which is what makes the record's status assertable against the ROUTE's own answer.
fn answering_handler() -> String {
    format!(
        "import json\n\
         \n\
         \n\
         def lambda_handler(event, context):\n\
         \x20   asked = (event.get(\"queryStringParameters\") or {{}}).get(\"{STATUS_PARAMETER}\")\n\
         \x20   return {{\"statusCode\": int(asked),\n\
         \x20           \"body\": json.dumps({{\"answered\": asked}})}}\n"
    )
}

/// One control-plane request that must answer `201`.
async fn created(
    router: &MultiServiceRouter,
    case: &str,
    what: &str,
    uri: &str,
    body: Value,
) -> Value {
    let (status, answered) = call_in_region(router, REGION, "POST", uri, Some(body)).await;
    assert_eq!(
        status, 201,
        "case {case:?}: {what} must answer 201, got {status} {answered}"
    );
    answered
}

/// A deployed WEBSOCKET api whose stage logs into `group` by [`FORMAT`], with one route per
/// `(route_key, function)` pair — so a case can deploy an api with a `$connect` route and one
/// without. Answers the `host:port` of the socket its `ApiEndpoint` announces, which is what
/// a case dials.
async fn given_api(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    routes: &[(&str, &str)],
) -> String {
    let api = created(
        router,
        case,
        "CreateApi",
        "/v2/apis",
        json!({
            "name": "connect-status-logged-api",
            "protocolType": "WEBSOCKET",
            "routeSelectionExpression": "$request.body.action"
        }),
    )
    .await;
    let api_id = as_str(&api, "apiId");
    let announced = as_str(&api, "apiEndpoint");
    let authority = announced
        .strip_prefix("wss://")
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: a WEBSOCKET api must announce a wss:// socket, got {announced:?}"
            )
        })
        .to_string();
    for (route_key, function) in routes {
        let integration = created(
            router,
            case,
            "CreateIntegration",
            &format!("/v2/apis/{api_id}/integrations"),
            json!({
                "integrationType": "AWS_PROXY",
                "integrationUri": function,
                "integrationMethod": "POST"
            }),
        )
        .await;
        let integration_id = as_str(&integration, "integrationId");
        created(
            router,
            case,
            "CreateRoute",
            &format!("/v2/apis/{api_id}/routes"),
            json!({
                "routeKey": route_key,
                "target": format!("integrations/{integration_id}")
            }),
        )
        .await;
    }
    // The destination is a log group of the ACCOUNT's: it exists before the stage is pointed
    // at it, because a delivery writes into a group that is there and creates none.
    create_log_group(router, group).await;
    created(
        router,
        case,
        "CreateStage",
        &format!("/v2/apis/{api_id}/stages"),
        json!({
            "stageName": STAGE,
            "accessLogSettings": {
                "destinationArn": log_group_arn(group),
                "format": FORMAT,
            }
        }),
    )
    .await;
    created(
        router,
        case,
        "CreateDeployment",
        &format!("/v2/apis/{api_id}/deployments"),
        json!({ "stageName": STAGE }),
    )
    .await;
    authority
}

/// Look again until `group` holds at least `expected` records, or the wait above runs out.
async fn records_once_at_least(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    expected: usize,
) -> Vec<String> {
    let deadline = Instant::now() + RECORD_WAIT;
    let mut records = delivered_to(router, group).await.unwrap_or_default();
    while records.len() < expected && Instant::now() < deadline {
        tokio::time::sleep(LOOK_INTERVAL).await;
        records = delivered_to(router, group).await.unwrap_or_default();
    }
    assert!(
        records.len() >= expected,
        "case {case:?}: expected at least {expected} record(s) in {group:?}, got {records:?}"
    );
    records
}

/// One record, read back as the JSON [`FORMAT`] renders.
fn parsed(case: &str, record: &str) -> Value {
    serde_json::from_str(record).unwrap_or_else(|error| {
        panic!("case {case:?}: the record {record:?} is not the JSON the format is: {error}")
    })
}

/// The value `member` carries in a record, as the record's own text: the id of the call is a
/// per-run value, so a record is read for it rather than asserted against a literal.
fn as_text(record: &Value, member: &str) -> String {
    record
        .get(member)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// One row: the status the `$connect` route answers, and whether the client's upgrade is
/// completed on it.
struct Row {
    case: &'static str,
    /// The status that row's `$connect` integration answers.
    answered: u16,
    /// Whether that status admits the connection: the `2xx` class does, every other status
    /// refuses the handshake with itself.
    admits: bool,
}

/// The destination the table below logs into.
const TABLE_GROUP: &str = "/apigwv2/websocket-connect-status";

#[tokio::test]
async fn a_connect_record_reports_the_status_the_connect_route_answered() {
    let case = "a connection request per status the $connect route answers";
    let (router, engine) = emulator();
    let function = given_function(
        &router,
        &engine,
        case,
        "socket-connect-status",
        &answering_handler(),
    )
    .await;
    let authority = given_api(
        &router,
        case,
        TABLE_GROUP,
        &[("$connect", &function), ("$default", &function)],
    )
    .await;

    let rows = [
        Row {
            case: "the success every admitted connection request is answered with",
            answered: 200,
            admits: true,
        },
        Row {
            case: "a success that is not 200",
            answered: 201,
            admits: true,
        },
        Row {
            case: "a success carrying no content",
            answered: 204,
            admits: true,
        },
        Row {
            case: "the last status of the success class",
            answered: 299,
            admits: true,
        },
        Row {
            case: "the first status past the success class",
            answered: 300,
            admits: false,
        },
        Row {
            case: "a backend that denies the client",
            answered: 403,
            admits: false,
        },
        Row {
            case: "a backend that refuses the client's credentials",
            answered: 401,
            admits: false,
        },
        Row {
            case: "a backend that failed",
            answered: 500,
            admits: false,
        },
    ];

    // The rows are driven one after another over the same stage, so the record a row leaves is
    // the one the group's next line is.
    let mut sockets: Vec<ClientSocket> = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let label = row.case;
        let answered = row.answered;
        let opened = handshake_at(
            label,
            &authority,
            &format!("/{STAGE}?{STATUS_PARAMETER}={answered}"),
        )
        .await;
        match (&opened, row.admits) {
            (Ok(_), true) | (Err(_), false) => {}
            (Ok(_), false) => panic!(
                "case {label:?}: a {answered} from the $connect route must refuse the handshake, \
                 got a completed upgrade"
            ),
            (Err(refused), true) => panic!(
                "case {label:?}: a {answered} from the $connect route must admit the connection, \
                 got a handshake refused {refused}"
            ),
        }
        if let Err(refused) = &opened {
            assert_eq!(
                *refused, answered,
                "case {label:?}: the handshake must be refused with the status the $connect \
                 route answered, got {refused}"
            );
        }
        // An admitted socket is held open until the end of the case: a close would leave the
        // $disconnect record of this connection between this row's record and the next row's.
        if let Ok(socket) = opened {
            sockets.push(socket);
        }
        let records = records_once_at_least(&router, label, TABLE_GROUP, index + 1).await;
        let record = parsed(label, &records[index]);
        assert_eq!(
            record,
            json!({
                "requestId": as_text(&record, "requestId"),
                "eventType": "CONNECT",
                "routeKey": "$connect",
                "status": answered.to_string(),
            }),
            "case {label:?}: expected the connection request's record to report the status its \
             route answered, got {records:?}"
        );
    }

    // Nothing of the session reports the opening handshake's own status: the `101` is the
    // transport's and is no route's response.
    let records = records_once_at_least(&router, case, TABLE_GROUP, rows.len()).await;
    for record in &records {
        let status = as_text(&parsed(case, record), "status");
        assert_ne!(
            status, "101",
            "case {case:?}: no record reports the upgrade's switch-protocols status, got \
             {record:?}"
        );
    }
}

/// The destination the case below logs into — its own group, so the records of these two apis
/// are never read as one sequence.
const NO_CONNECT_ROUTE_GROUP: &str = "/apigwv2/websocket-connect-status-no-route";

/// An api holding NO `$connect` route has nothing to serve the connection request with and
/// nothing to refuse it: the upgrade is completed, and the record of that call reports the
/// success of a route execution nothing refused rather than the upgrade's own `101`.
#[tokio::test]
async fn a_connect_record_of_an_api_with_no_connect_route_reports_a_succeeded_route() {
    let case = "an api with no $connect route";
    let (router, engine) = emulator();
    let function = given_function(
        &router,
        &engine,
        case,
        "socket-no-connect-route",
        &answering_handler(),
    )
    .await;
    let authority = given_api(
        &router,
        case,
        NO_CONNECT_ROUTE_GROUP,
        &[("$default", &function)],
    )
    .await;

    let _socket: ClientSocket = handshake_at(case, &authority, &format!("/{STAGE}"))
        .await
        .unwrap_or_else(|status| {
            panic!(
                "case {case:?}: an api with no $connect route has nothing to refuse the \
                 connection, got a handshake refused {status}"
            )
        });
    let records = records_once_at_least(&router, case, NO_CONNECT_ROUTE_GROUP, 1).await;
    let record = parsed(case, &records[0]);
    assert_eq!(
        record,
        json!({
            "requestId": as_text(&record, "requestId"),
            "eventType": "CONNECT",
            "routeKey": "$connect",
            "status": "200",
        }),
        "case {case:?}: expected the connection request's record to report a succeeded route, \
         got {records:?}"
    );
}
