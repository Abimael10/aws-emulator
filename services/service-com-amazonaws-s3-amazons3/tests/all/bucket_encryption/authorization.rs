// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for what AUTHORIZES a call on a bucket's DEFAULT-ENCRYPTION
//! configuration, for a caller that is an IAM identity of the account rather than the
//! account's own credential.
//!
//! Every assertion drives real HTTP requests through a router holding S3 beside IAM over one
//! shared engine (the harness the identity-authorization suite of this crate already owns), so
//! the identity a request speaks for, the policies in force on it and the wire answer are all
//! the real ones: the user, its access key and its policies are created through IAM's own API,
//! and each S3 request is presented with that user's access key id in its `Authorization`
//! header.
//!
//! RULE under test: a call on this subresource is charged for the action its own documentation
//! names — "the `s3:PutEncryptionConfiguration` permission is required in a policy" for the
//! put and for the reset (`PutBucketEncryption`'s and `DeleteBucketEncryption`'s own
//! documentation of their general purpose bucket permissions), and "the
//! `s3:GetEncryptionConfiguration` permission is required in a policy" for the report
//! (`GetBucketEncryption`'s) — judged against the documents in force on the signing principal
//! and against the bucket's own policy BEFORE anything is reported or written. A caller no
//! document in force allows that action is refused 403 `AccessDenied` naming the principal,
//! the action and the bucket; an explicit `Deny` refuses it whatever any grant says; a grant
//! in the bucket's own policy admits it; and a refused call leaves the bucket's configuration
//! exactly as it was.
//!
//! The rows differ along what that rule generalises over: WHICH operation the call is (the
//! put, the reset, the report), which actions the caller's own document grants (none, the
//! other half of the same subresource, a wildcard family that does not cover the action, the
//! action itself, a wildcard family that does cover it, every S3 action), WHERE the grant
//! comes from (the identity's documents or the bucket policy) and WHAT the put carries
//! (SSE-S3, SSE-KMS with a key, a Bucket-Key-only rule, two rules at once) — since none of
//! that may decide whether the call is authorized. The recorded case — one un-granted put of
//! an SSE-KMS configuration — is ONE row.

use service_com_amazonaws_s3_amazons3::state::{DEFAULT_SSE_ALGORITHM, SSE_C_ENCRYPTION_TYPE};
use smithy_server_runtime::MultiServiceRouter;

use crate::bucket_lifecycle::{create_bucket, put_bucket_policy, send_bytes_signed, Answer};
use crate::identity_authorization::{
    no_grant_message, put_user_policy, router, user_arn, user_with_grant,
};
// The payload writer, the reported-rule reader and the rule type of the suite this module
// belongs to, so what a row sends and what it compares against are the ones that suite pins.
use super::{as_reported, configuration_payload, get_encryption, reported_rules, Rule};

/// The IAM action a put or a reset of this subresource is charged for, written out here
/// rather than imported from the service so the action a refusal names has something to be
/// wrong about.
const PUT_ENCRYPTION_CONFIGURATION: &str = "s3:PutEncryptionConfiguration";

/// The IAM action a report of this subresource is charged for. See
/// [`PUT_ENCRYPTION_CONFIGURATION`].
const GET_ENCRYPTION_CONFIGURATION: &str = "s3:GetEncryptionConfiguration";

/// The code S3 answers an authorization refusal with.
const ACCESS_DENIED: &str = "AccessDenied";

/// The reason clause IAM's refusal carries for a request an explicit `Deny` in one of the
/// principal's own identity-based documents refuses, as against the absence of any grant.
const EXPLICIT_IDENTITY_DENY: &str = "with an explicit deny in an identity-based policy";

/// A KMS key id, in the first of the forms
/// `smithy_query(query="doc",shape="ServerSideEncryptionByDefault",member="KMSMasterKeyID")`
/// documents ("the key ID").
const KEY_ID: &str = "1234abcd-12ab-34cd-56ef-1234567890ab";

/// The endpoint host every request of this suite is addressed to: the Regional one, which
/// carries no bucket, so each request is path-style.
const REGIONAL_ENDPOINT_HOST: &str = "s3.us-east-1.amazonaws.com";

/// The access key id the ACCOUNT's own credential reaches this harness by — the one the
/// lifecycle harness signs with, which resolves to no IAM identity and so speaks for the
/// account itself.
const ACCOUNT_KEY_ID: &str = "emulator-test-key";

/// The configuration the account itself puts on every bucket of this suite before the row's
/// caller touches it — so a refused call has something to have left alone, and a reset has
/// something of the bucket's own to remove.
fn baseline() -> Vec<Rule> {
    vec![Rule::by_default("aws:kms")
        .with_key(KEY_ID)
        .with_bucket_key(true)]
}

/// The one rule a bucket carrying no configuration of its own reports: the documented SSE-S3
/// default, with the Bucket Key off and the blocked set every general purpose bucket carries
/// (`Rule::as_reported` materialises those two).
fn default_configuration() -> Vec<Rule> {
    as_reported(&[Rule::by_default(DEFAULT_SSE_ALGORITHM)])
}

/// One request presented as signed by the identity the access key id `akid` speaks for, with
/// the XML content type an encryption payload travels under. Dispatched through the lifecycle
/// harness's one signed sender rather than a second request builder.
async fn as_identity(
    router: &MultiServiceRouter,
    akid: &str,
    method: &str,
    uri: &str,
    body: &str,
) -> Answer {
    let authorization = format!(
        "AWS4-HMAC-SHA256 Credential={akid}/20260101/us-east-1/s3/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    );
    send_bytes_signed(
        router,
        &authorization,
        method,
        REGIONAL_ENDPOINT_HOST,
        uri,
        &[("content-type", "application/xml")],
        body.as_bytes(),
    )
    .await
}

/// `PUT /<bucket>?encryption` as the identity `akid` speaks for.
async fn put_encryption_as(
    router: &MultiServiceRouter,
    akid: &str,
    bucket: &str,
    rules: &[Rule],
) -> Answer {
    as_identity(
        router,
        akid,
        "PUT",
        &format!("/{bucket}?encryption"),
        &configuration_payload(rules),
    )
    .await
}

/// `DELETE /<bucket>?encryption` as the identity `akid` speaks for.
async fn delete_encryption_as(router: &MultiServiceRouter, akid: &str, bucket: &str) -> Answer {
    as_identity(router, akid, "DELETE", &format!("/{bucket}?encryption"), "").await
}

/// A bucket of this suite: created and configured by the ACCOUNT's own credential, which is
/// what every row's caller then tries to read or change.
async fn configured_bucket(router: &MultiServiceRouter, bucket: &str) {
    let created = create_bucket(router, bucket, None, None).await;
    assert_eq!(
        created.status, 200,
        "the bucket {bucket:?} must be created, got {}",
        created.status
    );
    let configured = as_identity(
        router,
        ACCOUNT_KEY_ID,
        "PUT",
        &format!("/{bucket}?encryption"),
        &configuration_payload(&baseline()),
    )
    .await;
    assert_eq!(
        configured.status, 200,
        "the account's own put must configure {bucket:?}, got {} and body {:?}",
        configured.status,
        String::from_utf8_lossy(&configured.body)
    );
}

/// The ARNs a grant for one bucket names: the bucket itself, for the bucket-scoped actions,
/// and every key in it, for the object-scoped ones.
fn bucket_resources(bucket: &str) -> Vec<String> {
    vec![
        format!("arn:aws:s3:::{bucket}"),
        format!("arn:aws:s3:::{bucket}/*"),
    ]
}

/// What a row's call does to the subresource.
#[derive(Debug, Clone)]
enum Call {
    /// `PutBucketEncryption` carrying these rules.
    Put(Vec<Rule>),
    /// `DeleteBucketEncryption`, which resets the configuration.
    Reset,
    /// `GetBucketEncryption`, which reports it.
    Report,
}

/// What the row's call must answer.
#[derive(Debug, Clone)]
enum Expected {
    /// Served with this status, after which the bucket holds what the call asked for.
    Served(u16),
    /// Refused 403 `AccessDenied` because no document in force allows this action, after
    /// which the bucket holds exactly what it did before.
    NoGrant(&'static str),
}

/// One row of the action-mapping table.
struct Row {
    case: &'static str,
    /// The actions the row's caller is granted by its own identity policy.
    granted: &'static [&'static str],
    call: Call,
    expected: Expected,
}

/// The table: one row per (grant, call) pair the rule distinguishes.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a put with no document in force at all",
            granted: &[],
            call: Call::Put(vec![Rule::by_default("AES256")]),
            expected: Expected::NoGrant(PUT_ENCRYPTION_CONFIGURATION),
        },
        Row {
            case: "a put granted only the report of the same subresource",
            granted: &["s3:GetEncryptionConfiguration"],
            call: Call::Put(vec![Rule::by_default("AES256")]),
            expected: Expected::NoGrant(PUT_ENCRYPTION_CONFIGURATION),
        },
        Row {
            case: "a put granted the s3:PutBucket* family, which does not cover the action",
            granted: &["s3:PutBucket*"],
            call: Call::Put(vec![Rule::by_default("AES256")]),
            expected: Expected::NoGrant(PUT_ENCRYPTION_CONFIGURATION),
        },
        Row {
            case: "a put granted its own action",
            granted: &["s3:PutEncryptionConfiguration"],
            call: Call::Put(vec![Rule::by_default("AES256")]),
            expected: Expected::Served(200),
        },
        Row {
            case: "an SSE-KMS put with no grant",
            granted: &[],
            call: Call::Put(vec![Rule::by_default("aws:kms").with_key(KEY_ID)]),
            expected: Expected::NoGrant(PUT_ENCRYPTION_CONFIGURATION),
        },
        Row {
            case: "an SSE-KMS put granted its own action",
            granted: &["s3:PutEncryptionConfiguration"],
            call: Call::Put(vec![Rule::by_default("aws:kms")
                .with_key(KEY_ID)
                .with_bucket_key(false)]),
            expected: Expected::Served(200),
        },
        Row {
            case: "a put of a Bucket-Key-only rule granted a wildcard family that covers it",
            granted: &["s3:Put*"],
            call: Call::Put(vec![Rule::bucket_key_only(true)]),
            expected: Expected::Served(200),
        },
        Row {
            case: "a put of two rules at once with no grant",
            granted: &[],
            call: Call::Put(vec![
                Rule::by_default("aws:kms:dsse").with_key(KEY_ID),
                Rule::by_default("AES256").blocking(&[SSE_C_ENCRYPTION_TYPE]),
            ]),
            expected: Expected::NoGrant(PUT_ENCRYPTION_CONFIGURATION),
        },
        Row {
            case: "a put granted every S3 action",
            granted: &["s3:*"],
            call: Call::Put(vec![Rule::by_default("AES256").with_bucket_key(true)]),
            expected: Expected::Served(200),
        },
        Row {
            case: "a reset with no document in force at all",
            granted: &[],
            call: Call::Reset,
            expected: Expected::NoGrant(PUT_ENCRYPTION_CONFIGURATION),
        },
        Row {
            case: "a reset granted only the report of the same subresource",
            granted: &["s3:GetEncryptionConfiguration"],
            call: Call::Reset,
            expected: Expected::NoGrant(PUT_ENCRYPTION_CONFIGURATION),
        },
        Row {
            case: "a reset granted the write action the put is charged for",
            granted: &["s3:PutEncryptionConfiguration"],
            call: Call::Reset,
            expected: Expected::Served(204),
        },
        Row {
            case: "a report granted only the write action",
            granted: &["s3:PutEncryptionConfiguration"],
            call: Call::Report,
            expected: Expected::NoGrant(GET_ENCRYPTION_CONFIGURATION),
        },
        Row {
            case: "a report granted its own action",
            granted: &["s3:GetEncryptionConfiguration"],
            call: Call::Report,
            expected: Expected::Served(200),
        },
    ]
}

/// RULE: every call on a bucket's default encryption is charged for the action its own
/// documentation names, and a caller no document in force allows that action is refused 403
/// `AccessDenied` with the bucket left exactly as it was.
#[tokio::test]
async fn each_encryption_call_is_charged_for_the_action_its_documentation_names() {
    let router = router();
    for (index, row) in rows().iter().enumerate() {
        // Each row gets its own bucket and its own caller, so no row's grant or mutation can
        // decide another's answer.
        let bucket = format!("encryption-authz-{index}");
        configured_bucket(&router, &bucket).await;
        let name = format!("encryption-authz-user-{index}");
        let akid = user_with_grant(&router, &name, row.granted, &bucket_resources(&bucket)).await;

        let answer = match &row.call {
            Call::Put(rules) => put_encryption_as(&router, &akid, &bucket, rules).await,
            Call::Reset => delete_encryption_as(&router, &akid, &bucket).await,
            Call::Report => {
                as_identity(&router, &akid, "GET", &format!("/{bucket}?encryption"), "").await
            }
        };
        let body = String::from_utf8_lossy(&answer.body).to_string();

        // What the bucket holds AFTER the row's call, read back as the account itself — which
        // is what says whether the call changed anything.
        let held = reported_rules(&get_encryption(&router, &bucket).await);
        match &row.expected {
            Expected::Served(status) => {
                assert_eq!(
                    answer.status, *status,
                    "case {:?}: expected the call to be served with {status}, got {} and body \
                     {body:?}",
                    row.case, answer.status
                );
                let expected = match &row.call {
                    Call::Put(rules) => as_reported(rules),
                    Call::Reset => default_configuration(),
                    Call::Report => as_reported(&baseline()),
                };
                assert_eq!(
                    held, expected,
                    "case {:?}: expected the bucket to hold {expected:?}, got {held:?}",
                    row.case
                );
            }
            Expected::NoGrant(action) => {
                let resource = format!("arn:aws:s3:::{bucket}");
                assert_eq!(
                    (answer.status, answer.error_code(), answer.error_message()),
                    (
                        403,
                        ACCESS_DENIED.to_string(),
                        no_grant_message(&user_arn(&name), action, &resource)
                    ),
                    "case {:?}: expected the refusal to name the principal, {action} and \
                     {resource}, got {} and body {body:?}",
                    row.case,
                    answer.status
                );
                let unchanged = as_reported(&baseline());
                assert_eq!(
                    held, unchanged,
                    "case {:?}: a refused call must leave {unchanged:?}, got {held:?}",
                    row.case
                );
            }
        }
    }
}

/// RULE: an explicit `Deny` about the request in one of the caller's own documents refuses it
/// whatever any grant says, and the refusal names that deny rather than an absent grant.
#[tokio::test]
async fn an_explicit_deny_refuses_the_write_whatever_grants_it() {
    let router = router();
    let bucket = "encryption-authz-denied";
    configured_bucket(&router, bucket).await;
    let name = "encryption-authz-denied-user";
    // Granted EVERY S3 action by one document, and denied this one action by another.
    let akid = user_with_grant(&router, name, &["s3:*"], &bucket_resources(bucket)).await;
    let denial = format!(
        "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"Deny\",\
         \"Action\":[\"{PUT_ENCRYPTION_CONFIGURATION}\"],\
         \"Resource\":[\"arn:aws:s3:::{bucket}\"]}}]}}"
    );
    assert!(
        put_user_policy(&router, name, "denied", &denial).await,
        "the deny of {name:?} must be stored"
    );

    let resource = format!("arn:aws:s3:::{bucket}");
    let expected = format!(
        "User: {} is not authorized to perform: {PUT_ENCRYPTION_CONFIGURATION} on resource: \
         \"{resource}\" {EXPLICIT_IDENTITY_DENY}",
        user_arn(name)
    );
    for (case, answer) in [
        (
            "the put",
            put_encryption_as(&router, &akid, bucket, &[Rule::by_default("AES256")]).await,
        ),
        (
            "the reset",
            delete_encryption_as(&router, &akid, bucket).await,
        ),
    ] {
        let body = String::from_utf8_lossy(&answer.body).to_string();
        assert_eq!(
            (answer.status, answer.error_code(), answer.error_message()),
            (403, ACCESS_DENIED.to_string(), expected.clone()),
            "case {case:?}: expected the refusal to name the explicit deny, got {} and body \
             {body:?}",
            answer.status
        );
        let held = reported_rules(&get_encryption(&router, bucket).await);
        let unchanged = as_reported(&baseline());
        assert_eq!(
            held, unchanged,
            "case {case:?}: a refused call must leave {unchanged:?}, got {held:?}"
        );
    }
}

/// RULE: the grant may come from the BUCKET's own policy — one that names the caller admits
/// the write on its own, and one that names another principal does not.
#[tokio::test]
async fn a_bucket_policy_grant_naming_the_caller_admits_the_write() {
    let router = router();
    let bucket = "encryption-authz-bucket-policy";
    configured_bucket(&router, bucket).await;
    // Neither caller carries an identity policy of its own, so the bucket's policy is the only
    // document that can allow either of them.
    let granted_name = "encryption-authz-granted-user";
    let granted = user_with_grant(&router, granted_name, &[], &[]).await;
    let other_name = "encryption-authz-other-user";
    let other = user_with_grant(&router, other_name, &[], &[]).await;
    let policy = format!(
        "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"Allow\",\
         \"Principal\":{{\"AWS\":\"{}\"}},\"Action\":[\"{PUT_ENCRYPTION_CONFIGURATION}\"],\
         \"Resource\":[\"arn:aws:s3:::{bucket}\"]}}]}}",
        user_arn(granted_name)
    );
    let stored = put_bucket_policy(&router, bucket, &policy).await;
    assert_eq!(
        stored.status, 204,
        "the bucket policy must be stored, got {} and body {:?}",
        stored.status,
        String::from_utf8_lossy(&stored.body)
    );

    // The principal the policy does NOT name is still refused, and leaves the bucket alone.
    let refused = put_encryption_as(&router, &other, bucket, &[Rule::by_default("AES256")]).await;
    let resource = format!("arn:aws:s3:::{bucket}");
    assert_eq!(
        (
            refused.status,
            refused.error_code(),
            refused.error_message()
        ),
        (
            403,
            ACCESS_DENIED.to_string(),
            no_grant_message(
                &user_arn(other_name),
                PUT_ENCRYPTION_CONFIGURATION,
                &resource
            )
        ),
        "case \"a principal the bucket policy does not name\": expected the refusal, got {} and \
         body {:?}",
        refused.status,
        String::from_utf8_lossy(&refused.body)
    );
    let held = reported_rules(&get_encryption(&router, bucket).await);
    let unchanged = as_reported(&baseline());
    assert_eq!(
        held, unchanged,
        "case \"a principal the bucket policy does not name\": must leave {unchanged:?}, got \
         {held:?}"
    );

    // The principal it does name writes the configuration on the bucket policy's grant alone.
    let rules = vec![Rule::by_default("AES256").with_bucket_key(false)];
    let served = put_encryption_as(&router, &granted, bucket, &rules).await;
    assert_eq!(
        served.status, 200,
        "case \"a principal the bucket policy names\": expected 200, got {} and body {:?}",
        served.status,
        String::from_utf8_lossy(&served.body)
    );
    let held = reported_rules(&get_encryption(&router, bucket).await);
    let expected = as_reported(&rules);
    assert_eq!(
        held, expected,
        "case \"a principal the bucket policy names\": expected {expected:?}, got {held:?}"
    );
}

/// RULE: the ACCOUNT's own credential is bounded by none of this — which is also what the
/// read-backs above lean on, so it is asserted rather than assumed.
#[tokio::test]
async fn the_accounts_own_credential_is_left_the_whole_subresource() {
    let router = router();
    let bucket = "encryption-authz-account";
    configured_bucket(&router, bucket).await;
    let report = get_encryption(&router, bucket).await;
    assert_eq!(
        report.status, 200,
        "the account's own report must be served, got {} and body {:?}",
        report.status,
        String::from_utf8_lossy(&report.body)
    );
    let held = reported_rules(&report);
    let expected = as_reported(&baseline());
    assert_eq!(
        held, expected,
        "the account's own report must carry {expected:?}, got {held:?}"
    );
    let reset = as_identity(
        &router,
        ACCOUNT_KEY_ID,
        "DELETE",
        &format!("/{bucket}?encryption"),
        "",
    )
    .await;
    assert_eq!(
        reset.status, 204,
        "the account's own reset must be served, got {} and body {:?}",
        reset.status,
        String::from_utf8_lossy(&reset.body)
    );
    let held = reported_rules(&get_encryption(&router, bucket).await);
    let expected = default_configuration();
    assert_eq!(
        held, expected,
        "after the account's own reset the bucket must hold {expected:?}, got {held:?}"
    );
}
