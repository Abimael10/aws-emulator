// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for what AUTHORIZES a call on a bucket's own POLICY, for a caller that is
//! an IAM identity of the account rather than the account's own credential.
//!
//! Every assertion drives real HTTP requests through a router holding S3 beside IAM over one
//! shared engine (the harness the identity-authorization suite of this crate owns), so the
//! identity a request speaks for, the policies in force on it and the wire answer are all the
//! real ones: the user, its access key and its policies are created through IAM's own API, and
//! each S3 request is presented with that user's access key id in its `Authorization` header.
//!
//! RULE under test: each of the three calls on this subresource is charged for the action its
//! own documentation names — "the `s3:GetBucketPolicy` permission is required in a policy"
//! (`GetBucketPolicy`), "the `s3:PutBucketPolicy` permission is required in a policy"
//! (`PutBucketPolicy`) and "the `s3:DeleteBucketPolicy` permission is required in a policy"
//! (`DeleteBucketPolicy`) — judged against the documents in force on the signing principal and
//! against the bucket's own policy BEFORE anything is reported or written. A caller no document
//! in force allows that action is refused 403 `AccessDenied` naming the principal, the action
//! and the bucket; an explicit `Deny` refuses it whatever any grant says; and a refused call
//! leaves the bucket's policy exactly as it was, while a principal that IS allowed the action
//! performs it.
//!
//! The rows differ along what the rule generalises over: WHICH call it is (the report, the
//! write, the removal), and which actions the caller's own document grants (none, another of
//! the three, a wildcard family that does not cover the action, a wildcard family that does,
//! the action itself, and every S3 action). The recorded case — one un-granted removal — is
//! ONE row.

use smithy_server_runtime::MultiServiceRouter;

use crate::bucket_lifecycle::{create_bucket, send_bytes_signed, Answer};
use crate::identity_authorization::{
    no_grant_message, put_user_policy, router, user_arn, user_with_grant,
};

/// The three actions the subresource's calls are charged for, written out here rather than
/// imported from the service so the action a refusal names has something to be wrong about.
const GET_BUCKET_POLICY: &str = "s3:GetBucketPolicy";
/// See [`GET_BUCKET_POLICY`].
const PUT_BUCKET_POLICY: &str = "s3:PutBucketPolicy";
/// See [`GET_BUCKET_POLICY`].
const DELETE_BUCKET_POLICY: &str = "s3:DeleteBucketPolicy";

/// The code S3 answers an authorization refusal with, and its status.
const ACCESS_DENIED: &str = "AccessDenied";

/// The reason clause IAM's refusal carries for a request an explicit `Deny` in one of the
/// principal's own identity-based documents refuses, as against the absence of any grant.
const EXPLICIT_IDENTITY_DENY: &str = "with an explicit deny in an identity-based policy";

/// The code S3 answers a read of a bucket that carries no policy with.
const NO_SUCH_BUCKET_POLICY: &str = "NoSuchBucketPolicy";

/// The endpoint host every request of this suite is addressed to: the Regional one, which
/// carries no bucket, so each request is path-style.
const REGIONAL_ENDPOINT_HOST: &str = "s3.us-east-1.amazonaws.com";

/// The access key id the ACCOUNT's own credential reaches this harness by — the one the
/// lifecycle harness signs with, which resolves to no IAM identity and so speaks for the
/// account itself.
const ACCOUNT_KEY_ID: &str = "emulator-test-key";

/// The policy the account itself puts on every bucket of this suite before the row's caller
/// touches it, so a refused call has something to have left alone and a removal something to
/// take away. It grants a read of the bucket's objects to the account itself and names none of
/// this suite's users, so it is no grant for any of them.
///
/// Written in the form S3 renders a stored policy in — a one-value `Action`/`Resource` as that
/// string rather than as a one-element array — so the document read back is comparable to the
/// one submitted (that rendering is a fixed point).
fn baseline_policy(bucket: &str) -> String {
    format!(
        "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Sid\":\"baseline\",\
         \"Effect\":\"Allow\",\"Principal\":{{\"AWS\":\"arn:aws:iam::{}:root\"}},\
         \"Action\":\"s3:GetObject\",\"Resource\":\"arn:aws:s3:::{bucket}/*\"}}]}}",
        engine::emulator_account_id()
    )
}

/// A document a row's write asks the bucket to carry, distinct from the baseline so a served
/// write is visible in the read-back. In the same rendered form, for the same reason.
fn replacement_policy(bucket: &str) -> String {
    format!(
        "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Sid\":\"replacement\",\
         \"Effect\":\"Allow\",\"Principal\":{{\"AWS\":\"arn:aws:iam::{}:root\"}},\
         \"Action\":\"s3:ListBucket\",\"Resource\":\"arn:aws:s3:::{bucket}\"}}]}}",
        engine::emulator_account_id()
    )
}

/// One request presented as signed by the identity the access key id `akid` speaks for.
/// Dispatched through the lifecycle harness's one signed sender rather than a second request
/// builder.
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
        &[("content-type", "application/json")],
        body.as_bytes(),
    )
    .await
}

/// `GET /<bucket>?policy` as the identity `akid` speaks for.
async fn get_policy_as(router: &MultiServiceRouter, akid: &str, bucket: &str) -> Answer {
    as_identity(router, akid, "GET", &format!("/{bucket}?policy"), "").await
}

/// `DELETE /<bucket>?policy` as the identity `akid` speaks for.
async fn delete_policy_as(router: &MultiServiceRouter, akid: &str, bucket: &str) -> Answer {
    as_identity(router, akid, "DELETE", &format!("/{bucket}?policy"), "").await
}

/// `PUT /<bucket>?policy` as the identity `akid` speaks for, carrying `document`.
async fn put_policy_as(
    router: &MultiServiceRouter,
    akid: &str,
    bucket: &str,
    document: &str,
) -> Answer {
    as_identity(router, akid, "PUT", &format!("/{bucket}?policy"), document).await
}

/// What the bucket carries now, read back as the ACCOUNT itself: the document, or `None` when
/// the bucket carries no policy at all.
async fn held_policy(router: &MultiServiceRouter, bucket: &str) -> Option<String> {
    let read = get_policy_as(router, ACCOUNT_KEY_ID, bucket).await;
    match read.status {
        200 => Some(String::from_utf8_lossy(&read.body).to_string()),
        _ => {
            assert_eq!(
                read.error_code(),
                NO_SUCH_BUCKET_POLICY,
                "a bucket carrying no policy must answer {NO_SUCH_BUCKET_POLICY}, got {:?}",
                read.error_code()
            );
            None
        }
    }
}

/// A bucket of this suite: created and given its baseline policy by the ACCOUNT's own
/// credential, which is what every row's caller then tries to read, replace or remove.
async fn policied_bucket(router: &MultiServiceRouter, bucket: &str) {
    let created = create_bucket(router, bucket, None, None).await;
    assert_eq!(
        created.status, 200,
        "the bucket {bucket:?} must be created, got {}",
        created.status
    );
    let stored = put_policy_as(router, ACCOUNT_KEY_ID, bucket, &baseline_policy(bucket)).await;
    assert_eq!(
        stored.status, 204,
        "the account's own put must store the baseline policy of {bucket:?}, got {} and body {:?}",
        stored.status,
        String::from_utf8_lossy(&stored.body)
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
#[derive(Debug, Clone, Copy)]
enum Call {
    /// `GetBucketPolicy`, which reports the document.
    Report,
    /// `PutBucketPolicy`, which replaces it.
    Write,
    /// `DeleteBucketPolicy`, which removes it.
    Remove,
}

/// What the row's call must answer.
#[derive(Debug, Clone, Copy)]
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
            case: "a report with no document in force at all",
            granted: &[],
            call: Call::Report,
            expected: Expected::NoGrant(GET_BUCKET_POLICY),
        },
        Row {
            case: "a report granted only the write of the same subresource",
            granted: &[PUT_BUCKET_POLICY],
            call: Call::Report,
            expected: Expected::NoGrant(GET_BUCKET_POLICY),
        },
        Row {
            case: "a report granted only the removal of the same subresource",
            granted: &[DELETE_BUCKET_POLICY],
            call: Call::Report,
            expected: Expected::NoGrant(GET_BUCKET_POLICY),
        },
        Row {
            case: "a report granted a wildcard family that does not cover the action",
            granted: &["s3:GetObject*"],
            call: Call::Report,
            expected: Expected::NoGrant(GET_BUCKET_POLICY),
        },
        Row {
            case: "a report granted its own action",
            granted: &[GET_BUCKET_POLICY],
            call: Call::Report,
            expected: Expected::Served(200),
        },
        Row {
            case: "a report granted a wildcard family that covers the action",
            granted: &["s3:GetBucket*"],
            call: Call::Report,
            expected: Expected::Served(200),
        },
        Row {
            case: "a removal with no document in force at all",
            granted: &[],
            call: Call::Remove,
            expected: Expected::NoGrant(DELETE_BUCKET_POLICY),
        },
        Row {
            case: "a removal granted only the write of the same subresource",
            granted: &[PUT_BUCKET_POLICY],
            call: Call::Remove,
            expected: Expected::NoGrant(DELETE_BUCKET_POLICY),
        },
        Row {
            case: "a removal granted only the report of the same subresource",
            granted: &[GET_BUCKET_POLICY],
            call: Call::Remove,
            expected: Expected::NoGrant(DELETE_BUCKET_POLICY),
        },
        Row {
            case: "a removal granted its own action",
            granted: &[DELETE_BUCKET_POLICY],
            call: Call::Remove,
            expected: Expected::Served(204),
        },
        Row {
            case: "a removal granted every S3 action",
            granted: &["s3:*"],
            call: Call::Remove,
            expected: Expected::Served(204),
        },
        Row {
            case: "a write with no document in force at all",
            granted: &[],
            call: Call::Write,
            expected: Expected::NoGrant(PUT_BUCKET_POLICY),
        },
        Row {
            case: "a write granted only the removal of the same subresource",
            granted: &[DELETE_BUCKET_POLICY],
            call: Call::Write,
            expected: Expected::NoGrant(PUT_BUCKET_POLICY),
        },
        Row {
            case: "a write granted its own action",
            granted: &[PUT_BUCKET_POLICY],
            call: Call::Write,
            expected: Expected::Served(204),
        },
    ]
}

/// RULE: every call on a bucket's policy is charged for the action its own documentation
/// names, and a caller no document in force allows that action is refused 403 `AccessDenied`
/// with the bucket's policy left exactly as it was.
#[tokio::test]
async fn each_bucket_policy_call_is_charged_for_the_action_its_documentation_names() {
    let router = router();
    for (index, row) in rows().iter().enumerate() {
        // Each row gets its own bucket and its own caller, so no row's grant or mutation can
        // decide another's answer.
        let bucket = format!("policy-authz-{index}");
        policied_bucket(&router, &bucket).await;
        let name = format!("policy-authz-user-{index}");
        let akid = user_with_grant(&router, &name, row.granted, &bucket_resources(&bucket)).await;
        let baseline = baseline_policy(&bucket);
        let replacement = replacement_policy(&bucket);

        let answer = match row.call {
            Call::Report => get_policy_as(&router, &akid, &bucket).await,
            Call::Write => put_policy_as(&router, &akid, &bucket, &replacement).await,
            Call::Remove => delete_policy_as(&router, &akid, &bucket).await,
        };
        let body = String::from_utf8_lossy(&answer.body).to_string();
        let held = held_policy(&router, &bucket).await;

        match row.expected {
            Expected::Served(status) => {
                assert_eq!(
                    answer.status, status,
                    "case {:?}: expected the call to be served with {status}, got {} and body \
                     {body:?}",
                    row.case, answer.status
                );
                let expected = match row.call {
                    Call::Report => Some(baseline.clone()),
                    Call::Write => Some(replacement.clone()),
                    Call::Remove => None,
                };
                assert_eq!(
                    held, expected,
                    "case {:?}: expected the bucket to hold {expected:?}, got {held:?}",
                    row.case
                );
                if matches!(row.call, Call::Report) {
                    assert_eq!(
                        body, baseline,
                        "case {:?}: a served report must answer the stored document, got \
                         {body:?}",
                        row.case
                    );
                }
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
                assert_eq!(
                    held,
                    Some(baseline.clone()),
                    "case {:?}: a refused call must leave the baseline policy, got {held:?}",
                    row.case
                );
            }
        }
    }
}

/// RULE: an explicit `Deny` about the request in one of the caller's own documents refuses it
/// whatever any grant says, the refusal names that deny rather than an absent grant, and a
/// TWIN principal that is allowed the same action performs it on the same bucket.
#[tokio::test]
async fn an_explicit_deny_refuses_the_removal_while_a_permitted_twin_performs_it() {
    let router = router();
    let bucket = "policy-authz-denied";
    policied_bucket(&router, bucket).await;
    let baseline = baseline_policy(bucket);

    // Granted EVERY S3 action by one document, and denied this one action by another.
    let denied_name = "policy-authz-denied-user";
    let denied = user_with_grant(&router, denied_name, &["s3:*"], &bucket_resources(bucket)).await;
    let denial = format!(
        "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"Deny\",\
         \"Action\":[\"{DELETE_BUCKET_POLICY}\"],\
         \"Resource\":[\"arn:aws:s3:::{bucket}\"]}}]}}"
    );
    assert!(
        put_user_policy(&router, denied_name, "denied", &denial).await,
        "the deny of {denied_name:?} must be stored"
    );
    // The twin: the same account, granted exactly the action the first is denied.
    let twin_name = "policy-authz-twin-user";
    let twin = user_with_grant(
        &router,
        twin_name,
        &[DELETE_BUCKET_POLICY],
        &bucket_resources(bucket),
    )
    .await;

    let resource = format!("arn:aws:s3:::{bucket}");
    let expected = format!(
        "User: {} is not authorized to perform: {DELETE_BUCKET_POLICY} on resource: \
         \"{resource}\" {EXPLICIT_IDENTITY_DENY}",
        user_arn(denied_name)
    );
    let refused = delete_policy_as(&router, &denied, bucket).await;
    let body = String::from_utf8_lossy(&refused.body).to_string();
    assert_eq!(
        (
            refused.status,
            refused.error_code(),
            refused.error_message()
        ),
        (403, ACCESS_DENIED.to_string(), expected),
        "case \"the denied principal\": expected the refusal to name the explicit deny, got {} \
         and body {body:?}",
        refused.status
    );
    assert_eq!(
        held_policy(&router, bucket).await,
        Some(baseline.clone()),
        "case \"the denied principal\": the policy must be left exactly as it was"
    );

    // The twin, which is what shows the refusal is about the principal and not about the call.
    let removed = delete_policy_as(&router, &twin, bucket).await;
    assert_eq!(
        removed.status,
        204,
        "case \"the permitted twin\": expected HTTP 204, got {} and body {:?}",
        removed.status,
        String::from_utf8_lossy(&removed.body)
    );
    assert_eq!(
        held_policy(&router, bucket).await,
        None,
        "case \"the permitted twin\": the policy must be gone afterwards"
    );
}
