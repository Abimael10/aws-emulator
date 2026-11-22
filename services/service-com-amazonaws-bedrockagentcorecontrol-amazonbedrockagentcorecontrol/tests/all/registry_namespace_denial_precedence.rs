// SPDX-License-Identifier: CC0-1.0

//! WHERE THE AGENTCORE REGISTRY NAMESPACE'S DENIAL SITS IN THE ORDER, driven end to end over
//! the wire path (a `restJson1` request through the multi-service router into the mounted
//! service), so what is asserted is what a client holding those credentials sees.
//!
//! THE RULE UNDER TEST: a call of an action namespace this account may not use is refused
//! HTTP 403 `AccessDeniedException` with the sentence this endpoint denies a call in — the
//! principal that signed, as `sts:GetCallerIdentity` reports it, and the action
//! `bedrock-agentcore:<OperationName>` the route's own operation is performed under — and that
//! refusal PRECEDES every statement about the request: the constraints its members break, the
//! existence of the resource its route addresses, and the form the addressing identifier was
//! carried in. The denial is specific to that namespace: the same credentials keep being
//! served every other family of this endpoint.
//!
//! `tests/all/registry_entitlement_gate.rs` pins the rule over the registry plane's twelve
//! operations and over the kind of principal blamed (the account's own published credential
//! and an assumed-role session allowed `bedrock-agentcore:*`). This suite pins the ORDER —
//! the dimension that decides whether a client feature-detecting the namespace sees the denial
//! or a verdict about its own request — over the registry reads and writes a client reaches
//! the namespace through, each sent in a form that some OTHER refusal would otherwise answer.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_bedrockagentcorecontrol_amazonbedrockagentcorecontrol::EmulatedService;

/// The Region every row signs for.
const REGION: &str = "us-east-1";

/// An access key id neither identity store holds a record for: the account's own published
/// credential, whose principal is the one `sts:GetCallerIdentity` answers for it.
const AMBIENT_KEY_ID: &str = "AKIDEXAMPLE";

/// The error code `restJson1` carries in `x-amzn-errortype`, at the status the model gives it
/// (`AccessDeniedException`, `httpError` 403 on every registry operation).
const ACCESS_DENIED: &str = "AccessDeniedException";
const ACCESS_DENIED_STATUS: u16 = 403;

/// A `registryId` inside `RegistryId`'s shape (`^[a-zA-Z0-9]{12,16}$`,
/// `smithy_query operation=GetRegistry`). It names no stored registry: a refusal that precedes
/// every handler cannot depend on one.
const REGISTRY_ID: &str = "aaaaaaaaaaaa";

/// `RegistryName`'s maximum length (len 1..64, `smithy_query operation=CreateRegistry`), and
/// `ClientToken`'s minimum (len 33..256, same call): a row one past each bound is a request
/// the modelled constraint walk would refuse.
const NAME_MAX_LENGTH: usize = 64;
const CLIENT_TOKEN_MIN_LENGTH: usize = 33;

/// `MaxResults`' minimum on a registry listing (range 1..100,
/// `smithy_query operation=ListRegistries`).
const MAX_RESULTS_MIN: i64 = 1;

/// The authorizer type whose configuration the model requires alongside it
/// (`RegistryAuthorizerType`, `smithy_query operation=CreateRegistry`): a create naming it and
/// carrying no configuration is a request the service refuses on its own.
const CUSTOM_JWT: &str = "CUSTOM_JWT";

/// One answer: the status, the error code, the `message` and the whole body.
struct Answer {
    status: u16,
    code: String,
    message: String,
    body: Value,
}

/// The endpoint under test: one router over one engine, carrying this service.
struct Endpoint {
    router: MultiServiceRouter,
}

impl Endpoint {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::default();
        router.register_service(EmulatedService::new(engine));
        Self { router }
    }

    /// Send one call signed with the account's own published credential.
    async fn send(&self, method: &str, uri: &str, body: Option<&Value>) -> Answer {
        let payload = body.map_or_else(Bytes::new, |value| {
            Bytes::from(serde_json::to_vec(value).expect("the request body serializes"))
        });
        let mut request = http::Request::builder()
            .method(method)
            .uri(uri)
            // The SigV4 credential scope routes the request, names its Region and names the
            // identity it speaks for; the signature itself is not checked here.
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential={AMBIENT_KEY_ID}/20260101/{REGION}/\
                     bedrock-agentcore/aws4_request, SignedHeaders=host, Signature=abc"
                ),
            )
            .header("host", "bedrock-agentcore-control.amazonaws.com");
        if body.is_some() {
            request = request.header("content-type", "application/json");
        }
        let request = request.body(payload).expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let code = response
            .headers()
            .get("x-amzn-errortype")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();
        let parsed: Value = if response.body().is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(response.body()).unwrap_or(Value::Null)
        };
        Answer {
            status,
            code,
            message: parsed
                .pointer("/message")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            body: parsed,
        }
    }
}

/// The principal the account's own published credential speaks for, as
/// `sts:GetCallerIdentity` reports it — the value the refusal blames.
fn ambient_principal() -> String {
    ::service_common::caller_principal::ambient_user_arn()
}

/// The sentence this endpoint denies a call in: the principal that signed and the action it
/// asked for, and nothing further.
fn not_authorized(principal: &str, operation: &str) -> String {
    format!("User: {principal} is not authorized to perform: bedrock-agentcore:{operation}")
}

/// Assert `answer` is that denial of `operation`, and that it reports nothing of the plane it
/// refused to touch.
fn assert_denied(answer: &Answer, case: &str, operation: &str) {
    let expected = not_authorized(&ambient_principal(), operation);
    assert_eq!(
        (
            answer.status,
            answer.code.as_str(),
            answer.message.as_str()
        ),
        (ACCESS_DENIED_STATUS, ACCESS_DENIED, expected.as_str()),
        "case {case:?}: expected {ACCESS_DENIED_STATUS} {ACCESS_DENIED} {expected:?}, got {} \
         {:?} with {:?}",
        answer.status,
        answer.code,
        answer.body
    );
    for member in ["registryArn", "registries", "status", "name"] {
        assert!(
            answer.body.get(member).is_none(),
            "case {case:?}: a denied call reports no {member}, got {:?}",
            answer.body
        );
    }
}

/// A registry call as a client sends it: the operation its route names, the route and a body
/// for the routes that take one.
struct Call {
    case: &'static str,
    operation: &'static str,
    method: &'static str,
    uri: String,
    body: Option<Value>,
}

/// The ARN form of a registry identifier, percent-encoded for the single path segment the
/// modelled `@http` label occupies. `RegistryIdentifier` admits either form — the bare id or
/// the registry's ARN (`smithy_query operation=GetRegistry`) — so the denial must not depend on
/// which one the caller addressed its registry by.
fn registry_arn_label() -> String {
    format!(
        "arn:aws:bedrock-agentcore:{REGION}:{}:registry/{REGISTRY_ID}",
        ::engine::emulator_account_id()
    )
    .replace(':', "%3A")
    .replace('/', "%2F")
}

/// RULE: each registry action is denied under its own action name, whichever route, method,
/// query and identifier form the call carried.
#[tokio::test]
async fn each_registry_action_is_denied_under_its_own_action_name() {
    let endpoint = Endpoint::new();
    // The rows span the four actions a client reaches the namespace through, the HTTP method,
    // whether the call carries a body, the query the SDK appends for a page or a filter, and
    // the identifier form a route's label was addressed by.
    let calls = vec![
        Call {
            case: "a create of a registry",
            operation: "CreateRegistry",
            method: "POST",
            uri: "/registries".to_string(),
            body: Some(json!({"name": "denial-probe"})),
        },
        Call {
            case: "a create carrying every optional member",
            operation: "CreateRegistry",
            method: "POST",
            uri: "/registries".to_string(),
            body: Some(json!({
                "name": "denial-probe-configured",
                "description": "probing the namespace",
                "authorizerType": "AWS_IAM",
                "approvalConfiguration": {"autoApproval": true},
                "clientToken": "a".repeat(CLIENT_TOKEN_MIN_LENGTH),
            })),
        },
        Call {
            case: "a bare listing",
            operation: "ListRegistries",
            method: "GET",
            uri: "/registries".to_string(),
            body: None,
        },
        Call {
            case: "a listing asking for one page of one",
            operation: "ListRegistries",
            method: "GET",
            uri: format!("/registries?maxResults={MAX_RESULTS_MIN}"),
            body: None,
        },
        Call {
            case: "a listing filtered by status and authorizer type",
            operation: "ListRegistries",
            method: "GET",
            uri: "/registries?status=READY&authorizerType=AWS_IAM".to_string(),
            body: None,
        },
        Call {
            case: "a read of one registry by its id",
            operation: "GetRegistry",
            method: "GET",
            uri: format!("/registries/{REGISTRY_ID}"),
            body: None,
        },
        Call {
            case: "a read of one registry by its ARN",
            operation: "GetRegistry",
            method: "GET",
            uri: format!("/registries/{}", registry_arn_label()),
            body: None,
        },
        Call {
            case: "a delete of one registry by its id",
            operation: "DeleteRegistry",
            method: "DELETE",
            uri: format!("/registries/{REGISTRY_ID}"),
            body: None,
        },
        Call {
            case: "a delete of one registry by its ARN",
            operation: "DeleteRegistry",
            method: "DELETE",
            uri: format!("/registries/{}", registry_arn_label()),
            body: None,
        },
    ];

    for call in calls {
        let answer = endpoint
            .send(call.method, &call.uri, call.body.as_ref())
            .await;
        assert_denied(&answer, call.case, call.operation);
    }
}

/// RULE's ORDER: the denial precedes every statement about the request — a call whose members
/// break the model's constraints, whose own prose rule the service would refuse, or whose route
/// addresses a registry this account does not hold, is answered the identical denial and never
/// a verdict about itself.
#[tokio::test]
async fn the_denial_precedes_every_statement_about_the_request() {
    let endpoint = Endpoint::new();
    // The rows span the kinds of statement that would otherwise answer first: an absent
    // `@required` member, a member one past a length bound, a member outside its pattern, a
    // value outside a numeric range, a value outside an enum, a rule the model states in prose,
    // and a resource the account does not hold.
    let calls = vec![
        Call {
            case: "a create carrying no name at all",
            operation: "CreateRegistry",
            method: "POST",
            uri: "/registries".to_string(),
            body: Some(json!({})),
        },
        Call {
            case: "a create whose name is one character past the maximum",
            operation: "CreateRegistry",
            method: "POST",
            uri: "/registries".to_string(),
            body: Some(json!({"name": "a".repeat(NAME_MAX_LENGTH + 1)})),
        },
        Call {
            case: "a create whose name is outside the member's pattern",
            operation: "CreateRegistry",
            method: "POST",
            uri: "/registries".to_string(),
            body: Some(json!({"name": "-leading-dash"})),
        },
        Call {
            case: "a create whose clientToken is one character short of the minimum",
            operation: "CreateRegistry",
            method: "POST",
            uri: "/registries".to_string(),
            body: Some(json!({
                "name": "denial-probe-token",
                "clientToken": "a".repeat(CLIENT_TOKEN_MIN_LENGTH - 1),
            })),
        },
        Call {
            case: "a create naming the authorizer type whose configuration it omits",
            operation: "CreateRegistry",
            method: "POST",
            uri: "/registries".to_string(),
            body: Some(json!({"name": "denial-probe-jwt", "authorizerType": CUSTOM_JWT})),
        },
        Call {
            case: "a listing asking for fewer results than the range admits",
            operation: "ListRegistries",
            method: "GET",
            uri: format!("/registries?maxResults={}", MAX_RESULTS_MIN - 1),
            body: None,
        },
        Call {
            case: "a listing filtered by a status outside the enum",
            operation: "ListRegistries",
            method: "GET",
            uri: "/registries?status=NOT_A_REGISTRY_STATUS".to_string(),
            body: None,
        },
        Call {
            case: "a read of a registry the account does not hold",
            operation: "GetRegistry",
            method: "GET",
            uri: format!("/registries/{REGISTRY_ID}"),
            body: None,
        },
        Call {
            case: "a delete of a registry the account does not hold",
            operation: "DeleteRegistry",
            method: "DELETE",
            uri: format!("/registries/{REGISTRY_ID}"),
            body: None,
        },
    ];

    for call in calls {
        let answer = endpoint
            .send(call.method, &call.uri, call.body.as_ref())
            .await;
        assert_denied(&answer, call.case, call.operation);
    }
}

/// RULE's SCOPE: the denial is the namespace's, not a blanket refusal of the endpoint — the
/// same credentials are served every other family, each reporting its own collection.
#[tokio::test]
async fn the_denial_is_specific_to_the_registry_namespace() {
    let endpoint = Endpoint::new();

    let denied = endpoint
        .send("POST", "/registries", Some(&json!({"name": "denial-probe"})))
        .await;
    assert_denied(&denied, "the registry create", "CreateRegistry");

    // (case, the route, the collection member the listing reports)
    let served = [
        ("ListAgentRuntimes", "/runtimes/", "agentRuntimes"),
        ("ListMemories", "/memories/", "memories"),
    ];
    for (case, uri, member) in served {
        let answer = endpoint.send("POST", uri, Some(&json!({}))).await;
        assert_eq!(
            answer.status,
            200,
            "case {case:?}: expected HTTP 200 for a call outside the registry namespace, got {} \
             with {:?}",
            answer.status,
            answer.body
        );
        assert!(
            answer.body.get(member).is_some_and(Value::is_array),
            "case {case:?}: expected the {member} list, got {:?}",
            answer.body
        );
    }
}
