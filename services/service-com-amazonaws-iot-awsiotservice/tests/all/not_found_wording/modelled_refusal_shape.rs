// SPDX-License-Identifier: CC0-1.0

//! The error SHAPE a constraint refusal of this service names, and the half of a clause
//! that states a COLLECTION — the two halves of that envelope its parent suite
//! ([`super`]) does not pin, end to end over HTTP.
//!
//! The rules under test, neither of them specific to one operation or one value:
//!
//! * the shape named is the one the OPERATION models for an invalid request — the classic
//!   IoT surface's legacy `InvalidRequestException` (named qualified with this service's
//!   API doc namespace), and `ValidationException` for the operations of the
//!   software-package catalog and the commands plane, whose models declare it
//!   (`smithy_query(query="operation",operation="CreateCommand")`);
//! * the clause is AWS's one-per-violation sentence, and where the rejected value is a
//!   COLLECTION it quotes that collection — an empty list as `[]`, an empty map as `{}` —
//!   rather than a count of what it held, while a clause broken by the caller's own TEXT
//!   states the bound and no value, which is the rule the sibling suite pins.
//!
//! The rows span what those rules generalise over: which plane the operation belongs to,
//! which constraint class the request breaks (a list's `@length`, a map's `@length`, a
//! string's `@length`, a `@pattern`, an absent `@required` member), and where the member is
//! bound (the URI path and the body).

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region every call below is signed for.
const REGION: &str = "eu-central-1";

/// The error shape the software-package and commands planes model for an invalid request,
/// as the wire names it.
const VALIDATION_EXCEPTION: &str = "ValidationException";

/// The error shape the classic IoT surface refuses an invalid request with, as this
/// service names it on the wire: the shape name qualified with its API doc namespace.
const QUALIFIED_INVALID_REQUEST: &str =
    "InvalidRequestException:http://iot.amazonaws.com/doc/2015-10-02/";

/// A SigV4 `Authorization` header whose credential scope names the `iot` signing name and
/// [`REGION`]: the scope is how the router picks the claiming service. The signature is
/// never checked here — the harness turns request authentication off.
fn signed_for_iot() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDTESTIOT/20260101/{REGION}/iot/aws4_request, \
         SignedHeaders=host;x-amz-date, Signature=0000"
    )
}

/// One response, reduced to what a client branches on.
#[derive(Debug)]
struct Reply {
    status: u16,
    error_code: Option<String>,
    body: Value,
}

impl Reply {
    /// The `message` the error body carries, or the empty string when it carries none.
    fn message(&self) -> String {
        self.body
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }
}

/// The emulator under test: ONE shared engine with the IoT service mounted on it, reached
/// the way the production router reaches it.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(engine));
        Self { router }
    }

    async fn call(&self, method: &str, path: &str, body: Option<&Value>) -> Reply {
        let payload = body.map_or_else(Vec::new, |value| {
            serde_json::to_vec(value).expect("a request body serializes")
        });
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://iot.{REGION}.amazonaws.com{path}"))
            .header("authorization", signed_for_iot())
            .header("content-type", "application/json")
            .body(bytes::Bytes::from(payload))
            .expect("a well-formed request");

        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let error_code = response
            .headers()
            .get("x-amzn-errortype")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let bytes = response.into_body();
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        Reply {
            status,
            error_code,
            body,
        }
    }
}

/// One request the model's constraints refuse: what it is, how it is addressed, the error
/// shape the wire must name and the clause after AWS's envelope.
struct Refused {
    case: &'static str,
    method: &'static str,
    path: String,
    body: Option<Value>,
    /// The `x-amzn-errortype` the response must name.
    error_code: &'static str,
    /// What follows `1 validation error detected: ` in the message.
    clause: String,
}

/// A `CreateCommandRequest` body carrying `mandatory_parameters` and nothing else.
fn create_command_body(mandatory_parameters: Value) -> Value {
    json!({ "mandatoryParameters": mandatory_parameters })
}

/// Every row of the refusal table.
fn refused_requests() -> Vec<Refused> {
    vec![
        // A COLLECTION member outside its `@length`: `CommandParameterList` is declared
        // `@length(min: 1)` on `CreateCommand$mandatoryParameters`
        // (`smithy_query(query="operation",operation="CreateCommand")`), and the clause
        // quotes the list the request carried.
        Refused {
            case: "CreateCommand with an empty mandatoryParameters",
            method: "PUT",
            path: "/commands/probe-command".to_string(),
            body: Some(create_command_body(json!([]))),
            error_code: VALIDATION_EXCEPTION,
            clause: "Value '[]' at 'mandatoryParameters' failed to satisfy constraint: \
                     Member must have length greater than or equal to 1"
                .to_string(),
        },
        // The same class on a MAP member of the other plane: `CreatePackage$tags` is a
        // map declared `@length(min: 1, max: 50)`
        // (`smithy_query(query="operation",operation="CreatePackage")`), and the clause
        // quotes the map the request carried.
        Refused {
            case: "CreatePackage with an empty tags map",
            method: "PUT",
            path: "/packages/probe-package".to_string(),
            body: Some(json!({ "tags": {} })),
            error_code: VALIDATION_EXCEPTION,
            clause: "Value '{}' at 'tags' failed to satisfy constraint: Member must have \
                     length greater than or equal to 1"
                .to_string(),
        },
        // A URI LABEL outside its `@pattern` on the commands plane: `CommandId` is
        // modelled `^[a-zA-Z0-9_-]+$`, and this service states a `@pattern` clause with
        // the expression and no value.
        Refused {
            case: "CreateCommand under an id outside its pattern",
            method: "PUT",
            path: "/commands/probe.command".to_string(),
            body: Some(create_command_body(json!([{ "name": "signal", "type": "STRING" }]))),
            error_code: VALIDATION_EXCEPTION,
            clause: "Value at 'commandId' failed to satisfy constraint: Member must satisfy \
                     regular expression pattern: [a-zA-Z0-9_-]+"
                .to_string(),
        },
        // A STRING member past its `@length` maximum on the commands plane:
        // `UpdateCommand$displayName` is `length 0..64`
        // (`smithy_query(query="operation",operation="UpdateCommand")`). This service states
        // a length clause broken by the caller's own text with NO value — the clause names
        // the bound — so what the plane changes is the SHAPE the refusal names. The command
        // does not exist, which is immaterial — the constraint is read before any handler
        // runs.
        Refused {
            case: "UpdateCommand with a displayName past its maximum",
            method: "PATCH",
            path: "/commands/probe-command".to_string(),
            body: Some(json!({ "displayName": "d".repeat(65) })),
            error_code: VALIDATION_EXCEPTION,
            clause: "Value at 'displayName' failed to satisfy constraint: Member must have \
                     length less than or equal to 64"
                .to_string(),
        },
        // A URI LABEL outside its `@pattern` on the package catalog: `PackageName` is
        // modelled `^[a-zA-Z0-9-_.]+$`.
        Refused {
            case: "CreatePackage under a name outside its pattern",
            method: "PUT",
            path: "/packages/probe:package".to_string(),
            body: Some(json!({})),
            error_code: VALIDATION_EXCEPTION,
            clause: "Value at 'packageName' failed to satisfy constraint: Member must satisfy \
                     regular expression pattern: [a-zA-Z0-9-_.]+"
                .to_string(),
        },
        // The CLASSIC surface, for the same two classes: an absent `@required` member and
        // a URI label outside its pattern are refused with the legacy shape, so the shape
        // named is the operation's and not this service's one answer for everything.
        Refused {
            case: "CreatePolicy with the required document absent",
            method: "POST",
            path: "/policies/probe-policy".to_string(),
            body: Some(json!({})),
            error_code: QUALIFIED_INVALID_REQUEST,
            clause: "Value at 'policyDocument' failed to satisfy constraint: Member must not \
                     be null"
                .to_string(),
        },
        Refused {
            case: "CreateDomainConfiguration on a name outside its pattern",
            method: "POST",
            path: "/domainConfigurations/IoT:reserved".to_string(),
            body: Some(json!({})),
            error_code: QUALIFIED_INVALID_REQUEST,
            clause: format!(
                "Value at 'domainConfigurationName' failed to satisfy constraint: Member must \
                 satisfy regular expression pattern: {}",
                r"[\w.-]+"
            ),
        },
    ]
}

/// RULE: a constraint refusal names the error shape its OPERATION models, and states AWS's
/// one-clause-per-violation envelope with the rejected value rendered as AWS renders one.
#[tokio::test]
async fn a_constraint_refusal_names_the_shape_its_operation_models() {
    for case in refused_requests() {
        // A fresh emulator per row: a constraint refusal is decided by the REQUEST, so it
        // must not depend on what another row left behind.
        let emulator = Emulator::new();
        let reply = emulator
            .call(case.method, &case.path, case.body.as_ref())
            .await;
        assert_eq!(
            reply.status, 400,
            "case {:?}: expected status 400, got {} {:?}",
            case.case, reply.status, reply.body
        );
        assert_eq!(
            reply.error_code.as_deref(),
            Some(case.error_code),
            "case {:?}: expected x-amzn-errortype {:?}, got {:?}",
            case.case,
            case.error_code,
            reply.error_code
        );
        let expected = format!("1 validation error detected: {}", case.clause);
        assert_eq!(
            reply.message(),
            expected,
            "case {:?}: expected message {expected:?}, got {:?}",
            case.case,
            reply.message()
        );
    }
}

/// RULE: the layer that states those refusals changes nothing about a request that
/// satisfies the model — the operation serves it exactly as it did.
#[tokio::test]
async fn a_request_inside_the_constraints_is_served_by_its_operation() {
    let emulator = Emulator::new();

    // A create the model admits, on each of the two planes served through that layer, read
    // back through the operation that reports it.
    let created = emulator
        .call(
            "PUT",
            "/commands/probe-command",
            Some(&create_command_body(
                json!([{ "name": "signal", "type": "STRING" }]),
            )),
        )
        .await;
    assert_eq!(
        created.status, 200,
        "a create inside the constraints must be served, got {} {:?}",
        created.status, created.body
    );
    let read = emulator.call("GET", "/commands/probe-command", None).await;
    assert_eq!(
        read.status, 200,
        "the created command must be readable, got {} {:?}",
        read.status, read.body
    );
    assert_eq!(
        read.body.get("commandId").and_then(Value::as_str),
        Some("probe-command"),
        "the read must report the id the command was created under, got {:?}",
        read.body
    );

    let package = emulator
        .call(
            "PUT",
            "/packages/probe-package",
            Some(&json!({ "description": "a package the model admits" })),
        )
        .await;
    assert_eq!(
        package.status, 200,
        "a package create inside the constraints must be served, got {} {:?}",
        package.status, package.body
    );
    let package_read = emulator.call("GET", "/packages/probe-package", None).await;
    assert_eq!(
        package_read.status, 200,
        "the created package must be readable, got {} {:?}",
        package_read.status, package_read.body
    );
    assert_eq!(
        package_read
            .body
            .get("packageName")
            .and_then(Value::as_str),
        Some("probe-package"),
        "the read must report the name the package was created under, got {:?}",
        package_read.body
    );
}
