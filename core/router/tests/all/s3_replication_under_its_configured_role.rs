// SPDX-License-Identifier: CC0-1.0

//! S3 cross-bucket replication over the IAM records that authorize it, on the ASSEMBLED
//! emulator.
//!
//! RULE: a write into a bucket carrying an enabled replication rule is copied to the
//! destination that rule names by Amazon S3 assuming "the Identity and Access Management
//! (IAM) role that Amazon S3 assumes when replicating objects"
//! (`ReplicationConfiguration$Role`) and acting as it — so the copy is made exactly where the
//! permissions IN FORCE on that role allow the replication actions at the source's versions
//! and at the destination's objects, whichever identity-based document carries the grant and
//! however that document is written. Where the grant does not reach the destination the
//! rule names, nothing is copied there and the source version settles on the terminal
//! `FAILED` ("if one or more destinations fails replication the header will return FAILED",
//! `HeadObjectOutput$ReplicationStatus`); where it does, the source settles on `COMPLETED`
//! and the destination holds the version byte-identically, under the same key and version id,
//! reported as a `REPLICA` (`s3#ReplicationStatus`).
//!
//! The table's rows differ along what the rule generalises over:
//!
//! * WHERE the grant lives — an INLINE policy embedded in the role (`PutRolePolicy`) and a
//!   MANAGED policy attached to it (`CreatePolicy` then `AttachRolePolicy`), the two
//!   identity-based forms a role's permissions arrive in;
//! * HOW the grant is WRITTEN — each action named in full, a prefix wildcard over a family of
//!   them, or the whole service wildcard scoped by its resources;
//! * WHICH resources it names — the source's versions and the destination's objects, a
//!   bucket beside its objects, and another bucket's objects.
//!
//! What an existing suite already owns is not restated here: the trust-policy half of the
//! rule, and the inline grant naming each action in full, are pinned by
//! `service-com-amazonaws-s3-amazons3/tests/all/replication_role_trust.rs`.
//!
//! Two crates have to agree for the rule to hold (IAM holds the role, its trust document and
//! the policies in force on it; S3 resolves that role per write and performs the copies), so
//! the suite lives here and drives the emulator as it is assembled. Every call goes over the
//! wire: the role and its documents are made by IAM's own API, the configuration by
//! `PutBucketReplication`, the write by `PutObject`, and the outcome is read back with
//! `HeadObject` on the source and `GetObject` on the destination.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, form_encode, opt_element};

/// The Region every request here is signed for, and so the endpoint it addresses.
const REGION: &str = "us-east-1";

/// The service principal a replication role's trust policy has to admit for S3 to assume it:
/// S3's own, as the replication role's trust relationship names it (Amazon S3 User Guide,
/// *Setting up permissions for replication*).
const S3_SERVICE_PRINCIPAL: &str = "s3.amazonaws.com";

/// The role every row's configuration names.
const ROLE_NAME: &str = "s3-replication-granted-role";

/// The source bucket of every row, and the destination its one rule names.
const SOURCE: &str = "xsvc-replication-grant-source";
/// See [`SOURCE`].
const DESTINATION: &str = "xsvc-replication-grant-destination";

/// A bucket no row creates, for the grant that reaches somewhere else entirely.
const ELSEWHERE: &str = "xsvc-replication-grant-elsewhere";

/// The key every row writes, and the bytes it writes there.
const KEY: &str = "replicated/hello.txt";
/// See [`KEY`].
const BODY: &str = "the bytes a replication copies";

/// The `ID` every row's one replication rule carries, read back off `GetBucketReplication`.
const RULE_ID: &str = "only-rule";

/// The `ReplicationRule$Status` of a rule that replicates, which is also the
/// `VersioningConfiguration$Status` both buckets are created with.
const ENABLED: &str = "Enabled";

/// The header `HeadObject` and `GetObject` report a version's replication outcome in
/// (`HeadObjectOutput$ReplicationStatus`).
const STATUS_HEADER: &str = "x-amz-replication-status";

/// The header a versioned write reports the id of the version it recorded in.
const VERSION_HEADER: &str = "x-amz-version-id";

/// The status a source version reports while its replication is still in flight.
const PENDING: &str = "PENDING";

/// The status a source version settles on once every copy its configuration called for was
/// made, and the one it settles on when a copy was not.
const COMPLETED: &str = "COMPLETED";
/// See [`COMPLETED`].
const FAILED: &str = "FAILED";

/// The status the COPY carries in the destination bucket.
const REPLICA: &str = "REPLICA";

/// How long a settled status is waited FOR before the wait is called a failure, and how often
/// to look again meanwhile. The deadline is a generous ceiling on an asynchronous outcome
/// rather than a measurement of one: "Amazon S3 replicates objects asynchronously"
/// (`PutBucketReplication`), so what is waited for is the CONDITION and nothing here asserts
/// how long the span was.
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);
/// See [`SETTLE_DEADLINE`].
const SETTLE_POLL: Duration = Duration::from_millis(50);

/// The emulator as it is ASSEMBLED, on one engine, so S3's own hop to IAM is answered by the
/// IAM this suite wrote its records through.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what routes
/// the request. The signature is never checked: request authentication is off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDTEST/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// One awsQuery call to IAM, answered with the HTTP status and the XML body.
async fn iam(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-05-08");
    for (name, value) in members {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let host = format!("iam.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", host)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("iam"))
        .body(Bytes::from(body))
        .expect("a well-formed IAM request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One request against S3's REST surface, path-style, as its status, its headers and its body.
async fn s3(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: &str,
) -> (u16, http::HeaderMap, String) {
    let host = format!("s3.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host)
        .header("authorization", authorization("s3"))
        .header("content-type", "application/octet-stream")
        .body(Bytes::from(body.to_owned()))
        .expect("a well-formed S3 request");
    let response = router.dispatch(request).await;
    let (parts, answered) = response.into_parts();
    (
        parts.status.as_u16(),
        parts.headers,
        String::from_utf8_lossy(&answered).into_owned(),
    )
}

/// One response header, as text.
fn header(headers: &http::HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

/// The ARN of the role [`ROLE_NAME`] in this emulator's account.
fn role_arn() -> String {
    format!(
        "arn:aws:iam::{}:role/{ROLE_NAME}",
        engine::emulator_account_id()
    )
}

/// The trust document a role S3's replication can assume carries: one statement letting the
/// S3 service principal perform the assume.
fn trust_document() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"Service": S3_SERVICE_PRINCIPAL},
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string()
}

/// A permissions policy carrying `statements`.
fn policy(statements: Vec<Value>) -> String {
    json!({"Version": "2012-10-17", "Statement": statements}).to_string()
}

/// One statement: `effect` for `actions` on `resources`, each written as JSON so a row may
/// name one value or a list.
fn statement(effect: &str, actions: Value, resources: Value) -> Value {
    json!({"Effect": effect, "Action": actions, "Resource": resources})
}

/// The objects of `name`, as a policy's `Resource` names them.
fn objects_of(name: &str) -> Value {
    json!(format!("arn:aws:s3:::{name}/*"))
}

/// The bucket `name` AND its objects, as a two-element `Resource` list — the form a document
/// written for both the bucket-level and the object-level actions carries.
fn bucket_and_objects_of(name: &str) -> Value {
    json!([format!("arn:aws:s3:::{name}"), format!("arn:aws:s3:::{name}/*")])
}

/// The actions replication performs at the SOURCE: the read of the version every copy begins
/// with, beside that version's ACL. The names are the ones the Amazon S3 User Guide's
/// replication role policy grants (*Setting up permissions for replication*).
fn source_actions() -> Value {
    json!(["s3:GetObjectVersionForReplication", "s3:GetObjectVersionAcl"])
}

/// The actions it performs at the DESTINATION — the write of the replica and of a replicated
/// delete marker, from the same published policy.
fn destination_actions() -> Value {
    json!(["s3:ReplicateObject", "s3:ReplicateDelete"])
}

/// The bucket-level actions that policy grants on the source bucket itself, beside the
/// object-level ones.
fn bucket_actions() -> Value {
    json!(["s3:GetReplicationConfiguration", "s3:ListBucket"])
}

/// A grant written with each action named in full, on the source's versions and the
/// destination's objects.
fn explicit_grant() -> String {
    policy(vec![
        statement("Allow", source_actions(), objects_of(SOURCE)),
        statement("Allow", destination_actions(), objects_of(DESTINATION)),
    ])
}

/// The same grant written with PREFIX WILDCARDS over the two action families, which is how a
/// client routinely writes the replication policy.
fn wildcard_grant() -> String {
    policy(vec![
        statement("Allow", json!(["s3:GetObjectVersion*"]), objects_of(SOURCE)),
        statement("Allow", json!(["s3:Replicate*"]), objects_of(DESTINATION)),
    ])
}

/// The published role policy's three statements: the bucket-level actions on the source bucket
/// and its objects, the version reads on the source's objects, and the replication writes on
/// the destination's objects.
fn three_statement_grant() -> String {
    policy(vec![
        statement("Allow", bucket_actions(), bucket_and_objects_of(SOURCE)),
        statement("Allow", source_actions(), objects_of(SOURCE)),
        statement("Allow", destination_actions(), objects_of(DESTINATION)),
    ])
}

/// The whole service wildcard, scoped by the two buckets' objects.
fn service_wildcard_grant() -> String {
    policy(vec![statement(
        "Allow",
        json!("s3:*"),
        json!([
            format!("arn:aws:s3:::{SOURCE}/*"),
            format!("arn:aws:s3:::{DESTINATION}/*")
        ]),
    )])
}

/// A grant whose replication writes land at ANOTHER bucket's objects, so the destination this
/// configuration names is covered by nothing.
fn elsewhere_grant() -> String {
    policy(vec![
        statement("Allow", source_actions(), objects_of(SOURCE)),
        statement("Allow", destination_actions(), objects_of(ELSEWHERE)),
    ])
}

/// Where the grant a row states LIVES — the two identity-based forms a role's permissions
/// arrive in.
#[derive(Clone, Copy, Debug)]
enum GrantSource {
    /// An inline policy embedded in the role itself (`PutRolePolicy`).
    Inline,
    /// A managed policy of this account, attached to the role (`CreatePolicy` then
    /// `AttachRolePolicy`).
    Attached,
}

impl GrantSource {
    /// The name this form goes by in an assertion's message.
    fn label(self) -> &'static str {
        match self {
            Self::Inline => "an inline policy",
            Self::Attached => "an attached managed policy",
        }
    }

    /// Put `document` in force on the role [`ROLE_NAME`] through THIS form, using IAM's own
    /// API.
    async fn grant(self, router: &MultiServiceRouter, case: &str, document: &str) {
        match self {
            Self::Inline => {
                let (status, body) = iam(
                    router,
                    "PutRolePolicy",
                    &[
                        ("RoleName", ROLE_NAME),
                        ("PolicyName", "replication"),
                        ("PolicyDocument", document),
                    ],
                )
                .await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: embedding the inline policy must succeed, got {status} with \
                     {body}"
                );
            }
            Self::Attached => {
                let (status, body) = iam(
                    router,
                    "CreatePolicy",
                    &[
                        ("PolicyName", "replication-grant"),
                        ("PolicyDocument", document),
                    ],
                )
                .await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: creating the managed policy must succeed, got {status} with \
                     {body}"
                );
                let arn = element(&body, "Arn");
                let (status, body) = iam(
                    router,
                    "AttachRolePolicy",
                    &[("RoleName", ROLE_NAME), ("PolicyArn", &arn)],
                )
                .await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: attaching {arn:?} to the role must succeed, got {status} \
                     with {body}"
                );
            }
        }
    }
}

/// One row: where the grant lives, what it says, and whether the write is then copied.
struct Row {
    case: &'static str,
    grant_source: GrantSource,
    document: String,
    /// `true` is a replica in the destination and a source version settling on [`COMPLETED`];
    /// `false` is nothing in the destination and [`FAILED`].
    replicated: bool,
}

/// The table.
///
/// The rows an inline grant naming every replication action in full, and an inline grant
/// reaching the source's versions alone, would occupy are left to the suite that owns them
/// (`service-com-amazonaws-s3-amazons3/tests/all/replication_role_trust.rs`), so this one
/// states what it adds: the ATTACHED managed-policy form, and the grants written as wildcards.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the replication actions written as prefix wildcards",
            grant_source: GrantSource::Inline,
            document: wildcard_grant(),
            replicated: true,
        },
        Row {
            case: "the bucket-level and object-level statements of the published role policy",
            grant_source: GrantSource::Inline,
            document: three_statement_grant(),
            replicated: true,
        },
        Row {
            case: "the whole service wildcard scoped to the two buckets' objects",
            grant_source: GrantSource::Inline,
            document: service_wildcard_grant(),
            replicated: true,
        },
        Row {
            case: "a managed policy naming every replication action in full",
            grant_source: GrantSource::Attached,
            document: explicit_grant(),
            replicated: true,
        },
        Row {
            case: "a managed policy whose actions are prefix wildcards",
            grant_source: GrantSource::Attached,
            document: wildcard_grant(),
            replicated: true,
        },
        Row {
            case: "a managed policy whose replication writes land at another bucket",
            grant_source: GrantSource::Attached,
            document: elsewhere_grant(),
            replicated: false,
        },
    ]
}

/// Create the role [`ROLE_NAME`] through IAM's own `CreateRole`, trusting the S3 service
/// principal — so what each row varies is the role's PERMISSIONS and not its trust.
async fn create_role(router: &MultiServiceRouter, case: &str) {
    let trust = trust_document();
    let (status, body) = iam(
        router,
        "CreateRole",
        &[
            ("RoleName", ROLE_NAME),
            ("AssumeRolePolicyDocument", &trust),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: creating the role must succeed, got {status} with {body}"
    );
}

/// A bucket with versioning enabled — the state a replication configuration requires of both
/// the source and the destination bucket.
async fn versioned_bucket(router: &MultiServiceRouter, case: &str, name: &str) {
    let (status, _, body) = s3(router, "PUT", &format!("/{name}"), "").await;
    assert_eq!(
        status, 200,
        "case {case:?}: the bucket {name:?} must be created, got {status} with {body}"
    );
    let (status, _, body) = s3(
        router,
        "PUT",
        &format!("/{name}?versioning"),
        &format!(
            "<VersioningConfiguration><Status>{ENABLED}</Status></VersioningConfiguration>"
        ),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: versioning must be enabled on {name:?}, got {status} with {body}"
    );
}

/// `PUT /<source>?replication` — one enabled rule over the whole bucket, replicating to
/// [`DESTINATION`] under the role [`role_arn`] — then `GET /<source>?replication`, so every row
/// pins that the configuration is accepted and reported back whatever the role may do.
async fn configure_replication(router: &MultiServiceRouter, case: &str) {
    let body = format!(
        "<ReplicationConfiguration><Role>{}</Role>\
         <Rule><ID>{RULE_ID}</ID><Priority>1</Priority><Status>{ENABLED}</Status>\
         <Filter><Prefix></Prefix></Filter>\
         <DeleteMarkerReplication><Status>Disabled</Status></DeleteMarkerReplication>\
         <Destination><Bucket>arn:aws:s3:::{DESTINATION}</Bucket></Destination></Rule>\
         </ReplicationConfiguration>",
        role_arn()
    );
    let (status, _, answered) = s3(router, "PUT", &format!("/{SOURCE}?replication"), &body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the configuration must be accepted whatever the role may do, got \
         {status} with {answered}"
    );

    let (status, _, reported) = s3(router, "GET", &format!("/{SOURCE}?replication"), "").await;
    assert_eq!(
        status, 200,
        "case {case:?}: the configuration must be reported back, got {status} with {reported}"
    );
    for (name, expected) in [
        ("Role", role_arn()),
        ("ID", RULE_ID.to_string()),
        ("Status", ENABLED.to_string()),
    ] {
        let read = opt_element(&reported, name);
        assert_eq!(
            read.as_deref(),
            Some(expected.as_str()),
            "case {case:?}: expected the stored configuration to report {name} {expected:?}, got \
             {read:?}"
        );
    }
}

/// The `x-amz-replication-status` the source version at [`KEY`] SETTLES on: `HeadObject` is
/// looked at again until it reports something other than [`PENDING`], or until
/// [`SETTLE_DEADLINE`] has passed with nothing settled.
async fn settled_status(router: &MultiServiceRouter, case: &str) -> String {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, headers, body) = s3(router, "HEAD", &format!("/{SOURCE}/{KEY}"), "").await;
        assert_eq!(
            status, 200,
            "case {case:?}: the source version must read back, got {status} with {body}"
        );
        let reported = header(&headers, STATUS_HEADER).unwrap_or_else(|| {
            panic!(
                "case {case:?}: a version a replication rule covers must report \
                 {STATUS_HEADER}, got headers {headers:?}"
            )
        });
        if reported != PENDING {
            return reported;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the replication to settle, got {PENDING:?} still"
        );
        tokio::time::sleep(SETTLE_POLL).await;
    }
}

/// RULE: a write is copied to the destination its bucket's replication configuration names
/// exactly where the permissions in force on the role that configuration names allow the
/// replication actions there — whichever document carries the grant and however it is written
/// — and a destination the grant does not reach holds nothing while the source version settles
/// on `FAILED`.
#[tokio::test]
async fn a_write_is_replicated_where_the_configured_roles_permissions_reach_the_destination() {
    for row in rows() {
        let case = format!("{} stated in {}", row.case, row.grant_source.label());
        let router = build_router();
        create_role(&router, &case).await;
        row.grant_source.grant(&router, &case, &row.document).await;

        versioned_bucket(&router, &case, SOURCE).await;
        versioned_bucket(&router, &case, DESTINATION).await;
        configure_replication(&router, &case).await;

        let (status, written, body) = s3(&router, "PUT", &format!("/{SOURCE}/{KEY}"), BODY).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the write must be accepted, got {status} with {body}"
        );
        let version_id = header(&written, VERSION_HEADER).unwrap_or_else(|| {
            panic!(
                "case {case:?}: a versioned write must report a version id, got headers \
                 {written:?}"
            )
        });

        let expected = if row.replicated { COMPLETED } else { FAILED };
        let settled = settled_status(&router, &case).await;
        assert_eq!(
            settled, expected,
            "case {case:?}: expected the source version to settle on {expected:?}, got \
             {settled:?}"
        );

        let (status, headers, copied) =
            s3(&router, "GET", &format!("/{DESTINATION}/{KEY}"), "").await;
        if row.replicated {
            assert_eq!(
                status, 200,
                "case {case:?}: expected the copy in {DESTINATION:?}, got {status} with {copied}"
            );
            assert_eq!(
                copied, BODY,
                "case {case:?}: expected the replica to carry the source's bytes, got {copied:?}"
            );
            let reported = header(&headers, STATUS_HEADER);
            assert_eq!(
                reported.as_deref(),
                Some(REPLICA),
                "case {case:?}: expected the copy to report {REPLICA:?}, got {reported:?}"
            );
            let copied_version = header(&headers, VERSION_HEADER);
            assert_eq!(
                copied_version.as_deref(),
                Some(version_id.as_str()),
                "case {case:?}: expected the replica to keep the source's version id \
                 {version_id:?}, got {copied_version:?}"
            );
        } else {
            assert_eq!(
                status, 404,
                "case {case:?}: expected nothing in {DESTINATION:?}, got {status} with {copied}"
            );
        }
    }
}
