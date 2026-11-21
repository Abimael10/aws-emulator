// SPDX-License-Identifier: CC0-1.0

//! Whether the CALLER's own authority on a secret's customer managed key decides a read of
//! that secret, end to end over the wire: Secrets Manager, KMS, IAM and the token service on
//! ONE shared engine, with the key, the role and its policies written through those services'
//! own stores.
//!
//! THE RULE: an operation that cannot be served without using a customer managed KMS key
//! performs that use on the CALLER's behalf, so the authority in force on the key for THAT
//! caller — the key policy, and, where the key policy delegates to the account, the
//! principal's own identity-based policies — decides it, and a caller the key does not permit
//! is refused `AccessDeniedException` before the value is served.
//!
//! The rows differ along what the rule generalises over: WHERE the authority comes from (the
//! role's own identity policy under a key policy that delegates to the account, a key policy
//! naming the role itself, neither), how widely the identity grant is scoped (this key,
//! another key), and whether the secret names a customer managed key at all.
//!
//! THE RULE'S OTHER HALF, which the second table below spans: the cryptographic call is one
//! this service makes FOR the caller, so a refusal of it for want of authority is answered as
//! the access decision it is — the common `AccessDeniedException` (HTTP 400) whose whole
//! message is this service's own fixed sentence [`KMS_ACCESS_DENIED_MESSAGE`] — and nothing
//! KMS answered the service reaches the caller: not the principal the call was judged for,
//! not the cryptographic action it asked for, not the key it named, not the
//! policy-evaluation reason and not the diagnostic envelope a relayed KMS refusal ends in.
//! It is the SAME sentence however the grant misses the key (it is silent about KMS, points
//! at another key, points at the key's alias ARN rather than at the key, is conditioned on an
//! encryption-context entry the call does not carry that value for, or is overridden by an
//! explicit deny) and whichever operation needed the access — and on the batched read it is
//! one per-entry `Errors` element while the other requested secrets are still served.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_kms_trentservice::state as kms_store;
use service_com_amazonaws_secretsmanager_secretsmanager::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

use crate::identity_based_authorization::{
    error_code, error_message, given_role_policy, given_session, role_arn, session_arn,
    ACCESS_DENIED, ACCESS_DENIED_STATUS, ACCOUNT_KEY_ID, SESSION_KEY_ID,
};
use crate::secret_client_request_token::fresh_token;

/// The Region every request below signs for: a KMS key is regional, and a secret's key is
/// used in the Region the secret lives in.
const REGION: &str = "us-west-2";

/// The whole `Message` of the refusal: Secrets Manager's own sentence for a KMS call on the
/// caller's behalf that the authority in force did not reach.
const KMS_ACCESS_DENIED_MESSAGE: &str = "Access to KMS is not allowed";

/// The value the secret under the key holds, so a served read is asserted on the material.
const SECRET_VALUE: &str = "the-value-behind-a-customer-managed-key";

/// Secrets Manager, KMS, IAM and the token service over ONE shared engine, the shape
/// `core/router/src/lib.rs` mounts them in.
struct Emulator {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        router.register_service(service_com_amazonaws_kms_trentservice::EmulatedService::new(
            Arc::clone(&engine),
        ));
        router.register_service(
            service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        router.register_service(
            service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        given_session(&engine);
        Self { router, engine }
    }

    /// POST one awsJson1_1 operation of `service` signed with `access_key_id`.
    async fn call(
        &self,
        service: &str,
        target_prefix: &str,
        access_key_id: &str,
        operation: &str,
        body: &Value,
    ) -> (u16, Value) {
        let request = http::Request::builder()
            .method("POST")
            .uri(format!("https://{service}.{REGION}.amazonaws.com/"))
            .header("content-type", "application/x-amz-json-1.1")
            .header("x-amz-target", format!("{target_prefix}.{operation}"))
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 \
                     Credential={access_key_id}/20260101/{REGION}/{service}/aws4_request, \
                     SignedHeaders=host;x-amz-target, Signature=deadbeef"
                ),
            )
            .body(bytes::Bytes::from(
                serde_json::to_vec(body).expect("the request body is serializable"),
            ))
            .expect("a valid request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let parsed = if response.body().is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(response.body()).unwrap_or(Value::Null)
        };
        (status, parsed)
    }

    /// One Secrets Manager operation signed with `access_key_id`.
    async fn sm(&self, access_key_id: &str, operation: &str, body: &Value) -> (u16, Value) {
        self.call(
            "secretsmanager",
            "secretsmanager",
            access_key_id,
            operation,
            body,
        )
        .await
    }

    /// One Secrets Manager operation, as the account's own credential, that must succeed.
    async fn sm_ok(&self, case: &str, operation: &str, body: &Value) -> Value {
        let (status, parsed) = self.sm(ACCOUNT_KEY_ID, operation, body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from {operation}, got {status} with {parsed}"
        );
        parsed
    }

    /// One KMS operation, as the account's own credential, that must succeed.
    async fn kms_ok(&self, case: &str, operation: &str, body: &Value) -> Value {
        let (status, parsed) = self
            .call("kms", "TrentService", ACCOUNT_KEY_ID, operation, body)
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from KMS {operation}, got {status} with {parsed}"
        );
        parsed
    }

    /// Mint a customer managed key, carrying `policy` when the row names one and AWS's
    /// default key policy otherwise, and answer its ARN.
    async fn create_key(&self, case: &str, policy: Option<Value>) -> String {
        let mut body = json!({});
        if let Some(policy) = policy {
            body["Policy"] = json!(policy.to_string());
            body["BypassPolicyLockoutSafetyCheck"] = json!(true);
        }
        let created = self.kms_ok(case, "CreateKey", &body).await;
        created["KeyMetadata"]["Arn"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: CreateKey reported no Arn in {created}"))
            .to_string()
    }
}

/// An identity-based document allowing `actions` on `resources`.
fn allow(actions: Value, resources: Value) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{"Effect": "Allow", "Action": actions, "Resource": resources}],
    })
}

/// A key policy whose one statement allows `principal` and the account's own root every
/// cryptographic action — the shape that names a caller ITSELF rather than only delegating
/// to the account. The account is named beside it because the secret under the key is
/// created with the account's own credential.
fn key_policy_for(principal: &str) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": "named-principal",
            "Effect": "Allow",
            "Principal": {"AWS": [
                principal,
                format!("arn:aws:iam::{}:root", ::engine::emulator_account_id()),
            ]},
            "Action": "kms:*",
            "Resource": "*",
        }],
    })
}

/// The ARN of one AWS-MANAGED policy this emulator's IAM carries the published document of.
///
/// Source: `service_com_amazonaws_iam_awsidentitymanagementv20100508::state::
/// AWS_MANAGED_POLICIES`, the catalog of AWS-published policy documents — read from there
/// rather than spelled out, so a row attaches a policy IAM really publishes.
fn aws_managed_policy_arn(policy_name: &str) -> String {
    iam_catalog(policy_name).arn()
}

/// The catalog entry for `policy_name`, which a row attaches and whose document decides it.
fn iam_catalog(
    policy_name: &str,
) -> &'static service_com_amazonaws_iam_awsidentitymanagementv20100508::state::AwsManagedPolicy {
    service_com_amazonaws_iam_awsidentitymanagementv20100508::state::aws_managed_policy_named(
        policy_name,
    )
    .unwrap_or_else(|| panic!("IAM publishes the managed policy {policy_name:?}"))
}

/// Attach the AWS-managed policy `policy_arn` to the role the seeded session speaks for —
/// what `iam:AttachRolePolicy` does.
fn given_attached_policy(engine: &Arc<engine::InMemoryEngine>, policy_arn: &str) {
    use service_com_amazonaws_iam_awsidentitymanagementv20100508::state as iam;

    // The role the session is a session of, named by the ARN this suite reports for it, so
    // the name is stated in exactly one place.
    let arn = role_arn();
    let role_name = arn
        .rsplit('/')
        .next()
        .expect("the role ARN names the role");
    let store = iam::service_state(&**engine).expect("the IAM store");
    let mut store = store.lock();
    store
        .role_attached_policies
        .entry(iam::name_key(role_name))
        .or_default()
        .insert(policy_arn.to_string());
}

/// One row: what the key policy says, where the role's own authority comes from and what it
/// says about the key, and whether the read is served.
struct Row {
    case: &'static str,
    /// Whether the key carries a policy naming the role itself (else AWS's default policy,
    /// which delegates to the account).
    key_policy_names_the_role: bool,
    /// Whether the role's identity policy reaches `kms:Decrypt`, and on which key.
    identity_decrypt: Decrypt,
    /// The friendly name of an AWS-managed policy the role carries ATTACHED, if any: an
    /// attached policy's document is in force on the role exactly as an inline one is.
    attached: Option<&'static str>,
    served: bool,
}

/// What the role's own identity policy says about `kms:Decrypt`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Decrypt {
    /// Nothing at all: the policy grants this service's action alone.
    NotGranted,
    /// Granted on the key the secret names.
    OnThisKey,
    /// Granted, but on another key than the one the secret names.
    OnAnotherKey,
}

const ROWS: &[Row] = &[
    Row {
        case: "the key policy delegates to the account and the role is granted no kms:Decrypt",
        key_policy_names_the_role: false,
        identity_decrypt: Decrypt::NotGranted,
        attached: None,
        served: false,
    },
    Row {
        case: "the key policy delegates to the account and the role is granted kms:Decrypt",
        key_policy_names_the_role: false,
        identity_decrypt: Decrypt::OnThisKey,
        attached: None,
        served: true,
    },
    Row {
        case: "the role's kms:Decrypt names another key than the secret's",
        key_policy_names_the_role: false,
        identity_decrypt: Decrypt::OnAnotherKey,
        attached: None,
        served: false,
    },
    Row {
        case: "the key policy names the role itself, which grants outright",
        key_policy_names_the_role: true,
        identity_decrypt: Decrypt::NotGranted,
        attached: None,
        served: true,
    },
    Row {
        // An ATTACHED AWS-managed policy is a document like any other: this one states
        // nothing about KMS, so it leaves the read refused.
        case: "the role carries an attached AWS-managed policy that states no kms:Decrypt",
        key_policy_names_the_role: false,
        identity_decrypt: Decrypt::NotGranted,
        attached: Some("AWSLambdaBasicExecutionRole"),
        served: false,
    },
    Row {
        // ...and one that DOES reach the action is a grant, which is what shows the
        // attached document is read rather than ignored.
        case: "the role carries an attached AWS-managed policy that allows every action",
        key_policy_names_the_role: false,
        identity_decrypt: Decrypt::NotGranted,
        attached: Some("AdministratorAccess"),
        served: true,
    },
];

/// RULE: the authority in force on the secret's customer managed key decides the read FOR
/// THIS CALLER, over the documents in force on the caller's identity — its inline policies
/// AND the default version of each policy attached to it — so a role none of them lets
/// reach `kms:Decrypt` on that key is refused `AccessDeniedException` under a key policy
/// that delegates to the account, and one whose documents or whose key policy reach the key
/// is served.
#[tokio::test]
async fn a_callers_own_authority_on_the_secrets_key_decides_the_read() {
    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let emulator = Emulator::new();
        let policy = row
            .key_policy_names_the_role
            .then(|| key_policy_for(&role_arn()));
        let key_arn = emulator.create_key(case, policy).await;
        let other_key_arn = emulator.create_key(case, None).await;
        let name = format!("customer-key-authority-{index}");
        emulator
            .sm_ok(
                case,
                "CreateSecret",
                &json!({
                    "Name": &name,
                    "KmsKeyId": &key_arn,
                    "SecretString": SECRET_VALUE,
                    "ClientRequestToken": fresh_token(),
                }),
            )
            .await;

        let decrypt_resource = match row.identity_decrypt {
            Decrypt::NotGranted => None,
            Decrypt::OnThisKey => Some(key_arn.clone()),
            Decrypt::OnAnotherKey => Some(other_key_arn.clone()),
        };
        let document = match &decrypt_resource {
            None => allow(json!("secretsmanager:GetSecretValue"), json!("*")),
            Some(resource) => json!({
                "Version": "2012-10-17",
                "Statement": [
                    {
                        "Effect": "Allow",
                        "Action": "secretsmanager:GetSecretValue",
                        "Resource": "*",
                    },
                    {
                        "Effect": "Allow",
                        "Action": "kms:Decrypt",
                        "Resource": resource,
                    },
                ],
            }),
        };
        given_role_policy(&emulator.engine, Some(&document));
        if let Some(policy_name) = row.attached {
            given_attached_policy(&emulator.engine, &aws_managed_policy_arn(policy_name));
        }

        let (status, parsed) = emulator
            .sm(SESSION_KEY_ID, "GetSecretValue", &json!({"SecretId": &name}))
            .await;
        if row.served {
            assert_eq!(
                (status, parsed["SecretString"].as_str()),
                (200, Some(SECRET_VALUE)),
                "case {case:?}: expected the value to be served to {}, got {status} with \
                 {parsed}",
                session_arn()
            );
            continue;
        }
        assert_eq!(
            (
                status,
                error_code(&parsed).as_str(),
                error_message(&parsed).as_str()
            ),
            (
                ACCESS_DENIED_STATUS,
                ACCESS_DENIED,
                KMS_ACCESS_DENIED_MESSAGE
            ),
            "case {case:?}: expected the read to be refused, got {status} with {parsed}"
        );
    }
}

/// The cryptographic actions on a secret's customer managed key its life needs: `kms:Decrypt`
/// to open a stored value and `kms:GenerateDataKey` to seal a new one — "you also need
/// kms:Decrypt permissions for that key" (`GetSecretValue`, this service's Smithy model) and
/// "you need … kms:GenerateDataKey" (`CreateSecret`). Every grant below carries BOTH, so what
/// each row varies is WHERE the grant points and never which action it names.
const KEY_ACTIONS: [&str; 2] = ["kms:Decrypt", "kms:GenerateDataKey"];

/// The actions of THIS service the calls below need, granted on everything in every row: what
/// each row turns on is the authority on the KEY, so this service's own gate never decides it.
const SERVICE_ACTIONS: [&str; 4] = [
    "secretsmanager:GetSecretValue",
    "secretsmanager:PutSecretValue",
    "secretsmanager:BatchGetSecretValue",
    "secretsmanager:DescribeSecret",
];

/// The value a write stores, which a REFUSED write must leave unstored.
const REPLACEMENT_VALUE: &str = "the-value-a-refused-write-must-not-store";

/// The value of the secret left on the Amazon Web Services managed key, which names no key a
/// caller needs authority on — so it is served to the role in every row and is what shows a
/// batched read answering the entries it can beside the one it cannot.
const MANAGED_SECRET_VALUE: &str = "the-value-behind-the-service-managed-key";

/// The alias a caller gives the secret's key, so a row can scope a grant to the alias ARN of a
/// key that really carries that alias.
const KEY_ALIAS: &str = "alias/the-secrets-own-key";

/// Where the role's identity-based grant of [`KEY_ACTIONS`] points, which is what decides
/// whether any authority in force reaches the cryptographic operation on the key the secret
/// names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyAuthority {
    /// Nothing about KMS at all: the role is granted this service's actions and no more.
    Silent,
    /// The key the secret names — the positive control, and the only row that is served.
    TheKey,
    /// Another key of the same account and Region.
    AnotherKey,
    /// The ALIAS ARN of the key the secret names. An alias is a name FOR a key, not the
    /// resource a cryptographic operation is authorized on, so a statement scoped to it
    /// reaches no key.
    TheKeysAliasArn,
    /// The key the secret names, under a condition on the encryption-context entry naming the
    /// secret set to ANOTHER secret's ARN: the call carries this secret's ARN there, so the
    /// statement does not apply to it.
    TheKeyUnderAnotherSecretsContext,
    /// Everything, beside an explicit `Deny` of the same actions, under a key policy that
    /// names the role itself — so the key's own policy allows outright and the deny is what
    /// refuses: an explicit deny is final whatever else allows.
    DeniedOverAKeyPolicyAllow,
}

impl KeyAuthority {
    /// Every shape of grant under test.
    const EVERY: [Self; 6] = [
        Self::Silent,
        Self::TheKey,
        Self::AnotherKey,
        Self::TheKeysAliasArn,
        Self::TheKeyUnderAnotherSecretsContext,
        Self::DeniedOverAKeyPolicyAllow,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Silent => "the role's policy states nothing about KMS",
            Self::TheKey => "the role is granted the actions on the key itself",
            Self::AnotherKey => "the grant names another key",
            Self::TheKeysAliasArn => "the grant names the key's alias ARN",
            Self::TheKeyUnderAnotherSecretsContext => {
                "the grant is conditioned on another secret's encryption context"
            }
            Self::DeniedOverAKeyPolicyAllow => {
                "an explicit deny overrides a key policy naming the role"
            }
        }
    }

    /// Whether any authority in force reaches the cryptographic operation on the secret's key,
    /// which is the one row whose calls are served.
    fn reaches_the_key(self) -> bool {
        self == Self::TheKey
    }

    /// Whether the key carries a policy naming the role ITSELF, which only the explicit-deny
    /// row needs: every other row leaves AWS's default policy in place, so the role's own
    /// identity policy is the document that decides.
    fn key_policy_names_the_role(self) -> bool {
        self == Self::DeniedOverAKeyPolicyAllow
    }

    /// The identity-based document this row puts in force on the role: this service's actions
    /// on everything, and the cryptographic actions wherever this row points them.
    fn document(self, target: &Target) -> Value {
        let mut statements = vec![json!({
            "Effect": "Allow",
            "Action": SERVICE_ACTIONS,
            "Resource": "*",
        })];
        match self {
            Self::Silent => {}
            Self::TheKey => statements.push(json!({
                "Effect": "Allow",
                "Action": KEY_ACTIONS,
                "Resource": &target.key_arn,
            })),
            Self::AnotherKey => statements.push(json!({
                "Effect": "Allow",
                "Action": KEY_ACTIONS,
                "Resource": &target.other_key_arn,
            })),
            Self::TheKeysAliasArn => statements.push(json!({
                "Effect": "Allow",
                "Action": KEY_ACTIONS,
                "Resource": &target.key_alias_arn,
            })),
            Self::TheKeyUnderAnotherSecretsContext => statements.push(json!({
                "Effect": "Allow",
                "Action": KEY_ACTIONS,
                "Resource": &target.key_arn,
                // "kms:EncryptionContext:<context-key> … compares the encryption context pair
                // in the request with the pair specified in the policy" (KMS's own condition
                // keys). The service seals and opens a version under the entry naming the
                // secret the version belongs to, so a statement naming another secret's ARN
                // there covers none of this secret's calls.
                "Condition": {
                    "StringEquals": {
                        "kms:EncryptionContext:SecretARN": &target.managed_secret_arn,
                    },
                },
            })),
            Self::DeniedOverAKeyPolicyAllow => {
                statements.push(json!({
                    "Effect": "Allow",
                    "Action": KEY_ACTIONS,
                    "Resource": "*",
                }));
                statements.push(json!({
                    "Effect": "Deny",
                    "Action": KEY_ACTIONS,
                    "Resource": "*",
                }));
            }
        }
        json!({"Version": "2012-10-17", "Statement": statements})
    }
}

/// Which call needs the key, and therefore how the refusal reaches the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Call {
    /// `GetSecretValue`: the read needs `kms:Decrypt`, and the refusal is the response's own
    /// error.
    Read,
    /// `PutSecretValue`: the write needs `kms:GenerateDataKey`, the refusal is the response's
    /// own error, and a refused write stores nothing.
    Write,
    /// `BatchGetSecretValue`: the call answers 200 and the refusal belongs to the ONE entry it
    /// covers — "If Secrets Manager encounters errors such as AccessDeniedException while
    /// attempting to retrieve any of the secrets, you can see the errors in Errors in the
    /// response." (`BatchGetSecretValue`, this service's Smithy model.)
    BatchedRead,
}

impl Call {
    const EVERY: [Self; 3] = [Self::Read, Self::Write, Self::BatchedRead];

    fn label(self) -> &'static str {
        match self {
            Self::Read => "GetSecretValue",
            Self::Write => "PutSecretValue",
            Self::BatchedRead => "BatchGetSecretValue",
        }
    }
}

/// What one row's requests and documents are written against: the key the secret names, a
/// second key, the key's alias ARN, and the ARNs of the two secrets.
struct Target {
    key_arn: String,
    other_key_arn: String,
    key_alias_arn: String,
    secret_arn: String,
    managed_secret_arn: String,
}

/// RULE: a KMS authorization refusal of the cryptographic call this service makes on the
/// caller's behalf for a secret's customer managed key is answered as the common
/// `AccessDeniedException` (HTTP 400) whose whole `Message` is this service's own fixed
/// sentence [`KMS_ACCESS_DENIED_MESSAGE`], carrying nothing of what KMS answered the service —
/// however the authority in force misses that key and whichever operation needed it, and as
/// one per-entry `Errors` element on the batched read, whose other entries are still served.
#[tokio::test]
async fn a_kms_authorization_refusal_is_answered_the_services_own_fixed_sentence() {
    for authority in KeyAuthority::EVERY {
        for call in Call::EVERY {
            let case = format!("{}, {}", authority.label(), call.label());
            let emulator = Emulator::new();
            let policy = authority
                .key_policy_names_the_role()
                .then(|| key_policy_for(&role_arn()));
            let key_arn = emulator.create_key(&case, policy).await;
            let other_key_arn = emulator.create_key(&case, None).await;
            // The alias is minted the way a caller mints one, so the row that scopes a grant
            // to an alias ARN names an alias the key really carries.
            emulator
                .kms_ok(
                    &case,
                    "CreateAlias",
                    &json!({"AliasName": KEY_ALIAS, "TargetKeyId": &key_arn}),
                )
                .await;

            // The secret under the key, and a second secret left on the Amazon Web Services
            // managed key: the one whose ARN a context-scoped grant names, and the entry a
            // batched read still serves whatever the customer managed key answered.
            let created = emulator
                .sm_ok(
                    &case,
                    "CreateSecret",
                    &json!({
                        "Name": "under-a-customer-managed-key",
                        "KmsKeyId": &key_arn,
                        "SecretString": SECRET_VALUE,
                        "ClientRequestToken": fresh_token(),
                    }),
                )
                .await;
            let managed = emulator
                .sm_ok(
                    &case,
                    "CreateSecret",
                    &json!({
                        "Name": "on-the-service-managed-key",
                        "SecretString": MANAGED_SECRET_VALUE,
                        "ClientRequestToken": fresh_token(),
                    }),
                )
                .await;
            let target = Target {
                key_alias_arn: kms_store::alias_arn(REGION, KEY_ALIAS),
                key_arn,
                other_key_arn,
                secret_arn: reported_arn(&created, &case),
                managed_secret_arn: reported_arn(&managed, &case),
            };
            given_role_policy(&emulator.engine, Some(&authority.document(&target)));

            match call {
                Call::Read => {
                    let (status, parsed) = emulator
                        .sm(
                            SESSION_KEY_ID,
                            "GetSecretValue",
                            &json!({"SecretId": &target.secret_arn}),
                        )
                        .await;
                    if authority.reaches_the_key() {
                        assert_eq!(
                            (status, parsed["SecretString"].as_str()),
                            (200, Some(SECRET_VALUE)),
                            "case {case:?}: expected the value to be served, got {status} with \
                             {parsed}"
                        );
                    } else {
                        assert_refused_by_kms(&case, status, &parsed, &target);
                    }
                }
                Call::Write => {
                    let (status, parsed) = emulator
                        .sm(
                            SESSION_KEY_ID,
                            "PutSecretValue",
                            &json!({
                                "SecretId": &target.secret_arn,
                                "SecretString": REPLACEMENT_VALUE,
                                "ClientRequestToken": fresh_token(),
                            }),
                        )
                        .await;
                    let expected = if authority.reaches_the_key() {
                        assert_eq!(
                            status, 200,
                            "case {case:?}: expected the write to be stored, got {status} with \
                             {parsed}"
                        );
                        REPLACEMENT_VALUE
                    } else {
                        assert_refused_by_kms(&case, status, &parsed, &target);
                        SECRET_VALUE
                    };
                    // What the secret HOLDS afterwards, read back through the account's own
                    // credential: a served write stored its value and a refused one stored
                    // nothing.
                    let read = emulator
                        .sm_ok(
                            &case,
                            "GetSecretValue",
                            &json!({"SecretId": &target.secret_arn}),
                        )
                        .await;
                    assert_eq!(
                        read["SecretString"].as_str(),
                        Some(expected),
                        "case {case:?}: expected the secret to hold {expected:?}, got {read}"
                    );
                }
                Call::BatchedRead => {
                    let (status, parsed) = emulator
                        .sm(
                            SESSION_KEY_ID,
                            "BatchGetSecretValue",
                            &json!({
                                "SecretIdList": [&target.managed_secret_arn, &target.secret_arn],
                            }),
                        )
                        .await;
                    assert_eq!(
                        status, 200,
                        "case {case:?}: a batched read answers 200 whatever one entry met, got \
                         {status} with {parsed}"
                    );
                    // The secret on the service-managed key is served in every row: a refusal
                    // of one entry's key is that entry's alone.
                    let served = served_values(&parsed, &case);
                    let mut want: Vec<(String, String)> = vec![(
                        target.managed_secret_arn.clone(),
                        MANAGED_SECRET_VALUE.to_string(),
                    )];
                    if authority.reaches_the_key() {
                        want.push((target.secret_arn.clone(), SECRET_VALUE.to_string()));
                    }
                    assert_eq!(
                        served, want,
                        "case {case:?}: expected the batch to serve {want:?}, got {parsed}"
                    );

                    let errors = parsed["Errors"]
                        .as_array()
                        .unwrap_or_else(|| panic!("case {case:?}: no Errors in {parsed}"))
                        .clone();
                    if authority.reaches_the_key() {
                        assert!(
                            errors.is_empty(),
                            "case {case:?}: expected no per-entry error, got {parsed}"
                        );
                        continue;
                    }
                    assert_eq!(
                        errors.len(),
                        1,
                        "case {case:?}: expected the one secret under the key the caller cannot \
                         use to report one error, got {parsed}"
                    );
                    let error = &errors[0];
                    assert_eq!(
                        (
                            error["SecretId"].as_str(),
                            error["ErrorCode"].as_str(),
                            error["Message"].as_str()
                        ),
                        (
                            Some(target.secret_arn.as_str()),
                            Some(ACCESS_DENIED),
                            Some(KMS_ACCESS_DENIED_MESSAGE)
                        ),
                        "case {case:?}: expected the per-entry refusal of \
                         {:?}, got {error}",
                        target.secret_arn
                    );
                    assert_states_nothing_of_the_kms_answer(
                        &case,
                        error["Message"].as_str().unwrap_or_default(),
                        &target,
                    );
                }
            }
        }
    }
}

/// Assert `parsed` is this service's own refusal of a KMS call the authority in force did not
/// reach: HTTP 400, the common `AccessDeniedException`, and the whole fixed sentence.
fn assert_refused_by_kms(case: &str, status: u16, parsed: &Value, target: &Target) {
    let message = error_message(parsed);
    assert_eq!(
        (status, error_code(parsed).as_str(), message.as_str()),
        (
            ACCESS_DENIED_STATUS,
            ACCESS_DENIED,
            KMS_ACCESS_DENIED_MESSAGE
        ),
        "case {case:?}: expected the call to be refused {KMS_ACCESS_DENIED_MESSAGE:?}, got \
         {status} with {parsed}"
    );
    assert_states_nothing_of_the_kms_answer(case, &message, target);
}

/// Assert `message` carries nothing of the answer KMS gave the SERVICE: the cryptographic call
/// is the service's own, so the principal it was judged for, the action it asked for, the key
/// it named, the policy-evaluation reason and the diagnostic envelope a relayed refusal ends
/// in are all withheld from the caller — the sentence has no variable part at all.
fn assert_states_nothing_of_the_kms_answer(case: &str, message: &str, target: &Target) {
    let withheld = [
        session_arn(),
        role_arn(),
        target.key_arn.clone(),
        target.key_alias_arn.clone(),
        "kms:".to_string(),
        "AWSKMS".to_string(),
        "Request ID".to_string(),
        "policy".to_string(),
    ];
    for fragment in withheld {
        assert!(
            !message.contains(&fragment),
            "case {case:?}: the refusal states {fragment:?}, which belongs to the answer KMS \
             gave this service: {message:?}"
        );
    }
}

/// The ARN a create reported for the secret it made.
fn reported_arn(created: &Value, case: &str) -> String {
    created["ARN"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the create reported no ARN in {created}"))
        .to_string()
}

/// The `SecretValues` of a batched read, as (ARN, SecretString) pairs in the order answered.
fn served_values(parsed: &Value, case: &str) -> Vec<(String, String)> {
    parsed["SecretValues"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: no SecretValues in {parsed}"))
        .iter()
        .map(|entry| {
            (
                entry["ARN"].as_str().unwrap_or_default().to_string(),
                entry["SecretString"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
            )
        })
        .collect()
}
