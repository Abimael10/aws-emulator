// SPDX-License-Identifier: CC0-1.0

//! WHO may reach this service's control plane, driven end to end over the real wire path (a
//! restJson1 request through the multi-service router into the mounted service), so what is
//! asserted is what a client holding those credentials sees.
//!
//! THE RULE UNDER TEST: every operation whose model declares `AccessDeniedException`
//! evaluates the identity-based IAM policies in force on the principal its credentials name
//! — against `rolesanywhere:<OperationName>` on the ARN the request addresses — BEFORE it
//! reads or writes anything. A request no statement allows (IAM's default deny), or one an
//! explicit `Deny` covers, is answered HTTP 403 `AccessDeniedException` naming the principal,
//! the action, that resource and which of IAM's rules refused it, and it performs nothing;
//! a request a matching `Allow` covers reaches the operation's own answer.
//!
//! The user, the role, their policies, the access keys and the session all live in the IAM
//! and token-service stores of the SAME shared engine, and every one of them is created
//! through those services' OWN wire path (`iam:CreateUser`, `iam:PutUserPolicy`,
//! `iam:CreateAccessKey`, `iam:CreateRole`, `iam:PutRolePolicy`, `sts:AssumeRole`) — so the
//! documents under test are the ones those operations store, and the principal a refusal
//! blames is the one `sts:GetCallerIdentity` answers for the key the request signed with.
//!
//! The tables span what the rule generalises over: the operation (all 27 of this model's
//! operations that declare the refusal, which is every one of them but the three still
//! scaffolded), the resource each one addresses (a profile, a trust anchor, a certificate
//! revocation list and a subject by id, an ARN the request itself names, and the collection
//! a create or a listing works over), the document in force (none about this service, an
//! allow of exactly the action, of the whole namespace, of another service's actions, an
//! allow scoped to another resource, and an explicit deny beside an allow) and the principal
//! (an IAM user's own long-term key, an assumed-role session, and the account's own
//! published credential).

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every call below is signed for, and so the Region of every ARN they are judged
/// against.
const REGION: &str = "us-west-2";

/// A PEM bundle holding one real certificate authority: a self-signed CA certificate,
/// valid 2020-2100, written with `rcgen` and kept beside the suites that need one.
/// `CreateTrustAnchor` refuses certificate data it cannot read as PEM X.509 certificates,
/// so the allowed leg of the gate below presents an authority a trust anchor can hold.
const CERTIFICATE_BUNDLE_PEM: &str = include_str!("../data/certificate_authority_a.pem");

/// The action namespace this service's operations are published under, which is its
/// `arnNamespace` and its `sigv4Name` alike (`smithy_query(query="service_info")`).
const ACTION_PREFIX: &str = "rolesanywhere";

/// The date stamp every request is signed with: the credential scope is what carries the
/// Region and the access key id.
const SIGNED_DATE: &str = "20260101";

/// An access key id neither the IAM store nor the session registry holds a record for: the
/// position the long-term credential this emulator publishes at start is in, which is the
/// position AWS account root credentials are in.
const ACCOUNT_KEY: &str = "AKIDNOSTOREHOLDSTHIS";

/// Identifiers inside the modelled `Uuid` (`@length(36, 36)`,
/// `@pattern("[a-f0-9]{8}-([a-z0-9]{4}-){3}[a-z0-9]{12}")`) that nothing ever mints, so a row
/// a policy ALLOWS meets the operation's own not-found answer rather than a second
/// authorization decision.
const UNMINTED_PROFILE: &str = "11111111-1111-1111-1111-111111111111";
const UNMINTED_TRUST_ANCHOR: &str = "22222222-2222-2222-2222-222222222222";
const UNMINTED_CRL: &str = "33333333-3333-3333-3333-333333333333";
const UNMINTED_SUBJECT: &str = "44444444-4444-4444-4444-444444444444";

/// A value of the modelled `CertificateField` enum
/// (`smithy_query(query="shape", shape="CertificateField")`).
const CERTIFICATE_FIELD: &str = "x509Subject";

/// A value of the modelled `NotificationEvent` enum
/// (`smithy_query(query="shape", shape="NotificationEvent")`).
const NOTIFICATION_EVENT: &str = "CA_CERTIFICATE_EXPIRY";

/// What one call answered: its HTTP status, the wire error code and the parsed body.
#[derive(Debug)]
struct Reply {
    status: u16,
    error_code: String,
    body: Value,
}

impl Reply {
    /// The answer's `message`, or the empty string when it carries none.
    fn message(&self) -> String {
        self.body
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }
}

/// A principal a row signs as: the key its requests carry, and the ARN a refusal must name.
#[derive(Debug, Clone)]
struct Principal {
    access_key_id: String,
    arn: String,
}

/// This service mounted beside the IAM and token services over ONE shared engine, so the
/// policies and the sessions the cases create are the ones those services hold.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // Signatures are not what these cases are about; the credential scope is still read
        // for the routing key, the ARN Region and the access key id the principal is
        // resolved from.
        router.set_request_authentication(false);
        router.register_service(
            service_com_amazonaws_rolesanywhere_rolesanywhere::EmulatedService::new(Arc::clone(
                &engine,
            )),
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

    /// One call to this service, signed (in the routing sense) with `access_key_id`.
    async fn call(
        &self,
        access_key_id: &str,
        method: &str,
        path: &str,
        body: Option<&Value>,
    ) -> Reply {
        let payload = body.map_or_else(Vec::new, |value| value.to_string().into_bytes());
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://{ACTION_PREFIX}.{REGION}.amazonaws.com{path}"))
            .header("host", format!("{ACTION_PREFIX}.{REGION}.amazonaws.com"))
            .header("content-type", "application/json")
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 \
                     Credential={access_key_id}/{SIGNED_DATE}/{REGION}/{ACTION_PREFIX}/\
                     aws4_request, SignedHeaders=host, Signature=0"
                ),
            )
            .body(bytes::Bytes::from(payload))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let header_code = response
            .headers()
            .get("x-amzn-errortype")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .rsplit(['#', '/'])
            .next()
            .unwrap_or_default()
            .to_string();
        let bytes = response.into_body();
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        let error_code = if header_code.is_empty() {
            body.get("__type")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .rsplit(['#', '/'])
                .next()
                .unwrap_or_default()
                .to_string()
        } else {
            header_code
        };
        Reply {
            status,
            error_code,
            body,
        }
    }

    /// One awsQuery call to IAM or the token service, signed with the account's own
    /// credential, which the setup uses because it is the credential that holds no policies
    /// at all.
    async fn query(
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
            .header("host", format!("{signing_name}.amazonaws.com"))
            .header("content-type", "application/x-www-form-urlencoded")
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential={ACCOUNT_KEY}/{SIGNED_DATE}/{REGION}/\
                     {signing_name}/aws4_request, SignedHeaders=host, Signature=0"
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

    /// Create an IAM user carrying `policy` as its inline policy (when one is given), mint a
    /// long-term access key for it, and answer that key with the user's own ARN.
    async fn user(&self, user_name: &str, policy: Option<&str>) -> Principal {
        let created = self
            .query(
                "iam",
                "2010-05-08",
                "CreateUser",
                &[("UserName", user_name.to_string())],
            )
            .await;
        let arn = xml_element(&created, "Arn")
            .unwrap_or_else(|| panic!("CreateUser answers the user's Arn, got {created}"));
        if let Some(policy) = policy {
            self.put_user_policy(user_name, policy).await;
        }
        let minted = self
            .query(
                "iam",
                "2010-05-08",
                "CreateAccessKey",
                &[("UserName", user_name.to_string())],
            )
            .await;
        Principal {
            access_key_id: xml_element(&minted, "AccessKeyId")
                .unwrap_or_else(|| panic!("CreateAccessKey answers an AccessKeyId, got {minted}")),
            arn,
        }
    }

    /// Put `policy` on the user as its inline policy, replacing whatever it carried — what
    /// `iam:PutUserPolicy` does, so a document written here is in force on the NEXT request.
    async fn put_user_policy(&self, user_name: &str, policy: &str) {
        self.query(
            "iam",
            "2010-05-08",
            "PutUserPolicy",
            &[
                ("UserName", user_name.to_string()),
                ("PolicyName", "inline".to_string()),
                ("PolicyDocument", policy.to_string()),
            ],
        )
        .await;
    }

    /// Create an IAM role carrying `policy` as its inline policy, assume it through the token
    /// service, and answer the session's access key with the session ARN.
    async fn assumed_role(&self, role_name: &str, policy: &str) -> Principal {
        let account = ::engine::emulator_account_id();
        let trust = json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Effect": "Allow",
                "Principal": {"AWS": format!("arn:aws:iam::{account}:root")},
                "Action": "sts:AssumeRole",
            }],
        })
        .to_string();
        self.query(
            "iam",
            "2010-05-08",
            "CreateRole",
            &[
                ("RoleName", role_name.to_string()),
                ("AssumeRolePolicyDocument", trust),
            ],
        )
        .await;
        self.query(
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
        let assumed = self
            .query(
                "sts",
                "2011-06-15",
                "AssumeRole",
                &[
                    (
                        "RoleArn",
                        format!("arn:aws:iam::{account}:role/{role_name}"),
                    ),
                    ("RoleSessionName", "probe".to_string()),
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
}

/// Percent-encode one `application/x-www-form-urlencoded` component, leaving only RFC 3986's
/// unreserved characters literal — so a policy document's braces and quotes survive the round
/// trip.
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

/// A policy document allowing `actions` on `resources`.
fn allow(actions: Value, resources: Value) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{"Effect": "Allow", "Action": actions, "Resource": resources}],
    })
    .to_string()
}

/// A policy document allowing every action of this service and explicitly denying `denied` —
/// the shape of a break-glass deny laid over a working grant.
fn allow_all_but(denied: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [
            {"Effect": "Allow", "Action": format!("{ACTION_PREFIX}:*"), "Resource": "*"},
            {"Effect": "Deny", "Action": [denied], "Resource": "*"},
        ],
    })
    .to_string()
}

/// This service's ARN for `resource_path`, in the Region every call is signed for.
fn arn_of(resource_path: &str) -> String {
    format!(
        "arn:aws:{ACTION_PREFIX}:{REGION}:{}:{resource_path}",
        ::engine::emulator_account_id()
    )
}

/// The sentence IAM refuses an unauthorized request with: the principal that signed, the
/// action it asked for, the resource it addressed and which of IAM's rules withheld the
/// grant.
fn not_authorized(principal: &str, action: &str, resource: &str, reason: &str) -> String {
    format!("User: {principal} is not authorized to perform: {action} on resource: {resource} {reason}")
}

/// The clause IAM closes a refusal with when an explicit `Deny` covered the request.
const EXPLICIT_DENY: &str = "with an explicit deny in an identity-based policy";

/// The clause IAM closes a refusal with when no readable document allowed the request.
fn default_deny(action: &str) -> String {
    format!("because no identity-based policy allows the {action} action")
}

/// One request a decision is taken about: the call itself, the operation and the resource IAM
/// judges it by, and the answer an AUTHORIZED caller gets — the operation's own, which is how
/// a served call is told from a refused one.
struct Gated {
    operation: &'static str,
    method: &'static str,
    path: String,
    body: Option<Value>,
    /// The resource field of the ARN this request is judged against.
    resource: String,
    /// `(status, error code)` an authorized caller meets. Every row that addresses an
    /// identifier nothing minted meets that operation's own not-found refusal, which is the
    /// proof the call got PAST authorization.
    served: (u16, &'static str),
}

/// A `crlData` the modelled `Blob` `@length(1, 300000)` admits, carried as the base64 the
/// restJson1 codec reads a blob from.
fn crl_data() -> Value {
    json!(aws_smithy_types::base64::encode(
        b"-----BEGIN X509 CRL-----\nMA==\n-----END X509 CRL-----\n".as_slice()
    ))
}

/// Every operation of this model that declares `AccessDeniedException` and is implemented:
/// the whole gated surface rather than a sample of it, each with a request of its own.
///
/// The three operations left out are the ones still scaffolded (`EnableCrl`, `DisableCrl`,
/// `ListSubjects`), which answer 501 and perform nothing.
fn gated_operations() -> Vec<Gated> {
    let profile = format!("profile/{UNMINTED_PROFILE}");
    let trust_anchor = format!("trust-anchor/{UNMINTED_TRUST_ANCHOR}");
    let crl = format!("crl/{UNMINTED_CRL}");
    let subject = format!("subject/{UNMINTED_SUBJECT}");
    let not_found = (404, "ResourceNotFoundException");
    vec![
        Gated {
            operation: "CreateProfile",
            method: "POST",
            path: "/profiles".to_string(),
            body: Some(json!({
                "name": "authorized",
                "roleArns": [format!("arn:aws:iam::{}:role/Vended", ::engine::emulator_account_id())],
            })),
            // A create addresses a profile whose id does not exist yet, so the decision is
            // taken against every profile of the account.
            resource: arn_of("profile/*"),
            served: (201, ""),
        },
        Gated {
            operation: "CreateTrustAnchor",
            method: "POST",
            path: "/trustanchors".to_string(),
            body: Some(json!({
                "name": "authorized",
                "source": {
                    "sourceType": "CERTIFICATE_BUNDLE",
                    "sourceData": {"x509CertificateData": CERTIFICATE_BUNDLE_PEM},
                },
            })),
            resource: arn_of("trust-anchor/*"),
            served: (201, ""),
        },
        Gated {
            operation: "ImportCrl",
            method: "POST",
            path: "/crls".to_string(),
            body: Some(json!({
                "name": "authorized",
                "crlData": crl_data(),
                "trustAnchorArn": arn_of(&trust_anchor),
            })),
            resource: arn_of("crl/*"),
            served: not_found,
        },
        Gated {
            operation: "ListProfiles",
            method: "GET",
            path: "/profiles".to_string(),
            body: None,
            resource: arn_of("profile/*"),
            served: (200, ""),
        },
        Gated {
            operation: "ListTrustAnchors",
            method: "GET",
            path: "/trustanchors".to_string(),
            body: None,
            resource: arn_of("trust-anchor/*"),
            served: (200, ""),
        },
        Gated {
            operation: "ListCrls",
            method: "GET",
            path: "/crls".to_string(),
            body: None,
            resource: arn_of("crl/*"),
            served: (200, ""),
        },
        Gated {
            operation: "GetProfile",
            method: "GET",
            path: format!("/profile/{UNMINTED_PROFILE}"),
            body: None,
            resource: arn_of(&profile),
            served: not_found,
        },
        Gated {
            operation: "DeleteProfile",
            method: "DELETE",
            path: format!("/profile/{UNMINTED_PROFILE}"),
            body: None,
            resource: arn_of(&profile),
            served: not_found,
        },
        Gated {
            operation: "EnableProfile",
            method: "POST",
            path: format!("/profile/{UNMINTED_PROFILE}/enable"),
            body: None,
            resource: arn_of(&profile),
            served: not_found,
        },
        Gated {
            operation: "DisableProfile",
            method: "POST",
            path: format!("/profile/{UNMINTED_PROFILE}/disable"),
            body: None,
            resource: arn_of(&profile),
            served: not_found,
        },
        Gated {
            operation: "UpdateProfile",
            method: "PATCH",
            path: format!("/profile/{UNMINTED_PROFILE}"),
            body: Some(json!({"name": "renamed"})),
            resource: arn_of(&profile),
            served: not_found,
        },
        Gated {
            operation: "PutAttributeMapping",
            method: "PUT",
            path: format!("/profiles/{UNMINTED_PROFILE}/mappings"),
            body: Some(json!({
                "certificateField": CERTIFICATE_FIELD,
                "mappingRules": [{"specifier": "CN"}],
            })),
            resource: arn_of(&profile),
            served: not_found,
        },
        Gated {
            operation: "DeleteAttributeMapping",
            method: "DELETE",
            path: format!("/profiles/{UNMINTED_PROFILE}/mappings?certificateField={CERTIFICATE_FIELD}"),
            body: None,
            resource: arn_of(&profile),
            served: not_found,
        },
        Gated {
            operation: "GetTrustAnchor",
            method: "GET",
            path: format!("/trustanchor/{UNMINTED_TRUST_ANCHOR}"),
            body: None,
            resource: arn_of(&trust_anchor),
            served: not_found,
        },
        Gated {
            operation: "DeleteTrustAnchor",
            method: "DELETE",
            path: format!("/trustanchor/{UNMINTED_TRUST_ANCHOR}"),
            body: None,
            resource: arn_of(&trust_anchor),
            served: not_found,
        },
        Gated {
            operation: "EnableTrustAnchor",
            method: "POST",
            path: format!("/trustanchor/{UNMINTED_TRUST_ANCHOR}/enable"),
            body: None,
            resource: arn_of(&trust_anchor),
            served: not_found,
        },
        Gated {
            operation: "DisableTrustAnchor",
            method: "POST",
            path: format!("/trustanchor/{UNMINTED_TRUST_ANCHOR}/disable"),
            body: None,
            resource: arn_of(&trust_anchor),
            served: not_found,
        },
        Gated {
            operation: "UpdateTrustAnchor",
            method: "PATCH",
            path: format!("/trustanchor/{UNMINTED_TRUST_ANCHOR}"),
            body: Some(json!({"name": "renamed"})),
            resource: arn_of(&trust_anchor),
            served: not_found,
        },
        Gated {
            operation: "PutNotificationSettings",
            method: "PATCH",
            path: "/put-notifications-settings".to_string(),
            body: Some(json!({
                "trustAnchorId": UNMINTED_TRUST_ANCHOR,
                "notificationSettings": [{"enabled": true, "event": NOTIFICATION_EVENT}],
            })),
            resource: arn_of(&trust_anchor),
            served: not_found,
        },
        Gated {
            operation: "ResetNotificationSettings",
            method: "PATCH",
            path: "/reset-notifications-settings".to_string(),
            body: Some(json!({
                "trustAnchorId": UNMINTED_TRUST_ANCHOR,
                "notificationSettingKeys": [{"event": NOTIFICATION_EVENT}],
            })),
            resource: arn_of(&trust_anchor),
            served: not_found,
        },
        Gated {
            operation: "GetCrl",
            method: "GET",
            path: format!("/crl/{UNMINTED_CRL}"),
            body: None,
            resource: arn_of(&crl),
            served: not_found,
        },
        Gated {
            operation: "DeleteCrl",
            method: "DELETE",
            path: format!("/crl/{UNMINTED_CRL}"),
            body: None,
            resource: arn_of(&crl),
            served: not_found,
        },
        Gated {
            operation: "UpdateCrl",
            method: "PATCH",
            path: format!("/crl/{UNMINTED_CRL}"),
            body: Some(json!({"name": "renamed"})),
            resource: arn_of(&crl),
            served: not_found,
        },
        Gated {
            operation: "GetSubject",
            method: "GET",
            path: format!("/subject/{UNMINTED_SUBJECT}"),
            body: None,
            resource: arn_of(&subject),
            served: not_found,
        },
        Gated {
            operation: "ListTagsForResource",
            method: "GET",
            path: format!("/ListTagsForResource?resourceArn={}", arn_of(&profile)),
            body: None,
            // An ARN-addressed call is judged against the ARN the request itself names.
            resource: arn_of(&profile),
            served: not_found,
        },
        Gated {
            operation: "TagResource",
            method: "POST",
            path: "/TagResource".to_string(),
            body: Some(json!({
                "resourceArn": arn_of(&profile),
                "tags": [{"key": "team", "value": "platform"}],
            })),
            resource: arn_of(&profile),
            served: not_found,
        },
        Gated {
            operation: "UntagResource",
            method: "POST",
            path: "/UntagResource".to_string(),
            body: Some(json!({
                "resourceArn": arn_of(&profile),
                "tagKeys": ["team"],
            })),
            resource: arn_of(&profile),
            served: not_found,
        },
    ]
}

/// Assert `reply` is the modelled authorization refusal carrying `expected`.
fn assert_refused(case: &str, reply: &Reply, expected: &str) {
    assert_eq!(
        (reply.status, reply.error_code.as_str()),
        (403, "AccessDeniedException"),
        "case {case:?}: expected 403 AccessDeniedException, got {reply:?}"
    );
    assert_eq!(
        reply.message(),
        expected,
        "case {case:?}: expected the IAM refusal {expected:?}, got {:?}",
        reply.message()
    );
}

/// RULE, per operation: the decision is taken on `rolesanywhere:<OperationName>` and the ARN
/// the request addresses, before anything is read or written — so an explicit `Deny` of just
/// that action refuses it while every other action stays allowed, a document about another
/// service allows none of them, and a grant of this service's actions lets each one reach its
/// own answer.
#[tokio::test]
async fn every_gated_operation_is_decided_on_its_own_action_and_resource() {
    let emulator = Emulator::new();
    // Three principals, each holding one document: a grant of this service, a grant of
    // another service only, and (per row) a grant of this service with that row's action
    // explicitly denied.
    let allowed = emulator
        .user(
            "allowed-caller",
            Some(&allow(json!(format!("{ACTION_PREFIX}:*")), json!("*"))),
        )
        .await;
    let elsewhere = emulator
        .user("other-service-caller", Some(&allow(json!("sts:*"), json!("*"))))
        .await;
    let denied = emulator
        .user("denied-caller", Some(&allow_all_but("rolesanywhere:None")))
        .await;

    for row in gated_operations() {
        let action = format!("{ACTION_PREFIX}:{}", row.operation);

        // 1. A document that states nothing about this service: IAM's default deny.
        let reply = emulator
            .call(
                &elsewhere.access_key_id,
                row.method,
                &row.path,
                row.body.as_ref(),
            )
            .await;
        assert_refused(
            &format!("{}/another-service-only", row.operation),
            &reply,
            &not_authorized(&elsewhere.arn, &action, &row.resource, &default_deny(&action)),
        );

        // 2. A grant of every action of this service, with THIS action explicitly denied:
        // the deny wins, and it is read for this action alone.
        emulator
            .put_user_policy("denied-caller", &allow_all_but(&action))
            .await;
        let reply = emulator
            .call(
                &denied.access_key_id,
                row.method,
                &row.path,
                row.body.as_ref(),
            )
            .await;
        assert_refused(
            &format!("{}/explicit-deny", row.operation),
            &reply,
            &not_authorized(&denied.arn, &action, &row.resource, EXPLICIT_DENY),
        );

        // 3. The same principal with ANOTHER action denied is allowed this one, so the deny
        // above refused the action it named rather than the whole service.
        emulator
            .put_user_policy("denied-caller", &allow_all_but("rolesanywhere:NoSuchAction"))
            .await;
        let reply = emulator
            .call(
                &denied.access_key_id,
                row.method,
                &row.path,
                row.body.as_ref(),
            )
            .await;
        assert_ne!(
            (reply.status, reply.error_code.as_str()),
            (403, "AccessDeniedException"),
            "case {}/another-action-denied: a deny of another action must not refuse this \
             one, got {reply:?}",
            row.operation
        );

        // 4. A grant of this service's actions: the call reaches the operation's own answer.
        let reply = emulator
            .call(
                &allowed.access_key_id,
                row.method,
                &row.path,
                row.body.as_ref(),
            )
            .await;
        assert_eq!(
            (reply.status, reply.error_code.as_str()),
            row.served,
            "case {}/allowed: expected the operation's own answer {:?}, got {reply:?}",
            row.operation,
            row.served
        );
    }
}

/// RULE: the decision is taken against the ARN the request ADDRESSES, so a grant scoped to
/// one resource covers the calls that address it and refuses the calls that address another —
/// and a call that addresses no single resource is covered only by a grant over the
/// collection.
#[tokio::test]
async fn a_grant_is_read_for_the_resource_the_request_addresses() {
    let emulator = Emulator::new();
    // Two profiles the account's own credential creates, so the rows below address resources
    // that exist and a served answer is the operation's 200 rather than its 404.
    let mut ids = Vec::new();
    for name in ["granted", "withheld"] {
        let created = emulator
            .call(
                ACCOUNT_KEY,
                "POST",
                "/profiles",
                Some(&json!({"name": name, "roleArns": []})),
            )
            .await;
        assert_eq!(
            created.status, 201,
            "case {name:?}: the fixture CreateProfile must succeed, got {created:?}"
        );
        ids.push(
            created.body["profile"]["profileId"]
                .as_str()
                .unwrap_or_else(|| panic!("case {name:?}: no profileId in {created:?}"))
                .to_string(),
        );
    }
    let granted_arn = arn_of(&format!("profile/{}", ids[0]));
    let withheld_arn = arn_of(&format!("profile/{}", ids[1]));
    let action = format!("{ACTION_PREFIX}:GetProfile");

    // (case, what the grant is scoped to, whether each call is covered)
    let rows: &[(&str, Value, bool, bool, bool)] = &[
        (
            "one profile by ARN",
            json!(granted_arn.clone()),
            true,
            false,
            false,
        ),
        (
            "every profile of the account",
            json!(arn_of("profile/*")),
            true,
            true,
            true,
        ),
        ("every resource", json!("*"), true, true, true),
        (
            "a profile of another Region",
            json!(format!(
                "arn:aws:{ACTION_PREFIX}:eu-west-3:{}:profile/*",
                ::engine::emulator_account_id()
            )),
            false,
            false,
            false,
        ),
    ];

    for (case, resources, granted_covered, withheld_covered, listing_covered) in rows {
        let user = format!("scoped-{}", case.replace(' ', "-"));
        let principal = emulator
            .user(
                &user,
                Some(&allow(
                    json!([action.clone(), format!("{ACTION_PREFIX}:ListProfiles")]),
                    resources.clone(),
                )),
            )
            .await;
        // (which call, the ARN it is judged against, whether the grant covers it)
        let calls: [(&str, String, String, bool); 3] = [
            (
                "the granted profile",
                format!("/profile/{}", ids[0]),
                granted_arn.clone(),
                *granted_covered,
            ),
            (
                "the other profile",
                format!("/profile/{}", ids[1]),
                withheld_arn.clone(),
                *withheld_covered,
            ),
            (
                "the listing, which addresses no single profile",
                "/profiles".to_string(),
                arn_of("profile/*"),
                *listing_covered,
            ),
        ];
        for (what, path, resource, covered) in calls {
            let labelled = format!("{case}/{what}");
            let operation = if path == "/profiles" {
                format!("{ACTION_PREFIX}:ListProfiles")
            } else {
                action.clone()
            };
            let reply = emulator
                .call(&principal.access_key_id, "GET", &path, None)
                .await;
            if covered {
                assert_eq!(
                    reply.status, 200,
                    "case {labelled:?}: a covered call must be served, got {reply:?}"
                );
            } else {
                assert_refused(
                    &labelled,
                    &reply,
                    &not_authorized(
                        &principal.arn,
                        &operation,
                        &resource,
                        &default_deny(&operation),
                    ),
                );
            }
        }
    }
}

/// RULE: a refused call PERFORMS NOTHING — the decision is taken before the store is reached,
/// so no profile is created, no flag moves, no tag is written and nothing is deleted, and
/// every one of those facts is read back through this service's own operations.
#[tokio::test]
async fn a_refused_call_changes_no_state() {
    let emulator = Emulator::new();
    let created = emulator
        .call(
            ACCOUNT_KEY,
            "POST",
            "/profiles",
            Some(&json!({
                "name": "fixture",
                "roleArns": [],
                "enabled": true,
                "tags": [{"key": "owner", "value": "fixture"}],
            })),
        )
        .await;
    assert_eq!(
        created.status, 201,
        "case \"fixture\": the fixture CreateProfile must succeed, got {created:?}"
    );
    let before = created.body["profile"].clone();
    let id = before["profileId"]
        .as_str()
        .unwrap_or_else(|| panic!("case \"fixture\": no profileId in {created:?}"))
        .to_string();
    let arn = arn_of(&format!("profile/{id}"));

    // A principal every one of this service's actions is explicitly denied for.
    let denied = emulator
        .user(
            "break-glass",
            Some(&allow_all_but(&format!("{ACTION_PREFIX}:*"))),
        )
        .await;

    // (case, method, path, body) — one write of each kind the service serves.
    let attempts: Vec<(&str, &str, String, Option<Value>)> = vec![
        (
            "CreateProfile",
            "POST",
            "/profiles".to_string(),
            Some(json!({"name": "never stored", "roleArns": []})),
        ),
        (
            "DisableProfile",
            "POST",
            format!("/profile/{id}/disable"),
            None,
        ),
        (
            "UpdateProfile",
            "PATCH",
            format!("/profile/{id}"),
            Some(json!({"name": "never renamed"})),
        ),
        (
            "TagResource",
            "POST",
            "/TagResource".to_string(),
            Some(json!({"resourceArn": arn, "tags": [{"key": "sneaked", "value": "in"}]})),
        ),
        (
            "UntagResource",
            "POST",
            "/UntagResource".to_string(),
            Some(json!({"resourceArn": arn, "tagKeys": ["owner"]})),
        ),
        (
            "DeleteProfile",
            "DELETE",
            format!("/profile/{id}"),
            None,
        ),
    ];
    for (case, method, path, body) in attempts {
        let reply = emulator
            .call(&denied.access_key_id, method, &path, body.as_ref())
            .await;
        assert_eq!(
            (reply.status, reply.error_code.as_str()),
            (403, "AccessDeniedException"),
            "case {case:?}: expected the refusal, got {reply:?}"
        );
    }

    // Read back as the account: the fixture is exactly as created, and it is the only profile
    // this account holds.
    let read = emulator
        .call(ACCOUNT_KEY, "GET", &format!("/profile/{id}"), None)
        .await;
    assert_eq!(
        read.status, 200,
        "case \"after the refusals\": GetProfile must still answer, got {read:?}"
    );
    assert_eq!(
        read.body["profile"], before,
        "case \"after the refusals\": the profile must be exactly as created, got {:?}",
        read.body["profile"]
    );
    let listed = emulator.call(ACCOUNT_KEY, "GET", "/profiles", None).await;
    assert_eq!(
        listed.body["profiles"].as_array().map(Vec::len),
        Some(1),
        "case \"after the refusals\": no refused create may store a profile, got {listed:?}"
    );
    let tags = emulator
        .call(
            ACCOUNT_KEY,
            "GET",
            &format!("/ListTagsForResource?resourceArn={arn}"),
            None,
        )
        .await;
    assert_eq!(
        tags.body["tags"],
        json!([{"key": "owner", "value": "fixture"}]),
        "case \"after the refusals\": the tag set must be the one the create left, got {tags:?}"
    );
}

/// RULE: the principal the decision is taken about — and the one a refusal names — is the
/// identity the request's CREDENTIAL speaks for, whichever kind it is: an IAM user's own
/// long-term key is judged on the user's policies, an assumed-role session's key on the
/// ROLE's, and a key no store holds a record for is the account's own published credential,
/// which holds no policies at all and is served.
#[tokio::test]
async fn the_principal_judged_is_the_one_the_credential_speaks_for() {
    let emulator = Emulator::new();
    let action = format!("{ACTION_PREFIX}:ListProfiles");

    // (case, the principal, whether it is served, the clause a refusal carries)
    let allowed_user = emulator
        .user(
            "listing-user",
            Some(&allow(json!(action.clone()), json!("*"))),
        )
        .await;
    let unstated_user = emulator.user("silent-user", None).await;
    let allowed_session = emulator
        .assumed_role("listing-role", &allow(json!(action.clone()), json!("*")))
        .await;
    let denied_session = emulator
        .assumed_role("break-glass-role", &allow_all_but(&action))
        .await;

    let rows: Vec<(&str, Principal, bool, String)> = vec![
        (
            "a key no store holds, the account's own credential",
            Principal {
                access_key_id: ACCOUNT_KEY.to_string(),
                arn: String::new(),
            },
            true,
            String::new(),
        ),
        (
            "an IAM user whose policy allows the action",
            allowed_user,
            true,
            String::new(),
        ),
        (
            "an IAM user carrying no policy at all",
            unstated_user,
            false,
            default_deny(&action),
        ),
        (
            "an assumed-role session whose role allows the action",
            allowed_session,
            true,
            String::new(),
        ),
        (
            "an assumed-role session whose role denies the action",
            denied_session,
            false,
            EXPLICIT_DENY.to_string(),
        ),
    ];

    for (case, principal, served, reason) in rows {
        let reply = emulator
            .call(&principal.access_key_id, "GET", "/profiles", None)
            .await;
        if served {
            assert_eq!(
                reply.status, 200,
                "case {case:?}: expected the listing to be served, got {reply:?}"
            );
        } else {
            assert_refused(
                case,
                &reply,
                &not_authorized(&principal.arn, &action, &arn_of("profile/*"), &reason),
            );
            // A session's refusal names the SESSION, not the role, because that is the
            // identity the credential speaks for.
            assert!(
                reply.message().contains(&principal.arn),
                "case {case:?}: the refusal must name {:?}, got {:?}",
                principal.arn,
                reply.message()
            );
        }
    }
}
