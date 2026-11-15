// SPDX-License-Identifier: CC0-1.0

//! The identity-source precondition of a `$connect` route's `REQUEST` authorizer, driven over
//! the api's OWN announced socket by a client that writes the opening handshake's bytes and
//! reads the HTTP response back — the way a WebSocket client library's first exchange looks on
//! the wire.
//!
//! The rule these rows assert, one level above the recorded case (one WebSocket api, one
//! `REQUEST` authorizer over one header, a handshake that omits that header):
//!
//! > A WebSocket opening handshake against a deployed stage is always ANSWERED: either with
//! > the `101` of a completed upgrade or with an HTTP refusal the client can read. When the
//! > `$connect` route that serves the connection request is fronted by an `Authorizer` of the
//! > api, every entry of that authorizer's `IdentitySource` must be present in the request,
//! > not null and non-empty before the authorizer's function is invoked at all; a handshake
//! > that presents one of them absent, null or empty is refused `401`
//! > `{"message":"Unauthorized"}` with no invocation, and one that presents all of them is
//! > past this step, so whatever answers it is not this refusal.
//!
//! That is `Authorizer$IdentitySource` verbatim: "These parameters are used to perform
//! runtime validation for Lambda-based authorizers by verifying all of the identity-related
//! request parameters are present in the request, not null, and non-empty. Only when this is
//! true does the authorizer invoke the authorizer Lambda function. Otherwise, it returns a
//! 401 Unauthorized response without calling the Lambda function."
//! (`smithy_query(query="doc",shape="CreateAuthorizerRequest",member="IdentitySource")`).
//!
//! What a handshake whose identity sources ARE satisfied earns here is the answer of the
//! authorizer's own function, which this emulator holds no deployment package for — these
//! rows mount this service alone — so no decision can be obtained and the gateway answers
//! `500` `{"message":"Internal server error"}`. So a `500` in a row below means "the
//! precondition let this handshake through", and is asserted for exactly that. The suite that
//! drives the same precondition at the SERVICE endpoint is
//! `websocket_connect_authorization.rs`, and the one that drives it with the authorizer's
//! function actually running is `websocket_lambda_routes.rs`; what is asserted here and
//! nowhere else is the raw-wire answer of the announced socket — that a refused handshake is
//! answered at all, with the status line and the body the client reads.
//!
//! The rows span what the rule generalises over: which request part the identity source names
//! (a header, a query-string parameter, a stage variable, a context parameter), how many
//! entries it has (none, one, two), the spelling of the expression (`route.request.…` and the
//! `$request.…` form the same member documents), the case of the name the handshake presents,
//! and the form of the value presented (absent, empty, whitespace only, a value).

use std::time::Duration;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::api_lifecycle::{as_str, call, emulator};
// The key an RFC 6455 client offers, and the one reading of the announced socket's address:
// both already belong to this crate's WebSocket suites, so neither is written again here.
use crate::websocket_handshake_data_plane::CLIENT_KEY;
use crate::websocket_transport_data_plane::dial_loopback;

/// The status and sentence a handshake whose authorizer was not invoked is refused with; see
/// the module docs' quotation of `Authorizer$IdentitySource`.
pub(crate) const UNAUTHORIZED: u16 = 401;
pub(crate) const UNAUTHORIZED_MESSAGE: &str = "Unauthorized";

/// The status and sentence a handshake the precondition LET THROUGH earns here: past it the
/// authorizer's own function decides, and no function runs in this emulator, so the gateway
/// answers for a connection request its configuration cannot be served from.
pub(crate) const LET_THROUGH: u16 = 500;
pub(crate) const LET_THROUGH_MESSAGE: &str = "Internal server error";

/// The stage every row deploys.
pub(crate) const STAGE: &str = "prod";

/// How long a row waits for the socket's answer before failing. Generous — it bounds nothing
/// the rule is about, only a handshake that will never be answered, which is the defect this
/// suite pins — and no row asserts anything about how long an answer took.
const ANSWER_DEADLINE: Duration = Duration::from_secs(60);

/// A well-formed Lambda authorizer URI, the form `AuthorizerUri` documents: "For REQUEST
/// authorizers, this must be a well-formed Lambda function URI, for example,
/// arn:aws:apigateway:{region}:lambda:path/2015-03-31/functions/{function-arn}/invocations".
fn lambda_uri(region: &str, function: &str) -> String {
    format!(
        "arn:aws:apigateway:{region}:lambda:path/2015-03-31/functions/\
         arn:aws:lambda:{region}:123456789012:function:{function}/invocations"
    )
}

/// One api, deployed, with the socket its `ApiEndpoint` announces.
pub(crate) struct Deployed {
    /// The `host:port` of the api's own socket, read off the announced `ApiEndpoint`.
    pub(crate) authority: String,
}

/// Create a WEBSOCKET api whose `$connect` route is fronted by one `REQUEST` authorizer over
/// `identity_sources`, with a stage carrying `stage_variables` and a deployment released to
/// it.
///
/// Every create is read back through its own `Get…` before a handshake is sent, so the data
/// plane answers from the records a later caller reads and not from an echo of the request.
pub(crate) async fn deploy(
    emulator: &MultiServiceRouter,
    case: &str,
    identity_sources: &[&str],
    stage_variables: &[(&str, &str)],
) -> Deployed {
    let (status, created) = call(
        emulator,
        "POST",
        "/v2/apis",
        Some(json!({
            "name": "identity-source-socket-api",
            "protocolType": "WEBSOCKET",
            "routeSelectionExpression": "$request.body.action"
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateApi must answer 201, got {created}"
    );
    let api_id = as_str(&created, "apiId");
    let announced = as_str(&created, "apiEndpoint");
    let authority = announced
        .strip_prefix("wss://")
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: a WEBSOCKET api must announce a wss:// socket, got \
                 {announced:?}"
            )
        })
        .to_string();

    let (status, authorizer) = call(
        emulator,
        "POST",
        &format!("/v2/apis/{api_id}/authorizers"),
        Some(json!({
            "name": "connect-auth",
            "authorizerType": "REQUEST",
            "authorizerUri": lambda_uri(crate::api_lifecycle::REGION, "connect-auth"),
            "identitySource": identity_sources
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateAuthorizer must answer 201, got {authorizer}"
    );
    let authorizer_id = as_str(&authorizer, "authorizerId");
    let (status, fetched) = call(
        emulator,
        "GET",
        &format!("/v2/apis/{api_id}/authorizers/{authorizer_id}"),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetAuthorizer must answer 200, got {fetched}"
    );
    assert_eq!(
        fetched.get("identitySource"),
        Some(&json!(identity_sources)),
        "case {case:?}: the stored authorizer must report the identity sources it was created \
         with, got {fetched}"
    );

    let (status, route) = call(
        emulator,
        "POST",
        &format!("/v2/apis/{api_id}/routes"),
        Some(json!({
            "routeKey": "$connect",
            "authorizationType": "CUSTOM",
            "authorizerId": authorizer_id,
            "target": format!(
                "integrations/{}",
                crate::api_lifecycle::mock_integration(emulator, case, &api_id).await
            )
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateRoute must answer 201, got {route}"
    );
    let route_id = as_str(&route, "routeId");
    let (status, fetched_route) = call(
        emulator,
        "GET",
        &format!("/v2/apis/{api_id}/routes/{route_id}"),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetRoute must answer 200, got {fetched_route}"
    );
    assert_eq!(
        as_str(&fetched_route, "authorizerId"),
        authorizer_id,
        "case {case:?}: the stored route must name the authorizer that decides it, got \
         {fetched_route}"
    );

    let variables: Value = stage_variables
        .iter()
        .map(|(name, value)| ((*name).to_string(), json!(value)))
        .collect::<serde_json::Map<String, Value>>()
        .into();
    let (status, stage) = call(
        emulator,
        "POST",
        &format!("/v2/apis/{api_id}/stages"),
        Some(json!({ "stageName": STAGE, "stageVariables": variables })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateStage must answer 201, got {stage}"
    );
    let (status, deployment) = call(
        emulator,
        "POST",
        &format!("/v2/apis/{api_id}/deployments"),
        Some(json!({ "stageName": STAGE })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateDeployment must answer 201, got {deployment}"
    );
    let (status, fetched_stage) = call(
        emulator,
        "GET",
        &format!("/v2/apis/{api_id}/stages/{STAGE}"),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetStage must answer 200, got {fetched_stage}"
    );
    assert!(
        !as_str(&fetched_stage, "deploymentId").is_empty(),
        "case {case:?}: the stage must carry the released deployment, got {fetched_stage}"
    );

    Deployed { authority }
}

/// What the socket answered one opening handshake with, as the client read it off the wire.
pub(crate) struct Answered {
    pub(crate) status: u16,
    /// The response head as written, status line and fields.
    pub(crate) head: String,
    /// Whatever followed the head.
    pub(crate) body: String,
}

impl Answered {
    /// The `message` member of the body, or `None` when the body is not a JSON object naming
    /// one.
    pub(crate) fn message(&self) -> Option<String> {
        serde_json::from_str::<Value>(&self.body)
            .ok()
            .as_ref()
            .and_then(|document| document.get("message"))
            .and_then(Value::as_str)
            .map(str::to_string)
    }
}

/// Write one RFC 6455 opening handshake to the api's announced socket and read the HTTP
/// response it is answered with.
///
/// The bytes are written and read directly, rather than through a client library, because what
/// this suite pins is that the handshake is ANSWERED at all: a client that gets no status line
/// cannot tell a refused upgrade from an endpoint that is not there.
async fn handshake(
    case: &str,
    authority: &str,
    query: &str,
    headers: &[(&str, &str)],
) -> Answered {
    let stream = dial_loopback(case, authority)
        .await
        .unwrap_or_else(|error| {
            panic!(
                "case {case:?}: the announced socket {authority:?} must accept a connection, \
                 got {error}"
            )
        });
    answered_over(case, stream, authority, query, headers).await
}

/// [`handshake`] over a stream the caller has already opened — the same bytes, so the socket's
/// answer is read the same whether the client dialled in the clear or negotiated TLS first
/// (the api's one announced address serves both).
pub(crate) async fn answered_over<S>(
    case: &str,
    mut stream: S,
    authority: &str,
    query: &str,
    headers: &[(&str, &str)],
) -> Answered
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let path = if query.is_empty() {
        format!("/{STAGE}")
    } else {
        format!("/{STAGE}?{query}")
    };
    let mut request = format!("GET {path} HTTP/1.1\r\n");
    for (name, value) in [
        ("host", authority),
        ("connection", "Upgrade"),
        ("upgrade", "websocket"),
        ("sec-websocket-key", CLIENT_KEY),
        ("sec-websocket-version", "13"),
    ]
    .iter()
    .chain(headers.iter())
    {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    request.push_str("\r\n");
    stream
        .write_all(request.as_bytes())
        .await
        .unwrap_or_else(|error| panic!("case {case:?}: the handshake must be sent, got {error}"));
    stream
        .flush()
        .await
        .unwrap_or_else(|error| panic!("case {case:?}: the handshake must be flushed, got {error}"));

    let bytes = tokio::time::timeout(ANSWER_DEADLINE, read_answer(case, &mut stream))
        .await
        .unwrap_or_else(|_elapsed| {
            panic!(
                "case {case:?}: the opening handshake must be answered — a client that reads no \
                 status at all cannot tell a refusal from an endpoint that serves nothing"
            )
        });
    let answered = String::from_utf8_lossy(&bytes).to_string();
    let (head, body) = answered
        .split_once("\r\n\r\n")
        .unwrap_or_else(|| panic!("case {case:?}: expected an HTTP response, got {answered:?}"));
    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected a status line, got {head:?}")
        });
    Answered {
        status,
        head: head.to_string(),
        body: body.to_string(),
    }
}

/// Read until the answer is complete: the response head, and the body that follows it when the
/// server writes one, up to the end of what it sends.
async fn read_answer<S>(case: &str, stream: &mut S) -> Vec<u8>
where
    S: tokio::io::AsyncRead + Unpin,
{
    let mut bytes: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        match stream.read(&mut chunk).await {
            Ok(0) => break,
            Ok(read) => bytes.extend_from_slice(&chunk[..read]),
            Err(error) => {
                // A transport failure BEFORE the answer was read is a failure of the case
                // and is reported as one; after it, the server has merely finished writing.
                assert!(
                    answered_in_full(&bytes),
                    "case {case:?}: the connection failed before the handshake was answered: \
                     {error}"
                );
                break;
            }
        }
        if answered_in_full(&bytes) {
            break;
        }
    }
    bytes
}

/// Whether `bytes` carry a whole answer, so there is nothing left to wait for.
///
/// The head decides: a `Content-Length` field says how many body bytes follow (zero included,
/// which is a bodyless answer — a `101` among them), and an answer that declares none is whole
/// once a complete JSON document has arrived, which is what a refusal the gateway states
/// carries. Anything else is read on until the server has finished writing, so no answer is
/// ever read as half of itself.
fn answered_in_full(bytes: &[u8]) -> bool {
    let text = String::from_utf8_lossy(bytes);
    text.split_once("\r\n\r\n").is_some_and(|(head, body)| {
        let declared = head
            .lines()
            .filter_map(|line| line.split_once(':'))
            .find(|(name, _)| name.trim().eq_ignore_ascii_case("content-length"))
            .and_then(|(_, value)| value.trim().parse::<usize>().ok());
        match declared {
            Some(length) => body.len() >= length,
            None => !body.is_empty() && serde_json::from_str::<Value>(body).is_ok(),
        }
    })
}

/// One row: the authorizer's identity sources, the stage's variables, what the handshake
/// presents, and the answer the precondition owes it.
struct Row {
    case: &'static str,
    identity_sources: &'static [&'static str],
    stage_variables: &'static [(&'static str, &'static str)],
    /// The raw query string the handshake carries.
    query: &'static str,
    /// The header fields the handshake carries beyond the RFC 6455 ones.
    headers: &'static [(&'static str, &'static str)],
    /// The status and sentence the socket must answer with.
    expected: (u16, &'static str),
}

#[tokio::test]
async fn the_announced_socket_answers_a_handshake_missing_an_identity_source_with_401() {
    // The four request parts an identity source may name are the ones
    // `Authorizer$IdentitySource` lists — "the identity source can be headers, query string
    // parameters, stage variables, and context parameters" — and the two spellings are the
    // same member's `route.request.header.Auth` and `$request.header.Auth`.
    let rows: &[Row] = &[
        Row {
            case: "one header source the handshake omits",
            identity_sources: &["route.request.header.Auth"],
            stage_variables: &[],
            query: "",
            headers: &[],
            expected: (UNAUTHORIZED, UNAUTHORIZED_MESSAGE),
        },
        Row {
            case: "one header source presented empty, which is not non-empty",
            identity_sources: &["route.request.header.Auth"],
            stage_variables: &[],
            query: "",
            headers: &[("Auth", "")],
            expected: (UNAUTHORIZED, UNAUTHORIZED_MESSAGE),
        },
        Row {
            case: "one header source presented as whitespace, which is empty too",
            identity_sources: &["route.request.header.Auth"],
            stage_variables: &[],
            query: "",
            headers: &[("Auth", "   ")],
            expected: (UNAUTHORIZED, UNAUTHORIZED_MESSAGE),
        },
        Row {
            case: "one header source presented under another case, which a header name is \
                   read in",
            identity_sources: &["route.request.header.Auth"],
            stage_variables: &[],
            query: "",
            headers: &[("AUTH", "let-me-in")],
            expected: (LET_THROUGH, LET_THROUGH_MESSAGE),
        },
        Row {
            case: "one query-string source the handshake omits",
            identity_sources: &["route.request.querystring.token"],
            stage_variables: &[],
            query: "",
            headers: &[],
            expected: (UNAUTHORIZED, UNAUTHORIZED_MESSAGE),
        },
        Row {
            case: "one query-string source present with no value",
            identity_sources: &["route.request.querystring.token"],
            stage_variables: &[],
            query: "token=",
            headers: &[],
            expected: (UNAUTHORIZED, UNAUTHORIZED_MESSAGE),
        },
        Row {
            case: "one query-string source presented",
            identity_sources: &["route.request.querystring.token"],
            stage_variables: &[],
            query: "token=abc",
            headers: &[],
            expected: (LET_THROUGH, LET_THROUGH_MESSAGE),
        },
        Row {
            case: "two sources of which only one is presented: the precondition is \
                   conjunctive",
            identity_sources: &[
                "route.request.header.Auth",
                "route.request.querystring.token",
            ],
            stage_variables: &[],
            query: "",
            headers: &[("Auth", "let-me-in")],
            expected: (UNAUTHORIZED, UNAUTHORIZED_MESSAGE),
        },
        Row {
            case: "two sources, both presented",
            identity_sources: &[
                "route.request.header.Auth",
                "route.request.querystring.token",
            ],
            stage_variables: &[],
            query: "token=abc",
            headers: &[("Auth", "let-me-in")],
            expected: (LET_THROUGH, LET_THROUGH_MESSAGE),
        },
        Row {
            case: "the $request spelling of a header source, omitted",
            identity_sources: &["$request.header.Auth"],
            stage_variables: &[],
            query: "",
            headers: &[],
            expected: (UNAUTHORIZED, UNAUTHORIZED_MESSAGE),
        },
        Row {
            case: "the $request spelling of a header source, presented",
            identity_sources: &["$request.header.Auth"],
            stage_variables: &[],
            query: "",
            headers: &[("Auth", "let-me-in")],
            expected: (LET_THROUGH, LET_THROUGH_MESSAGE),
        },
        Row {
            case: "a stage-variable source the serving stage does not carry",
            identity_sources: &["stagevariables.tenant"],
            stage_variables: &[],
            query: "",
            headers: &[],
            expected: (UNAUTHORIZED, UNAUTHORIZED_MESSAGE),
        },
        Row {
            case: "a stage-variable source the serving stage carries",
            identity_sources: &["stagevariables.tenant"],
            stage_variables: &[("tenant", "blue")],
            query: "",
            headers: &[],
            expected: (LET_THROUGH, LET_THROUGH_MESSAGE),
        },
        Row {
            case: "a context source, which the service computes for every call",
            identity_sources: &["context.identity.sourceIp"],
            stage_variables: &[],
            query: "",
            headers: &[],
            expected: (LET_THROUGH, LET_THROUGH_MESSAGE),
        },
        Row {
            case: "no identity source at all: there is no precondition to fail",
            identity_sources: &[],
            stage_variables: &[],
            query: "",
            headers: &[],
            expected: (LET_THROUGH, LET_THROUGH_MESSAGE),
        },
    ];

    let emulator = emulator();
    for row in rows {
        let case = row.case;
        let deployed = deploy(&emulator, case, row.identity_sources, row.stage_variables).await;
        let answered = handshake(case, &deployed.authority, row.query, row.headers).await;
        let (status, message) = row.expected;
        assert_eq!(
            answered.status, status,
            "case {case:?}: expected the handshake answered {status}, got {} with head {:?}",
            answered.status, answered.head
        );
        // No row upgrades: the refusal is answered instead, so the field RFC 6455 §4.2.2
        // requires of a completed handshake is absent from what the client read.
        assert!(
            !answered
                .head
                .to_ascii_lowercase()
                .contains("sec-websocket-accept"),
            "case {case:?}: a refused handshake carries no accept field, got {:?}",
            answered.head
        );
        assert_eq!(
            answered.message().as_deref(),
            Some(message),
            "case {case:?}: expected the body to word the refusal {message:?}, got {:?}",
            answered.body
        );
    }
}
