// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of the IAM AUTHORIZATION `GetTableEncryption`
//! takes.
//!
//! RULE: an operation of this service that documents a permission ("You must
//! have the s3tables:GetTableEncryption permission to use this operation") and
//! models `AccessDeniedException` is decided against the CALLING PRINCIPAL's
//! identity-based IAM policies before this service answers anything — the
//! principal being the identity the access key id the request presents speaks
//! for, and for an `sts:AssumeRole` credential that is the session, whose
//! permissions are the role's. The decision is IAM's own: an explicit `Deny`
//! about the request wins over every `Allow`, an `Allow` with no `Deny` permits,
//! and a principal with neither is refused. The resource decided against is the
//! one the operation acts on, which for a table's configuration is the TABLE,
//! under its bucket's ARN and by the id it was minted with. A refusal carries
//! the modelled `AccessDeniedException` (HTTP 403) naming the principal, the
//! action `s3tables:<OperationName>`, that resource and the rule that withheld
//! the grant, and it reports nothing of the table.
//!
//! The rows span what the rule generalises over: whether the principal holds no
//! policy / a broad grant / a grant scoped to this table's ARN / a grant scoped
//! to ANOTHER table's ARN / a grant plus an explicit deny, and — beside the
//! table — that the account's own published credential keeps being served and
//! that a refusal precedes the lookup, so a caller holding no grant is refused
//! for a table the account does not even hold.
//!
//! The principals are made with IAM's and STS's OWN operations over the same
//! engine, so the documents this service reads are the ones those services
//! wrote, and every call drives this service the way a client does.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::harness::{encode, Reply};

/// The IAM action the operation under test is performed under.
const ACTION: &str = "s3tables:GetTableEncryption";

/// The modelled error code of an authorization refusal.
const ACCESS_DENIED: &str = "AccessDeniedException";

/// The region every call here is signed for.
pub(crate) const REGION: &str = "us-west-2";

/// The namespace the fixture tables live in.
const NAMESPACE: &str = "auth_ns";

/// The `RoleSessionName` every `AssumeRole` here asks for — the last segment of
/// the assumed-role ARN a refusal names as the principal.
pub(crate) const SESSION: &str = "probe";

/// The access-key id standing for the account's own published credential:
/// neither STS nor IAM holds a record for it, so it resolves to the account's
/// ambient identity, which holds no IAM policies — the position AWS account root
/// credentials are in. Every fixture is made with it.
pub(crate) const ACCOUNT_KEY: &str = "AKIDEXAMPLE";

/// A principal a call is signed as: the access key id it presents, and the ARN a
/// refusal must blame it by — what `sts:GetCallerIdentity` reports for that key.
pub(crate) struct Principal {
    pub(crate) access_key_id: String,
    pub(crate) arn: String,
}

/// This service beside the IAM and STS services its callers are made with, all
/// on ONE shared engine.
///
/// `super::harness::Emulator` mounts this service alone and signs every call
/// with one key, which is right for a suite about what this service stores; a
/// suite about WHO is calling needs the two services that hold principals and
/// their policies, and a credential scope naming a different key per call. It is
/// shared with the other suites about who is calling
/// (`table_bucket_encryption_authorization.rs`) rather than copied.
pub(crate) struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    pub(crate) fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // Nothing here signs: the `Authorization` header carries only the
        // credential scope, which is where a service reads the caller's access
        // key id and Region from.
        router.set_request_authentication(false);
        router.register_service(
            service_com_amazonaws_s3tables_s3tablebuckets::EmulatedService::new(Arc::clone(
                &engine,
            )),
        );
        router.register_service(
            service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        router.register_service(
            service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(engine),
        );
        Self { router }
    }

    /// One restJson1 call to this service, signed for [`REGION`] with
    /// `access_key_id` in the credential scope.
    pub(crate) async fn call_as(
        &self,
        access_key_id: &str,
        method: &str,
        path: &str,
        body: Option<&Value>,
    ) -> Reply {
        let payload = body.map_or_else(Vec::new, |value| {
            serde_json::to_vec(value).expect("a request body serializes")
        });
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://s3tables.{REGION}.amazonaws.com{path}"))
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/{REGION}/s3tables/\
                     aws4_request, SignedHeaders=host;x-amz-date, Signature=0000"
                ),
            )
            .header("content-type", "application/json")
            .body(bytes::Bytes::from(payload))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let error_code = response
            .headers()
            .get("x-amzn-errortype")
            .and_then(|value| value.to_str().ok())
            .map(|value| {
                value
                    .rsplit('#')
                    .next()
                    .unwrap_or(value)
                    .split(':')
                    .next()
                    .unwrap_or(value)
                    .to_string()
            });
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

    /// `GetTableEncryption` as `access_key_id`.
    async fn get_table_encryption_as(
        &self,
        access_key_id: &str,
        bucket_arn: &str,
        namespace: &str,
        table: &str,
    ) -> Reply {
        self.call_as(
            access_key_id,
            "GET",
            &format!(
                "/tables/{}/{}/{}/encryption",
                encode(bucket_arn),
                encode(namespace),
                encode(table)
            ),
            None,
        )
        .await
    }

    /// A call signed with the account's own credential that must succeed.
    pub(crate) async fn ok(&self, method: &str, path: &str, body: Option<&Value>) -> Reply {
        let reply = self.call_as(ACCOUNT_KEY, method, path, body).await;
        assert!(
            (200..300).contains(&reply.status),
            "{method} {path}: the setup call must succeed, got {status} {payload}",
            status = reply.status,
            payload = reply.body
        );
        reply
    }

    /// One awsQuery call to IAM or STS, signed with the account's own
    /// credential, which must succeed.
    pub(crate) async fn query(
        &self,
        signing_name: &str,
        version: &str,
        action: &str,
        params: &[(&str, String)],
    ) -> String {
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
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential={ACCOUNT_KEY}/20260101/{REGION}/{signing_name}/\
                     aws4_request, SignedHeaders=host, Signature=0000"
                ),
            )
            .body(bytes::Bytes::from(body))
            .expect("a well-formed awsQuery request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let text = String::from_utf8_lossy(response.body()).to_string();
        assert_eq!(
            status, 200,
            "case {action:?}: the setup call must succeed, got {status} with {text}"
        );
        text
    }
}

/// Percent-encode one `application/x-www-form-urlencoded` component, leaving
/// only RFC 3986's unreserved characters literal — so a policy document's braces
/// and quotes survive the round trip.
fn form_encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

/// The text of the first `<name>…</name>` element of an awsQuery response.
fn xml_element(body: &str, name: &str) -> Option<String> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let start = body.find(&open)? + open.len();
    let end = body[start..].find(&close)? + start;
    Some(body[start..end].to_string())
}

/// One identity-based policy document carrying one statement.
pub(crate) fn statement(effect: &str, action: &str, resource: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{"Effect": effect, "Action": action, "Resource": resource}],
    })
    .to_string()
}

/// Create an IAM role carrying the trust policy `trust` and one inline policy
/// per document in `policies`, and answer the ARN IAM minted for it.
///
/// The CreateRole-plus-PutRolePolicy half every suite that needs a role of its
/// own performs, held here once: [`assumed_role`] goes on to assume the role it
/// makes, while a suite about a role a SERVICE acts as
/// (`table_replication_role_backfill.rs`) only needs the role to exist with the
/// documents it carries.
pub(crate) async fn created_role(
    aws: &Emulator,
    role_name: &str,
    trust: &str,
    policies: &[String],
) -> String {
    let created = aws
        .query(
            "iam",
            "2010-05-08",
            "CreateRole",
            &[
                ("RoleName", role_name.to_string()),
                ("AssumeRolePolicyDocument", trust.to_string()),
            ],
        )
        .await;
    // SEVERAL inline policies, so a decision taken over the union of the
    // documents in force is distinguishable from one taken inside a single
    // document.
    for (index, policy) in policies.iter().enumerate() {
        aws.query(
            "iam",
            "2010-05-08",
            "PutRolePolicy",
            &[
                ("RoleName", role_name.to_string()),
                ("PolicyName", format!("inline-{index}")),
                ("PolicyDocument", policy.clone()),
            ],
        )
        .await;
    }
    xml_element(&created, "Arn")
        .unwrap_or_else(|| panic!("CreateRole answers the role's Arn, got {created}"))
}

/// Create an IAM role carrying one inline policy per document in `policies`,
/// assume it through STS, and answer the session's access key id with the
/// session ARN.
pub(crate) async fn assumed_role(
    aws: &Emulator,
    role_name: &str,
    policies: &[String],
) -> Principal {
    let account = engine::emulator_account_id();
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"AWS": format!("arn:aws:iam::{account}:root")},
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    let role_arn = created_role(aws, role_name, &trust, policies).await;
    let assumed = aws
        .query(
            "sts",
            "2011-06-15",
            "AssumeRole",
            &[
                ("RoleArn", role_arn),
                ("RoleSessionName", SESSION.to_string()),
            ],
        )
        .await;
    Principal {
        access_key_id: xml_element(&assumed, "AccessKeyId")
            .unwrap_or_else(|| panic!("AssumeRole answers an AccessKeyId, got {assumed}")),
        arn: xml_element(&assumed, "Arn")
            .unwrap_or_else(|| panic!("AssumeRole answers the session's Arn, got {assumed}")),
    }
}

/// A table bucket holding one namespace with the tables `tables` names,
/// answering `(bucket ARN, the ARN of each table in order)`.
async fn fixture(aws: &Emulator, bucket: &str, tables: &[&str]) -> (String, Vec<String>) {
    let created = aws
        .ok("PUT", "/buckets", Some(&json!({ "name": bucket })))
        .await;
    let arn = created.text("arn");
    aws.ok(
        "PUT",
        &format!("/namespaces/{}", encode(&arn)),
        Some(&json!({ "namespace": [NAMESPACE] })),
    )
    .await;
    let mut arns = Vec::new();
    for table in tables {
        let made = aws
            .ok(
                "PUT",
                &format!("/tables/{}/{}", encode(&arn), encode(NAMESPACE)),
                Some(&json!({ "name": table, "format": "ICEBERG" })),
            )
            .await;
        arns.push(made.text("tableARN"));
    }
    (arn, arns)
}

/// The sentence IAM refuses a call with, stated here independently of the
/// service under test: who was refused, which action, on which resource and
/// which of IAM's rules withheld the grant.
fn refusal_sentence(principal: &str, resource: &str, reason: &str) -> String {
    format!("User: {principal} is not authorized to perform: {ACTION} on resource: {resource} {reason}")
}

#[tokio::test]
async fn a_read_is_decided_against_the_callers_identity_based_policies() {
    let aws = Emulator::new();
    let (bucket_arn, table_arns) = fixture(&aws, "s3t-auth-table", &["subject", "other"]).await;
    let subject = table_arns[0].clone();
    let other = table_arns[1].clone();

    // (case, the inline policies the role carries, the refusal's reason clause
    // when the call must be refused)
    let rows: Vec<(&str, Vec<String>, Option<String>)> = vec![
        (
            "a role carrying no policy at all",
            Vec::new(),
            Some(format!(
                "because no identity-based policy allows the {ACTION} action"
            )),
        ),
        (
            "a role granted every action of this service on every resource",
            vec![statement("Allow", "s3tables:*", "*")],
            None,
        ),
        (
            "a role granted this action on this table's ARN",
            vec![statement("Allow", ACTION, &subject)],
            None,
        ),
        (
            "a role granted this action on ANOTHER table's ARN",
            vec![statement("Allow", ACTION, &other)],
            Some(format!(
                "because no identity-based policy allows the {ACTION} action"
            )),
        ),
        (
            "a role granted this action on the bucket holding the table",
            vec![statement("Allow", ACTION, &bucket_arn)],
            Some(format!(
                "because no identity-based policy allows the {ACTION} action"
            )),
        ),
        (
            "a role granted another action of this service on this table",
            vec![statement("Allow", "s3tables:GetTableStorageClass", &subject)],
            Some(format!(
                "because no identity-based policy allows the {ACTION} action"
            )),
        ),
        (
            "a role granted everything and denied this action",
            vec![
                statement("Allow", "s3tables:*", "*"),
                statement("Deny", ACTION, &subject),
            ],
            Some("with an explicit deny in an identity-based policy".to_string()),
        ),
    ];

    for (index, (case, policies, reason)) in rows.iter().enumerate() {
        let principal = assumed_role(&aws, &format!("s3t-auth-role-{index}"), policies).await;
        let read = aws
            .get_table_encryption_as(&principal.access_key_id, &bucket_arn, NAMESPACE, "subject")
            .await;
        match reason {
            None => {
                assert_eq!(
                    read.status, 200,
                    "case {case:?}: an allowed read must be served, got {status} {body}",
                    status = read.status,
                    body = read.body
                );
                assert_eq!(
                    read.body
                        .get("encryptionConfiguration")
                        .and_then(|configuration| configuration.get("sseAlgorithm"))
                        .and_then(Value::as_str),
                    Some("AES256"),
                    "case {case:?}: an allowed read reports the table's configuration, got {body}",
                    body = read.body
                );
            }
            Some(reason) => {
                assert_eq!(
                    read.status, 403,
                    "case {case:?}: a refused read must answer 403, got {status} {body}",
                    status = read.status,
                    body = read.body
                );
                assert_eq!(
                    read.error_code.as_deref(),
                    Some(ACCESS_DENIED),
                    "case {case:?}: expected {ACCESS_DENIED}, got {actual:?}",
                    actual = read.error_code
                );
                assert_eq!(
                    read.message(),
                    refusal_sentence(&principal.arn, &subject, reason),
                    "case {case:?}: expected IAM's sentence, got {actual:?}",
                    actual = read.message()
                );
                assert!(
                    read.body.get("encryptionConfiguration").is_none(),
                    "case {case:?}: a refusal reports nothing of the table, got {body}",
                    body = read.body
                );
            }
        }
    }

    // The account's own published credential holds no IAM policies — the
    // position root credentials are in — so it keeps being served.
    let served = aws
        .get_table_encryption_as(ACCOUNT_KEY, &bucket_arn, NAMESPACE, "subject")
        .await;
    assert_eq!(
        served.status, 200,
        "case \"the account's own credential\": the read must be served, got {status} {body}",
        status = served.status,
        body = served.body
    );
}

#[tokio::test]
async fn a_refusal_precedes_the_lookup_and_changes_nothing() {
    let aws = Emulator::new();
    let (bucket_arn, _) = fixture(&aws, "s3t-auth-absent", &["resident"]).await;
    let principal = assumed_role(&aws, "s3t-auth-nothing", &[]).await;

    // A caller holding no grant is refused for a table the account does not
    // hold, with the same class and sentence as for one it does: the decision is
    // taken before the answer, so the refusal discloses no existence.
    let absent = aws
        .get_table_encryption_as(
            &principal.access_key_id,
            &bucket_arn,
            NAMESPACE,
            "never_created",
        )
        .await;
    assert_eq!(
        absent.status, 403,
        "case \"a table that does not exist\": expected 403, got {status} {body}",
        status = absent.status,
        body = absent.body
    );
    assert_eq!(
        absent.error_code.as_deref(),
        Some(ACCESS_DENIED),
        "case \"a table that does not exist\": expected {ACCESS_DENIED}, got {actual:?}",
        actual = absent.error_code
    );

    // And the refused reads left the account exactly as it was: the owner still
    // reads the resident table's configuration.
    let owner = aws
        .get_table_encryption_as(ACCOUNT_KEY, &bucket_arn, NAMESPACE, "resident")
        .await;
    assert_eq!(
        owner.status, 200,
        "case \"the owner's own read\": expected 200, got {status} {body}",
        status = owner.status,
        body = owner.body
    );
}
