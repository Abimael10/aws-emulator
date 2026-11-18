// SPDX-License-Identifier: CC0-1.0

//! A KMS key confined by `kms:ViaService` to one service's path to it serves the Secrets
//! Manager envelope and refuses the caller's own direct cryptographic calls.
//!
//! The RULE pinned here: **a KMS request another service issues on the caller's behalf while
//! serving a call of its own carries `kms:ViaService` = that service's regional endpoint name
//! (`<endpoint-prefix>.<region>.amazonaws.com`), while the same caller's own request to KMS
//! carries no value for that key at all** — so a key-policy clause over `kms:ViaService`
//! separates the two routes: the service's path is admitted or refused by what the clause
//! says about that endpoint, and the direct call is decided with the key absent.
//! "`kms:ViaService` … limits use of a KMS key to requests from particular Amazon Web
//! Services services" (KMS's own condition keys, as
//! `services/service-com-amazonaws-kms-trentservice/src/state/authorization.rs` records it).
//!
//! TWO crates have to agree for that to hold, which is why the suite lives here: Secrets
//! Manager is the service that wraps and unwraps a secret value under the secret's customer
//! managed key, and KMS is the service that holds the key policy and decides every use of the
//! key. The client step this suite drives is the one a key policy like this is written for —
//! confine the key to the secret store, keep reading and writing secrets, and lose the
//! ability to use the key directly.
//!
//! The rows differ along what the rule generalises over: WHICH operator the clause is written
//! under (a negated comparison, an existence test, a positive comparison), whether the clause
//! is on a `Deny` beside an unconditional grant or is the key policy's only grant, and WHICH
//! service endpoint it names (this one, and another service's) — with an unconditioned policy
//! as the control. Each row is checked on all three routes: the write through the envelope
//! (`kms:GenerateDataKey`), the read back through it (`kms:Decrypt`), and the caller's own
//! direct `kms:GenerateDataKey`.
//!
//! The second test pins the other half of the rule: the session says HOW the request arrived
//! and nothing about WHO it speaks for, so a clause that admits this service's path still
//! admits it only for the principal the key policy names — the caller whose own call the
//! envelope is serving, and not the service relaying it.
//!
//! No row writes a key id, a secret ARN or a version id: every identifier asserted on is one
//! the services reported, because any single run's generated id is no part of the rule.

use std::collections::BTreeMap;
use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_iam_awsidentitymanagementv20100508::state as iam;
use smithy_server_runtime::MultiServiceRouter;

// The awsJson error-code reader of this test crate, written ONCE rather than per suite.
use crate::logs_delivery_source_cloudfront_resource::error_code;

/// The Region every request below is signed for, and the Region the endpoint name a
/// `kms:ViaService` clause names is built in.
const REGION: &str = "us-east-1";

/// The access key id the ACCOUNT's own credential presents: one neither the token service nor
/// IAM holds a record for, which speaks for the account itself.
const ACCOUNT_KEY_ID: &str = "AKIDTHEACCOUNTITSELF";

/// The refusal a caller whose authority does not reach a cryptographic operation on the key
/// is answered with, and its status.
const ACCESS_DENIED: &str = "AccessDeniedException";
const ACCESS_DENIED_STATUS: u16 = 400;

/// The whole `Message` Secrets Manager answers when the KMS call it makes on the caller's
/// behalf is refused for want of authority on the key: this service's own sentence for that
/// class, which names no principal, action or key.
const KMS_ACCESS_DENIED_MESSAGE: &str = "Access to KMS is not allowed";

/// The value the secret is created with, and the one a later write replaces it by — so a
/// served read is asserted on the material rather than on a status.
const BASELINE_VALUE: &str = "the-value-the-secret-was-created-with";
const UPDATED_VALUE: &str = "the-value-the-later-write-stored";

/// The name a key policy holds its one document under (`PolicyName`, the KMS model).
const POLICY_NAME: &str = "default";

/// The `Sid` of the statement a row attaches, which the read-back asserts persisted.
const ROW_SID: &str = "viaServiceRow";

/// The cryptographic actions a key policy confining a key to one service's path to it is
/// written over — two of them action PATTERNS rather than action names, which is how such a
/// statement is written in practice.
fn cryptographic_actions() -> Value {
    json!([
        "kms:Encrypt",
        "kms:Decrypt",
        "kms:GenerateDataKey",
        "kms:GenerateDataKey*",
        "kms:ReEncrypt*",
    ])
}

/// The regional endpoint name of an AWS service, which is the value `kms:ViaService` carries
/// for a request that service makes on a caller's behalf.
fn endpoint(vendor: &str) -> String {
    format!("{vendor}.{REGION}.amazonaws.com")
}

/// Secrets Manager, KMS, IAM and the token service over ONE engine, the shape
/// `core/router/src/lib.rs` mounts them in: the envelope calls Secrets Manager makes reach
/// KMS through the emulator's own in-process transport, and the identity behind a request is
/// resolved out of the token and IAM stores.
fn emulator() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_secretsmanager_secretsmanager::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_kms_trentservice::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    (router, engine)
}

/// The SigV4 `Authorization` header of a request signed with `access_key_id` for
/// `signing_name`, as every AWS SDK sends it. The signature itself is never checked here; what
/// the decision reads off it is the credential, which is what tells this suite's two callers
/// apart.
fn signed_by(access_key_id: &str, signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/{REGION}/{signing_name}/\
         aws4_request, SignedHeaders=host, Signature=not-verified"
    )
}

/// The status and parsed body of one dispatched request (`Value::Null` for an empty or
/// unparseable body).
async fn answered(router: &MultiServiceRouter, request: http::Request<Bytes>) -> (u16, Value) {
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One awsJson1_1 call of `signing_name`, addressed by `target_prefix` and signed with
/// `access_key_id`, with whatever status it answered — a refusal is half of what this suite
/// asserts, so the status is the caller's.
async fn call(
    router: &MultiServiceRouter,
    access_key_id: &str,
    signing_name: &str,
    target_prefix: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", endpoint(signing_name))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .header("authorization", signed_by(access_key_id, signing_name))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed test request");
    answered(router, request).await
}

/// One KMS call the CALLER makes itself, which carries no value for `kms:ViaService`.
async fn kms(
    router: &MultiServiceRouter,
    access_key_id: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    call(
        router,
        access_key_id,
        "kms",
        "TrentService",
        operation,
        body,
    )
    .await
}

/// One Secrets Manager call, whose envelope reaches KMS on this caller's behalf.
async fn secretsmanager(
    router: &MultiServiceRouter,
    access_key_id: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    call(
        router,
        access_key_id,
        "secretsmanager",
        "secretsmanager",
        operation,
        body,
    )
    .await
}

/// The `message` of an error body, under either casing the envelope may carry it in.
fn error_message(body: &Value) -> String {
    body.get("message")
        .or_else(|| body.get("Message"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// A body member as a string, or `"<unset>"` when the member is absent — so an assertion
/// reports a missing member rather than panicking on it.
fn member(body: &Value, name: &str) -> String {
    body.get(name)
        .and_then(Value::as_str)
        .unwrap_or("<unset>")
        .to_string()
}

/// A `ClientRequestToken` for one write: "You must provide a ClientRequestToken value"
/// (`CreateSecretRequest$ClientRequestToken`) for a call no SDK client filled one in for,
/// padded to the modelled 32-character minimum of `ClientRequestTokenType`
/// (`smithy_query(query="shape",shape="ClientRequestTokenType")`).
fn version_token(label: &str) -> String {
    format!("{label:0<32}")
}

/// The account's root principal, which the unconditional grant of AWS's default key policy
/// names and which a row's own grant delegates to.
fn account_root() -> String {
    format!("arn:aws:iam::{}:root", ::engine::emulator_account_id())
}

/// The grant AWS's default key policy carries: the account that owns the key may use it, the
/// decision then resting on that account's own identity policies.
fn account_grant() -> Value {
    json!({
        "Sid": "EnableIAMUserPermissions",
        "Effect": "Allow",
        "Principal": {"AWS": account_root()},
        "Action": "kms:*",
        "Resource": "*",
    })
}

/// One row: the key policy statements it attaches to the key, and whether each of the three
/// routes is then served.
struct Row {
    case: &'static str,
    /// The statements the row's key policy holds, in order.
    statements: Vec<Value>,
    /// Whether a write and a read through the Secrets Manager envelope are served.
    envelope_served: bool,
    /// Whether the caller's own direct `kms:GenerateDataKey` on the key is served.
    direct_served: bool,
}

/// The table: one row per form a `kms:ViaService` clause takes.
fn rows() -> Vec<Row> {
    let this_service = endpoint("secretsmanager");
    let another_service = endpoint("s3");
    vec![
        Row {
            case: "no clause at all: the key is not confined to any service's path",
            statements: vec![account_grant()],
            envelope_served: true,
            direct_served: true,
        },
        Row {
            case: "a Deny on every use NOT arriving through this service",
            statements: vec![
                account_grant(),
                json!({
                    "Sid": ROW_SID,
                    "Effect": "Deny",
                    "Principal": {"AWS": "*"},
                    "Action": cryptographic_actions(),
                    "Resource": "*",
                    "Condition": {"StringNotEquals": {"kms:ViaService": [this_service.clone()]}},
                }),
            ],
            envelope_served: true,
            direct_served: false,
        },
        Row {
            case: "a Deny on every use NOT arriving through ANOTHER service",
            statements: vec![
                account_grant(),
                json!({
                    "Sid": ROW_SID,
                    "Effect": "Deny",
                    "Principal": {"AWS": "*"},
                    "Action": cryptographic_actions(),
                    "Resource": "*",
                    "Condition": {
                        "StringNotEquals": {"kms:ViaService": [another_service.clone()]},
                    },
                }),
            ],
            envelope_served: false,
            direct_served: false,
        },
        Row {
            case: "a Deny on every use carrying NO value for the key at all",
            statements: vec![
                account_grant(),
                json!({
                    "Sid": ROW_SID,
                    "Effect": "Deny",
                    "Principal": {"AWS": "*"},
                    "Action": cryptographic_actions(),
                    "Resource": "*",
                    "Condition": {"Null": {"kms:ViaService": "true"}},
                }),
            ],
            envelope_served: true,
            direct_served: false,
        },
        Row {
            case: "the key policy's only cryptographic grant is gated on this service",
            statements: vec![json!({
                "Sid": ROW_SID,
                "Effect": "Allow",
                "Principal": {"AWS": account_root()},
                "Action": cryptographic_actions(),
                "Resource": "*",
                "Condition": {"StringEquals": {"kms:ViaService": this_service}},
            })],
            envelope_served: true,
            direct_served: false,
        },
        Row {
            case: "the key policy's only cryptographic grant is gated on ANOTHER service",
            statements: vec![json!({
                "Sid": ROW_SID,
                "Effect": "Allow",
                "Principal": {"AWS": account_root()},
                "Action": cryptographic_actions(),
                "Resource": "*",
                "Condition": {"StringEquals": {"kms:ViaService": another_service}},
            })],
            envelope_served: false,
            direct_served: false,
        },
    ]
}

/// Assert `answered` is the refusal a use of the key the authorities in force do not reach is
/// answered with, and nothing else.
fn refused(case: &str, route: &str, status: u16, body: &Value) {
    assert_eq!(
        (status, error_code(body).as_str()),
        (ACCESS_DENIED_STATUS, ACCESS_DENIED),
        "case {case:?}: expected {route} to be refused HTTP {ACCESS_DENIED_STATUS} \
         {ACCESS_DENIED}, got {status} with {body}"
    );
}

/// RULE: the envelope Secrets Manager seals and opens a secret value with reaches the secret's
/// key THROUGH Secrets Manager, so `kms:ViaService` answers this service's endpoint for it,
/// while the caller's own call to KMS answers nothing for that key — and a key policy clause
/// over `kms:ViaService` therefore decides the two routes apart.
#[tokio::test]
async fn a_via_service_clause_decides_the_envelope_and_the_direct_call_apart() {
    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        // One emulator per row, so no row can see another's key or secret.
        let (router, _engine) = emulator();

        // The key, made through KMS's own wire so the secret names a key that service really
        // holds, and the secret under it — both while the default key policy is in force, the
        // way a client prepares a secret before confining its key.
        let (status, created) = kms(&router, ACCOUNT_KEY_ID, "CreateKey", &json!({})).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateKey, got {status} with {created}"
        );
        let key_arn = member(&created["KeyMetadata"], "Arn");

        let name = format!("via-service-row-{index}");
        let (status, secret) = secretsmanager(
            &router,
            ACCOUNT_KEY_ID,
            "CreateSecret",
            &json!({
                "Name": &name,
                "KmsKeyId": &key_arn,
                "SecretString": BASELINE_VALUE,
                "ClientRequestToken": version_token(&format!("create-row-{index}-")),
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateSecret, got {status} with {secret}"
        );
        let secret_arn = member(&secret, "ARN");
        let baseline_version = member(&secret, "VersionId");

        // The baseline controls: both routes are served while the key is unconfined, so what a
        // row asserts below is the clause's doing and not the setup's.
        for (route, (status, body)) in [
            (
                "the direct kms:GenerateDataKey",
                kms(
                    &router,
                    ACCOUNT_KEY_ID,
                    "GenerateDataKey",
                    &json!({"KeyId": &key_arn, "KeySpec": "AES_256"}),
                )
                .await,
            ),
            (
                "the read through the envelope",
                secretsmanager(
                    &router,
                    ACCOUNT_KEY_ID,
                    "GetSecretValue",
                    &json!({"SecretId": &secret_arn}),
                )
                .await,
            ),
        ] {
            assert_eq!(
                status, 200,
                "case {case:?}: expected the baseline {route} to be served, got {status} with \
                 {body}"
            );
        }

        // The row's own key policy, attached through KMS's own wire and read back, so what the
        // routes meet below is a document the key really carries.
        let policy = json!({"Version": "2012-10-17", "Statement": row.statements}).to_string();
        let (status, put) = kms(
            &router,
            ACCOUNT_KEY_ID,
            "PutKeyPolicy",
            &json!({
                "KeyId": &key_arn,
                "PolicyName": POLICY_NAME,
                "Policy": &policy,
                "BypassPolicyLockoutSafetyCheck": true,
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from PutKeyPolicy, got {status} with {put}"
        );
        let (status, read_back) = kms(
            &router,
            ACCOUNT_KEY_ID,
            "GetKeyPolicy",
            &json!({"KeyId": &key_arn, "PolicyName": POLICY_NAME}),
        )
        .await;
        let held = member(&read_back, "Policy");
        // KMS reports a policy in its OWN rendering (its indentation, and a one-element
        // condition list as the value itself), so the read-back is asserted on what the
        // document SAYS: the statements it holds and the condition key the row scoped them by.
        let reported = serde_json::from_str::<Value>(&held).unwrap_or(Value::Null);
        let statements = reported["Statement"].as_array().map_or(0, Vec::len);
        assert_eq!(
            (status, statements, held.contains("kms:ViaService")),
            (
                200,
                row.statements.len(),
                policy.contains("kms:ViaService")
            ),
            "case {case:?}: expected GetKeyPolicy to report the document the row attached, got \
             {status} with {read_back}"
        );

        // The WRITE through the envelope: a fresh data key under the secret's own key.
        let (status, written) = secretsmanager(
            &router,
            ACCOUNT_KEY_ID,
            "PutSecretValue",
            &json!({
                "SecretId": &secret_arn,
                "SecretString": UPDATED_VALUE,
                "ClientRequestToken": version_token(&format!("write-row-{index}-")),
            }),
        )
        .await;
        if row.envelope_served {
            assert_eq!(
                status, 200,
                "case {case:?}: expected PutSecretValue to be served through this service's path \
                 to the key, got {status} with {written}"
            );
            let new_version = member(&written, "VersionId");
            assert_ne!(
                new_version, baseline_version,
                "case {case:?}: expected the write to report a new VersionId, got \
                 {new_version:?} with {written}"
            );

            // The READ back through the envelope, for the new version and for the baseline one:
            // both are opened under the same key through the same path.
            for (route, body, expected) in [
                (
                    "the current version",
                    json!({"SecretId": &secret_arn}),
                    UPDATED_VALUE,
                ),
                (
                    "the baseline version",
                    json!({"SecretId": &secret_arn, "VersionId": &baseline_version}),
                    BASELINE_VALUE,
                ),
            ] {
                let (status, read) =
                    secretsmanager(&router, ACCOUNT_KEY_ID, "GetSecretValue", &body).await;
                assert_eq!(
                    (status, member(&read, "SecretString").as_str()),
                    (200, expected),
                    "case {case:?}: expected {route} to be served {expected:?}, got {status} \
                     with {read}"
                );
            }
        } else {
            refused(case, "PutSecretValue", status, &written);
            assert_eq!(
                error_message(&written),
                KMS_ACCESS_DENIED_MESSAGE,
                "case {case:?}: expected this service's own sentence for a KMS refusal, got \
                 {written}"
            );
            let (status, read) = secretsmanager(
                &router,
                ACCOUNT_KEY_ID,
                "GetSecretValue",
                &json!({"SecretId": &secret_arn}),
            )
            .await;
            refused(case, "GetSecretValue", status, &read);
            assert_eq!(
                error_message(&read),
                KMS_ACCESS_DENIED_MESSAGE,
                "case {case:?}: expected this service's own sentence for a KMS refusal, got {read}"
            );
        }

        // The caller's OWN call to KMS, which carries no value for the condition key at all.
        let (status, direct) = kms(
            &router,
            ACCOUNT_KEY_ID,
            "GenerateDataKey",
            &json!({"KeyId": &key_arn, "KeySpec": "AES_256"}),
        )
        .await;
        if row.direct_served {
            assert_eq!(
                status, 200,
                "case {case:?}: expected the direct kms:GenerateDataKey to be served, got \
                 {status} with {direct}"
            );
        } else {
            refused(case, "the direct kms:GenerateDataKey", status, &direct);
        }
    }
}

/// The IAM user the second test's calls are signed as: the principal a key policy can name,
/// and the one whose own call the envelope's KMS calls are made for.
const USER_NAME: &str = "via-service-reader";

/// The long-term access key id IAM holds for that user, which is how a request signed with it
/// speaks for the user rather than for the account.
const USER_KEY_ID: &str = "AKIDTHEVIASERVICEREADER";

/// The ARN IAM holds that user under, which is the spelling a key-policy statement names it by.
fn user_arn() -> String {
    format!(
        "arn:aws:iam::{}:user/{USER_NAME}",
        ::engine::emulator_account_id()
    )
}

/// Record the user, the key record that maps [`USER_KEY_ID`] to it, and `document` as its one
/// inline policy — what `iam:CreateUser`, `iam:CreateAccessKey` and `iam:PutUserPolicy` record,
/// written through IAM's own store so this service reads exactly what those handlers put in
/// force.
fn given_user(engine: &Arc<engine::InMemoryEngine>, document: &Value) {
    let minted = ::aws_smithy_types::DateTime::from_secs(0);
    let store = iam::service_state(&**engine).expect("the IAM store");
    let mut store = store.lock();
    store.users.insert(
        iam::name_key(USER_NAME),
        ::service_com_amazonaws_iam_awsidentitymanagementv20100508::ops::types::User {
            path: "/".to_string(),
            user_name: USER_NAME.to_string(),
            user_id: "AIDAVIASERVICEREADER".to_string(),
            arn: user_arn(),
            create_date: minted,
            password_last_used: None,
            permissions_boundary: None,
            tags: None,
        },
    );
    store.access_keys.insert(
        USER_KEY_ID.to_string(),
        iam::AccessKeyRecord {
            user_key: iam::name_key(USER_NAME),
            status: iam::ACCESS_KEY_ACTIVE_STATUS.to_string(),
            create_date: minted,
        },
    );
    store.user_inline_policies.insert(
        iam::name_key(USER_NAME),
        BTreeMap::from([(
            iam::name_key("api-only"),
            iam::InlinePolicy {
                policy_name: "api-only".to_string(),
                policy_document: document.to_string(),
            },
        )]),
    );
}

/// RULE, the other half: a forward access session states HOW a KMS request arrived and nothing
/// about WHO it speaks for, so a key policy that grants the cryptographic actions to ONE
/// principal through this service's path grants them for that principal's secret reads and
/// writes — and for nobody else's, however the request arrived.
#[tokio::test]
async fn a_forwarded_call_is_decided_for_the_callers_own_principal() {
    let case = "a cryptographic grant naming one principal, gated on this service's path";
    let (router, engine) = emulator();
    // The caller: an IAM user whose own policy reaches this service's API and says nothing
    // about KMS, so nothing but the KEY's own policy can admit the envelope's calls.
    given_user(
        &engine,
        &json!({
            "Version": "2012-10-17",
            "Statement": [{"Effect": "Allow", "Action": "secretsmanager:*", "Resource": "*"}],
        }),
    );

    // The key and the secret under it, prepared by the account before the key is confined.
    let (status, created) = kms(&router, ACCOUNT_KEY_ID, "CreateKey", &json!({})).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateKey, got {status} with {created}"
    );
    let key_arn = member(&created["KeyMetadata"], "Arn");
    let (status, secret) = secretsmanager(
        &router,
        ACCOUNT_KEY_ID,
        "CreateSecret",
        &json!({
            "Name": "via-service-for-one-principal",
            "KmsKeyId": &key_arn,
            "SecretString": BASELINE_VALUE,
            "ClientRequestToken": version_token("create-for-one-principal-"),
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateSecret, got {status} with {secret}"
    );
    let secret_arn = member(&secret, "ARN");

    // The key policy: the USER itself may use the key, and only through this service's path.
    // It names no other principal and delegates to no account, so it is the whole authority.
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": ROW_SID,
            "Effect": "Allow",
            "Principal": {"AWS": user_arn()},
            "Action": cryptographic_actions(),
            "Resource": "*",
            "Condition": {"StringEquals": {"kms:ViaService": endpoint("secretsmanager")}},
        }],
    })
    .to_string();
    let (status, put) = kms(
        &router,
        ACCOUNT_KEY_ID,
        "PutKeyPolicy",
        &json!({
            "KeyId": &key_arn,
            "PolicyName": POLICY_NAME,
            "Policy": &policy,
            "BypassPolicyLockoutSafetyCheck": true,
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from PutKeyPolicy, got {status} with {put}"
    );

    // The USER's write and read through the envelope: the grant names this principal and the
    // call arrives through this service, so both are served.
    let (status, written) = secretsmanager(
        &router,
        USER_KEY_ID,
        "PutSecretValue",
        &json!({
            "SecretId": &secret_arn,
            "SecretString": UPDATED_VALUE,
            "ClientRequestToken": version_token("write-for-one-principal-"),
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the write of the principal the grant names to be served, got \
         {status} with {written}"
    );
    let (status, read) = secretsmanager(
        &router,
        USER_KEY_ID,
        "GetSecretValue",
        &json!({"SecretId": &secret_arn}),
    )
    .await;
    assert_eq!(
        (status, member(&read, "SecretString").as_str()),
        (200, UPDATED_VALUE),
        "case {case:?}: expected the read of that principal to be served {UPDATED_VALUE:?}, got \
         {status} with {read}"
    );

    // ANOTHER principal's read through the same path is refused: the session did not make the
    // key usable by whoever asks, and the account's own credential is not the grantee.
    let (status, denied) = secretsmanager(
        &router,
        ACCOUNT_KEY_ID,
        "GetSecretValue",
        &json!({"SecretId": &secret_arn}),
    )
    .await;
    refused(case, "the read of a principal the grant does not name", status, &denied);
    assert_eq!(
        error_message(&denied),
        KMS_ACCESS_DENIED_MESSAGE,
        "case {case:?}: expected this service's own sentence for a KMS refusal, got {denied}"
    );

    // And the grantee's OWN direct call to KMS is refused: it carries no value for the
    // condition the grant is gated on.
    let (status, direct) = kms(
        &router,
        USER_KEY_ID,
        "GenerateDataKey",
        &json!({"KeyId": &key_arn, "KeySpec": "AES_256"}),
    )
    .await;
    refused(
        case,
        "the grantee's own direct kms:GenerateDataKey",
        status,
        &direct,
    );
}
