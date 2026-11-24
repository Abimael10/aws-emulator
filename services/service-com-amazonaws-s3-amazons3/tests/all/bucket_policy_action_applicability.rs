// SPDX-License-Identifier: CC0-1.0

//! Whether the ACTIONS a bucket policy's statements name can be performed on the RESOURCES
//! those same statements name.
//!
//! The rule under test: a statement states its grant as who, which actions and which
//! resources, and every S3 action is performed on one resource TYPE — the `bucket` type,
//! named `arn:<partition>:s3:::<bucket>`, the `object` type, named
//! `arn:<partition>:s3:::<bucket>/<key>`, or no resource type at all for an action performed
//! on the account. A statement whose action is performed on no type any resource it names is
//! of grants nothing, and neither does one naming a value the policy LANGUAGE cannot read as
//! an action, so `PutBucketPolicy` refuses the whole document — `MalformedPolicy`, HTTP 400 —
//! rather than storing a grant that cannot work, and the `<Detail>` of that refusal names the
//! action and the statement the caller has to change. The bucket's own policy is left
//! exactly as it was.
//!
//! The two sentences are each error class's own fixed text
//! (`service_com_amazonaws_s3_amazons3::state::INAPPLICABLE_ACTION_MESSAGE` for an action
//! performed on no resource the statement names, `service_common::iam_policy`'s
//! `POLICY_INVALID_ACTION_MESSAGE` for a value that is no action at all); everything variable
//! about the refusal is in the `<Detail>`.
//!
//! An `s3:` action the applicability catalog has filed no row for is NOT a fault: the catalog
//! is an excerpt, so it is read in one direction only and such a statement is stored, which
//! the last rows of the accepted table pin.
//!
//! What the rule generalises over, and what the rows below therefore differ along: WHICH
//! resource type the action takes (object, bucket, account-wide), which way round the
//! mismatch runs, how many actions and resources the statement names and in which form
//! (a bare string or a list), whether the action element carries an IAM wildcard, whether
//! the statement states a `Sid`, which statement of the document carries the fault, and the
//! namespace the action is published under. The recorded case that filed this — an object
//! read scoped to the bucket itself — is one row among them.
//!
//! Every assertion drives a real HTTP request through the router (the harness the
//! bucket-lifecycle suite in this crate owns), so what is pinned is the wire behaviour.

use service_com_amazonaws_s3_amazons3::state::{
    INAPPLICABLE_ACTION_MESSAGE, MALFORMED_POLICY_CODE,
};
use service_common::iam_policy::POLICY_INVALID_ACTION_MESSAGE;
use smithy_server_runtime::MultiServiceRouter;

use super::bucket_lifecycle::{create_bucket, get_bucket_policy, put_bucket_policy, router};
use super::bucket_public_access_block::allow_public_access;

/// The bucket every row is put on: created once per test, with Block Public Access lifted
/// because most rows grant to the wildcard principal (which that guardrail would deny on its
/// own, for reasons this suite is not about).
const BUCKET: &str = "action-applicability-bucket";

/// A document the bucket takes, which every refused row is asserted to have left in place:
/// an object read scoped to the bucket's OBJECTS, which is the resource type `s3:GetObject`
/// is performed on.
fn baseline() -> String {
    serde_json::json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": "ReadTheObjects",
            "Effect": "Allow",
            "Principal": "*",
            "Action": "s3:GetObject",
            "Resource": format!("arn:aws:s3:::{BUCKET}/*"),
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

/// The bucket's stored policy must still be the baseline — what a refused put leaves behind.
async fn the_baseline_is_still_stored(router: &MultiServiceRouter, case: &str) {
    let read = get_bucket_policy(router, BUCKET).await;
    assert_eq!(
        read.status, 200,
        "case {case:?}: the bucket's own policy must still be readable, got {} with body {:?}",
        read.status,
        String::from_utf8_lossy(&read.body)
    );
    assert_eq!(
        String::from_utf8_lossy(&read.body),
        baseline(),
        "case {case:?}: a refused put must leave the stored policy as it was"
    );
}

/// One refused row: what to call it, the document put, the sentence the refusal states and
/// the `<Detail>` it names the faulted action and statement in (`None` for a statement
/// stating no `Sid`, which has no name to quote).
struct Refused {
    case: &'static str,
    document: String,
    message: &'static str,
    detail: Option<String>,
}

/// Drive every row of `rows` and assert the refusal it must answer with, then that nothing it
/// carried reached the store.
async fn each_row_is_refused(rows: Vec<Refused>) {
    let router = bucket_with_baseline().await;
    for row in rows {
        let answer = put_bucket_policy(&router, BUCKET, &row.document).await;
        let body = String::from_utf8_lossy(&answer.body).to_string();
        assert_eq!(
            (answer.status, answer.error_code(), answer.error_message()),
            (400, MALFORMED_POLICY_CODE.to_string(), row.message.to_string()),
            "case {:?}: expected HTTP 400 {MALFORMED_POLICY_CODE} {:?}, got {} with body \
             {body:?}",
            row.case,
            row.message,
            answer.status
        );
        assert_eq!(
            answer.element("Detail"),
            row.detail,
            "case {:?}: expected the Detail {:?}, got {:?}",
            row.case,
            row.detail,
            answer.element("Detail")
        );
        the_baseline_is_still_stored(&router, row.case).await;
    }
}

/// The `<Detail>` text a refusal of statement `sid` about action `action` carries.
fn detail(action: &str, sid: &str) -> Option<String> {
    Some(format!("Action \"{action}\" in Statement \"{sid}\""))
}

/// The ARN of the bucket itself, and of the objects in it — the two resource types an S3
/// action is performed on.
fn bucket_arn() -> String {
    format!("arn:aws:s3:::{BUCKET}")
}

/// See [`bucket_arn`].
fn object_arn() -> String {
    format!("arn:aws:s3:::{BUCKET}/*")
}

#[tokio::test]
async fn an_action_performed_on_no_resource_the_statement_names_refuses_the_document() {
    let rows = vec![
        Refused {
            // The recorded case: the object read a CloudFront distribution is granted,
            // scoped to the bucket rather than to its objects.
            case: "an object action scoped to the bucket itself",
            document: serde_json::json!({
                "Version": "2012-10-17",
                "Statement": [{
                    "Sid": "AllowCloudFrontServicePrincipal",
                    "Effect": "Allow",
                    "Principal": {"Service": "cloudfront.amazonaws.com"},
                    "Action": "s3:GetObject",
                    "Resource": bucket_arn(),
                }],
            })
            .to_string(),
            message: INAPPLICABLE_ACTION_MESSAGE,
            detail: detail("s3:GetObject", "AllowCloudFrontServicePrincipal"),
        },
        Refused {
            // The same mismatch the other way round.
            case: "a bucket action scoped to the bucket's objects",
            document: serde_json::json!({
                "Statement": [{
                    "Sid": "ListTheBucket",
                    "Effect": "Allow",
                    "Principal": "*",
                    "Action": "s3:ListBucket",
                    "Resource": object_arn(),
                }],
            })
            .to_string(),
            message: INAPPLICABLE_ACTION_MESSAGE,
            detail: detail("s3:ListBucket", "ListTheBucket"),
        },
        Refused {
            // An action published with NO resource type: the account's own bucket listing
            // is performed on no bucket, so no bucket ARN can carry it.
            case: "an account-wide action scoped to the bucket",
            document: serde_json::json!({
                "Statement": [{
                    "Sid": "ListEveryBucket",
                    "Effect": "Allow",
                    "Principal": "*",
                    "Action": "s3:ListAllMyBuckets",
                    "Resource": bucket_arn(),
                }],
            })
            .to_string(),
            message: INAPPLICABLE_ACTION_MESSAGE,
            detail: detail("s3:ListAllMyBuckets", "ListEveryBucket"),
        },
        Refused {
            // A LIST of actions, only one of which is misplaced: the document is refused for
            // that one, which the Detail names.
            case: "one action of a list scoped to the wrong resource type",
            document: serde_json::json!({
                "Statement": [{
                    "Sid": "WriteAndLocate",
                    "Effect": "Allow",
                    "Principal": "*",
                    "Action": ["s3:PutObject", "s3:GetBucketLocation"],
                    "Resource": object_arn(),
                }],
            })
            .to_string(),
            message: INAPPLICABLE_ACTION_MESSAGE,
            detail: detail("s3:GetBucketLocation", "WriteAndLocate"),
        },
        Refused {
            // A LIST of resources, every one of them of the same type.
            case: "an object action scoped to a list of bucket ARNs",
            document: serde_json::json!({
                "Statement": [{
                    "Sid": "ReadTwoBuckets",
                    "Effect": "Allow",
                    "Principal": "*",
                    "Action": "s3:GetObject",
                    "Resource": [bucket_arn(), "arn:aws:s3:::another-bucket-entirely"],
                }],
            })
            .to_string(),
            message: INAPPLICABLE_ACTION_MESSAGE,
            detail: detail("s3:GetObject", "ReadTwoBuckets"),
        },
        Refused {
            // An action element carrying an IAM wildcard that reaches object actions alone.
            case: "a wildcard action element reaching object actions only",
            document: serde_json::json!({
                "Statement": [{
                    "Sid": "EveryObjectRead",
                    "Effect": "Allow",
                    "Principal": "*",
                    "Action": "s3:GetObjectVersion*",
                    "Resource": bucket_arn(),
                }],
            })
            .to_string(),
            message: INAPPLICABLE_ACTION_MESSAGE,
            detail: detail("s3:GetObjectVersion*", "EveryObjectRead"),
        },
        Refused {
            // A statement stating no `Sid` is refused all the same; it has no name to quote,
            // so the quoted statement name in the `<Detail>` is empty.
            case: "a statement stating no Sid",
            document: serde_json::json!({
                "Statement": [{
                    "Effect": "Allow",
                    "Principal": "*",
                    "Action": "s3:DeleteObject",
                    "Resource": bucket_arn(),
                }],
            })
            .to_string(),
            message: INAPPLICABLE_ACTION_MESSAGE,
            detail: detail("s3:DeleteObject", ""),
        },
        Refused {
            // The fault is in the SECOND statement; the first one is a grant that works.
            case: "the second statement of a document carries the fault",
            document: serde_json::json!({
                "Statement": [
                    {
                        "Sid": "First",
                        "Effect": "Allow",
                        "Principal": "*",
                        "Action": "s3:GetObject",
                        "Resource": object_arn(),
                    },
                    {
                        "Sid": "Second",
                        "Effect": "Deny",
                        "Principal": "*",
                        "Action": "s3:PutObject",
                        "Resource": bucket_arn(),
                    },
                ],
            })
            .to_string(),
            message: INAPPLICABLE_ACTION_MESSAGE,
            detail: detail("s3:PutObject", "Second"),
        },
    ];
    each_row_is_refused(rows).await;
}

#[tokio::test]
async fn a_value_the_policy_language_cannot_read_as_an_action_refuses_the_document() {
    let rows = vec![
        Refused {
            // Every action is published as `<namespace>:<action>`; a value naming no
            // namespace names no action of any service.
            case: "an action naming no namespace",
            document: serde_json::json!({
                "Statement": [{
                    "Sid": "AllowCloudFrontServicePrincipal",
                    "Effect": "Allow",
                    "Principal": {"Service": "cloudfront.amazonaws.com"},
                    "Action": "GetObject",
                    "Resource": object_arn(),
                }],
            })
            .to_string(),
            message: POLICY_INVALID_ACTION_MESSAGE,
            detail: detail("GetObject", "AllowCloudFrontServicePrincipal"),
        },
        Refused {
            // A namespace with no action after it, which is no pair either. The spelling is
            // faulted whatever the statement's resources are, so this row names every
            // resource there is.
            case: "a namespace naming no action, on every resource",
            document: serde_json::json!({
                "Statement": [{
                    "Sid": "EmptyAction",
                    "Effect": "Allow",
                    "Principal": "*",
                    "Action": "s3:",
                    "Resource": "*",
                }],
            })
            .to_string(),
            message: POLICY_INVALID_ACTION_MESSAGE,
            detail: detail("s3:", "EmptyAction"),
        },
        Refused {
            // One element of a list, the others readable.
            case: "one unreadable action among readable ones",
            document: serde_json::json!({
                "Statement": [{
                    "Sid": "ReadAndMore",
                    "Effect": "Allow",
                    "Principal": "*",
                    "Action": ["s3:GetObject", "s3:Get Object"],
                    "Resource": object_arn(),
                }],
            })
            .to_string(),
            message: POLICY_INVALID_ACTION_MESSAGE,
            detail: detail("s3:Get Object", "ReadAndMore"),
        },
        Refused {
            // A statement stating no `Sid` is refused all the same; the quoted statement
            // name in the `<Detail>` is then empty.
            case: "an unreadable action in a statement stating no Sid",
            document: serde_json::json!({
                "Statement": [{
                    "Effect": "Allow",
                    "Principal": "*",
                    "Action": "s3 GetObject",
                    "Resource": object_arn(),
                }],
            })
            .to_string(),
            message: POLICY_INVALID_ACTION_MESSAGE,
            detail: detail("s3 GetObject", ""),
        },
    ];
    each_row_is_refused(rows).await;
}

#[tokio::test]
async fn a_grant_whose_action_reaches_a_resource_it_names_is_stored() {
    let router = bucket_with_baseline().await;
    // Each row is a document the subresource takes: the action it names is performed on a
    // resource type the statement names, or the statement names a value this rule does not
    // place, in which case nothing about it says the grant cannot work.
    let rows: Vec<(&str, String)> = vec![
        (
            "an object action on the objects",
            serde_json::json!({
                "Statement": [{
                    "Sid": "Read",
                    "Effect": "Allow",
                    "Principal": "*",
                    "Action": "s3:GetObject",
                    "Resource": object_arn(),
                }],
            })
            .to_string(),
        ),
        (
            "a bucket action on the bucket",
            serde_json::json!({
                "Statement": [{
                    "Sid": "List",
                    "Effect": "Allow",
                    "Principal": "*",
                    "Action": "s3:ListBucket",
                    "Resource": bucket_arn(),
                }],
            })
            .to_string(),
        ),
        (
            "both types named, actions of both",
            serde_json::json!({
                "Statement": [{
                    "Sid": "ListAndRead",
                    "Effect": "Allow",
                    "Principal": "*",
                    "Action": ["s3:ListBucket", "s3:GetObject", "s3:GetBucketTagging"],
                    "Resource": [bucket_arn(), object_arn()],
                }],
            })
            .to_string(),
        ),
        (
            // A wildcard element reaches bucket actions too, so the bucket alone carries it.
            "every action of the service on the bucket",
            serde_json::json!({
                "Statement": [{
                    "Sid": "Everything",
                    "Effect": "Allow",
                    "Principal": "*",
                    "Action": "s3:*",
                    "Resource": bucket_arn(),
                }],
            })
            .to_string(),
        ),
        (
            // IAM compares action names without regard to case.
            "an object action spelled in lower case",
            serde_json::json!({
                "Statement": [{
                    "Sid": "LowerCase",
                    "Effect": "Allow",
                    "Principal": "*",
                    "Action": "s3:getobject",
                    "Resource": object_arn(),
                }],
            })
            .to_string(),
        ),
        (
            // The account's own listing, on the element that covers every resource.
            "an account-wide action on every resource",
            serde_json::json!({
                "Statement": [{
                    "Sid": "ListEveryBucket",
                    "Effect": "Allow",
                    "Principal": "*",
                    "Action": "s3:ListAllMyBuckets",
                    "Resource": "*",
                }],
            })
            .to_string(),
        ),
        (
            // Another namespace publishes its own actions: the one action a directory
            // bucket's policy names is `s3express:CreateSession`, on the bucket.
            "a directory bucket session grant",
            serde_json::json!({
                "Statement": [{
                    "Sid": "CreateSession",
                    "Effect": "Allow",
                    "Principal": "*",
                    "Action": "s3express:CreateSession",
                    "Resource": bucket_arn(),
                }],
            })
            .to_string(),
        ),
        (
            // An access point is a resource type this reading does not place, so an object
            // action named beside one is not faulted.
            "an object action on an access point",
            serde_json::json!({
                "Statement": [{
                    "Sid": "ThroughTheAccessPoint",
                    "Effect": "Allow",
                    "Principal": "*",
                    "Action": "s3:GetObject",
                    "Resource": "arn:aws:s3:us-east-1:000000000000:accesspoint/reports",
                }],
            })
            .to_string(),
        ),
        (
            // A bucket name carrying an IAM wildcard can expand into an object ARN, since
            // `*` stands for any run of characters.
            "an object action on a wildcarded bucket name",
            serde_json::json!({
                "Statement": [{
                    "Sid": "WildcardedName",
                    "Effect": "Allow",
                    "Principal": "*",
                    "Action": "s3:GetObject",
                    "Resource": format!("arn:aws:s3:::{BUCKET}*"),
                }],
            })
            .to_string(),
        ),
        (
            // A statement naming no resource at all states no resources for the action to
            // be judged against.
            "a statement naming no resource",
            serde_json::json!({
                "Statement": [{
                    "Sid": "NoResource",
                    "Effect": "Allow",
                    "Principal": "*",
                    "Action": "s3:GetObject",
                }],
            })
            .to_string(),
        ),
        (
            // An `s3:` action the applicability catalog has filed no row for: the catalog is
            // an excerpt of the publication, so it is read in ONE direction only and a
            // statement naming such an action is stored — a row that has not been filed yet
            // can never refuse a grant S3 accepts. (`s3:GetAccessPoint` is published against
            // the access point resource type, which this catalog does not carry.)
            "an action the applicability catalog does not carry",
            serde_json::json!({
                "Statement": [{
                    "Sid": "ThroughTheCatalogsGap",
                    "Effect": "Allow",
                    "Principal": "*",
                    "Action": "s3:GetAccessPoint",
                    "Resource": object_arn(),
                }],
            })
            .to_string(),
        ),
        (
            // The same, for an action no published row names at all: the LANGUAGE can read
            // it, so the document is stored.
            "a readable action no publication names",
            serde_json::json!({
                "Statement": [{
                    "Sid": "Pluralised",
                    "Effect": "Allow",
                    "Principal": "*",
                    "Action": "s3:GetObjects",
                    "Resource": object_arn(),
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
