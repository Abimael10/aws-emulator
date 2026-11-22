// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the KMS keys a function's deployment names, resolved in the key
//! service and asked what they can do.
//!
//! THE RULE: a key a write names is resolved in the key service BEFORE anything is stored, and
//! the write is refused when that service's answer says the key could not serve its purpose.
//!
//! * `Code$SourceKMSKeyArn` is "The ARN of the Key Management Service (KMS) customer managed
//!   key that's used to encrypt your function's .zip deployment package"
//!   (`smithy_query(query="operation",operation="CreateFunction,UpdateFunctionCode")`), so a
//!   key the key service holds no record of is a package Lambda could never unpack: the write
//!   is refused `InvalidParameterValueException` (HTTP 400) with nothing stored, stating
//!   Lambda's own sentence for the key it could not find and then relaying the key service's
//!   answer — the exception class it stated, the sentence it stated about the key, and the
//!   clause naming the relayed call, whose request id is that call's own and so differs per
//!   call.
//! * `KMSKeyArn` is the key "used to encrypt […] your function's environment variables", and
//!   storing them under it ENCRYPTS them, under the default symmetric algorithm. So the key
//!   has to be one the key service reports as supporting that algorithm
//!   (`smithy_query(query="doc",shape="KeyMetadata",member="EncryptionAlgorithms")`: "the
//!   encryption algorithms that the KMS key supports. You cannot use the KMS key with other
//!   encryption algorithms within KMS"): a key whose own algorithms do not name it — one for
//!   signing, for message authentication, for key agreement, or an asymmetric encryption key
//!   — is an encryption that fails, so the write is refused `ServiceException` (HTTP 500) with
//!   its fixed sentence and nothing is created. A key the key service holds no record of is
//!   the 400 above instead, and the key's STATE is no part of either: that is settled where
//!   the key is USED (`tests/all/environment_variable_key_state.rs`).
//!
//! The keys really are what a row says they are: each is minted with the key service's own
//! `CreateKey`, over the same engine, with the `KeySpec` and `KeyUsage` its row names.
//!
//! The rows differ along what the rule generalises over: what the key is (keys the key service
//! holds, of every usage `CreateKey` mints, and references it holds nothing under, differing
//! in the key id and in the form of the identifier), whether the request carries environment
//! variables at all and which variables they are, and which operation names the key.

use std::sync::Arc;

use serde_json::{json, Value};

use crate::durable_execution_completion::kms_call;
use crate::environment_variable_key_state::{created_key, router_with_kms};
use crate::function_lifecycle::{
    create_function, json_call, make_activation_due, make_update_due, package_bytes, zip_file,
    Answer, ACCOUNT, REGION, ROLE,
};

/// Lambda's own sentence for a KMS key it could not find, which a refusal of the key the
/// deployment package is encrypted with opens with.
const SOURCE_KEY_NOT_FOUND_SENTENCE: &str =
    "Lambda can't find the KMS key. Check your KMS key settings.";

/// The whole sentence a refusal of a key that cannot encrypt the environment variables
/// carries: fixed text with no variable part at all.
const ENCRYPTION_FAILED_MESSAGE: &str =
    "Lambda was unable to encrypt your environment variables. Please try again.";

/// Lambda and the key service on one engine, so a key a request names is a key this account
/// really holds (or really does not).
fn emulator() -> (Arc<engine::InMemoryEngine>, smithy_server_runtime::MultiServiceRouter) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let router = router_with_kms(&engine);
    (engine, router)
}

/// A key this account holds of the `KeySpec` and `KeyUsage` a row names, answered by the ARN
/// the key service minted for it.
async fn created_key_of(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    key_spec: &str,
    key_usage: &str,
) -> String {
    let created = kms_call(
        router,
        "CreateKey",
        &json!({ "KeySpec": key_spec, "KeyUsage": key_usage }),
    )
    .await;
    assert_eq!(
        created.status,
        200,
        "case {case:?}: CreateKey must succeed, got {} {}",
        created.status,
        String::from_utf8_lossy(&created.body)
    );
    created
        .json()
        .pointer("/KeyMetadata/Arn")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: CreateKey must report a key ARN, got {}",
                String::from_utf8_lossy(&created.body)
            )
        })
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
        "Code": { "ZipFile": zip_file(&package_bytes(64)) },
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

/// `GetFunction` on a function by name.
async fn get_function(
    router: &smithy_server_runtime::MultiServiceRouter,
    name: &str,
) -> Answer {
    json_call(
        router,
        "GET",
        &format!("/2015-03-31/functions/{name}"),
        &Value::Null,
    )
    .await
}

/// `GetFunctionConfiguration` on a function by name.
async fn get_configuration(
    router: &smithy_server_runtime::MultiServiceRouter,
    name: &str,
) -> Answer {
    json_call(
        router,
        "GET",
        &format!("/2015-03-31/functions/{name}/configuration"),
        &Value::Null,
    )
    .await
}

/// One reference the key service holds nothing under, and the sentence it states about it.
struct Absent {
    case: &'static str,
    key_arn: String,
    /// The sentence the refusal must relay, which is the key service's own answer about the
    /// reference.
    relayed: String,
}

/// The references the key service resolves nothing for, differing in the key id and in the
/// form of the identifier. `SourceKMSKeyArn`'s own pattern admits an ARN or nothing at all, so
/// every row here is an ARN.
fn absent_references() -> Vec<Absent> {
    vec![
        Absent {
            case: "a well-formed key id this endpoint never issued",
            key_arn: format!(
                "arn:aws:kms:{REGION}:{ACCOUNT}:key/11111111-2222-4333-8444-555555555555"
            ),
            relayed: format!(
                "Key 'arn:aws:kms:{REGION}:{ACCOUNT}:key/11111111-2222-4333-8444-555555555555' \
                 does not exist"
            ),
        },
        Absent {
            case: "a second key id this endpoint never issued",
            key_arn: format!(
                "arn:aws:kms:{REGION}:{ACCOUNT}:key/00000000-0000-0000-0000-000000000000"
            ),
            relayed: format!(
                "Key 'arn:aws:kms:{REGION}:{ACCOUNT}:key/00000000-0000-0000-0000-000000000000' \
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

/// Assert that `answered` is the modelled 400 stating Lambda's sentence for a key it could not
/// find, relaying the key service's own answer and then the clause naming the relayed call.
///
/// The request id in that clause belongs to the relayed CALL, so it differs per call and is
/// asserted as present rather than as a value.
fn assert_source_key_not_found(case: &str, answered: &Answer, relayed: &str) {
    assert_eq!(
        (answered.status, answered.error_code().as_str()),
        (400, "InvalidParameterValueException"),
        "case {case:?}: a package key the key service does not hold must be refused, got {} {}",
        answered.status,
        String::from_utf8_lossy(&answered.body)
    );
    let message = answered.string("message").unwrap_or_default();
    let head = format!(
        "{SOURCE_KEY_NOT_FOUND_SENTENCE} KMS exception: NotFoundException KMS message: \
         {relayed} (Service: AWSKMS; Status Code: 400; Error Code: NotFoundException; \
         Request ID: "
    );
    assert!(
        message.starts_with(&head),
        "case {case:?}: expected the message to start {head:?}, got {message:?}"
    );
    assert!(
        message.ends_with("; Proxy: null)"),
        "case {case:?}: expected the message to end \"; Proxy: null)\", got {message:?}"
    );
}

/// Assert that `answered` is the modelled 500 stating the one sentence a failed encryption of
/// the environment variables carries.
fn assert_encryption_failed(case: &str, answered: &Answer) {
    assert_eq!(
        (answered.status, answered.error_code().as_str()),
        (500, "ServiceException"),
        "case {case:?}: a key that cannot encrypt must be refused, got {} {}",
        answered.status,
        String::from_utf8_lossy(&answered.body)
    );
    assert_eq!(
        answered.string("Message").as_deref(),
        Some(ENCRYPTION_FAILED_MESSAGE),
        "case {case:?}: expected the encryption failure sentence, got {}",
        String::from_utf8_lossy(&answered.body)
    );
}

#[tokio::test]
async fn create_function_refuses_a_package_key_the_key_service_does_not_hold() {
    let (_engine, router) = emulator();
    let held = created_key(&router).await;

    for (index, absent) in absent_references().iter().enumerate() {
        // The rows alternate on whether the request also carries environment variables under a
        // key the key service HOLDS: the package key is resolved either way, because it is the
        // package's own encryption and not the environment's.
        let (environment_key, variables) = if index % 2 == 0 {
            (Some(held.as_str()), Some(json!({ "SECRET": "s3cret" })))
        } else {
            (None, None)
        };
        let name = format!("source-key-create-{index}");
        let answered = create_function(
            &router,
            &create_body(&name, environment_key, variables, Some(&absent.key_arn)),
        )
        .await;
        assert_source_key_not_found(absent.case, &answered, &absent.relayed);

        // Nothing was written: the function the refused create named is held under no record
        // at all.
        let fetched = get_configuration(&router, &name).await;
        assert_eq!(
            (fetched.status, fetched.error_code().as_str()),
            (404, "ResourceNotFoundException"),
            "case {:?}: a refused create must store no function, got {} {}",
            absent.case,
            fetched.status,
            String::from_utf8_lossy(&fetched.body)
        );
    }
}

#[tokio::test]
async fn create_function_accepts_a_package_key_the_key_service_holds() {
    let (_engine, router) = emulator();
    let first = created_key(&router).await;
    let second = created_key(&router).await;

    // Rows: two keys the key service really holds (so the acceptance is about the key
    // resolving and not about the first create being special), and the references that name no
    // key at all — absent, and the empty string the member's own pattern admits — which have
    // nothing to resolve.
    let cases: &[(&str, Option<&str>)] = &[
        ("a key the key service holds", Some(first.as_str())),
        ("a second key it holds", Some(second.as_str())),
        ("no package key named", None),
        (
            "the empty package key reference the member's pattern admits",
            Some(""),
        ),
    ];

    for (index, (case, source_key)) in cases.iter().enumerate() {
        let name = format!("source-key-served-{index}");
        let created = create_function(&router, &create_body(&name, None, None, *source_key)).await;
        assert_eq!(
            created.status,
            201,
            "case {case:?}: a package key the key service holds must create the function, got \
             {} {}",
            created.status,
            String::from_utf8_lossy(&created.body)
        );

        // Round trip: the package really is stored under the key the request named, which is
        // what `FunctionCodeLocation$SourceKMSKeyArn` reports — the reference exactly as
        // stated, the empty one that names no key included.
        let fetched = get_function(&router, &name).await;
        assert_eq!(
            fetched.status,
            200,
            "case {case:?}: the created function must be readable back, got {} {}",
            fetched.status,
            String::from_utf8_lossy(&fetched.body)
        );
        assert_eq!(
            fetched
                .json()
                .pointer("/Code/SourceKMSKeyArn")
                .and_then(Value::as_str),
            *source_key,
            "case {case:?}: the stored package key must be the one named, got {}",
            String::from_utf8_lossy(&fetched.body)
        );
    }
}

#[tokio::test]
async fn update_function_code_refuses_a_package_key_the_key_service_does_not_hold() {
    let (engine, router) = emulator();
    let held = created_key(&router).await;

    let created = create_function(
        &router,
        &create_body("source-key-update", None, None, None),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "the function the update changes must be created, got {} {}",
        created.status,
        String::from_utf8_lossy(&created.body)
    );

    let code_uri = "/2015-03-31/functions/source-key-update/code";
    for absent in absent_references() {
        // Code is updated on a function that is not mid-change, so the activation and update
        // transitions the clock already owes are materialised first.
        make_activation_due(&engine);
        make_update_due(&engine);
        let answered = json_call(
            &router,
            "PUT",
            code_uri,
            &json!({
                "ZipFile": zip_file(&package_bytes(96)),
                "SourceKMSKeyArn": absent.key_arn,
            }),
        )
        .await;
        assert_source_key_not_found(absent.case, &answered, &absent.relayed);

        // Nothing was applied: the function still holds a package named by no key.
        let fetched = get_function(&router, "source-key-update").await;
        assert_eq!(
            fetched.status,
            200,
            "case {:?}: the function must still be readable, got {} {}",
            absent.case,
            fetched.status,
            String::from_utf8_lossy(&fetched.body)
        );
        assert_eq!(
            fetched
                .json()
                .pointer("/Code/SourceKMSKeyArn")
                .and_then(Value::as_str),
            None,
            "case {:?}: a refused update must leave the package key as it was, got {}",
            absent.case,
            String::from_utf8_lossy(&fetched.body)
        );
    }

    // The same operation applies a key the key service DOES hold, which is what the refusals
    // above must not be mistaken for.
    make_activation_due(&engine);
    make_update_due(&engine);
    let updated = json_call(
        &router,
        "PUT",
        code_uri,
        &json!({ "ZipFile": zip_file(&package_bytes(96)), "SourceKMSKeyArn": held }),
    )
    .await;
    assert_eq!(
        updated.status,
        200,
        "case \"a key the key service holds\": the update must be applied, got {} {}",
        updated.status,
        String::from_utf8_lossy(&updated.body)
    );
    make_activation_due(&engine);
    make_update_due(&engine);
    let fetched = get_function(&router, "source-key-update").await;
    assert_eq!(
        fetched
            .json()
            .pointer("/Code/SourceKMSKeyArn")
            .and_then(Value::as_str),
        Some(held.as_str()),
        "case \"a key the key service holds\": the applied package key must be the one stored, \
         got {}",
        String::from_utf8_lossy(&fetched.body)
    );
}

/// One key the key service holds whose own encryption algorithms do not name the default
/// symmetric one, so the environment variables could never be sealed under it: its `KeySpec`
/// and `KeyUsage` as `CreateKey` takes them
/// (`smithy_query(query="operation",operation="CreateKey")` of the key service's model lists
/// every value of each).
struct Unencryptable {
    case: &'static str,
    key_spec: &'static str,
    key_usage: &'static str,
}

/// A key of each usage the key service mints that is not a symmetric encryption key —
/// signing (the recorded case, and an elliptic-curve spec beside it), message authentication,
/// key agreement — and an ASYMMETRIC encryption key, whose algorithms are its spec's rather
/// than the symmetric default.
fn unencryptable_keys() -> Vec<Unencryptable> {
    vec![
        Unencryptable {
            case: "an RSA signing key",
            key_spec: "RSA_2048",
            key_usage: "SIGN_VERIFY",
        },
        Unencryptable {
            case: "an elliptic-curve signing key",
            key_spec: "ECC_NIST_P256",
            key_usage: "SIGN_VERIFY",
        },
        Unencryptable {
            case: "an HMAC key",
            key_spec: "HMAC_256",
            key_usage: "GENERATE_VERIFY_MAC",
        },
        Unencryptable {
            case: "a key-agreement key",
            key_spec: "ECC_NIST_P256",
            key_usage: "KEY_AGREEMENT",
        },
        Unencryptable {
            case: "an asymmetric encryption key",
            key_spec: "RSA_2048",
            key_usage: "ENCRYPT_DECRYPT",
        },
    ]
}

#[tokio::test]
async fn create_function_refuses_an_environment_key_that_cannot_encrypt() {
    let (_engine, router) = emulator();

    for (index, unusable) in unencryptable_keys().iter().enumerate() {
        let key_arn =
            created_key_of(&router, unusable.case, unusable.key_spec, unusable.key_usage).await;
        // The rows vary the variables the key would have to encrypt — one, several, and one
        // carrying characters a JSON string escapes — because what is refused is the key and
        // not any particular value.
        let variables = match index % 3 {
            0 => json!({ "PROBE": "1" }),
            1 => json!({ "SECRET": "s3cret", "REGION_NAME": REGION, "COUNT": "12" }),
            _ => json!({ "QUOTED": "a \"quoted\" value\nwith a newline" }),
        };
        let name = format!("env-key-unusable-{index}");
        let answered = create_function(
            &router,
            &create_body(&name, Some(&key_arn), Some(variables), None),
        )
        .await;
        assert_encryption_failed(unusable.case, &answered);

        // Nothing was created: the function the refused create named is held under no record
        // at all.
        let fetched = get_function(&router, &name).await;
        assert_eq!(
            (fetched.status, fetched.error_code().as_str()),
            (404, "ResourceNotFoundException"),
            "case {:?}: a refused create must store no function, got {} {}",
            unusable.case,
            fetched.status,
            String::from_utf8_lossy(&fetched.body)
        );
    }
}

#[tokio::test]
async fn create_function_accepts_an_environment_key_that_encrypts() {
    let (_engine, router) = emulator();
    let first = created_key(&router).await;
    let second = created_key(&router).await;
    let signing = created_key_of(
        &router,
        "a signing key on a function with nothing to encrypt",
        "RSA_2048",
        "SIGN_VERIFY",
    )
    .await;

    // Rows: two symmetric encryption keys the key service holds, each with variables for them
    // to encrypt; one of them with no variables at all; and a key that CANNOT encrypt on a
    // function that gives it nothing to encrypt, which is the boundary of the rule — what is
    // encrypted is the variables, so a function with none has no encryption to perform.
    let cases: &[(&str, Option<&str>, Option<Value>)] = &[
        (
            "a symmetric encryption key with variables",
            Some(first.as_str()),
            Some(json!({ "SECRET": "s3cret", "REGION_NAME": REGION })),
        ),
        (
            "a second symmetric encryption key with variables",
            Some(second.as_str()),
            Some(json!({ "PROBE": "1" })),
        ),
        (
            "a symmetric encryption key with no variables",
            Some(first.as_str()),
            None,
        ),
        (
            "a signing key on a function with no variables",
            Some(signing.as_str()),
            None,
        ),
    ];

    for (index, (case, key_arn, variables)) in cases.iter().enumerate() {
        let name = format!("env-key-encrypts-{index}");
        let created = create_function(
            &router,
            &create_body(&name, *key_arn, variables.clone(), None),
        )
        .await;
        assert_eq!(
            created.status,
            201,
            "case {case:?}: the function must be created, got {} {}",
            created.status,
            String::from_utf8_lossy(&created.body)
        );

        // Round trip: the function really holds the key and the variables the request named.
        let fetched = get_configuration(&router, &name).await;
        assert_eq!(
            fetched.status,
            200,
            "case {case:?}: the created function must be readable back, got {} {}",
            fetched.status,
            String::from_utf8_lossy(&fetched.body)
        );
        let read = fetched.json();
        assert_eq!(
            read.get("KMSKeyArn").and_then(Value::as_str),
            *key_arn,
            "case {case:?}: the stored key must be the one configured, got {read}"
        );
        assert_eq!(
            read.pointer("/Environment/Variables")
                .cloned()
                .unwrap_or(Value::Null),
            variables.clone().unwrap_or(Value::Null),
            "case {case:?}: the stored variables must be the ones configured, got {read}"
        );
    }
}

#[tokio::test]
async fn update_function_configuration_refuses_an_environment_key_that_cannot_encrypt() {
    let (engine, router) = emulator();
    let held = created_key(&router).await;
    let replacement = created_key(&router).await;

    let created = create_function(
        &router,
        &create_body(
            "env-key-update-usable",
            Some(&held),
            Some(json!({ "SECRET": "s3cret" })),
            None,
        ),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "the function under a key that encrypts must be created, got {} {}",
        created.status,
        String::from_utf8_lossy(&created.body)
    );

    let configuration_uri = "/2015-03-31/functions/env-key-update-usable/configuration";
    for (index, unusable) in unencryptable_keys().iter().enumerate() {
        let key_arn =
            created_key_of(&router, unusable.case, unusable.key_spec, unusable.key_usage).await;
        // The rows alternate on whether the update also states the variables: the key has to
        // encrypt the ones the function will HOLD, which are this request's when it states
        // them and the stored ones when it does not.
        let mut body = json!({ "KMSKeyArn": key_arn });
        if index % 2 == 0 {
            body["Environment"] = json!({ "Variables": { "PROBE": "1" } });
        }
        make_activation_due(&engine);
        make_update_due(&engine);
        let answered = json_call(&router, "PUT", configuration_uri, &body).await;
        assert_encryption_failed(unusable.case, &answered);

        // The function's state is unchanged: it still holds the key it was created under, and
        // still holds its variables.
        let fetched = get_configuration(&router, "env-key-update-usable").await;
        assert_eq!(
            fetched.status,
            200,
            "case {:?}: the function must still be readable, got {} {}",
            unusable.case,
            fetched.status,
            String::from_utf8_lossy(&fetched.body)
        );
        let read = fetched.json();
        assert_eq!(
            read.get("KMSKeyArn").and_then(Value::as_str),
            Some(held.as_str()),
            "case {:?}: a refused update must leave the configured key as it was, got {read}",
            unusable.case
        );
        assert_eq!(
            read.pointer("/Environment/Variables/SECRET")
                .and_then(Value::as_str),
            Some("s3cret"),
            "case {:?}: a refused update must leave the environment as it was, got {read}",
            unusable.case
        );
    }

    // The same operation applies a key that DOES encrypt, which is what the refusals above
    // must not be mistaken for.
    make_activation_due(&engine);
    make_update_due(&engine);
    let updated = json_call(
        &router,
        "PUT",
        configuration_uri,
        &json!({ "KMSKeyArn": replacement }),
    )
    .await;
    assert_eq!(
        updated.status,
        200,
        "case \"a key that encrypts\": the update must be applied, got {} {}",
        updated.status,
        String::from_utf8_lossy(&updated.body)
    );
    make_activation_due(&engine);
    make_update_due(&engine);
    let fetched = get_configuration(&router, "env-key-update-usable").await;
    assert_eq!(
        fetched.json().get("KMSKeyArn").and_then(Value::as_str),
        Some(replacement.as_str()),
        "case \"a key that encrypts\": the replacement key must be the one stored, got {}",
        String::from_utf8_lossy(&fetched.body)
    );
}
