// SPDX-License-Identifier: CC0-1.0

//! The CREDENTIALS a running handler is given in its own environment, end to end over the
//! wire: Lambda, IAM and the token service on ONE engine, functions whose code reports what
//! `AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY` / `AWS_SESSION_TOKEN` hold and who the
//! identity behind them is.
//!
//! The rule pinned here: a managed runtime exports the invocation's own credentials to the
//! handler in those three reserved environment variables, and they are the TEMPORARY
//! credentials of a session of the function's execution role — so an SDK's default credential
//! chain inside the handler resolves exactly those three values and signs as that session, a
//! client the handler builds from the variables themselves signs as the same session, and the
//! identity the token service reports for either is `assumed-role/<role name>/<function name>`.
//! Because they are temporary, the key and the secret WITHOUT the session token present a
//! security token that resolves to nothing, which the token service answers HTTP 403
//! `InvalidClientTokenId`.
//!
//! The rows differ along what the rule generalises over: which managed runtime runs the
//! handler (four node releases and a python one, which are two different engines), which
//! execution role the function runs as, which function the session is named for, and where the
//! handler's client got its credentials from (none at all, the three variables, the two
//! without the token).

use std::io::Write as _;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::invoke_request_id::one_invocation_stream_at_a_time;

/// The Region every request below is signed for.
const REGION: &str = "us-east-1";

/// Where `CreateFunction` and `Invoke` are bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The emulator's own endpoint, as the `Host` of every call this suite makes to Lambda.
const LAMBDA_HOST: &str = "lambda.us-east-1.amazonaws.com";

/// How long this suite leaves between two readings of a status the clock owes. Every wait
/// below looks again for the CONDITION rather than sleeping for a span, so nothing here waits
/// on the emulator's behalf.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// How long a status the clock owes is waited for before the wait itself is the failure.
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);

/// The three reserved variables a managed runtime exports the invocation's credentials in
/// (the Lambda Developer Guide's reserved runtime environment variables).
const CREDENTIAL_VARIABLES: &[&str] = &[
    "AWS_ACCESS_KEY_ID",
    "AWS_SECRET_ACCESS_KEY",
    "AWS_SESSION_TOKEN",
];

/// The code the token service answers a request whose security token resolves to nothing
/// with, and the sentence it words it in — fixed text with no variable part.
const UNRESOLVED_CREDENTIAL: &str = "InvalidClientTokenId";
const UNRESOLVED_CREDENTIAL_MESSAGE: &str = "The security token included in the request is invalid.";

/// Lambda, IAM and the token service over ONE shared engine, as `core/router/src/lib.rs`
/// mounts them: a handler's SDK call is dispatched back into this same router, so the session
/// it signs with is the session this emulator minted for the role IAM holds.
fn router_on(engine: &Arc<engine::InMemoryEngine>, authenticating: bool) -> MultiServiceRouter {
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(authenticating);
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(engine)),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(
            Arc::clone(engine),
        ),
    );
    router
}

/// A SigV4 `Authorization` whose credential scope names `service`, which is what routes the
/// request. Nothing in it is verified by a router this suite built with authentication off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=emulator-test-key/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// One Lambda `restJson1` call, as its status, headers and parsed body.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: &[u8],
) -> (u16, http::HeaderMap, Value) {
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{LAMBDA_HOST}{uri}"))
        .header("host", LAMBDA_HOST)
        .header("authorization", authorization("lambda"))
        .header("content-type", "application/json")
        .body(bytes::Bytes::from(body.to_vec()))
        .expect("the test request is well formed");
    answered(router, request).await
}

/// Dispatch one request and read what it answered.
async fn answered(
    router: &MultiServiceRouter,
    request: http::Request<bytes::Bytes>,
) -> (u16, http::HeaderMap, Value) {
    let response = router.dispatch(request).await;
    let (parts, body) = response.into_parts();
    let parsed: Value = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&body)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&body).to_string()))
    };
    (parts.status.as_u16(), parts.headers, parsed)
}

/// `application/x-www-form-urlencoded` for one awsQuery parameter value, leaving only RFC
/// 3986's unreserved characters literal.
fn form_encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(char::from(byte));
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

/// Create the execution role `role_name`, trusted by Lambda — written with IAM's own
/// operation, so the role a session is minted of is the record `iam:CreateRole` wrote.
async fn execution_role(router: &MultiServiceRouter, case: &str, role_name: &str) -> String {
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"Service": "lambda.amazonaws.com"},
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    let body = format!(
        "Action=CreateRole&Version=2010-05-08&RoleName={}&AssumeRolePolicyDocument={}",
        form_encode(role_name),
        form_encode(&trust)
    );
    let request = http::Request::builder()
        .method("POST")
        .uri("https://iam.amazonaws.com/")
        .header("host", "iam.amazonaws.com")
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("iam"))
        .body(bytes::Bytes::from(body))
        .expect("the test request is well formed");
    let (status, _headers, created) = answered(router, request).await;
    assert_eq!(
        status, 200,
        "case {case:?}: iam:CreateRole must succeed, got {status} ({created})"
    );
    role_arn(role_name)
}

/// The ARN of the role IAM holds under `role_name`.
fn role_arn(role_name: &str) -> String {
    format!(
        "arn:aws:iam::{}:role/{role_name}",
        engine::emulator_account_id()
    )
}

/// The assumed-role session ARN a call an invocation of `function_name` running as
/// `role_name` makes speaks for: the session is named for the FUNCTION.
fn session_arn(role_name: &str, function_name: &str) -> String {
    format!(
        "arn:aws:sts::{}:assumed-role/{role_name}/{function_name}",
        engine::emulator_account_id()
    )
}

/// A .zip deployment package holding `source` under `entry`, and nothing else.
fn package(entry: &str, source: &str) -> Vec<u8> {
    let mut buffer = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut buffer);
        let options: zip::write::FileOptions<'_, '_, ()> = zip::write::FileOptions::default();
        writer.start_file(entry, options).expect("the entry starts");
        writer
            .write_all(source.as_bytes())
            .expect("the entry's bytes");
        writer.finish().expect("a finished archive");
    }
    buffer.into_inner()
}

/// Create one function and wait for the state an invocation needs, reading the CONDITION back
/// through the function's own configuration rather than sleeping for a span.
async fn create_function(router: &MultiServiceRouter, case: &str, row: &Row, role: &str) {
    let (status, _headers, created) = lambda(
        router,
        "POST",
        FUNCTIONS_URI,
        &serde_json::to_vec(&json!({
            "FunctionName": row.function,
            "Role": role,
            "Runtime": row.runtime,
            "Handler": row.handler,
            "Timeout": 30,
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD
                    .encode(package(row.entry, row.source)),
            },
        }))
        .expect("the create body serializes"),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: expected HTTP 201 from CreateFunction, got {status} ({created})"
    );
    let deadline = std::time::Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, _headers, configuration) = lambda(
            router,
            "GET",
            &format!("{FUNCTIONS_URI}/{}/configuration", row.function),
            &[],
        )
        .await;
        if status == 200 && configuration["State"] == json!("Active") {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: the function never became Active: {status} ({configuration})"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// `Invoke` the function synchronously with `event`, answering the `FunctionError` signal and
/// the payload the run produced.
async fn invoke(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    event: &Value,
) -> (Option<String>, Value) {
    let (status, headers, payload) = lambda(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{name}/invocations"),
        event.to_string().as_bytes(),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from Invoke, got {status} ({payload})"
    );
    let signal = headers
        .get("x-amz-function-error")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    (signal, payload)
}

/// The node handler every node row runs: it reports whether each of the three variables holds
/// a non-empty string, what the SDK's own credential provider resolves for a client built with
/// no credentials and for one built from those variables, then who the token service says each
/// of those two clients calls as — and, when the event asks for it, what happens to a client
/// built with the key and the secret and no session token.
const NODE_SOURCE: &str = r#"
'use strict';

const { STSClient, GetCallerIdentityCommand } = require('@aws-sdk/client-sts');

const KEYS = ['AWS_ACCESS_KEY_ID', 'AWS_SECRET_ACCESS_KEY', 'AWS_SESSION_TOKEN'];

function present() {
    const held = {};
    for (const key of KEYS) {
        const value = process.env[key];
        held[key] = typeof value === 'string' && value.length > 0;
    }
    return held;
}

async function identity(credentials) {
    const client = new STSClient(
        credentials === undefined ? {} : { credentials: credentials }
    );
    try {
        const answered = await client.send(new GetCallerIdentityCommand({}));
        return { arn: answered.Arn };
    } catch (error) {
        return {
            code: error.name,
            message: error.message,
            status: error.$metadata === undefined
                ? null
                : error.$metadata.httpStatusCode,
        };
    }
}

// What the client's own credential provider answers: a v3 client normalises whatever it was
// built with into one, and a client built with none resolves the default chain — which in a
// Lambda execution environment is the three variables the runtime exported. Only WHETHER the
// resolved credential is the environment's is reported; no value of it is.
async function resolved(credentials) {
    const client = new STSClient(
        credentials === undefined ? {} : { credentials: credentials }
    );
    const held = await client.config.credentials();
    return {
        provider: typeof client.config.credentials,
        matchesEnvironment: held.accessKeyId === process.env.AWS_ACCESS_KEY_ID
            && held.secretAccessKey === process.env.AWS_SECRET_ACCESS_KEY
            && held.sessionToken === process.env.AWS_SESSION_TOKEN,
    };
}

function fromEnvironment() {
    return {
        accessKeyId: process.env.AWS_ACCESS_KEY_ID,
        secretAccessKey: process.env.AWS_SECRET_ACCESS_KEY,
        sessionToken: process.env.AWS_SESSION_TOKEN,
    };
}

exports.handler = async (event) => {
    const report = { present: present() };
    if (event.mode === 'presence') {
        return report;
    }
    report.chainCredentials = await resolved();
    report.givenCredentials = await resolved(fromEnvironment());
    report.chain = await identity();
    report.environment = await identity(fromEnvironment());
    if (event.mode === 'without_token') {
        report.withoutToken = await identity({
            accessKeyId: process.env.AWS_ACCESS_KEY_ID,
            secretAccessKey: process.env.AWS_SECRET_ACCESS_KEY,
        });
    }
    return report;
};
"#;

/// The python handler of the one python row: the same three variables, read through that
/// runtime's own environment, which is what makes the rule the RUNTIME's and not one engine's.
const PYTHON_SOURCE: &str = r#"
import os

KEYS = ("AWS_ACCESS_KEY_ID", "AWS_SECRET_ACCESS_KEY", "AWS_SESSION_TOKEN")


def lambda_handler(event, context):
    return {"present": {key: bool(os.environ.get(key)) for key in KEYS}}
"#;

/// One row: the runtime that runs the handler, the function it runs as, the role that function
/// runs as, and whether the handler also reports the identity its credentials speak for.
struct Row {
    case: &'static str,
    runtime: &'static str,
    entry: &'static str,
    handler: &'static str,
    source: &'static str,
    function: &'static str,
    role: &'static str,
    reports_identity: bool,
}

/// The two roles the rows run as: one shared by two functions — so the session names differ
/// while the role does not — and one of its own.
const ROLE: &str = "credential-environment-exec";
const OTHER_ROLE: &str = "credential-environment-other-exec";

const ROWS: &[Row] = &[
    Row {
        case: "a nodejs22.x function",
        runtime: "nodejs22.x",
        entry: "index.js",
        handler: "index.handler",
        source: NODE_SOURCE,
        function: "credential-environment-node22",
        role: ROLE,
        reports_identity: true,
    },
    Row {
        // The same role, another function: the session a run's credentials speak for is named
        // for the FUNCTION, so this row's identity is a different session of the same role.
        case: "another function of the same execution role, on nodejs20.x",
        runtime: "nodejs20.x",
        entry: "index.js",
        handler: "index.handler",
        source: NODE_SOURCE,
        function: "credential-environment-node20",
        role: ROLE,
        reports_identity: true,
    },
    Row {
        // Another role: the identity behind the variables is a session of the role THIS
        // function runs as.
        case: "a function of another execution role, on nodejs18.x",
        runtime: "nodejs18.x",
        entry: "index.js",
        handler: "index.handler",
        source: NODE_SOURCE,
        function: "credential-environment-node18",
        role: OTHER_ROLE,
        reports_identity: true,
    },
    Row {
        // Another engine entirely: the variables are the RUNTIME's, so a python handler reads
        // the same three out of its own environment.
        case: "a python3.12 function",
        runtime: "python3.12",
        entry: "lambda_function.py",
        handler: "lambda_function.lambda_handler",
        source: PYTHON_SOURCE,
        function: "credential-environment-python",
        role: ROLE,
        reports_identity: false,
    },
];

/// What a handler reports for a credential provider that resolved the run's own environment:
/// a client's `config.credentials` is a FUNCTION, and what it answers is what the three
/// variables hold.
fn resolves_the_environment() -> Value {
    json!({ "provider": "function", "matchesEnvironment": true })
}

/// What a row's handler must report: all three variables held, and — for a row that reports
/// it — the function's own assumed-role session as the identity behind both the default
/// credential chain and a client built from the variables.
fn expected(row: &Row) -> Value {
    let mut report = json!({ "present": all_three_held() });
    if row.reports_identity {
        let session = json!({ "arn": session_arn(row.role, row.function) });
        report["chainCredentials"] = resolves_the_environment();
        report["givenCredentials"] = resolves_the_environment();
        report["chain"] = session.clone();
        report["environment"] = session;
    }
    report
}

/// The report a handler makes for an environment holding all three variables: one member per
/// reserved credential variable, each held as a non-empty string.
fn all_three_held() -> Value {
    let mut present = serde_json::Map::new();
    for name in CREDENTIAL_VARIABLES {
        present.insert((*name).to_string(), json!(true));
    }
    Value::Object(present)
}

/// RULE: every managed runtime exports the invocation's own credentials in the three reserved
/// credential variables, and they are a session of the function's execution role named for the
/// function — which is the identity both the SDK's default chain and a client built from the
/// variables call as.
#[tokio::test]
async fn a_handler_is_given_its_execution_role_session_in_the_reserved_variables() {
    let _stream = one_invocation_stream_at_a_time().await;
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine, false);
    let setup = "the roles and functions of the credential-environment table";
    execution_role(&router, setup, ROLE).await;
    execution_role(&router, setup, OTHER_ROLE).await;
    for row in ROWS {
        create_function(&router, row.case, row, &role_arn(row.role)).await;
    }

    for row in ROWS {
        let case = row.case;
        let mode = if row.reports_identity {
            "identity"
        } else {
            "presence"
        };
        let (signal, payload) = invoke(&router, case, row.function, &json!({ "mode": mode })).await;
        assert_eq!(
            signal, None,
            "case {case:?}: the handler must run to completion, got the function error \
             {signal:?} with {payload}"
        );
        let wanted = expected(row);
        assert_eq!(
            payload, wanted,
            "case {case:?}: expected the handler to report {wanted}, got {payload}"
        );
    }
}

/// The function the refusal test invokes, and the role it runs as.
const REFUSED_FUNCTION: &str = "credential-environment-refused";
const REFUSED_ROLE: &str = "credential-environment-refused-exec";

/// The row of the refusal test, as a function record of its own.
const REFUSED_ROW: Row = Row {
    case: "a nodejs22.x function signing with its own credentials",
    runtime: "nodejs22.x",
    entry: "index.js",
    handler: "index.handler",
    source: NODE_SOURCE,
    function: REFUSED_FUNCTION,
    role: REFUSED_ROLE,
    reports_identity: true,
};

/// RULE: the credentials the environment carries are TEMPORARY, so a request signed with the
/// key and the secret and WITHOUT the session token presents a security token that resolves to
/// nothing — HTTP 403 `InvalidClientTokenId` — while the same key and secret WITH the token are
/// served as the function's own session.
///
/// Both halves are driven through an emulator that VERIFIES request signatures, which is the
/// `--auth on` path the emulator binary runs: a credential is only temporary where something
/// checks it.
#[tokio::test]
async fn the_credentials_in_the_environment_are_refused_without_their_session_token() {
    let _stream = one_invocation_stream_at_a_time().await;
    let engine = Arc::new(engine::InMemoryEngine::new());
    // The resources are built through a router that verifies nothing; the INVOCATION below is
    // served by one that verifies every signature, over the same engine, so the handler's own
    // calls are verified too.
    let plain = router_on(&engine, false);
    let case = REFUSED_ROW.case;
    execution_role(&plain, case, REFUSED_ROLE).await;
    create_function(&plain, case, &REFUSED_ROW, &role_arn(REFUSED_ROLE)).await;

    let authenticating = router_on(&engine, true);
    let (access_key_id, secret_access_key) = match authenticating
        .vended_credentials()
        .into_iter()
        .next()
    {
        Some(emulator_server::VendedCredentials::AwsAccessKey {
            access_key_id,
            secret_access_key,
        }) => (access_key_id, secret_access_key),
        other => panic!(
            "with authentication on the emulator must vend a SigV4 key pair, got {other:?}"
        ),
    };

    // The invocation itself, signed with the key pair this emulator published for the run,
    // through its own presigner — so what this drives is a request the emulator computes the
    // same signature for.
    let path = format!("{FUNCTIONS_URI}/{REFUSED_FUNCTION}/invocations");
    let headers = [("content-type", "application/json")];
    let target = emulator_server::request_authentication::PresignTarget {
        endpoint: &format!("https://{LAMBDA_HOST}"),
        method: "POST",
        signing_name: "lambda",
        region: REGION,
        path: &path,
        query: &[],
        headers: &headers,
    };
    let url = emulator_server::request_authentication::presign_url(
        &target,
        &access_key_id,
        &secret_access_key,
        std::time::SystemTime::now(),
        300,
    )
    .expect("the invocation must be signable");
    let mut builder = http::Request::builder()
        .method("POST")
        .uri(url)
        .header("host", LAMBDA_HOST);
    for (name, value) in headers {
        builder = builder.header(name, value);
    }
    if let Some((name, value)) =
        emulator_server::request_authentication::presigned_payload_header("lambda", "POST")
    {
        builder = builder.header(name, value);
    }
    let request = builder
        .body(bytes::Bytes::from(
            json!({ "mode": "without_token" }).to_string(),
        ))
        .expect("the test request is well formed");
    let (status, headers, payload) = answered(&authenticating, request).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from a verified Invoke, got {status} ({payload})"
    );
    let signal = headers
        .get("x-amz-function-error")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    assert_eq!(
        signal, None,
        "case {case:?}: the handler must run to completion, got the function error \
         {signal:?} with {payload}"
    );

    let session = json!({ "arn": session_arn(REFUSED_ROLE, REFUSED_FUNCTION) });
    let wanted = json!({
        "present": all_three_held(),
        "chainCredentials": resolves_the_environment(),
        "givenCredentials": resolves_the_environment(),
        "chain": session.clone(),
        "environment": session,
        // The same key and secret, with no session token: a temporary credential AWS resolves
        // to nothing, under the fixed sentence it words that failure in.
        "withoutToken": {
            "code": UNRESOLVED_CREDENTIAL,
            "message": UNRESOLVED_CREDENTIAL_MESSAGE,
            "status": 403,
        },
    });
    assert_eq!(
        payload, wanted,
        "case {case:?}: expected the handler to report {wanted}, got {payload}"
    );
}
