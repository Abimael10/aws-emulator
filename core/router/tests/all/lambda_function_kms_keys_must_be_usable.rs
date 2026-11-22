// SPDX-License-Identifier: CC0-1.0

//! The KMS keys a Lambda function's deployment names, in the ASSEMBLED router.
//!
//! RULE: a key a write names is resolved in KMS before anything is stored, and the write is
//! refused when KMS's answer says the key could not serve its purpose — a key KMS holds no
//! record of for `Code$SourceKMSKeyArn` (`InvalidParameterValueException`, HTTP 400, relaying
//! KMS's own answer), and a key whose own encryption algorithms do not name the default
//! symmetric one for `KMSKeyArn` with environment variables to encrypt (`ServiceException`,
//! HTTP 500, with its fixed sentence). Nothing is created either way.
//!
//! The TABLE spanning that rule — every absent reference form, every key usage `CreateKey`
//! mints, the variables each key would have to encrypt, and the same rule on
//! `UpdateFunctionCode` and `UpdateFunctionConfiguration` — lives in the owning crate's own
//! suite (`services/service-com-amazonaws-lambda-awsgirapiservice/tests/
//! all/function_kms_key_usability.rs`). What only THIS suite reaches is the assembly: Lambda
//! and KMS mounted side by side over one engine the way `core/router/src/lib.rs` mounts every
//! service, so the hop Lambda makes to ask about a key is the one the running emulator makes.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::lambda_signing_name_routing::{ACCOUNT, REGION, ROLE, ZIP_FILE};

/// The `X-Amz-Target` prefix of KMS's awsJson1_1 wire.
const KMS_TARGET_PREFIX: &str = "TrentService";

/// Lambda's own sentence for a KMS key it could not find, which a refusal of the key the
/// deployment package is encrypted with opens with.
const SOURCE_KEY_NOT_FOUND_SENTENCE: &str =
    "Lambda can't find the KMS key. Check your KMS key settings.";

/// The whole sentence a refusal of a key that cannot encrypt the environment variables
/// carries: fixed text with no variable part.
const ENCRYPTION_FAILED_MESSAGE: &str =
    "Lambda was unable to encrypt your environment variables. Please try again.";

/// One emulator carrying both services over ONE shared engine, exactly as the production
/// router builds them.
fn emulator() -> MultiServiceRouter {
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
    router
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

/// Create a key through KMS's own `CreateKey` and answer the ARN it minted; `key_spec` and
/// `key_usage` are stated when the row names them, so a caller that names neither gets the
/// default symmetric encryption key.
async fn create_key(
    router: &MultiServiceRouter,
    case: &str,
    key_spec: Option<&str>,
    key_usage: Option<&str>,
) -> String {
    let mut body = json!({ "Description": case });
    if let Some(key_spec) = key_spec {
        body["KeySpec"] = json!(key_spec);
    }
    if let Some(key_usage) = key_usage {
        body["KeyUsage"] = json!(key_usage);
    }
    let (status, created) = kms(router, "CreateKey", &body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateKey must succeed, got {status} {created}"
    );
    created["KeyMetadata"]["Arn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: CreateKey reports an Arn, got {created}"))
        .to_string()
}

/// The `CreateFunction` body of a function named `name`, carrying the environment key, the
/// variables and the package key each row states.
fn create_body(
    name: &str,
    kms_key_arn: Option<&str>,
    variables: Option<Value>,
    source_kms_key_arn: Option<&str>,
) -> Value {
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
    if let Some(key_arn) = source_kms_key_arn {
        body["Code"]["SourceKMSKeyArn"] = json!(key_arn);
    }
    body
}

/// Assert that the function `name` is held under no record at all.
async fn assert_not_created(router: &MultiServiceRouter, case: &str, name: &str) {
    let (status, code, fetched) = lambda(
        router,
        "GET",
        &format!("/2015-03-31/functions/{name}/configuration"),
        None,
    )
    .await;
    assert_eq!(
        (status, code.as_str()),
        (404, "ResourceNotFoundException"),
        "case {case:?}: a refused create must store no function, got {status} {code:?}: {fetched}"
    );
}

#[tokio::test]
async fn create_function_resolves_its_package_key_in_the_mounted_kms() {
    let router = emulator();
    let held = create_key(&router, "the key the package is encrypted with", None, None).await;
    let absent = format!("arn:aws:kms:{REGION}:{ACCOUNT}:key/11111111-2222-4333-8444-555555555555");

    let (status, code, answered) = lambda(
        &router,
        "POST",
        "/2015-03-31/functions",
        Some(create_body("xsvc-source-key-absent", None, None, Some(&absent))),
    )
    .await;
    assert_eq!(
        (status, code.as_str()),
        (400, "InvalidParameterValueException"),
        "case \"a package key KMS does not hold\": expected the modelled 400, got {status} \
         {code:?}: {answered}"
    );
    let message = answered["message"].as_str().unwrap_or_default();
    // The request id in the relayed clause belongs to that call, so it differs per call and is
    // asserted as present rather than as a value.
    let head = format!(
        "{SOURCE_KEY_NOT_FOUND_SENTENCE} KMS exception: NotFoundException KMS message: Key \
         '{absent}' does not exist (Service: AWSKMS; Status Code: 400; Error Code: \
         NotFoundException; Request ID: "
    );
    assert!(
        message.starts_with(&head) && message.ends_with("; Proxy: null)"),
        "case \"a package key KMS does not hold\": expected {head:?} and the proxy clause, got \
         {message:?}"
    );
    assert_not_created(&router, "a package key KMS does not hold", "xsvc-source-key-absent").await;

    // The same create with a key the MOUNTED KMS really holds is served, which is what the
    // refusal above must not be mistaken for.
    let (status, code, created) = lambda(
        &router,
        "POST",
        "/2015-03-31/functions",
        Some(create_body("xsvc-source-key-held", None, None, Some(&held))),
    )
    .await;
    assert_eq!(
        (status, code.as_str()),
        (201, ""),
        "case \"a package key KMS holds\": the function must be created, got {status} {code:?}: \
         {created}"
    );
    let (status, _, fetched) =
        lambda(&router, "GET", "/2015-03-31/functions/xsvc-source-key-held", None).await;
    assert_eq!(
        (status, fetched["Code"]["SourceKMSKeyArn"].as_str()),
        (200, Some(held.as_str())),
        "case \"a package key KMS holds\": the stored package key must be the one named, got \
         {fetched}"
    );
}

#[tokio::test]
async fn create_function_asks_the_mounted_kms_what_the_environment_key_can_do() {
    let router = emulator();
    let signing = create_key(
        &router,
        "a signing key offered for the environment",
        Some("RSA_2048"),
        Some("SIGN_VERIFY"),
    )
    .await;
    let symmetric = create_key(&router, "a symmetric encryption key", None, None).await;

    let (status, code, answered) = lambda(
        &router,
        "POST",
        "/2015-03-31/functions",
        Some(create_body(
            "xsvc-env-key-signing",
            Some(&signing),
            Some(json!({ "PROBE": "1" })),
            None,
        )),
    )
    .await;
    assert_eq!(
        (status, code.as_str()),
        (500, "ServiceException"),
        "case \"a key that cannot encrypt\": expected the modelled 500, got {status} {code:?}: \
         {answered}"
    );
    assert_eq!(
        answered["Message"].as_str(),
        Some(ENCRYPTION_FAILED_MESSAGE),
        "case \"a key that cannot encrypt\": expected the encryption failure sentence, got \
         {answered}"
    );
    assert_not_created(&router, "a key that cannot encrypt", "xsvc-env-key-signing").await;

    // The same create under a key the mounted KMS reports as a symmetric encryption key is
    // served, and the function really holds it.
    let (status, code, created) = lambda(
        &router,
        "POST",
        "/2015-03-31/functions",
        Some(create_body(
            "xsvc-env-key-symmetric",
            Some(&symmetric),
            Some(json!({ "PROBE": "1" })),
            None,
        )),
    )
    .await;
    assert_eq!(
        (status, code.as_str()),
        (201, ""),
        "case \"a key that encrypts\": the function must be created, got {status} {code:?}: \
         {created}"
    );
    let (status, _, fetched) = lambda(
        &router,
        "GET",
        "/2015-03-31/functions/xsvc-env-key-symmetric/configuration",
        None,
    )
    .await;
    assert_eq!(
        (status, fetched["KMSKeyArn"].as_str()),
        (200, Some(symmetric.as_str())),
        "case \"a key that encrypts\": the stored key must be the one configured, got {fetched}"
    );
}
