// SPDX-License-Identifier: CC0-1.0

//! The GLOBAL condition keys that describe WHO signed a request and WHERE it was addressed,
//! as a session policy is decided against them: `aws:userid` and `aws:RequestedRegion`.
//!
//! The rule under test: **a clause over a global condition key this gate answers for every
//! request decides its statement, and the two keys that describe the request itself are
//! answered from the request and from the credential it presented — `aws:userid` with the
//! unique id of the principal the credential speaks for (the value `sts:GetCallerIdentity`
//! reports as `UserId`: `<role id>:<session name>` for a role session, the user's own unique
//! id for an IAM user's long-term key, `<account>:<name>` for a federated user), and
//! `aws:RequestedRegion` with the Region the request addressed.** A session policy is a
//! scope-down whose own grant has to reach the call, so a statement conditioned on one of
//! those keys permits exactly the requests its clause holds for and leaves every other to
//! IAM's implicit deny — which is what lets access be pinned to ONE named session, or to one
//! Region.
//!
//! The second rule this suite pins is what a session policy's `Resource` element reaches: an
//! object-key wildcard ARN (`arn:aws:s3:::<bucket>/*`) GRANTS every key under the bucket, so a
//! document that allows a prefix and denies one exact key inside it serves the siblings and
//! refuses that one — including where the denied key's ARN carries non-ASCII characters, which
//! are compared as the bytes they are and settle nothing about the other statements.
//!
//! Every assertion drives real HTTP requests through a router holding S3 beside the two
//! services such a decision reads from: IAM (the role, the user and their identity-based
//! documents) and STS (the mint itself — `AssumeRole`, `GetFederationToken`). Each S3 request
//! presents the access key id of the credential the mint vended, in the credential scope of
//! the Region the request addresses. Nothing reaches into a store to plant a key, a scope or a
//! decision.
//!
//! The tables span what the rules generalise over rather than one recorded call: which key the
//! clause names, which operator compares it (equality, a pattern, negated equality, a
//! set-membership list), the FORM of the compared value (the session's whole composite, the
//! role id alone, the session name alone, another session's composite, the prefix pattern), the
//! Region addressed (the one the clause names and another), the kind of credential (a role
//! session, a federated-user session, an IAM user's long-term key) and the operation called
//! (an object read and an object write, so the rule is pinned on both).

use std::sync::Arc;

use serde_json::json;
use smithy_server_runtime::MultiServiceRouter;

use super::bucket_lifecycle::Answer;
// The readings and spellings the role-session suites of this test binary already state,
// reused rather than copied: the awsQuery form encoder, the XML element reader, the
// identity-document builder, S3's object ARN and the refusal sentence.
use super::identity_authorization::form_encode;
use super::role_session_object_authorization::{
    denial_message, identity_document, object_arn, xml_element,
};

/// The Region this suite's fixtures are written in, and the one its Region-scoped session
/// policies name — the home Region of every row.
const HOME_REGION: &str = "us-east-1";

/// A SECOND Region, which the same credential addresses to make the away-Region leg of the
/// Region table. It is a real Region name of the same partition, so nothing but the clause can
/// decide the difference.
const AWAY_REGION: &str = "us-east-2";

/// The access-key id standing for the account's own published credential, which writes this
/// suite's fixtures: neither STS nor IAM holds a record of it, so it resolves to the account's
/// ambient identity.
///
/// `pub(crate)`, like the harness below it: the suite that drives a role's OWN permissions
/// policy across Regions and the principal forms of a bucket policy
/// ([`super::role_policy_region_and_principal_forms`]) writes its fixtures as the same
/// account rather than spelling a second credential for it.
pub(crate) const ACCOUNT_KEY: &str = "AKIDEMULATOR";

/// The code S3 answers an authorization refusal with, and the status it carries (Amazon S3
/// "Error responses": `AccessDenied`, HTTP 403).
const ACCESS_DENIED: &str = "AccessDenied";

/// The IAM action an object read is authorized under ("you must have the `READ` access to the
/// object" — `s3:GetObject`, `GetObject`).
const GET_OBJECT: &str = "s3:GetObject";

/// The IAM action an object write is authorized under ("you must have the `s3:PutObject`
/// permission", `PutObject`).
const PUT_OBJECT: &str = "s3:PutObject";

/// The bytes every fixture object holds. A policy is about who may reach an object, not about
/// what is in it, so one payload serves every row.
const BODY: &[u8] = b"session-condition-subject";

/// An emulator holding S3 beside the two services a decision about a minted session reads
/// from: IAM (the role or user and its identity-based documents) and STS (the mint).
///
/// `pub(crate)`: the suite that drives a role's OWN permissions policy across Regions and the
/// principal forms of a bucket policy ([`super::role_policy_region_and_principal_forms`])
/// needs the same three services over one engine, and reaches them through this one harness
/// instead of standing up a second one.
pub(crate) struct Emulator {
    router: MultiServiceRouter,
}

/// One credential a row calls with: the access key id a request is presented with, and the ARN
/// a refusal names it by.
pub(crate) struct Credential {
    pub(crate) access_key_id: String,
    pub(crate) principal_arn: String,
}

impl Emulator {
    pub(crate) fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // The suite signs nothing: the `Authorization` header carries only the credential
        // scope, which is where a service reads the Region and the caller's access key id.
        router.set_request_authentication(false);
        router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
            Arc::clone(&engine),
        ));
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

    /// One S3 request presented as signed with `access_key_id` for `region` — the credential
    /// scope is where this emulator reads both the calling principal and the Region addressed.
    pub(crate) async fn s3(
        &self,
        access_key_id: &str,
        region: &str,
        method: &str,
        uri: &str,
        body: &[u8],
    ) -> Answer {
        let host = format!("s3.{region}.amazonaws.com");
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://{host}{uri}"))
            .header("host", host.clone())
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/{region}/s3/\
                     aws4_request, SignedHeaders=host, Signature=0000"
                ),
            )
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

    /// One awsQuery call to IAM or STS, presented as signed with `access_key_id`.
    pub(crate) async fn query(
        &self,
        access_key_id: &str,
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
            .header("host", format!("{signing_name}.amazonaws.com"))
            .header("content-type", "application/x-www-form-urlencoded")
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/{HOME_REGION}/\
                     {signing_name}/aws4_request, SignedHeaders=host, Signature=0000"
                ),
            )
            .body(bytes::Bytes::from(body))
            .expect("a well-formed awsQuery request");
        let response = self.router.dispatch(request).await;
        (
            response.status().as_u16(),
            String::from_utf8_lossy(response.body()).to_string(),
        )
    }

    /// `PUT /<bucket>` as the account, addressed to `region`'s endpoint — with the location
    /// constraint that Region needs, since "an endpoint for one Region can only create a bucket
    /// in that Region" (`CreateBucket`).
    pub(crate) async fn create_bucket(&self, bucket: &str, region: &str) {
        let body = if region == "us-east-1" {
            String::new()
        } else {
            format!(
                "<CreateBucketConfiguration xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">\
                 <LocationConstraint>{region}</LocationConstraint></CreateBucketConfiguration>"
            )
        };
        let created = self
            .s3(
                ACCOUNT_KEY,
                region,
                "PUT",
                &format!("/{bucket}"),
                body.as_bytes(),
            )
            .await;
        assert_eq!(
            created.status,
            200,
            "the bucket {bucket:?} must be created first, got {} with {}",
            created.status,
            text(&created)
        );
    }

    /// `PUT /<bucket>/<key>` as the account — a fixture the read rows address.
    pub(crate) async fn write_fixture(&self, bucket: &str, region: &str, key: &str) {
        let written = self
            .s3(
                ACCOUNT_KEY,
                region,
                "PUT",
                &format!("/{bucket}/{}", encode_key(key)),
                BODY,
            )
            .await;
        assert_eq!(
            written.status,
            200,
            "the fixture {key:?} must be stored first, got {} with {}",
            written.status,
            text(&written)
        );
    }

    /// Create the IAM role `role_name` with `policy` as its inline document, and answer its ARN
    /// and the `RoleId` IAM minted for it — the id `aws:userid` is built from for its sessions.
    pub(crate) async fn create_role(&self, role_name: &str, policy: &str) -> (String, String) {
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
        let (status, body) = self
            .query(
                ACCOUNT_KEY,
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
        let role_id = xml_element(&body, "RoleId")
            .unwrap_or_else(|| panic!("CreateRole must answer a RoleId, got {body}"));
        let (status, put) = self
            .query(
                ACCOUNT_KEY,
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
            "putting the inline policy of {role_name:?} must succeed, got {status} {put}"
        );
        (format!("arn:aws:iam::{account}:role/{role_name}"), role_id)
    }

    /// Assume `role_arn` under `session_name`, passing the session policy `policy`.
    pub(crate) async fn assume(
        &self,
        case: &str,
        role_arn: &str,
        session_name: &str,
        policy: Option<&str>,
    ) -> Credential {
        let mut params = vec![
            ("RoleArn", role_arn.to_string()),
            ("RoleSessionName", session_name.to_string()),
        ];
        if let Some(policy) = policy {
            params.push(("Policy", policy.to_string()));
        }
        let (status, body) = self
            .query(ACCOUNT_KEY, "sts", "2011-06-15", "AssumeRole", &params)
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: assuming {role_arn:?} must succeed, got {status} {body}"
        );
        Credential {
            access_key_id: xml_element(&body, "AccessKeyId").unwrap_or_else(|| {
                panic!("case {case:?}: AssumeRole must answer an AccessKeyId, got {body}")
            }),
            principal_arn: xml_element(&body, "Arn").unwrap_or_else(|| {
                panic!("case {case:?}: AssumeRole must answer the session Arn, got {body}")
            }),
        }
    }

    /// An IAM user carrying `actions` on `"*"` as its inline policy, and the long-term access
    /// key id a request speaks for it with. Answers the key id and the user's own unique id —
    /// the value `aws:userid` holds for a request that key signs.
    pub(crate) async fn iam_user(&self, name: &str, actions: &[&str]) -> (String, String) {
        let (status, body) = self
            .query(
                ACCOUNT_KEY,
                "iam",
                "2010-05-08",
                "CreateUser",
                &[("UserName", name.to_string())],
            )
            .await;
        assert_eq!(status, 200, "the user {name:?} must be created, got {body}");
        let user_id = xml_element(&body, "UserId")
            .unwrap_or_else(|| panic!("CreateUser must answer a UserId, got {body}"));
        let document = identity_document(vec![json!({
            "Effect": "Allow",
            "Action": actions,
            "Resource": "*",
        })]);
        let (status, body) = self
            .query(
                ACCOUNT_KEY,
                "iam",
                "2010-05-08",
                "PutUserPolicy",
                &[
                    ("UserName", name.to_string()),
                    ("PolicyName", "granted".to_string()),
                    ("PolicyDocument", document),
                ],
            )
            .await;
        assert_eq!(
            status, 200,
            "the policy of {name:?} must be stored, got {body}"
        );
        let (status, body) = self
            .query(
                ACCOUNT_KEY,
                "iam",
                "2010-05-08",
                "CreateAccessKey",
                &[("UserName", name.to_string())],
            )
            .await;
        assert_eq!(
            status, 200,
            "an access key for {name:?} must be vended, got {body}"
        );
        let access_key_id = xml_element(&body, "AccessKeyId")
            .unwrap_or_else(|| panic!("CreateAccessKey must answer an AccessKeyId, got {body}"));
        (access_key_id, user_id)
    }

    /// Mint a federated-user session with the long-term key `long_term`, passing `policy` as
    /// its session policy. Answers the credential and the `FederatedUserId` the mint reported —
    /// the value `aws:userid` holds for a request that session signs.
    async fn federate(
        &self,
        case: &str,
        long_term: &str,
        name: &str,
        policy: &str,
    ) -> (Credential, String) {
        let (status, body) = self
            .query(
                long_term,
                "sts",
                "2011-06-15",
                "GetFederationToken",
                &[
                    ("Name", name.to_string()),
                    ("Policy", policy.to_string()),
                ],
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the mint for {name:?} must succeed, got {status} {body}"
        );
        let federated_user_id = xml_element(&body, "FederatedUserId").unwrap_or_else(|| {
            panic!("case {case:?}: GetFederationToken must answer a FederatedUserId, got {body}")
        });
        let credential = Credential {
            access_key_id: xml_element(&body, "AccessKeyId").unwrap_or_else(|| {
                panic!("case {case:?}: GetFederationToken must answer an AccessKeyId, got {body}")
            }),
            // `Credentials` carries no `Arn`, so the first one in the document is the
            // `FederatedUser`'s — the principal a refusal of this session names.
            principal_arn: xml_element(&body, "Arn").unwrap_or_else(|| {
                panic!("case {case:?}: GetFederationToken must answer the FederatedUser Arn, got {body}")
            }),
        };
        (credential, federated_user_id)
    }
}

/// An answer's body as text, with the XML entities S3 escapes a message's quotes as read back,
/// so an expected sentence can be compared as it was worded.
fn text(answer: &Answer) -> String {
    String::from_utf8_lossy(&answer.body)
        .replace("&quot;", "\"")
        .replace("&amp;", "&")
}

/// The path form of an object key: every segment percent-encoded, so a key carrying non-ASCII
/// characters addresses the very bytes it was stored under.
fn encode_key(key: &str) -> String {
    key.split('/')
        .map(form_encode)
        .collect::<Vec<_>>()
        .join("/")
}

/// The clause IAM publishes for a call the SESSION POLICY of the credential does not allow.
fn no_session_grant(action: &str) -> String {
    format!("because no session policy allows the {action} action")
}

/// `document` with every non-ASCII character written as a JSON `\uXXXX` escape — the form a
/// JSON serializer that emits ASCII produces, and the form a session policy carrying non-ASCII
/// text has to arrive in: the modelled document types admit only
/// `[\u0009\u000A\u000D\u0020-\u00FF]` (`GetFederationTokenRequest$Policy`,
/// `sessionPolicyDocumentType`), so a literal multi-byte character is outside the member's
/// pattern while its escape is inside it and decodes to the same text.
fn ascii_escaped(document: &str) -> String {
    document
        .chars()
        .map(|character| {
            if character.is_ascii() {
                character.to_string()
            } else {
                character
                    .encode_utf16(&mut [0; 2])
                    .iter()
                    .map(|unit| format!("\\u{unit:04x}"))
                    .collect()
            }
        })
        .collect()
}

/// The clause IAM publishes for a call a session policy explicitly denies.
const SESSION_DENY: &str = "with an explicit deny in a session policy";

/// Assert `answer` is S3's `AccessDenied` refusal, worded for `principal`, `action` and
/// `resource` with the clause `reason`.
///
/// `pub(crate)`, like the harness above it: the suite that drives a role's own
/// Region-conditioned policy and the principal forms of a bucket policy
/// ([`super::role_policy_region_and_principal_forms`]) asserts the same refusal through this
/// one reading rather than a second copy of it.
pub(crate) fn assert_refused(
    case: &str,
    answer: &Answer,
    principal: &str,
    action: &str,
    resource: &str,
    reason: &str,
) {
    assert_refusal(
        case,
        answer,
        &denial_message(principal, action, resource, reason),
    );
}

/// Assert `answer` is that same refusal for an action published with NO resource types, where
/// there is nothing for the `on resource:` clause to carry: the account's own listing is such an
/// action, and the sentence is the one the suite that drives that listing states
/// ([`super::account_wide_listing_authorization::account_wide_denial_message`]).
///
/// `pub(crate)` beside [`assert_refused`], so the resource-bearing and the resourceless form of
/// one sentence are read in one place.
pub(crate) fn assert_refused_without_resource(
    case: &str,
    answer: &Answer,
    principal: &str,
    action: &str,
    reason: &str,
) {
    assert_refusal(
        case,
        answer,
        &super::account_wide_listing_authorization::account_wide_denial_message(
            principal, action, reason,
        ),
    );
}

/// What the two readings above share: the status and the code S3 answers every authorization
/// refusal with, and the `expected` sentence each of them worded.
fn assert_refusal(case: &str, answer: &Answer, expected: &str) {
    let body = text(answer);
    assert_eq!(
        answer.status, 403,
        "case {case:?}: expected HTTP 403, got {} with {body}",
        answer.status
    );
    assert!(
        body.contains(ACCESS_DENIED),
        "case {case:?}: expected the {ACCESS_DENIED} code, got {body}"
    );
    assert!(
        body.contains(expected),
        "case {case:?}: expected the message {expected:?}, got {body}"
    );
}

/// Assert `answer` served the fixture's bytes.
///
/// `pub(crate)` beside [`assert_refused`], and for the same reason.
pub(crate) fn assert_served(case: &str, answer: &Answer) {
    assert_eq!(
        answer.status,
        200,
        "case {case:?}: expected the call to be authorized, got {} with {}",
        answer.status,
        text(answer)
    );
}

// ---------------------------------------------------------------------------
// RULE: `aws:userid` answers with the unique id of the principal the credential speaks for —
// the `<role id>:<session name>` composite of a role session — so a session policy conditioned
// on that composite permits exactly that session's calls.
// ---------------------------------------------------------------------------

/// What a row's clause compares `aws:userid` against: each is a FORM of the value, built from
/// the session's own composite at run time, so nothing here is a value copied out of one run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UserIdValue {
    /// The session's whole `<role id>:<session name>` composite — the value AWS documents and
    /// `sts:GetCallerIdentity` reports as `UserId`.
    OwnComposite,
    /// The role id with the session name replaced by a wildcard, under `StringLike`: every
    /// session of the role.
    RolePrefixPattern,
    /// The bare role id, which no session's `aws:userid` equals.
    RoleIdAlone,
    /// The bare session name, likewise.
    SessionNameAlone,
    /// ANOTHER session's composite: the same role, a different session name.
    OtherSessionComposite,
    /// The session's own composite under a NEGATED operator, which it does not satisfy.
    NotOwnComposite,
    /// A SET the session's composite is one entry of.
    CompositeInSet,
}

impl UserIdValue {
    /// The `Condition` object this form builds, for the session `<role_id>:<session_name>`.
    fn condition(self, role_id: &str, session_name: &str) -> serde_json::Value {
        let composite = format!("{role_id}:{session_name}");
        match self {
            Self::OwnComposite => json!({"StringEquals": {"aws:userid": composite}}),
            Self::RolePrefixPattern => {
                json!({"StringLike": {"aws:userid": format!("{role_id}:*")}})
            }
            Self::RoleIdAlone => json!({"StringEquals": {"aws:userid": role_id}}),
            Self::SessionNameAlone => json!({"StringEquals": {"aws:userid": session_name}}),
            Self::OtherSessionComposite => {
                json!({"StringEquals": {"aws:userid": format!("{role_id}:another-session")}})
            }
            Self::NotOwnComposite => json!({"StringNotEquals": {"aws:userid": composite}}),
            Self::CompositeInSet => {
                json!({"StringEquals": {"aws:userid": ["AIDAEXAMPLEUSERID", composite]}})
            }
        }
    }
}

/// RULE: `aws:userid` for a role session is the `<RoleId>:<RoleSessionName>` composite, so a
/// session policy whose only grant is conditioned on that composite permits the call, a grant
/// conditioned on any other form of the value permits nothing, and the role's own
/// unconditioned policy is never what decides it.
#[tokio::test]
async fn a_session_policy_conditioned_on_aws_userid_decides_the_sessions_own_calls() {
    let bucket = "userid-condition-subject";
    let key = "reports/held.csv";
    // The rows differ along the FORM of the compared value and the operator comparing it; the
    // last two rows make the same comparison on an object WRITE, so the rule is pinned on both
    // kinds of call.
    let rows: &[(&str, UserIdValue, &str, bool)] = &[
        (
            "the session's own documented composite",
            UserIdValue::OwnComposite,
            GET_OBJECT,
            true,
        ),
        (
            "every session of the role, by pattern",
            UserIdValue::RolePrefixPattern,
            GET_OBJECT,
            true,
        ),
        (
            "the session's composite as one entry of a set",
            UserIdValue::CompositeInSet,
            GET_OBJECT,
            true,
        ),
        (
            "the bare role id",
            UserIdValue::RoleIdAlone,
            GET_OBJECT,
            false,
        ),
        (
            "the bare session name",
            UserIdValue::SessionNameAlone,
            GET_OBJECT,
            false,
        ),
        (
            "another session's composite",
            UserIdValue::OtherSessionComposite,
            GET_OBJECT,
            false,
        ),
        (
            "every id BUT the session's own",
            UserIdValue::NotOwnComposite,
            GET_OBJECT,
            false,
        ),
        (
            "the session's own composite, on a write",
            UserIdValue::OwnComposite,
            PUT_OBJECT,
            true,
        ),
        (
            "another session's composite, on a write",
            UserIdValue::OtherSessionComposite,
            PUT_OBJECT,
            false,
        ),
    ];

    for (index, (case, value, action, allowed)) in rows.iter().enumerate() {
        let emulator = Emulator::new();
        emulator.create_bucket(bucket, HOME_REGION).await;
        emulator.write_fixture(bucket, HOME_REGION, key).await;
        // The role allows both object actions on the key unconditionally, so every refusal
        // below is the session policy's clause talking and never the role's own document.
        let role_policy = identity_document(vec![json!({
            "Effect": "Allow",
            "Action": [GET_OBJECT, PUT_OBJECT],
            "Resource": object_arn(bucket, key),
        })]);
        let (role_arn, role_id) = emulator
            .create_role(&format!("userid-role-{index}"), &role_policy)
            .await;
        let session_name = format!("pinned-session-{index}");
        let scope = identity_document(vec![json!({
            "Effect": "Allow",
            "Action": action,
            "Resource": object_arn(bucket, key),
            "Condition": value.condition(&role_id, &session_name),
        })]);
        let session = emulator
            .assume(case, &role_arn, &session_name, Some(&scope))
            .await;
        let (method, body): (&str, &[u8]) = match *action {
            PUT_OBJECT => ("PUT", b"rewritten"),
            _ => ("GET", b""),
        };
        let answer = emulator
            .s3(
                &session.access_key_id,
                HOME_REGION,
                method,
                &format!("/{bucket}/{key}"),
                body,
            )
            .await;
        if *allowed {
            assert_served(case, &answer);
        } else {
            assert_refused(
                case,
                &answer,
                &session.principal_arn,
                action,
                &object_arn(bucket, key),
                &no_session_grant(action),
            );
        }
    }
}

/// RULE, on the other credentials the same key describes: `aws:userid` is the unique id of the
/// principal the credential speaks for, whichever credential that is — the IAM user's own
/// unique id for a request signed with its long-term key, and `<account>:<Name>` for a
/// federated-user session, which is the id its mint reported.
#[tokio::test]
async fn aws_userid_answers_the_unique_id_of_every_kind_of_credential() {
    let bucket = "userid-credential-kinds";
    let key = "plain/other.txt";

    // --- an IAM user's own long-term access key ---
    let emulator = Emulator::new();
    emulator.create_bucket(bucket, HOME_REGION).await;
    emulator.write_fixture(bucket, HOME_REGION, key).await;
    let (long_term, user_id) = emulator.iam_user("pinned-user", &[GET_OBJECT]).await;
    for (case, condition, allowed) in [
        (
            "a user policy pinned to the user's own unique id",
            json!({"StringEquals": {"aws:userid": user_id.clone()}}),
            true,
        ),
        (
            "a user policy pinned to another unique id",
            json!({"StringEquals": {"aws:userid": "AIDAEXAMPLEUSERID"}}),
            false,
        ),
    ] {
        // The conditioned document replaces the user's own grant, so the clause is the only
        // thing that can decide the read.
        let document = identity_document(vec![json!({
            "Effect": "Allow",
            "Action": GET_OBJECT,
            "Resource": object_arn(bucket, key),
            "Condition": condition,
        })]);
        let (status, body) = emulator
            .query(
                ACCOUNT_KEY,
                "iam",
                "2010-05-08",
                "PutUserPolicy",
                &[
                    ("UserName", "pinned-user".to_string()),
                    ("PolicyName", "granted".to_string()),
                    ("PolicyDocument", document),
                ],
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the policy must be stored, got {body}"
        );
        let answer = emulator
            .s3(
                &long_term,
                HOME_REGION,
                "GET",
                &format!("/{bucket}/{key}"),
                b"",
            )
            .await;
        if allowed {
            assert_served(case, &answer);
        } else {
            assert_eq!(
                answer.status,
                403,
                "case {case:?}: expected HTTP 403, got {} with {}",
                answer.status,
                text(&answer)
            );
        }
    }

    // --- a federated-user session, whose unique id is <account>:<Name> ---
    for (case, allowed) in [
        ("a session policy pinned to the federated user's own id", true),
        ("a session policy pinned to another federated id", false),
    ] {
        let emulator = Emulator::new();
        emulator.create_bucket(bucket, HOME_REGION).await;
        emulator.write_fixture(bucket, HOME_REGION, key).await;
        let (long_term, _) = emulator
            .iam_user("federating-user", &[GET_OBJECT, "sts:GetFederationToken"])
            .await;
        let name = "pinned-federate";
        let expected_id = format!("{}:{name}", engine::emulator_account_id());
        let compared = if allowed {
            expected_id.clone()
        } else {
            format!("{}:somebody-else", engine::emulator_account_id())
        };
        let scope = identity_document(vec![json!({
            "Effect": "Allow",
            "Action": GET_OBJECT,
            "Resource": object_arn(bucket, key),
            "Condition": {"StringEquals": {"aws:userid": compared}},
        })]);
        let (session, federated_user_id) = emulator.federate(case, &long_term, name, &scope).await;
        assert_eq!(
            federated_user_id, expected_id,
            "case {case:?}: the mint must report <account>:<Name> as the federated user id"
        );
        let answer = emulator
            .s3(
                &session.access_key_id,
                HOME_REGION,
                "GET",
                &format!("/{bucket}/{key}"),
                b"",
            )
            .await;
        if allowed {
            assert_served(case, &answer);
        } else {
            assert_refused(
                case,
                &answer,
                &session.principal_arn,
                GET_OBJECT,
                &object_arn(bucket, key),
                &no_session_grant(GET_OBJECT),
            );
        }
    }
}

// ---------------------------------------------------------------------------
// RULE: `aws:RequestedRegion` answers with the Region the request addressed, so a session
// policy conditioned on one Region permits that Region's calls and refuses the SAME
// credential's calls to another.
// ---------------------------------------------------------------------------

/// RULE: a session policy whose only grant carries `StringEquals aws:RequestedRegion` permits
/// the call addressed to that Region and refuses the same session's call to another, while a
/// session the same role minted with NO session policy reaches both — so the Region clause is
/// what decides it, and a refused write leaves no object behind.
#[tokio::test]
async fn a_session_policy_conditioned_on_the_requested_region_gates_by_region() {
    let home_bucket = "region-scope-home";
    let away_bucket = "region-scope-away";
    let key = "landed.txt";

    let emulator = Emulator::new();
    emulator.create_bucket(home_bucket, HOME_REGION).await;
    emulator.create_bucket(away_bucket, AWAY_REGION).await;
    // The role allows every object action on both buckets, so nothing but the Region clause can
    // decide any row.
    let role_policy = identity_document(vec![json!({
        "Effect": "Allow",
        "Action": "s3:*",
        "Resource": [
            format!("arn:aws:s3:::{home_bucket}/*"),
            format!("arn:aws:s3:::{away_bucket}/*"),
            format!("arn:aws:s3:::{home_bucket}"),
            format!("arn:aws:s3:::{away_bucket}"),
        ],
    })]);
    let (role_arn, _) = emulator.create_role("region-scope-role", &role_policy).await;

    let pinned_scope = identity_document(vec![json!({
        "Effect": "Allow",
        "Action": "s3:*",
        "Resource": [
            format!("arn:aws:s3:::{home_bucket}/*"),
            format!("arn:aws:s3:::{away_bucket}/*"),
        ],
        "Condition": {"StringEquals": {"aws:RequestedRegion": HOME_REGION}},
    })]);
    let pinned = emulator
        .assume("pinned to the home Region", &role_arn, "pinned", Some(&pinned_scope))
        .await;
    // The control: the same role, no session policy, so it is capped by nothing.
    let control = emulator.assume("no session policy", &role_arn, "control", None).await;

    // One row per (credential, Region, bucket) leg the rule decides.
    let rows: &[(&str, &Credential, &str, &str, bool)] = &[
        (
            "the pinned session writing in the Region its clause names",
            &pinned,
            HOME_REGION,
            home_bucket,
            true,
        ),
        (
            "the pinned session writing in another Region",
            &pinned,
            AWAY_REGION,
            away_bucket,
            false,
        ),
        (
            "the control session writing in the home Region",
            &control,
            HOME_REGION,
            home_bucket,
            true,
        ),
        (
            "the control session writing in the away Region",
            &control,
            AWAY_REGION,
            away_bucket,
            true,
        ),
    ];

    for (case, credential, region, bucket, allowed) in rows {
        let written = emulator
            .s3(
                &credential.access_key_id,
                region,
                "PUT",
                &format!("/{bucket}/{key}"),
                BODY,
            )
            .await;
        if *allowed {
            assert_served(case, &written);
            let read = emulator
                .s3(
                    &credential.access_key_id,
                    region,
                    "GET",
                    &format!("/{bucket}/{key}"),
                    b"",
                )
                .await;
            assert_served(case, &read);
            assert_eq!(
                read.body.as_ref(),
                BODY,
                "case {case:?}: expected the written bytes back, got {}",
                text(&read)
            );
        } else {
            assert_refused(
                case,
                &written,
                &credential.principal_arn,
                PUT_OBJECT,
                &object_arn(bucket, key),
                &no_session_grant(PUT_OBJECT),
            );
            // A refused write stores nothing: the bucket owner's own HEAD finds no object.
            let head = emulator
                .s3(
                    ACCOUNT_KEY,
                    region,
                    "HEAD",
                    &format!("/{bucket}/{key}"),
                    b"",
                )
                .await;
            assert_eq!(
                head.status, 404,
                "case {case:?}: a refused write must leave no object behind, got {}",
                head.status
            );
        }
    }
}

// ---------------------------------------------------------------------------
// RULE: a session policy's object-key wildcard ARN grants every key under the bucket, and an
// exact-ARN `Deny` beside it carves out exactly that one key — non-ASCII key bytes included.
// ---------------------------------------------------------------------------

/// The key whose ARN a row's `Deny` names exactly: accented Latin and CJK characters, so the
/// comparison is over the key's own bytes rather than over an ASCII subset of them.
const UNICODE_KEY: &str = "café/日本語/Zürich-résumé.txt";

/// The sibling key the same `Allow` covers, which the `Deny` does not name.
const SIBLING_KEY: &str = "plain/other.txt";

/// One row of the wildcard table: what to call the case, the `Resource` element the row's
/// `Allow` carries (built from the bucket the row addresses, so no ARN is hard-coded), the key
/// the read then addresses, and whether that read is served.
struct WildcardRow {
    case: &'static str,
    allowed_resource: fn(&str) -> serde_json::Value,
    read_key: &'static str,
    served: bool,
}

/// RULE: the `Resource` element of a session-policy statement reaches whatever its pattern
/// matches, so an object-key wildcard ARN grants every key under the bucket while an exact-ARN
/// `Deny` beside it refuses only the key it names — whatever bytes that key carries.
#[tokio::test]
async fn a_session_policys_object_key_wildcard_grants_every_key_under_the_bucket() {
    let bucket = "wildcard-scope-subject";
    // The rows differ along the FORM of the Allow's `Resource` and the key the read addresses:
    // the bucket-wide wildcard, a prefix wildcard, the exact ARN, and IAM's own bare wildcard.
    let rows: &[WildcardRow] = &[
        WildcardRow {
            case: "the bucket's object wildcard, on the sibling key",
            allowed_resource: |bucket| json!(format!("arn:aws:s3:::{bucket}/*")),
            read_key: SIBLING_KEY,
            served: true,
        },
        WildcardRow {
            case: "the bucket's object wildcard, on the key the Deny names",
            allowed_resource: |bucket| json!(format!("arn:aws:s3:::{bucket}/*")),
            read_key: UNICODE_KEY,
            served: false,
        },
        WildcardRow {
            case: "a prefix wildcard covering the sibling key",
            allowed_resource: |bucket| json!(format!("arn:aws:s3:::{bucket}/plain/*")),
            read_key: SIBLING_KEY,
            served: true,
        },
        WildcardRow {
            case: "the sibling key's own exact ARN",
            allowed_resource: |bucket| json!(object_arn(bucket, SIBLING_KEY)),
            read_key: SIBLING_KEY,
            served: true,
        },
        WildcardRow {
            case: "the bare wildcard every resource matches",
            allowed_resource: |_bucket| json!("*"),
            read_key: SIBLING_KEY,
            served: true,
        },
        WildcardRow {
            case: "a list naming the bucket's object wildcard beside another bucket's",
            allowed_resource: |bucket| {
                json!([
                    "arn:aws:s3:::somewhere-else/*",
                    format!("arn:aws:s3:::{bucket}/*")
                ])
            },
            read_key: SIBLING_KEY,
            served: true,
        },
        WildcardRow {
            case: "another bucket's object wildcard alone",
            allowed_resource: |_bucket| json!("arn:aws:s3:::somewhere-else/*"),
            read_key: SIBLING_KEY,
            served: false,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let WildcardRow {
            case,
            allowed_resource,
            read_key,
            served,
        } = row;
        let emulator = Emulator::new();
        emulator.create_bucket(bucket, HOME_REGION).await;
        emulator.write_fixture(bucket, HOME_REGION, UNICODE_KEY).await;
        emulator.write_fixture(bucket, HOME_REGION, SIBLING_KEY).await;
        // The minting user's own policy allows the read on everything, so the session policy is
        // the only document that can withhold it.
        let (long_term, _) = emulator
            .iam_user(
                &format!("wildcard-user-{index}"),
                &[GET_OBJECT, "sts:GetFederationToken"],
            )
            .await;
        let scope = identity_document(vec![
            json!({
                "Sid": "AllowGetObject",
                "Effect": "Allow",
                "Action": GET_OBJECT,
                "Resource": allowed_resource(bucket),
            }),
            json!({
                "Sid": "DenyExactUnicodeKey",
                "Effect": "Deny",
                "Action": GET_OBJECT,
                "Resource": object_arn(bucket, UNICODE_KEY),
            }),
        ]);
        let (session, _) = emulator
            .federate(case, &long_term, &format!("fedwild{index}"), &ascii_escaped(&scope))
            .await;
        let answer = emulator
            .s3(
                &session.access_key_id,
                HOME_REGION,
                "GET",
                &format!("/{bucket}/{}", encode_key(read_key)),
                b"",
            )
            .await;
        if *served {
            assert_served(case, &answer);
            assert_eq!(
                answer.body.as_ref(),
                BODY,
                "case {case:?}: expected the stored bytes, got {}",
                text(&answer)
            );
        } else {
            // The key the `Deny` names is refused by that deny; a key no `Allow` reaches is
            // refused for the absent grant.
            let reason = if *read_key == UNICODE_KEY {
                SESSION_DENY.to_string()
            } else {
                no_session_grant(GET_OBJECT)
            };
            assert_refused(
                case,
                &answer,
                &session.principal_arn,
                GET_OBJECT,
                &object_arn(bucket, read_key),
                &reason,
            );
        }
    }
}

/// RULE, on the sequence a carve-out is used in: the SAME session reads the key its `Deny`
/// names and is refused, and the siblings its `Allow` covers are still served afterwards — a
/// refused read decides nothing about the next one, and the prefix is not lost to the carve-out.
#[tokio::test]
async fn a_denied_key_does_not_withdraw_the_rest_of_the_prefix_from_the_same_session() {
    let bucket = "wildcard-carveout-subject";
    let emulator = Emulator::new();
    emulator.create_bucket(bucket, HOME_REGION).await;
    for key in [UNICODE_KEY, SIBLING_KEY, "plain/second.txt"] {
        emulator.write_fixture(bucket, HOME_REGION, key).await;
    }
    let (long_term, _) = emulator
        .iam_user("carveout-user", &[GET_OBJECT, "sts:GetFederationToken"])
        .await;
    let scope = identity_document(vec![
        json!({
            "Sid": "AllowGetObject",
            "Effect": "Allow",
            "Action": GET_OBJECT,
            "Resource": format!("arn:aws:s3:::{bucket}/*"),
        }),
        json!({
            "Sid": "DenyExactUnicodeKey",
            "Effect": "Deny",
            "Action": GET_OBJECT,
            "Resource": object_arn(bucket, UNICODE_KEY),
        }),
    ]);
    let (session, _) = emulator
        .federate(
            "the carve-out sequence",
            &long_term,
            "fedcarveout",
            &ascii_escaped(&scope),
        )
        .await;

    // In order: the carved-out key, then each sibling the prefix grant covers, then the
    // carved-out key again — so neither the refusal nor a served read moves the decision.
    let legs: &[(&str, &str, bool)] = &[
        ("the carved-out key", UNICODE_KEY, false),
        ("a sibling under another prefix", SIBLING_KEY, true),
        ("a second sibling", "plain/second.txt", true),
        ("the carved-out key once more", UNICODE_KEY, false),
    ];
    for (case, key, served) in legs {
        let answer = emulator
            .s3(
                &session.access_key_id,
                HOME_REGION,
                "GET",
                &format!("/{bucket}/{}", encode_key(key)),
                b"",
            )
            .await;
        if *served {
            assert_served(case, &answer);
            assert_eq!(
                answer.body.as_ref(),
                BODY,
                "case {case:?}: expected the stored bytes, got {}",
                text(&answer)
            );
        } else {
            assert_refused(
                case,
                &answer,
                &session.principal_arn,
                GET_OBJECT,
                &object_arn(bucket, key),
                SESSION_DENY,
            );
        }
    }
}
