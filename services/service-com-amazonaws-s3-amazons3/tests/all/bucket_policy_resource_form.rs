// SPDX-License-Identifier: CC0-1.0

//! The FORM a bucket policy's `Resource` values have to be written in.
//!
//! The rule under test: a statement's `Resource` element names the resources its grant
//! covers, and a resource is named by its ARN — Amazon S3 publishes
//! `arn:<partition>:s3:::<bucket>` for a general purpose bucket,
//! `arn:<partition>:s3:::<bucket>/<key>` for an object in one, and its own forms for the
//! resources addressed through an access point. A value that is no ARN of this service — a
//! bare bucket name, a key, another service's resource — names nothing a bucket policy could
//! grant on, so `PutBucketPolicy` refuses the whole document (`MalformedPolicy`, HTTP 400,
//! "Policy has invalid resource") and the bucket's own policy is left exactly as it was.
//!
//! WHICH bucket's resources a well-formed ARN names is NOT this rule's question — a grant
//! covering another bucket's objects is still written in the published form; whether the
//! statement's action can be performed on the resources it names is
//! `crate::bucket_policy_action_applicability`'s.
//!
//! What the rule generalises over, and what the rows below therefore differ along: which way
//! the value is no ARN (a bare name, a key path, an ARN of another service, a truncated ARN),
//! whether the element is a bare value or a list, which statement of the document carries it,
//! and — on the accepted side — the published form (bucket, object, wildcarded, access point)
//! and the element covering every resource.
//!
//! Every assertion drives a real HTTP request through the router (the harness the
//! bucket-lifecycle suite in this crate owns), so what is pinned is the wire behaviour.

use service_com_amazonaws_s3_amazons3::state::{INVALID_RESOURCE_MESSAGE, MALFORMED_POLICY_CODE};
use smithy_server_runtime::MultiServiceRouter;

use super::bucket_lifecycle::{create_bucket, get_bucket_policy, put_bucket_policy, router};
use super::bucket_public_access_block::allow_public_access;

/// The bucket every row is put on.
const BUCKET: &str = "resource-form-bucket";

/// A document the bucket takes, which every refused row is asserted to have left in place.
fn baseline() -> String {
    document(serde_json::json!(format!("arn:aws:s3:::{BUCKET}/*")))
}

/// A one-statement document granting the objects' read over `resource` — the shape every row
/// varies the `Resource` element of.
fn document(resource: serde_json::Value) -> String {
    serde_json::json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": "ReadTheObjects",
            "Effect": "Allow",
            "Principal": "*",
            "Action": "s3:GetObject",
            "Resource": resource,
        }],
    })
    .to_string()
}

/// Stand the bucket up with the baseline policy stored on it.
async fn bucket_with_baseline() -> MultiServiceRouter {
    let router = router();
    let created = create_bucket(&router, BUCKET, None, None).await;
    assert_eq!(
        created.status, 200,
        "the bucket must be created first, got {} with body {:?}",
        created.status,
        String::from_utf8_lossy(&created.body)
    );
    // Every row grants to the wildcard principal, which Block Public Access would deny on
    // its own; this suite is about the resource's FORM, so the guardrail is lifted first.
    allow_public_access(&router, BUCKET).await;
    let stored = put_bucket_policy(&router, BUCKET, &baseline()).await;
    assert_eq!(
        stored.status, 204,
        "the baseline document must be stored, got {} with body {:?}",
        stored.status,
        String::from_utf8_lossy(&stored.body)
    );
    router
}

#[tokio::test]
async fn a_resource_that_is_no_arn_of_this_service_refuses_the_document() {
    let router = bucket_with_baseline().await;
    let rows: Vec<(&str, String)> = vec![
        ("the bucket's bare name", document(serde_json::json!(BUCKET))),
        (
            "a key path rather than an ARN",
            document(serde_json::json!(format!("{BUCKET}/*"))),
        ),
        (
            "an ARN missing its leading section",
            document(serde_json::json!(format!("aws:s3:::{BUCKET}/*"))),
        ),
        (
            "another service's resource",
            document(serde_json::json!(
                "arn:aws:sqs:us-east-1:000000000000:notifications"
            )),
        ),
        (
            // One value of a list, the others well formed.
            "one value of a list of resources",
            document(serde_json::json!([
                format!("arn:aws:s3:::{BUCKET}"),
                format!("arn:aws:s3:::{BUCKET}/*"),
                format!("{BUCKET}/private/*"),
            ])),
        ),
        (
            // The fault is in the SECOND statement; the first names resources this policy
            // can govern.
            "the second statement of a document carries the fault",
            serde_json::json!({
                "Statement": [
                    {
                        "Sid": "First",
                        "Effect": "Allow",
                        "Principal": "*",
                        "Action": "s3:GetObject",
                        "Resource": format!("arn:aws:s3:::{BUCKET}/*"),
                    },
                    {
                        "Sid": "Second",
                        "Effect": "Deny",
                        "Principal": "*",
                        "Action": "s3:ListBucket",
                        "Resource": BUCKET,
                    },
                ],
            })
            .to_string(),
        ),
        (
            // The complement element names resources too, and is read the same way.
            "a NotResource value that is no ARN",
            serde_json::json!({
                "Statement": [{
                    "Sid": "EverythingElse",
                    "Effect": "Deny",
                    "Principal": "*",
                    "Action": "s3:GetObject",
                    "NotResource": format!("{BUCKET}/public/*"),
                }],
            })
            .to_string(),
        ),
    ];
    for (case, document) in rows {
        let answer = put_bucket_policy(&router, BUCKET, &document).await;
        let body = String::from_utf8_lossy(&answer.body).to_string();
        assert_eq!(
            (answer.status, answer.error_code(), answer.error_message()),
            (
                400,
                MALFORMED_POLICY_CODE.to_string(),
                INVALID_RESOURCE_MESSAGE.to_string()
            ),
            "case {case:?}: expected HTTP 400 {MALFORMED_POLICY_CODE} \
             {INVALID_RESOURCE_MESSAGE:?}, got {} with body {body:?}",
            answer.status
        );
        // Nothing the refused document carried reached the store.
        let read = get_bucket_policy(&router, BUCKET).await;
        assert_eq!(
            String::from_utf8_lossy(&read.body),
            baseline(),
            "case {case:?}: a refused put must leave the stored policy as it was, got {} with \
             body {:?}",
            read.status,
            String::from_utf8_lossy(&read.body)
        );
    }
}

#[tokio::test]
async fn a_resource_named_in_a_published_form_is_stored() {
    let router = bucket_with_baseline().await;
    let rows: Vec<(&str, String)> = vec![
        (
            "the bucket itself, with a bucket action",
            serde_json::json!({
                "Statement": [{
                    "Sid": "ListTheBucket",
                    "Effect": "Allow",
                    "Principal": "*",
                    "Action": "s3:ListBucket",
                    "Resource": format!("arn:aws:s3:::{BUCKET}"),
                }],
            })
            .to_string(),
        ),
        (
            "the objects in it",
            document(serde_json::json!(format!("arn:aws:s3:::{BUCKET}/*"))),
        ),
        (
            "one object of it",
            document(serde_json::json!(format!("arn:aws:s3:::{BUCKET}/reports/1"))),
        ),
        (
            "the element covering every resource",
            document(serde_json::json!("*")),
        ),
        (
            "a wildcarded bucket name",
            document(serde_json::json!("arn:aws:s3:::*")),
        ),
        (
            "an ARN of another partition",
            document(serde_json::json!(format!("arn:aws-us-gov:s3:::{BUCKET}/*"))),
        ),
        (
            "the objects reached through an access point",
            document(serde_json::json!(
                "arn:aws:s3:us-east-1:000000000000:accesspoint/reports/object/*"
            )),
        ),
        (
            // WHICH bucket a grant covers is not this rule's question: the form is published,
            // so the document is stored.
            "another bucket's objects",
            document(serde_json::json!("arn:aws:s3:::another-bucket-entirely/*")),
        ),
        (
            "a list naming both resource types",
            serde_json::json!({
                "Statement": [{
                    "Sid": "ListAndRead",
                    "Effect": "Allow",
                    "Principal": "*",
                    "Action": ["s3:ListBucket", "s3:GetObject"],
                    "Resource": [
                        format!("arn:aws:s3:::{BUCKET}"),
                        format!("arn:aws:s3:::{BUCKET}/*"),
                    ],
                }],
            })
            .to_string(),
        ),
    ];
    for (case, document) in rows {
        let answer = put_bucket_policy(&router, BUCKET, &document).await;
        assert_eq!(
            answer.status, 204,
            "case {case:?}: expected HTTP 204, got {} with body {:?}",
            answer.status,
            String::from_utf8_lossy(&answer.body)
        );
        // Stored, not merely accepted: the read reports the document byte for byte.
        let read = get_bucket_policy(&router, BUCKET).await;
        assert_eq!(
            String::from_utf8_lossy(&read.body),
            document.as_str(),
            "case {case:?}: the stored policy must read back byte for byte, got {} with body \
             {:?}",
            read.status,
            String::from_utf8_lossy(&read.body)
        );
    }
}
