// SPDX-License-Identifier: CC0-1.0

//! What a WebSocket api's own announced socket WRITES BACK when the authorization of the
//! `$connect` route decides an opening handshake, read off the wire as a client library's
//! first exchange reads it, with the authorizer's own function really running.
//!
//! The rule these tables assert, one level above the recorded cases (one api, one `REQUEST`
//! authorizer over one query-string parameter, one handshake carrying a token the function
//! denies and one carrying none):
//!
//! > The authorization of the route that serves a connection request is applied BEFORE that
//! > route's integration, and it always ends in a terminal HTTP answer the client can read on
//! > the handshake itself: a handshake that does not present every entry of the authorizer's
//! > `IdentitySource` is refused `401` `{"message":"Unauthorized"}` without the authorizer's
//! > function being invoked at all, and a handshake whose `REQUEST` authorizer's function
//! > stated a decision that does not authorize `execute-api:Invoke` on that route's own ARN is
//! > refused `403` `{"message":"Forbidden"}`. Neither refusal carries the fields RFC 6455
//! > §4.2.2 requires of a completed handshake, and neither reaches the `$connect` route's
//! > integration. Only a handshake the function authorized is completed, and that one performs
//! > the `$connect` route's integration — with the principal the authorizer named — while the
//! > close performs the `$disconnect` route's own.
//!
//! The identity-source half is `Authorizer$IdentitySource` verbatim: "verifying all of the
//! identity-related request parameters are present in the request, not null, and non-empty.
//! Only when this is true does the authorizer invoke the authorizer Lambda function.
//! Otherwise, it returns a 401 Unauthorized response without calling the Lambda function"
//! (`smithy_query(query="doc",shape="CreateAuthorizerRequest",member="IdentitySource")`).
//!
//! What is asserted HERE and nowhere else is the SHAPE of that answer on the wire — the status
//! line with the reason phrase the status registry gives it, the absence of the upgrade fields,
//! and the sentence the body words the refusal in — for a decision an authorizer FUNCTION
//! stated, together with the proof that the `$connect` route's own function did not run for it.
//! A client that reads a server-fault status instead cannot tell a connection it will never be
//! given from a gateway that is unwell, and retries a decision the service made final. The
//! sibling suites own the neighbouring slices and are not restated: the status each kind of
//! stated decision earns is `websocket_lambda_routes.rs`, the identity-source precondition over
//! every spelling an identity source takes is
//! `websocket_connect_identity_source_socket_answer.rs` (whose raw-wire reader and `prod` stage
//! this suite reuses) and `websocket_connect_authorization.rs`, and the handshake's own
//! stage-resolution statuses are `websocket_transport_data_plane.rs`. That first suite's table
//! of stated decisions is the SUPERSET of the decisions driven here (it also covers an allow
//! about another action and a reply stating no decision at all, which the gateway's own `500` is
//! kept for), and it is the suite to extend with a new kind of decision; the rows below are the
//! ones whose answer this suite reads off the wire.
//!
//! Nothing below is a value this emulator fills in: every row's decision is one a Lambda
//! function RAN and returned, which is why AWS Lambda is mounted beside this service on one
//! engine, and every function here — the authorizer's included, which is why it is its own and
//! not the sibling suite's — RECORDS the event it was given in an Amazon SQS queue through the
//! AWS SDK its runtime supplies, the way a real `$connect` handler stores a connection id. That
//! recording is what a refusal can be read against at all: a lifecycle route's answer reaches no
//! client, so whether the `$connect` route's integration was reached is a fact only something
//! that ran can report.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::http_api_backend_dispatch::{control, given_function};
use crate::websocket_connect_identity_source_socket_answer::{
    answered_over, Answered, STAGE, UNAUTHORIZED, UNAUTHORIZED_MESSAGE,
};
use crate::websocket_transport_data_plane::{
    dial_loopback, handshake_at, ClientSocket, ACTION_SELECTION_EXPRESSION,
};

/// The Region the shared control-plane and Lambda callers sign every request for
/// ([`control`], [`given_function`]), and so the Region this suite's api, its functions and its
/// queue live in. [`authorizer_uri`] checks the function it is handed really lives there, so a
/// caller that signed for another Region fails with that statement rather than with a refusal
/// from a service that was never told about the function.
const REGION: &str = "us-east-1";

/// The status and sentence a stated decision that does not authorize the connection request
/// refuses the opening handshake with.
const FORBIDDEN: u16 = 403;
const FORBIDDEN_MESSAGE: &str = "Forbidden";

/// The status the gateway answers for a connection request it could not get a decision for at
/// all — which a loaded host produces transiently while the authorizer function's runtime is
/// starting, so a row expecting another status offers the handshake again until a decision
/// arrives.
const INTERNAL_SERVER_ERROR: u16 = 500;

/// How long a row keeps offering the handshake, and how long it waits for a record a function
/// already owes, looking again every [`LOOK_INTERVAL`]: both are generous, bound nothing any
/// row asserts on, and are waits for a CONDITION — a decision, a record — rather than for a
/// span of the clock.
const DECISION_WAIT: Duration = Duration::from_secs(120);
const RECORD_WAIT: Duration = Duration::from_secs(60);
const LOOK_INTERVAL: Duration = Duration::from_millis(50);

/// The identity source the authorizer of every case below is configured with: one query-string
/// parameter of the handshake, the spelling `Authorizer$IdentitySource` documents for a
/// WebSocket api ("the identity source can be headers, query string parameters, stage
/// variables, and context parameters").
const IDENTITY_SOURCE: &str = "route.request.querystring.token";

/// The tokens [`deciding_authorizer`] states each kind of decision for, and the principal it
/// names beside a policy document.
const ALLOW_TOKEN: &str = "allow";
const DENY_TOKEN: &str = "deny";
const DENY_BESIDE_ALLOW_TOKEN: &str = "deny-beside-allow";
const OTHER_ROUTE_TOKEN: &str = "another-route";
const PRINCIPAL: &str = "wire-connection-principal";

/// How long a received message stays invisible, in seconds: the longest the member admits
/// ("The visibility timeout for the queue, in seconds", `CreateQueue$Attributes`), so a record
/// this suite has already read never comes back to be counted twice — which is what lets a row
/// state that NOTHING ran for it.
const VISIBILITY_TIMEOUT_SECONDS: u32 = 43_200;

/// This service, AWS Lambda and Amazon SQS on ONE engine, with request authentication off: the
/// routes' functions live in Lambda, and the queue they record the event they were given in
/// lives in SQS.
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
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        Arc::clone(&engine),
    ));
    (router, engine)
}

/// One Amazon SQS call that must answer 200; the credential scope is what routes it there.
async fn sqs(emulator: &MultiServiceRouter, case: &str, operation: &str, body: Value) -> Value {
    let host = format!("sqs.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", &host)
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTESTAPIGWV2/20261021/{REGION}/sqs/aws4_request, \
                 SignedHeaders=host, Signature=0000"
            ),
        )
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", format!("AmazonSQS.{operation}"))
        .body(bytes::Bytes::from(body.to_string()))
        .expect("the request builds");
    let response = emulator.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let parsed: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    assert_eq!(
        status, 200,
        "case {case:?}: SQS {operation} must answer 200, got {status} {parsed}"
    );
    parsed
}

/// The queue every function of a case records the event it was given in.
async fn given_queue(emulator: &MultiServiceRouter, case: &str) -> String {
    let created = sqs(
        emulator,
        case,
        "CreateQueue",
        json!({
            "QueueName": "connect-authorizer-wire-records",
            "Attributes": { "VisibilityTimeout": VISIBILITY_TIMEOUT_SECONDS.to_string() }
        }),
    )
    .await;
    created
        .get("QueueUrl")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: CreateQueue answered no QueueUrl: {created}"))
        .to_string()
}

/// A lifecycle route's function: it records the event it was given in `queue_url` under `name`
/// and admits the call, which is how a call whose answer reaches no client is observed.
fn recording_handler(name: &str, queue_url: &str) -> String {
    format!(
        "import json\n\
         import boto3\n\
         \n\
         \n\
         def lambda_handler(event, context):\n\
         \x20   boto3.client(\"sqs\").send_message(\n\
         \x20       QueueUrl={queue:?},\n\
         \x20       MessageBody=json.dumps({{\"fn\": {name:?}, \"event\": event}}),\n\
         \x20   )\n\
         \x20   return {{\"statusCode\": 200}}\n",
        queue = queue_url,
        name = name,
    )
}

/// The `REQUEST` authorizer's own function: it records the event it was given and then states a
/// decision about the ARN THAT EVENT carried (`methodArn`) — so what tells the rows apart is the
/// effect, the action and the resource a statement names, and never a value written into this
/// test.
///
/// Every token the table does not name is answered in the OTHER format a Lambda authorizer
/// states a decision in: the boolean a simple response carries "instead of an IAM policy".
fn deciding_authorizer(queue_url: &str) -> String {
    format!(
        "import json\n\
         import boto3\n\
         \n\
         \n\
         def statement(effect, action, resource):\n\
         \x20   return {{\"Action\": action, \"Effect\": effect, \"Resource\": resource}}\n\
         \n\
         \n\
         def lambda_handler(event, context):\n\
         \x20   boto3.client(\"sqs\").send_message(\n\
         \x20       QueueUrl={queue:?},\n\
         \x20       MessageBody=json.dumps({{\"fn\": \"authorizer\", \"event\": event}}),\n\
         \x20   )\n\
         \x20   arn = event[\"methodArn\"]\n\
         \x20   token = (event.get(\"queryStringParameters\") or {{}}).get(\"token\", \"\")\n\
         \x20   invoke = \"execute-api:Invoke\"\n\
         \x20   if token == {allow:?}:\n\
         \x20       statements = [statement(\"Allow\", invoke, arn)]\n\
         \x20   elif token == {deny:?}:\n\
         \x20       statements = [statement(\"Deny\", invoke, arn)]\n\
         \x20   elif token == {both:?}:\n\
         \x20       statements = [statement(\"Allow\", invoke, arn),\n\
         \x20                     statement(\"Deny\", invoke, arn)]\n\
         \x20   elif token == {elsewhere:?}:\n\
         \x20       other = arn.rsplit(\"/\", 1)[0] + \"/$disconnect\"\n\
         \x20       statements = [statement(\"Allow\", invoke, other)]\n\
         \x20   else:\n\
         \x20       return {{\"isAuthorized\": False, \"context\": {{\"token\": token}}}}\n\
         \x20   return {{\"principalId\": {principal:?},\n\
         \x20           \"policyDocument\": {{\"Version\": \"2012-10-17\",\n\
         \x20                              \"Statement\": statements}},\n\
         \x20           \"context\": {{\"token\": token}}}}\n",
        queue = queue_url,
        allow = ALLOW_TOKEN,
        deny = DENY_TOKEN,
        both = DENY_BESIDE_ALLOW_TOKEN,
        elsewhere = OTHER_ROUTE_TOKEN,
        principal = PRINCIPAL,
    )
}

/// One control-plane create that must answer `201`.
async fn created(
    emulator: &MultiServiceRouter,
    case: &str,
    what: &str,
    uri: &str,
    body: Value,
) -> Value {
    let (status, answered) = control(emulator, "POST", uri, Some(body)).await;
    assert_eq!(
        status, 201,
        "case {case:?}: {what} must answer 201, got {status} {answered}"
    );
    answered
}

/// One control-plane read that must answer `200`.
async fn fetched(emulator: &MultiServiceRouter, case: &str, what: &str, uri: &str) -> Value {
    let (status, answered) = control(emulator, "GET", uri, None).await;
    assert_eq!(
        status, 200,
        "case {case:?}: {what} must answer 200, got {status} {answered}"
    );
    answered
}

/// The string member `name` of `document`, or the empty string when it names none.
fn text(document: &Value, name: &str) -> String {
    document
        .get(name)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The member of `event` at `pointer`, as JSON.
fn member(event: &Value, pointer: &str) -> Value {
    event.pointer(pointer).cloned().unwrap_or(Value::Null)
}

/// The `AuthorizerUri` of a `REQUEST` authorizer fronted by `function_arn`: "a well-formed
/// Lambda function URI, for example,
/// arn:aws:apigateway:{region}:lambda:path/2015-03-31/functions/{function-arn}/invocations"
/// (`Authorizer$AuthorizerUri`).
fn authorizer_uri(case: &str, function_arn: &str) -> String {
    let region = function_arn.split(':').nth(3).unwrap_or_default();
    assert_eq!(
        region, REGION,
        "case {case:?}: the authorizer's function must live in the Region every request here is \
         signed for, got {function_arn:?}"
    );
    format!(
        "arn:aws:apigateway:{region}:lambda:path/2015-03-31/functions/{function_arn}/invocations"
    )
}

/// One api of a case, with the socket its `ApiEndpoint` announces.
struct Deployed {
    api_id: String,
    /// The `host:port` of the api's own socket, read off the announced `ApiEndpoint`, so a case
    /// dials the address the service reported.
    authority: String,
    /// The queue the api's functions record what they were given in.
    queue: String,
}

/// A WEBSOCKET api whose `$connect` route is fronted by a `REQUEST` authorizer over
/// [`IDENTITY_SOURCE`] and whose `$connect` and `$disconnect` routes each front their OWN
/// function, with a stage and a deployment released to it.
///
/// Every record the data plane decides from is read back through its own `Get…` before a
/// handshake is offered, so what the socket answers comes from records a later caller reads and
/// not from an echo of a create.
async fn given_deployment(
    emulator: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    case: &str,
) -> Deployed {
    let queue = given_queue(emulator, case).await;
    let authorizer_function = given_function(
        emulator,
        engine,
        case,
        "wire-connect-authorizer",
        &deciding_authorizer(&queue),
    )
    .await;
    let connect = given_function(
        emulator,
        engine,
        case,
        "wire-connect-route",
        &recording_handler("$connect", &queue),
    )
    .await;
    let disconnect = given_function(
        emulator,
        engine,
        case,
        "wire-disconnect-route",
        &recording_handler("$disconnect", &queue),
    )
    .await;

    let api = created(
        emulator,
        case,
        "CreateApi",
        "/v2/apis",
        json!({
            "name": "connect-authorizer-wire-api",
            "protocolType": "WEBSOCKET",
            "routeSelectionExpression": ACTION_SELECTION_EXPRESSION
        }),
    )
    .await;
    let api_id = text(&api, "apiId");
    let announced = text(&api, "apiEndpoint");
    let authority = announced
        .strip_prefix("wss://")
        .unwrap_or_else(|| {
            panic!("case {case:?}: a WEBSOCKET api must announce a wss:// socket, got {announced:?}")
        })
        .to_string();

    let authorizer = created(
        emulator,
        case,
        "CreateAuthorizer",
        &format!("/v2/apis/{api_id}/authorizers"),
        json!({
            "name": "connect-auth",
            "authorizerType": "REQUEST",
            "authorizerUri": authorizer_uri(case, &authorizer_function),
            "identitySource": [IDENTITY_SOURCE],
            // No caching, so every handshake of a case is decided by its own invocation.
            "authorizerResultTtlInSeconds": 0
        }),
    )
    .await;
    let authorizer_id = text(&authorizer, "authorizerId");
    let read_back = fetched(
        emulator,
        case,
        "GetAuthorizer",
        &format!("/v2/apis/{api_id}/authorizers/{authorizer_id}"),
    )
    .await;
    assert_eq!(
        read_back.get("identitySource"),
        Some(&json!([IDENTITY_SOURCE])),
        "case {case:?}: the stored authorizer must report the identity source it was created \
         with, got {read_back}"
    );

    for (route_key, function, authorizer_id) in [
        ("$connect", &connect, Some(authorizer_id.as_str())),
        ("$disconnect", &disconnect, None),
    ] {
        let integration = created(
            emulator,
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
        let mut body = json!({
            "routeKey": route_key,
            "target": format!("integrations/{}", text(&integration, "integrationId"))
        });
        if let Some(authorizer_id) = authorizer_id {
            // `CUSTOM` is the `AuthorizationType` "for using a Lambda authorizer".
            body["authorizationType"] = json!("CUSTOM");
            body["authorizerId"] = json!(authorizer_id);
        }
        let route = created(
            emulator,
            case,
            "CreateRoute",
            &format!("/v2/apis/{api_id}/routes"),
            body,
        )
        .await;
        let read_back = fetched(
            emulator,
            case,
            "GetRoute",
            &format!("/v2/apis/{api_id}/routes/{}", text(&route, "routeId")),
        )
        .await;
        assert_eq!(
            text(&read_back, "routeKey"),
            route_key,
            "case {case:?}: the stored route must report its key, got {read_back}"
        );
        assert_eq!(
            read_back.get("authorizerId").and_then(Value::as_str),
            authorizer_id,
            "case {case:?}: the stored {route_key} route must name the authorizer that decides \
             it, got {read_back}"
        );
    }

    created(
        emulator,
        case,
        "CreateStage",
        &format!("/v2/apis/{api_id}/stages"),
        json!({ "stageName": STAGE }),
    )
    .await;
    created(
        emulator,
        case,
        "CreateDeployment",
        &format!("/v2/apis/{api_id}/deployments"),
        json!({ "stageName": STAGE }),
    )
    .await;
    let stage = fetched(
        emulator,
        case,
        "GetStage",
        &format!("/v2/apis/{api_id}/stages/{STAGE}"),
    )
    .await;
    assert!(
        !text(&stage, "deploymentId").is_empty(),
        "case {case:?}: the stage must carry the released deployment, got {stage}"
    );

    Deployed {
        api_id,
        authority,
        queue,
    }
}

/// Everything the functions of one case have recorded so far, each as `(the function's name,
/// the event it was given)`.
///
/// A record is read ONCE — the queue holds every received message invisible for
/// [`VISIBILITY_TIMEOUT_SECONDS`] — so the entries added since an index are exactly what ran
/// since then, which is what lets a row state that nothing ran for it.
#[derive(Debug, Default)]
struct Ledger {
    seen: Vec<(String, Value)>,
}

impl Ledger {
    /// Take whatever the queue holds right now.
    async fn drain(&mut self, emulator: &MultiServiceRouter, case: &str, queue: &str) {
        let received = sqs(
            emulator,
            case,
            "ReceiveMessage",
            json!({ "QueueUrl": queue, "MaxNumberOfMessages": 10, "WaitTimeSeconds": 0 }),
        )
        .await;
        let messages = received
            .get("Messages")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for message in messages {
            let Some(record) = message
                .get("Body")
                .and_then(Value::as_str)
                .and_then(|body| serde_json::from_str::<Value>(body).ok())
            else {
                continue;
            };
            self.seen
                .push((text(&record, "fn"), member(&record, "/event")));
        }
    }

    /// Wait until a record from `name` has arrived since `from`, and answer the event it
    /// carried.
    ///
    /// The wait is for a record the call already owes: an invocation this emulator started is
    /// not instantaneous, and the function's own `send_message` is a second in-process call the
    /// route's answer does not wait for.
    async fn awaited(
        &mut self,
        emulator: &MultiServiceRouter,
        case: &str,
        queue: &str,
        from: usize,
        name: &str,
    ) -> Value {
        let deadline = Instant::now() + RECORD_WAIT;
        loop {
            self.drain(emulator, case, queue).await;
            if let Some((_, event)) = self.seen[from..]
                .iter()
                .find(|(recorded, _)| recorded == name)
            {
                return event.clone();
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: expected a record from {name:?}, saw records from {:?}",
                self.names(from)
            );
            tokio::time::sleep(LOOK_INTERVAL).await;
        }
    }

    /// The names of the functions that have recorded something since `from`.
    fn names(&self, from: usize) -> Vec<&str> {
        self.seen[from..]
            .iter()
            .map(|(name, _)| name.as_str())
            .collect()
    }
}

/// One row of the refusal table: what the handshake presents for the authorizer's identity
/// source, and the refusal the authorization owes it.
struct Refusal {
    case: &'static str,
    /// The raw query string the handshake carries.
    query: &'static str,
    /// The status and sentence the socket must answer with.
    expected: (u16, &'static str),
    /// Whether the authorizer's own function is invoked for this row at all.
    authorizer_runs: bool,
}

/// The rows span what the refusal generalises over: whether the precondition in front of the
/// authorizer is satisfied (an absent identity source, one present but empty, and one
/// presented), and — past it — each way an IAM evaluation of the policy the function returned
/// fails to authorize `execute-api:Invoke` on the `$connect` route's own ARN, plus the same
/// refusal stated in the other format a Lambda authorizer answers in.
///
/// A row whose function RUNS comes first, so the rows after it are decided by a warm runtime,
/// and another one FOLLOWS every row whose function does not run — including last, so that the
/// record the final row waits for proves the queue has delivered everything sent before it. That
/// is what makes the closing claim (no handshake here reached the `$connect` route's
/// integration) a statement about records that had time to arrive rather than about a queue that
/// was merely asked too early.
const REFUSALS: &[Refusal] = &[
    Refusal {
        // The recorded case.
        case: "an explicit Deny on the route's own ARN",
        query: "token=deny",
        expected: (FORBIDDEN, FORBIDDEN_MESSAGE),
        authorizer_runs: true,
    },
    Refusal {
        case: "the identity source absent, which the authorizer is not invoked for",
        query: "",
        expected: (UNAUTHORIZED, UNAUTHORIZED_MESSAGE),
        authorizer_runs: false,
    },
    Refusal {
        case: "an explicit Deny beside an Allow for the same ARN",
        query: "token=deny-beside-allow",
        expected: (FORBIDDEN, FORBIDDEN_MESSAGE),
        authorizer_runs: true,
    },
    Refusal {
        case: "the identity source present and empty, which is absent too",
        query: "token=",
        expected: (UNAUTHORIZED, UNAUTHORIZED_MESSAGE),
        authorizer_runs: false,
    },
    Refusal {
        case: "an Allow naming another route of the same stage",
        query: "token=another-route",
        expected: (FORBIDDEN, FORBIDDEN_MESSAGE),
        authorizer_runs: true,
    },
    Refusal {
        case: "the identity source carried under another parameter altogether",
        query: "ticket=deny",
        expected: (UNAUTHORIZED, UNAUTHORIZED_MESSAGE),
        authorizer_runs: false,
    },
    Refusal {
        case: "the boolean of a simple response stating no",
        query: "token=shut",
        expected: (FORBIDDEN, FORBIDDEN_MESSAGE),
        authorizer_runs: true,
    },
];

/// Offer one opening handshake on the api's announced socket and read the answer off the wire,
/// offering it again while the gateway is still answering its own `500` for a row that owes
/// another status — what a loaded host produces transiently while the authorizer function's
/// runtime is starting. The wait is for a DECISION, and nothing asserts on the clock.
async fn refused(case: &str, authority: &str, query: &str, expected: u16) -> Answered {
    let deadline = Instant::now() + DECISION_WAIT;
    loop {
        let stream = dial_loopback(case, authority).await.unwrap_or_else(|error| {
            panic!(
                "case {case:?}: the announced socket {authority:?} must accept a connection, got \
                 {error}"
            )
        });
        let answered = answered_over(case, stream, authority, query, &[]).await;
        let transient = answered.status == INTERNAL_SERVER_ERROR && expected != INTERNAL_SERVER_ERROR;
        if !transient || Instant::now() >= deadline {
            return answered;
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

#[tokio::test]
async fn the_socket_refuses_an_unauthorized_handshake_terminally_and_runs_no_connect_function() {
    let case = "one api whose $connect route is guarded by a REQUEST authorizer over a \
                query-string parameter";
    let (emulator, engine) = emulator();
    let deployed = given_deployment(&emulator, &engine, case).await;
    let mut ledger = Ledger::default();

    for row in REFUSALS {
        let label = row.case;
        let (status, message) = row.expected;
        let from = ledger.seen.len();
        let answered = refused(label, &deployed.authority, row.query, status).await;

        assert_eq!(
            answered.status, status,
            "case {case:?} / {label}: expected the handshake refused {status}, got {} with head \
             {:?}",
            answered.status, answered.head
        );
        // The status line names the status and the reason phrase the status registry gives it
        // (`http::StatusCode::canonical_reason`), so a client reading only the first line reads
        // the refusal's own class rather than a server fault.
        let reason = http::StatusCode::from_u16(status)
            .ok()
            .and_then(|status| status.canonical_reason())
            .unwrap_or_else(|| panic!("case {case:?} / {label}: {status} is a known status"));
        let status_line = answered.head.lines().next().unwrap_or_default();
        assert!(
            status_line.contains(&format!("{status} {reason}")),
            "case {case:?} / {label}: expected the status line to state {status} {reason:?}, got \
             {status_line:?}"
        );
        // No row upgrades, so none of the fields RFC 6455 §4.2.2 requires of a completed
        // handshake is in what the client read: a refusal is not half an upgrade.
        let head = answered.head.to_ascii_lowercase();
        for field in ["upgrade: websocket", "sec-websocket-accept", "connection: upgrade"] {
            assert!(
                !head.contains(field),
                "case {case:?} / {label}: a refused handshake carries no {field:?}, got {:?}",
                answered.head
            );
        }
        assert_eq!(
            answered.message().as_deref(),
            Some(message),
            "case {case:?} / {label}: expected the body to word the refusal {message:?}, got {:?}",
            answered.body
        );

        if row.authorizer_runs {
            // The authorizer's function ran and decided about the `$connect` route's own ARN.
            let event = ledger
                .awaited(&emulator, label, &deployed.queue, from, "authorizer")
                .await;
            assert_eq!(
                member(&event, "/requestContext/routeKey"),
                json!("$connect"),
                "case {case:?} / {label}: the authorizer must be asked about the route serving \
                 the connection request, got {event}"
            );
            assert!(
                member(&event, "/methodArn")
                    .as_str()
                    .is_some_and(|arn| arn.contains(&deployed.api_id)
                        && arn.contains(STAGE)
                        && arn.ends_with("$connect")),
                "case {case:?} / {label}: the authorizer event must name the route it decides, \
                 got {event}"
            );
        } else {
            // "Otherwise, it returns a 401 Unauthorized response without calling the Lambda
            // function": the handshake was answered terminally and no function of the api ran.
            ledger.drain(&emulator, label, &deployed.queue).await;
        }
        // Whatever decided it, the route's own integration was never reached: the authorization
        // is applied before anything that route fronts.
        assert!(
            !ledger.names(from).contains(&"$connect"),
            "case {case:?} / {label}: a refused handshake must not reach the $connect route's \
             integration, records since this handshake: {:?}",
            ledger.names(from)
        );
        if !row.authorizer_runs {
            assert!(
                ledger.names(from).is_empty(),
                "case {case:?} / {label}: no function may run for a handshake refused before the \
                 authorizer, records since this handshake: {:?}",
                ledger.names(from)
            );
        }
    }

    // The last row waited for a record of its own, so the queue has delivered everything the
    // handshakes before it sent: over the whole table, not one refused connection request
    // reached the `$connect` route's integration.
    ledger.drain(&emulator, case, &deployed.queue).await;
    assert!(
        !ledger.names(0).contains(&"$connect"),
        "case {case:?}: no refused handshake may reach the $connect route's integration, records \
         written over the whole table: {:?}",
        ledger.names(0)
    );
}

#[tokio::test]
async fn an_authorized_handshake_upgrades_and_each_lifecycle_route_runs_its_own_function() {
    let case = "a handshake the $connect authorizer's function allows";
    let (emulator, engine) = emulator();
    let deployed = given_deployment(&emulator, &engine, case).await;
    let mut ledger = Ledger::default();

    // The handshake is offered again while the gateway is still answering its own `500` for the
    // authorizer's runtime starting; the wait is for a decision and nothing asserts on the
    // clock. A completed handshake is read by the WebSocket client implementation itself, which
    // checks the `101`, the `Upgrade` field and the `Sec-WebSocket-Accept` the key it offered
    // owes — the RFC 6455 §4.2.2 fields the refusals above carry none of.
    let deadline = Instant::now() + DECISION_WAIT;
    let socket: ClientSocket = loop {
        let offered = handshake_at(
            case,
            &deployed.authority,
            &format!("/{STAGE}?token={ALLOW_TOKEN}&trace=wire"),
        )
        .await;
        match offered {
            Ok(socket) => break socket,
            Err(status) if status == INTERNAL_SERVER_ERROR && Instant::now() < deadline => {
                tokio::time::sleep(LOOK_INTERVAL).await;
            }
            Err(status) => panic!(
                "case {case:?}: a decision that authorizes the connection request must complete \
                 the handshake, got {status}"
            ),
        }
    };

    // The `$connect` route's OWN function served the connection request, with the principal the
    // authorizer named and the handshake's own query string.
    let connect = ledger
        .awaited(&emulator, case, &deployed.queue, 0, "$connect")
        .await;
    let connection_id = member(&connect, "/requestContext/connectionId");
    for (what, pointer, expected) in [
        (
            "the kind of call",
            "/requestContext/eventType",
            json!("CONNECT"),
        ),
        (
            "the route that served it",
            "/requestContext/routeKey",
            json!("$connect"),
        ),
        ("the stage", "/requestContext/stage", json!(STAGE)),
        (
            "the principal the authorizer named",
            "/requestContext/authorizer/principalId",
            json!(PRINCIPAL),
        ),
        (
            "the context the authorizer returned",
            "/requestContext/authorizer/token",
            json!(ALLOW_TOKEN),
        ),
        (
            "the handshake's query string",
            "/queryStringParameters/trace",
            json!("wire"),
        ),
    ] {
        assert_eq!(
            member(&connect, pointer),
            expected,
            "case {case:?}: expected {what} at {pointer} to be {expected}, got {connect}"
        );
    }
    assert!(
        connection_id.as_str().is_some_and(|id| !id.trim().is_empty()),
        "case {case:?}: the connection request must carry the minted connectionId, got {connect}"
    );

    // And the close is the `$disconnect` route's own function, about the same connection.
    drop(socket);
    let disconnect = ledger
        .awaited(&emulator, case, &deployed.queue, 0, "$disconnect")
        .await;
    for (what, pointer, expected) in [
        (
            "the kind of call",
            "/requestContext/eventType",
            json!("DISCONNECT"),
        ),
        (
            "the route that served it",
            "/requestContext/routeKey",
            json!("$disconnect"),
        ),
        (
            "the connection that ended",
            "/requestContext/connectionId",
            connection_id.clone(),
        ),
    ] {
        assert_eq!(
            member(&disconnect, pointer),
            expected,
            "case {case:?}: expected {what} at {pointer} to be {expected}, got {disconnect}"
        );
    }
}
