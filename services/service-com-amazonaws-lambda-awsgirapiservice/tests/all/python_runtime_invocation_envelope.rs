// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for what the `python3.x` invoke runtime hands the handler and hands
//! back: `Invoke` with `InvocationType=RequestResponse` (`POST
//! /2015-03-31/functions/{FunctionName}/invocations`), driven over real HTTP through the
//! router.
//!
//! Four rules are under test, each over a table of inputs:
//!
//! * the CONTEXT object exposes its identity and its client context as OBJECTS carrying
//!   their own members — `identity.cognito_identity_id` /
//!   `identity.cognito_identity_pool_id`, and `client_context.custom` / `.env` / `.client`
//!   with the mobile client's own members on it — each reading `None` where the invocation
//!   carried no value for it, and `client_context` itself reading `None` only where the call
//!   sent no client context at all;
//! * `context.invoked_function_arn` is the ARN the CALL addressed: the function's ARN
//!   carrying exactly the qualifier the call named, and the unqualified ARN only for a call
//!   that named none;
//! * a failure the runtime raised while it was still INITIALIZING the execution environment
//!   is classified by what failed — a handler name that does not split, a handler module the
//!   interpreter itself is built with, a module that does not import, one that does not
//!   compile, a module whose own top-level code raised (reported under that exception's own
//!   class), and a module that defines no such entry point — and names no invocation, while
//!   a failure of the handler's own run names the invocation it failed;
//! * the PAYLOAD a call sends is refused to the caller only when no JSON value can be read
//!   out of it, and one holding nothing but whitespace is refused as holding no content; a
//!   payload whose first value is complete reaches the runtime, which answers the unmarshal
//!   error for whatever follows it.
//!
//! The rows differ along what each rule generalises over: which members a client context
//! document carries, which qualifier a call names and where it names it, which class of
//! initialization failure the package produces, and which class of fault a payload carries.

use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use serde_json::{json, Value};
use service_com_amazonaws_lambda_awsgirapiservice::state::ACTIVATION_DELAY;
use service_com_amazonaws_lambda_awsgirapiservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

use crate::invoke_request_id::{is_host_bound, one_invocation_stream_at_a_time};
// The one multi-entry deployment-archive writer this target has.
use crate::node_invoke_run::package;

/// Where `CreateFunction` is bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// A SigV4 credential scope naming this service's signing name (`lambda`) in `us-east-1`.
/// Nothing in it is verified: the harness turns request authentication off.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=emulator-test-key/20260101/us-east-1/lambda/aws4_request, \
     SignedHeaders=host, Signature=not-verified";

/// A well-formed execution role ARN, satisfying the model's `RoleArn` pattern.
const ROLE: &str = "arn:aws:iam::000000000000:role/service-role/lambda-exec";

/// The `Runtime` every row of this suite runs.
const PYTHON: &str = "python3.12";

/// The module every package of this suite holds its handler in.
const HANDLER: &str = "index.lambda_handler";

/// The header AWS reports a response's request id in.
const REQUEST_ID_HEADER: &str = "x-amzn-requestid";

/// The header AWS reports an invocation whose function failed in.
const FUNCTION_ERROR_HEADER: &str = "x-amz-function-error";

/// How long past a due instant this suite waits before calling. Every transition is owed by
/// the CLOCK, so nothing here sleeps on the emulator's behalf.
const SLACK: Duration = Duration::from_millis(150);

/// How many times a call waits out one of the emulator's own host bounds before its answer
/// is read as the run's.
const SLOT_ATTEMPTS: usize = 6;

/// What a dispatched request answered.
struct Answer {
    status: u16,
    headers: http::HeaderMap,
    body: bytes::Bytes,
}

impl Answer {
    fn json(&self) -> Value {
        if self.body.is_empty() {
            return Value::Null;
        }
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    fn header(&self, name: &str) -> Option<String> {
        self.headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
    }

    /// The id this answer names its request by.
    fn request_id(&self) -> String {
        self.header(REQUEST_ID_HEADER).unwrap_or_default()
    }

    /// The member `name` of the answered document, as text.
    fn member(&self, name: &str) -> Option<String> {
        self.json()
            .get(name)
            .and_then(Value::as_str)
            .map(str::to_string)
    }

    /// The member `name` of the answered document, whatever its type.
    fn value(&self, name: &str) -> Value {
        self.json().get(name).cloned().unwrap_or(Value::Null)
    }
}

/// One service mounted on its own engine, reached the way the production router reaches it.
fn router_on(engine: &Arc<engine::InMemoryEngine>) -> MultiServiceRouter {
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(Arc::clone(engine)));
    router
}

/// Dispatch one request, with the headers `headers` names.
async fn call(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: &[u8],
    headers: &[(&str, &str)],
) -> Answer {
    let mut builder = http::Request::builder()
        .method(method)
        .uri(format!("http://lambda.us-east-1.amazonaws.com{uri}"))
        .header("host", "lambda.us-east-1.amazonaws.com")
        .header("authorization", AUTHORIZATION)
        .header("content-type", "application/json");
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let request = builder
        .body(bytes::Bytes::from(body.to_vec()))
        .expect("the test request must build");
    let response = router.dispatch(request).await;
    let (parts, body) = response.into_parts();
    Answer {
        status: parts.status.as_u16(),
        headers: parts.headers,
        body,
    }
}

/// Create a function holding `entries`, configured with `handler` and `timeout`, published
/// when `publish` says so, and wait out the provisioning window the create documents.
async fn create_function(
    router: &MultiServiceRouter,
    name: &str,
    entries: &[(&str, &str)],
    handler: &str,
    timeout: i64,
    publish: bool,
) {
    let created = call(
        router,
        "POST",
        FUNCTIONS_URI,
        &serde_json::to_vec(&json!({
            "FunctionName": name,
            "Role": ROLE,
            "Runtime": PYTHON,
            "Handler": handler,
            "Timeout": timeout,
            "Publish": publish,
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD.encode(package(entries)),
            },
        }))
        .expect("the create body serializes"),
        &[],
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "CreateFunction {name:?} must succeed, got {} {}",
        created.status,
        created.text()
    );
    tokio::time::sleep(ACTIVATION_DELAY + SLACK).await;
}

/// Remove the function `name`, so the next row of a table creates its own.
async fn delete_function(router: &MultiServiceRouter, case: &str, name: &str) {
    let deleted = call(router, "DELETE", &format!("{FUNCTIONS_URI}/{name}"), &[], &[]).await;
    assert_eq!(
        deleted.status, 204,
        "case {case}: the function must be removable between rows, got {} {}",
        deleted.status,
        deleted.text()
    );
}

/// `Invoke` on the function `reference` names, with the headers `headers` carries,
/// re-driven while the answer reports one of the emulator's own HOST BOUNDS rather than
/// what the run came to: every suite of this test binary contends for the emulator's run
/// slots and for the resident size it guards the whole process with, and an answer carrying
/// one of those says nothing about the rule under test.
async fn invoke(
    router: &MultiServiceRouter,
    reference: &str,
    payload: &[u8],
    headers: &[(&str, &str)],
) -> Answer {
    invoke_qualified(router, reference, None, payload, headers).await
}

/// [`invoke`], naming `qualifier` in the `Qualifier` query parameter the operation binds it
/// to.
async fn invoke_qualified(
    router: &MultiServiceRouter,
    reference: &str,
    qualifier: Option<&str>,
    payload: &[u8],
    headers: &[(&str, &str)],
) -> Answer {
    let mut answered = invoke_once(router, reference, qualifier, payload, headers).await;
    for _ in 1..SLOT_ATTEMPTS {
        if !is_host_bound(answered.status, &answered.text()) {
            break;
        }
        tokio::time::sleep(SLACK).await;
        answered = invoke_once(router, reference, qualifier, payload, headers).await;
    }
    answered
}

/// One `Invoke` on the function `reference` names.
async fn invoke_once(
    router: &MultiServiceRouter,
    reference: &str,
    qualifier: Option<&str>,
    payload: &[u8],
    headers: &[(&str, &str)],
) -> Answer {
    let query = match qualifier {
        Some(qualifier) => format!("?Qualifier={qualifier}"),
        None => String::new(),
    };
    call(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{reference}/invocations{query}"),
        payload,
        headers,
    )
    .await
}

/// A handler reporting what it was handed on its CONTEXT: the identity and client context
/// objects read through their own members, the ARN the call addressed, and two readings of
/// the time the invocation has left.
const CONTEXT_REPORT: &str = "def lambda_handler(event, context):\n    \
     identity = context.identity\n    \
     holder = context.client_context\n    \
     client = None if holder is None else holder.client\n    \
     return {\n        \
         'identity_class': type(identity).__name__,\n        \
         'cognito_identity_id': identity.cognito_identity_id,\n        \
         'cognito_identity_pool_id': identity.cognito_identity_pool_id,\n        \
         'client_context_absent': holder is None,\n        \
         'client_context_class': None if holder is None else type(holder).__name__,\n        \
         'custom': None if holder is None else holder.custom,\n        \
         'env': None if holder is None else holder.env,\n        \
         'client_absent': client is None,\n        \
         'installation_id': None if client is None else client.installation_id,\n        \
         'app_title': None if client is None else client.app_title,\n        \
         'arn': context.invoked_function_arn,\n        \
         'first_remaining': context.get_remaining_time_in_millis(),\n        \
         'second_remaining': context.get_remaining_time_in_millis(),\n    \
     }\n";

/// One row of the client-context table: the document the call sends, and the four members
/// the handler must read back off its context.
struct ContextCase {
    case: &'static str,
    document: Option<Value>,
    custom: Value,
    environment: Value,
    installation: Value,
    title: Value,
}

/// The identity and the client context a handler reads off its context are OBJECTS carrying
/// their own members, whatever the invocation supplied for them.
///
/// The rows differ along what the rule generalises over: a call that sends no client context
/// at all, one that sends all three members, one that sends only part of them, and one whose
/// `client` names the mobile client's own members.
#[tokio::test]
async fn the_context_exposes_its_identity_and_client_context_as_objects() {
    let _stream = one_invocation_stream_at_a_time().await;
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    let name = "probe-context-members";
    create_function(&router, name, &[("index.py", CONTEXT_REPORT)], HANDLER, 10, false).await;

    let cases: Vec<ContextCase> = vec![
        ContextCase {
            case: "no client context at all",
            document: None,
            custom: Value::Null,
            environment: Value::Null,
            installation: Value::Null,
            title: Value::Null,
        },
        ContextCase {
            case: "every member of a client context",
            document: Some(json!({
                "custom": {"tenant": "blue"},
                "env": {"platform": "android"},
                "client": {"installation_id": "install-7", "app_title": "probe"},
            })),
            custom: json!({"tenant": "blue"}),
            environment: json!({"platform": "android"}),
            installation: json!("install-7"),
            title: json!("probe"),
        },
        ContextCase {
            case: "a client context carrying only its custom members",
            document: Some(json!({"custom": {"tenant": "green"}})),
            custom: json!({"tenant": "green"}),
            environment: Value::Null,
            installation: Value::Null,
            title: Value::Null,
        },
        ContextCase {
            case: "a client context whose client names only its installation",
            document: Some(json!({"client": {"installation_id": "install-9"}})),
            custom: Value::Null,
            environment: Value::Null,
            installation: json!("install-9"),
            title: Value::Null,
        },
    ];

    for row in cases {
        let case = row.case;
        let document = row.document;
        let encoded = document.as_ref().map(|document| {
            base64::engine::general_purpose::STANDARD.encode(
                serde_json::to_vec(document).expect("the client context serializes"),
            )
        });
        let headers: Vec<(&str, &str)> = match encoded.as_deref() {
            Some(encoded) => vec![("x-amz-client-context", encoded)],
            None => Vec::new(),
        };
        let answered = invoke(&router, name, b"{}", &headers).await;
        assert_eq!(
            (answered.status, answered.header(FUNCTION_ERROR_HEADER)),
            (200, None),
            "case {case:?}: expected a clean 200, got {} {}",
            answered.status,
            answered.text()
        );
        // The identity is always an object, and both its members read back.
        assert_eq!(
            answered.member("identity_class").as_deref(),
            Some("CognitoIdentity"),
            "case {case:?}: expected the identity to be its own object, got {}",
            answered.text()
        );
        assert_eq!(
            (
                answered.value("cognito_identity_id"),
                answered.value("cognito_identity_pool_id"),
            ),
            (Value::Null, Value::Null),
            "case {case:?}: expected both identity members to read as absent, got {}",
            answered.text()
        );
        // The client context is an object exactly when the call sent one.
        assert_eq!(
            answered.value("client_context_absent"),
            json!(document.is_none()),
            "case {case:?}: expected a client context only where the call sent one, got {}",
            answered.text()
        );
        if document.is_some() {
            assert_eq!(
                answered.member("client_context_class").as_deref(),
                Some("ClientContext"),
                "case {case:?}: expected the client context to be its own object, got {}",
                answered.text()
            );
        }
        assert_eq!(
            (
                answered.value("custom"),
                answered.value("env"),
                answered.value("installation_id"),
                answered.value("app_title"),
            ),
            (row.custom, row.environment, row.installation, row.title),
            "case {case:?}: expected the client context's members as the call sent them, \
             got {}",
            answered.text()
        );
    }

    // The time the invocation has left is bounded by the function's own Timeout and never
    // grows: two readings inside one run count down rather than up. The MARGIN the handler is
    // entered with — that the runtime's own work of receiving the invocation is not charged to
    // the timeout — is pinned by [`crate::invocation_remaining_time_countdown`], which is
    // where the entry slack this emulator allows is stated.
    let answered = invoke(&router, name, b"{}", &[]).await;
    let first = answered.value("first_remaining").as_i64().unwrap_or_default();
    let second = answered.value("second_remaining").as_i64().unwrap_or_default();
    assert!(
        first > 0 && first <= 10_000,
        "case the time remaining: expected a reading inside the configured 10 s timeout, \
         got {first} in {}",
        answered.text()
    );
    assert!(
        second <= first,
        "case the time remaining: expected the second reading not to exceed the first, got \
         {second} after {first} in {}",
        answered.text()
    );
    delete_function(&router, "the context members", name).await;
}

/// `context.invoked_function_arn` is the ARN the CALL addressed, carrying exactly the
/// qualifier the call named.
///
/// The rows differ along what the rule generalises over: a call that names no qualifier, one
/// that names the unpublished version by name, one that names a published version number,
/// one that names an alias, and one that carries the qualifier in the `FunctionName` itself.
#[tokio::test]
async fn the_invoked_arn_carries_the_qualifier_the_call_named() {
    let _stream = one_invocation_stream_at_a_time().await;
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    let name = "probe-invoked-arn";
    create_function(&router, name, &[("index.py", CONTEXT_REPORT)], HANDLER, 10, true).await;
    let alias = call(
        &router,
        "POST",
        &format!("{FUNCTIONS_URI}/{name}/aliases"),
        &serde_json::to_vec(&json!({ "Name": "live", "FunctionVersion": "1" }))
            .expect("the alias body serializes"),
        &[],
    )
    .await;
    assert_eq!(
        alias.status,
        201,
        "CreateAlias must succeed, got {} {}",
        alias.status,
        alias.text()
    );

    let unqualified = format!("arn:aws:lambda:us-east-1:000000000000:function:{name}");
    // (case, the FunctionName the call names, the Qualifier it names, the ARN the handler
    // must read)
    let cases: &[(&str, String, Option<&str>, String)] = &[
        (
            "no qualifier at all",
            name.to_string(),
            None,
            unqualified.clone(),
        ),
        (
            "the unpublished version named by name",
            name.to_string(),
            Some("$LATEST"),
            format!("{unqualified}:$LATEST"),
        ),
        (
            "a published version number",
            name.to_string(),
            Some("1"),
            format!("{unqualified}:1"),
        ),
        (
            "an alias",
            name.to_string(),
            Some("live"),
            format!("{unqualified}:live"),
        ),
        (
            "the qualifier carried in the name",
            format!("{name}:1"),
            None,
            format!("{unqualified}:1"),
        ),
        (
            "the unpublished version carried in the name",
            format!("{name}:$LATEST"),
            None,
            format!("{unqualified}:$LATEST"),
        ),
    ];

    for (case, reference, qualifier, expected) in cases {
        let answered = invoke_qualified(&router, reference, *qualifier, b"{}", &[]).await;
        assert_eq!(
            (answered.status, answered.header(FUNCTION_ERROR_HEADER)),
            (200, None),
            "case {case:?}: expected a clean 200, got {} {}",
            answered.status,
            answered.text()
        );
        assert_eq!(
            answered.member("arn").as_deref(),
            Some(expected.as_str()),
            "case {case:?}: expected the ARN the call addressed, got {}",
            answered.text()
        );
    }
    delete_function(&router, "the invoked ARN", name).await;
}

/// One row of the initialization table: the package, the handler string, the class and the
/// start of the message the failure must carry, and whether an invocation was bound to the
/// run when it failed.
struct FailureCase {
    case: &'static str,
    entries: &'static [(&'static str, &'static str)],
    handler: &'static str,
    error_type: &'static str,
    message: &'static str,
    bound: bool,
}

/// The class an INITIALIZATION failure is reported under, and the invocation its document
/// names.
///
/// The rows differ along what the rule generalises over: which part of resolving the handler
/// failed — the name, the module's own identity, its import, its syntax, its top-level code,
/// its entry point — and, for the last row, a failure of the handler's own RUN, which is
/// bound to the invocation that made it.
#[tokio::test]
async fn an_initialization_failure_is_classified_by_what_failed_and_names_no_invocation() {
    let _stream = one_invocation_stream_at_a_time().await;
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);

    let cases: &[FailureCase] = &[
        FailureCase {
            case: "a handler name that does not split",
            entries: &[("index.py", "def lambda_handler(event, context):\n    return 1\n")],
            handler: "lambda_handler",
            error_type: "Runtime.MalformedHandlerName",
            message: "Bad handler 'lambda_handler': ",
            bound: false,
        },
        FailureCase {
            case: "a handler module the interpreter itself is built with",
            entries: &[("index.py", "def lambda_handler(event, context):\n    return 1\n")],
            handler: "sys.lambda_handler",
            error_type: "Runtime.BuiltInModuleConflict",
            message: "Cannot use built-in module sys as a handler module",
            bound: false,
        },
        FailureCase {
            case: "a handler module the interpreter reports as built in",
            entries: &[("index.py", "def lambda_handler(event, context):\n    return 1\n")],
            handler: "time.lambda_handler",
            error_type: "Runtime.BuiltInModuleConflict",
            message: "Cannot use built-in module time as a handler module",
            bound: false,
        },
        FailureCase {
            case: "a module that does not import",
            entries: &[("index.py", "import no_such_module_at_all\n")],
            handler: HANDLER,
            error_type: "Runtime.ImportModuleError",
            message: "Unable to import module 'index': ",
            bound: false,
        },
        FailureCase {
            case: "a dotted module that does not import",
            entries: &[("package/handler.py", "import no_such_module_at_all\n")],
            handler: "package.handler.lambda_handler",
            error_type: "Runtime.ImportModuleError",
            message: "Unable to import module 'package.handler': ",
            bound: false,
        },
        FailureCase {
            case: "a module that does not compile",
            entries: &[("index.py", "def lambda_handler(event, context)\n    return 1\n")],
            handler: HANDLER,
            error_type: "Runtime.UserCodeSyntaxError",
            message: "Syntax error in module 'index': ",
            bound: false,
        },
        FailureCase {
            case: "a module whose own top-level code raised",
            entries: &[(
                "index.py",
                "raise LookupError('the table is not here')\n\n\n\
                 def lambda_handler(event, context):\n    return 1\n",
            )],
            handler: HANDLER,
            error_type: "LookupError",
            message: "the table is not here",
            bound: false,
        },
        FailureCase {
            case: "a module that defines no such entry point",
            entries: &[("index.py", "def other(event, context):\n    return 1\n")],
            handler: HANDLER,
            error_type: "Runtime.HandlerNotFound",
            message: "Handler 'lambda_handler' missing on module 'index'",
            bound: false,
        },
        FailureCase {
            case: "a handler that raised while it ran",
            entries: &[(
                "index.py",
                "def lambda_handler(event, context):\n    raise ValueError('boom')\n",
            )],
            handler: HANDLER,
            error_type: "ValueError",
            message: "boom",
            bound: true,
        },
    ];

    for row in cases {
        let case = row.case;
        let name = "probe-initialization-class";
        create_function(&router, name, row.entries, row.handler, 10, false).await;
        let answered = invoke(&router, name, b"{}", &[]).await;
        if is_host_bound(answered.status, &answered.text()) {
            delete_function(&router, case, name).await;
            continue;
        }
        assert_eq!(
            (
                answered.status,
                answered.header(FUNCTION_ERROR_HEADER).as_deref()
            ),
            (200, Some("Unhandled")),
            "case {case:?}: expected a 200 reporting the function's failure, got {} {}",
            answered.status,
            answered.text()
        );
        assert_eq!(
            answered.member("errorType").as_deref(),
            Some(row.error_type),
            "case {case:?}: expected the class {}, got {}",
            row.error_type,
            answered.text()
        );
        let reported = answered.member("errorMessage").unwrap_or_default();
        assert!(
            reported.starts_with(row.message),
            "case {case:?}: expected a message opening {:?}, got {reported:?}",
            row.message
        );
        // A failure of the initialization belongs to no invocation, so its document names
        // none; a failure of the handler's own run names the invocation it failed.
        let expected_id = if row.bound {
            answered.request_id()
        } else {
            String::new()
        };
        assert_eq!(
            answered.member("requestId").as_deref(),
            Some(expected_id.as_str()),
            "case {case:?}: expected the document to name {expected_id:?}, got {}",
            answered.text()
        );
        delete_function(&router, case, name).await;
    }
}

/// The PAYLOAD a call sends: which payloads are refused to the caller, and which reach the
/// runtime.
///
/// The rows differ along what the rule generalises over: a payload that is a complete value,
/// one that is empty, one holding nothing but whitespace, one whose first value is complete
/// and carries data after it, and one no value can be read out of at all.
#[tokio::test]
async fn a_payload_is_refused_only_where_no_json_value_can_be_read_out_of_it() {
    let _stream = one_invocation_stream_at_a_time().await;
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    let name = "probe-payload-handoff";
    create_function(
        &router,
        name,
        &[(
            "index.py",
            "def lambda_handler(event, context):\n    return {'event': event}\n",
        )],
        HANDLER,
        10,
        false,
    )
    .await;

    /// What a payload must be answered with.
    enum Expected {
        /// A clean 200 carrying the event the handler was given.
        Served(Value),
        /// The modelled 400, carrying exactly this message.
        Refused(String),
        /// A 200 reporting the runtime's own failure: this class, and exactly this
        /// message — the parser's own diagnostic, under the runtime's own frame for it.
        Unmarshalled(&'static str, &'static str),
    }

    let frame = "Could not parse request body into json: Could not parse payload into json: ";
    let cases: Vec<(&str, Vec<u8>, Expected)> = vec![
        (
            "a complete object",
            br#"{"k":1}"#.to_vec(),
            Expected::Served(json!({"k": 1})),
        ),
        (
            "a complete array",
            b"[1,2]".to_vec(),
            Expected::Served(json!([1, 2])),
        ),
        (
            "no payload at all",
            Vec::new(),
            Expected::Served(json!({})),
        ),
        (
            "nothing but whitespace",
            b"  \n\t ".to_vec(),
            Expected::Refused(format!("{frame}No payload content found")),
        ),
        (
            "a single space",
            b" ".to_vec(),
            Expected::Refused(format!("{frame}No payload content found")),
        ),
        (
            "a complete value with data after it",
            br#"{"k":1}xx"#.to_vec(),
            Expected::Unmarshalled(
                "Runtime.UnmarshalError",
                "Unable to unmarshal input: Extra data: line 1 column 8 (char 7)",
            ),
        ),
        (
            "a complete value with a second value after it",
            br#"{"k":1} {"k":2}"#.to_vec(),
            Expected::Unmarshalled(
                "Runtime.UnmarshalError",
                "Unable to unmarshal input: Extra data: line 1 column 9 (char 8)",
            ),
        ),
        (
            "a bare word where a value belongs",
            b"not json".to_vec(),
            Expected::Refused(format!(
                "{frame}Unrecognized token 'not': was expecting (JSON String, Number, Array, \
                 Object or token 'null', 'true' or 'false')\n at [Source: REDACTED \
                 (`StreamReadFeature.INCLUDE_SOURCE_IN_LOCATION` disabled); line: 1, column: 1]"
            )),
        ),
    ];

    for (case, payload, expected) in cases {
        let answered = invoke(&router, name, &payload, &[]).await;
        match expected {
            Expected::Served(event) => {
                assert_eq!(
                    (
                        answered.status,
                        answered.header(FUNCTION_ERROR_HEADER).as_deref()
                    ),
                    (200, None),
                    "case {case:?}: expected a clean 200, got {} {}",
                    answered.status,
                    answered.text()
                );
                assert_eq!(
                    answered.value("event"),
                    event,
                    "case {case:?}: expected the handler to be given {event}, got {}",
                    answered.text()
                );
            }
            Expected::Refused(message) => {
                assert_eq!(
                    answered.status,
                    400,
                    "case {case:?}: expected the modelled refusal, got {} {}",
                    answered.status,
                    answered.text()
                );
                assert_eq!(
                    answered.member("message").as_deref(),
                    Some(message.as_str()),
                    "case {case:?}: expected the message {message:?}, got {}",
                    answered.text()
                );
            }
            Expected::Unmarshalled(error_type, message) => {
                assert_eq!(
                    (
                        answered.status,
                        answered.header(FUNCTION_ERROR_HEADER).as_deref()
                    ),
                    (200, Some("Unhandled")),
                    "case {case:?}: expected the runtime's own failure, got {} {}",
                    answered.status,
                    answered.text()
                );
                assert_eq!(
                    answered.member("errorType").as_deref(),
                    Some(error_type),
                    "case {case:?}: expected the class {error_type}, got {}",
                    answered.text()
                );
                let reported = answered.member("errorMessage").unwrap_or_default();
                assert_eq!(
                    reported, message,
                    "case {case:?}: expected the message {message:?}, got {reported:?}"
                );
            }
        }
    }
    delete_function(&router, "the payload hand-off", name).await;
}
