// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the layer operations addressed by an ARN that names ANOTHER
//! account, and for the revision precondition of a layer version's permissions policy.
//!
//! Two rules are under test here, both about what a layer call ANSWERS rather than about
//! what it stores:
//!
//! * every operation on a layer takes the layer's ARN as an alternative to its bare name,
//!   and that ARN names the account that OWNS the layer — so a call naming another account's
//!   layer is decided by the owning account's grant, which this endpoint holds none of: it is
//!   refused `403 AccessDeniedException` naming the caller, the action IAM publishes for the
//!   operation and the ARN the call addressed, rather than reported as a resource that does
//!   not exist. A layer of the account this endpoint serves is unaffected, however the
//!   request named it;
//! * a `RevisionId` precondition that did not hold on a layer version's policy is refused
//!   with the fixed sentence naming the read that reports that policy's latest revision,
//!   which is the layer policy read and not the function one.
//!
//! Every suite drives real HTTP requests through the router, so the assertions are about the
//! wire behaviour (status, body) rather than about any handler's internals.

use std::sync::Arc;

use base64::Engine as _;
use serde_json::{json, Value};
use service_com_amazonaws_lambda_awsgirapiservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The router picks a service by the SigV4 credential scope's service field, so every
/// request carries one naming `lambda` in `us-east-1`. Neither the key id nor the signature
/// is a credential: this harness turns request authentication off below, exactly as the
/// emulator runs it unless started with authentication on, so nothing here is verified.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=emulator-test-key/20260101/us-east-1/lambda/aws4_request, \
     SignedHeaders=host, Signature=not-verified";

/// The region the `AUTHORIZATION` scope above names, which every minted ARN carries.
const REGION: &str = "us-east-1";

/// The account id every emulator ARN carries (`engine::EMULATOR_ACCOUNT_ID`).
const ACCOUNT: &str = "000000000000";

/// An account id that is NOT this endpoint's, inside `LayerName`'s own `\d{12}` account
/// segment. The value matters only in that it differs from [`ACCOUNT`]; three more follow in
/// the table below, so no row depends on this one id.
const OTHER_ACCOUNT: &str = "111122223333";

/// The wire error code an authorization refusal carries.
const ACCESS_DENIED: &str = "AccessDeniedException";

/// The wire error code a precondition that did not hold carries.
const PRECONDITION_FAILED: &str = "PreconditionFailedException";

/// One service mounted on its own engine, reached exactly the way the production router
/// reaches it.
fn router_on(engine: &Arc<engine::InMemoryEngine>) -> MultiServiceRouter {
    let mut router = MultiServiceRouter::new();
    // Pinned off so the suite does not depend on how the emulator process was started;
    // these tests assert Lambda's behaviour, not the emulator's SigV4 check.
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(Arc::clone(engine)));
    router
}

/// What a dispatched request answered.
struct Answer {
    status: u16,
    body: bytes::Bytes,
}

impl Answer {
    /// The response body as JSON, or `Value::Null` for an empty body (the 204 answers).
    fn json(&self) -> Value {
        if self.body.is_empty() {
            return Value::Null;
        }
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }

    /// The response body as text, for a failure message.
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    /// The wire error code (`__type`) of an error response.
    fn error_code(&self) -> String {
        self.json()
            .get("__type")
            .and_then(Value::as_str)
            .map_or_else(|| self.text(), str::to_string)
    }

    /// The `message` of an error response, under either casing the JSON envelopes use.
    fn error_message(&self) -> String {
        let body = self.json();
        body.get("Message")
            .or_else(|| body.get("message"))
            .and_then(Value::as_str)
            .map_or_else(|| self.text(), str::to_string)
    }
}

/// Dispatch one JSON request, the shape every operation here uses.
async fn json_call(router: &MultiServiceRouter, method: &str, uri: &str, body: &Value) -> Answer {
    let payload = if body.is_null() {
        Vec::new()
    } else {
        serde_json::to_vec(body).expect("the test request body must serialize")
    };
    let request = http::Request::builder()
        .method(method)
        .uri(format!("http://lambda.us-east-1.amazonaws.com{uri}"))
        .header("host", "lambda.us-east-1.amazonaws.com")
        .header("authorization", AUTHORIZATION)
        .header("content-type", "application/json")
        .body(bytes::Bytes::from(payload))
        .expect("the test request must build");
    let response = router.dispatch(request).await;
    let (parts, body) = response.into_parts();
    Answer {
        status: parts.status.as_u16(),
        body,
    }
}

/// Percent-encode the characters an ARN carries that a URI would otherwise read as
/// structure.
fn encoded(value: &str) -> String {
    value
        .replace('%', "%25")
        .replace(':', "%3A")
        .replace('/', "%2F")
        .replace('&', "%26")
        .replace('=', "%3D")
        .replace(' ', "%20")
        .replace('+', "%2B")
        .replace('#', "%23")
}

/// Layer archive bytes of a given length; the API reports only the length and the digest.
fn archive_bytes(length: usize) -> Vec<u8> {
    (0..length)
        .map(|index| u8::try_from(index % 251).unwrap_or(0))
        .collect()
}

/// A `Content.ZipFile` member: restJson1 carries a blob base64-encoded in a JSON body.
fn zip_file(bytes: &[u8]) -> Value {
    Value::String(base64::engine::general_purpose::STANDARD.encode(bytes))
}

/// The unqualified ARN of the layer `name` in `account`.
fn layer_arn(account: &str, name: &str) -> String {
    format!("arn:aws:lambda:{REGION}:{account}:layer:{name}")
}

/// The ARN of version `version` of the layer `name` in `account`.
fn layer_version_arn(account: &str, name: &str, version: i64) -> String {
    format!("{}:{version}", layer_arn(account, name))
}

/// The principal the emulator's own long-term credential speaks for, which is what an
/// authorization refusal blames. Read from the one definition of it rather than written out,
/// so the suite pins the rule and not one run's identity.
fn caller() -> String {
    ::service_common::caller_principal::ambient_user_arn()
}

/// The sentence a refusal carries when the resource's own policy grants the caller nothing:
/// the principal, the action and the resource, exactly as
/// `service_common::iam_authorization` words it for every service in this tree.
fn not_authorized(action: &str, resource: &str) -> String {
    format!(
        "User: {caller} is not authorized to perform: {action} on resource: {resource} because \
         no resource-based policy allows the {action} action",
        caller = caller()
    )
}

/// Publish one version of a layer of THIS account, so every suite below runs against a store
/// that holds a layer under the same bare name the cross-account rows name.
async fn publish_own_layer(router: &MultiServiceRouter, name: &str) -> i64 {
    let body = json!({ "Content": { "ZipFile": zip_file(&archive_bytes(96)) } });
    let answer = json_call(
        router,
        "POST",
        &format!("/2018-10-31/layers/{name}/versions"),
        &body,
    )
    .await;
    assert_eq!(
        answer.status,
        201,
        "expected 201 from PublishLayerVersion on {name}, got {} {}",
        answer.status,
        answer.text()
    );
    answer
        .json()
        .get("Version")
        .and_then(Value::as_i64)
        .unwrap_or_else(|| panic!("expected a Version member, got {}", answer.text()))
}

/// One call on a layer, as a request: which operation it is, and how it is addressed.
struct Call {
    /// What makes this row different from the others.
    case: &'static str,
    /// The IAM action the IAM Service Authorization Reference publishes for the operation
    /// ("Actions defined by AWS Lambda"), which a refusal names.
    action: &'static str,
    /// The HTTP method the model binds the operation to.
    method: &'static str,
    /// The request URI, built from the `LayerName` value the row addresses the layer by.
    uri: fn(&str) -> String,
    /// The request body, empty for the operations that take none.
    body: fn() -> Value,
    /// Whether the resource the refusal names is the layer VERSION (the operations that
    /// address one) rather than the layer itself.
    versioned: bool,
}

/// The version number every row below addresses, and the one this suite's own layers are
/// published at.
const VERSION: i64 = 1;

/// Every layer operation this service serves that takes a `LayerName`, plus the by-ARN read
/// that takes a `LayerVersionArn`: each row is one operation, addressed by the ARN form of
/// its layer reference.
fn layer_calls() -> Vec<Call> {
    vec![
        Call {
            case: "GetLayerVersion",
            action: "lambda:GetLayerVersion",
            method: "GET",
            uri: |named| format!("/2018-10-31/layers/{}/versions/{VERSION}", encoded(named)),
            body: || Value::Null,
            versioned: true,
        },
        Call {
            case: "GetLayerVersionPolicy",
            action: "lambda:GetLayerVersionPolicy",
            method: "GET",
            uri: |named| {
                format!(
                    "/2018-10-31/layers/{}/versions/{VERSION}/policy",
                    encoded(named)
                )
            },
            body: || Value::Null,
            versioned: true,
        },
        Call {
            case: "AddLayerVersionPermission",
            action: "lambda:AddLayerVersionPermission",
            method: "POST",
            uri: |named| {
                format!(
                    "/2018-10-31/layers/{}/versions/{VERSION}/policy",
                    encoded(named)
                )
            },
            body: || {
                json!({
                    "StatementId": "shared",
                    "Action": "lambda:GetLayerVersion",
                    "Principal": ACCOUNT,
                })
            },
            versioned: true,
        },
        Call {
            case: "RemoveLayerVersionPermission",
            action: "lambda:RemoveLayerVersionPermission",
            method: "DELETE",
            uri: |named| {
                format!(
                    "/2018-10-31/layers/{}/versions/{VERSION}/policy/shared",
                    encoded(named)
                )
            },
            body: || Value::Null,
            versioned: true,
        },
        Call {
            case: "DeleteLayerVersion",
            action: "lambda:DeleteLayerVersion",
            method: "DELETE",
            uri: |named| format!("/2018-10-31/layers/{}/versions/{VERSION}", encoded(named)),
            body: || Value::Null,
            versioned: true,
        },
        Call {
            case: "ListLayerVersions",
            action: "lambda:ListLayerVersions",
            method: "GET",
            uri: |named| format!("/2018-10-31/layers/{}/versions", encoded(named)),
            body: || Value::Null,
            versioned: false,
        },
        Call {
            case: "PublishLayerVersion",
            action: "lambda:PublishLayerVersion",
            method: "POST",
            uri: |named| format!("/2018-10-31/layers/{}/versions", encoded(named)),
            body: || json!({ "Content": { "ZipFile": zip_file(&archive_bytes(32)) } }),
            versioned: false,
        },
    ]
}

/// RULE: a layer call addressed by an ARN of another account is refused `403
/// AccessDeniedException` naming the caller, the operation's own IAM action and the ARN it
/// addressed — on every operation that takes a layer reference, and for every account other
/// than this endpoint's own.
#[tokio::test]
async fn a_layer_of_another_account_is_refused_rather_than_reported_missing() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);

    // The bare name every row below names: this endpoint holds a layer under it, so a
    // refusal cannot be coming from the name being unknown here.
    let held = "cross-account-probe";
    let published = publish_own_layer(&router, held).await;
    assert_eq!(
        published, VERSION,
        "expected the first publish to be version {VERSION}, got {published}"
    );

    // Accounts other than this endpoint's, each a well-formed 12-digit account id: the
    // examples AWS documents, and the lowest and highest ids an account segment can carry.
    let accounts = [OTHER_ACCOUNT, "123456789012", "000000000001", "999999999999"];

    for call in layer_calls() {
        for account in accounts {
            let case = format!("{} addressed in account {account}", call.case);
            let named = layer_arn(account, held);
            let answer = json_call(&router, call.method, &(call.uri)(&named), &(call.body)()).await;
            assert_eq!(
                answer.status,
                403,
                "case {case:?}: expected 403, got {} {}",
                answer.status,
                answer.text()
            );
            assert_eq!(
                answer.error_code(),
                ACCESS_DENIED,
                "case {case:?}: expected {ACCESS_DENIED}, got {:?} in {}",
                answer.error_code(),
                answer.text()
            );
            let resource = if call.versioned {
                layer_version_arn(account, held, VERSION)
            } else {
                named.clone()
            };
            let expected = not_authorized(call.action, &resource);
            assert_eq!(
                answer.error_message(),
                expected,
                "case {case:?}: expected {expected:?}, got {:?}",
                answer.error_message()
            );
        }
    }

    // The by-ARN read takes the version's own ARN, so the version it addresses comes from
    // that value: the refusal names it exactly as the request did.
    for account in accounts {
        let case = format!("GetLayerVersionByArn addressed in account {account}");
        for version in [VERSION, 7] {
            let arn = layer_version_arn(account, held, version);
            let answer = json_call(
                &router,
                "GET",
                &format!("/2018-10-31/layers?find=LayerVersion&Arn={}", encoded(&arn)),
                &Value::Null,
            )
            .await;
            assert_eq!(
                answer.status,
                403,
                "case {case:?}: expected 403 for version {version}, got {} {}",
                answer.status,
                answer.text()
            );
            assert_eq!(
                answer.error_code(),
                ACCESS_DENIED,
                "case {case:?}: expected {ACCESS_DENIED}, got {:?} in {}",
                answer.error_code(),
                answer.text()
            );
            // Both read entry points are judged under the one action IAM publishes for the
            // read.
            let expected = not_authorized("lambda:GetLayerVersion", &arn);
            assert_eq!(
                answer.error_message(),
                expected,
                "case {case:?}: expected {expected:?}, got {:?}",
                answer.error_message()
            );
        }
    }

    // Every refused call changed nothing: the layer this account does hold still reads back
    // at the version it was published at, and no cross-account publish minted a second one.
    let read = json_call(
        &router,
        "GET",
        &format!("/2018-10-31/layers/{held}/versions/{VERSION}"),
        &Value::Null,
    )
    .await;
    assert_eq!(
        read.status,
        200,
        "expected the held layer version to still read back, got {} {}",
        read.status,
        read.text()
    );
    assert_eq!(
        read.json().get("LayerVersionArn"),
        Some(&json!(layer_version_arn(ACCOUNT, held, VERSION))),
        "expected the held version's own ARN, got {}",
        read.text()
    );
    let listed = json_call(
        &router,
        "GET",
        &format!("/2018-10-31/layers/{held}/versions"),
        &Value::Null,
    )
    .await;
    assert_eq!(
        listed
            .json()
            .get("LayerVersions")
            .and_then(Value::as_array)
            .map(Vec::len),
        Some(1),
        "expected the refused calls to have published and deleted nothing, got {}",
        listed.text()
    );
}

/// RULE: the same operations against a layer of THIS account are served, whether the request
/// names it by its bare name or by its ARN — the account the ARN carries is the only thing
/// the rule above turns on.
#[tokio::test]
async fn a_layer_of_this_account_is_served_however_the_request_names_it() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);

    // One layer per reference form, so the writes of one row cannot be what serves another.
    /// One way a request can name a layer of this account.
    struct Reference {
        /// What makes this row different from the other.
        case: &'static str,
        /// The bare layer name this row publishes under.
        held: &'static str,
        /// How the row addresses that layer.
        named: fn(&str) -> String,
    }

    let rows = [
        Reference {
            case: "a bare layer name",
            held: "own-by-name",
            named: |name| name.to_string(),
        },
        Reference {
            case: "this account's own layer ARN",
            held: "own-by-arn",
            named: |name| layer_arn(ACCOUNT, name),
        },
    ];

    for Reference { case, held, named } in rows {
        let published = publish_own_layer(&router, held).await;
        let named = named(held);

        // The publish, addressed by the row's reference form, adds a version to the same
        // layer rather than being refused.
        let again = json_call(
            &router,
            "POST",
            &format!("/2018-10-31/layers/{}/versions", encoded(&named)),
            &json!({ "Content": { "ZipFile": zip_file(&archive_bytes(48)) } }),
        )
        .await;
        assert_eq!(
            again.status,
            201,
            "case {case:?}: expected 201 from PublishLayerVersion, got {} {}",
            again.status,
            again.text()
        );
        assert_eq!(
            again.json().get("Version"),
            Some(&json!(published + 1)),
            "case {case:?}: expected the next version of the same layer, got {}",
            again.text()
        );

        // The read reports the version the publish assigned, with the layer's own ARNs.
        let read = json_call(
            &router,
            "GET",
            &format!(
                "/2018-10-31/layers/{}/versions/{published}",
                encoded(&named)
            ),
            &Value::Null,
        )
        .await;
        assert_eq!(
            read.status,
            200,
            "case {case:?}: expected 200 from GetLayerVersion, got {} {}",
            read.status,
            read.text()
        );
        assert_eq!(
            read.json().get("LayerVersionArn"),
            Some(&json!(layer_version_arn(ACCOUNT, held, published))),
            "case {case:?}: expected the stored version's ARN, got {}",
            read.text()
        );

        // The permission write and the policy read go through too, and the listing and the
        // delete answer their own modelled successes.
        let granted = json_call(
            &router,
            "POST",
            &format!(
                "/2018-10-31/layers/{}/versions/{published}/policy",
                encoded(&named)
            ),
            &json!({
                "StatementId": "shared",
                "Action": "lambda:GetLayerVersion",
                "Principal": OTHER_ACCOUNT,
            }),
        )
        .await;
        assert_eq!(
            granted.status,
            201,
            "case {case:?}: expected 201 from AddLayerVersionPermission, got {} {}",
            granted.status,
            granted.text()
        );
        let policy = json_call(
            &router,
            "GET",
            &format!(
                "/2018-10-31/layers/{}/versions/{published}/policy",
                encoded(&named)
            ),
            &Value::Null,
        )
        .await;
        assert_eq!(
            policy.status,
            200,
            "case {case:?}: expected 200 from GetLayerVersionPolicy, got {} {}",
            policy.status,
            policy.text()
        );
        let listed = json_call(
            &router,
            "GET",
            &format!("/2018-10-31/layers/{}/versions", encoded(&named)),
            &Value::Null,
        )
        .await;
        assert_eq!(
            listed
                .json()
                .get("LayerVersions")
                .and_then(Value::as_array)
                .map(Vec::len),
            Some(2),
            "case {case:?}: expected both published versions listed, got {}",
            listed.text()
        );
        let removed = json_call(
            &router,
            "DELETE",
            &format!(
                "/2018-10-31/layers/{}/versions/{published}/policy/shared",
                encoded(&named)
            ),
            &Value::Null,
        )
        .await;
        assert_eq!(
            removed.status,
            204,
            "case {case:?}: expected 204 from RemoveLayerVersionPermission, got {} {}",
            removed.status,
            removed.text()
        );
        let deleted = json_call(
            &router,
            "DELETE",
            &format!(
                "/2018-10-31/layers/{}/versions/{published}",
                encoded(&named)
            ),
            &Value::Null,
        )
        .await;
        assert_eq!(
            deleted.status,
            204,
            "case {case:?}: expected 204 from DeleteLayerVersion, got {} {}",
            deleted.status,
            deleted.text()
        );
    }
}

/// The sentence a `RevisionId` precondition that did not hold gets on a layer version's
/// permissions policy: the fixed text, naming the read that reports that policy's latest
/// revision.
const LAYER_POLICY_REVISION_REFUSAL: &str = "The Revision Id provided does not match the latest \
     Revision Id. Call the GetLayerPolicy API to retrieve the latest Revision Id";

/// RULE: a `RevisionId` the layer version's policy is not at refuses both permission
/// operations with that one sentence, whatever the supplied id and whatever revision the
/// policy is actually at — and the sentence carries nothing of the request.
#[tokio::test]
async fn a_layer_policy_revision_mismatch_names_the_layer_policy_read() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);

    let held = "revision-probe";
    let version = publish_own_layer(&router, held).await;
    let policy_uri = format!("/2018-10-31/layers/{held}/versions/{version}/policy");

    // A grant so the version carries a policy at a revision of its own; the rows below name
    // ids that are not it.
    let granted = json_call(
        &router,
        "POST",
        &policy_uri,
        &json!({
            "StatementId": "held-statement",
            "Action": "lambda:GetLayerVersion",
            "Principal": ACCOUNT,
        }),
    )
    .await;
    assert_eq!(
        granted.status,
        201,
        "expected 201 from AddLayerVersionPermission, got {} {}",
        granted.status,
        granted.text()
    );
    let revision = granted
        .json()
        .get("RevisionId")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| panic!("expected a RevisionId, got {}", granted.text()));

    // Ids the policy is not at: another UUID, one of a different shape, and an empty-shaped
    // token — the sentence is the same for all of them.
    let stale = [
        "00000000-0000-0000-0000-000000000000",
        "0d0d0d0d-1e1e-2f2f-3a3a-4b4b4b4b4b4b",
        "not-a-revision",
    ];

    for supplied in stale {
        let rows = [
            (
                "AddLayerVersionPermission",
                "POST",
                format!("{policy_uri}?RevisionId={supplied}"),
                json!({
                    "StatementId": "another-statement",
                    "Action": "lambda:GetLayerVersion",
                    "Principal": ACCOUNT,
                }),
            ),
            (
                "RemoveLayerVersionPermission",
                "DELETE",
                format!("{policy_uri}/held-statement?RevisionId={supplied}"),
                Value::Null,
            ),
        ];
        for (operation, method, uri, body) in rows {
            let case = format!("{operation} at the revision {supplied:?}");
            let answer = json_call(&router, method, &uri, &body).await;
            assert_eq!(
                answer.status,
                412,
                "case {case:?}: expected 412, got {} {}",
                answer.status,
                answer.text()
            );
            assert_eq!(
                answer.error_code(),
                PRECONDITION_FAILED,
                "case {case:?}: expected {PRECONDITION_FAILED}, got {:?} in {}",
                answer.error_code(),
                answer.text()
            );
            let reported = answer.error_message();
            assert_eq!(
                reported, LAYER_POLICY_REVISION_REFUSAL,
                "case {case:?}: expected {LAYER_POLICY_REVISION_REFUSAL:?}, got {reported:?}"
            );
            // Fixed text: neither the id the caller supplied nor the resource it addressed
            // appears in it.
            for quoted in [supplied, held, revision.as_str()] {
                assert!(
                    !reported.contains(quoted),
                    "case {case:?}: the sentence is fixed, so it may not name {quoted:?}, got \
                     {reported:?}"
                );
            }
        }
    }

    // Every refused precondition left the policy at the revision it was already at, and the
    // statement it held still there.
    let policy = json_call(&router, "GET", &policy_uri, &Value::Null).await;
    assert_eq!(
        policy.json().get("RevisionId"),
        Some(&json!(revision)),
        "expected the policy to stay at the revision it was at, got {}",
        policy.text()
    );

    // And the SAME precondition, supplied at the revision the policy IS at, is served: the
    // sentence above is about the mismatch, not about the member being present.
    let served = json_call(
        &router,
        "POST",
        &format!("{policy_uri}?RevisionId={revision}"),
        &json!({
            "StatementId": "second-statement",
            "Action": "lambda:GetLayerVersion",
            "Principal": ACCOUNT,
        }),
    )
    .await;
    assert_eq!(
        served.status,
        201,
        "expected the matching revision to be served, got {} {}",
        served.status,
        served.text()
    );
}
