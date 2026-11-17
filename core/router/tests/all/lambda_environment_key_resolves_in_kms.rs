// SPDX-License-Identifier: CC0-1.0

//! The customer managed key a function's environment is encrypted with, resolved through KMS.
//!
//! RULE: `KMSKeyArn` is "The ARN of the Key Management Service (KMS) customer managed key
//! that's used to encrypt […] your function's environment variables"
//! (`smithy_query(query="operation",operation="CreateFunction,UpdateFunctionConfiguration")`),
//! so an operation that configures a function's environment key resolves it through KMS first
//! and refuses the write with `InvalidParameterValueException` (HTTP 400) when KMS holds no
//! record of the key, leaving the function's state exactly as it was. The refusal states the
//! encryption Lambda could not perform and relays KMS's own answer: the exception class it
//! stated and the sentence it stated about the key. The key's STATE is not a configure-time
//! refusal — which is why a disabled key leaves the function configurable and refuses its
//! INVOCATIONS instead, and re-enabling the key restores them.
//!
//! Two services have to agree for this to hold — Lambda asks and KMS answers — so this is the
//! suite that can pin it: the Lambda crate's own tests see no KMS at all, and an emulator
//! assembled without one accepts the reference as given.
//!
//! The rows span what the rule generalises over: which key the reference names (keys KMS
//! really holds, one of them disabled, and references it holds nothing under, differing in
//! the key id, in its form and in the Region the ARN names), whether the request carries
//! environment variables at all, and which of the two operations configures the key.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_lambda_awsgirapiservice::state::ComAmazonawsLambdaAwsgirapiserviceState;
use smithy_server_runtime::MultiServiceRouter;

use crate::lambda_signing_name_routing::{ACCOUNT, REGION, ROLE, ZIP_FILE};

/// The `X-Amz-Target` prefix of KMS's awsJson1_1 wire.
const KMS_TARGET_PREFIX: &str = "TrentService";

/// Lambda's own sentence for the encryption it could not perform under the key a request
/// named, which every control-plane refusal of such a key opens with.
const ENCRYPT_NOT_FOUND_MESSAGE: &str = "Lambda was unable to encrypt your environment \
                                         variables because the KMS key was not found. Please \
                                         check your KMS key settings.";

/// One emulator carrying both services over ONE shared engine, exactly as the production
/// router builds them. The engine is handed back too, so a row can bring a function's
/// clock-owed activation forward instead of waiting for it.
fn emulator() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Nothing here signs: the `Authorization` header carries only the credential scope,
    // which is where a service reads the Region and the caller's access key id.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_kms_trentservice::EmulatedService::new(Arc::clone(&engine)),
    );
    (router, engine)
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name`.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDTESTLAMBDA/20260101/{REGION}/{signing_name}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One KMS call on its awsJson1_1 wire, answered with the HTTP status and the JSON body.
async fn kms(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("kms.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{KMS_TARGET_PREFIX}.{operation}"))
        .header("authorization", authorization("kms"))
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("the test request builds");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let raw = response.into_body();
    let parsed = if raw.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&raw)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&raw).to_string()))
    };
    (status, parsed)
}

/// One restJson1 call to the Lambda control plane, answered with the HTTP status, the wire
/// error code (`""` on a success) and the JSON body.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (u16, String, Value) {
    let payload = body
        .map(|value| Bytes::from(serde_json::to_vec(&value).expect("the test body serializes")))
        .unwrap_or_default();
    let request = http::Request::builder()
        .method(method)
        .uri(format!("http://lambda.{REGION}.amazonaws.com{uri}"))
        .header("host", format!("lambda.{REGION}.amazonaws.com"))
        .header("authorization", authorization("lambda"))
        .header("content-type", "application/json")
        .body(payload)
        .expect("the test request builds");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let headers = response.headers().clone();
    let bytes = response.into_body();
    let parsed: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    let code = parsed
        .get("__type")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            headers
                .get("x-amzn-errortype")
                .and_then(|value| value.to_str().ok())
                .map(str::to_string)
        })
        .unwrap_or_default();
    (status, code, parsed)
}

/// Create a symmetric encryption key through KMS's own `CreateKey` and answer its ARN.
async fn create_key(router: &MultiServiceRouter, description: &str) -> String {
    let (status, created) = kms(router, "CreateKey", &json!({ "Description": description })).await;
    assert_eq!(
        status, 200,
        "case {description:?}: CreateKey must succeed, got {status} {created}"
    );
    created["KeyMetadata"]["Arn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {description:?}: CreateKey reports an Arn, got {created}"))
        .to_string()
}

/// Turn a key KMS holds off through its own `DisableKey`.
async fn disable_key(router: &MultiServiceRouter, key_arn: &str) {
    let (status, answered) = kms(router, "DisableKey", &json!({ "KeyId": key_arn })).await;
    assert_eq!(
        status, 200,
        "case {key_arn:?}: DisableKey must succeed, got {status} {answered}"
    );
}

/// The `CreateFunction` body of a function named `name`, carrying `environment` and the key
/// reference `kms_key_arn` when either is given.
fn create_body(name: &str, kms_key_arn: Option<&str>, variables: Option<Value>) -> Value {
    let mut body = json!({
        "FunctionName": name,
        "Role": ROLE,
        "Runtime": "python3.12",
        "Handler": "index.handler",
        "Code": { "ZipFile": ZIP_FILE },
    });
    if let Some(key_arn) = kms_key_arn {
        body["KMSKeyArn"] = json!(key_arn);
    }
    if let Some(variables) = variables {
        body["Environment"] = json!({ "Variables": variables });
    }
    body
}

/// Move every stored function's activation and in-flight-update instants into the past, so
/// the next call materialises the `Pending` -> `Active` and update-finished transitions the
/// clock owes. This is what waiting would do, without waiting.
fn make_activation_due(engine: &Arc<engine::InMemoryEngine>) {
    let state = engine.state::<ComAmazonawsLambdaAwsgirapiserviceState>();
    let mut store = state.lock();
    let past = SystemTime::now() - Duration::from_secs(1);
    for function in store.functions.values_mut() {
        function.active_due = past;
        if function.last_update_due.is_some() {
            function.last_update_due = Some(past);
        }
    }
}

/// One reference KMS holds nothing under, and the sentence KMS states about it.
struct Absent {
    case: &'static str,
    key_arn: String,
    /// The sentence the refusal must relay, which is KMS's own answer about the reference.
    relayed: String,
}

/// The references KMS resolves nothing for, differing in the key id, in the form of the
/// identifier and in the Region the ARN names.
fn absent_references() -> Vec<Absent> {
    vec![
        Absent {
            case: "a well-formed key id this endpoint never issued",
            key_arn: format!("arn:aws:kms:{REGION}:{ACCOUNT}:key/00000000-0000-0000-0000-000000000000"),
            relayed: format!(
                "Key 'arn:aws:kms:{REGION}:{ACCOUNT}:key/00000000-0000-0000-0000-000000000000' \
                 does not exist"
            ),
        },
        Absent {
            case: "a second key id this endpoint never issued",
            key_arn: format!("arn:aws:kms:{REGION}:{ACCOUNT}:key/9f8e7d6c-5b4a-3928-1706-fedcba987654"),
            relayed: format!(
                "Key 'arn:aws:kms:{REGION}:{ACCOUNT}:key/9f8e7d6c-5b4a-3928-1706-fedcba987654' \
                 does not exist"
            ),
        },
        Absent {
            case: "a key ARN whose last segment is no key id at all",
            key_arn: format!("arn:aws:kms:{REGION}:{ACCOUNT}:key/not-a-key-id"),
            relayed: format!("Invalid keyId 'arn:aws:kms:{REGION}:{ACCOUNT}:key/not-a-key-id'"),
        },
    ]
}

#[tokio::test]
async fn create_function_refuses_an_environment_key_kms_does_not_hold() {
    let (router, _engine) = emulator();

    for (index, absent) in absent_references().iter().enumerate() {
        // The rows alternate on whether the request carries environment variables, which the
        // key encrypts: the reference is resolved either way, because the grant is held on the
        // key the configuration names.
        let variables = if index % 2 == 0 {
            Some(json!({ "SECRET": "s3cret" }))
        } else {
            None
        };
        let name = format!("env-key-create-{index}");
        let (status, code, answered) = lambda(
            &router,
            "POST",
            "/2015-03-31/functions",
            Some(create_body(&name, Some(&absent.key_arn), variables)),
        )
        .await;

        assert_eq!(
            (status, code.as_str()),
            (400, "InvalidParameterValueException"),
            "case {:?}: a key KMS does not hold must be refused, got {status} {code:?}: {answered}",
            absent.case
        );
        let message = answered["message"].as_str().unwrap_or_default();
        let expected = format!(
            "{ENCRYPT_NOT_FOUND_MESSAGE} KMS Exception: NotFoundException KMS Message: {}",
            absent.relayed
        );
        assert_eq!(
            message, expected,
            "case {:?}: expected Lambda's sentence and KMS's relayed answer, got {message:?}",
            absent.case
        );

        // Nothing was written: the function the refused create named is held under no record
        // at all.
        let (status, code, fetched) = lambda(
            &router,
            "GET",
            &format!("/2015-03-31/functions/{name}/configuration"),
            None,
        )
        .await;
        assert_eq!(
            (status, code.as_str()),
            (404, "ResourceNotFoundException"),
            "case {:?}: a refused create must store no function, got {status} {code:?}: {fetched}",
            absent.case
        );
    }
}

#[tokio::test]
async fn create_function_accepts_an_environment_key_kms_holds() {
    let (router, _engine) = emulator();

    let first = create_key(&router, "the first environment key").await;
    let second = create_key(&router, "the second environment key").await;
    let disabled = create_key(&router, "an environment key turned off").await;
    disable_key(&router, &disabled).await;

    // Rows: two keys KMS really holds (so the acceptance is about the key resolving and not
    // about the first create being special), one of them carrying variables to encrypt; a key
    // KMS holds that is DISABLED, whose state is settled at invoke time rather than here, so
    // the configuration stands; and the references that name no key at all — absent, and the
    // empty string the member's own pattern admits — which have nothing to resolve.
    let cases: &[(&str, Option<&str>, Option<Value>)] = &[
        (
            "a key KMS holds, with variables to encrypt",
            Some(first.as_str()),
            Some(json!({ "SECRET": "s3cret", "REGION_NAME": REGION })),
        ),
        ("a second key KMS holds", Some(second.as_str()), None),
        (
            "a key KMS holds that is disabled",
            Some(disabled.as_str()),
            Some(json!({ "SECRET": "s3cret" })),
        ),
        (
            "no key named, with variables",
            None,
            Some(json!({ "PLAIN": "value" })),
        ),
        ("no key and no variables", None, None),
        (
            "the empty key reference the member's pattern admits",
            Some(""),
            Some(json!({ "PLAIN": "value" })),
        ),
    ];

    for (index, (case, key_arn, variables)) in cases.iter().enumerate() {
        let name = format!("env-key-served-{index}");
        let (status, code, created) = lambda(
            &router,
            "POST",
            "/2015-03-31/functions",
            Some(create_body(&name, *key_arn, variables.clone())),
        )
        .await;
        assert_eq!(
            (status, code.as_str()),
            (201, ""),
            "case {case:?}: a key KMS holds must create the function, got {status} {code:?}: \
             {created}"
        );
        assert_eq!(
            created["KMSKeyArn"].as_str(),
            *key_arn,
            "case {case:?}: the configured key must be reported back, got {created}"
        );

        // Round trip: the function was really stored under the key the request named, which is
        // what the refusal above must prevent.
        let (status, code, fetched) = lambda(
            &router,
            "GET",
            &format!("/2015-03-31/functions/{name}/configuration"),
            None,
        )
        .await;
        assert_eq!(
            (status, code.as_str()),
            (200, ""),
            "case {case:?}: the created function must be readable back, got {status} {code:?}: \
             {fetched}"
        );
        assert_eq!(
            fetched["KMSKeyArn"].as_str(),
            *key_arn,
            "case {case:?}: the stored key must be the one configured, got {fetched}"
        );
    }
}

#[tokio::test]
async fn update_function_configuration_refuses_an_environment_key_kms_does_not_hold() {
    let (router, engine) = emulator();

    let held = create_key(&router, "the function's first environment key").await;
    let replacement = create_key(&router, "the key that replaces it").await;
    let (status, code, created) = lambda(
        &router,
        "POST",
        "/2015-03-31/functions",
        Some(create_body(
            "env-key-update",
            Some(&held),
            Some(json!({ "SECRET": "s3cret" })),
        )),
    )
    .await;
    assert_eq!(
        (status, code.as_str()),
        (201, ""),
        "the function under a key KMS holds must be created, got {status} {code:?}: {created}"
    );

    let configuration_uri = "/2015-03-31/functions/env-key-update/configuration";
    for absent in absent_references() {
        // An update is applied to a function that is not mid-change, so the activation and
        // update transitions the clock already owes are materialised first.
        make_activation_due(&engine);
        let (status, code, answered) = lambda(
            &router,
            "PUT",
            configuration_uri,
            Some(json!({ "KMSKeyArn": absent.key_arn })),
        )
        .await;
        assert_eq!(
            (status, code.as_str()),
            (400, "InvalidParameterValueException"),
            "case {:?}: a key KMS does not hold must be refused, got {status} {code:?}: {answered}",
            absent.case
        );
        let message = answered["message"].as_str().unwrap_or_default();
        let expected = format!(
            "{ENCRYPT_NOT_FOUND_MESSAGE} KMS Exception: NotFoundException KMS Message: {}",
            absent.relayed
        );
        assert_eq!(
            message, expected,
            "case {:?}: expected Lambda's sentence and KMS's relayed answer, got {message:?}",
            absent.case
        );

        // The function's state is unchanged: it still holds the key it was created under, and
        // still holds its variables.
        let (status, code, fetched) = lambda(&router, "GET", configuration_uri, None).await;
        assert_eq!(
            (status, code.as_str()),
            (200, ""),
            "case {:?}: the function must still be readable, got {status} {code:?}: {fetched}",
            absent.case
        );
        assert_eq!(
            fetched["KMSKeyArn"].as_str(),
            Some(held.as_str()),
            "case {:?}: a refused update must leave the configured key as it was, got {fetched}",
            absent.case
        );
        assert_eq!(
            fetched["Environment"]["Variables"]["SECRET"].as_str(),
            Some("s3cret"),
            "case {:?}: a refused update must leave the environment as it was, got {fetched}",
            absent.case
        );
    }

    // The same operation applies a key KMS DOES hold, which is what the refusals above must
    // not be mistaken for.
    make_activation_due(&engine);
    let (status, code, updated) = lambda(
        &router,
        "PUT",
        configuration_uri,
        Some(json!({ "KMSKeyArn": replacement })),
    )
    .await;
    assert_eq!(
        (status, code.as_str()),
        (200, ""),
        "case \"a key KMS holds\": the update must be applied, got {status} {code:?}: {updated}"
    );
    assert_eq!(
        updated["KMSKeyArn"].as_str(),
        Some(replacement.as_str()),
        "case \"a key KMS holds\": the replacement key must be reported back, got {updated}"
    );
    make_activation_due(&engine);
    let (_, _, fetched) = lambda(&router, "GET", configuration_uri, None).await;
    assert_eq!(
        fetched["KMSKeyArn"].as_str(),
        Some(replacement.as_str()),
        "case \"a key KMS holds\": the replacement key must be the one stored, got {fetched}"
    );
}
