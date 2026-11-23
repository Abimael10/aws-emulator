// SPDX-License-Identifier: CC0-1.0

//! Which reads of an SSE-KMS object a GRANT on the object's own KMS key reaches, and what an
//! explicit deny in that key's policy does to one — across Amazon S3, KMS, IAM and STS on one
//! engine.
//!
//! RULE: serving the bytes of an object stored under a customer-managed KMS key performs
//! `kms:Decrypt` on that key as the principal the request's credential speaks for and through
//! S3's own service path, so such a read is served exactly when the authorities in force on that
//! key reach that action for that principal — the key's own policy, the principal's
//! identity-based documents, and the GRANTS on the key issued to it — and a read no authority
//! reaches is refused S3's `AccessDenied` (HTTP 403) carrying KMS's own denial sentence instead
//! of the body. A grant permits exactly the operations it lists, to the grantee principal it
//! names, under its own encryption-context constraints, which are judged against the context S3
//! binds an object's data key to; an explicit `Deny` in the key policy overrides every allow, a
//! grant included; and withdrawing either — `RevokeGrant`, or a `PutKeyPolicy` that drops the
//! deny — changes the very next read, because each read is decided afresh.
//!
//! Sources. "If the object is encrypted with SSE-KMS, you must also have the kms:GenerateDataKey
//! and kms:Decrypt permissions in IAM identity-based policies and KMS key policies for the KMS
//! key" (`HeadObject`'s own documentation of what such a read needs). A grant is "a policy
//! instrument that allows Amazon Web Services principals to use KMS keys in cryptographic
//! operations", it lists "the operations that the grant permits"
//! (`smithy_query(query="doc",shape="CreateGrantRequest",member="Operations")`), and its
//! constraints "allow the permissions in the grant only when the encryption context in the
//! request matches (EncryptionContextEquals) or includes (EncryptionContextSubset) the encryption
//! context specified in the constraint"
//! (`smithy_query(query="doc",shape="CreateGrantRequest",member="Constraints")`); `RevokeGrant`
//! "terminates the permissions that the grant allows"
//! (`smithy_query(query="doc",shape="RevokeGrant")`). "An explicit deny in any policy overrides
//! any allows" (IAM User Guide, *Policy evaluation logic*). The entry S3 binds is the ARN of what
//! was encrypted under `aws:s3:arn` (Amazon S3 User Guide, *Using server-side encryption with KMS
//! keys*, "Encryption context").
//!
//! Four crates have to agree for that to hold — KMS holds the key, its policy and its grants and
//! takes the decision, Amazon S3 stores the object and asks for the decrypt on the reader's
//! behalf under its own encryption context, IAM holds the roles and their inline documents, and
//! STS mints the session each read is signed as — so the suite lives here, where a crate-local
//! one could only see one side of it. What the neighbours pin and this one does not repeat: the
//! Amazon S3 crate's `sse_kms_decrypt_authorization` tables the same read against KEY POLICY
//! statements and their conditions, and `lambda_in_function_kms_grant_and_key_policy` tables a
//! grant against a handler's OWN `kms:Decrypt` call made to KMS directly. What only this suite
//! reaches is a grant deciding the decrypt S3 performs on a caller's behalf, judged under the
//! encryption context S3 itself bound.
//!
//! The rows differ along what the grant rule generalises over: WHICH operations the grant lists
//! (the decrypt alone, several including it, another one only, none at all), WHOSE grant it is
//! (the reading role, another role of the account) and WHICH encryption-context constraint it
//! carries (none, a subset naming the object read, a subset naming the other object under the
//! same key, an equality over the whole context S3 bound). Every row probes BOTH objects stored
//! under its own key, so a grant reaching further than the object its constraint names fails
//! here, and then revokes the grant and probes both again.
//!
//! The requests are driven through a router carrying those four services with request
//! authentication off, as every suite in this test crate drives it: what each call carries is a
//! credential scope, which is where the Region and the calling access key id are read.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, form_encode, opt_element};

/// The Region every request here is signed for, and so the Region the key, the bucket, the roles
/// and the sessions belong to.
const REGION: &str = "us-east-1";

/// The account this emulator serves, which every ARN below carries.
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// The access-key id standing for the account's own published credential: neither STS nor IAM
/// holds a record of it, so it resolves to the account's ambient identity — the position AWS
/// account root credentials are in. Every setup call is made with it.
const ACCOUNT_KEY: &str = "AKIDEMULATOR";

/// The bucket every object of this suite is stored in.
const BUCKET: &str = "sse-kms-grant-authority";

/// The bytes every object holds. A grant is about who may have an object's plaintext, not about
/// what is in it, so one payload serves every row.
const BODY: &[u8] = b"sse-kms payload behind a grant";

/// The `ServerSideEncryption` value that is SSE-KMS, the one a KMS key id belongs to
/// (`smithy_query(query="shape",shape="ServerSideEncryption")` lists `aws:kms` among its values).
const SSE_KMS: &str = "aws:kms";

/// The `GrantOperation` a read that serves an SSE-KMS object's bytes performs on the key, and the
/// IAM action it is authorized under (source: `smithy_query(query="shape",shape="GrantOperation")`
/// for the first, which lists `Decrypt`, `Encrypt` and `GenerateDataKey` among its values).
const DECRYPT_OPERATION: &str = "Decrypt";
const DECRYPT_ACTION: &str = "kms:Decrypt";

/// The encryption-context ENTRY NAME S3 binds an object's data key to (Amazon S3 User Guide,
/// *Using server-side encryption with KMS keys*, "Encryption context"), which is what a grant
/// constraint over an S3 object names.
const ARN_CONTEXT_ENTRY: &str = "aws:s3:arn";

/// The code S3 answers an authorization refusal with, and the status it carries (Amazon S3 "Error
/// responses": `AccessDenied`, HTTP 403).
const ACCESS_DENIED: &str = "AccessDenied";
const ACCESS_DENIED_STATUS: u16 = 403;

/// The response headers a read of an SSE-KMS object reports its encryption in.
const SSE_HEADER: &str = "x-amz-server-side-encryption";
const SSE_KEY_ID_HEADER: &str = "x-amz-server-side-encryption-aws-kms-key-id";

/// One call's answer: the status, the bound headers and the raw body.
struct Answer {
    status: u16,
    headers: http::HeaderMap,
    body: bytes::Bytes,
}

impl Answer {
    /// A bound response header's value.
    fn header(&self, name: &str) -> Option<String> {
        self.headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
    }

    /// The body as text, for S3's restXml error documents.
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    /// The `<Code>` of an S3 error document, or the whole body when it carries none.
    fn error_code(&self) -> String {
        let body = self.text();
        opt_element(&body, "Code").unwrap_or(body)
    }

    /// The `<Message>` of an S3 error document, or the whole body when it carries none.
    fn error_message(&self) -> String {
        let body = self.text();
        opt_element(&body, "Message").unwrap_or(body)
    }
}

/// One role session a read is made as: the access key id its requests are signed with, the
/// assumed-role session ARN a refusal names, and the ARN of the role a grant names it by.
#[derive(Debug, Clone)]
struct Session {
    access_key_id: String,
    session_arn: String,
    role_arn: String,
}

/// An emulator holding the four services one such read is decided by.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<::engine::InMemoryEngine> = Arc::new(::engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // Nothing here is signed: the `Authorization` header carries only the credential scope,
        // which is where a service reads the Region and the caller's access key id.
        router.set_request_authentication(false);
        router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
            Arc::clone(&engine),
        ));
        router.register_service(
            service_com_amazonaws_kms_trentservice::EmulatedService::new(Arc::clone(&engine)),
        );
        router.register_service(
            service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        router.register_service(
            service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(
                engine,
            ),
        );
        Self { router }
    }

    /// One Amazon S3 request, addressed path-style and signed with `access_key_id` in its SigV4
    /// credential scope — which is where this emulator reads which principal a request speaks
    /// for.
    async fn s3(
        &self,
        access_key_id: &str,
        method: &str,
        uri: &str,
        headers: &[(&str, &str)],
        body: &[u8],
    ) -> Answer {
        let host = format!("s3.{REGION}.amazonaws.com");
        let mut builder = http::Request::builder()
            .method(method)
            .uri(format!("https://{host}{uri}"))
            .header("host", host)
            .header("authorization", authorization(access_key_id, "s3"));
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        let request = builder
            .body(bytes::Bytes::copy_from_slice(body))
            .expect("the test request must build");
        let response = self.router.dispatch(request).await;
        let (parts, body) = response.into_parts();
        Answer {
            status: parts.status.as_u16(),
            headers: parts.headers,
            body,
        }
    }

    /// One KMS `awsJson1_1` call as the account's own credential, reached by
    /// `X-Amz-Target: TrentService.<Operation>`.
    async fn kms(&self, operation: &str, body: &Value) -> (u16, Value) {
        let request = http::Request::builder()
            .method("POST")
            .uri(format!("https://kms.{REGION}.amazonaws.com/"))
            .header("content-type", "application/x-amz-json-1.1")
            .header("x-amz-target", format!("TrentService.{operation}"))
            .header("authorization", authorization(ACCOUNT_KEY, "kms"))
            .body(bytes::Bytes::from(body.to_string()))
            .expect("the test request must build");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let payload = if response.body().is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(response.body()).unwrap_or(Value::Null)
        };
        (status, payload)
    }

    /// A KMS call that must succeed.
    async fn kms_ok(&self, case: &str, operation: &str, body: &Value) -> Value {
        let (status, payload) = self.kms(operation, body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: KMS {operation} {body} must succeed, got {status} with {payload}"
        );
        payload
    }

    /// One awsQuery call to IAM or STS, as the account's own credential.
    async fn query(
        &self,
        signing_name: &str,
        version: &str,
        action: &str,
        params: &[(&str, String)],
    ) -> (u16, String) {
        let mut body = format!("Action={action}&Version={version}");
        for (name, value) in params {
            body.push('&');
            body.push_str(name);
            body.push('=');
            body.push_str(&form_encode(value));
        }
        let request = http::Request::builder()
            .method("POST")
            .uri(format!("https://{signing_name}.amazonaws.com/"))
            .header("content-type", "application/x-www-form-urlencoded")
            .header("authorization", authorization(ACCOUNT_KEY, signing_name))
            .body(bytes::Bytes::from(body))
            .expect("a well-formed awsQuery request");
        let response = self.router.dispatch(request).await;
        (
            response.status().as_u16(),
            String::from_utf8_lossy(response.body()).into_owned(),
        )
    }

    /// Create an IAM role carrying `policy` as its inline document, assume it through STS, and
    /// answer the session that assume minted.
    ///
    /// The document is the role's permission for THIS service's own gate on the read
    /// (`s3:GetObject`); nothing in it reaches a KMS action, so what decides the decrypt is the
    /// key's own authorities, which is the subject here.
    async fn assumed_role(&self, role_name: &str, policy: &str) -> Session {
        let role_arn = format!("arn:aws:iam::{ACCOUNT}:role/{role_name}");
        let trust = json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Effect": "Allow",
                "Principal": {"AWS": format!("arn:aws:iam::{ACCOUNT}:root")},
                "Action": "sts:AssumeRole",
            }],
        })
        .to_string();
        let (status, body) = self
            .query(
                "iam",
                "2010-05-08",
                "CreateRole",
                &[
                    ("RoleName", role_name.to_string()),
                    ("AssumeRolePolicyDocument", trust),
                ],
            )
            .await;
        assert_eq!(
            status, 200,
            "creating the role {role_name:?} must succeed, got {status} {body}"
        );
        let (status, body) = self
            .query(
                "iam",
                "2010-05-08",
                "PutRolePolicy",
                &[
                    ("RoleName", role_name.to_string()),
                    ("PolicyName", "inline".to_string()),
                    ("PolicyDocument", policy.to_string()),
                ],
            )
            .await;
        assert_eq!(
            status, 200,
            "putting the inline policy of {role_name:?} must succeed, got {status} {body}"
        );
        let (status, body) = self
            .query(
                "sts",
                "2011-06-15",
                "AssumeRole",
                &[
                    ("RoleArn", role_arn.clone()),
                    ("RoleSessionName", "probe".to_string()),
                ],
            )
            .await;
        assert_eq!(
            status, 200,
            "assuming {role_name:?} must succeed, got {status} {body}"
        );
        Session {
            access_key_id: element(&body, "AccessKeyId"),
            session_arn: element(&body, "Arn"),
            role_arn,
        }
    }

    /// A symmetric encryption key carrying `policy` as its key policy, as `(key id, key ARN)`.
    ///
    /// The document is the one under test, so it need not leave the creating caller a path to a
    /// later `kms:PutKeyPolicy`; KMS's policy-lockout safety check has its own tests in the KMS
    /// crate, so the key is created the way the model provides for.
    async fn key(&self, case: &str, policy: &str) -> (String, String) {
        let metadata = self
            .kms_ok(
                case,
                "CreateKey",
                &json!({
                    "Description": case,
                    "Policy": policy,
                    "BypassPolicyLockoutSafetyCheck": true,
                }),
            )
            .await["KeyMetadata"]
            .clone();
        (
            text(case, &metadata, "KeyId"),
            text(case, &metadata, "Arn"),
        )
    }

    /// Attach `policy` to `key_id` — the one call that changes which principals the key's own
    /// document reaches.
    async fn put_key_policy(&self, case: &str, key_id: &str, policy: &str) {
        self.kms_ok(
            case,
            "PutKeyPolicy",
            &json!({
                "KeyId": key_id,
                "Policy": policy,
                "BypassPolicyLockoutSafetyCheck": true,
            }),
        )
        .await;
    }

    /// Issue a grant on `key_id` to `grantee` permitting `operations` under `constraints`, and
    /// answer the `GrantId` KMS minted for it.
    async fn create_grant(
        &self,
        case: &str,
        key_id: &str,
        grantee: &str,
        operations: &[&str],
        constraints: Option<Value>,
    ) -> String {
        let mut request = json!({
            "KeyId": key_id,
            "GranteePrincipal": grantee,
            "Operations": operations,
        });
        if let Some(constraints) = constraints {
            request["Constraints"] = constraints;
        }
        let created = self.kms_ok(case, "CreateGrant", &request).await;
        text(case, &created, "GrantId")
    }

    /// Withdraw the grant `grant_id` on `key_id`.
    async fn revoke_grant(&self, case: &str, key_id: &str, grant_id: &str) {
        self.kms_ok(
            case,
            "RevokeGrant",
            &json!({"KeyId": key_id, "GrantId": grant_id}),
        )
        .await;
    }

    /// `PUT /<bucket>` — the bucket every object of this suite is stored in.
    async fn create_bucket(&self) {
        let created = self
            .s3(ACCOUNT_KEY, "PUT", &format!("/{BUCKET}"), &[], b"")
            .await;
        assert_eq!(
            created.status, 200,
            "the bucket must be created first, got {} with body {:?}",
            created.status,
            created.text()
        );
    }

    /// Store `object_key` under SSE-KMS with `kms_key_id`, as the account — the write every read
    /// below is of — and answer the key id the write reported for it.
    async fn put_object(&self, case: &str, object_key: &str, kms_key_id: &str) -> String {
        let written = self
            .s3(
                ACCOUNT_KEY,
                "PUT",
                &format!("/{BUCKET}/{object_key}"),
                &[
                    (SSE_HEADER, SSE_KMS),
                    (SSE_KEY_ID_HEADER, kms_key_id),
                ],
                BODY,
            )
            .await;
        assert_eq!(
            written.status, 200,
            "case {case:?}: the object {object_key:?} must be stored first, got {} with body {:?}",
            written.status,
            written.text()
        );
        written.header(SSE_KEY_ID_HEADER).unwrap_or_else(|| {
            panic!("case {case:?}: the write of {object_key:?} must report the key it used")
        })
    }

    /// `GET /<bucket>/<key>` as `access_key_id` — the read under test.
    async fn get_object(&self, access_key_id: &str, object_key: &str) -> Answer {
        self.s3(
            access_key_id,
            "GET",
            &format!("/{BUCKET}/{object_key}"),
            &[],
            b"",
        )
        .await
    }
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name` and is signed with
/// `access_key_id`, which is what routes the request and names its caller.
fn authorization(access_key_id: &str, signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/{REGION}/{signing_name}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One string member of a JSON answer.
fn text(case: &str, payload: &Value, member: &str) -> String {
    payload[member]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a {member}, got {payload}"))
        .to_string()
}

/// A key policy document carrying exactly `statements`.
fn policy(statements: Vec<Value>) -> String {
    json!({"Version": "2012-10-17", "Statement": statements}).to_string()
}

/// The statement the DEFAULT key policy carries: the account's root user, which delegates the
/// decision to that account's own identity-based policies — so a role of the account whose own
/// documents reach no KMS action holds nothing from it, and the account's own credential does.
fn allow_account() -> Value {
    json!({
        "Effect": "Allow",
        "Principal": {"AWS": format!("arn:aws:iam::{ACCOUNT}:root")},
        "Action": "kms:*",
        "Resource": "*",
    })
}

/// A statement denying `action` to `principal` over every resource.
fn deny(principal: &str, action: &str) -> Value {
    json!({
        "Effect": "Deny",
        "Principal": {"AWS": principal},
        "Action": action,
        "Resource": "*",
    })
}

/// An identity document allowing `actions` on every resource — what a reading role carries so
/// S3's own gate admits its `GetObject`.
fn identity_policy(actions: Value) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{"Effect": "Allow", "Action": actions, "Resource": "*"}],
    })
    .to_string()
}

/// The ARN of the object `key` in [`BUCKET`]: the value of the encryption-context entry S3 binds
/// an object's data key to, which is what a grant constraint over that object names. An S3
/// bucket's ARN leaves the Region and account fields empty, "because a bucket name is globally
/// unique".
fn object_arn(key: &str) -> String {
    format!("arn:aws:s3:::{BUCKET}/{key}")
}

/// The sentence KMS words a refusal of a caller whose key policy DELEGATES the action to its
/// account and whose own identity documents do not allow it, written out here rather than built
/// from either service's helper so the wording has something to be wrong about: who was refused,
/// which action, on which key, and the clause naming the document that has to change.
fn no_identity_policy(principal: &str, key_arn: &str) -> String {
    format!(
        "User: {principal} is not authorized to perform: {DECRYPT_ACTION} on resource: \
         {key_arn} because no identity-based policy allows the {DECRYPT_ACTION} action"
    )
}

/// The sentence KMS words a refusal an explicit deny in the KEY's own document took, written out
/// for the same reason.
fn explicit_deny(principal: &str, key_arn: &str) -> String {
    format!(
        "User: {principal} is not authorized to perform: {DECRYPT_ACTION} on resource: \
         {key_arn} with an explicit deny in a resource-based policy"
    )
}

/// Which encryption-context constraint a row's grant carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Constraint {
    /// None at all, so the grant covers every read of the key.
    Unconstrained,
    /// `EncryptionContextSubset` naming the entry of the FIRST object under the row's key.
    SubsetOfFirst,
    /// `EncryptionContextSubset` naming the entry of the SECOND object under the row's key.
    SubsetOfSecond,
    /// `EncryptionContextEquals` over the whole context S3 binds for the FIRST object, which for
    /// a write supplying none of its own is that one entry.
    EqualsFirst,
}

/// Whose grant a row issues.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Grantee {
    /// The role the reads below are made as.
    Reader,
    /// Another role of the same account, which the reading sessions are no session of.
    Other,
}

/// One row: the grant issued on this row's own key, and what each of the two objects stored under
/// that key must answer the reading session.
struct Row {
    case: &'static str,
    /// The object-key prefix this row's two objects are stored under, so each row reads its own.
    prefix: &'static str,
    /// The `GrantOperation`s the grant lists; empty for a row that issues no grant at all.
    operations: &'static [&'static str],
    grantee: Grantee,
    constraint: Constraint,
    /// Whether the read of the FIRST object must be served.
    first_served: bool,
    /// Whether the read of the SECOND object must be served.
    second_served: bool,
}

/// The rows, spanning the operations a grant lists, whose grant it is, and the
/// encryption-context constraint it carries.
const ROWS: &[Row] = &[
    Row {
        case: "a grant of the decrypt to the reading role",
        prefix: "decrypt-only",
        operations: &[DECRYPT_OPERATION],
        grantee: Grantee::Reader,
        constraint: Constraint::Unconstrained,
        first_served: true,
        second_served: true,
    },
    Row {
        case: "a grant listing several operations, the decrypt among them",
        prefix: "several-operations",
        operations: &["Encrypt", DECRYPT_OPERATION, "GenerateDataKey"],
        grantee: Grantee::Reader,
        constraint: Constraint::Unconstrained,
        first_served: true,
        second_served: true,
    },
    Row {
        case: "a grant listing another operation only",
        prefix: "another-operation",
        operations: &["Encrypt"],
        grantee: Grantee::Reader,
        constraint: Constraint::Unconstrained,
        first_served: false,
        second_served: false,
    },
    Row {
        case: "a grant of the decrypt to ANOTHER role of the account",
        prefix: "another-grantee",
        operations: &[DECRYPT_OPERATION],
        grantee: Grantee::Other,
        constraint: Constraint::Unconstrained,
        first_served: false,
        second_served: false,
    },
    Row {
        case: "a context subset naming the first object's own entry",
        prefix: "subset-first",
        operations: &[DECRYPT_OPERATION],
        grantee: Grantee::Reader,
        constraint: Constraint::SubsetOfFirst,
        first_served: true,
        second_served: false,
    },
    Row {
        case: "a context subset naming the other object under the same key",
        prefix: "subset-second",
        operations: &[DECRYPT_OPERATION],
        grantee: Grantee::Reader,
        constraint: Constraint::SubsetOfSecond,
        first_served: false,
        second_served: true,
    },
    Row {
        case: "a context equality over the whole context the first object's write bound",
        prefix: "equals-first",
        operations: &[DECRYPT_OPERATION],
        grantee: Grantee::Reader,
        constraint: Constraint::EqualsFirst,
        first_served: true,
        second_served: false,
    },
    Row {
        case: "no grant on the key at all",
        prefix: "no-grant",
        operations: &[],
        grantee: Grantee::Reader,
        constraint: Constraint::Unconstrained,
        first_served: false,
        second_served: false,
    },
];

/// The key policy every row's key carries: the account delegation alone, so nothing in the
/// document reaches the reading role and a served read is served by the GRANT.
fn delegating_policy() -> String {
    policy(vec![allow_account()])
}

/// The constraint document a row's grant carries, judged against the context S3 binds.
fn constraint_document(constraint: Constraint, first: &str, second: &str) -> Option<Value> {
    match constraint {
        Constraint::Unconstrained => None,
        Constraint::SubsetOfFirst => Some(json!({
            "EncryptionContextSubset": {ARN_CONTEXT_ENTRY: object_arn(first)},
        })),
        Constraint::SubsetOfSecond => Some(json!({
            "EncryptionContextSubset": {ARN_CONTEXT_ENTRY: object_arn(second)},
        })),
        Constraint::EqualsFirst => Some(json!({
            "EncryptionContextEquals": {ARN_CONTEXT_ENTRY: object_arn(first)},
        })),
    }
}

/// Assert one read answered what the row expects of it: the stored bytes with the encryption the
/// write recorded, or S3's `AccessDenied` carrying KMS's sentence for a key whose policy
/// delegates to the account, and no byte of the object.
fn assert_read(
    case: &str,
    object_key: &str,
    read: &Answer,
    served: bool,
    session: &Session,
    key_arn: &str,
    reported_key_id: &str,
) {
    if served {
        assert_eq!(
            read.status,
            200,
            "case {case:?}: the read of {object_key:?} must be served, got {} with body {:?}",
            read.status,
            read.text()
        );
        assert_eq!(
            read.body.as_ref(),
            BODY,
            "case {case:?}: expected the stored bytes of {object_key:?}, got {:?}",
            read.text()
        );
        assert_eq!(
            read.header(SSE_HEADER).as_deref(),
            Some(SSE_KMS),
            "case {case:?}: expected {object_key:?} to report {SSE_KMS:?}, got {:?}",
            read.header(SSE_HEADER)
        );
        assert_eq!(
            read.header(SSE_KEY_ID_HEADER).as_deref(),
            Some(reported_key_id),
            "case {case:?}: expected {object_key:?} to report the key its write did \
             ({reported_key_id:?}), got {:?}",
            read.header(SSE_KEY_ID_HEADER)
        );
        return;
    }
    assert_eq!(
        read.status, ACCESS_DENIED_STATUS,
        "case {case:?}: the read of {object_key:?} must be refused with HTTP \
         {ACCESS_DENIED_STATUS}, got {} with body {:?}",
        read.status,
        read.text()
    );
    assert_eq!(
        read.error_code(),
        ACCESS_DENIED,
        "case {case:?}: expected the {ACCESS_DENIED:?} code for {object_key:?}, got {:?}",
        read.text()
    );
    assert_eq!(
        read.error_message(),
        no_identity_policy(&session.session_arn, key_arn),
        "case {case:?}: expected KMS's denial sentence for {object_key:?}, got {:?}",
        read.error_message()
    );
    assert!(
        !read.body.as_ref().windows(BODY.len()).any(|at| at == BODY),
        "case {case:?}: a refused read of {object_key:?} must carry none of the object's bytes, \
         got {:?}",
        read.text()
    );
}

#[tokio::test]
async fn an_sse_kms_read_is_served_exactly_where_a_grant_on_its_key_reaches_the_decrypt() {
    let emulator = Emulator::new();
    emulator.create_bucket().await;
    // Both roles carry the S3 permission the read itself needs and no KMS action at all, so what
    // decides the decrypt is the key's own authorities.
    let reader = emulator
        .assumed_role("grant-reader", &identity_policy(json!("s3:GetObject")))
        .await;
    let other = emulator
        .assumed_role("grant-bystander", &identity_policy(json!("s3:GetObject")))
        .await;

    for row in ROWS {
        let case = row.case;
        let first = format!("{}/first", row.prefix);
        let second = format!("{}/second", row.prefix);
        // One key per row, so no row is decided by a grant another row issued.
        let (key_id, key_arn) = emulator.key(case, &delegating_policy()).await;
        let reported = emulator.put_object(case, &first, &key_id).await;
        let reported_second = emulator.put_object(case, &second, &key_id).await;
        assert_eq!(
            reported, reported_second,
            "case {case:?}: both objects are stored under one key, got {reported:?} and \
             {reported_second:?}"
        );

        let grantee = match row.grantee {
            Grantee::Reader => &reader,
            Grantee::Other => &other,
        };
        let grant_id = if row.operations.is_empty() {
            None
        } else {
            Some(
                emulator
                    .create_grant(
                        case,
                        &key_id,
                        &grantee.role_arn,
                        row.operations,
                        constraint_document(row.constraint, &first, &second),
                    )
                    .await,
            )
        };

        let read_first = emulator.get_object(&reader.access_key_id, &first).await;
        assert_read(
            case,
            &first,
            &read_first,
            row.first_served,
            &reader,
            &key_arn,
            &reported,
        );
        let read_second = emulator.get_object(&reader.access_key_id, &second).await;
        assert_read(
            case,
            &second,
            &read_second,
            row.second_served,
            &reader,
            &key_arn,
            &reported,
        );

        // A grant is held until it is revoked, and revoking it "terminates the permissions that
        // the grant allows": the very next read of either object is refused, whichever of them
        // the grant reached.
        if let Some(grant_id) = grant_id {
            emulator.revoke_grant(case, &key_id, &grant_id).await;
            for object_key in [&first, &second] {
                let read = emulator.get_object(&reader.access_key_id, object_key).await;
                assert_read(
                    case,
                    object_key,
                    &read,
                    false,
                    &reader,
                    &key_arn,
                    &reported,
                );
            }
        }
    }
}

#[tokio::test]
async fn a_key_policy_deny_refuses_the_read_the_grant_reaches_until_the_deny_is_removed() {
    // The other half of the rule, over ONE key: an explicit deny in the key's own document
    // overrides the grant that would otherwise open the object, it reaches the principal it names
    // and no other, and it lifts the moment the document drops it.
    let case = "an explicit deny of the reading role's decrypt beside a grant of it";
    let emulator = Emulator::new();
    emulator.create_bucket().await;
    let reader = emulator
        .assumed_role("denied-reader", &identity_policy(json!("s3:GetObject")))
        .await;
    let other = emulator
        .assumed_role("undenied-reader", &identity_policy(json!("s3:GetObject")))
        .await;
    let (key_id, key_arn) = emulator
        .key(
            case,
            &policy(vec![
                allow_account(),
                deny(&reader.role_arn, DECRYPT_ACTION),
            ]),
        )
        .await;
    let object_key = "denied-by-the-key-policy";
    let reported = emulator.put_object(case, object_key, &key_id).await;
    // Both roles hold the same grant of the decrypt on the same key, so the only difference
    // between the two readers is the statement that names one of them.
    for session in [&reader, &other] {
        emulator
            .create_grant(
                case,
                &key_id,
                &session.role_arn,
                &[DECRYPT_OPERATION],
                None,
            )
            .await;
    }

    let refused = emulator
        .get_object(&reader.access_key_id, object_key)
        .await;
    assert_eq!(
        refused.status, ACCESS_DENIED_STATUS,
        "case {case:?}: the denied role's read must be refused with HTTP \
         {ACCESS_DENIED_STATUS}, got {} with body {:?}",
        refused.status,
        refused.text()
    );
    assert_eq!(
        refused.error_code(),
        ACCESS_DENIED,
        "case {case:?}: expected the {ACCESS_DENIED:?} code, got {:?}",
        refused.text()
    );
    assert_eq!(
        refused.error_message(),
        explicit_deny(&reader.session_arn, &key_arn),
        "case {case:?}: expected KMS's sentence for an explicit deny, got {:?}",
        refused.error_message()
    );
    assert!(
        !refused.body.as_ref().windows(BODY.len()).any(|at| at == BODY),
        "case {case:?}: a refused read must carry none of the object's bytes, got {:?}",
        refused.text()
    );

    // The statement names one role, so the other role's session reads the same object under the
    // same document and from the same key.
    let served = emulator.get_object(&other.access_key_id, object_key).await;
    assert_read(
        case,
        object_key,
        &served,
        true,
        &other,
        &key_arn,
        &reported,
    );

    // The one call that changes the decision — and the very next read is served, so no answer
    // here is cached.
    emulator
        .put_key_policy(case, &key_id, &policy(vec![allow_account()]))
        .await;
    let lifted = emulator
        .get_object(&reader.access_key_id, object_key)
        .await;
    assert_read(
        case,
        object_key,
        &lifted,
        true,
        &reader,
        &key_arn,
        &reported,
    );
}
