// SPDX-License-Identifier: CC0-1.0

//! What admits an ASSUMED-ROLE SESSION to an object read: the bucket policy's own `Principal`
//! element, and a conditioned grant decided against the bucket's tags while ABAC is enabled on
//! it.
//!
//! RULE 1 (a resource policy names a principal, and a role names every session of it): a bucket
//! policy statement whose `Principal` names an IAM role's ARN is in force on every session
//! assumed from that role — "the temporary security credentials … have the permissions of the
//! role" (`AssumeRole`) and the resource-based policy "is evaluated together" with the
//! identity's own documents, "and access is granted if either allows it" (IAM User Guide,
//! *Policy evaluation logic*) — so an `Allow` so matched serves the read with no identity-based
//! grant anywhere, an explicit `Deny` so matched refuses it whatever the identity's documents
//! allow (` with an explicit deny in a resource-based policy`), and a statement naming any other
//! principal — another role, another session of the same role — decides nothing about that
//! session, leaving the read to IAM's implicit deny (`because no identity-based policy allows
//! the <action> action`).
//!
//! RULE 2 (an ABAC pair is a pair of VALUES, one from each side): while a general purpose
//! bucket's ABAC status is `Enabled` — "if ABAC is enabled on your bucket, you can use tags on
//! the bucket for access control" (`GetBucketAbac`'s own documentation) — every call addressed
//! to that bucket or to an object it holds answers `aws:ResourceTag/<key>` from the bucket's tag
//! set and `aws:PrincipalTag/<key>` from the calling session's tags, and a condition VALUE
//! written as IAM's policy variable `${aws:PrincipalTag/<key>}` is substituted from that same
//! context before the comparison. So a grant conditioned that way serves the read for the
//! session whose tag EQUALS the bucket's and withholds it from every other session, under
//! whichever tag key the clause is written over.
//!
//! Four crates have to agree for either rule to hold — IAM holds the role and its documents,
//! STS mints the tagged session, CloudFormation deploys the policy a stack declares and S3 takes
//! the decision and serves (or withholds) the bytes — so this is the suite that can pin them:
//! a crate-local suite only ever sees one side. The per-service halves live beside their own
//! service: S3's own refusal wording, its ABAC status plane and the principal spellings a bucket
//! policy may name are that crate's tests.
//!
//! The rows span what the rules generalise over rather than one recorded call: for rule 1 WHICH
//! principal the statement names (the role, the role among a list, another role, the calling
//! session itself, another session of the same role, no policy at all), the `Effect` it states,
//! the resource it is scoped over (every object of the bucket, the one object addressed),
//! whether the role carries an identity-based grant beside it, and WHICH session of the role
//! calls; for rule 2 the condition's tag KEY and the characters of its value, whether the two
//! sides' values are equal (and whether they differ only in case), which side carries the key at
//! all, whether the condition value is the policy variable or a literal, and how many other tags
//! sit beside the pair. Rule 1 is then driven again through a CloudFormation-deployed
//! `AWS::S3::BucketPolicy` whose `Effect` comes from a stack parameter, so the flip from `Allow`
//! to `Deny` is a stack update rather than a second document.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::json;
use smithy_server_runtime::MultiServiceRouter;

// The percent-encoder every awsQuery suite of this crate shares and the XML element readers
// written ONCE in `query_wire`, which its own header asks every new suite to call rather than
// copy. `elements` decodes the entities S3's serializer escapes a message's quotes as.
use crate::query_wire::{element, elements, form_encode, opt_element, outputs_of};

/// The Region every request here is signed for.
const REGION: &str = "us-east-1";

/// The long-term key every emulator publishes — the account's own credential, which no
/// identity-based document bounds. Every fixture call is made with it, so what a row decides is
/// its own session's access and nothing else.
const ACCOUNT_KEY: &str = "AKIAIOSFODNN7EXAMPLE";

/// The API versions the three awsQuery services this suite calls name.
const IAM_VERSION: &str = "2010-05-08";
const STS_VERSION: &str = "2011-06-15";
const CLOUDFORMATION_VERSION: &str = "2010-05-15";

/// The key every row reads, and the bytes behind it: a served read must answer exactly these.
const KEY: &str = "doc.txt";
const BODY: &str = "role-session-readable-bytes";

/// The action an object read naming no version is authorized under: "if you request the current
/// version of an object without a specific `versionId` in the request header, only the
/// `s3:GetObject` permission is required" (`GetObject`'s own documentation).
const GET_OBJECT: &str = "s3:GetObject";

/// The error code S3 answers a call the policies in force refuse with.
const ACCESS_DENIED: &str = "AccessDenied";

/// The value of `BucketAbacStatus` that puts a bucket's tags in force on access decisions
/// (`smithy_query(query="shape",shape="BucketAbacStatus")` names `Enabled` and `Disabled`).
const ABAC_ENABLED: &str = "Enabled";

/// The statuses a stack operation settles at, and the in-flight suffix a poll waits past.
const IN_FLIGHT_SUFFIX: &str = "_IN_PROGRESS";
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const UPDATE_COMPLETE: &str = "UPDATE_COMPLETE";

/// How long a poll keeps looking for the condition it waits on, and how often it looks again.
/// The deadline is far longer than the span this emulator gives a stack's transitions, so a
/// loaded host only makes a wait longer: what is waited FOR is the condition, never a length of
/// time.
const WAIT_DEADLINE: Duration = Duration::from_secs(60);
const WAIT_POLL: Duration = Duration::from_millis(50);

/// The account every ARN this emulator mints carries, which owns every bucket and role below.
fn account() -> &'static str {
    engine::emulator_account_id()
}

/// The four services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them: the role IAM holds is the role STS mints a session of,
/// and the policy CloudFormation installs is the policy S3 decides against.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Nothing here is really signed; what names the caller is the access key id in the
    // credential scope, which is what this suite varies.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        engine,
    ));
    router
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name` and whose
/// credential is `access_key_id` — where a service reads the caller's identity from.
fn authorization(access_key_id: &str, signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/{REGION}/{signing_name}/\
         aws4_request, SignedHeaders=host, Signature=not-verified"
    )
}

/// One awsQuery call of `signing_name`, presented as signed by `access_key_id`.
async fn query(
    router: &MultiServiceRouter,
    access_key_id: &str,
    signing_name: &str,
    version: &str,
    action: &str,
    members: &[(String, String)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={version}");
    for (name, value) in members {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let host = format!("{signing_name}.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", host)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(access_key_id, signing_name))
        .body(Bytes::from(body))
        .expect("the test request builds");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One S3 REST call on the path-style address, presented as signed by `access_key_id`.
async fn s3(
    router: &MultiServiceRouter,
    access_key_id: &str,
    method: &str,
    path: &str,
    content_type: &str,
    body: &[u8],
) -> (u16, String) {
    let host = format!("s3.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host)
        .header("content-type", content_type)
        .header("authorization", authorization(access_key_id, "s3"))
        .body(Bytes::copy_from_slice(body))
        .expect("the test request builds");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One role session a row calls as.
struct Session {
    /// The session's access key id, which is where every service reads WHO is calling.
    access_key_id: String,
    /// The assumed-role session ARN `sts:GetCallerIdentity` reports for it, which a refusal
    /// names and which a resource policy may name as its principal.
    session_arn: String,
}

/// The trust policy every role here carries: the account's own root may assume it and may attach
/// session tags — "to pass session tags … the role's trust policy must allow the
/// `sts:TagSession` action" (`AssumeRoleRequest$Tags`).
fn trust_policy() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"AWS": format!("arn:aws:iam::{}:root", account())},
            "Action": ["sts:AssumeRole", "sts:TagSession"],
        }],
    })
    .to_string()
}

/// Create `role_name` trusted by this account's root, embed `policy` as its one inline document
/// where a row states one, and answer the role's ARN as IAM reports it.
async fn create_role(
    router: &MultiServiceRouter,
    case: &str,
    role_name: &str,
    policy: Option<&str>,
) -> String {
    let (status, body) = query(
        router,
        ACCOUNT_KEY,
        "iam",
        IAM_VERSION,
        "CreateRole",
        &[
            ("RoleName".to_string(), role_name.to_string()),
            ("AssumeRolePolicyDocument".to_string(), trust_policy()),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateRole({role_name:?}) must be served, got {status} with {body}"
    );
    let role_arn = element(&body, "Arn");
    if let Some(policy) = policy {
        let (status, body) = query(
            router,
            ACCOUNT_KEY,
            "iam",
            IAM_VERSION,
            "PutRolePolicy",
            &[
                ("RoleName".to_string(), role_name.to_string()),
                ("PolicyName".to_string(), "in-force".to_string()),
                ("PolicyDocument".to_string(), policy.to_string()),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: PutRolePolicy({role_name:?}) must be served, got {status} with {body}"
        );
    }
    role_arn
}

/// Assume `role_arn` under `session_name`, attaching `tags` as the session's tags, and answer
/// the session STS minted. The answer carries credential material, so nothing here quotes it.
async fn assume(
    router: &MultiServiceRouter,
    case: &str,
    role_arn: &str,
    session_name: &str,
    tags: &[(&str, &str)],
) -> Session {
    let mut members = vec![
        ("RoleArn".to_string(), role_arn.to_string()),
        ("RoleSessionName".to_string(), session_name.to_string()),
    ];
    // The `Tags` member of `AssumeRole`, as awsQuery flattens a list of structures.
    for (position, (key, value)) in tags.iter().enumerate() {
        members.push((
            format!("Tags.member.{}.Key", position + 1),
            (*key).to_string(),
        ));
        members.push((
            format!("Tags.member.{}.Value", position + 1),
            (*value).to_string(),
        ));
    }
    let (status, body) = query(
        router,
        ACCOUNT_KEY,
        "sts",
        STS_VERSION,
        "AssumeRole",
        &members,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: AssumeRole({role_arn:?}) with tags {tags:?} must be served, got {status}"
    );
    Session {
        access_key_id: opt_element(&body, "AccessKeyId").unwrap_or_else(|| {
            panic!("case {case:?}: AssumeRole must answer an AccessKeyId for {session_name:?}")
        }),
        session_arn: opt_element(&body, "Arn").unwrap_or_else(|| {
            panic!("case {case:?}: AssumeRole must answer the session Arn for {session_name:?}")
        }),
    }
}

/// `PUT /<bucket>` then `PUT /<bucket>/<key>` as the account: the bucket and the object every
/// row reads, written before any policy or tag is installed.
async fn bucket_holding_the_object(router: &MultiServiceRouter, case: &str, bucket: &str) {
    let (status, body) = s3(
        router,
        ACCOUNT_KEY,
        "PUT",
        &format!("/{bucket}"),
        "application/xml",
        b"",
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateBucket({bucket:?}) must be served, got {status} with {body}"
    );
    put_object(router, case, bucket, BODY).await;
}

/// `PUT /<bucket>/<key>` as the account.
async fn put_object(router: &MultiServiceRouter, case: &str, bucket: &str, body: &str) {
    let (status, answered) = s3(
        router,
        ACCOUNT_KEY,
        "PUT",
        &format!("/{bucket}/{KEY}"),
        "application/octet-stream",
        body.as_bytes(),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: PutObject({bucket:?}/{KEY}) must be served, got {status} with {answered}"
    );
}

/// `PUT /<bucket>?policy` as the account — the bucket's own policy.
async fn put_bucket_policy(router: &MultiServiceRouter, case: &str, bucket: &str, document: &str) {
    let (status, body) = s3(
        router,
        ACCOUNT_KEY,
        "PUT",
        &format!("/{bucket}?policy"),
        "application/xml",
        document.as_bytes(),
    )
    .await;
    assert_eq!(
        status, 204,
        "case {case:?}: PutBucketPolicy({bucket:?}) must be served, got {status} with {body}"
    );
}

/// `PUT /<bucket>?tagging` as the account — the tag set an ABAC decision is taken over.
async fn tag_bucket(
    router: &MultiServiceRouter,
    case: &str,
    bucket: &str,
    tags: &[(&str, &str)],
) {
    let entries: String = tags
        .iter()
        .map(|(key, value)| format!("<Tag><Key>{key}</Key><Value>{value}</Value></Tag>"))
        .collect();
    let document = format!(
        "<Tagging xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><TagSet>{entries}</TagSet>\
         </Tagging>"
    );
    let (status, body) = s3(
        router,
        ACCOUNT_KEY,
        "PUT",
        &format!("/{bucket}?tagging"),
        "application/xml",
        document.as_bytes(),
    )
    .await;
    assert_eq!(
        status, 204,
        "case {case:?}: the tag set {tags:?} must be stored on {bucket:?}, got {status} with \
         {body}"
    );
}

/// `PUT /<bucket>?abac` as the account, read back through `GetBucketAbac`, so a row is decided
/// under the status the bucket really carries.
async fn enable_abac(router: &MultiServiceRouter, case: &str, bucket: &str) {
    let document = format!(
        "<AbacStatus xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><Status>{ABAC_ENABLED}\
         </Status></AbacStatus>"
    );
    let (status, body) = s3(
        router,
        ACCOUNT_KEY,
        "PUT",
        &format!("/{bucket}?abac"),
        "application/xml",
        document.as_bytes(),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: PutBucketAbac({bucket:?}) must be served, got {status} with {body}"
    );
    let (status, body) = s3(
        router,
        ACCOUNT_KEY,
        "GET",
        &format!("/{bucket}?abac"),
        "application/xml",
        b"",
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetBucketAbac({bucket:?}) must report the status, got {status} with {body}"
    );
    assert_eq!(
        opt_element(&body, "Status").as_deref(),
        Some(ABAC_ENABLED),
        "case {case:?}: expected the ABAC status {ABAC_ENABLED:?} on {bucket:?}, got {body}"
    );
}

/// The ARN a policy and a refusal name the object addressed by — S3's own form, which carries
/// neither a Region nor an account.
fn object_arn(bucket: &str) -> String {
    format!("arn:aws:s3:::{bucket}/{KEY}")
}

/// The ARN a statement scopes a grant over every object of the bucket by.
fn every_object_arn(bucket: &str) -> String {
    format!("arn:aws:s3:::{bucket}/*")
}

/// Which of IAM's refusing rules decided, as the clause closing S3's sentence words it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reason {
    /// Nothing in force allowed the read — IAM's implicit deny.
    NoGrant,
    /// A statement of the BUCKET's own policy said `Deny`.
    ResourceDeny,
}

impl Reason {
    fn clause(self) -> String {
        match self {
            Self::NoGrant => {
                format!("because no identity-based policy allows the {GET_OBJECT} action")
            }
            Self::ResourceDeny => "with an explicit deny in a resource-based policy".to_string(),
        }
    }
}

/// The whole sentence a withheld read is refused with: who, which action, which object —
/// double-quoted — and why.
fn expected_sentence(principal: &str, bucket: &str, reason: Reason) -> String {
    format!(
        "User: {principal} is not authorized to perform: {GET_OBJECT} on resource: \"{}\" {}",
        object_arn(bucket),
        reason.clause()
    )
}

/// The text of the first `<Message>` of an error document, entities decoded.
fn message(body: &str) -> String {
    elements(body, "Message")
        .into_iter()
        .next()
        .unwrap_or_else(|| format!("<no Message> in {body}"))
}

/// Assert a read was served exactly the stored bytes.
fn assert_served(case: &str, status: u16, body: &str) {
    assert_eq!(
        (status, body),
        (200, BODY),
        "case {case:?}: expected the object's exact bytes, got {status} with {body:?}"
    );
}

/// Assert a read was refused in S3's `AccessDenied` envelope, with IAM's sentence for `reason`
/// and none of the object's bytes.
fn assert_refused(
    case: &str,
    status: u16,
    body: &str,
    principal: &str,
    bucket: &str,
    reason: Reason,
) {
    assert_eq!(
        status, 403,
        "case {case:?}: expected HTTP 403, got {status} with {body}"
    );
    assert_eq!(
        opt_element(body, "Code").as_deref(),
        Some(ACCESS_DENIED),
        "case {case:?}: expected the {ACCESS_DENIED} code, got {body}"
    );
    assert_eq!(
        message(body),
        expected_sentence(principal, bucket, reason),
        "case {case:?}: expected IAM's sentence for {reason:?}, got {}",
        message(body)
    );
    assert!(
        !body.contains(BODY),
        "case {case:?}: a refused read must carry no object bytes, got {body}"
    );
}

// ---------------------------------------------------------------------------
// RULE 1: a bucket policy's `Principal` naming a role is in force on every session of that role.
// ---------------------------------------------------------------------------

/// WHICH principal a row's bucket policy names, in the spelling it names it by.
#[derive(Debug, Clone, Copy)]
enum Named {
    /// The bucket carries no policy at all.
    Nothing,
    /// The ARN of the role the calling session is a session of.
    TheRole,
    /// That same ARN, in a LIST beside another role's.
    TheRoleAmongSeveral,
    /// Another role's ARN.
    AnotherRole,
    /// The calling session's own ARN, as STS reports it.
    TheCallingSession,
    /// A different session of the SAME role, named by its own session ARN.
    AnotherSessionOfTheRole,
}

/// The resource a row's statement is scoped over.
#[derive(Debug, Clone, Copy)]
enum Scope {
    /// Every object of the bucket.
    EveryObject,
    /// The one object the read addresses.
    TheObject,
}

/// What the policies in force must make of a row's read.
#[derive(Debug, Clone, Copy)]
enum Expected {
    Served,
    Refused(Reason),
}

/// One row of the principal table.
struct GrantRow {
    case: &'static str,
    named: Named,
    /// The `Effect` the statement states; read only where [`Named`] installs one.
    effect: &'static str,
    scope: Scope,
    /// Whether the role carries an identity-based grant of the read beside the bucket policy.
    identity_grant: bool,
    /// Whether the read is made by the SECOND session of the role rather than the first.
    second_session: bool,
    expected: Expected,
}

const GRANT_ROWS: &[GrantRow] = &[
    GrantRow {
        case: "no bucket policy and no identity grant",
        named: Named::Nothing,
        effect: "Allow",
        scope: Scope::EveryObject,
        identity_grant: false,
        second_session: false,
        expected: Expected::Refused(Reason::NoGrant),
    },
    GrantRow {
        case: "an Allow naming the role's ARN, with no identity grant at all",
        named: Named::TheRole,
        effect: "Allow",
        scope: Scope::EveryObject,
        identity_grant: false,
        second_session: false,
        expected: Expected::Served,
    },
    GrantRow {
        case: "an Allow naming the role's ARN, scoped to the one object addressed",
        named: Named::TheRole,
        effect: "Allow",
        scope: Scope::TheObject,
        identity_grant: false,
        second_session: false,
        expected: Expected::Served,
    },
    GrantRow {
        case: "an Allow naming the role's ARN among several principals",
        named: Named::TheRoleAmongSeveral,
        effect: "Allow",
        scope: Scope::EveryObject,
        identity_grant: false,
        second_session: false,
        expected: Expected::Served,
    },
    GrantRow {
        case: "an Allow naming the role's ARN, read by another session of that role",
        named: Named::TheRole,
        effect: "Allow",
        scope: Scope::EveryObject,
        identity_grant: false,
        second_session: true,
        expected: Expected::Served,
    },
    GrantRow {
        case: "an Allow naming the calling session's own ARN",
        named: Named::TheCallingSession,
        effect: "Allow",
        scope: Scope::EveryObject,
        identity_grant: false,
        second_session: false,
        expected: Expected::Served,
    },
    GrantRow {
        case: "an Allow naming another session of the same role",
        named: Named::AnotherSessionOfTheRole,
        effect: "Allow",
        scope: Scope::EveryObject,
        identity_grant: false,
        second_session: false,
        expected: Expected::Refused(Reason::NoGrant),
    },
    GrantRow {
        case: "an Allow naming another role's ARN",
        named: Named::AnotherRole,
        effect: "Allow",
        scope: Scope::EveryObject,
        identity_grant: false,
        second_session: false,
        expected: Expected::Refused(Reason::NoGrant),
    },
    GrantRow {
        case: "a Deny naming the role's ARN over the role's own identity grant",
        named: Named::TheRole,
        effect: "Deny",
        scope: Scope::EveryObject,
        identity_grant: true,
        second_session: false,
        expected: Expected::Refused(Reason::ResourceDeny),
    },
    GrantRow {
        case: "a Deny naming the role's ARN, read by another session of that role",
        named: Named::TheRole,
        effect: "Deny",
        scope: Scope::EveryObject,
        identity_grant: true,
        second_session: true,
        expected: Expected::Refused(Reason::ResourceDeny),
    },
    GrantRow {
        case: "a Deny naming another role's ARN over the role's own identity grant",
        named: Named::AnotherRole,
        effect: "Deny",
        scope: Scope::EveryObject,
        identity_grant: true,
        second_session: false,
        expected: Expected::Served,
    },
];

/// One identity-based document granting the read over every object of `bucket`: no `Principal`
/// element, since the identity it is embedded in IS the principal.
fn identity_grant(bucket: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": GET_OBJECT,
            "Resource": every_object_arn(bucket),
        }],
    })
    .to_string()
}

/// The bucket policy a row installs, or `None` for the row whose bucket carries none.
fn bucket_policy(
    row: &GrantRow,
    bucket: &str,
    role_arn: &str,
    other_role_arn: &str,
    session_arn: &str,
    role_name: &str,
) -> Option<String> {
    // A session of this role that was never minted, named in the ONE spelling a resource policy
    // names a session by: the role's name and a session name, which is what makes the session
    // segment part of the comparison rather than a suffix truncated to the role.
    let another_session = format!(
        "arn:aws:sts::{}:assumed-role/{role_name}/a-third-session",
        account()
    );
    let principal = match row.named {
        Named::Nothing => return None,
        Named::TheRole => json!({"AWS": role_arn}),
        Named::TheRoleAmongSeveral => json!({"AWS": [other_role_arn, role_arn]}),
        Named::AnotherRole => json!({"AWS": other_role_arn}),
        Named::TheCallingSession => json!({"AWS": session_arn}),
        Named::AnotherSessionOfTheRole => json!({"AWS": another_session}),
    };
    let resource = match row.scope {
        Scope::EveryObject => every_object_arn(bucket),
        Scope::TheObject => object_arn(bucket),
    };
    Some(
        json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Sid": "ReaderGetObject",
                "Effect": row.effect,
                "Principal": principal,
                "Action": GET_OBJECT,
                "Resource": resource,
            }],
        })
        .to_string(),
    )
}

/// RULE 1: a bucket policy statement naming a role's ARN as its `Principal` decides the read for
/// every session assumed from that role — an `Allow` serves it with no identity-based grant
/// anywhere, an explicit `Deny` refuses it over such a grant, and a statement naming any other
/// principal leaves the read where it was.
#[tokio::test]
async fn a_bucket_policy_naming_a_role_decides_every_session_of_that_role() {
    let router = build_router();
    let account = account();
    let mut served = 0_usize;
    let mut refused = 0_usize;
    for (index, row) in GRANT_ROWS.iter().enumerate() {
        let case = row.case;
        let bucket = format!("role-grant-{index}-{account}");
        bucket_holding_the_object(&router, case, &bucket).await;

        let role_name = format!("role-grant-reader-{index}");
        let other_name = format!("role-grant-bystander-{index}");
        let document = row.identity_grant.then(|| identity_grant(&bucket));
        let role_arn = create_role(&router, case, &role_name, document.as_deref()).await;
        // A real second role of the same account, so a statement naming "another role" is a
        // grant to SOMEBODY rather than a malformed one.
        let other_role_arn = create_role(&router, case, &other_name, None).await;

        let first = assume(&router, case, &role_arn, "first-session", &[]).await;
        let second = assume(&router, case, &role_arn, "second-session", &[]).await;
        let caller = if row.second_session { &second } else { &first };

        if let Some(policy) = bucket_policy(
            row,
            &bucket,
            &role_arn,
            &other_role_arn,
            &caller.session_arn,
            &role_name,
        ) {
            put_bucket_policy(&router, case, &bucket, &policy).await;
        }

        let (status, body) = s3(
            &router,
            &caller.access_key_id,
            "GET",
            &format!("/{bucket}/{KEY}"),
            "application/octet-stream",
            b"",
        )
        .await;
        match row.expected {
            Expected::Served => {
                served += 1;
                assert_served(case, status, &body);
            }
            Expected::Refused(reason) => {
                refused += 1;
                assert_refused(case, status, &body, &caller.session_arn, &bucket, reason);
            }
        }
    }
    assert!(
        served > 0 && refused > 0,
        "the table must drive both served and refused reads, got {served} served and {refused} \
         refused"
    );
}

// ---------------------------------------------------------------------------
// RULE 2: an ABAC grant is decided by the pair (the bucket's tag, the session's tag), with the
// policy variable on the clause's own side substituted first.
// ---------------------------------------------------------------------------

/// One row of the ABAC pair table.
struct AbacRow {
    case: &'static str,
    /// The tag key the clause is written over — `aws:ResourceTag/<key>` on the clause's key
    /// side and `${aws:PrincipalTag/<key>}` on its value side.
    tag_key: &'static str,
    bucket_tags: &'static [(&'static str, &'static str)],
    session_tags: &'static [(&'static str, &'static str)],
    /// `None` is the policy-variable form; `Some` a literal the bucket's tag is compared with.
    literal: Option<&'static str>,
    allowed: bool,
}

const ABAC_ROWS: &[AbacRow] = &[
    AbacRow {
        case: "the two sides carry the same value under the clause's key",
        tag_key: "project",
        bucket_tags: &[("project", "atlas")],
        session_tags: &[("project", "atlas")],
        literal: None,
        allowed: true,
    },
    AbacRow {
        case: "the session's value differs from the bucket's only in case",
        tag_key: "project",
        bucket_tags: &[("project", "atlas")],
        session_tags: &[("project", "Atlas")],
        literal: None,
        allowed: false,
    },
    AbacRow {
        case: "a key and a value made of letters, digits and a hyphen",
        tag_key: "cost-center",
        bucket_tags: &[("cost-center", "cc-1042")],
        session_tags: &[("cost-center", "cc-1042")],
        literal: None,
        allowed: true,
    },
    AbacRow {
        case: "the bucket carries no tag under the clause's key",
        tag_key: "project",
        bucket_tags: &[("division", "atlas")],
        session_tags: &[("project", "atlas")],
        literal: None,
        allowed: false,
    },
    AbacRow {
        case: "the session carries no tag at all",
        tag_key: "project",
        bucket_tags: &[("project", "atlas")],
        session_tags: &[],
        literal: None,
        allowed: false,
    },
    AbacRow {
        case: "a literal value the bucket's tag equals, for an untagged session",
        tag_key: "project",
        bucket_tags: &[("project", "atlas")],
        session_tags: &[],
        literal: Some("atlas"),
        allowed: true,
    },
    AbacRow {
        case: "a literal value the bucket's tag does not equal",
        tag_key: "project",
        bucket_tags: &[("project", "beacon")],
        session_tags: &[("project", "atlas")],
        literal: Some("atlas"),
        allowed: false,
    },
    AbacRow {
        case: "the pair sits among several tags, under another key than the first",
        tag_key: "owner",
        bucket_tags: &[("project", "atlas"), ("owner", "platform"), ("tier", "gold")],
        session_tags: &[("project", "beacon"), ("owner", "platform")],
        literal: None,
        allowed: true,
    },
];

/// An identity-based document whose only statement grants the read over every object of `bucket`
/// under a `StringEquals` clause comparing the bucket's own `tag_key` tag with `expected`.
fn conditioned_grant(bucket: &str, tag_key: &str, expected: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": GET_OBJECT,
            "Resource": every_object_arn(bucket),
            "Condition": {
                "StringEquals": {format!("aws:ResourceTag/{tag_key}"): expected},
            },
        }],
    })
    .to_string()
}

/// RULE 2: while ABAC is enabled on the bucket, a grant conditioned on the bucket's own tag is
/// decided by the pair of values — the bucket's tag and what the clause's value resolves to,
/// which for IAM's policy variable is the calling session's own tag — so the matched session
/// reads the object and every other session is refused with IAM's implicit-deny sentence.
#[tokio::test]
async fn an_abac_conditioned_grant_admits_the_tag_matched_session_only() {
    let router = build_router();
    let account = account();
    let mut served = 0_usize;
    let mut refused = 0_usize;
    for (index, row) in ABAC_ROWS.iter().enumerate() {
        let case = row.case;
        let bucket = format!("abac-pair-{index}-{account}");
        bucket_holding_the_object(&router, case, &bucket).await;
        tag_bucket(&router, case, &bucket, row.bucket_tags).await;
        enable_abac(&router, case, &bucket).await;

        // The clause's own value: IAM's policy variable naming the caller's tag, or a literal.
        let expected = match row.literal {
            Some(literal) => literal.to_string(),
            None => format!("${{aws:PrincipalTag/{}}}", row.tag_key),
        };
        let policy = conditioned_grant(&bucket, row.tag_key, &expected);
        let role_name = format!("abac-pair-role-{index}");
        let role_arn = create_role(&router, case, &role_name, Some(&policy)).await;
        let session = assume(
            &router,
            case,
            &role_arn,
            &format!("abac-session-{index}"),
            row.session_tags,
        )
        .await;

        let (status, body) = s3(
            &router,
            &session.access_key_id,
            "GET",
            &format!("/{bucket}/{KEY}"),
            "application/octet-stream",
            b"",
        )
        .await;
        if row.allowed {
            served += 1;
            assert_served(case, status, &body);
        } else {
            refused += 1;
            assert_refused(
                case,
                status,
                &body,
                &session.session_arn,
                &bucket,
                Reason::NoGrant,
            );
        }
    }
    assert!(
        served > 0 && refused > 0,
        "the table must drive both served and refused reads, got {served} served and {refused} \
         refused"
    );
}

// ---------------------------------------------------------------------------
// RULE 1, deployed: the same grant declared by a stack, whose `Effect` is a stack parameter.
// ---------------------------------------------------------------------------

/// The parameter names the deployed template reads, and the two effects a revision states.
const BUCKET_PARAMETER: &str = "BucketNameParam";
const EFFECT_PARAMETER: &str = "ReaderEffect";
const ALLOW: &str = "Allow";
const DENY: &str = "Deny";

/// The template both revisions deploy: a role the account may assume, a bucket named by a
/// parameter, and the bucket's own policy granting that role's ARN the read under the `Effect`
/// the other parameter states.
fn stack_template() -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Parameters": {
            BUCKET_PARAMETER: {"Type": "String"},
            EFFECT_PARAMETER: {"Type": "String", "Default": ALLOW},
        },
        "Resources": {
            "ReaderRole": {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "AssumeRolePolicyDocument": {
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Effect": "Allow",
                            "Principal": {
                                "AWS": {"Fn::Sub": "arn:aws:iam::${AWS::AccountId}:root"},
                            },
                            "Action": "sts:AssumeRole",
                        }],
                    },
                },
            },
            "Bucket": {
                "Type": "AWS::S3::Bucket",
                "Properties": {"BucketName": {"Ref": BUCKET_PARAMETER}},
            },
            "ReaderPolicy": {
                "Type": "AWS::S3::BucketPolicy",
                "Properties": {
                    "Bucket": {"Ref": "Bucket"},
                    "PolicyDocument": {
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Sid": "ReaderGetObject",
                            "Effect": {"Ref": EFFECT_PARAMETER},
                            "Principal": {"AWS": {"Fn::GetAtt": ["ReaderRole", "Arn"]}},
                            "Action": GET_OBJECT,
                            "Resource": {"Fn::Sub": "${Bucket.Arn}/*"},
                        }],
                    },
                },
            },
        },
        "Outputs": {
            "BucketName": {"Value": {"Ref": "Bucket"}},
            "ReaderRoleArn": {"Value": {"Fn::GetAtt": ["ReaderRole", "Arn"]}},
        },
    })
    .to_string()
}

/// Deploy (`CreateStack`) or redeploy (`UpdateStack`) the template with `effect` in force, and
/// answer the `DescribeStacks` body the stack SETTLED on.
async fn deploy(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    stack: &str,
    bucket: &str,
    effect: &str,
) -> String {
    let members = vec![
        ("StackName".to_string(), stack.to_string()),
        ("TemplateBody".to_string(), stack_template()),
        // The template declares an IAM role CloudFormation names itself, which is what this
        // capability covers.
        (
            "Capabilities.member.1".to_string(),
            "CAPABILITY_IAM".to_string(),
        ),
        (
            "Parameters.member.1.ParameterKey".to_string(),
            BUCKET_PARAMETER.to_string(),
        ),
        (
            "Parameters.member.1.ParameterValue".to_string(),
            bucket.to_string(),
        ),
        (
            "Parameters.member.2.ParameterKey".to_string(),
            EFFECT_PARAMETER.to_string(),
        ),
        (
            "Parameters.member.2.ParameterValue".to_string(),
            effect.to_string(),
        ),
    ];
    let (status, body) = query(
        router,
        ACCOUNT_KEY,
        "cloudformation",
        CLOUDFORMATION_VERSION,
        action,
        &members,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: {action}({stack:?}) must be served, got {status} with {body}"
    );
    // Wait for the stack to SETTLE rather than for a span to pass.
    let deadline = Instant::now() + WAIT_DEADLINE;
    loop {
        let (status, described) = query(
            router,
            ACCOUNT_KEY,
            "cloudformation",
            CLOUDFORMATION_VERSION,
            "DescribeStacks",
            &[("StackName".to_string(), stack.to_string())],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeStacks({stack:?}) must be served, got {status} with {described}"
        );
        let reported = opt_element(&described, "StackStatus").unwrap_or_default();
        if !reported.is_empty() && !reported.ends_with(IN_FLIGHT_SUFFIX) {
            return described;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {stack:?} to settle, last status {reported:?}"
        );
        tokio::time::sleep(WAIT_POLL).await;
    }
}

/// Read `bucket`'s object as `access_key_id` until the answer's status is `wanted`, and answer
/// the body it came with.
///
/// The CONDITION is what is waited for — the grant the deployment just installed taking effect —
/// so a loaded host only makes the wait longer.
async fn read_until(
    router: &MultiServiceRouter,
    case: &str,
    access_key_id: &str,
    bucket: &str,
    wanted: u16,
) -> String {
    let deadline = Instant::now() + WAIT_DEADLINE;
    loop {
        let (status, body) = s3(
            router,
            access_key_id,
            "GET",
            &format!("/{bucket}/{KEY}"),
            "application/octet-stream",
            b"",
        )
        .await;
        if status == wanted {
            return body;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected GetObject to answer {wanted}, last answer {status} with \
             {body}"
        );
        tokio::time::sleep(WAIT_POLL).await;
    }
}

/// RULE 1, through the deployment a client writes: the `Effect` of a stack-declared
/// `AWS::S3::BucketPolicy` naming the stack's own role decides that role's sessions — `Allow`
/// serves the read, the stack updated to `Deny` refuses it with the resource-policy clause, and
/// the bucket owner, named by neither statement, reads throughout.
#[tokio::test]
async fn a_stack_declared_bucket_policy_effect_decides_its_reader_role() {
    let router = build_router();
    let case = "the stack-declared reader grant";
    let stack = "reader-grant-stack";
    let bucket = format!("reader-grant-{}", account());

    let created = deploy(&router, case, "CreateStack", stack, &bucket, ALLOW).await;
    assert_eq!(
        opt_element(&created, "StackStatus").as_deref(),
        Some(CREATE_COMPLETE),
        "case {case:?}: the stack must reach {CREATE_COMPLETE}, got {created}"
    );
    let outputs = outputs_of(&created);
    assert_eq!(
        outputs.get("BucketName").map(String::as_str),
        Some(bucket.as_str()),
        "case {case:?}: expected the stack to publish the bucket it created, got {outputs:?}"
    );
    let role_arn = outputs
        .get("ReaderRoleArn")
        .unwrap_or_else(|| panic!("case {case:?}: the stack must publish its role ARN"))
        .clone();

    // The owner writes the object the reader reads, after the stack is up: the grant is the
    // bucket policy's, so nothing about the object depends on who wrote it.
    put_object(&router, case, &bucket, BODY).await;
    let session = assume(&router, case, &role_arn, "reader-session", &[]).await;

    let served = read_until(&router, case, &session.access_key_id, &bucket, 200).await;
    assert_served("the Allow revision", 200, &served);

    let updated = deploy(&router, case, "UpdateStack", stack, &bucket, DENY).await;
    assert_eq!(
        opt_element(&updated, "StackStatus").as_deref(),
        Some(UPDATE_COMPLETE),
        "case {case:?}: the update must reach {UPDATE_COMPLETE}, got {updated}"
    );

    let denied = read_until(&router, case, &session.access_key_id, &bucket, 403).await;
    assert_refused(
        "the Deny revision",
        403,
        &denied,
        &session.session_arn,
        &bucket,
        Reason::ResourceDeny,
    );
    // The owner is named by neither statement, so the Deny leaves its own read untouched.
    let (status, body) = s3(
        &router,
        ACCOUNT_KEY,
        "GET",
        &format!("/{bucket}/{KEY}"),
        "application/octet-stream",
        b"",
    )
    .await;
    assert_served("the owner under the Deny revision", status, &body);
}
